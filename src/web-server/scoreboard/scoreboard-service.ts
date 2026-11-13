/**
 * Serves the Model Scoreboard view. With the history store (Scoreboard v2) every run is kept
 * forever in aac-store: an ingest pass every two minutes (and on a read when the last is older
 * than the cache) records what the share has new, and a view is built from the store for the
 * requested filters, cached until the runs change. Without the store (no native module,
 * AAC_STORE=off) the share is re-read on demand as before, unfiltered, and the view says so.
 * At server start AAC writes the catalog seeds the share has no file for.
 */
import {
  agentReportsRoot,
  ensureReportsLayout,
  type ReaderFs,
} from '../status/agent-reports-reader';
import {
  DEFAULT_DASHBOARD_TIME_ZONE,
  readDashboardPreferences,
} from '../services/dashboard-preferences';
import { addCalendarDays, localMidnight } from '../services/account-analytics-range';
import { CatalogReader, seedCatalog, type SeedFailure } from './catalog';
import { RunsReader } from './runs-reader';
import { ReadersCheckWriter } from '../status/readers-check';
import {
  buildScoreboardView,
  type ScoreboardFilters,
  type ScoreboardView,
} from './scoreboard-view';
import { composeByModel } from './by-model';
import { openStatusHandle } from '../../status-native/pinned';
import type { NativeStatusHandle } from '../../status-native/native-loader';
import { getAacStore, openAacStore, type AacStore } from '../../store/store-service';
import { RUNS_MORE_MS, RUNS_PASS_MS, RunsStore, type RunsQuery } from './runs-store';

/** A read within this long of the last view gets that view. */
export const SCOREBOARD_CACHE_MS = 60_000;
/** How long a read waits for the share before answering with what it has. */
export const SCOREBOARD_SCAN_WAIT_MS = 8_000;
/** Filtered views kept at once. */
const VIEW_CACHE_ENTRIES = 16;

export const ALL_RUNS: ScoreboardFilters = { from: null, to: null, source: 'all', taskType: null };

export interface ScoreboardServiceDeps {
  root?: () => string;
  now?: () => number;
  timeZone?: () => string;
  fs?: ReaderFs;
  scanWaitMs?: number;
  /** The aac-status handle for the by-model composer; null means no module (tests). */
  native?: NativeStatusHandle | null;
  /** Writes the runs rows of the readers check file; the default follows this service's root. */
  readersCheck?: ReadersCheckWriter;
  /** The history store (the server passes `getAacStore`); absent or null keeps the share-only path. */
  store?: () => AacStore | null;
}

interface Cached {
  at: number;
  generation: number;
  view: ScoreboardView;
}

