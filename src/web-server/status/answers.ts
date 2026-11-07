/**
 * The owner's answers to decision cards (docs/status-tab.md, Answers).
 *
 * The deck sends a set of entries at once (a "send"). Order of authority, each step recorded in
 * the history:
 * 1. the private store `<ccsDir>/status/answers.jsonl` (append, local disk): a record exists once
 *    its line is written, and the history is read from it;
 * 2. each record's signed copy under `<reports>/v1/answers/` (temporary file and rename), the
 *    public key at `keys/aac-answers.pub` and `keys/<keyId>.pub`, and a signed send summary
 *    `answers/batches/<sendId>.json`;
 * 3. one pm-mail ring per asker per send, after the signed copies are in place. A card without a
 *    pm-mail name is not rung: its agent reads its answers file.
 * A failed share write, summary or ring is retried by `retryPending()` and shown, never hidden.
 * Every entry is checked against the card as it is on disk at that moment.
 */
import { createHash, randomBytes, randomUUID } from 'crypto';
import fs from 'fs';
import path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { DeadlineExceededError, withDeadline } from '../../utils/settle-deadline';
import { agentReportsRoot } from './agent-reports-reader';
import {
  DOORBELL_OFF_DETAIL,
  NO_MAIL_DETAIL,
  pmMailDoorbell,
  ringBody,
  ringSubject,
  sendSummary,
  type Doorbell,
  type RingResult,
  type SentRecord,
} from './answer-delivery';
import {
  CLOSING_KINDS,
  entryKind,
  normalizeNote,
  parseAnswerEntry,
  refused,
  shareFileName,
  validChoice,
  type AnswerEntry,
  type AnswerKind,
  type EntryResult,
  type RefusalCode,
} from './answer-entries';
import {
  loadOrCreateAnswerKey,
  signAnswer,
  signDocument,
  type AnswerKey,
  type SignedAnswer,
  type UnsignedAnswer,
  canonicalJson,
} from './answer-signing';
import {
  ANSWER_SCHEMA,
  isCardId,
  isPmMailName,
  type CardDoc,
  type ParsedReport,
} from './report-schema';
import { readRegularFile } from './share-read';

export * from './answer-entries';
export {
  DOORBELL_SENDER,
  NO_MAIL_DETAIL,
  pmMailDoorbell,
  type Doorbell,
  type RingResult,
} from './answer-delivery';

/** Retries of a failed share write, summary or ring before the history says it gave up. */
export const MAX_DELIVERY_ATTEMPTS = 20;

/** How long each step may take before the share or the doorbell counts as hung. */
const DEFAULT_TIMING = {
  /** One signed file (with the public keys) reaching the share. */
  shareMs: 10_000,
  /** One fresh read of a card. */
  readMs: 5_000,
  /** One whole send, inside the serial queue: 50 entries on a hung share still end. */
  sendBudgetMs: 30_000,
  /** One retry pass, including its share writes and rings. */
  retryBudgetMs: 45_000,
  /** One ring: pm-mail's own 30 s timeout and its kill grace, so this only fires on a lost one. */
  ringMs: 40_000,
};
export type AnswerTiming = typeof DEFAULT_TIMING;

/** The client ids a duplicate send is recognised by: the last 200, for at most a day. */
const MAX_REMEMBERED_SENDS = 200;
const REMEMBER_SEND_MS = 24 * 60 * 60 * 1000;
/** Draft ids: every one from the last day, and the newest 1,000 whatever their age. */
const MIN_REMEMBERED_ENTRIES = 1000;
const REMEMBER_ENTRY_MS = 24 * 60 * 60 * 1000;

