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
  verifySignedAnswer,
} from './answer-signing';
import {
  ANSWER_SCHEMA,
  isCardId,
  isPmMailName,
  type CardDoc,
  type ParsedReport,
} from './report-schema';
import { readRegularFile } from './share-read';
import { findSecretIn } from './schema-helpers';
import { findTypedSecret } from './typed-secrets';
import { holdWriterLock, WriterBusyError } from './writer-lock';
import { registerCleanup } from '../../errors/cleanup-registry';
import { ConfigError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';

const logger = createLogger('status-answers');

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
/** The share already holds this card's signed answer to a higher version. */
class ShareHasNewerError extends Error {}
/**
 * A record was refused and the disk refused its `void` line too: the line may be in the journal, uncancelled.
 * The store holds `line` as voided and keeps writing its void line (see StatusAnswers.unvoided).
 */
class AppendUnclearError extends ConfigError {
  constructor(
    readonly line: string,
    file: string
  ) {
    super('The answer store refused a record and could not cancel it.', file);
  }
}

/** Appends one `void` line cancelling `line` and syncs it. Throws if the disk refuses either. */
function appendVoid(fd: number, line: string, file: string): void {
  const cancel = Buffer.from(`\n${JSON.stringify({ type: 'void', line })}\n`);
  for (let done = 0; done < cancel.length; ) {
    const taken = fs.writeSync(fd, cancel, done, cancel.length - done);
    if (taken <= 0) throw new ConfigError('The answer store took no part of a void line.', file);
    done += taken;
  }
  fs.fsyncSync(fd);
}

/** Opens the store, appends one void line for `line` and closes it; throws if it did not land. */
function appendVoidLine(file: string, line: string): void {
  const fd = fs.openSync(file, 'a+', 0o600);
  try {
    appendVoid(fd, line, file);
  } finally {
    try {
      fs.closeSync(fd);
    } catch {
      /* the void is synced or the write already threw; a close error changes neither */
    }
  }
}

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
async function mkdirBelow(v1: string, dir: string, check: () => void): Promise<void> {
  let current = v1;
  for (const segment of path.relative(v1, dir).split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    await fs.promises.mkdir(current).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== 'EEXIST') throw error;
    });
    check();
  }
}

/**
 * Temporary dot file in the same folder, flushed, then renamed over the target. Preparation is async;
 * the final rename is synchronous so no await separates it from the ownership-generation check.
 * A share that wakes after the deadline or a lock handoff cannot publish an obsolete attempt.
 */
async function writeAtomic(
  v1: string,
  file: string,
  body: string,
  proceed: () => boolean,
  beforeRename?: () => Promise<void>
): Promise<void> {
  const check = () => {
    if (!proceed()) throw new ShareWriteDroppedError();
  };
  await mkdirBelow(v1, path.dirname(file), check);
  check();
  const temp = path.join(
    path.dirname(file),
    `.${path.basename(file)}.tmp-${randomBytes(6).toString('hex')}`
  );
  try {
    const handle = await fs.promises.open(temp, 'wx', 0o644);
    try {
      check();
      await handle.writeFile(body);
      check();
      await handle.sync();
      check();
    } finally {
      await handle.close();
    }
    check();
    await beforeRename?.();
    // No asynchronous gap between the ownership-generation check and publication.
    check();
    fs.renameSync(temp, file);
  } catch (error) {
    await fs.promises.rm(temp, { force: true }).catch(() => undefined);
    throw error;
  }
}

/**
 * Appends one record to the store. The store is only ever appended to, never cut or overwritten, so no writer can
 * remove or damage a record another wrote: each line carries its own id (`line`), and a record that fails to be
 * written whole and synced is cancelled by a `void` line naming that id, which the loader honours.
 *
 * The append is committed once its fsync succeeds: the line is then durable and uncancelled, and every reader
 * after a restart counts it. A close error after that changes nothing (it is logged), so a durable record is
 * never reported unsaved while it stays in the journal, where a retry of the same draft would record it again.
 *
 * A refused record whose void line the disk refuses too throws AppendUnclearError naming the line: the caller
 * holds it as voided and writes the void again later (StatusAnswers.unvoided).
 */
