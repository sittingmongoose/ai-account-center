import type {
  ClaudeDashboardPlatform,
  DashboardAccount,
  DashboardAccountStatus,
  DashboardAccountWindow,
  DashboardPlatform,
  DashboardProvider,
} from './account-dashboard-types';
import type { AccountAnalyticsRangePreset } from './account-analytics-range';
import type { ModelPricingSource } from '../model-pricing';

export type AccountAnalyticsRange = AccountAnalyticsRangePreset;
export interface AccountAnalyticsQuery {
  platform: ClaudeDashboardPlatform;
  range: AccountAnalyticsRange;
  provider: DashboardProvider | 'all';
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

export type AccountAnalyticsActivityProvider = 'claude' | 'codex';

export interface AccountAnalyticsSessionRow extends AccountAnalyticsActivityTotals {
  /** First 16 hex of SHA-256('aac-session-v1:' + provider + ':' + sessionId); stable and not reversible. */
  key: string;
  provider: AccountAnalyticsActivityProvider;
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
    provider: AccountAnalyticsActivityProvider | null;
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
  scope: 'ubuntu-local-cli';
  /** The zone used for `byDay`, `byDayModel` and anomaly dates. `byHour[].hour` stays a UTC instant. */
  timezone: string;
  accountAttribution: 'unavailable';
  costBasis: 'estimated-api-equivalent';
  fetchedAt: string | null;
  message: string;
  totals: AccountAnalyticsActivityTotals | null;
  providers: Array<{
    provider: AccountAnalyticsActivityProvider;
    label: string;
    totals: AccountAnalyticsActivityTotals;
    usageEvents: number;
    sessionCount: number;
  }>;
  /** `requestCount` is null when any hour in the bucket came from a snapshot without it. */
  byDay: Array<
    AccountAnalyticsActivityTotals & {
      date: string;
      provider: AccountAnalyticsActivityProvider;
      requestCount: number | null;
    }
  >;
  byHour: Array<
    AccountAnalyticsActivityTotals & {
      hour: string;
      provider: AccountAnalyticsActivityProvider;
      requestCount: number | null;
    }
  >;
  models: Array<
    AccountAnalyticsActivityTotals & {
      model: string;
      provider: AccountAnalyticsActivityProvider;
      rates: AccountAnalyticsModelRates | null;
    }
  >;
  /** Top 12 models by estimated cost keep their name; the rest of each day fold into "Other models". */
  byDayModel: Array<
    AccountAnalyticsActivityTotals & {
      date: string;
      provider: AccountAnalyticsActivityProvider;
      model: string;
    }
  >;
  /** Sessions active in range, without paths, ids or project names. Null unless status is ok or cached. */
  sessions: {
    total: number;
    sample: AccountAnalyticsSessionRow[];
    truncated: boolean;
  } | null;
  anomalies: AccountAnalyticsAnomalies | null;
}

/** Internal facts the activity reader passes to the analytics service; never serialised. */
export interface AccountAnalyticsActivityCoverage {
  /** Oldest retained hourly bucket across both local CLI sources, or null. */
  oldestHourAt: number | null;
  /** Providers with at least one local hourly bucket in the requested range. */
  providersWithActivity: AccountAnalyticsActivityProvider[];
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
    /** At least one retained quota sample in range. */
    hasQuotaHistory: boolean;
    /** Claude Code or Codex local CLI activity in range. */
    hasActivity: boolean;
  }>;
  accounts: AccountAnalyticsAccount[];
  activity: AccountAnalyticsActivity;
}
