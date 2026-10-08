import type {
  ClaudeDashboardPlatform,
  DashboardAccount,
  DashboardAccountStatus,
  DashboardAccountWindow,
  DashboardPlatform,
  DashboardProvider,
} from './account-dashboard-types';
import type { AccountAnalyticsRangePreset } from './account-analytics-range';
import type { AnalyticsRemoteHost } from './analytics-remote-transport';
import type { DashboardHost } from './dashboard-hosts';
import type { ModelPricingSource } from '../model-pricing';

export type AccountAnalyticsRange = AccountAnalyticsRangePreset;
export interface AccountAnalyticsQuery {
  platform: ClaudeDashboardPlatform;
  range: AccountAnalyticsRange;
  provider: AccountAnalyticsUsageProvider | 'all';
  account: string;
  /** Explicit manual refresh of native activity; quota refresh remains separate. */
  refresh?: boolean;
  /** IANA zone for local-day buckets; UTC when absent. */
  tz?: string;
  /** Inclusive local days (`YYYY-MM-DD`), only with `range: 'custom'`. */
  from?: string;
  to?: string;
}

/** One observed quota, never an additive token count or provider invoice. */
export interface AccountAnalyticsPoint {
  sampledAt: string;
  observedAt: string;
  usedPercent: number | null;
  remainingPercent: number | null;
  used: number | null;
  limit: number | null;
  remaining: number | null;
  resetAt: string | null;
  expiresAt: string | null;
  status: DashboardAccountStatus;
  source: string;
  platform: DashboardPlatform;
  isActive: boolean;
}

export interface AccountAnalyticsWindow extends DashboardAccountWindow {
  points: AccountAnalyticsPoint[];
}

export interface AccountAnalyticsAccount extends Omit<DashboardAccount, 'windows'> {
  /** The provider is switchable and this account can be a switch target. */
  switchable: boolean;
  /** Hidden from Home and the trays; Analytics still lists it. */
  hidden: boolean;
  firstSampleAt: string | null;
  lastSampleAt: string | null;
  sampleCount: number;
  windows: AccountAnalyticsWindow[];
}

export interface AccountAnalyticsActivityTotals {
  /** Uncached input; cache creation and reads are counted separately, once. */
  inputTokens: number;
  /** Includes reasoning output when the native provider reports it. */
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  estimatedCostUsd: number;
  /**
   * USD of `estimatedCostUsd` priced only at the unknown-model fallback rate: tokens with no
   * logged cost and no listed rate. Clients show that part as not logged, never as a cost.
   */
  fallbackCostUsd?: number;
  /**
   * Estimated USD per token type from list rates. Null when any contributing
   * model has no usable rate; never a zero standing in for unknown.
   */
  costByType: AccountAnalyticsCostByType | null;
  /** True when the parts are within max($0.01, 0.5%) of `estimatedCostUsd`. Parts are never rescaled. */
  costByTypeReconciled: boolean;
}

export interface AccountAnalyticsCostByType {
  input: number;
  output: number;
  cacheWrite: number;
  cacheRead: number;
}

export interface AccountAnalyticsModelRates {
  inputPerMillion: number;
  outputPerMillion: number;
  cacheCreationPerMillion: number;
  cacheReadPerMillion: number;
  source: ModelPricingSource;
}

/** A tool whose CLI usage logs the server reads (a collection source, not a provider). */
export type AccountAnalyticsActivityProvider =
  | 'claude'
  | 'codex'
  | 'omp'
  | 'muse'
  | 'zcode'
  | 'jsonl'
  | 'antigravity';

/**
 * The dashboard provider that served a usage row: the tool's own provider for Claude Code, Codex and the Muse
 * Code CLI, else the route the log records (account-analytics-attribution.ts). "other" is a route no provider
 * claims, never a guess.
 */
export type AccountAnalyticsUsageProvider = DashboardProvider | 'other';

export type AccountAnalyticsSourceState =
  | 'ok'
  | 'cached'
  | 'unavailable'
  | 'not_installed'
  /** The scan is still working on this tool: it is running now, or ran out of time before it finished. */
  | 'scanning'
  /** The tool was scanned and its logs hold no usage inside the 31-day window. */
  | 'no_usage';

export interface AccountAnalyticsSource {
  tool: AccountAnalyticsActivityProvider | 'cursor';
  host: DashboardHost;
  state: AccountAnalyticsSourceState;
  /** Last successful scan, null when never scanned. */
  lastScanAt: string | null;
  /** Usage events retained from this tool and host. */
  rowCount: number;
  /** Fixed reason, e.g. why a tool has no local usage log. */
  detail: string | null;
}

export interface AccountAnalyticsSessionRow extends AccountAnalyticsActivityTotals {
  /** First 16 hex of SHA-256('aac-session-v1:' + tool + ':' + sessionId); stable and not reversible. */
  key: string;
  /** The provider that served most of the session's tokens. */
  provider: AccountAnalyticsUsageProvider;
  lastActivity: string;
  models: string[];
  target: string | null;
}

export type AccountAnalyticsAnomalyType =
  | 'cost_spike'
  | 'high_input'
  | 'high_io_ratio'
  | 'high_cache_read';