function appendLine(file: string, record: Record<string, unknown>): void {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const fd = fs.openSync(file, 'a+', 0o600);
  try {
    // Every record starts on a line of its own: a torn line (a crash, or another writer's failed part) can never
    // swallow it. Blank lines are skipped on load.
    const line = randomBytes(8).toString('hex');
    const data = Buffer.from(`\n${JSON.stringify({ ...record, line })}\n`);
    let written = 0;
    try {
      // One write, taken whole or refused: a second part could land after another writer's line and split this one,
      // so a short write (a file-size limit, a full disk) refuses the record instead of finishing it.
      written = fs.writeSync(fd, data, 0, data.length);
      if (written !== data.length)
        throw new ConfigError('The answer store took part of a record.', file);
      fs.fsyncSync(fd);
    } catch (error) {
      // A refused record must not count after a restart (it would ring): cancel it. The leading newline ends whatever
      // part of it was written. If the disk refuses this too, the caller is told so and writes the void again.
      if (written > 0) {
        try {
          appendVoid(fd, line, file);
        } catch {
          throw new AppendUnclearError(line, file);
        }
      }
      throw error;
    }
  } finally {
    try {
      fs.closeSync(fd);
    } catch (error) {
      // Committed or refused, the outcome is already decided above; a close error cannot change it.
      try {
        logger.warn('status.answer_store_close_failed', 'The answer store could not be closed', {
          code: (error as NodeJS.ErrnoException | null)?.code ?? null,
        });
      } catch {
        /* Logging never changes a record's outcome. */
      }
    }
  }
}

const PENDING: StepState = Object.freeze({ state: 'pending', at: null, detail: null, attempts: 0 });

/** Of two readings of one delivery step, the one further along: done, else the one with more attempts. */
function furthest(a: StepState, b: StepState): StepState {
  if (a.state === 'done') return a;
  if (b.state === 'done') return b;
  return a.attempts >= b.attempts ? a : b;
}

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

interface Replay {
  at: number;
  results: EntryResult[];
  entriesHash: string | null;
  partial: boolean;
}

/**
 * The client retry key an answer line carries, so a restart can rebuild the send's deduplication from the answer
 * lines alone, without its send line: the request's id and entries hash, this entry's place in the request, and
 * the results of the entries since the send's previous stored record (none of which was stored).
 */
interface RequestKey {
  clientSendId: string;
  entriesHash: string;
  entryIndex: number;
  priorResults: EntryResult[];
}

function isEntryResult(value: unknown): value is EntryResult {
  const result = value as EntryResult | null;
  return (
    !!result &&
    typeof result === 'object' &&
    (result.cardId === null || typeof result.cardId === 'string') &&
    typeof result.outcome === 'string' &&
    (result.code === null || typeof result.code === 'string') &&
    (result.error === null || typeof result.error === 'string')
  );
}

