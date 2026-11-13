/** The Model Scoreboard view (docs/scoreboard.md, GET /api/scoreboard v1). */
import type { ReportLink, RunDoc } from '../status/report-schema';
import type { SourceState } from '../status/status-view-types';
import type {
  CatalogBenchmark,
  CatalogMeasure,
  CatalogModel,
  CatalogTaskType,
  CatalogUse,
} from './catalog';

export interface ScoreboardSource {
  id: 'scoreboard-runs' | 'scoreboard-catalog';
  label: string;
  state: SourceState;
  asOf: string | null;
  ageSeconds: number | null;
  staleAfterSeconds: number | null;
  detail: string;
}

/** A median with its range. Unknown stays null. */
export interface Stat {
  value: number | null;
  n: number;
  low: number | null;
  high: number | null;
}

export interface Overturn {
  /** The run that was overturned. */
  run: string;
  /** The review run that overturned it. */
  review: string;
  /** The reviewer model: `review.by`, else the review run's model. */
  by: string | null;
  disputed: boolean;
  evidence: string | null;
}

export interface CostRate {
  value: number | null;
  unit: string;
  n: number;
}

export interface MeasureValue {
  value: number | null;
  unit: string;
  /** The runs behind the value (items for quality). */
  n: number;
  low: number | null;
  high: number | null;
  interval: 'wilson90' | 'range' | null;
  /** The counts behind a share or a metric. */
  of: number | null;
  per: number | null;
  /** True unless every run behind the value has the catalog's `preliminaryUnless` metric at 1. */
  preliminary: boolean;
  /** speed: median wall seconds per run. */
  perTask?: Stat;
  /** quality */
  disputed?: number;
  cannotVerify?: number;
  rounds?: Stat;
  /** quality: items that passed on their first review round. */
  firstTry?: number;
  overturns?: Overturn[];
  /** thoroughness */
  files?: Stat;
  records?: Stat;
  /** cost: measured, per accepted unit; never mixed with credits or with each other. */
  quota?: Array<CostRate & { window: string }>;
  usd?: CostRate | null;
  /** cost: an estimate, always labelled. */
  estimate?: {
    label: 'estimate';
    credits: CostRate | null;
    quota: Array<CostRate & { window: string }>;
    methods: string[];
    sources: string[];
  } | null;
}

export interface FlagView {
  kind: RunDoc['flags'][number]['kind'];
  run: string;
  detail: string;
  evidence: string;
  reportedBy: string | null;
}

export interface RunSummary {
  id: string;
  writer: string;
  source: RunDoc['source'];
  benchmark: string | null;
  project: string | null;
  stream: string | null;
  task: { id: string; type: string | null; title: string };
  model: string | null;
  label: string | null;
  runtime: string | null;
  effort: string | null;
  process: { id: string; version: string | null } | null;
  outcome: string;
  endedAt: string;
  wallSeconds: number | null;
  imported: { from: string; at: string } | null;
  review: {
    of: string;
    verdict: 'pass' | 'fail' | 'cannot-verify';
    overturned: boolean;
    disputed: boolean;
    by: string | null;
  } | null;
  flags: number;
  evidence: ReportLink[];
}

export type Verdict = 'recommended' | 'fine' | 'avoid' | 'not-enough-data';

/** One row's numbers from one kind of data: never averaged with another column. */
export interface Column {
  source: RunDoc['source'];
  benchmark: string | null;
  benchmarkLabel: string | null;
  n: number;
  imported: number;
  /** Blind reviews of this column's runs. */
  reviews: number;
  ranked: boolean;
  /** 1 is best, among ranked columns of the same source (and benchmark) in this use. */
  rank: number | null;
  score: number | null;
  verdict: Verdict;
  verdictLabel: string;
  why: string;
  preliminary: boolean;
  measures: Record<string, MeasureValue>;
  flags: FlagView[];
  /** The newest runs: at most 20 in an unranked column, 5 in a ranked one. */
  runs: RunSummary[];
}

