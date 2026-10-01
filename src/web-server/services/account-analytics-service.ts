import { getCcsDir } from '../../utils/config-manager';
import { getAccountDashboard } from './account-dashboard-service';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { getAccountAnalyticsActivity } from './account-analytics-activity';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import type {
  AccountAnalytics,
  AccountAnalyticsAccount,
  AccountAnalyticsActivity,
  AccountAnalyticsPoint,
  AccountAnalyticsQuery,
  AccountAnalyticsRange,
} from './account-analytics-types';
import {
  accountAnalyticsIdentity,
  analyticsWindowIdentity,
  appendAccountAnalyticsSnapshot,
  FileAccountAnalyticsHistoryStore,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsHistoryStore,
  type AccountAnalyticsObservation,
} from './account-analytics-history';

export interface AccountAnalyticsDeps {
  getDashboard?: typeof getAccountDashboard;
  getActivity?: (
    query: AccountAnalyticsQuery,
    from: number,
    to: number
  ) => Promise<AccountAnalyticsActivity>;
  createHistoryStore?: (scope: string) => AccountAnalyticsHistoryStore;
  scope?: () => string;
  now?: () => number;
  samplingIntervalMs?: () => number;
  schedule?: (callback: () => void, intervalMs: number) => ReturnType<typeof setTimeout>;
  cancelSchedule?: (timer: ReturnType<typeof setTimeout> | undefined) => void;
}

interface HistoryState {
  queue: Promise<void>;
  loaded: boolean;
  data: AccountAnalyticsHistoryData | null;
  available: boolean;
  store: AccountAnalyticsHistoryStore;
}

const RANGE_MS: Record<AccountAnalyticsRange, number> = {
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
  '30d': 30 * 24 * 60 * 60 * 1000,
};
const BUCKET_MINUTES: Record<AccountAnalyticsRange, number> = { '24h': 10, '7d': 60, '30d': 180 };

function unavailableActivity(message: string): AccountAnalyticsActivity {
  return {
    status: 'unavailable',
    scope: 'ubuntu-local-cli',
    timezone: 'UTC',
    accountAttribution: 'unavailable',
    costBasis: 'estimated-api-equivalent',
    fetchedAt: null,
    message,
    totals: null,
    providers: [],
    byDay: [],
    byHour: [],
    models: [],
  };
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

function accountSeries(
  account: DashboardAccount,
  history: AccountAnalyticsHistoryData | null,
  from: number,
  to: number,
  bucketMinutes: number
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
  return {
    ...account,
    firstSampleAt: records[0]?.sampledAt ?? null,
    lastSampleAt: records[records.length - 1]?.sampledAt ?? null,
    sampleCount: records.length,
    windows: [...preferredWindows.entries()].slice(0, 256).map(([identityKey, window]) => {
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
          .slice(-241),
      };
    }),
  };
}

/** Independent snapshot history keeps quotas separate from billed/token activity. */
export class AccountAnalyticsService {
  private readonly histories = new Map<string, HistoryState>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private pendingSample: Promise<void> | null = null;
  private running = false;
  private generation = 0;

  constructor(private readonly deps: AccountAnalyticsDeps = {}) {}

  private state(scope: string): HistoryState {
    let state = this.histories.get(scope);
    if (!state) {
      state = {
        queue: Promise.resolve(),
        loaded: false,
        data: null,
        available: true,
        store: (
          this.deps.createHistoryStore ??
          ((directory) => new FileAccountAnalyticsHistoryStore(directory))
        )(scope),
      };
      this.histories.set(scope, state);
      // Configuration scopes are few; this bound also prevents test/dev scopes
      // from accumulating data indefinitely in a long-lived dashboard.
      while (this.histories.size > 8) {
        const oldest = this.histories.keys().next().value;
        if (oldest === undefined) break;
        this.histories.delete(oldest);
      }
    }
    return state;
  }

  private async record(accounts: DashboardAccount[], scope: string): Promise<HistoryState> {
    const state = this.state(scope);
    state.queue = state.queue
      .then(async () => {
        if (!state.loaded) {
          try {
            state.data = await state.store.read();
            state.available = true;
          } catch {
            state.available = false;
          }
          state.loaded = true;
        }
        // Never overwrite an unreadable/unknown history file. Current snapshots
        // remain visible while persistence explicitly reports unavailable.
        if (!state.available) return;
        const next = appendAccountAnalyticsSnapshot(
          state.data,
          accounts,
          (this.deps.now ?? Date.now)()
        );
        const changed = JSON.stringify(next) !== JSON.stringify(state.data);
        state.data = next;
        if (changed) {
          try {
            await state.store.write(next);
          } catch {
            state.available = false;
          }
        }
      })
      .catch(() => {
        state.available = false;
      });
    await state.queue;
    return state;
  }