export interface AccountAnalyticsAnomalies {
  thresholds: {
    costSpikeMultiplier: 2;
    highInputTokens: 10_000_000;
    highIoRatio: 100;
    highCacheReadTokens: 1_000_000_000;
  };
  items: Array<{
    date: string;
    type: AccountAnalyticsAnomalyType;
    provider: AccountAnalyticsUsageProvider | null;
    model: string | null;
    value: number;
    threshold: number;
    message: string;
  }>;
  summary: {
    totalAnomalies: number;
    highInputDays: number;
    highIoRatioDays: number;
    costSpikeDays: number;
    highCacheReadDays: number;
  };
}

export interface AccountAnalyticsActivity {
  status: 'ok' | 'cached' | 'loading' | 'unavailable';
  /**
   * A collection is running in the background; the numbers are the last snapshot. Absent (or false)
   * means this answer is settled: 'cached' without refreshing is the best available, not a stale read.
   */
  refreshing?: boolean;
  /** Remote hosts the running collection is still waiting on; absent or empty when settled. */
  refreshingRemote?: AnalyticsRemoteHost[];
  scope: 'multi-host-cli';
  /** The zone used for `byDay`, `byDayModel` and anomaly dates. `byHour[].hour` stays a UTC instant. */
  timezone: string;
  accountAttribution: 'unavailable';
  costBasis: 'estimated-api-equivalent';
  fetchedAt: string | null;
  message: string;
  totals: AccountAnalyticsActivityTotals | null;
  /** One row per provider that served usage in range, in the dashboard's provider order, "other" last. */
  providers: Array<{
    provider: AccountAnalyticsUsageProvider;
    label: string;
    totals: AccountAnalyticsActivityTotals;
    usageEvents: number;
    /** Sessions with any usage this provider served; a session can count under several providers. */
    sessionCount: number;
    /** The tools whose logs hold this usage. */
    tools: AccountAnalyticsActivityProvider[];
  }>;
  /** `requestCount` is null when any hour in the bucket came from a snapshot without it. */
  byDay: Array<
    AccountAnalyticsActivityTotals & {
      date: string;
      provider: AccountAnalyticsUsageProvider;
      requestCount: number | null;
    }
  >;
  byHour: Array<
    AccountAnalyticsActivityTotals & {
      hour: string;
      provider: AccountAnalyticsUsageProvider;
      requestCount: number | null;
    }
  >;
  /**
   * One row per provider and model with usage in range, ranked by estimated cost and then by tokens. Every such
   * model is published (bounded at 500 rows); a cheap or unpriced model is never cut to make room.
   */
  models: Array<
    AccountAnalyticsActivityTotals & {
      model: string;
      provider: AccountAnalyticsUsageProvider;
      rates: AccountAnalyticsModelRates | null;
      /** The tools whose logs hold this provider's usage of the model. */
      tools: AccountAnalyticsActivityProvider[];
    }
  >;
  /** Top 12 models by estimated cost keep their name; the rest of each day fold into "Other models". */
  byDayModel: Array<
    AccountAnalyticsActivityTotals & {
      date: string;
      provider: AccountAnalyticsUsageProvider;
      model: string;
    }
  >;
  /** Sessions active in range, without paths, ids or project names. Null unless status is ok or cached. */
  sessions: {
    /** Distinct sessions, each counted once however many providers served it. */
    total: number;
    sample: AccountAnalyticsSessionRow[];
    truncated: boolean;
  } | null;
  anomalies: AccountAnalyticsAnomalies | null;
  /** One row per tool and host, in tool/host order; fixed entries explain missing tools. */
  sources: AccountAnalyticsSource[];
}

/** Internal facts the activity reader passes to the analytics service; never serialised. */
export interface AccountAnalyticsActivityCoverage {
  /** Oldest retained hourly bucket across both local CLI sources, or null. */
  oldestHourAt: number | null;
  /** Dashboard providers that served at least one retained hourly bucket in the requested range. */
  providersWithActivity: DashboardProvider[];
}

export interface AccountAnalytics {
  schemaVersion: 1;
  updatedAt: string;
  range: {
    preset: AccountAnalyticsRange;
    from: string;
    to: string;
    bucketMinutes: number;
    /** The zone the request named (UTC by default). */
    tz: string;
    /** The zone used for day buckets; equals `tz`. */
    dayBucketTz: string;
    /** Oldest retained quota sample or activity hour, at most 31 days back; bounds the custom picker. */
    availableFrom: string;
  };
  filters: Pick<AccountAnalyticsQuery, 'platform' | 'provider' | 'account'>;
  history: {
    status: 'ok' | 'unavailable';
    retentionDays: 30;
    collectedSince: string | null;
    oldestSampleAt: string | null;
    newestSampleAt: string | null;
    sampleCount: number;
    message: string;
  };
  summary: {
    accountCount: number;
    availableAccounts: number;
    /** @deprecated Equals `activeAccountIds.codex`; kept for one release. */
    activeCodexAccountId: string | null;
    /** The active account of each switchable provider, or null when none is active. */
    activeAccountIds: Partial<Record<'codex' | 'antigravity', string | null>>;
    sampleCount: number;
  };
  /**
   * Every provider with at least one account or local activity in range,
   * ordered by the provider table. The three counts follow the account filter.
   */
  providers: Array<{
    provider: DashboardProvider;
    label: string;
    order: number;
    visible: boolean;
    accountCount: number;
    availableAccounts: number;
    latestSampleAt: string | null;
    /** At least one retained quota sample in range for an account the account filter matches. */
    hasQuotaHistory: boolean;
    /** Claude Code or Codex local CLI activity in range. */
    hasActivity: boolean;
  }>;
  accounts: AccountAnalyticsAccount[];
  activity: AccountAnalyticsActivity;
}
