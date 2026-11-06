/**
 * The owner's answers to decision cards (docs/status-tab.md, Answers).
 *
 * Order of authority, each step recorded in the history:
 * 1. the private store `<ccsDir>/status/answers.jsonl` (append, local disk): the answer exists once
 *    this line is written, and the history is read from it;
 * 2. the signed copy `<reports>/v1/answers/<card>.json` (temporary file and rename), with the
 *    public key at `<reports>/v1/keys/aac-answers.pub`;
 * 3. the doorbell: `pm-mail --as aac-answers send <replyTo.pmMail> ...` after the signed copy is in
 *    place. The mail only says which card; the signed file is the answer.
 * A failed share write or doorbell is retried by `retryPending()` and shown, never hidden.
 * Every answer is checked against the card as it is on disk at that moment.
 */
import { randomBytes, randomUUID } from 'crypto';
import fs from 'fs';
import path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { withDeadline } from '../../utils/settle-deadline';
import { agentReportsRoot } from './agent-reports-reader';
import {
  loadOrCreateAnswerKey,
  signAnswer,
  type AnswerKey,
  type SignedAnswer,
  type UnsignedAnswer,
} from './answer-signing';
import {
  childEnv,
  commandFailure,
  findExecutable,
  runCommand,
  type CommandRunner,
} from './status-collectors';
import {
  ANSWER_SCHEMA,
  isCardId,
  isPmMailName,
  type CardDoc,
  type ParsedReport,
} from './report-schema';

export const ANSWER_NOTE_MAX = 4000;
export const DOORBELL_SENDER = 'aac-answers';
/** Retries of a failed share write or doorbell before the history says it gave up. */
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
  answer: SignedAnswer;
  /** The card version answered, as it was read at that moment. */
  card: CardDoc;
  share: StepState;
  doorbell: StepState;
}

export interface AnswerDelivery {
  state: 'rang' | 'failed' | 'pending';
  at: string | null;
  detail: string | null;
  share: StepState;
  doorbell: StepState;
}

export type AnswerOutcome =
  | { ok: true; entry: AnswerHistoryEntry }
  | {
      ok: false;
      status: number;
      code: 'card_changed' | 'invalid_choice' | 'invalid_note' | 'answer_not_saved';
      error: string;
    };

const REFUSALS = {
  card_changed: {
    status: 409,
    error: 'This card changed or is no longer open. Reload it and answer again.',
  },
  invalid_choice: { status: 400, error: 'The choice does not match this card’s options.' },
  invalid_note: {
    status: 400,
    error: `The note must be at most ${ANSWER_NOTE_MAX} characters of plain text.`,
  },
  answer_not_saved: { status: 500, error: 'The answer could not be saved. Nothing was sent.' },
} as const;

type Refusal = Extract<AnswerOutcome, { ok: false }>;

/** An answer stored and shared; `ring` sends the doorbell (absent when the share write failed). */
type CardAnswer = { ok: true; entry: AnswerHistoryEntry; ring?: () => Promise<void> } | Refusal;

function refuse(code: keyof typeof REFUSALS): Refusal {
  return { ok: false, code, ...REFUSALS[code] };
}

/** The device word for an answer, from the browser's User-Agent. */
export function answerDevice(userAgent: unknown): string {
  const ua = typeof userAgent === 'string' ? userAgent : '';
  if (/iPhone/.test(ua)) return 'iPhone';
  if (/iPad/.test(ua)) return 'iPad';
  if (/Android/.test(ua)) return 'Android';
  if (/Macintosh|Mac OS X/.test(ua)) return 'Mac';
  if (/Windows/.test(ua)) return 'Windows';
  if (/Linux|X11/.test(ua)) return 'Linux';
  return 'browser';
}

const NOTE_CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/;
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;

