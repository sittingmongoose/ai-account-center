/** The Status tab view's types (docs/status-tab.md, GET /api/status v1) and the collected inputs. */
import type { QuotaReportState } from './quota-report';
import type { SpendView } from './status-view-quota';
import type { SnapshotAccount } from '../agent-usage/agent-usage-snapshot';
import type { AgentReportsState } from './agent-reports-reader';
import type { AnswerDelivery, AnswerHistoryEntry, SendRecord } from './answers';
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
  ReadProblem,
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
  /**
   * Named parts that failed while the rest was read (registry files, browser-check hosts, Simlock
   * workers). Any makes the source not `ok`, though `asOf` is the latest read.
   */
  problems?: ReadProblem[];
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
  /** The sends those answers came in, so each delivery shows its send summary step. */
  sends?: readonly SendRecord[];
  answerHistoryProblem?: string | null;
  /** Names that never appear as threads (the doorbell's own pm-mail name). */
  hiddenThreadNames?: readonly string[];
  /** quota-watch's share copy; absent when the server does not read it (shown as unconfigured). */
  quota?: QuotaReportState;
  /** The pinned-threads panel (pass-through: the composer's own output, or its unavailability). */
  pinned?: PinnedPanel;
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

/** A source's state as attached to the readings it supplied on another source's panel. */
export type SourceRef = Pick<Source, 'id' | 'label' | 'state' | 'asOf' | 'ageSeconds' | 'detail'>;

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
  /** pm-mail rows: the registry's state, age and problem (its failure keeps the last good row); null on report rows. */
  registrySource: SourceRef | null;
}

/** The pinned-threads panel (PINNED-THREADS.md section 1): the composer's output, passed
 * through untouched, or why there is none. Every word is decided in native/aac-status. */
export type PinnedPanel = PinnedView | { unavailable: string };

export interface PinnedView {
  asOfMs: number;
  /** The section head's count line, e.g. "5 pinned in T3 · 1 looping · 2 waiting on you". */
  head: string;
  counts: { pinned: number; looping: number; waitingOnYou: number };
  machineProblems: Array<{ machine: string; label: string; text: string }>;
  updates: PinnedUpdateView[];
}

export interface PinnedUpdateView {
  id: string;
  title: string;
  /** The machine whose T3 holds the pin: vm, mac, windows or nas1. */
  machine: string;
  /** "Puppet Master · pm7-build · Opus 5.5 xhigh", or null with no report to build it from. */
  who: string | null;
  /** fresh | stale | unreadable | none (the report cases of PINNED-THREADS.md section 1). */
  reportState: 'fresh' | 'stale' | 'unreadable' | 'none';
  percent: { text: string; done: number | null; of: number | null } | null;
  /** The sentence beside the percent: the estimate fallback, the disagreement, "No task list." */
  percentNote: string | null;
  now: string | null;
  blocked: { text: string; waitingOnYou: boolean };
  looping: { answer: 'yes' | 'no' | 'cantTell'; text: string };
  left: Array<{ id: string; title: string }>;
  done: Array<{ id: string; title: string }>;
  updated: { text: string; at: string; ageSeconds: number } | null;
  reportNote: string | null;
  staleNote: string | null;
  unreadableNote: string | null;
  summary: string | null;
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
  /** browser-check's state, age and problem for `browsers`, `recordings`, `gpuPct` and `sessions`. */
  slotsSource: SourceRef;
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
  /** The reset passed after this reading: the number is history, not a current limit or projection. */
  resetPassed: boolean;
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
    /** null means unreadable, never an empty successful history. */
    answered: AnsweredCardView[] | null;
    answerHistoryProblem: string | null;
    batches: BatchView[];
    /**
     * True only when the latest scan is known to have seen every card: it listed `v1/writers` and
     * every writer and `cards/` folder, read every file it tried (a file refused for its content is
     * a named problem and does not count), hit no scan limit and is not stale. While false, a card
     * missing from `open` may simply not have been read, so nothing may be concluded from it.
     * Unavailable answer history also makes this false, preserving drafts until it recovers.
     */
    complete: boolean;
  };
  /** The pinned threads' updates (PINNED-THREADS.md), composed in the aac-status native
   * module and passed through; absent only when the service has not provided it. */
  pinned?: PinnedPanel;
  projects: ProjectView[];
  /** `also`: the panel's secondary sources; their failures are shown on the panel and its rows. */
  threads: { source: 'agent-reports'; also: ['pm-mail']; rows: ThreadView[] };
  machines: { source: 'machines'; also: ['browsers']; rows: MachineView[] };
  simlock: { source: 'simlock'; capacity: SimlockCapacity[]; leases: SimlockLease[] };
  usage: { source: 'usage'; warnings: UsageWarning[]; accounts: UsageAccountView[] };
  /** quota-watch's per-stream estimates (docs/status-tab.md, Spend). */
  spend: SpendView;
}
