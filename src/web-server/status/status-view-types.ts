/** The Status tab view's types (docs/status-tab.md, GET /api/status v1) and the collected inputs. */
import type { SnapshotAccount } from '../agent-usage/agent-usage-snapshot';
import type { AgentReportsState } from './agent-reports-reader';
import type { AnswerDelivery, AnswerHistoryEntry } from './answers';
import type {
  BlockedOn,
  CardSection,
  CardDoc,
  CardMedia,
  CardOption,
  CardRecommendation,
  ItemState,
  MachineId,
  ReportLink,
  TaskItem,
  ThreadDoc,
} from './report-schema';
import type {
  BrowserMachine,
  BrowserSession,
  MachineSample,
  RegistryThread,
  SimlockCapacity,
  SimlockLease,
  SimlockStatus,
} from './status-collectors';

// ---------------------------------------------------------------------------------------------
// Inputs

/** One collector's latest result: the last good data and the last failure, each with its time. */
export interface Collected<T> {
  data: T | null;
  /** When `data` was read (epoch ms). */
  asOf: number | null;
  /** The last attempt's failure, in fixed words; null after a success. */
  error: { reason: string; since: number } | null;
  /** The tool or folder does not exist here (shown as `missing`, not `error`). */
  missing?: boolean;
}

export interface UsageInput {
  accounts: SnapshotAccount[];
  sampledAt: number;
  staleAfterSeconds: number;
}

export interface StatusInputs {
  now: number;
  timeZone: string;
  reports: AgentReportsState;
  registry: Collected<RegistryThread[]>;
  machines: Record<MachineId, Collected<MachineSample>>;
  browsers: Collected<BrowserMachine[]>;
  simlock: Collected<SimlockStatus>;
  usage: UsageInput | null;
  /** Every stored answer, oldest first. */
  answers: AnswerHistoryEntry[];
  /** Names that never appear as threads (the doorbell's own pm-mail name). */
  hiddenThreadNames?: readonly string[];
}

// ---------------------------------------------------------------------------------------------
// View

export type SourceId =
  | 'agent-reports'
  | 'pm-mail'
  | 'machines'
  | 'browsers'
  | 'simlock'
  | 'usage'
  | 'quota-watch';
export type SourceState = 'ok' | 'stale' | 'missing' | 'unreadable' | 'error' | 'unconfigured';
export type Tone = 'good' | 'warn' | 'bad';

export interface Source {
  id: SourceId;
  label: string;
  state: SourceState;
  asOf: string | null;
  ageSeconds: number | null;
  staleAfterSeconds: number | null;
  detail: string;
}

export interface HeadlineRun {
  text: string;
  strong?: boolean;
  tone?: Tone;
}

/** A media item with where the page loads it: `/api/status/files/<writer>/<path>` (session only). */
export type MediaView = CardMedia & { url: string; posterUrl: string | null };

/** A section as the page gets it: media items carry their URLs. */
export type SectionView =
  | Exclude<CardSection, { type: 'media' }>
  | { type: 'media'; title: string | null; fold: boolean; items: MediaView[] };

export interface CardOptionView {
  key: string;
  label: string;
  gets: string | null;
  costs: string | null;
  media: MediaView[];
  sections: SectionView[];
  stance: CardOption['stance'];
}

/** A note the owner already sent on an open card. */
export interface CardNoteView {
  /** note, or object (an objection, in the owner's words). */
  kind: 'note' | 'object';
  note: string;
  at: string;
  delivery: AnswerDelivery;
}

export interface CardView {
  id: string;
  label: string;
  writer: string;
  seq: number;
  project: string | null;
  stream: string | null;
  askedBy: string | null;
  title: string;
  now: string | null;
  nowAsOf: string | null;
  oldAnswer: { text: string; at: string | null } | null;
  whatChanged: string | null;
  options: CardOptionView[];
  recommendation: CardRecommendation | null;
  multiple: boolean;
  urgency: CardDoc['urgency'];
  /** What waits on the answer, or null for "nothing waits on this". */
  blocks: string | null;
  /** The card's content sections, in order; unknown types arrive as `{ type, title, fold, unknown: true }`. */
  sections: SectionView[];
  batch: string | null;
  evidence: ReportLink[];
  replies: Array<{ at: string; text: string }>;
  kind: CardDoc['kind'];
  neededBy: CardDoc['neededBy'];
  exception: CardDoc['exception'];
  reversible: CardDoc['reversible'];
  verdict: string | null;
  group: string | null;
  agreement: CardDoc['agreement'];
  doubt: string | null;
  appliesTo: CardDoc['appliesTo'];
  changesCanon: CardDoc['changesCanon'];
  related: string[];
  askedAt: string;
  ageSeconds: number;
  askerState: ThreadDoc['state'] | 'gone' | 'unknown';
  cardSha256: string;
  /** The owner's notes already sent on this card, oldest first. */
  notes: CardNoteView[];
  /**
   * Why this card cannot be answered: the file's current problem (the copy shown is the last good
   * one), or a judgement card that carries a recommendation. A sample card has no problem but is
   * not answerable either.
   */
  problem: string | null;
  answerable: boolean;
}