/** The note as stored: trimmed of surrounding whitespace only; null when it is not acceptable. */
export function normalizeNote(note: unknown): string | null {
  if (note === undefined || note === null) return '';
  if (typeof note !== 'string') return null;
  const trimmed = note.trim();
  if (NOTE_CONTROL.test(trimmed) || LONE_SURROGATE.test(trimmed)) return null;
  if ([...trimmed].length > ANSWER_NOTE_MAX) return null;
  return trimmed;
}

/** Whether a choice (option keys) and note are a valid answer to this card. */
export function validChoice(card: CardDoc, choice: string[], note: string): boolean {
  const keys = new Set(card.options.map((option) => option.key));
  if (new Set(choice).size !== choice.length) return false;
  if (!choice.every((key) => keys.has(key))) return false;
  if (!card.multiple && choice.length > 1) return false;
  if (note && !card.allowNote) return false;
  if (choice.length === 0) return card.allowNote && note.length > 0;
  return true;
}

export interface RingResult {
  ok: boolean;
  detail: string;
}

export type Doorbell = (to: string, subject: string, body: string) => Promise<RingResult>;

/** `pm-mail --as aac-answers send <to> <subject> -m <body>`, spawned with a timeout. */
export function pmMailDoorbell(run: CommandRunner = runCommand): Doorbell {
  return async (to, subject, body) => {
    const bin = findExecutable('pm-mail', ['/mnt/Cursor/Inbox/pm-mail']);
    const result = await run(bin, ['--as', DOORBELL_SENDER, 'send', to, subject, '-m', body], {
      timeoutMs: 30_000,
      maxBytes: 64 * 1024,
      env: childEnv(),
    });
    return result.code === 0
      ? { ok: true, detail: `Rang ${to} through pm-mail.` }
      : { ok: false, detail: commandFailure('pm-mail', result) };
  };
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

/** The page's delivery summary for one answer. */
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
  if (doorbell.state === 'skipped')
    return { ...base, state: 'failed', at: doorbell.at, detail: doorbell.detail };
  if (share.state === 'done' && doorbell.state === 'done')
    return { ...base, state: 'rang', at: doorbell.at, detail: doorbell.detail };
  return { ...base, state: 'pending', at: null, detail: 'Delivering.' };
}

export interface AnswerInput {
  cardId: string;
  cardSeq: number;
  cardSha256: string;
  choice: string[];
  note?: string;
}

export interface BatchInput {
  batch: string;
  cards: Array<{ id: string; seq: number; cardSha256: string }>;
  except: string[];
}

export type BatchCardOutcome =
  | 'answered'
  | 'excepted'
  | 'card_changed'
  | 'not_in_batch'
  | 'no_recommendation'
  | 'answer_not_saved';

export interface BatchResult {
  batchAnswer: string;
  results: Array<{ id: string; outcome: BatchCardOutcome; entry?: AnswerHistoryEntry }>;
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

export class StatusAnswers {
  private entries: AnswerHistoryEntry[] | null = null;
  private queue: Promise<unknown> = Promise.resolve();
  private key: AnswerKey | null = null;
  private retrying = false;
  /** Answers whose doorbell is running now, so a retry never rings twice at once. */
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

