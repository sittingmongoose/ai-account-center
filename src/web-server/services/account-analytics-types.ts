import type {
  ClaudeDashboardPlatform,
  DashboardAccount,
  DashboardAccountStatus,
  DashboardAccountWindow,
  DashboardPlatform,
  DashboardProvider,
} from './account-dashboard-types';

export type AccountAnalyticsRange = '24h' | '7d' | '30d';
export interface AccountAnalyticsQuery {
  platform: ClaudeDashboardPlatform;
  range: AccountAnalyticsRange;
  provider: DashboardProvider | 'all';
  account: string;
  /** Explicit manual refresh of native activity; quota refresh remains separate. */
  refresh?: boolean;
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
}

export interface AccountAnalyticsActivity {
  status: 'ok' | 'cached' | 'loading' | 'unavailable';
  scope: 'ubuntu-local-cli';
  timezone: 'UTC';
  accountAttribution: 'unavailable';
  costBasis: 'estimated-api-equivalent';
  fetchedAt: string | null;
  message: string;
  totals: AccountAnalyticsActivityTotals | null;
  providers: Array<{
    provider: 'claude' | 'codex';
    label: string;
    totals: AccountAnalyticsActivityTotals;
    usageEvents: number;
    sessionCount: number;
  }>;
  byDay: Array<AccountAnalyticsActivityTotals & { date: string; provider: 'claude' | 'codex' }>;
  byHour: Array<AccountAnalyticsActivityTotals & { hour: string; provider: 'claude' | 'codex' }>;
  models: Array<AccountAnalyticsActivityTotals & { model: string; provider: 'claude' | 'codex' }>;
}

export interface AccountAnalytics {
  schemaVersion: 1;
  updatedAt: string;
  range: { preset: AccountAnalyticsRange; from: string; to: string; bucketMinutes: number };
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
    activeCodexAccountId: string | null;
    sampleCount: number;
  };
  providers: Array<{
    provider: DashboardProvider;
    label: string;
    accountCount: number;
    availableAccounts: number;
    latestSampleAt: string | null;
  }>;
  accounts: AccountAnalyticsAccount[];
  activity: AccountAnalyticsActivity;
}