export interface ScoreboardRow {
  key: string;
  model: string;
  label: string;
  inCatalog: boolean;
  runtime: string | null;
  effort: string | null;
  process: { id: string; version: string | null } | null;
  n: { observed: number; benchmark: number };
  /** The headline: the observed column when it is ranked, else the first ranked benchmark column. */
  basis: 'observed' | 'benchmark' | null;
  benchmark: string | null;
  ranked: boolean;
  rank: number | null;
  verdict: Verdict;
  verdictLabel: string;
  why: string;
  observed: Column | null;
  benchmarks: Column[];
}

export interface ScoreboardTaskType {
  id: string;
  label: string;
  description: string | null;
  unit: CatalogTaskType['unit'];
  measures: string[];
  byProcess: boolean;
  rows: ScoreboardRow[];
}

export interface ScoreboardUse {
  id: string;
  label: string;
  description: string | null;
  taskTypes: ScoreboardTaskType[];
}

/** Why a run was not scored: the scan did not read it, it was unreadable, or it names no catalog model or task type. */
export type UnscoredReason =
  'scan-limit' | 'no-model' | 'no-task-type' | 'unknown-model' | 'unknown-task-type';

export interface UnscoredExample {
  /** The run id (safe for the page; never an account). */
  id: string;
  /** The model id as written (`model.id`), before alias resolution, or null. */
  model: string | null;
  /** The task type id as written, or null. */
  taskType: string | null;
  reason: UnscoredReason;
  endedAt: string;
}

/**
 * Runs that count toward no row and no verdict, by reason, so `total` is every run not scored.
 * `scanLimit` is the run files past any scan cap, from `RunsState.skipped.runFiles` (runs, writers
 * or bytes; the newest days/writers are kept, named in the runs Source). `unreadable` is the run files the runs reader
 * refused (named in the runs Source, and refused in the readers check) with no earlier good copy to
 * score. Every run it accepted has at most one reason, in this precedence: `no-model`, `no-task-type`,
 * `unknown-model` (its model resolves to no catalog id), `unknown-task-type` (its task type is not in
 * the catalog). The six counts sum to `total`. `unknownModels` names each raw unknown model id (sorted
 * by runs desc, then id, at most 20) so new aliases can be added from here. `examples` holds the newest
 * few accepted runs across every reason (at most 10, newest first; an unreadable file and a scan-limit
 * file have no run to show).
 */
export interface UnscoredSummary {
  total: number;
  scanLimit: number;
  unreadable: number;
  noModel: number;
  noTaskType: number;
  unknownModel: number;
  unknownTaskType: number;
  unknownModels: Array<{ id: string; runs: number }>;
  examples: UnscoredExample[];
}

/** The server-side filters of `GET /api/scoreboard` (Scoreboard v2). */
export interface ScoreboardFilters {
  /** Local calendar days (`YYYY-MM-DD`) in the dashboard's zone, inclusive; null is open. */
  from: string | null;
  to: string | null;
  /** real = observed work only, benchmark = benchmarks only. */
  source: 'all' | 'real' | 'benchmark';
  taskType: string | null;
}

export interface ScoreboardFiltersView extends ScoreboardFilters {
  /** False without the history store: every run read is shown and nothing can be filtered. */
  available: boolean;
  /** The catalog's task types, for the page's filter. */
  taskTypes: Array<{ id: string; label: string }>;
}

/** Families in order, each catalog model's family and order, and the effort order (Rust). */
export interface ScoreboardIdentity {
  catalogHash: string;
  families: Array<{ id: string; label: string; short: string; order: number }>;
  models: Record<string, { family: string | null; order: number; label: string }>;
  efforts: string[];
}