  /** Every answer in the private store, oldest first (folded with its delivery steps). */
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
        const entry: AnswerHistoryEntry = {
          id: record.id,
          answer: record.answer as SignedAnswer,
          card: record.card as CardDoc,
          share: { ...PENDING },
          doorbell: { ...PENDING },
        };
        entries.push(entry);
        byId.set(entry.id, entry);
      } else if (record.type === 'delivery' && typeof record.id === 'string') {
        const entry = byId.get(record.id);
        const step = record.step === 'share' || record.step === 'doorbell' ? record.step : null;
        if (!entry || !step) continue;
        entry[step] = {
          state:
            (['done', 'failed', 'skipped'] as const).find((s) => s === record.state) ?? 'failed',
          at: typeof record.at === 'string' ? record.at : null,
          detail: typeof record.detail === 'string' ? record.detail : null,
          attempts: entry[step].attempts + 1,
        };
      }
    }
    this.entries = entries;
    return entries;
  }

  /** Newest first. */
  history(limit = 50): AnswerHistoryEntry[] {
    return this.list().slice().reverse().slice(0, Math.max(0, limit));
  }

  /** Whether this exact card version already has an answer. */
  isAnswered(cardId: string, seq: number, sha256: string): boolean {
    return this.list().some(
      (entry) =>
        entry.answer.cardId === cardId &&
        entry.answer.cardSeq === seq &&
        entry.answer.cardSha256 === sha256
    );
  }

  private answerKey(): AnswerKey {
    if (!this.key) this.key = loadOrCreateAnswerKey(this.storeDir());
    return this.key;
  }

  private recordStep(
    entry: AnswerHistoryEntry,
    step: StepName,
    state: StepState['state'],
    detail: string
  ) {
    const at = new Date(this.now()).toISOString();
    try {
      appendLine(this.storeFile(), { type: 'delivery', id: entry.id, step, state, at, detail });
    } catch {
      /* The in-memory history still shows it; the next retry records again. */
    }
    entry[step] = { state, at, detail, attempts: entry[step].attempts + 1 };
    this.deps.onChange?.();
  }

  /** Writes the signed copy and the public key to the share. */
  private async writeShare(entry: AnswerHistoryEntry): Promise<boolean> {
    const v1 = path.join(this.reportsRoot(), 'v1');
    try {
      await withDeadline(
        (async () => {
          const pub = path.join(v1, 'keys', 'aac-answers.pub');
          const key = this.answerKey();
          const current = await fs.promises.readFile(pub, 'utf8').catch(() => '');
          if (current !== key.publicKeyPem) await writeAtomic(pub, key.publicKeyPem);
          await writeAtomic(
            path.join(v1, 'answers', `${entry.answer.cardId}.json`),
            `${JSON.stringify(entry.answer, null, 2)}\n`
          );
        })(),
        SHARE_DEADLINE_MS,
        undefined,
        'Status answer share write'
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

  private async ring(entry: AnswerHistoryEntry): Promise<void> {
    if (this.ringing.has(entry.id)) return;
    this.ringing.add(entry.id);
    try {
      await this.ringOnce(entry);
    } finally {
      this.ringing.delete(entry.id);
    }
  }

  private async ringOnce(entry: AnswerHistoryEntry): Promise<void> {
    const to = entry.card.replyTo?.pmMail;
    if (!to || !isPmMailName(to)) {
      this.recordStep(
        entry,
        'doorbell',
        'skipped',
        'The card names no pm-mail recipient; nobody was rung.'
      );
      return;
    }
    if (!this.deps.doorbell && process.env.AAC_STATUS_DOORBELL === 'off') {
      this.recordStep(
        entry,
        'doorbell',
        'skipped',
        'The pm-mail doorbell is turned off on this server.'
      );
      return;
    }
    const { cardId, cardSeq } = entry.answer;
    const subject = `Answer for ${cardId}`;
    const body = [
      `The owner answered card ${cardId} (seq ${cardSeq}) on the AI Account Center Status tab.`,
      `The signed answer file is the authority: agent-reports/v1/answers/${cardId}.json`,
      '(public key agent-reports/v1/keys/aac-answers.pub). This mail is only a doorbell.',
    ].join('\n');
    let result: RingResult;
    try {
      result = await (this.deps.doorbell ?? pmMailDoorbell())(to, subject, body);
    } catch {
      result = { ok: false, detail: 'pm-mail failed.' };
    }
    this.recordStep(entry, 'doorbell', result.ok ? 'done' : 'failed', result.detail);
  }

  /** Store, sign, share; the doorbell runs after the response. Must run inside `serial`. */
  private async answerCard(
    parsed: ParsedReport<CardDoc>,
    choice: string[],
    note: string,
    device: string,
    via: UnsignedAnswer['via'],
    batchAnswer?: string
  ): Promise<CardAnswer> {
    const unsigned: UnsignedAnswer = {
      schema: ANSWER_SCHEMA,
      cardId: parsed.doc.id,
      cardSeq: parsed.doc.seq,
      cardSha256: parsed.sha256,
      choice,
      note,
      via,
      ...(batchAnswer ? { batchAnswer } : {}),
      answeredAt: new Date(this.now()).toISOString(),
      answeredBy: 'owner',
      device,
    };
    let entry: AnswerHistoryEntry;
    try {
      entry = {
        id: randomUUID(),
        answer: signAnswer(unsigned, this.answerKey()),
        card: parsed.doc,
        share: { ...PENDING },
        doorbell: { ...PENDING },
      };
      this.list();
      appendLine(this.storeFile(), {
        type: 'answer',
        id: entry.id,
        at: unsigned.answeredAt,
        answer: entry.answer,
        card: entry.card,
      });
    } catch {
      return refuse('answer_not_saved');
    }
    this.list().push(entry);
    this.deps.onChange?.();
    const shared = await this.writeShare(entry);
    return { ok: true, entry, ring: shared ? () => this.ring(entry) : undefined };
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

  async submit(input: AnswerInput, device: string): Promise<AnswerOutcome> {
    const outcome = await this.serial(async (): Promise<CardAnswer> => {
      const note = normalizeNote(input.note);
      if (note === null) return refuse('invalid_note');
      const parsed = await this.openCard(input.cardId, input.cardSeq, input.cardSha256);
      if (!parsed) return refuse('card_changed');
      if (!validChoice(parsed.doc, input.choice, note)) return refuse('invalid_choice');
      return this.answerCard(parsed, input.choice, note, device, 'card');
    });
    if (!outcome.ok) return outcome;
    if (outcome.ring) void outcome.ring();
    return { ok: true, entry: outcome.entry };
  }

  /** "Approve all, except ...": each listed card not excepted gets its recommendation. */
  async submitBatch(input: BatchInput, device: string): Promise<BatchResult> {
    const batchAnswer = randomUUID();
    const rings: Array<() => Promise<void>> = [];
    const results = await this.serial(async () => {
      const except = new Set(input.except);
      const out: BatchResult['results'] = [];
      for (const card of input.cards) {
        if (except.has(card.id)) {
          out.push({ id: card.id, outcome: 'excepted' });
          continue;
        }
        const parsed = await this.openCard(card.id, card.seq, card.cardSha256);
        if (!parsed) {
          out.push({ id: card.id, outcome: 'card_changed' });
          continue;
        }
        if (parsed.doc.batch !== input.batch) {
          out.push({ id: card.id, outcome: 'not_in_batch' });
          continue;
        }
        if (!parsed.doc.recommendation) {
          out.push({ id: card.id, outcome: 'no_recommendation' });
          continue;
        }
        const answered = await this.answerCard(
          parsed,
          [parsed.doc.recommendation.key],
          '',
          device,
          'approve-all-except',
          batchAnswer
        );
        if (!answered.ok) {
          out.push({ id: card.id, outcome: 'answer_not_saved' });
          continue;
        }
        if (answered.ring) rings.push(answered.ring);
        out.push({ id: card.id, outcome: 'answered', entry: answered.entry });
      }
      return out;
    });
    for (const ring of rings) void ring();
    return { batchAnswer, results };
  }

  /** Retries every share write and doorbell that has not succeeded and has attempts left. */
  async retryPending(): Promise<void> {
    if (this.retrying) return;
    this.retrying = true;
    try {
      for (const entry of this.list()) {
        if (entry.share.state !== 'done') {
          if (entry.share.attempts >= MAX_DELIVERY_ATTEMPTS) continue;
          if (!(await this.serial(() => this.writeShare(entry)))) continue;
        }
        if (
          (entry.doorbell.state === 'pending' || entry.doorbell.state === 'failed') &&
          entry.doorbell.attempts < MAX_DELIVERY_ATTEMPTS
        )
          await this.ring(entry);
      }
    } finally {
      this.retrying = false;
    }
  }
}
