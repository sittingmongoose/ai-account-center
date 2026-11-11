/**
 * Serves the Model Scoreboard view. Runs and the catalog are rescanned on demand: a read older than
 * the short cache starts a rescan (stat first, read only changed files) and waits for it a bounded
 * time, so a hung share answers with the last good data and a Source that turns stale. Nothing runs
 * while nobody is reading. At server start AAC writes the catalog seeds the share has no file for.
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
import { CatalogReader, seedCatalog, type SeedFailure } from './catalog';
import { RunsReader } from './runs-reader';
import { ReadersCheckWriter } from '../status/readers-check';
import { buildScoreboardView, type ScoreboardView } from './scoreboard-view';
import { composeByModel } from './by-model';
import { openStatusHandle } from '../../status-native/pinned';
import type { NativeStatusHandle } from '../../status-native/native-loader';

/** A read within this long of the last view gets that view. */
export const SCOREBOARD_CACHE_MS = 10_000;
/** How long a read waits for the share before answering with what it has. */
export const SCOREBOARD_SCAN_WAIT_MS = 8_000;

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
}

export class ScoreboardService {
  private readonly catalog: CatalogReader;
  private readonly runs: RunsReader;
  /** The aac-status handle for the by-model composer; null when there is no module. */
  private readonly native: NativeStatusHandle | null;
  private cached: { at: number; view: ScoreboardView } | null = null;
  private building: Promise<ScoreboardView> | null = null;
  private seedFailures: SeedFailure[] = [];

  constructor(private readonly deps: ScoreboardServiceDeps = {}) {
    const options = { root: () => this.root, now: () => this.now(), fs: deps.fs };
    this.catalog = new CatalogReader(options);
    this.runs = new RunsReader(options);
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

  private timeZone(): string {
    if (this.deps.timeZone) return this.deps.timeZone();
    try {
      return readDashboardPreferences().timeZone;
    } catch {
      return DEFAULT_DASHBOARD_TIME_ZONE;
    }
  }

  /** Scans within the wait; a scan still running carries on and the next read picks it up. */
  private async scan(): Promise<void> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const waited = new Promise<void>((resolve) => {
      timer = setTimeout(resolve, this.deps.scanWaitMs ?? SCOREBOARD_SCAN_WAIT_MS);
      timer.unref?.();
    });
    await Promise.race([Promise.all([this.catalog.scan(), this.runs.scan()]), waited]);
    if (timer) clearTimeout(timer);
    // The check file's runs rows, from the same snapshot the view builds on. The Status
    // scan's rows live under the same root, so one file per writer carries both.
    const check = this.deps.readersCheck ?? new ReadersCheckWriter({ root: () => this.root });
    check.recordRuns(this.runs.snapshot());
    await check.flush().catch(() => undefined);
  }

  private async build(): Promise<ScoreboardView> {
    await this.scan();
    const now = this.now();
    const view = buildScoreboardView({
      now,
      timeZone: this.timeZone(),
      root: this.root,
      catalog: this.catalog.snapshot(),
      runs: this.runs.snapshot(),
      seedFailures: this.seedFailures,
    });
    // The by-model matrix is the native composer's own words over the view just built;
    // when it cannot be had (no module, a refused input) the view carries no field.
    const byModel = this.native ? await composeByModel(view, { handle: this.native }) : null;
    const withByModel: ScoreboardView = byModel ? { ...view, byModel } : view;
    this.cached = { at: now, view: withByModel };
    return withByModel;
  }

  /** The view now, rescanning when the last one is older than the cache. Overlapping reads share one. */
  view(): Promise<ScoreboardView> {
    if (this.cached && this.now() - this.cached.at < SCOREBOARD_CACHE_MS)
      return Promise.resolve(this.cached.view);
    this.building ??= this.build().finally(() => {
      this.building = null;
    });
    return this.building;
  }

  /** The folders and the catalog seeds. Never throws. */
  async seed(): Promise<void> {
    try {
      await ensureReportsLayout(this.root);
      const failures: SeedFailure[] = [];
      await seedCatalog(this.root, undefined, (failure) => failures.push(failure));
      this.seedFailures = failures;
      this.cached = null;
    } catch {
      /* The catalog Source shows what is missing. */
    }
  }
}

let service: ScoreboardService | null = null;

export function getScoreboardService(): ScoreboardService {
  service ??= new ScoreboardService();
  return service;
}

/** At server start. Never throws and never holds the start up. */
export function startScoreboardService(): void {
  void getScoreboardService().seed();
}