  async get(query: AccountAnalyticsQuery): Promise<AccountAnalytics> {
    const scope = (this.deps.scope ?? getCcsDir)();
    const dashboard = await (this.deps.getDashboard ?? getAccountDashboard)(query.platform, false);
    const state = await this.record(dashboard.accounts, scope);
    const now = (this.deps.now ?? Date.now)();
    const from = now - RANGE_MS[query.range];
    const accounts = dashboard.accounts.filter(
      (account) =>
        (query.provider === 'all' || query.provider === account.provider) &&
        (query.account === 'all' || query.account === account.id)
    );
    const series = accounts.map((account) =>
      accountSeries(account, state.data, from, now, BUCKET_MINUTES[query.range])
    );
    const sampleCount = series.reduce((sum, account) => sum + account.sampleCount, 0);
    const stamps = series
      .flatMap((account) => [account.firstSampleAt, account.lastSampleAt])
      .filter((stamp): stamp is string => stamp !== null)
      .sort();
    const providers = [...new Set(accounts.map((account) => account.provider))].map((provider) => {
      const matches = series.filter((account) => account.provider === provider);
      return {
        provider,
        label: matches[0].providerLabel,
        accountCount: matches.length,
        availableAccounts: matches.filter(
          (account) => account.status === 'ok' || account.status === 'cached'
        ).length,
        latestSampleAt:
          matches
            .map((account) => account.lastSampleAt)
            .filter((stamp): stamp is string => stamp !== null)
            .sort()
            .pop() ?? null,
      };
    });
    const activity = await (this.deps.getActivity ?? getAccountAnalyticsActivity)(
      query,
      from,
      now
    ).catch(() => unavailableActivity('Local usage history is temporarily unavailable.'));
    return {
      schemaVersion: 1,
      updatedAt: new Date(now).toISOString(),
      range: {
        preset: query.range,
        from: new Date(from).toISOString(),
        to: new Date(now).toISOString(),
        bucketMinutes: BUCKET_MINUTES[query.range],
      },
      filters: { platform: query.platform, provider: query.provider, account: query.account },
      history: {
        status: state.available ? 'ok' : 'unavailable',
        retentionDays: 30,
        collectedSince: state.data?.collectedSince ?? null,
        oldestSampleAt: stamps[0] ?? null,
        newestSampleAt: stamps[stamps.length - 1] ?? null,
        sampleCount,
        message: !state.available
          ? 'Quota history could not be saved or read. Current account usage remains available.'
          : sampleCount === 0
            ? 'No measured quota history is available for this selection yet.'
            : 'Quota observations are retained for up to 30 days, with older readings downsampled hourly. Missing readings remain gaps; percentages and balances are not added or averaged.',
      },
      summary: {
        accountCount: accounts.length,
        availableAccounts: accounts.filter(
          (account) => account.status === 'ok' || account.status === 'cached'
        ).length,
        activeCodexAccountId:
          dashboard.accounts.find((account) => account.provider === 'codex' && account.isActive)
            ?.id ?? null,
        sampleCount,
      },
      providers,
      accounts: series,
      activity,
    };
  }

  /** Samples already-bounded dashboard collectors, coalescing both platforms. */
  sample(): Promise<void> {
    if (this.pendingSample) return this.pendingSample;
    const scope = (this.deps.scope ?? getCcsDir)();
    this.pendingSample = (async () => {
      const dashboards = await Promise.allSettled(
        ['mac', 'windows'].map((platform) =>
          (this.deps.getDashboard ?? getAccountDashboard)(platform as 'mac' | 'windows', false)
        )
      );
      const unique = new Map<string, DashboardAccount>();
      for (const result of dashboards)
        if (result.status === 'fulfilled')
          for (const account of result.value.accounts) {
            const identity = accountAnalyticsIdentity(account);
            const previous = unique.get(identity);
            const sampled = Date.parse(account.sampledAt ?? account.fetchedAt ?? '');
            const oldSampled = previous
              ? Date.parse(previous.sampledAt ?? previous.fetchedAt ?? '')
              : -Infinity;
            const available = account.status === 'ok' || account.status === 'cached';
            const previouslyAvailable = previous?.status === 'ok' || previous?.status === 'cached';
            if (
              !previous ||
              (available && !previouslyAvailable) ||
              (available === previouslyAvailable &&
                Number.isFinite(sampled) &&
                (!Number.isFinite(oldSampled) || sampled >= oldSampled))
            )
              unique.set(identity, account);
          }
      await this.record([...unique.values()], scope);
    })().finally(() => {
      this.pendingSample = null;
    });
    return this.pendingSample;
  }

  private schedule(generation: number): void {
    if (!this.running || generation !== this.generation) return;
    const configured =
      this.deps.samplingIntervalMs?.() ?? getAccountRefreshIntervalSeconds() * 1000;
    const interval = Number.isFinite(configured)
      ? Math.min(3_600_000, Math.max(30_000, configured))
      : 60_000;
    this.timer = (this.deps.schedule ?? setTimeout)(() => {
      if (!this.running || generation !== this.generation) return;
      this.timer = undefined;
      void this.sample()
        .catch(() => {})
        .finally(() => this.schedule(generation));
    }, interval);
    this.timer.unref?.();
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    const generation = ++this.generation;
    void this.sample()
      .catch(() => {})
      .finally(() => this.schedule(generation));
  }

  /** Changing settings replaces the deadline without letting an old poll fork timers. */
  reschedule(): void {
    if (!this.running) return;
    (this.deps.cancelSchedule ?? clearTimeout)(this.timer);
    this.timer = undefined;
    this.schedule(++this.generation);
  }

  stop(): void {
    this.running = false;
    ++this.generation;
    (this.deps.cancelSchedule ?? clearTimeout)(this.timer);
    this.timer = undefined;
  }
}

let service: AccountAnalyticsService | undefined;
function singleton(): AccountAnalyticsService {
  service ??= new AccountAnalyticsService();
  return service;
}
export function getAccountAnalytics(query: AccountAnalyticsQuery): Promise<AccountAnalytics> {
  return singleton().get(query);
}
export function startAccountAnalyticsSampling(): void {
  singleton().start();
}
export function stopAccountAnalyticsSampling(): void {
  service?.stop();
}
export function rescheduleAccountAnalyticsSampling(): void {
  service?.reschedule();
}
