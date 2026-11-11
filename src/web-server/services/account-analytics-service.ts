import { getCcsDir } from '../../utils/config-manager';
import { getAccountDashboard } from './account-dashboard-service';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { readDashboardPreferences } from './dashboard-preferences';
import {
  fixedAnalyticsSourceEntries,
  getAccountAnalyticsActivity,
  type AccountAnalyticsActivityResult,
} from './account-analytics-activity';
import {
  ACCOUNT_ANALYTICS_RETAINED_MS,
  canonicalAccountAnalyticsTimeZone,
  localDate,
  localMidnight,
  resolveAccountAnalyticsRange,
  validateAccountAnalyticsRangeShape,
} from './account-analytics-range';
import {
  analyticsProviderRows,
  dashboardRegistryFacts,
  defaultAnalyticsProviderTable,
  type AnalyticsProviderEntry,
} from './account-analytics-providers';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import type {
  AccountAnalytics,
  AccountAnalyticsAccount,
  AccountAnalyticsActivity,
  AccountAnalyticsPoint,
  AccountAnalyticsQuery,
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
import {
  openAacStore,
  STORE_RETENTION_DAYS,
  type AacStore,
  type LongBurnSpan,
} from '../../store/store-service';
import {
  MAX_ACCOUNTS_PER_SAMPLE,
  storeObservationFromAccount,
} from '../../store/usage-observations';
import { backfillUsageHistory } from '../../store/usage-backfill';

export interface AccountAnalyticsDeps {
  getDashboard?: typeof getAccountDashboard;
  getActivity?: (
    query: AccountAnalyticsQuery,
    from: number,
    to: number,
    options?: { tz?: string }
  ) => Promise<AccountAnalyticsActivityResult>;
  /** The server's provider table; a dashboard registry, when present, overrides labels and order. */
  providerTable?: () => AnalyticsProviderEntry[];
  createHistoryStore?: (scope: string) => AccountAnalyticsHistoryStore;
  scope?: () => string;
  now?: () => number;
  samplingIntervalMs?: () => number;
  schedule?: (callback: () => void, intervalMs: number) => ReturnType<typeof setTimeout>;
  cancelSchedule?: (timer: ReturnType<typeof setTimeout> | undefined) => void;
  /**
   * The history store for this scope, or null for the JSON history file. By default the
   * process's store (docs/store.md) serves the default scope unless a JSON store is injected.
   */
  usageStore?: (scope: string) => Promise<AacStore | null>;
}

/** Long-period burn of one window: 7 and 30 days across resets, from the store's rollups. */
export interface AccountAnalyticsLongBurn {
  last7Days: LongBurnSpan;
  last30Days: LongBurnSpan;
}

/**
 * What one finished sample saw: the accounts it recorded (one row per account, the best of both
 * dashboard views) and the quota history after recording them, or null while that history cannot
 * be read. Listeners run after the sample, never inside it, and cannot delay or fail it.
 */
export interface AccountAnalyticsSampleEvent {
  accounts: DashboardAccount[];
  history: AccountAnalyticsHistoryData | null;
  sampledAt: number;
  /**
   * With the store: each live window's 7- and 30-day burn, keyed by `longBurnKey`. Absent
   * without the store.
   */
  longBurn?: Map<string, AccountAnalyticsLongBurn>;
}

const DAY_MS = 86_400_000;
const STORE_RETENTION_MS = STORE_RETENTION_DAYS * DAY_MS;
/** The 7-day rolling history the agent snapshot publishes. */
const SNAPSHOT_HISTORY_MS = 7 * DAY_MS;

/** The key of `AccountAnalyticsSampleEvent.longBurn`: account identity and window identity. */
export function longBurnKey(account: DashboardAccount, window: DashboardAccountWindow): string {
  return JSON.stringify([accountAnalyticsIdentity(account), analyticsWindowIdentity(window)]);
}

async function defaultUsageStore(scope: string): Promise<AacStore | null> {
  // The store lives under the default CCS folder; another scope keeps its JSON file.
  if (scope !== getCcsDir()) return null;
  const opened = await openAacStore();
  return opened.store;
}
export type AccountAnalyticsSampleListener = (event: AccountAnalyticsSampleEvent) => unknown;

const sampleListeners = new Set<AccountAnalyticsSampleListener>();

/** Called after every finished sample (each refresh interval). Returns the unsubscribe call. */
export function onAccountAnalyticsSample(listener: AccountAnalyticsSampleListener): () => void {
  sampleListeners.add(listener);
  return () => {
    sampleListeners.delete(listener);
  };
}

function notifySample(event: AccountAnalyticsSampleEvent): void {
  for (const listener of sampleListeners) {
    try {
      void Promise.resolve(listener(event)).catch(() => {});
    } catch {
      /* A listener never fails a sample. */
    }
  }
}

interface HistoryState {
  queue: Promise<void>;
  revision: number;
  loaded: boolean;
  data: AccountAnalyticsHistoryData | null;
  available: boolean;
  store: AccountAnalyticsHistoryStore;
  /** The history store, when this scope uses it; then `data` is not kept in memory. */
  usageStore: AacStore | null;
  backfilled: boolean;
}

/** 31 days of 3-hour buckets plus the partial one at the start. */
const MAX_POINTS_PER_WINDOW = 249;

function unavailableActivity(message: string, tz: string): AccountAnalyticsActivity {
  return {
    status: 'unavailable',
    scope: 'multi-host-cli',
    timezone: tz,
    accountAttribution: 'unavailable',
    costBasis: 'estimated-api-equivalent',
    fetchedAt: null,
    message,
    totals: null,
    providers: [],
    byDay: [],
    byHour: [],
    models: [],
    byDayModel: [],
    sessions: null,
    anomalies: null,
    sources: fixedAnalyticsSourceEntries(),
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
  bucketMinutes: number,
  registry: { switchable: boolean; hidden: boolean }
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
    switchable: registry.switchable,
    hidden: registry.hidden,
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
          .slice(-MAX_POINTS_PER_WINDOW),
      };
    }),
  };
}