function contentHash(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

/**
 * What a draft says, for telling a retry of it from another use of its id. A note or an objection
 * leaves the card's version out: an agent may reply on the card between a lost response and the
 * retry, and words already sent must not be recorded again for that. An answer or a done closes one
 * version, so it binds that version: replayed on a reopened card it would silently drop the answer.
 */
function draftHash(entry: AnswerEntry, note: string): string {
  const kind = entry.kind ?? (entry.choice.length > 0 ? 'answer' : 'note');
  return contentHash({
    cardId: entry.cardId,
    choice: entry.choice,
    note,
    via: entry.via,
    kind: entry.kind ?? null,
    ...(CLOSING_KINDS.has(kind) ? { cardSeq: entry.cardSeq, cardSha256: entry.cardSha256 } : {}),
  });
}

const SUPERSEDED_DETAIL =
  'Not delivered: a newer answer to this card replaced this one before it reached the share.';
const SHARE_UNAVAILABLE_DETAIL = 'The reports share is not available.';

/** The share's `v1` folder is not there: not mounted, most likely. Nothing was created. */
class ShareUnavailableError extends Error {}
/** The write was abandoned just before its rename: it is too late or no longer wanted. */
class ShareWriteDroppedError extends Error {}

/** Time left, on the wall clock: a test's frozen `now` must not make a budget endless. */
function startBudget(ms: number) {
  const ends = Date.now() + ms;
  let spent = false;
  return {
    left: () => (spent ? 0 : Math.max(0, ends - Date.now())),
    spend: () => {
      spent = true;
    },
  };
}
type Budget = ReturnType<typeof startBudget>;

export function statusStoreDir(): string {
  return path.join(getCcsDir(), 'status');
}

export type StepName = 'share' | 'doorbell';

export interface StepState {
  state: 'pending' | 'done' | 'failed' | 'skipped';
  at: string | null;
  detail: string | null;
  attempts: number;
}

export interface AnswerHistoryEntry {
  id: string;
  /** The send this record came in; null for records from before sends existed. */
  sendId: string | null;
  answer: SignedAnswer;
  /** The card version answered, as it was read at that moment. */
  card: CardDoc;
  share: StepState;
  doorbell: StepState;
}

export interface SendRecord {
  sendId: string;
  at: string;
  device: string;
  entryIds: string[];
  summary: StepState;
}

export interface AnswerDelivery {
  state: 'rang' | 'failed' | 'pending';
  at: string | null;
  detail: string | null;
  share: StepState;
  doorbell: StepState;
}

/**
 * Creates the folders from `v1` down to `dir`, one level at a time and never `v1` or its parents, so
 * a share that goes away mid-write cannot grow a local folder where its mount point was.
 */
async function mkdirBelow(v1: string, dir: string): Promise<void> {
  let current = v1;
  for (const segment of path.relative(v1, dir).split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    await fs.promises.mkdir(current).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== 'EEXIST') throw error;
    });
  }
}

/**
 * Temporary dot file in the same folder, flushed, then renamed over the target (async: a share can
 * hang). `proceed` is asked again just before the rename: a share that hung for a while may wake up
 * after the caller has given up or a newer record has been written.
 */
async function writeAtomic(
  v1: string,
  file: string,
  body: string,
  proceed: () => boolean
): Promise<void> {
  await mkdirBelow(v1, path.dirname(file));
  const temp = path.join(
    path.dirname(file),
    `.${path.basename(file)}.tmp-${randomBytes(6).toString('hex')}`
  );
  try {
    const handle = await fs.promises.open(temp, 'wx', 0o644);
    try {
      await handle.writeFile(body);
      await handle.sync();
    } finally {
      await handle.close();
    }
    if (!proceed()) throw new ShareWriteDroppedError();
    await fs.promises.rename(temp, file);
  } catch (error) {
    await fs.promises.rm(temp, { force: true }).catch(() => undefined);
    throw error;
  }
}