/** One by-model row's grouping and sort keys (native/aac-status `group_keys`). */
export interface ScoreboardRowKeys {
  family: string;
  modelOrder: number;
  effortOrder: number;
  verdicts: [number, number, number];
  rank: number | null;
  runs: number;
  passed: number | null;
  decided: number | null;
  cost: number | null;
  costUnit: string | null;
  costJob: string | null;
  lastRunAt: string | null;
}

export interface ScoreboardGroups {
  families: Array<{ id: string; label: string; short: string; order: number }>;
  efforts: string[];
  /** View id to by-model row key to its keys. */
  views: Record<string, Record<string, ScoreboardRowKeys>>;
}

/** Where the runs come from: the history store (kept forever) or the share alone. */
export interface ScoreboardHistory {
  store: 'on' | 'off';
  /** Every run kept, whatever the filters. */
  runs: number | null;
  importing: boolean;
}

export interface ScoreboardView {
  cached?: boolean;
  cacheAgeSeconds?: number;
  refreshing?: boolean;
  version: 1;
  generatedAt: string;
  refresh: { pollSeconds: number };
  sources: ScoreboardSource[];
  catalog: {
    minRuns: number;
    models: CatalogModel[];
    taskTypes: CatalogTaskType[];
    measures: CatalogMeasure[];
    uses: CatalogUse[];
    benchmarks: CatalogBenchmark[];
  };
  uses: ScoreboardUse[];
  recent: RunSummary[];
  /** Runs read but never scored, by reason (the page shows these instead of hiding them). */
  unscored: UnscoredSummary;
  /** Scoreboard v2: the filters applied, the identity facts for grouping, and the history. */
  filters?: ScoreboardFiltersView;
  identity?: ScoreboardIdentity | null;
  history?: ScoreboardHistory;
  /** The by-model rows' grouping and sort keys (aac-status `composeGroups`); absent when unavailable. */
  groups?: ScoreboardGroups;
  /** The by-model matrix (CONTRACT-spend-bymodel.md section 2), composed in the
   * aac-status native module and passed through; absent when it cannot be had. */
  byModel?: ByModelView;
}

// ---------------------------------------------------------------------------------------------
// byModel (the native composer's own output, passed through)

/** One run of the sentence type `/api/status` already uses. */
export interface ByModelRun {
  text: string;
  strong?: true;
  tone?: 'warn' | 'crit' | 'good' | 'acc';
}

export interface ByModelView {
  defaultView: 'pipeline';
  intro: string;
  /** The quiet sentences at the end of the page. */
  readNote: ByModelRun[];
  readLink: { label: string; anchor: string };
  bandSummary: string;
  views: ByModelViewSection[];
}

export interface ByModelViewSection {
  id: 'pipeline' | 'pm-users';
  label: string;
  short: string;
  meta: ByModelRun[];
  picks: ByModelPick[];
  jobs: Array<{ id: string; label: string }>;
  models: ByModelModel[];
  foot: ByModelRun[];
}

export interface ByModelPick {
  job: string;
  jobLabel: string;
  model: { label: string; effort: string | null; mark: string | null } | null;
  noneLabel: string | null;
  why: ByModelRun[];
}

export interface ByModelModel {
  key: string;
  model: string;
  label: string;
  effort: string | null;
  effortText: string;
  runtime: string;
  mark: string | null;
  runs: number;
  head: string;
  headRuns: ByModelRun[];
  sheetHead: string;
  cells: Record<string, ByModelCell | null>;
  report: ByModelReport[];
}

export interface ByModelCell {
  verdict: 'recommended' | 'fine' | 'avoid' | 'not-enough-data';
  word: string;
  /** Chunks the page keeps whole: it breaks only between them, never mid-phrase. */
  sub: string[];
}

export interface ByModelReport {
  job: string;
  label: string;
  verdict: string;
  word: string;
  basis: string;
  measures: ByModelMeasure[];
  compare: ByModelRun[];
}

export interface ByModelMeasure {
  label: string;
  value: string;
  sub: string;
  note: string;
  range: { low: number | null; high: number | null; value: number | null } | null;
}