/** 7- and 30-day burn for every live percent window, from the store's hourly rollups. */
async function longBurnFor(
  store: AacStore,
  accounts: DashboardAccount[],
  now: number
): Promise<Map<string, AccountAnalyticsLongBurn>> {
  const refs: Array<{ identity: string; window: string; key: string }> = [];
  for (const account of accounts)
    for (const window of account.windows ?? [])
      if (typeof window.usedPercent === 'number' && (window.kind ?? 'rate_limit') === 'rate_limit')
        refs.push({
          identity: accountAnalyticsIdentity(account),
          window: analyticsWindowIdentity(window),
          key: longBurnKey(account, window),
        });
  const result = new Map<string, AccountAnalyticsLongBurn>();
  if (refs.length === 0) return result;
  const answer = await store.burnLong(
    refs.map(({ identity, window }) => ({ identity, window })),
    [7 * DAY_MS, 30 * DAY_MS],
    now
  );
  answer.forEach((entry, index) => {
    const [last7Days, last30Days] = entry.spans;
    if (last7Days && last30Days) result.set(refs[index].key, { last7Days, last30Days });
  });
  return result;
}

/** Independent snapshot history keeps quotas separate from billed/token activity. */
export class AccountAnalyticsService {
  private readonly answers = new Map<
    string,
    { activity: AccountAnalyticsActivityResult; answer: AccountAnalytics }
  >();
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
        revision: 0,
        loaded: false,
        data: null,
        available: true,
        store: (
          this.deps.createHistoryStore ??
          ((directory) => new FileAccountAnalyticsHistoryStore(directory))
        )(scope),
        usageStore: null,
        backfilled: false,
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

  private usageStoreFor(scope: string): Promise<AacStore | null> {
    if (this.deps.usageStore) return this.deps.usageStore(scope);
    // An injected JSON history store (tests, other scopes) keeps the JSON path.
    if (this.deps.createHistoryStore) return Promise.resolve(null);
    return defaultUsageStore(scope);
  }

