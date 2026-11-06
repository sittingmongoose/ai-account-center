/** The Status tab view's types (docs/status-tab.md, GET /api/status v1) and the collected inputs. */
import type { SnapshotAccount } from '../agent-usage/agent-usage-snapshot';
import type { AgentReportsState } from './agent-reports-reader';
import type { AnswerDelivery, AnswerHistoryEntry } from './answers';
import type {
  BlockedOn,
  CardDoc,
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

export interface CardOptionView {
  key: string;
  label: string;
  gets: string | null;
  costs: string | null;
  /** `url` is `/api/status/files/<writer>/<path>` (session only). */
  image: { url: string; kind: 'built' | 'mockup' } | null;
}

/** A note the owner already sent on an open card. */
export interface CardNoteView {
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
  recommendation: { key: string; why: string | null } | null;
  multiple: boolean;
  urgency: CardDoc['urgency'];
  blocks: string | null;
  batch: string | null;
  evidence: ReportLink[];
  replies: Array<{ at: string; text: string }>;
  askedAt: string;
  ageSeconds: number;
  askerState: ThreadDoc['state'] | 'gone' | 'unknown';
  cardSha256: string;
  /** The owner's notes already sent on this card, oldest first. */
  notes: CardNoteView[];
  /** The card file's current problem: the copy shown is the last good one and cannot be answered. */
  problem: string | null;
  answerable: boolean;
}

export interface AnsweredCardView {
  card: CardView;
  answer: {
    kind: 'answer';
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
