/**
 * The pure part of one Analytics computation: the oldest available time, the final range, and the
 * answer around the activity block. Its inputs are what the computation already read (the dashboard,
 * the quota history, the activity coverage, the clock). No collector, store, clock or file is read
 * here. This is the reference the M3 Rust composer (`crates/aac-views/src/analytics.rs`) must equal.
 */
import { resolveAccountAnalyticsRange } from './account-analytics-range';
import type { ResolvedAccountAnalyticsRange } from './account-analytics-range';
import {
  analyticsProviderRows,
  dashboardRegistryFacts,
  type AnalyticsProviderEntry,
} from './account-analytics-providers';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import type {
  AccountAnalytics,
  AccountAnalyticsAccount,
  AccountAnalyticsPoint,
  AccountAnalyticsQuery,
} from './account-analytics-types';
import {
  accountAnalyticsIdentity,
  analyticsWindowIdentity,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsObservation,
} from './account-analytics-history';
import { STORE_RETENTION_DAYS } from '../../store/store-service';

/** 31 days of 3-hour buckets plus the partial one at the start. */
const MAX_POINTS_PER_WINDOW = 249;

/** Everything of an Analytics answer except its `activity` block, which the activity reader builds. */
export type AccountAnalyticsComposed = Omit<AccountAnalytics, 'activity'>;

export interface AccountAnalyticsRangeInput {
  /** The validated query with its zone resolved. */
  query: AccountAnalyticsQuery;
  now: number;
  /** The oldest time the readers cover: the stored bound, else 31 days back. */
  retainedFrom: number;
  bounds: { oldest: number } | null;
  /** Oldest stored quota sample; `Infinity` when there is none. */
  oldestQuota: number;
  /** Oldest activity hour the coverage reports, when it reports one. */
  coverageOldestHourAt: number | null | undefined;
  /** The range resolved before the activity read (every preset but `all` keeps it). */
  provisional: ResolvedAccountAnalyticsRange;
  /** Local midnight today in the query's zone; read only when no oldest time is known. */
  todayMidnight: () => number;
}

/** The oldest available time and the final range of one computation. */
export function analyticsAvailableRange(input: AccountAnalyticsRangeInput): {
  availableFrom: number;
  range: ResolvedAccountAnalyticsRange;
} {
  const { query, now, retainedFrom, bounds, oldestQuota, provisional } = input;
  const oldest = Math.min(oldestQuota, input.coverageOldestHourAt ?? Infinity);
  const availableFrom = Number.isFinite(oldest)
    ? Math.min(now, Math.max(retainedFrom, oldest))
    : Math.max(retainedFrom, input.todayMidnight());
  const range =
    query.range === 'all'
      ? resolveAccountAnalyticsRange(query, now, availableFrom, bounds ?? undefined)
      : provisional;
  return { availableFrom, range };
}

export interface AccountAnalyticsComposeInput {
  query: AccountAnalyticsQuery;
  now: number;
  tz: string;
  range: ResolvedAccountAnalyticsRange;
  availableFrom: number;
  /** `activity.resolution`, when the activity reader set one. */
  resolution: 'hour' | 'day' | undefined;
  /** Whether the quota history could be read and saved. */
  available: boolean;
  /** Whether the history store (rather than the JSON history file) serves this scope. */
  stored: boolean;
  oldestQuota: number;
  history: AccountAnalyticsHistoryData | null;
  dashboard: { accounts: DashboardAccount[]; providers?: unknown; settings?: unknown };
  providerTable: AnalyticsProviderEntry[];
  providersWithActivity: readonly string[];
}

function point(
  record: AccountAnalyticsObservation,
  window: DashboardAccountWindow | undefined
): AccountAnalyticsPoint {
  const cachedWindow = window?.status === 'cached';
  return {
    sampledAt: cachedWindow ? (window?.sampledAt ?? '') : record.sampledAt,
    observedAt: record.observedAt,
    usedPercent: window?.usedPercent ?? null,
    remainingPercent: window?.remainingPercent ?? null,
    used: window?.used ?? null,
    limit: window?.limit ?? null,
    remaining: window?.remaining ?? null,
    resetAt: window?.resetAt ?? null,
    expiresAt: window?.expiresAt ?? null,
    source: cachedWindow ? `${record.source} (cached window)` : record.source,
    platform: record.platform,
    status: cachedWindow ? 'cached' : record.status,
    isActive: record.isActive,
  };
}

function antigravitySeriesOrder(key: string): number {
  if (key === 'cli-gemini-weekly') return 0;
  if (key === 'gemini-weekly') return 1;
  return 2;
}