  private async record(accounts: DashboardAccount[], scope: string): Promise<HistoryState> {
    const state = this.state(scope);
    state.queue = state.queue
      .then(async () => {
        if (!state.loaded) {
          state.usageStore = await this.usageStoreFor(scope).catch(() => null);
          if (!state.usageStore) {
            try {
              state.data = await state.store.read();
              state.available = true;
            } catch {
              state.available = false;
            }
          }
          state.loaded = true;
        }
        if (state.usageStore) {
          await this.recordInStore(state, accounts, scope);
          return;
        }
        // Never overwrite an unreadable/unknown history file. Current snapshots
        // remain visible while persistence explicitly reports unavailable.
        if (!state.available) return;
        const previous = state.data;
        const next = appendAccountAnalyticsSnapshot(
          previous,
          accounts,
          (this.deps.now ?? Date.now)()
        );
        // appendAccountAnalyticsSnapshot carries records it did not replace over by
        // object identity, and schemaVersion/collectedSince are fixed for a non-null
        // history, so an equal length with all-equal references serializes exactly
        // like the previous data. A null history always counts as changed.
        const changed =
          previous === null ||
          next.records.length !== previous.records.length ||
          next.records.some((record, index) => record !== previous.records[index]);
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
    state.revision++;
    return state;
  }

  /** The store decides which readings are new; the JSON history is imported once, first. */
  private async recordInStore(
    state: HistoryState,
    accounts: DashboardAccount[],
    scope: string
  ): Promise<void> {
    const store = state.usageStore as AacStore;
    if (!state.backfilled) {
      const providers = new Map(
        accounts.map((account) => [accountAnalyticsIdentity(account), account.provider])
      );
      // A failed import is retried at the next sample; live readings wait for it, so the
      // older history is never refused as older than the latest reading.
      await backfillUsageHistory(store, scope, providers);
      state.backfilled = true;
    }
    const now = (this.deps.now ?? Date.now)();
    const observations = accounts.slice(0, MAX_ACCOUNTS_PER_SAMPLE).flatMap((account) => {
      const observation = storeObservationFromAccount(account, now, STORE_RETENTION_MS);
      return observation ? [observation] : [];
    });
    try {
      await store.record(observations);
      state.available = true;
    } catch {
      state.available = false;
    }
  }

  /** The history for a time range: from the store (thinned there) or the in-memory JSON. */
  private async historyFor(
    state: HistoryState,
    fromMs: number,
    toMs: number,
    nowMs: number,
    bucketMs: number | null
  ): Promise<AccountAnalyticsHistoryData | null> {
    if (!state.usageStore) return state.data;
    try {
      const records = await state.usageStore.historyRecords({
        fromMs: Math.max(0, Math.floor(fromMs)),
        toMs: Math.max(0, Math.ceil(toMs)),
        nowMs,
        thinning: bucketMs === null ? { kind: 'history' } : { kind: 'bucket', bucketMs },
      });
      return {
        schemaVersion: 1,
        collectedSince: records[0]?.sampledAt ?? new Date(nowMs).toISOString(),
        records,
      };
    } catch {
      state.available = false;
      return null;
    }
  }

  async get(requested: AccountAnalyticsQuery): Promise<AccountAnalytics> {
    // Malformed ranges and zones fail before any collector or history read.
    validateAccountAnalyticsRangeShape(requested);
    const scope = (this.deps.scope ?? getCcsDir)();
    // Without a zone in the request, the day buckets follow the display time
    // zone from Settings (America/New_York until one is saved).
    const tz = canonicalAccountAnalyticsTimeZone(
      requested.tz ?? readDashboardPreferences(scope).timeZone
    );
    const query: AccountAnalyticsQuery = { ...requested, tz };
    const clock = this.deps.now ?? Date.now;
    // So does a custom range outside retention: a refused request records no
    // quota snapshot. The range is resolved again after the snapshot, whose
    // sample times must not fall after `to`.
    const checkedAt = clock();
    resolveAccountAnalyticsRange(query, checkedAt, checkedAt - ACCOUNT_ANALYTICS_RETAINED_MS);
    const dashboard = await (this.deps.getDashboard ?? getAccountDashboard)(query.platform, false);
    const state = this.state(scope);
    // Reads initialise history but never append observations; the sampler owns writes.
    if (!state.loaded) {
      state.queue = state.queue.then(async () => {
        if (state.loaded) return;
        state.usageStore = await this.usageStoreFor(scope).catch(() => null);
        if (!state.usageStore) {
          try {
            state.data = await state.store.read();
          } catch {
            state.available = false;
          }
        }
        state.loaded = true;
      });
      await state.queue;
    }
    const now = clock();
    const retainedFrom = now - ACCOUNT_ANALYTICS_RETAINED_MS;
    // Every range except `all` resolves without the data; `all` reads the
    // whole retained window and then starts at the oldest retained point.
    const provisional = resolveAccountAnalyticsRange(query, now, retainedFrom);
    const activityResult = await (this.deps.getActivity ?? getAccountAnalyticsActivity)(
      query,
      provisional.from,
      provisional.to,
      { tz }
    ).catch((): AccountAnalyticsActivityResult =>
      unavailableActivity('Local usage history is temporarily unavailable.', tz)
    );
    const answerKey = JSON.stringify([
      scope,
      query,
      state.revision,
      Math.floor(now / 60_000),
      dashboard.accounts,
      dashboard.providers,
      dashboard.settings,
      (this.deps.providerTable ?? defaultAnalyticsProviderTable)(),
    ]);
    const retained = this.answers.get(answerKey);
    if (retained?.activity === activityResult)
      return {
        ...retained.answer,
        cached: true,
        cacheAgeSeconds: Math.max(0, now - Date.parse(retained.answer.updatedAt)) / 1000,
      };
    const { coverage, ...activity } = activityResult;
    let oldestQuota = Infinity;
    if (state.usageStore) {
      const stored = await state.usageStore.oldest().catch(() => null);
      if (stored !== null && Number.isFinite(stored)) oldestQuota = stored;
    } else {
      for (const record of state.data?.records ?? []) {
        const sampled = Date.parse(record.sampledAt);
        if (Number.isFinite(sampled) && sampled < oldestQuota) oldestQuota = sampled;
      }
    }
    const oldest = Math.min(oldestQuota, coverage?.oldestHourAt ?? Infinity);
    const availableFrom = Number.isFinite(oldest)
      ? Math.min(now, Math.max(retainedFrom, oldest))
      : Math.max(retainedFrom, localMidnight(localDate(now, tz), tz));
    const range =
      query.range === 'all' ? resolveAccountAnalyticsRange(query, now, availableFrom) : provisional;
    const { from, to } = range;
    const history = await this.historyFor(state, from, to, now, range.bucketMinutes * 60_000);
    const stored = state.usageStore !== null;

    const registry = dashboardRegistryFacts(
      dashboard,
      (this.deps.providerTable ?? defaultAnalyticsProviderTable)()
    );
    const accounts = dashboard.accounts.filter(
      (account) =>
        (query.provider === 'all' || query.provider === account.provider) &&
        (query.account === 'all' || query.account === account.id)
    );
    const series = accounts.map((account) =>
      accountSeries(account, history, from, to, range.bucketMinutes, {
        switchable: registry.switchable(account),
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
      providersWithActivity: coverage?.providersWithActivity ?? [],
    });
    const activeAccountIds: AccountAnalytics['summary']['activeAccountIds'] = {};
    for (const entry of registry.table)
      if (entry.switchable && (entry.id === 'codex' || entry.id === 'antigravity'))
        activeAccountIds[entry.id] =
          dashboard.accounts.find((account) => account.provider === entry.id && account.isActive)
            ?.id ?? null;
    const activeCodexAccountId =
      dashboard.accounts.find((account) => account.provider === 'codex' && account.isActive)?.id ??
      null;
    const answer: AccountAnalytics = {
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
      },
      filters: { platform: query.platform, provider: query.provider, account: query.account },
      history: {
        status: state.available ? 'ok' : 'unavailable',
        retentionDays: stored ? STORE_RETENTION_DAYS : 30,
        collectedSince:
          (stored && Number.isFinite(oldestQuota) ? new Date(oldestQuota).toISOString() : null) ??
          history?.collectedSince ??
          null,
        oldestSampleAt: stamps[0] ?? null,
        newestSampleAt: stamps[stamps.length - 1] ?? null,
        sampleCount,
        message: !state.available
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
      activity,
    };
    this.answers.set(answerKey, { activity: activityResult, answer });
    while (this.answers.size > 32) {
      const oldest = this.answers.keys().next().value;
      if (oldest === undefined) break;
      this.answers.delete(oldest);
    }
    return answer;
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
      const accounts = [...unique.values()];
      const state = await this.record(accounts, scope);
      if (sampleListeners.size > 0) {
        const sampledAt = (this.deps.now ?? Date.now)();
        const history = state.usageStore
          ? await this.historyFor(
              state,
              sampledAt - SNAPSHOT_HISTORY_MS,
              sampledAt,
              sampledAt,
              null
            )
          : state.data;
        const longBurn = state.usageStore
          ? await longBurnFor(state.usageStore, accounts, sampledAt).catch(() => undefined)
          : undefined;
        notifySample({
          accounts,
          history: state.available ? history : null,
          sampledAt,
          ...(longBurn ? { longBurn } : {}),
        });
      }
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
    // Node's Timeout and Bun's Timer are both real timers. One annotation covers the call.
    this.timer = (this.deps.schedule ?? setTimeout)(() => {
      if (!this.running || generation !== this.generation) return;
      this.timer = undefined;
      void this.sample()
        .catch(() => {})
        .finally(() => this.schedule(generation));
    }, interval) as ReturnType<typeof setTimeout>;
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