export class ScoreboardService {
  private readonly catalog: CatalogReader;
  private readonly runs: RunsReader;
  private readonly runsStore: RunsStore;
  /** The aac-status handle for the by-model composer; null when there is no module. */
  private readonly native: NativeStatusHandle | null;
  private readonly cached = new Map<string, Cached>();
  private readonly building = new Map<string, Promise<ScoreboardView>>();
  private seedFailures: SeedFailure[] = [];
  private ingestTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly deps: ScoreboardServiceDeps = {}) {
    const options = { root: () => this.root, now: () => this.now(), fs: deps.fs };
    this.catalog = new CatalogReader(options);
    this.runs = new RunsReader(options);
    this.runsStore = new RunsStore({
      store: () => this.store(),
      root: () => this.root,
      now: () => this.now(),
    });
    if (deps.native !== undefined) {
      this.native = deps.native;
    } else {
      const opened = openStatusHandle();
      this.native = 'unavailable' in opened ? null : opened;
    }
  }

  get root(): string {
    return (this.deps.root ?? agentReportsRoot)();
  }

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private store(): AacStore | null {
    return this.deps.store ? this.deps.store() : null;
  }

  private timeZone(): string {
    if (this.deps.timeZone) return this.deps.timeZone();
    try {
      return readDashboardPreferences().timeZone;
    } catch {
      return DEFAULT_DASHBOARD_TIME_ZONE;
    }
  }

  /** Waits for `work` at most the scan wait; work still running carries on. */
  private async bounded(work: Promise<unknown>): Promise<void> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const waited = new Promise<void>((resolve) => {
      timer = setTimeout(resolve, this.deps.scanWaitMs ?? SCOREBOARD_SCAN_WAIT_MS);
      timer.unref?.();
    });
    await Promise.race([work, waited]);
    if (timer) clearTimeout(timer);
  }

  /** The by-model matrix is the native composer's own words over the view just built. */
  private async finish(view: ScoreboardView): Promise<ScoreboardView> {
    const byModel = this.native ? await composeByModel(view, { handle: this.native }) : null;
    return {
      ...(byModel ? { ...view, byModel } : view),
      cached: true,
      cacheAgeSeconds: 0,
      refreshing: false,
    };
  }

  private recordCheck(runs: Parameters<ReadersCheckWriter['recordRuns']>[0]): void {
    // The check file's runs rows. The Status scan's rows live under the same root, so one file
    // per writer carries both. Fire and forget (F2): a hung share never stalls the build.
    const check = this.deps.readersCheck ?? new ReadersCheckWriter({ root: () => this.root });
    check.recordRuns(runs);
    check.flushSoon();
  }

  /** Share only: every run read, no filters. */
  private async buildFromShare(): Promise<ScoreboardView> {
    await this.bounded(Promise.all([this.catalog.scan(), this.runs.scan()]));
    const runs = this.runs.snapshot();
    this.recordCheck(runs);
    const catalog = this.catalog.snapshot();
    const view = buildScoreboardView({
      now: this.now(),
      timeZone: this.timeZone(),
      root: this.root,
      catalog,
      runs,
      seedFailures: this.seedFailures,
    });
    return this.finish({
      ...view,
      filters: {
        ...ALL_RUNS,
        available: false,
        taskTypes: catalog.taskTypes.map((type) => ({ id: type.id, label: type.label })),
      },
      identity: null,
      history: { store: 'off', runs: null, importing: false },
    });
  }

  /** The store query of a filter: local days in the dashboard's zone. */
  private query(filters: ScoreboardFilters): RunsQuery {
    const zone = this.timeZone();
    return {
      ...(filters.from ? { fromMs: localMidnight(filters.from, zone) } : {}),
      ...(filters.to ? { toMs: localMidnight(addCalendarDays(filters.to, 1), zone) - 1 } : {}),
      ...(filters.source === 'real' ? { source: 'observed' as const } : {}),
      ...(filters.source === 'benchmark' ? { source: 'benchmark' as const } : {}),
      ...(filters.taskType ? { taskType: filters.taskType } : {}),
    };
  }

  /** From the history store, filtered there (Rust); reviews outside the filter still join. */
  private async buildFromStore(filters: ScoreboardFilters): Promise<ScoreboardView> {
    await this.bounded(this.catalog.scan());
    const query = this.query(filters);
    const runs = await this.runsStore.state(query);
    const filtered =
      filters.from !== null ||
      filters.to !== null ||
      filters.source !== 'all' ||
      filters.taskType !== null;
    const joinReviews = filtered
      ? (
          await this.runsStore.query({
            ...(query.fromMs !== undefined ? { fromMs: query.fromMs } : {}),
            reviewsOnly: true,
          })
        ).map((run) => run.doc)
      : [];
    if (!filtered) this.recordCheck(runs);
    const catalog = this.catalog.snapshot();
    const view = buildScoreboardView({
      now: this.now(),
      timeZone: this.timeZone(),
      root: this.root,
      catalog,
      runs,
      joinReviews,
      seedFailures: this.seedFailures,
    });
    const status = await this.store()
      ?.runs<{ runs: number }>('runsStatus')
      .catch(() => null);
    return this.finish({
      ...view,
      filters: {
        ...filters,
        available: true,
        taskTypes: catalog.taskTypes.map((type) => ({ id: type.id, label: type.label })),
      },
      identity: await this.runsStore.identity().catch(() => null),
      history: {
        store: 'on',
        runs: status?.runs ?? null,
        importing: this.runsStore.last?.more ?? false,
      },
    });
  }

  /** One ingest pass now, unless one ran within the cache time. Never rejects. */
  private async freshen(wait: boolean): Promise<void> {
    const last = this.runsStore.last;
    if (last && this.now() - last.at < SCOREBOARD_CACHE_MS) return;
    const pass = this.runsStore.pass().catch(() => null);
    if (wait) await this.bounded(pass);
  }

  /** The view now. Overlapping reads of the same filters share one build. */
  async view(filters: ScoreboardFilters = ALL_RUNS): Promise<ScoreboardView> {
    const storeOn = this.store() !== null;
    const key = storeOn ? JSON.stringify(filters) : 'share';
    if (storeOn) await this.freshen(this.runsStore.last === null);
    const generation = storeOn ? (this.runsStore.last?.generation ?? 0) : 0;
    const hit = this.cached.get(key);
    const now = this.now();
    if (hit && hit.generation === generation && now - hit.at < SCOREBOARD_CACHE_MS)
      return { ...hit.view, cacheAgeSeconds: Math.max(0, now - hit.at) / 1000 };
    let building = this.building.get(key);
    if (!building) {
      // A store that refuses a call (a module without the runs methods, a closed store) leaves
      // the share-only view, which says the filters are unavailable.
      building = (
        storeOn
          ? this.buildFromStore(filters).catch(() => this.buildFromShare())
          : this.buildFromShare()
      )
        .then((view) => {
          this.cached.delete(key);
          this.cached.set(key, { at: this.now(), generation, view });
          while (this.cached.size > VIEW_CACHE_ENTRIES)
            this.cached.delete(this.cached.keys().next().value as string);
          return view;
        })
        .finally(() => this.building.delete(key));
      this.building.set(key, building);
    }
    if (hit) {
      // The last view while a rebuild runs; its sources keep their measured age.
      void building.catch(() => {});
      const age = Math.max(0, now - hit.at) / 1000;
      return {
        ...hit.view,
        cacheAgeSeconds: age,
        refreshing: true,
        sources: hit.view.sources.map((source) => ({
          ...source,
          state: source.state === 'ok' ? 'stale' : source.state,
          ageSeconds: source.ageSeconds === null ? null : source.ageSeconds + age,
          detail: `${source.detail} Cached view; rescan running.`,
        })),
      };
    }
    return building;
  }

  /**
   * Keeps the history current: a pass now, again within a second while a backfill has more,
   * then every two minutes. Timers never hold the process open.
   */
  startIngest(): void {
    if (this.ingestTimer) return;
    const tick = async () => {
      let delay = RUNS_PASS_MS;
      if (this.store()) {
        const pass = await this.runsStore.pass().catch(() => null);
        if (pass?.more) delay = RUNS_MORE_MS;
      }
      this.ingestTimer = setTimeout(() => void tick(), delay);
      this.ingestTimer.unref?.();
    };
    this.ingestTimer = setTimeout(() => void tick(), 0);
    this.ingestTimer.unref?.();
  }

  stopIngest(): void {
    if (this.ingestTimer) clearTimeout(this.ingestTimer);
    this.ingestTimer = null;
  }

  /** The folders and the catalog seeds. Never throws. */
  async seed(): Promise<void> {
    try {
      await ensureReportsLayout(this.root);
      const failures: SeedFailure[] = [];
      await seedCatalog(this.root, undefined, (failure) => failures.push(failure));
      this.seedFailures = failures;
      this.cached.clear();
    } catch {
      /* The catalog Source shows what is missing. */
    }
  }
}

let service: ScoreboardService | null = null;

export function getScoreboardService(): ScoreboardService {
  service ??= new ScoreboardService({ store: getAacStore });
  return service;
}

/** At server start. Never throws and never holds the start up. */
export function startScoreboardService(): void {
  const scoreboard = getScoreboardService();
  void scoreboard.seed();
  // Runs are kept forever once the history store is open; without it the share is read on demand.
  void openAacStore()
    .then((opened) => {
      if (opened.store) scoreboard.startIngest();
    })
    .catch(() => undefined);
}
