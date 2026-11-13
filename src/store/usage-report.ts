/**
 * The TypeScript mirror of the usage record contract, `aac.usage-report.v1`. The Rust type
 * `native/aac-store/src/contract.rs` is the authority and checks every report the store receives;
 * these types only let today's TypeScript collectors hand their rows over until the Rust readers
 * replace them.
 */

export const USAGE_REPORT_SCHEMA = 'aac.usage-report.v1';

/**
 * A record dated more than this after the clock is not written to cells (contract
 * `FUTURE_SLACK_MS`): a clock running ahead or a bad log line would otherwise move the 72-hour
 * settle line past now and stop every live write. It is counted as `futureDated` instead.
 */
export const FUTURE_SLACK_MS = 5 * 60_000;

/** Known flaws of a cell's counts (contract `QUALITY_FLAGS`), kept with the cell for good. */
export type UsageQualityFlag = 'outputIncludesReasoningTwice';
/** Bit of `outputIncludesReasoningTwice` in a range row's quality column. */
export const Q_OUTPUT_INCLUDES_REASONING_TWICE = 1;

export type UsageReportState =
  | 'ok'
  | 'no_usage'
  | 'partial'
  | 'cached'
  | 'scanning'
  | 'not_installed'
  | 'installed_no_logs'
  | 'unreadable'
  | 'no_reader'
  | 'scan_failed';

export interface UsageCounts {
  roots?: number;
  filesFound?: number;
  filesRead?: number;
  filesFailed?: number;
  newestMtime?: string | null;
  /** Records left out because they were dated more than `FUTURE_SLACK_MS` after the read. */
  futureDated?: number;
}

/** Absolute totals of one UTC hour for one session, route, model, effort and account. */
export interface UsageCell {
  hour: string;
  sessionKey: string | null;
  sessionHash?: string | null;
  route: string;
  modelRaw: string;
  effort?: string | null;
  effortSource?: string | null;
  accountKey?: string | null;
  accountSource?: string | null;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  reasoningOutputTokens?: number | null;
  /** The cost the tool itself logged; null when it logs none. */
  loggedCostUsd?: number | null;
  /** Computed by the server: the API-equivalent estimate and its fallback-priced part. */
  costUsd?: number | null;
  fallbackCostUsd?: number | null;
  events?: number | null;
  quality?: UsageQualityFlag[];
}

export interface UsageSessionPart {
  route: string;
  modelRaw: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  loggedCostUsd?: number | null;
  costUsd?: number | null;
  fallbackCostUsd?: number | null;
}

export interface UsageSession {
  sessionKey: string;
  sessionHash?: string | null;
  first: string;
  last: string;
  parts: UsageSessionPart[];
  target?: string | null;
  cwd?: { key: string; path: string } | null;
  quality?: UsageQualityFlag[];
}

export interface UsageReport {
  schema: typeof USAGE_REPORT_SCHEMA;
  computer: string;
  tool: string;
  readAt: string;
  state: UsageReportState;
  complete: boolean;
  coverFrom?: string | null;
  cause?: string | null;
  counts?: UsageCounts;
  cells: UsageCell[];
  sessions: UsageSession[];
  kind?: 'live' | 'backfill' | 'import';
  /** Days (UTC day numbers) whose cells were left out because they did not change. */
  sameDays?: number[];
}

export interface UsageIngestResult {
  accepted: number;
  refused: Array<{ index: number; code: string; message: string }>;
  put: number;
  deleted: number;
  sessions: number;
  events: number;
  readsLogged: number;
}

/** One row of `activityRange`: see `columns` in the answer. */
export type ActivityRangeRow = [
  bucketMs: number,
  computer: number | null,
  tool: number,
  route: number,
  model: number,
  inputTokens: number,
  outputTokens: number,
  cacheCreationTokens: number,
  cacheReadTokens: number,
  loggedCostNano: number | null,
  costNano: number | null,
  fallbackCostNano: number | null,
  events: number | null,
  /** Quality bits (`Q_OUTPUT_INCLUDES_REASONING_TWICE`); rows never mix flagged and clean cells. */
  quality: number,
];

export interface ActivityRangeSession {
  tool: string;
  sessionKey: string | null;
  computer: string | null;
  firstMs: number;
  lastMs: number;
  target: string | null;
  parts: UsageSessionPart[];
  quality?: UsageQualityFlag[];
}

export interface ActivityRange {
  columns: string[];
  computers: string[];
  tools: string[];
  routes: string[];
  models: string[];
  rows: ActivityRangeRow[];
  /** Per tool and flag set: the first and last flagged hour in the range (`toMs` ends it). */
  quality?: Array<{ tool: string; flags: UsageQualityFlag[]; fromMs: number; toMs: number }>;
  sessions: {
    total: number;
    signatures: Array<{ tool: string; pairs: Array<[string, string]>; count: number }>;
    sample: ActivityRangeSession[];
    truncated: boolean;
  };
}

export interface ActivitySourceState {
  computer: string;
  tool: string;
  oldestMs: number | null;
  newestMs: number | null;
  lastReadMs: number | null;
  lastCompleteMs: number | null;
  state: string | null;
  cause: string | null;
  complete: boolean | null;
  badSinceMs: number | null;
  /** Records the current read left out as dated after it. */
  futureDated?: number | null;
  backfill: {
    state: string;
    files_read?: number;
    files_total?: number;
    oldest_h?: number;
    cause?: string;
    at: number;
  } | null;
  covered: Array<[number, number]>;
}

export interface ActivitySources {
  sources: ActivitySourceState[];
  computers: Array<{
    computer: string;
    oldestMs: number | null;
    newestMs: number | null;
    lastReadMs: number | null;
    covered: boolean;
    gaps: Array<[number, number]>;
  }>;
}