export interface AnsweredCardView {
  card: CardView;
  answer: {
    /** answer (a choice) or done (an action card); notes and objections stay on the open card. */
    kind: 'answer' | 'done';
    choice: string[];
    note: string;
    via: 'deck' | 'recommended';
    answeredAt: string;
    device: string;
    delivery: AnswerDelivery;
  };
}

/** Open cards that share `batch`: kept together in the deck's order. */
export interface BatchView {
  id: string;
  label: string;
  cardIds: string[];
}

export type ItemCounts = Record<'total' | ItemState, number>;

export interface StreamView {
  id: string;
  label: string;
  owner: string | null;
  goal: string | null;
  writer: string;
  board: { path: string; ageSeconds: number | null } | null;
  source: 'tasks' | 'board-only';
  asOf: string | null;
  ageSeconds: number | null;
  stale: boolean;
  problem: string | null;
  counts: ItemCounts | null;
  completion: { done: number; of: number } | null;
  items: Array<TaskItem & { ageSeconds: number | null }>;
}

export interface ProjectView {
  id: string;
  label: string;
  streams: StreamView[];
  counts: ItemCounts;
  completion: { done: number; of: number } | null;
}

export interface ThreadView {
  name: string;
  writer: string | null;
  machine: MachineId | null;
  project: string | null;
  stream: string | null;
  /** Null for a thread known only from pm-mail. */
  state: ThreadDoc['state'] | null;
  doing: string | null;
  blockedOn: BlockedOn | null;
  progress: ThreadDoc['progress'];
  model: string | null;
  effort: string | null;
  account: string | null;
  source: 'report' | 'pm-mail';
  lastSeen: string | null;
  ageSeconds: number | null;
  stale: boolean;
  /** How far ahead of this server's clock the writer's `writtenAt` was, when over 2 minutes. */
  clockAheadSeconds: number | null;
  problem: string | null;
}

export interface MachineView {
  id: MachineId;
  label: string;
  state: 'ok' | 'stale' | 'unreachable';
  asOf: string | null;
  ageSeconds: number | null;
  detail: string | null;
  cpu: { cores: number | null; load1: number | null; busyPct: number | null } | null;
  memory: { availableGb: number | null; totalGb: number | null } | null;
  disk: Array<{ mount: string; freeGb: number; totalGb: number }>;
  browsers: { running: number | null; max: number | null } | null;
  recordings: { running: number | null; max: number | null } | null;
  gpuPct: number | null;
  sessions: BrowserSession[];
  warnings: string[];
}

export interface UsageAccountView {
  id: string;
  provider: string;
  label: string;
  host: string;
  active: boolean;
  window: string;
  kind: string;
  usedPct: number | null;
  remaining: number | null;
  limit: number | null;
  unit: string | null;
  resetsAt: string | null;
  projectedFullAt: string | null;
  runsOutBeforeReset: boolean | null;
  atLimit: boolean;
  stale: boolean;
}

export interface UsageWarning {
  tone: Tone;
  text: string;
}

export interface StatusView {
  version: 1;
  generatedAt: string;
  refresh: { pollSeconds: number };
  headline: {
    tone: Tone;
    runs: HeadlineRun[];
    counts: {
      openCards: number;
      blockedItems: number;
      workingThreads: number;
      staleSources: number;
    };
  };
  sources: Source[];
  cards: {
    source: 'agent-reports';
    open: CardView[];
    answered: AnsweredCardView[];
    batches: BatchView[];
  };
  projects: ProjectView[];
  threads: { source: 'agent-reports'; rows: ThreadView[] };
  machines: { source: 'machines'; rows: MachineView[] };
  simlock: { source: 'simlock'; capacity: SimlockCapacity[]; leases: SimlockLease[] };
  usage: { source: 'usage'; warnings: UsageWarning[]; accounts: UsageAccountView[] };
}
