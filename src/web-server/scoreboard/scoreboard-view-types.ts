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

export interface ScoreboardView {
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
}
