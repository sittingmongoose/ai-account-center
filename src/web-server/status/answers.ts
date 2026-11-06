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
import { randomBytes, randomUUID } from 'crypto';
import fs from 'fs';
import path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { withDeadline } from '../../utils/settle-deadline';
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
} from './answer-signing';
import {
  ANSWER_SCHEMA,
  isCardId,
  isPmMailName,
  type CardDoc,
  type ParsedReport,
} from './report-schema';

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
const SHARE_DEADLINE_MS = 10_000;

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

/** Temporary dot file in the same folder, flushed, then renamed over the target (async: a share can hang). */
async function writeAtomic(file: string, body: string): Promise<void> {
  await fs.promises.mkdir(path.dirname(file), { recursive: true });
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
  if (share.state === 'failed')
    return {
      ...base,
      state: 'failed',
      at: share.at,
      detail:
        share.attempts >= MAX_DELIVERY_ATTEMPTS
          ? `The signed copy could not be written to the share after ${share.attempts} attempts.`
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
}

const RECORD_KINDS: readonly string[] = ['answer', 'note', 'object', 'done'];

export class StatusAnswers {
  private entries: AnswerHistoryEntry[] | null = null;
  private readonly sends = new Map<string, SendRecord>();
  private queue: Promise<unknown> = Promise.resolve();
  private key: AnswerKey | null = null;
  private retrying = false;
  /** Records whose ring is running now, so a retry never rings twice at once. */
  private readonly ringing = new Set<string>();

  constructor(private readonly deps: StatusAnswersDeps) {}

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
    this.deps.onChange?.();
  }

  /** One signed file under `<reports>/v1/answers/`, after making sure both public key files are current. */
  private async writeShareFile(name: string, body: string): Promise<void> {
    const v1 = path.join(this.reportsRoot(), 'v1');
    await withDeadline(
      (async () => {
        const key = this.answerKey();
        // Verifiers try keys/<keyId>.pub first, so a rotation keeps old answers checkable.
        for (const file of [
          path.join(v1, 'keys', 'aac-answers.pub'),
          path.join(v1, 'keys', `${key.keyId}.pub`),
        ]) {
          const current = await fs.promises.readFile(file, 'utf8').catch(() => '');
          if (current !== key.publicKeyPem) await writeAtomic(file, key.publicKeyPem);
        }
        await writeAtomic(path.join(v1, 'answers', name), body);
      })(),
      SHARE_DEADLINE_MS,
      undefined,
      'Status answer share write'
    );
  }

  /** Writes one record's signed copy to the share. */
  private async writeShare(entry: AnswerHistoryEntry): Promise<boolean> {
    try {
      await this.writeShareFile(
        shareFileName(entry.answer, entry.id),
        `${JSON.stringify(entry.answer, null, 2)}\n`
      );
      this.recordStep(entry, 'share', 'done', 'Signed copy written to the share.');
      return true;
    } catch {
      this.recordStep(
        entry,
        'share',
        'failed',
        'The signed copy could not be written to the share.'
      );
      return false;
    }
  }

  /** Writes a send's signed summary to `answers/batches/<sendId>.json`. */
  private async writeSummary(send: SendRecord): Promise<boolean> {
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
        `${JSON.stringify(signed, null, 2)}\n`
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
  private async ringGroups(entries: AnswerHistoryEntry[]): Promise<void> {
    const groups = new Map<
      string,
      { to: string; sendId: string | null; entries: AnswerHistoryEntry[] }
    >();
    for (const entry of entries) {
      if (entry.share.state !== 'done' || this.ringing.has(entry.id)) continue;
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
    for (const group of groups.values()) {
      for (const entry of group.entries) this.ringing.add(entry.id);
      try {
        const records: SentRecord[] = group.entries.map((entry) => ({
          answer: entry.answer,
          card: entry.card,
          file: shareFileName(entry.answer, entry.id),
        }));
        let result: RingResult;
        try {
          result = await (this.deps.doorbell ?? pmMailDoorbell())(
            group.to,
            ringSubject(records),
            ringBody(group.sendId, records, this.answerKey().keyId)
          );
        } catch {
          result = { ok: false, detail: 'pm-mail failed.' };
        }
        for (const entry of group.entries)
          this.recordStep(entry, 'doorbell', result.ok ? 'done' : 'failed', result.detail);
      } finally {
        for (const entry of group.entries) this.ringing.delete(entry.id);
      }
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
    sendId: string
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
      });
    } catch {
      return null;
    }
    this.list().push(entry);
    this.deps.onChange?.();
    await this.writeShare(entry);
    return entry;
  }

  /** The card if it is open on disk at exactly this version and not yet answered. */
  private async openCard(id: string, seq: number, sha256: string) {
    if (!isCardId(id)) return null;
    const parsed = await this.deps.readCard(id);
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
    recorded: AnswerHistoryEntry[]
  ): Promise<EntryResult> {
    const entry = parseAnswerEntry(raw);
    const named = (raw as { cardId?: unknown } | null)?.cardId;
    if (!entry) return refused(isCardId(named) ? named : null, 'invalid_entry');
    const note = normalizeNote(entry.note);
    if (note === null) return refused(entry.cardId, 'invalid_note');
    const parsed = await this.openCard(entry.cardId, entry.cardSeq, entry.cardSha256);
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
      sendId
    );
    if (!done) return refused(entry.cardId, 'answer_not_saved');
    recorded.push(done);
    return { cardId: entry.cardId, outcome: 'recorded', code: null, error: null };
  }

  /**
   * An answer set from the deck: each entry validated and recorded on its own, one result per
   * entry in order. Then the signed send summary, then one ring per asker.
   */
  async submitSet(entries: unknown[], device: string): Promise<EntryResult[]> {
    const sendId = randomUUID();
    const recorded: AnswerHistoryEntry[] = [];
    const results = await this.serial(async () => {
      const out: EntryResult[] = [];
      for (const raw of entries) out.push(await this.recordEntry(raw, device, sendId, recorded));
      if (recorded.length > 0) {
        const send: SendRecord = {
          sendId,
          at: new Date(this.now()).toISOString(),
          device,
          entryIds: recorded.map((entry) => entry.id),
          summary: { ...PENDING },
        };
        this.record({ type: 'send', sendId, at: send.at, device, entryIds: send.entryIds });
        this.sends.set(sendId, send);
        await this.writeSummary(send);
      }
      return out;
    });
    if (recorded.length > 0) void this.ringGroups(recorded).catch(() => undefined);
    return results;
  }

  /** Retries every share write, send summary and ring that has not succeeded and has attempts left. */
  async retryPending(): Promise<void> {
    if (this.retrying) return;
    this.retrying = true;
    try {
      for (const entry of this.list()) {
        if (entry.share.state !== 'done' && entry.share.attempts < MAX_DELIVERY_ATTEMPTS)
          await this.serial(() => this.writeShare(entry));
      }
      for (const send of this.sendList()) {
        if (send.summary.state !== 'done' && send.summary.attempts < MAX_DELIVERY_ATTEMPTS)
          await this.serial(() => this.writeSummary(send));
      }
      await this.ringGroups(
        this.list().filter(
          (entry) =>
            (entry.doorbell.state === 'pending' || entry.doorbell.state === 'failed') &&
            entry.doorbell.attempts < MAX_DELIVERY_ATTEMPTS
        )
      );
    } finally {
      this.retrying = false;
    }
  }
}