export class StatusAnswers {
  private entries: AnswerHistoryEntry[] | null = null;
  /**
   * This process's own delivery objects (see holdsEntry), kept across failed reads and reads made without the store;
   * never served as authoritative history. A record another process wrote is never held: the journal has it.
   */
  private readonly heldEntries = new Map<string, AnswerHistoryEntry>();
  /** Ids of the answer lines this process appended and stored (fsync succeeded). */
  private readonly written = new Set<string>();
  /** Ids whose share write this process is running. */
  private readonly sharing = new Set<string>();
  /** Send ids this process created, or whose summary it is writing. */
  private readonly ownSends = new Set<string>();
  private readonly summarizing = new Set<string>();
  /** Record ids the last journal fold found cancelled by a void line. */
  private voidedIds = new Set<string>();
  /**
   * Line ids this process refused whose void line has not landed yet (the disk refused it too). Every fold treats
   * them as voided, and the void is written again on each gain, retry pass and send, and on close and exit, like
   * a pending completion. Once it lands the journal itself cancels the line.
   */
  private readonly unvoided = new Set<string>();
  private historyUnavailable = false;
  /** The journal's size, mtime and inode when `entries` was read; a non-owner reads it again once it changes. */
  private loadedFrom: string | null = null;
  private readonly heldSends = new Map<string, SendRecord>();
  /** Successful effects awaiting a durable completion line; repair persistence without repeating them. */
  private readonly pendingCompletions = new Map<
    string,
    {
      line: Record<string, unknown>;
      target: AnswerHistoryEntry | SendRecord;
      step: StepName | 'summary';
      value: StepState;
    }
  >();
  private readonly sends = new Map<string, SendRecord>();
  private queue: Promise<unknown> = Promise.resolve();
  private key: AnswerKey | null = null;
  private retrying = false;
  /** Whether this instance holds the store's writer lock (see writer()). */
  private owner = false;
  /** Counts the times this instance gained the store; a send started under one count records only under it. */
  private epoch = 0;
  /** The count a send in progress started under (sends run one at a time, see serial()). */
  private sendEpoch: number | null = null;
  /**
   * Records a retry must not ring: from the moment a send creates them until its own ring is over
   * (the send rings them itself, whole), and while any ring of them is running.
   */
  private readonly ringing = new Set<string>();
  /**
   * Client send id -> what that send answered, oldest first. Never the server's own batch id. `partial` marks
   * results rebuilt from answer lines alone (the send line never landed): they cover the request up to its last
   * stored record, and the entries after it, none of which was recorded, are judged again.
   */
  private readonly replays = new Map<string, Replay>();
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
    if (this.historyUnavailable) {
      try {
        this.writer();
        return this.list();
      } catch (error) {
        if (!(error instanceof WriterBusyError)) throw error;
        // A viewer needs an authoritative read, but no writer ownership or decision authority.
      }
    }
    // The writer's cache is the history: only it appends. Anyone else (a second dashboard, or this one after
    // losing the store) reads the journal again whenever it changed, so a viewer never serves its first
    // snapshot forever.
    const from = this.owner && this.entries ? this.loadedFrom : this.journalStamp();
    if (this.entries && (this.owner || (from !== null && from === this.loadedFrom)))
      return this.entries;
    if (this.entries) {
      // The fresh read replaces this read-only history. Only this process's own delivery objects stay attached (the
      // next gain merges them back by id); a record it merely read is never held, so a cancelled one cannot return.
      this.holdOwn(this.entries);
      this.entries = null;
    }
    let text = '';
    try {
      text = fs.readFileSync(this.storeFile(), 'utf8');
    } catch (error) {
      // Only absence is an empty journal. Failed reads leave the cache unloaded for a later retry.
      if ((error as NodeJS.ErrnoException | null)?.code !== 'ENOENT') {
        this.historyUnavailable = true;
        throw error;
      }
    }
    for (const [id, send] of this.sends)
      if (this.holdsSend(id) && !this.heldSends.has(id)) this.heldSends.set(id, send);
    this.sends.clear();
    this.replays.clear();
    this.drafts.clear();
    const loaded = this.load(text);
    this.loadedFrom = from;
    this.historyUnavailable = false;
    return loaded;
  }

  /**
   * Whether this process still needs this record's object: it wrote the record, or a share write, ring or pending
   * completion it started still updates the object.
   */
  private holdsEntry(id: string): boolean {
    return (
      this.written.has(id) ||
      this.sharing.has(id) ||
      this.ringing.has(id) ||
      this.pendingCompletions.has(id + ':share') ||
      this.pendingCompletions.has(id + ':doorbell')
    );
  }

  private holdsSend(sendId: string): boolean {
    return (
      this.ownSends.has(sendId) ||
      this.summarizing.has(sendId) ||
      this.pendingCompletions.has(sendId + ':summary')
    );
  }

  /** Keeps this process's own delivery objects among `entries` attached; everything else is the journal's. */
  private holdOwn(entries: AnswerHistoryEntry[]): void {
    for (const held of entries)
      if (this.holdsEntry(held.id) && !this.heldEntries.has(held.id))
        this.heldEntries.set(held.id, held);
  }

  /** What identifies the journal's current contents: it is only appended to, so any change moves its size. */
  private journalStamp(): string | null {
    try {
      const stat = fs.statSync(this.storeFile());
      return `${stat.size}:${stat.mtimeMs}:${stat.ino}`;
    } catch (error) {
      // Absent is a state of its own; any other failure leaves the stamp unknown, so the read decides.
      return (error as NodeJS.ErrnoException | null)?.code === 'ENOENT' ? 'absent' : null;
    }
  }

  /** Fold an already-read journal; I/O failures cannot clear held delivery objects. */
  private load(text: string): AnswerHistoryEntry[] {
    const entries: AnswerHistoryEntry[] = [];
    const byId = new Map<string, AnswerHistoryEntry>();
    const records: Record<string, unknown>[] = [];
    const voided = new Set<string>();
    /** Client send id -> the request's results, by entry index, as far as its answer lines say. */
    const requests = new Map<string, { hash: string; slots: Array<EntryResult | undefined> }>();
    for (const line of text.split('\n')) {
      if (!line.trim()) continue;
      try {
        const record = JSON.parse(line) as Record<string, unknown>;
        if (!record || typeof record !== 'object') continue;
        if (record.type === 'void' && typeof record.line === 'string') voided.add(record.line);
        else records.push(record);
      } catch {
        /* a torn or partial line */
      }
    }
    for (const line of this.unvoided) voided.add(line);
    const voidedIds = new Set<string>();
    for (const record of records) {
      // a record its writer cancelled (it failed to be stored whole) does not count
      if (typeof record.line === 'string' && voided.has(record.line)) {
        if (typeof record.id === 'string' && record.type === 'answer') voidedIds.add(record.id);
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
        const index = record.entryIndex;
        if (
          typeof record.clientSendId === 'string' &&
          typeof record.entriesHash === 'string' &&
          typeof index === 'number' &&
          Number.isInteger(index) &&
          index >= 0 &&
          Array.isArray(record.priorResults) &&
          record.priorResults.length <= index &&
          record.priorResults.every(isEntryResult)
        ) {
          const known = requests.get(record.clientSendId);
          const request =
            known && known.hash === record.entriesHash
              ? known
              : { hash: record.entriesHash, slots: [] as Array<EntryResult | undefined> };
          requests.set(record.clientSendId, request);
          const prior = record.priorResults as EntryResult[];
          for (let k = 0; k < prior.length; k += 1) {
            const slot = index - prior.length + k;
            if (!request.slots[slot]) request.slots[slot] = prior[k];
          }
          // A stored record always answers recorded: it is in the journal, whatever an earlier line said.
          request.slots[index] = {
            cardId: entry.answer.cardId,
            outcome: 'recorded',
            code: null,
            error: null,
          };
          const results: EntryResult[] = [];
          for (const slot of request.slots) {
            if (!slot) break;
            results.push(slot);
          }
          this.remember(
            record.clientSendId,
            typeof record.at === 'string' ? Date.parse(record.at) : 0,
            results,
            record.entriesHash,
            true
          );
        }
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
        if (typeof record.clientSendId === 'string' && Array.isArray(record.results)) {
          const results = record.results as EntryResult[];
          const hash = typeof record.entriesHash === 'string' ? record.entriesHash : null;
          this.remember(
            record.clientSendId,
            typeof record.at === 'string' ? Date.parse(record.at) : 0,
            results,
            hash,
            false
          );
          if (hash) requests.set(record.clientSendId, { hash, slots: [...results] });
          else requests.delete(record.clientSendId);
        }
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
    this.voidedIds = voidedIds;
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

  /** Takes or confirms the store's writer lock: no other process may append to the store or touch the key. */
  private writer(): void {
    this.flushCompletions();
    let gained: boolean;
    try {
      gained = holdWriterLock(this.storeDir());
    } catch (error) {
      this.owner = false;
      throw error;
    }
    if (gained || !this.owner) {
      // Newly the store's writer: another process may have written meanwhile, so history, sends, replays and drafts
      // are read again from the store before anything is judged against them. A record or send this process already
      // holds keeps its object (a ring or summary still running updates it), with the further-along delivery steps.
      // Read first: a failed gain must keep the objects an in-flight delivery still updates.
      // Fold this successful read only after ownership reconciliation can proceed.
      const from = this.journalStamp();
      let text: string;
      try {
        text = fs.readFileSync(this.storeFile(), 'utf8');
      } catch (error) {
        this.owner = false;
        if ((error as NodeJS.ErrnoException | null)?.code !== 'ENOENT') {
          this.holdOwn(this.entries ?? []);
          this.entries = null;
          this.historyUnavailable = true;
          throw error;
        }
        text = '';
      }
      // Only this process's own delivery objects carry over; whatever else the cache held is read again.
      this.holdOwn(this.entries ?? []);
      const heldEntries = new Map([...this.heldEntries].filter(([id]) => this.holdsEntry(id)));
      const heldSends = new Map(
        [...this.sends, ...this.heldSends].filter(([id]) => this.holdsSend(id))
      );
      this.entries = null;
      this.sends.clear();
      this.replays.clear();
      this.drafts.clear();
      this.entries = this.load(text).map((fresh) => {
        const held = heldEntries.get(fresh.id);
        if (!held) return fresh;
        heldEntries.delete(fresh.id);
        held.share = furthest(held.share, fresh.share);
        held.doorbell = furthest(held.doorbell, fresh.doorbell);
        return held;
      });
      // A record of this process's own that is absent from this reload stays attached; one the reload shows
      // cancelled, or one this process did not write, is not put back (the journal is the history).
      for (const held of heldEntries.values())
        if (this.written.has(held.id) && !this.voidedIds.has(held.id)) this.entries.push(held);
      for (const [sendId, held] of heldSends) {
        const fresh = this.sends.get(sendId);
        if (fresh) {
          held.summary = furthest(held.summary, fresh.summary);
          held.entryIds = fresh.entryIds;
        } else if (!this.ownSends.has(sendId)) continue;
        this.sends.set(sendId, held);
      }
      this.heldEntries.clear();
      this.heldSends.clear();
      this.loadedFrom = from;
      this.historyUnavailable = false;
      this.owner = true;
      this.epoch += 1;
    }
    // Even an empty in-memory store must load successfully before replay, recording or ringing.
    this.list();
    this.flushCompletions();
    if (this.pendingCompletions.size)
      throw new ConfigError('Delivery completion could not be saved.', this.storeFile());
  }

  /**
   * Completion facts require no read or decision: done stays done. Delivery is at least once:
   * a crash/kill between ringing and appending done can ring again. Receivers dedupe by the
   * file name plus signature.keyId + signature.value, stable across retries.
   */
  flushCompletions(): void {
    if (!this.pendingCompletions.size && !this.unvoided.size) return;
    try {
      if (holdWriterLock(this.storeDir())) this.owner = false;
    } catch {
      this.owner = false;
      return; // Retain the facts for the next gain; never write under another live owner.
    }
    // Cancels first: a refused record stays held as voided until its void line lands.
    for (const line of this.unvoided) {
      try {
        appendVoidLine(this.storeFile(), line);
      } catch {
        continue;
      }
      this.unvoided.delete(line);
    }
    for (const [key, pending] of this.pendingCompletions) {
      try {
        this.append(pending.line);
      } catch {
        continue;
      }
      if (pending.step === 'summary') (pending.target as SendRecord).summary = pending.value;
      else (pending.target as AnswerHistoryEntry)[pending.step] = pending.value;
      this.pendingCompletions.delete(key);
    }
    if (!this.pendingCompletions.size && !this.unvoided.size) {
      this.unregisterCompletionCleanup?.();
      this.unregisterCompletionCleanup = null;
    }
  }

  private unregisterCompletionCleanup: (() => void) | null = null;

  /**
   * Appends one line to the store. A refused line whose void the disk refused too is held as voided from here on
   * (see unvoided), and its void is written again later; the error still refuses the line.
   */
  private append(line: Record<string, unknown>): void {
    try {
      appendLine(this.storeFile(), line);
    } catch (error) {
      if (error instanceof AppendUnclearError) {
        this.unvoided.add(error.line);
        this.registerFlush();
      }
      throw error;
    }
  }

  private watchCompletions(): void {
    this.registerFlush();
    this.flushCompletions();
  }

  /** Flushes pending completions and void lines on close and exit, while either is waiting. */
  private registerFlush(): void {
    if (!this.unregisterCompletionCleanup) {
      const flush = () => this.flushCompletions();
      // Prepend so done lines land before writer-lock's exit listener releases ownership.
      process.prependOnceListener('exit', flush);
      const unregister = registerCleanup(flush); // Existing SIGTERM/SIGINT exit behavior is unchanged.
      this.unregisterCompletionCleanup = () => {
        process.off('exit', flush);
        unregister();
      };
    }
  }

  private answerKey(): AnswerKey {
    if (!this.key) {
      this.writer();
      this.key = loadOrCreateAnswerKey(this.storeDir());
    }
    return this.key;
  }

  private record(line: Record<string, unknown>): boolean {
    try {
      this.writer();
      this.append(line);
      return true;
    } catch {
      if (line.type === 'delivery' && line.state === 'done') {
        try {
          holdWriterLock(this.storeDir());
          this.append(line);
          return true;
        } catch {
          // Only failed persistence remains queued; this completion makes no cached decision.
        }
      }
      return false;
    }
  }

  private recordStep(
    entry: AnswerHistoryEntry,
    step: StepName,
    state: StepState['state'],
    detail: string
  ) {
    // The store first (gaining it reads what other writers did): a step another writer finished stays done, so a
    // late failure here neither overwrites it nor sends the retry to ring again.
    this.gainQuietly();
    if (
      state !== 'done' &&
      (entry[step].state === 'done' || this.pendingCompletions.has(entry.id + ':' + step))
    )
      return;
    const at = new Date(this.now()).toISOString();
    const line = { type: 'delivery', id: entry.id, step, state, at, detail };
    const value = { state, at, detail, attempts: entry[step].attempts + 1 };
    const key = entry.id + ':' + step;
    if (this.record(line)) entry[step] = value;
    else if (state === 'done')
      this.pendingCompletions.set(key, { line, target: entry, step, value });
    else if (!this.pendingCompletions.has(key)) entry[step] = value;
    if (this.pendingCompletions.size) this.watchCompletions();
    this.notifyChange();
  }

  /** writer(), where a refusal is not this caller's to report (record() refuses the write anyway). */
  private gainQuietly(): void {
    try {
      this.writer();
    } catch {
      /* another process holds the store */
    }
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
    entriesHash: string | null,
    partial: boolean
  ): void {
    this.replays.delete(clientSendId);
    this.replays.set(clientSendId, { at, results, entriesHash, partial });
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
  private replayOf(clientSendId: string, entriesHash: string): Replay | null {
    this.list();
    const held = this.replays.get(clientSendId);
    if (!held || this.now() - held.at >= REMEMBER_SEND_MS) return null;
    return held.entriesHash === null || held.entriesHash === entriesHash ? held : null;
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
    wanted: () => boolean = () => true,
    beforeRename?: (file: string) => Promise<void>
  ): Promise<void> {
    this.writer();
    const epoch = this.epoch;
    const v1 = path.join(this.reportsRoot(), 'v1');
    let expired = false;
    const proceed = () => {
      // Confirm ownership even after a failed lookup. Reacquisition reloads/reconciles history,
      // but cannot authorize an attempt begun in an earlier ownership generation.
      try {
        this.writer();
      } catch {
        return false;
      }
      return !expired && this.epoch === epoch && wanted();
    };
    const check = () => {
      if (!proceed()) throw new ShareWriteDroppedError();
    };
    await withDeadline(
      (async () => {
        const stat = await fs.promises.stat(v1).catch(() => null);
        check();
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
          check();
          if (current !== key.publicKeyPem) await writeAtomic(v1, file, key.publicKeyPem, proceed);
          check();
        }
        const target = path.join(v1, 'answers', name);
        await writeAtomic(v1, target, body, proceed, () =>
          beforeRename ? beforeRename(target) : Promise.resolve()
        );
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
    // By id: a non-owner's refreshed history holds fresh objects, while a delivery may hold the earlier one.
    return all
      .slice(all.findIndex((held) => held.id === entry.id) + 1)
      .some(
        (later) =>
          later.id !== entry.id &&
          CLOSING_KINDS.has(later.answer.kind) &&
          later.answer.cardId === entry.answer.cardId
      );
  }

  /**
   * The last guard on `<card>.json`, whatever the store remembers: it never goes back to a lower
   * `cardSeq`. Only a file with our own signature counts, so a file anyone dropped on the share
   * cannot block a real answer. A file that cannot be read fails the write, to be retried.
   */
  private async refuseOverNewer(entry: AnswerHistoryEntry, file: string): Promise<void> {
    let bytes: Buffer;
    try {
      bytes = await readRegularFile(file, 64 * 1024);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException | null)?.code;
      // Nothing there, not a plain file, or a file this server can't read: none of these is a newer answer of ours
      // (the store already refused older versions), so the write goes ahead and replaces it.
      if (['ENOENT', 'ENOTREG', 'ELOOP', 'EACCES', 'EPERM'].includes(code ?? '')) return;
      throw error;
    }
    const held = verifySignedAnswer(bytes, this.answerKey().publicKeyPem);
    // Only this server's own closing answer to this same card blocks: a signed file copied from another card (the
    // share's answers folder is writable by every writer) or a note is no newer answer to this one.
    if (
      held.ok &&
      held.answer.cardId === entry.answer.cardId &&
      CLOSING_KINDS.has(held.answer.kind) &&
      held.answer.cardSeq > entry.answer.cardSeq
    )
      throw new ShareHasNewerError();
  }

  private markSuperseded(entry: AnswerHistoryEntry): void {
    this.recordStep(entry, 'share', 'skipped', SUPERSEDED_DETAIL);
    if (entry.doorbell.state !== 'done')
      this.recordStep(entry, 'doorbell', 'skipped', SUPERSEDED_DETAIL);
  }

  /** Writes one record's signed copy to the share. */
  private async writeShare(entry: AnswerHistoryEntry, limitMs: number): Promise<boolean> {
    // While it runs, this object is this process's own delivery (see holdsEntry).
    if (this.sharing.has(entry.id)) return this.writeShareHeld(entry, limitMs);
    this.sharing.add(entry.id);
    try {
      return await this.writeShareHeld(entry, limitMs);
    } finally {
      this.sharing.delete(entry.id);
    }
  }

  private async writeShareHeld(entry: AnswerHistoryEntry, limitMs: number): Promise<boolean> {
    if (this.superseded(entry)) {
      this.markSuperseded(entry);
      return false;
    }
    try {
      await this.writeShareFile(
        shareFileName(entry.answer, entry.id),
        `${JSON.stringify(entry.answer, null, 2)}\n`,
        limitMs,
        () => !this.superseded(entry),
        CLOSING_KINDS.has(entry.answer.kind)
          ? (file) => this.refuseOverNewer(entry, file)
          : undefined
      );
      this.recordStep(entry, 'share', 'done', 'Signed copy written to the share.');
      return true;
    } catch (error) {
      if (
        error instanceof ShareHasNewerError ||
        (error instanceof ShareWriteDroppedError && this.superseded(entry))
      )
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
    if (this.summarizing.has(send.sendId)) return this.writeSummaryHeld(send, limitMs);
    this.summarizing.add(send.sendId);
    try {
      return await this.writeSummaryHeld(send, limitMs);
    } finally {
      this.summarizing.delete(send.sendId);
    }
  }

  private async writeSummaryHeld(send: SendRecord, limitMs: number): Promise<boolean> {
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
    this.gainQuietly();
    if (
      state !== 'done' &&
      (send.summary.state === 'done' || this.pendingCompletions.has(send.sendId + ':summary'))
    )
      return true;
    const line = { type: 'delivery', id: send.sendId, step: 'summary', state, at, detail };
    const value = { state, at, detail, attempts: send.summary.attempts + 1 };
    if (this.record(line)) send.summary = value;
    else if (state === 'done')
      this.pendingCompletions.set(send.sendId + ':summary', {
        line,
        target: send,
        step: 'summary',
        value,
      });
    else if (!this.pendingCompletions.has(send.sendId + ':summary')) send.summary = value;
    if (this.pendingCompletions.size) this.watchCompletions();
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
        // Another writer may have rung while an earlier ring was out: the store is confirmed (gaining it again
        // reconciles the delivery steps), and what is already rung is not rung again. Another holder rings its own.
        try {
          this.writer();
        } catch {
          break;
        }
        group.entries = group.entries.filter(
          (entry) => entry.doorbell.state !== 'done' && entry.doorbell.state !== 'skipped'
        );
        if (group.entries.length === 0) continue;
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
    draft: { entryId: string; hash: string } | null,
    request: RequestKey | null
  ): Promise<AnswerHistoryEntry | 'unclear' | null> {
    const id = randomUUID();
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
        id,
        sendId,
        answer: signAnswer(unsigned, this.answerKey()),
        card: parsed.doc,
        share: { ...PENDING },
        doorbell: { ...PENDING },
      };
      this.list();
      this.writer();
      // the store changed hands since this send began: what it checked may be stale, so nothing is recorded
      if (this.sendEpoch !== null && this.epoch !== this.sendEpoch) return null;
      this.append({
        type: 'answer',
        id: entry.id,
        sendId,
        at: unsigned.answeredAt,
        answer: entry.answer,
        card: entry.card,
        ...(draft ? { entryId: draft.entryId, entryHash: draft.hash } : {}),
        ...(request ?? {}),
      });
    } catch (error) {
      // Refused, and its void refused too: held as voided here, but a restart before the void lands may count it.
      return error instanceof AppendUnclearError ? 'unclear' : null;
    }
    // No await since the line was written: a retry sees the record only with its claim on it.
    this.written.add(entry.id);
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
    // An older copy of the card can land on the share and read fresh here. The owner has already
    // spoken on a later version, and a closing answer to this one would replace that signed file.
    if (seq < this.highestRecordedSeq(id)) return null;
    if (this.isAnswered(id, seq, sha256)) return null;
    return parsed;
  }

  private highestRecordedSeq(cardId: string): number {
    let highest = 0;
    for (const { answer } of this.list())
      if (answer.cardId === cardId && answer.cardSeq > highest) highest = answer.cardSeq;
    return highest;
  }

  /** One entry of a set; must run inside `serial`. */
  private async recordEntry(
    raw: unknown,
    device: string,
    sendId: string,
    recorded: AnswerHistoryEntry[],
    budget: Budget,
    request: RequestKey | null
  ): Promise<EntryResult> {
    const entry = parseAnswerEntry(raw);
    const named = (raw as { cardId?: unknown } | null)?.cardId;
    if (!entry) return refused(isCardId(named) ? named : null, 'invalid_entry');
    const note = normalizeNote(entry.note);
    if (note === null) return refused(entry.cardId, 'invalid_note');
    // the note is signed and written to the share and the mail: the same secret check as any file there (decoded,
    // JSON inside it walked); the refusal names no part of it
    const secret = note ? (findSecretIn(note, note) ?? findTypedSecret(note)) : null;
    if (secret) return refused(entry.cardId, 'note_secret', secret);
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
    // a second dashboard on this store records nothing: one process appends and holds the key
    try {
      this.writer();
    } catch (error) {
      return refused(
        entry.cardId,
        error instanceof WriterBusyError ? 'store_busy' : 'answer_not_saved'
      );
    }
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
      entry.entryId && hash ? { entryId: entry.entryId, hash } : null,
      request
    );
    if (done === 'unclear') return refused(entry.cardId, 'answer_save_unclear');
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
        // the store first: a second dashboard records nothing, and a newly gained store is read again before any
        // replay or answer is judged against it
        try {
          this.writer();
          this.sendEpoch = this.epoch;
        } catch (error) {
          const code = error instanceof WriterBusyError ? 'store_busy' : 'answer_not_saved';
          return entries.map((raw) => {
            const named = (raw as { cardId?: unknown } | null)?.cardId;
            return refused(isCardId(named) ? named : null, code);
          });
        }
        const replay =
          clientSendId && entriesHash ? this.replayOf(clientSendId, entriesHash) : null;
        if (replay && (!replay.partial || replay.results.length >= entries.length))
          return replay.results.slice(0, entries.length);
        // Rebuilt from answer lines alone: the known results stand, and only the entries after the request's last
        // stored record, none of which was stored, are judged again.
        const out: EntryResult[] = replay ? replay.results.slice() : [];
        const budget = startBudget(this.timing.sendBudgetMs);
        let lastStored = out.length - 1;
        for (let index = out.length; index < entries.length; index += 1) {
          const raw = entries[index];
          const before = recorded.length;
          const request: RequestKey | null =
            clientSendId && entriesHash
              ? {
                  clientSendId,
                  entriesHash,
                  entryIndex: index,
                  priorResults: out.slice(lastStored + 1),
                }
              : null;
          try {
            out.push(await this.recordEntry(raw, device, sendId, recorded, budget, request));
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
          if (recorded.length > before) lastStored = index;
        }
        if (recorded.length > 0) {
          const send: SendRecord = {
            sendId,
            at: new Date(this.now()).toISOString(),
            device,
            entryIds: recorded.map((entry) => entry.id),
            summary: { ...PENDING },
          };
          // A send line that fails to append loses nothing a restart needs: each answer line carries the retry key
          // (see RequestKey), and the summary is rebuilt from the records.
          this.record({
            type: 'send',
            sendId,
            at: send.at,
            device,
            entryIds: send.entryIds,
            ...(clientSendId ? { clientSendId, results: out, entriesHash } : {}),
          });
          this.ownSends.add(sendId);
          this.sends.set(sendId, send);
          if (clientSendId) this.remember(clientSendId, this.now(), out, entriesHash, false);
          // Out of time, the summary stays pending and the retry writes it.
          const left = budget.left();
          if (left > 0) await this.writeSummary(send, Math.min(this.timing.shareMs, left));
        }
        return out;
      }).finally(() => {
        this.sendEpoch = null;
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
    // another process holds the store: its retries are its own, but its history is read again if it changed
    try {
      this.writer();
    } catch {
      try {
        this.list();
      } catch {
        /* history now reads unavailable until a good read */
      }
      return;
    }
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