function appendLine(file: string, record: unknown): void {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const fd = fs.openSync(file, 'a', 0o600);
  try {
    fs.writeSync(fd, `${JSON.stringify(record)}\n`);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
}

const PENDING: StepState = Object.freeze({ state: 'pending', at: null, detail: null, attempts: 0 });

function stepFrom(record: Record<string, unknown>, before: StepState): StepState {
  return {
    state: (['done', 'failed', 'skipped'] as const).find((s) => s === record.state) ?? 'failed',
    at: typeof record.at === 'string' ? record.at : null,
    detail: typeof record.detail === 'string' ? record.detail : null,
    attempts: before.attempts + 1,
  };
}

/** The page's delivery summary for one record. */
export function deliveryView(entry: AnswerHistoryEntry): AnswerDelivery {
  const { share, doorbell } = entry;
  const base = { share, doorbell };
  if (share.state === 'skipped')
    return { ...base, state: 'failed', at: share.at, detail: share.detail };
  if (share.state === 'failed')
    return {
      ...base,
      state: 'failed',
      at: share.at,
      detail:
        share.attempts >= MAX_DELIVERY_ATTEMPTS
          ? `The signed copy could not be written to the share after ${share.attempts} attempts.`
          : share.detail === SHARE_UNAVAILABLE_DETAIL
            ? 'The reports share is not available, so the signed copy is not written yet; retrying.'
            : 'The signed copy could not be written to the share yet; retrying.',
    };
  if (doorbell.state === 'failed')
    return {
      ...base,
      state: 'failed',
      at: doorbell.at,
      detail:
        doorbell.attempts >= MAX_DELIVERY_ATTEMPTS
          ? `The asker could not be rung after ${doorbell.attempts} attempts; the signed answer is on the share.`
          : 'The asker could not be rung yet; retrying. The signed answer is on the share.',
    };
  // No mail name: the signed file is on the share and the agent reads it; nothing more to do.
  if (share.state === 'done' && doorbell.state === 'skipped' && doorbell.detail === NO_MAIL_DETAIL)
    return { ...base, state: 'rang', at: doorbell.at, detail: NO_MAIL_DETAIL };
  if (doorbell.state === 'skipped')
    return { ...base, state: 'failed', at: doorbell.at, detail: doorbell.detail };
  if (share.state === 'done' && doorbell.state === 'done')
    return { ...base, state: 'rang', at: doorbell.at, detail: doorbell.detail };
  return { ...base, state: 'pending', at: null, detail: 'Delivering.' };
}

export interface StatusAnswersDeps {
  storeDir?: () => string;
  reportsRoot?: () => string;
  /** The card as it is on disk now. */
  readCard: (cardId: string) => Promise<ParsedReport<CardDoc> | null>;
  doorbell?: Doorbell;
  now?: () => number;
  /** Called after any change the page shows (a new answer, a delivery step). */
  onChange?: () => void;
  timing?: Partial<AnswerTiming>;
}

const RECORD_KINDS: readonly string[] = ['answer', 'note', 'object', 'done'];

export class StatusAnswers {
  private entries: AnswerHistoryEntry[] | null = null;
  private readonly sends = new Map<string, SendRecord>();
  private queue: Promise<unknown> = Promise.resolve();
  private key: AnswerKey | null = null;
  private retrying = false;
  /**
   * Records a retry must not ring: from the moment a send creates them until its own ring is over
   * (the send rings them itself, whole), and while any ring of them is running.
   */
  private readonly ringing = new Set<string>();
  /** Client send id -> what that send answered, oldest first. Never the server's own batch id. */
  private readonly replays = new Map<
    string,
    { at: number; results: EntryResult[]; entriesHash: string | null }
  >();
  /** Draft id -> the content it was recorded with and when, oldest first. */
  private readonly drafts = new Map<string, { at: number; hash: string }>();
  private readonly timing: AnswerTiming;

  constructor(private readonly deps: StatusAnswersDeps) {
    this.timing = { ...DEFAULT_TIMING, ...deps.timing };
  }

  /** A hint to the page; a listener that throws must not cost a record its result. */
  private notifyChange(): void {
    try {
      this.deps.onChange?.();
    } catch {
      /* Only a hint. */
    }
  }

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private storeDir(): string {
    return (this.deps.storeDir ?? statusStoreDir)();
  }

  private storeFile(): string {
    return path.join(this.storeDir(), 'answers.jsonl');
  }

  private reportsRoot(): string {
    return (this.deps.reportsRoot ?? agentReportsRoot)();
  }

  /** Runs one change at a time, so two requests can never both answer the same card version. */
  private serial<T>(work: () => Promise<T>): Promise<T> {
    const next = this.queue.then(work, work);
    this.queue = next.catch(() => undefined);
    return next;
  }

  /** Every record in the private store, oldest first (folded with its delivery steps). */
  list(): AnswerHistoryEntry[] {
    if (this.entries) return this.entries;
    const entries: AnswerHistoryEntry[] = [];
    const byId = new Map<string, AnswerHistoryEntry>();
    let text = '';
    try {
      text = fs.readFileSync(this.storeFile(), 'utf8');
    } catch {
      text = '';
    }
    for (const line of text.split('\n')) {
      if (!line.trim()) continue;
      let record: Record<string, unknown>;
      try {
        record = JSON.parse(line) as Record<string, unknown>;
      } catch {
        continue;
      }
      if (
        record.type === 'answer' &&
        typeof record.id === 'string' &&
        record.answer &&
        record.card
      ) {
        const answer = record.answer as SignedAnswer;
        const entry: AnswerHistoryEntry = {
          id: record.id,
          sendId: typeof record.sendId === 'string' ? record.sendId : null,
          answer: answer.kind ? answer : { ...answer, kind: 'answer' },
          card: record.card as CardDoc,
          share: { ...PENDING },
          doorbell: { ...PENDING },
        };
        entries.push(entry);
        byId.set(entry.id, entry);
        // A crash between a record and its send line left a send with no summary, which the ring
        // names. Rebuilt from its records, the retry writes it; a send line further on replaces this.
        if (entry.sendId) {
          const send = this.sends.get(entry.sendId);
          if (send) send.entryIds.push(entry.id);
          else
            this.sends.set(entry.sendId, {
              sendId: entry.sendId,
              at: typeof record.at === 'string' ? record.at : entry.answer.answeredAt,
              device: entry.answer.device,
              entryIds: [entry.id],
              summary: { ...PENDING },
            });
        }
        if (typeof record.entryId === 'string' && typeof record.entryHash === 'string')
          this.rememberDraft(
            record.entryId,
            typeof record.at === 'string' ? Date.parse(record.at) : 0,
            record.entryHash
          );
      } else if (record.type === 'send' && typeof record.sendId === 'string') {
        this.sends.set(record.sendId, {
          sendId: record.sendId,
          at: typeof record.at === 'string' ? record.at : '',
          device: typeof record.device === 'string' ? record.device : 'browser',
          entryIds: Array.isArray(record.entryIds)
            ? record.entryIds.filter((id): id is string => typeof id === 'string')
            : [],
          summary: { ...PENDING },
        });
        if (typeof record.clientSendId === 'string' && Array.isArray(record.results))
          this.remember(
            record.clientSendId,
            typeof record.at === 'string' ? Date.parse(record.at) : 0,
            record.results as EntryResult[],
            typeof record.entriesHash === 'string' ? record.entriesHash : null
          );
      } else if (record.type === 'delivery' && typeof record.id === 'string') {
        if (record.step === 'summary') {
          const send = this.sends.get(record.id);
          if (send) send.summary = stepFrom(record, send.summary);
          continue;
        }
        const entry = byId.get(record.id);
        const step = record.step === 'share' || record.step === 'doorbell' ? record.step : null;
        if (!entry || !step) continue;
        entry[step] = stepFrom(record, entry[step]);
      }
    }
    this.entries = entries;
    return entries;
  }

  /** Newest first. */
  history(limit = 50): AnswerHistoryEntry[] {
    return this.list().slice().reverse().slice(0, Math.max(0, limit));
  }

  /** The sends, oldest first. */
  sendList(): SendRecord[] {
    this.list();
    return [...this.sends.values()];
  }

  /** Whether this exact card version already has an answer or a done (notes and objections leave it open). */
  isAnswered(cardId: string, seq: number, sha256: string): boolean {
    return this.list().some(
      (entry) =>
        CLOSING_KINDS.has(entry.answer.kind) &&
        entry.answer.cardId === cardId &&
        entry.answer.cardSeq === seq &&
        entry.answer.cardSha256 === sha256
    );
  }

  private answerKey(): AnswerKey {
    if (!this.key) this.key = loadOrCreateAnswerKey(this.storeDir());
    return this.key;
  }

  private record(line: Record<string, unknown>): void {
    try {
      appendLine(this.storeFile(), line);
    } catch {
      /* The in-memory history still shows it; the next retry records again. */
    }
  }

  private recordStep(
    entry: AnswerHistoryEntry,
    step: StepName,
    state: StepState['state'],
    detail: string
  ) {
    const at = new Date(this.now()).toISOString();
    this.record({ type: 'delivery', id: entry.id, step, state, at, detail });
    entry[step] = { state, at, detail, attempts: entry[step].attempts + 1 };
    this.notifyChange();
  }

  private rememberDraft(entryId: string, at: number, hash: string): void {
    this.drafts.delete(entryId);
    this.drafts.set(entryId, { at, hash });
    const now = this.now();
    for (const [oldest, held] of this.drafts) {
      if (this.drafts.size <= MIN_REMEMBERED_ENTRIES || now - held.at < REMEMBER_ENTRY_MS) break;
      this.drafts.delete(oldest);
    }
  }

  private remember(
    clientSendId: string,
    at: number,
    results: EntryResult[],
    entriesHash: string | null
  ): void {
    this.replays.delete(clientSendId);
    this.replays.set(clientSendId, { at, results, entriesHash });
    for (const oldest of this.replays.keys()) {
      if (this.replays.size <= MAX_REMEMBERED_SENDS) break;
      this.replays.delete(oldest);
    }
  }

  /**
   * What an earlier send with this client id answered, while it is still remembered and only for the
   * same entries (draft ids included): the id is the client's claim, and different entries under it
   * are a new send. Sends stored before the hash existed cannot be compared and count as the same.
   */
  private replayOf(clientSendId: string, entriesHash: string): EntryResult[] | null {
    this.list();
    const held = this.replays.get(clientSendId);
    if (!held || this.now() - held.at >= REMEMBER_SEND_MS) return null;
    return held.entriesHash === null || held.entriesHash === entriesHash ? held.results : null;
  }

  /**
   * One signed file under `<reports>/v1/answers/`, after making sure both public key files are
   * current. The share's `v1` folder must already exist. A write that outlives its deadline is
   * dropped before its rename, so it can never land over what was written since.
   */
  private async writeShareFile(
    name: string,
    body: string,
    limitMs: number,
    wanted: () => boolean = () => true
  ): Promise<void> {
    const v1 = path.join(this.reportsRoot(), 'v1');
    let expired = false;
    const proceed = () => !expired && wanted();
    await withDeadline(
      (async () => {
        const stat = await fs.promises.stat(v1).catch(() => null);
        if (!stat?.isDirectory()) throw new ShareUnavailableError();
        const key = this.answerKey();
        // Verifiers try keys/<keyId>.pub first, so a rotation keeps old answers checkable.
        for (const file of [
          path.join(v1, 'keys', 'aac-answers.pub'),
          path.join(v1, 'keys', `${key.keyId}.pub`),
        ]) {
          const current = await readRegularFile(file, 64 * 1024)
            .then((bytes) => bytes.toString('utf8'))
            .catch(() => '');
          if (current !== key.publicKeyPem) await writeAtomic(v1, file, key.publicKeyPem, proceed);
        }
        await writeAtomic(v1, path.join(v1, 'answers', name), body, proceed);
      })(),
      limitMs,
      () => {
        expired = true;
      },
      'Status answer share write'
    );
  }

  /**
   * Whether a later record closes the same card. Answers and dones share one file, `<card>.json`, so
   * writing this record now would put the older answer back over the newer one.
   */
  private superseded(entry: AnswerHistoryEntry): boolean {
    if (!CLOSING_KINDS.has(entry.answer.kind)) return false;
    const all = this.list();
    return all
      .slice(all.indexOf(entry) + 1)
      .some(
        (later) =>
          CLOSING_KINDS.has(later.answer.kind) && later.answer.cardId === entry.answer.cardId
      );
  }

  private markSuperseded(entry: AnswerHistoryEntry): void {
    this.recordStep(entry, 'share', 'skipped', SUPERSEDED_DETAIL);
    if (entry.doorbell.state !== 'done')
      this.recordStep(entry, 'doorbell', 'skipped', SUPERSEDED_DETAIL);
  }

  /** Writes one record's signed copy to the share. */
  private async writeShare(entry: AnswerHistoryEntry, limitMs: number): Promise<boolean> {
    if (this.superseded(entry)) {
      this.markSuperseded(entry);
      return false;
    }
    try {
      await this.writeShareFile(
        shareFileName(entry.answer, entry.id),
        `${JSON.stringify(entry.answer, null, 2)}\n`,
        limitMs,
        () => !this.superseded(entry)
      );
      this.recordStep(entry, 'share', 'done', 'Signed copy written to the share.');
      return true;
    } catch (error) {
      if (error instanceof ShareWriteDroppedError && this.superseded(entry))
        this.markSuperseded(entry);
      else
        this.recordStep(
          entry,
          'share',
          'failed',
          error instanceof ShareUnavailableError
            ? SHARE_UNAVAILABLE_DETAIL
            : 'The signed copy could not be written to the share.'
        );
      return false;
    }
  }

  /** Writes a send's signed summary to `answers/batches/<sendId>.json`. */
  private async writeSummary(send: SendRecord, limitMs: number): Promise<boolean> {
    const byId = new Map(this.list().map((entry) => [entry.id, entry]));
    const answers = send.entryIds.flatMap((id) => {
      const entry = byId.get(id);
      return entry ? [entry.answer] : [];
    });
    const at = new Date(this.now()).toISOString();
    let state: StepState['state'] = 'done';
    let detail = 'Signed send summary written to the share.';
    try {
      const signed = signDocument(
        sendSummary(send.sendId, send.at, send.device, answers),
        this.answerKey()
      );
      await this.writeShareFile(
        `batches/${send.sendId}.json`,
        `${JSON.stringify(signed, null, 2)}\n`,
        limitMs
      );
    } catch {
      state = 'failed';
      detail = 'The signed send summary could not be written to the share.';
    }
    this.record({ type: 'delivery', id: send.sendId, step: 'summary', state, at, detail });
    send.summary = { state, at, detail, attempts: send.summary.attempts + 1 };
    return state === 'done';
  }

  /**
   * Rings each asker once for its records: records are grouped by send (or alone, for records from
   * before sends) and by the card's pm-mail name. A record whose signed copy is not on the share
   * yet waits for its retry.
   */
  private async ringGroups(entries: AnswerHistoryEntry[], budget?: Budget): Promise<void> {
    const groups = new Map<
      string,
      { to: string; sendId: string | null; entries: AnswerHistoryEntry[] }
    >();
    for (const entry of entries) {
      if (entry.share.state !== 'done' || this.ringing.has(entry.id)) continue;
      if (entry.doorbell.state === 'done' || entry.doorbell.state === 'skipped') continue;
      const to = entry.card.replyTo?.pmMail;
      if (!to || !isPmMailName(to)) {
        this.recordStep(entry, 'doorbell', 'skipped', NO_MAIL_DETAIL);
        continue;
      }
      if (!this.deps.doorbell && process.env.AAC_STATUS_DOORBELL === 'off') {
        this.recordStep(entry, 'doorbell', 'skipped', DOORBELL_OFF_DETAIL);
        continue;
      }
      const key = `${entry.sendId ?? entry.id}\n${to}`;
      const group = groups.get(key) ?? { to, sendId: entry.sendId, entries: [] };
      group.entries.push(entry);
      groups.set(key, group);
    }
    // Every group is claimed before the first await, so a retry starting during the first ring
    // cannot ring a later group as well.
    const claimed = [...groups.values()].flatMap((group) => group.entries);
    for (const entry of claimed) this.ringing.add(entry.id);
    try {
      for (const group of groups.values()) {
        if (budget && budget.left() <= 0) break;
        const records: SentRecord[] = group.entries.map((entry) => ({
          answer: entry.answer,
          card: entry.card,
          file: shareFileName(entry.answer, entry.id),
        }));
        let result: RingResult;
        try {
          result = await withDeadline(
            (this.deps.doorbell ?? pmMailDoorbell())(
              group.to,
              ringSubject(records),
              ringBody(group.sendId, records, this.answerKey().keyId)
            ),
            this.timing.ringMs,
            undefined,
            'Status answer ring'
          );
        } catch {
          result = { ok: false, detail: 'pm-mail failed.' };
        }
        for (const entry of group.entries)
          this.recordStep(entry, 'doorbell', result.ok ? 'done' : 'failed', result.detail);
      }
    } finally {
      for (const entry of claimed) this.ringing.delete(entry.id);
    }
  }

  /** Store, sign and share one record. Must run inside `serial`. */
  private async answerCard(
    parsed: ParsedReport<CardDoc>,
    choice: string[],
    note: string,
    device: string,
    via: UnsignedAnswer['via'],
    kind: AnswerKind,
    sendId: string,
    budget: Budget,
    recorded: AnswerHistoryEntry[],
    draft: { entryId: string; hash: string } | null
  ): Promise<AnswerHistoryEntry | null> {
    const unsigned: UnsignedAnswer = {
      schema: ANSWER_SCHEMA,
      cardId: parsed.doc.id,
      cardSeq: parsed.doc.seq,
      cardSha256: parsed.sha256,
      kind,
      choice,
      note,
      via,
      answeredAt: new Date(this.now()).toISOString(),
      answeredBy: 'owner',
      device,
    };
    let entry: AnswerHistoryEntry;
    try {
      entry = {
        id: randomUUID(),
        sendId,
        answer: signAnswer(unsigned, this.answerKey()),
        card: parsed.doc,
        share: { ...PENDING },
        doorbell: { ...PENDING },
      };
      this.list();
      appendLine(this.storeFile(), {
        type: 'answer',
        id: entry.id,
        sendId,
        at: unsigned.answeredAt,
        answer: entry.answer,
        card: entry.card,
        ...(draft ? { entryId: draft.entryId, entryHash: draft.hash } : {}),
      });
    } catch {
      return null;
    }
    // No await since the line was written: a retry sees the record only with its claim on it.
    this.list().push(entry);
    this.ringing.add(entry.id);
    recorded.push(entry);
    if (draft) this.rememberDraft(draft.entryId, this.now(), draft.hash);
    this.notifyChange();
    // Out of time, the record stays pending and the retry writes it: it is already stored.
    const left = budget.left();
    if (left > 0) await this.writeShare(entry, Math.min(this.timing.shareMs, left));
    return entry;
  }

  /**
   * The card if it is open on disk at exactly this version and not yet answered, null if it is not,
   * `no_answer` if the share cannot say in time. A hung share uses up the send's budget at once: the
   * entries after this one would only wait for the same share.
   */
  private async openCard(id: string, seq: number, sha256: string, budget: Budget) {
    if (!isCardId(id)) return null;
    const left = budget.left();
    if (left <= 0) return 'no_answer' as const;
    let parsed: ParsedReport<CardDoc> | null;
    try {
      parsed = await withDeadline(
        this.deps.readCard(id),
        Math.min(this.timing.readMs, left),
        undefined,
        'Status card read'
      );
    } catch (error) {
      if (error instanceof DeadlineExceededError) budget.spend();
      return 'no_answer' as const;
    }
    if (!parsed || parsed.doc.state !== 'open') return null;
    if (parsed.doc.seq !== seq || parsed.sha256 !== sha256) return null;
    if (this.isAnswered(id, seq, sha256)) return null;
    return parsed;
  }

  /** One entry of a set; must run inside `serial`. */
  private async recordEntry(
    raw: unknown,
    device: string,
    sendId: string,
    recorded: AnswerHistoryEntry[],
    budget: Budget
  ): Promise<EntryResult> {
    const entry = parseAnswerEntry(raw);
    const named = (raw as { cardId?: unknown } | null)?.cardId;
    if (!entry) return refused(isCardId(named) ? named : null, 'invalid_entry');
    const note = normalizeNote(entry.note);
    if (note === null) return refused(entry.cardId, 'invalid_note');
    // Before the card is read: a retried answer's card has closed since, and that is no refusal.
    this.list();
    const hash = entry.entryId ? draftHash(entry, note) : null;
    const held = entry.entryId ? this.drafts.get(entry.entryId) : undefined;
    if (held) {
      if (held.hash !== hash) return refused(entry.cardId, 'invalid_entry');
      return { cardId: entry.cardId, outcome: 'recorded', code: null, error: null };
    }
    const parsed = await this.openCard(entry.cardId, entry.cardSeq, entry.cardSha256, budget);
    if (parsed === 'no_answer') return refused(entry.cardId, 'share_unavailable');
    if (!parsed) return refused(entry.cardId, 'card_changed');
    if (!validChoice(parsed.doc, entry.choice)) return refused(entry.cardId, 'invalid_choice');
    const kind = entryKind(parsed.doc, entry, note);
    if (!RECORD_KINDS.includes(kind)) return refused(entry.cardId, kind as RefusalCode);
    if (
      entry.via === 'recommended' &&
      (kind !== 'answer' ||
        !parsed.doc.recommendation ||
        entry.choice.length !== 1 ||
        entry.choice[0] !== parsed.doc.recommendation.key)
    )
      return refused(entry.cardId, 'invalid_via');
    const done = await this.answerCard(
      parsed,
      entry.choice,
      note,
      device,
      entry.via,
      kind as AnswerKind,
      sendId,
      budget,
      recorded,
      entry.entryId && hash ? { entryId: entry.entryId, hash } : null
    );
    if (!done) return refused(entry.cardId, 'answer_not_saved');
    return { cardId: entry.cardId, outcome: 'recorded', code: null, error: null };
  }

  /**
   * An answer set from the deck: each entry validated and recorded on its own, one result per
   * entry in order. Then the signed send summary, then one ring per asker.
   *
   * `clientSendId` is the client's retry key, kept apart from `sendId`, the server's own id that
   * names and signs the batch. The look-up runs inside the queue, so a duplicate that arrives while
   * the first send is still running waits for it and answers from its results: nothing is recorded,
   * written or rung again. A send that recorded nothing is not remembered; a retry may yet succeed.
   * Each entry's own `entryId` is the finer guard: see `recordEntry`.
   */
  async submitSet(
    entries: unknown[],
    device: string,
    clientSendId: string | null = null
  ): Promise<EntryResult[]> {
    const sendId = randomUUID();
    const recorded: AnswerHistoryEntry[] = [];
    const entriesHash = clientSendId ? contentHash(entries) : null;
    try {
      return await this.serial(async () => {
        const replay =
          clientSendId && entriesHash ? this.replayOf(clientSendId, entriesHash) : null;
        if (replay) return replay;
        const budget = startBudget(this.timing.sendBudgetMs);
        const out: EntryResult[] = [];
        for (const raw of entries) {
          const before = recorded.length;
          try {
            out.push(await this.recordEntry(raw, device, sendId, recorded, budget));
          } catch {
            // Whatever went wrong, the other entries still get their answers. A record that exists
            // says so: its delivery is retried like any other.
            const named = (raw as { cardId?: unknown } | null)?.cardId;
            const cardId = isCardId(named) ? named : null;
            out.push(
              recorded.length > before
                ? { cardId, outcome: 'recorded', code: null, error: null }
                : refused(cardId, 'answer_not_saved')
            );
          }
        }
        if (recorded.length > 0) {
          const send: SendRecord = {
            sendId,
            at: new Date(this.now()).toISOString(),
            device,
            entryIds: recorded.map((entry) => entry.id),
            summary: { ...PENDING },
          };
          this.record({
            type: 'send',
            sendId,
            at: send.at,
            device,
            entryIds: send.entryIds,
            ...(clientSendId ? { clientSendId, results: out, entriesHash } : {}),
          });
          this.sends.set(sendId, send);
          if (clientSendId) this.remember(clientSendId, this.now(), out, entriesHash);
          // Out of time, the summary stays pending and the retry writes it.
          const left = budget.left();
          if (left > 0) await this.writeSummary(send, Math.min(this.timing.shareMs, left));
        }
        return out;
      });
    } finally {
      // The claim passes to the ring in one step: it takes its records back before its first await.
      for (const entry of recorded) this.ringing.delete(entry.id);
      if (recorded.length > 0) void this.ringGroups(recorded).catch(() => undefined);
    }
  }

  /** Retries every share write, send summary and ring that has not succeeded and has attempts left. */
  async retryPending(): Promise<void> {
    if (this.retrying) return;
    this.retrying = true;
    try {
      // A hung share spends this pass's budget; what is left waits for the next pass, uncounted.
      const budget = startBudget(this.timing.retryBudgetMs);
      const shareLimit = () => Math.min(this.timing.shareMs, budget.left());
      for (const entry of this.list()) {
        const wanted = () =>
          (entry.share.state === 'pending' || entry.share.state === 'failed') &&
          entry.share.attempts < MAX_DELIVERY_ATTEMPTS;
        if (!wanted()) continue;
        await this.serial(async () => {
          // Asked again in the queue: a send that was running may have written it meanwhile.
          if (wanted() && shareLimit() > 0) await this.writeShare(entry, shareLimit());
        });
      }
      for (const send of this.sendList()) {
        const wanted = () =>
          send.summary.state !== 'done' && send.summary.attempts < MAX_DELIVERY_ATTEMPTS;
        if (!wanted()) continue;
        await this.serial(async () => {
          if (wanted() && shareLimit() > 0) await this.writeSummary(send, shareLimit());
        });
      }
      await this.ringGroups(
        this.list().filter(
          (entry) =>
            (entry.doorbell.state === 'pending' || entry.doorbell.state === 'failed') &&
            entry.doorbell.attempts < MAX_DELIVERY_ATTEMPTS
        ),
        budget
      );
    } finally {
      this.retrying = false;
    }
  }
}
