import { readNativeCollection, workerReadsNative } from '../native-collection-feed';
import { broadcastDashboardEvent } from '../dashboard-events';
import { privateFileOwners } from '../../shared/private-files';
import { publishSampledDashboard } from '../views/dashboard-feed';
import {
  requestRustStore,
  rustStoreConnection,
  scheduleRustCollection,
} from '../rust-service-transport';
import { getCcsDir } from '../../utils/config-manager';
import { createHash } from 'crypto';
import { BoundedCache } from '../../utils/bounded-cache';
import { getAccountDashboard } from './account-dashboard-service';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { readDashboardPreferences } from './dashboard-preferences';
import { rustAnalyticsWritersOn } from '../rust-service-groups';
import { startAnalyticsWriterFeeds, stopAnalyticsWriterFeeds } from './analytics-writer-feeds';
import type { RustService } from '../rust-service';
import {
  fixedAnalyticsSourceEntries,
  getAccountAnalyticsActivity,
  getAccountAnalyticsActivityLive,
  startAccountAnalyticsActivityBackground,
  stopAccountAnalyticsActivityBackground,
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
  defaultAnalyticsProviderTable,
  type AnalyticsProviderEntry,
} from './account-analytics-providers';
import {
  analyticsAvailableRange,
  composeAccountAnalytics,
  type AccountAnalyticsComposed,
  type AccountAnalyticsComposeInput,
  type AccountAnalyticsRangeInput,
} from './account-analytics-compose';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import type {
  AccountAnalytics,
  AccountAnalyticsActivity,
  AccountAnalyticsQuery,
} from './account-analytics-types';
import {
  accountAnalyticsIdentity,
  analyticsWindowIdentity,
  appendAccountAnalyticsSnapshot,
  FileAccountAnalyticsHistoryStore,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsHistoryStore,
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
import type { AnalyticsOwnedActivity, AnalyticsOwnedFacts } from '../rust-analytics-owned-input';
import { backfillUsageHistory } from '../../store/usage-backfill';
import { AnalyticsSavedViews } from './account-analytics-saved-views';
import { createLogger } from '../../services/logging';

const logger = createLogger('web-server:analytics');

export interface AccountAnalyticsDeps {
  getDashboard?: typeof getAccountDashboard;
  /**
   * The history store's oldest activity hour and quota sample (docs/store.md 12); null without the
   * store. Unset: the store's own answer, or none when `getActivity` is injected (tests).
   */
  storedHistory?: (
    scope: string
  ) => Promise<{ activity: number | null; quota: number | null } | null>;
  getActivity?: (
    query: AccountAnalyticsQuery,
    from: number,
    to: number,
    options?: { tz?: string }
  ) => Promise<AccountAnalyticsActivityResult>;
  /** The owned read's live collector state (R5/A2); absent keeps the TS computation. */
  getActivityLive?: (
    query: AccountAnalyticsQuery,
    from: number,
    to: number
  ) => AnalyticsOwnedActivity;
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
  /**
   * Save each selection's last answer under the scope (`account-analytics/analytics-views-v1.json`) and answer the
   * first read after a restart from it. Default false (tests never touch disk); the live singleton enables it.
   */
  persistViews?: boolean;
  /**
   * How long a read waits for the answer it started before it is answered with the newest saved one (marked `stale`
   * and `refreshing`). A first read with nothing saved waits for its answer. Defaults to 250 ms.
   */
  freshBudgetMs?: number;
  /** Monotonic milliseconds for the slow-read timing log; `performance.now` by default. */
  elapsedMs?: () => number;
}

/** Said with a saved answer when the newest computation of its selection failed (never the error itself). */
export const REFRESH_ERROR = 'The newest numbers could not be computed; the last ones are shown.';
/**
 * Hands one computation's pure composition to another composer (the M3 Rust service, from the
 * guarded route). `legacy` is this file's own composition; the composer answers it or an equal one.
 */
export type AccountAnalyticsComposer = (
  facts: { rangeInput: AccountAnalyticsRangeInput; compose: AccountAnalyticsComposeInput },
  legacy: () => AccountAnalyticsComposed
) => Promise<AccountAnalyticsComposed>;

/**
 * Hands the whole computation to the owned Rust read (R5/A2): it reads the hosted store itself
 * and answers the full answer, activity included, or null to keep this file's computation.
 */
export type AccountAnalyticsOwnedComposer = (
  facts: AnalyticsOwnedFacts
) => Promise<AccountAnalytics | null>;
/** A computation slower than this is logged with its phases (`analytics.read_timing`). */
const SLOW_READ_MS = 1_000;

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

/** An answer computed for this read: its numbers are as of now; `refreshing` follows the log scan. */
function fresh(answer: AccountAnalytics): AccountAnalytics {
  return {
    ...answer,
    asOf: answer.updatedAt,
    stale: false,
    refreshing: answer.activity.refreshing === true || answer.activity.status === 'loading',
  };
}

/** Independent snapshot history keeps quotas separate from billed/token activity. */
export class AccountAnalyticsService {
  private readonly answers = new BoundedCache<{
    key: string;
    activity: AccountAnalyticsActivityResult;
    answer: AccountAnalytics;
  }>(4, 24 * 1024 * 1024);
  private readonly histories = new Map<string, HistoryState>();
  private readonly views = new Map<string, AnalyticsSavedViews>();
  /** The computation running for each scope and selection; reads arriving meanwhile join it. */
  private readonly flights = new Map<string, Promise<AccountAnalytics>>();
  /** How long each selection's last computation took. */
  private readonly computeMs = new Map<string, number>();
  /** How each selection's newest finished computation ended, by its start order. */
  private readonly outcomes = new Map<string, { sequence: number; failed: boolean }>();
  /** Computations in the order they started. */
  private sequence = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private cancelRustTick: (() => void) | undefined;
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
    const hosted =
      privateFileOwners()['account-usage-observations'] === 'rust-service'
        ? rustStoreConnection(scope)
        : null;
    if (hosted) {
      try {
        await requestRustStore(
          hosted,
          'recordDashboard',
          JSON.stringify({
            now,
            accounts: accounts.slice(0, MAX_ACCOUNTS_PER_SAMPLE),
          })
        );
        state.available = true;
        return;
      } catch {
        // record is idempotent if the reply was lost after the store committed.
      }
    }
    const observations = accounts.slice(0, MAX_ACCOUNTS_PER_SAMPLE).flatMap((account) => {
      const observation = storeObservationFromAccount(account, now, Number.POSITIVE_INFINITY);
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

  /**
   * The answer for a selection. A read is answered with the newest numbers there are, at once: it starts (or joins)
   * the computation of a new answer and waits for it at most `freshBudgetMs`; when that is not enough, it is answered
   * with the selection's saved answer, marked `stale` and `refreshing`, and the computation finishes behind it (the
   * page reads again while `refreshing`). Only a selection with nothing saved waits for its first answer. Every
   * answer carries `asOf`, when its numbers were computed.
   */
  async get(
    requested: AccountAnalyticsQuery,
    composer?: AccountAnalyticsComposer,
    owned?: AccountAnalyticsOwnedComposer
  ): Promise<AccountAnalytics> {
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
    const stored = await this.storedBounds(scope, checkedAt);
    resolveAccountAnalyticsRange(
      query,
      checkedAt,
      checkedAt - ACCOUNT_ANALYTICS_RETAINED_MS,
      stored ?? undefined
    );
    const { refresh: _refresh, ...selection } = query;
    const key = JSON.stringify([
      selection.platform,
      selection.range,
      selection.provider,
      selection.account,
      selection.tz,
      selection.from ?? null,
      selection.to ?? null,
    ]);
    const views = this.savedViews(scope);
    const saved = views.get(key);
    const flightKey = JSON.stringify([scope, key]);
    let flight = this.flights.get(flightKey);
    const budget = this.deps.freshBudgetMs ?? 250;
    // Every read computes (or joins the computation running): an unchanged selection is answered from the
    // computation's own memo in milliseconds, and a changed one is never served as current. The computation starts
    // after this turn of the event loop, so an answer served from the saved view is written before its synchronous
    // parts (the snapshot load and the projection) hold the loop.
    if (query.refresh === true || !flight) {
      const elapsed = this.deps.elapsedMs ?? (() => performance.now());
      const started = new Promise<void>((resolve) => setImmediate(resolve)).then(() => {
        const begin = elapsed();
        const sequence = ++this.sequence;
        return this.compute(query, scope, tz, composer, owned).then(
          (answer) => {
            this.computeMs.set(flightKey, elapsed() - begin);
            this.settle(flightKey, sequence, false);
            // kept without the per-read memo fields: a kept answer is described by its own `asOf`
            const { cached: _cached, cacheAgeSeconds: _age, ...kept } = answer;
            views.set(key, kept, sequence);
            return answer;
          },
          (error: unknown) => {
            this.settle(flightKey, sequence, true);
            throw error;
          }
        );
      });
      flight = started.finally(() => {
        if (this.flights.get(flightKey) === flight) this.flights.delete(flightKey);
      });
      this.flights.set(flightKey, flight);
      // a computation nobody waits for any more must not raise an unhandled rejection
      flight.catch(() => undefined);
    }
    if (!saved) return fresh(await (flight as Promise<AccountAnalytics>));
    // A selection whose last computation fit the budget waits for this one that long. One that did not, or that has
    // not been computed since the server started, is answered from the saved view at once.
    const last = this.computeMs.get(flightKey);
    if (last !== undefined && last <= budget) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const answer = await Promise.race([
        flight.then(fresh, () => null),
        new Promise<null>((resolve) => {
          timer = setTimeout(() => resolve(null), budget);
        }),
      ]).finally(() => clearTimeout(timer));
      if (answer) return answer;
    }
    // The newest finished computation failed: the saved answer is older than this read and nothing newer is on its
    // way, so it is never said to be refreshing (the page says it could not update and polls at its normal pace).
    if (this.outcomes.get(flightKey)?.failed === true)
      return {
        ...saved,
        asOf: saved.updatedAt,
        stale: true,
        refreshing: false,
        refreshError: REFRESH_ERROR,
      };
    // The saved answer is current while it is younger than the refresh interval (plus a poll's slack) and was
    // computed by this server; otherwise (or with its time ahead of the clock) it is said to be older than this read,
    // and the page reads again shortly.
    const age = (this.deps.now ?? Date.now)() - Date.parse(saved.updatedAt);
    if (!views.fromDisk(key) && age >= 0 && age < this.currentForMs()) return fresh(saved);
    return { ...saved, asOf: saved.updatedAt, stale: true, refreshing: true };
  }

  private readonly bounds = new Map<string, { at: number; value: { oldest: number } | null }>();
  /**
   * With the history store: the oldest stored usage (activity hour or quota sample), never later
   * than the 31 days the live readers always read. Null without the store. Kept for a minute.
   */
  private async storedBounds(scope: string, now: number): Promise<{ oldest: number } | null> {
    const held = this.bounds.get(scope);
    if (held && now - held.at < 60_000 && now >= held.at) return held.value;
    const read =
      this.deps.storedHistory ??
      (this.deps.getActivity
        ? async () => null
        : async (at: string) => {
            const store = await defaultUsageStore(at);
            if (!store) return null;
            const [activity, quota] = await Promise.all([
              store.activityOldest().catch(() => null),
              store.oldest().catch(() => null),
            ]);
            return { activity, quota };
          });
    let value: { oldest: number } | null = null;
    try {
      const found = await read(scope);
      if (found) {
        const candidates = [
          found.activity,
          found.quota,
          now - ACCOUNT_ANALYTICS_RETAINED_MS,
        ].filter((v): v is number => typeof v === 'number' && Number.isFinite(v));
        value = { oldest: Math.min(...candidates) };
      }
    } catch {
      value = null;
    }
    this.bounds.set(scope, { at: now, value });
    while (this.bounds.size > 8) {
      const oldest = this.bounds.keys().next();
      if (oldest.done) break;
      this.bounds.delete(oldest.value);
    }
    return value;
  }

  /** Records how a selection's computation ended, when it started after the last one recorded. */
  private settle(flightKey: string, sequence: number, failed: boolean): void {
    const held = this.outcomes.get(flightKey);
    if (held && held.sequence > sequence) return;
    this.outcomes.delete(flightKey);
    this.outcomes.set(flightKey, { sequence, failed });
    for (const map of [this.outcomes, this.computeMs])
      while (map.size > 64) {
        const oldest = map.keys().next();
        if (oldest.done) break;
        map.delete(oldest.value);
      }
  }

  /** How long a saved answer counts as current: the refresh interval and 30 s for the page's poll to arrive. */
  private currentForMs(): number {
    const configured =
      this.deps.samplingIntervalMs?.() ?? getAccountRefreshIntervalSeconds() * 1000;
    const interval = Number.isFinite(configured)
      ? Math.min(3_600_000, Math.max(30_000, configured))
      : 60_000;
    return interval + 30_000;
  }

  private savedViews(scope: string): AnalyticsSavedViews {
    let views = this.views.get(scope);
    if (!views) {
      views = new AnalyticsSavedViews(scope, this.deps.persistViews === true, this.deps.now);
      this.views.set(scope, views);
      while (this.views.size > 8) {
        const oldest = this.views.keys().next();
        if (oldest.done) break;
        this.views.get(oldest.value)?.close();
        this.views.delete(oldest.value);
      }
    }
    return views;
  }

  /** Writes every saved answer now (the server is stopping). */
  flushSavedViews(): void {
    for (const views of this.views.values()) views.close(true);
  }

  /** One full computation of a selection's answer (validated, zone resolved). Slow ones are logged by phase. */
  private async compute(
    query: AccountAnalyticsQuery,
    scope: string,
    tz: string,
    composer?: AccountAnalyticsComposer,
    owned?: AccountAnalyticsOwnedComposer
  ): Promise<AccountAnalytics> {
    const clock = this.deps.now ?? Date.now;
    const elapsed = this.deps.elapsedMs ?? (() => performance.now());
    const begin = elapsed();
    const phases: Record<string, number> = {};
    let mark = begin;
    const lap = (name: string): void => {
      const at = elapsed();
      phases[name] = Math.round(at - mark);
      mark = at;
    };
    const dashboard = await (this.deps.getDashboard ?? getAccountDashboard)(query.platform, false);
    lap('dashboardMs');
    // The owned read (R5/A2): Rust reads the hosted store itself and composes the whole answer.
    // It runs before this file's history and activity reads; a decline keeps them, unchanged.
    if (owned && this.deps.getActivityLive) {
      const ownedNow = clock();
      const ownedBounds = await this.storedBounds(scope, ownedNow);
      const ownedRetained = ownedBounds
        ? ownedBounds.oldest
        : ownedNow - ACCOUNT_ANALYTICS_RETAINED_MS;
      const provisional = resolveAccountAnalyticsRange(
        query,
        ownedNow,
        ownedRetained,
        ownedBounds ?? undefined
      );
      const ownedState = this.state(scope);
      const answer = await owned({
        query,
        scope,
        now: ownedNow,
        tz,
        available: ownedState.available,
        bounds: ownedBounds,
        provisional,
        dashboard,
        providerTable: (this.deps.providerTable ?? defaultAnalyticsProviderTable)(),
        activity: this.deps.getActivityLive(query, provisional.from, provisional.to),
      });
      if (answer) return answer;
    }
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
    lap('historyOpenMs');
    const now = clock();
    // With the history store ranges reach back to the oldest stored usage (docs/store.md 12).
    const bounds = await this.storedBounds(scope, now);
    const retainedFrom = bounds ? bounds.oldest : now - ACCOUNT_ANALYTICS_RETAINED_MS;
    // Every range except `all` resolves without the data; `all` reads the
    // whole retained window and then starts at the oldest retained point.
    const provisional = resolveAccountAnalyticsRange(query, now, retainedFrom, bounds ?? undefined);
    const activityResult = await (this.deps.getActivity ?? getAccountAnalyticsActivity)(
      query,
      provisional.from,
      provisional.to,
      { tz }
    ).catch((): AccountAnalyticsActivityResult =>
      unavailableActivity('Local usage history is temporarily unavailable.', tz)
    );
    lap('activityMs');
    const answerKey = createHash('sha256')
      .update(
        JSON.stringify([
          scope,
          query,
          state.revision,
          Math.floor(now / 60_000),
          dashboard.accounts,
          dashboard.providers,
          dashboard.settings,
          (this.deps.providerTable ?? defaultAnalyticsProviderTable)(),
        ])
      )
      .digest('hex');
    const selectionKey = createHash('sha256')
      .update(JSON.stringify([scope, query]))
      .digest('hex');
    const retained = this.answers.get(selectionKey);
    if (retained?.key === answerKey && retained.activity === activityResult)
      return {
        ...retained.answer,
        cached: true,
        cacheAgeSeconds: Math.max(0, now - Date.parse(retained.answer.updatedAt)) / 1000,
      };
    lap('keyMs');
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
    const rangeInput = {
      query,
      now,
      retainedFrom,
      bounds,
      oldestQuota,
      coverageOldestHourAt: coverage?.oldestHourAt,
      provisional,
      todayMidnight: () => localMidnight(localDate(now, tz), tz),
    };
    const { availableFrom, range } = analyticsAvailableRange(rangeInput);
    const { from, to } = range;
    lap('oldestMs');
    // Quota readings are kept for 90 days; a longer range reads only those days of them.
    const quotaFrom = bounds ? Math.max(from, now - STORE_RETENTION_DAYS * 86_400_000) : from;
    const history = await this.historyFor(state, quotaFrom, to, now, range.bucketMinutes * 60_000);
    lap('historyMs');
    const composeInput: AccountAnalyticsComposeInput = {
      query,
      now,
      tz,
      range,
      availableFrom,
      resolution: activity.resolution,
      available: state.available,
      stored: state.usageStore !== null,
      oldestQuota,
      history,
      dashboard,
      providerTable: (this.deps.providerTable ?? defaultAnalyticsProviderTable)(),
      providersWithActivity: coverage?.providersWithActivity ?? [],
    };
    // The guarded route may hand this read to the M3 Rust composer; it never reads a source itself.
    const composed = composer
      ? await composer({ rangeInput, compose: composeInput }, () =>
          composeAccountAnalytics(composeInput)
        )
      : composeAccountAnalytics(composeInput);
    const answer: AccountAnalytics = { ...composed, activity };
    this.answers.set(selectionKey, { key: answerKey, activity: activityResult, answer });
    lap('seriesMs');
    const totalMs = Math.round(elapsed() - begin);
    if (totalMs >= SLOW_READ_MS)
      logger.info('analytics.read_timing', 'Slow analytics computation', {
        totalMs,
        range: query.range,
        filtered: query.provider !== 'all' || query.account !== 'all',
        accounts: dashboard.accounts.length,
        historyRecords: history?.records.length ?? 0,
        hourRows: activity.byHour.length,
        ...phases,
      });
    return answer;
  }

  /** Samples already-bounded dashboard collectors, coalescing both platforms. */
  sample(): Promise<void> {
    if (this.pendingSample) return this.pendingSample;
    const scope = (this.deps.scope ?? getCcsDir)();
    this.pendingSample = (async () => {
      const platforms = ['mac', 'windows'] as const;
      const dashboards = await Promise.allSettled(
        platforms.map((platform) =>
          (this.deps.getDashboard ?? getAccountDashboard)(platform, false)
        )
      );
      if (privateFileOwners()['account-usage-observations'] === 'rust-service') {
        void Promise.allSettled(
          dashboards.map((result, index) =>
            result.status === 'fulfilled'
              ? publishSampledDashboard(scope, platforms[index], result.value)
              : undefined
          )
        );
      }
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
      await this.recordSample([...unique.values()], (this.deps.now ?? Date.now)());
    })().finally(() => {
      this.pendingSample = null;
    });
    return this.pendingSample;
  }

  async recordSample(accounts: DashboardAccount[], sampledAt: number): Promise<void> {
    const scope = (this.deps.scope ?? getCcsDir)();
    const state = await this.record(accounts, scope);
    broadcastDashboardEvent({ type: 'analytics-changed' });
    if (sampleListeners.size > 0) {
      const history = state.usageStore
        ? await this.historyFor(state, sampledAt - SNAPSHOT_HISTORY_MS, sampledAt, sampledAt, null)
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
  }

  private schedule(generation: number): void {
    if (!this.running || generation !== this.generation) return;
    const configured =
      this.deps.samplingIntervalMs?.() ?? getAccountRefreshIntervalSeconds() * 1000;
    const interval = Number.isFinite(configured)
      ? Math.min(3_600_000, Math.max(30_000, configured))
      : 60_000;
    const poll = () => {
      if (!this.running || generation !== this.generation) return;
      this.timer = undefined;
      this.cancelRustTick = undefined;
      void this.sample()
        .catch(() => {})
        .finally(() => this.schedule(generation));
    };
    const scheduleReference = (delay: number) => {
      this.timer = (this.deps.schedule ?? setTimeout)(poll, delay) as ReturnType<typeof setTimeout>;
      this.timer.unref?.();
    };
    if (
      !this.deps.schedule &&
      privateFileOwners()['account-usage-observations'] === 'rust-service'
    ) {
      const began = performance.now();
      this.cancelRustTick = scheduleRustCollection(
        (this.deps.scope ?? getCcsDir)(),
        Math.trunc(interval),
        (due) => {
          this.cancelRustTick = undefined;
          if (!this.running || generation !== this.generation) return;
          if (due) poll();
          else scheduleReference(Math.max(0, interval - (performance.now() - began)));
        }
      );
      if (this.cancelRustTick) return;
    }
    scheduleReference(interval);
  }

  private clearSamplingTimer(): void {
    this.cancelRustTick?.();
    this.cancelRustTick = undefined;
    (this.deps.cancelSchedule ?? clearTimeout)(this.timer);
    this.timer = undefined;
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
    this.clearSamplingTimer();
    this.schedule(++this.generation);
  }

  stop(): void {
    this.running = false;
    ++this.generation;
    this.clearSamplingTimer();
  }
}

let service: AccountAnalyticsService | undefined;
function singleton(): AccountAnalyticsService {
  service ??= new AccountAnalyticsService({
    persistViews: true,
    getActivityLive: getAccountAnalyticsActivityLive,
  });
  return service;
}
export function getAccountAnalytics(
  query: AccountAnalyticsQuery,
  composer?: AccountAnalyticsComposer,
  owned?: AccountAnalyticsOwnedComposer
): Promise<AccountAnalytics> {
  return singleton().get(query, composer, owned);
}
let stopNativeSamples: (() => void) | undefined;

function startNativeSamples(): () => void {
  const stopped = new AbortController();
  let last = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function next(): Promise<void> {
    try {
      const sample = (await readNativeCollection('/sample', stopped.signal)) as {
        sampledAt?: number;
        accounts?: DashboardAccount[];
      };
      if (
        !stopped.signal.aborted &&
        typeof sample.sampledAt === 'number' &&
        Number.isSafeInteger(sample.sampledAt) &&
        sample.sampledAt > last &&
        Array.isArray(sample.accounts)
      ) {
        await singleton().recordSample(sample.accounts, sample.sampledAt);
        last = sample.sampledAt;
      }
    } catch {
      // No sample is available during cold start. Only the native owner can produce one.
    } finally {
      if (!stopped.signal.aborted) {
        timer = setTimeout(() => void next(), 2500);
        timer.unref();
      }
    }
  }
  void next();
  return () => {
    stopped.abort();
    clearTimeout(timer);
  };
}

export function startAccountAnalyticsSampling(
  rustService?: RustService,
  nativeCollection = false
): void {
  // G14: with the writers group on, the Rust service samples quota and collects activity; the
  // front door feeds it the inputs and stands its own writers down so nothing writes twice.
  if (rustService?.mode === 'on' && rustAnalyticsWritersOn()) {
    startAnalyticsWriterFeeds(rustService, nativeCollection);
    return;
  }
  if (!nativeCollection && !workerReadsNative()) singleton().start();
  if (
    process.platform === 'win32' &&
    nativeCollection &&
    workerReadsNative() &&
    process.env.AAC_CUT_PUBLIC_COLLECTION === '1'
  ) {
    stopNativeSamples ??= startNativeSamples();
  }
  // Usage history is collected in the background too, so it grows while no page is open.
  startAccountAnalyticsActivityBackground();
}
export function stopAccountAnalyticsSampling(): void {
  stopNativeSamples?.();
  stopNativeSamples = undefined;
  stopAnalyticsWriterFeeds();
  stopAccountAnalyticsActivityBackground();
  service?.stop();
  service?.flushSavedViews();
}
export function rescheduleAccountAnalyticsSampling(): void {
  service?.reschedule();
}