export function accountSeries(
  account: DashboardAccount,
  history: AccountAnalyticsHistoryData | null,
  from: number,
  to: number,
  bucketMinutes: number,
  registry: { hidden: boolean }
): AccountAnalyticsAccount {
  const identity = accountAnalyticsIdentity(account);
  const records = (history?.records ?? []).filter(
    (record) =>
      record.identity === identity &&
      Date.parse(record.sampledAt) >= from &&
      Date.parse(record.sampledAt) <= to
  );
  const windows = new Map<string, DashboardAccountWindow>();
  for (const record of records)
    for (const window of record.windows) windows.set(analyticsWindowIdentity(window), window);
  for (const window of account.windows) windows.set(analyticsWindowIdentity(window), window);
  const currentWindows = new Map<string, DashboardAccountWindow>();
  for (const window of account.windows) currentWindows.set(analyticsWindowIdentity(window), window);
  const preferredWindows = new Map(currentWindows);
  for (const [key, window] of [...windows.entries()].reverse())
    if (!preferredWindows.has(key)) preferredWindows.set(key, window);
  const entries = [...preferredWindows.entries()];
  if (account.provider === 'antigravity') {
    entries.sort(([, a], [, b]) => antigravitySeriesOrder(a.key) - antigravitySeriesOrder(b.key));
  }
  return {
    ...account,
    hidden: registry.hidden,
    firstSampleAt: records[0]?.sampledAt ?? null,
    lastSampleAt: records[records.length - 1]?.sampledAt ?? null,
    sampleCount: records.length,
    windows: entries.slice(0, 256).map(([identityKey, window]) => {
      const buckets = new Map<number, AccountAnalyticsPoint>();
      for (const record of records) {
        const matching = record.windows.find(
          (candidate) => analyticsWindowIdentity(candidate) === identityKey
        );
        const actualPoint = point(record, matching);
        const sampleTime = Date.parse(actualPoint.sampledAt);
        // Retained balances keep the measured time, even while core quota is
        // refreshed repeatedly. Do not add fresh constant-balance points.
        if (!Number.isFinite(sampleTime) || sampleTime < from || sampleTime > to) continue;
        const bucket = Math.floor(sampleTime / (bucketMinutes * 60_000));
        buckets.set(bucket, actualPoint);
      }
      return {
        ...window,
        // Historical series remain selectable, but an absent current reading
        // must never inherit a prior source's amounts, reset, or plan flags.
        ...(!currentWindows.has(identityKey)
          ? {
              usedPercent: null,
              remainingPercent: null,
              used: null,
              limit: null,
              remaining: null,
              resetAt: null,
              expiresAt: null,
              unlimited: undefined,
              enabled: undefined,
            }
          : {}),
        points: [...buckets.values()]
          .sort((a, b) => Date.parse(a.sampledAt) - Date.parse(b.sampledAt))
          .slice(-MAX_POINTS_PER_WINDOW),
      };
    }),
  };
}

/** The answer of one computation without its activity block. */
export function composeAccountAnalytics(
  input: AccountAnalyticsComposeInput
): AccountAnalyticsComposed {
  const { query, now, tz, range, availableFrom, history, dashboard, stored, oldestQuota } = input;
  const { from, to } = range;
  const registry = dashboardRegistryFacts(dashboard, input.providerTable);
  const accounts = dashboard.accounts.filter(
    (account) =>
      (query.provider === 'all' || query.provider === account.provider) &&
      (query.account === 'all' || query.account === account.id)
  );
  const series = accounts.map((account) =>
    accountSeries(account, history, from, to, range.bucketMinutes, {
      hidden: registry.hidden(account),
    })
  );
  const sampleCount = series.reduce((sum, account) => sum + account.sampleCount, 0);
  const stamps = series
    .flatMap((account) => [account.firstSampleAt, account.lastSampleAt])
    .filter((stamp): stamp is string => stamp !== null)
    .sort();

  const providers = analyticsProviderRows({
    registry,
    accounts: dashboard.accounts,
    account: query.account,
    records: history?.records ?? [],
    from,
    to,
    providersWithActivity: input.providersWithActivity,
  });
  const activeAccountIds: AccountAnalytics['summary']['activeAccountIds'] = {};
  for (const entry of registry.table)
    if (entry.id === 'codex' || entry.id === 'antigravity')
      activeAccountIds[entry.id] =
        dashboard.accounts.find((account) => account.provider === entry.id && account.isActive)
          ?.id ?? null;
  const activeCodexAccountId =
    dashboard.accounts.find((account) => account.provider === 'codex' && account.isActive)?.id ??
    null;
  return {
    schemaVersion: 1,
    updatedAt: new Date(now).toISOString(),
    range: {
      preset: range.preset,
      from: new Date(from).toISOString(),
      to: new Date(to).toISOString(),
      bucketMinutes: range.bucketMinutes,
      tz,
      dayBucketTz: tz,
      availableFrom: new Date(availableFrom).toISOString(),
      resolution: input.resolution ?? 'hour',
      ...(query.range === 'custom' && query.from && query.to
        ? { days: { from: query.from, to: query.to } }
        : {}),
    },
    filters: { platform: query.platform, provider: query.provider, account: query.account },
    history: {
      status: input.available ? 'ok' : 'unavailable',
      retentionDays: stored ? STORE_RETENTION_DAYS : 30,
      collectedSince:
        (stored && Number.isFinite(oldestQuota) ? new Date(oldestQuota).toISOString() : null) ??
        history?.collectedSince ??
        null,
      oldestSampleAt: stamps[0] ?? null,
      newestSampleAt: stamps[stamps.length - 1] ?? null,
      sampleCount,
      message: !input.available
        ? 'Quota history could not be saved or read. Current account usage remains available.'
        : sampleCount === 0
          ? 'No measured quota history is available for this selection yet.'
          : stored
            ? 'Every changed quota reading is kept for 90 days, and hourly and daily summaries are kept after that. Missing readings remain gaps; percentages and balances are not added or averaged.'
            : 'Quota observations are retained for up to 30 days, with older readings downsampled hourly. Missing readings remain gaps; percentages and balances are not added or averaged.',
    },
    summary: {
      accountCount: accounts.length,
      availableAccounts: accounts.filter(
        (account) => account.status === 'ok' || account.status === 'cached'
      ).length,
      activeCodexAccountId,
      activeAccountIds,
      sampleCount,
    },
    providers,
    accounts: series,
  };
}
