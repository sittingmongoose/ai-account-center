import * as fs from 'fs';
import * as path from 'path';
import { Worker } from 'worker_threads';
import { getCcsDir } from '../../utils/config-manager';
import { getDefaultClaudeConfigDir } from '../../utils/claude-config-path';
import { listAccountInstancePaths } from '../../management/instance-directory';
import { CCSError } from '../../errors/error-types';
import { resolveCodexConfigPaths } from './compatible-cli-config-paths';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import {
  accountAnalyticsActivityCoverage,
  projectAccountAnalyticsActivity,
  type SourceData,
} from './account-analytics-projection';
import {
  defaultAccountAnalyticsPricing,
  memoiseAccountAnalyticsPricing,
  type AccountAnalyticsPricingLookup,
} from './account-analytics-pricing';
import type {
  UsageWorkerRequest,
  UsageWorkerResult,
  UsageWorkerResponse,
} from '../usage/worker-client';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsActivityCoverage,
  AccountAnalyticsQuery,
} from './account-analytics-types';

export {
  accountAnalyticsActivityCoverage,
  projectAccountAnalyticsActivity,
  type SourceData,
} from './account-analytics-projection';
export {
  defaultAccountAnalyticsPricing,
  memoiseAccountAnalyticsPricing,
  type AccountAnalyticsPricingLookup,
} from './account-analytics-pricing';
export { detectAccountAnalyticsAnomalies } from './account-analytics-anomalies';

interface ActivityState {
  fetchedAt: number;
  pending: Promise<void> | null;
  generation: number;
  manualRefreshPending: boolean;
  sources: SourceData[];
  partial: boolean;
  /** Rates memoised for the current snapshot; replaced whenever the snapshot is. */
  pricing: AccountAnalyticsPricingLookup;
}
export interface AccountAnalyticsActivityDeps {
  loadWorker?: (request: UsageWorkerRequest) => Promise<UsageWorkerResult>;
  requests?: () => Array<{ provider: 'claude' | 'codex'; request: UsageWorkerRequest }>;
  now?: () => number;
  scope?: () => string;
  responseBudgetMs?: number;
  refreshIntervalSeconds?: () => number;
  pricing?: AccountAnalyticsPricingLookup;
}

/** The public activity plus internal coverage facts the analytics service strips before sending. */
export type AccountAnalyticsActivityResult = AccountAnalyticsActivity & {
  coverage?: AccountAnalyticsActivityCoverage;
};

const MAX_DIRECTORIES = 24;
const MAX_ROWS = 100_000;
const MAX_WORKER_TIME_MS = 20_000;
const MAX_COLLECTION_TIME_MS = 60_000;

/** Unlike legacy merged caches, each worker keeps the native tool dimension. */
export function loadAccountAnalyticsWorker(
  request: UsageWorkerRequest,
  budgetMs = MAX_WORKER_TIME_MS
): Promise<UsageWorkerResult> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(
      path.join(__dirname, '../usage', `native-usage-worker${path.extname(__filename)}`),
      {
        workerData: request,
        env: { ...process.env, CCS_DIR: getCcsDir() },
        resourceLimits: { maxOldGenerationSizeMb: 256, stackSizeMb: 4 },
      }
    );
    let settled = false;
    const finish = (data?: UsageWorkerResult): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      worker.removeAllListeners();
      void worker.terminate().catch(() => {});
      if (
        data &&
        [data.daily, data.hourly, data.monthly, data.session].every(
          (rows) => Array.isArray(rows) && rows.length <= MAX_ROWS
        )
      )
        resolve(data);
      else reject(new CCSError('Local analytics worker could not return bounded usage history'));
    };
    const timer = setTimeout(() => finish(), Math.min(MAX_WORKER_TIME_MS, Math.max(1, budgetMs)));
    worker.once('message', (response: UsageWorkerResponse) =>
      finish(response?.ok === true ? response.data : undefined)
    );
    worker.once('error', () => finish());
    worker.once('exit', () => finish());
  });
}

function localRequests(): Array<{ provider: 'claude' | 'codex'; request: UsageWorkerRequest }> {
  const ccsDir = getCcsDir();
  const activity = { minDate: Date.now() - 31 * 86_400_000, cacheDir: path.join(ccsDir, 'cache') };
  const claudeRoots = [path.join(getDefaultClaudeConfigDir(), 'projects')];
  try {
    for (const instance of listAccountInstancePaths(path.join(ccsDir, 'instances')))
      claudeRoots.push(path.join(instance, 'projects'));
  } catch {
    /* No configured instance history. */
  }
  const unique = new Set<string>();
  const requests: Array<{ provider: 'claude' | 'codex'; request: UsageWorkerRequest }> = [];
  for (const directory of claudeRoots) {
    try {
      if (!fs.statSync(directory).isDirectory()) continue;
      const real = fs.realpathSync(directory);
      if (unique.has(real)) continue;
      unique.add(real);
      requests.push({
        provider: 'claude',
        request: { kind: 'claude', projectsDir: real, activity },
      });
    } catch {
      /* Absent history is not an error or measured zero. */
    }
    if (requests.length >= MAX_DIRECTORIES) break;
  }
  const codexHome = resolveCodexConfigPaths().baseDir;
  if (fs.existsSync(path.join(codexHome, 'sessions')))
    requests.push({
      provider: 'codex',
      request: { kind: 'codex', codexHome, cacheDir: path.join(ccsDir, 'cache'), activity },
    });
  return requests;
}

export class AccountAnalyticsActivityService {
  private readonly states = new Map<string, ActivityState>();
  constructor(private readonly deps: AccountAnalyticsActivityDeps = {}) {}

  private refreshIntervalMs(): number {
    const seconds = (this.deps.refreshIntervalSeconds ?? getAccountRefreshIntervalSeconds)();
    return (Number.isFinite(seconds) ? Math.min(3600, Math.max(30, seconds)) : 60) * 1000;
  }

  private async collect(state: ActivityState, generation: number): Promise<void> {
    const requests = (this.deps.requests ?? localRequests)().slice(0, MAX_DIRECTORIES + 1);
    const collected = new Map<'claude' | 'codex', UsageWorkerResult[]>();
    let failed = false;
    const deadline = Date.now() + MAX_COLLECTION_TIME_MS;
    // Two concurrent workers bound memory while preserving all detected roots.
    for (let index = 0; index < requests.length; index += 2) {
      // A manual refresh supersedes the old read without starting overlapping
      // generations. Let its current bounded batch finish, then scan afresh.
      if (generation !== state.generation) return;
      if (Date.now() >= deadline) {
        failed = true;
        break;
      }
      const batch = requests.slice(index, index + 2);
      const results = await Promise.allSettled(
        batch.map((source) =>
          this.deps.loadWorker
            ? this.deps.loadWorker(source.request)
            : loadAccountAnalyticsWorker(source.request, deadline - Date.now())
        )
      );
      results.forEach((result, position) => {
        if (result.status === 'rejected') {
          failed = true;
          return;
        }
        const provider = batch[position].provider;
        const existing = collected.get(provider) ?? [];
        const cutoff = (this.deps.now ?? Date.now)() - 31 * 86_400_000;
        const data = result.value;
        if (data.scan && !data.scan.complete) failed = true;
        // An unfinished cold scan with no events is not a measured zero.
        if (data.scan && !data.scan.complete && data.eventCount === 0) return;
        // The analytics response only covers 30 days. Keep bounded recent
        // summaries in memory rather than retaining historical project paths.
        const hourly = data.hourly
          .filter((hour) => Date.parse(`${hour.hour.replace(' ', 'T')}:00Z`) >= cutoff)
          .slice(0, 744);
        const recentSessions = data.session.filter(
          (session) => Date.parse(session.lastActivity) >= cutoff
        );
        if (recentSessions.length > 10_000) failed = true;
        existing.push({
          ...data,
          daily: [],
          monthly: [],
          hourly,
          session: recentSessions
            .slice(0, 10_000)
            .map((session) => ({ ...session, projectPath: '' })),
        });
        collected.set(provider, existing);
      });
    }
    if (generation !== state.generation) return;
    const now = (this.deps.now ?? Date.now)();
    const updated: SourceData[] = [];
    for (const provider of ['claude', 'codex'] as const) {
      const data = collected.get(provider);
      if (data) updated.push({ provider, data, fetchedAt: new Date(now).toISOString() });
      else {
        const previous = state.sources.find((source) => source.provider === provider);
        if (previous && requests.some((source) => source.provider === provider))
          updated.push(previous);
      }
    }
    state.sources = updated;
    state.pricing = this.snapshotPricing();
    state.partial = failed || requests.length >= MAX_DIRECTORIES + 1;
    state.fetchedAt = now;
  }

  private snapshotPricing(): AccountAnalyticsPricingLookup {
    return memoiseAccountAnalyticsPricing(this.deps.pricing ?? defaultAccountAnalyticsPricing);
  }

  private startCollection(state: ActivityState): void {
    state.pending = (async () => {
      let generation: number;
      do {
        generation = state.generation;
        try {
          await this.collect(state, generation);
        } catch {
          if (generation === state.generation) {
            state.partial = true;
            state.fetchedAt = (this.deps.now ?? Date.now)();
          }
        }
        // At most one explicit refresh is queued behind an obsolete read.
      } while (generation !== state.generation);
    })().finally(() => {
      state.pending = null;
      state.manualRefreshPending = false;
    });
  }

  async get(
    query: AccountAnalyticsQuery,
    from: number,
    to: number,
    options: { tz?: string } = {}
  ): Promise<AccountAnalyticsActivityResult> {
    const tz = options.tz ?? 'UTC';
    const scope = (this.deps.scope ?? getCcsDir)();
    let state = this.states.get(scope);
    if (!state) {
      state = {
        fetchedAt: -Infinity,
        pending: null,
        generation: 0,
        manualRefreshPending: false,
        sources: [],
        partial: false,
        pricing: this.snapshotPricing(),
      };
      this.states.set(scope, state);
      while (this.states.size > 8) {
        const oldest = this.states.keys().next().value;
        if (oldest === undefined) break;
        this.states.delete(oldest);
      }
    }
    if (
      query.refresh !== true &&
      (query.account !== 'all' ||
        (query.provider !== 'all' && query.provider !== 'claude' && query.provider !== 'codex'))
    )
      return {
        ...projectAccountAnalyticsActivity(
          [],
          query,
          from,
          to,
          'unavailable',
          'Local activity is unavailable for this selection.',
          { tz }
        ),
        // Already-read history still says which providers have activity, so
        // the provider list stays stable while a quota-only filter is chosen.
        coverage: accountAnalyticsActivityCoverage(state.sources, from, to),
      };
    if (query.refresh === true && !state.manualRefreshPending) {
      state.generation++;
      state.manualRefreshPending = true;
      if (!state.pending) this.startCollection(state);
    } else if (
      !state.pending &&
      (this.deps.now ?? Date.now)() - state.fetchedAt >= this.refreshIntervalMs()
    ) {
      state.generation++;
      this.startCollection(state);
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (state.pending) {
      try {
        await Promise.race([
          state.pending,
          new Promise<void>((resolve) => {
            timer = setTimeout(resolve, this.deps.responseBudgetMs ?? 1500);
          }),
        ]);
      } finally {
        clearTimeout(timer);
      }
    }
    const status =
      state.sources.length === 0
        ? state.pending
          ? 'loading'
          : 'unavailable'
        : state.partial || state.pending
          ? 'cached'
          : 'ok';
    const scans = state.sources.flatMap((source) =>
      source.data
        .map((data) => data.scan)
        .filter((scan): scan is NonNullable<UsageWorkerResult['scan']> => scan !== undefined)
    );
    const totalFiles = scans.reduce((sum, scan) => sum + scan.totalFiles, 0);
    const completedFiles = scans.reduce((sum, scan) => sum + scan.completedFiles, 0);
    const unfinishedFiles = scans.reduce((sum, scan) => sum + (scan.unfinishedFiles ?? 0), 0);
    const scanProgress = scans.some((scan) => !scan.complete)
      ? completedFiles + unfinishedFiles === totalFiles && unfinishedFiles > 0
        ? ` All available records have been read; ${unfinishedFiles} local log files end with unfinished records and will be checked for changes.`
        : ` ${completedFiles} of ${totalFiles} local log files have been read; remaining files resume from saved positions on the next check.`
      : '';
    const activity = projectAccountAnalyticsActivity(
      state.sources,
      query,
      from,
      to,
      status,
      status === 'loading'
        ? 'Local CLI history is being read. Quota history is available immediately.'
        : status === 'unavailable'
          ? 'No usable local Claude Code or Codex history is available.'
          : state.pending
            ? 'Local CLI activity is refreshing. Previously read records are shown until the bounded scan completes; cost is an API-equivalent estimate, not a subscription charge.'
            : state.partial
              ? 'The available local records are shown while the bounded history scan continues; some sources may be unavailable. Cost is an API-equivalent estimate, not a subscription charge.' +
                scanProgress
              : `Local Ubuntu CLI activity, across accounts, for UTC hourly buckets starting in this range; days are grouped in ${tz}. Cost is an API-equivalent estimate, not a subscription charge. Usage events are parsed log entries; session counts mean sessions active in this range.`,
      { tz, pricing: state.pricing }
    );
    return { ...activity, coverage: accountAnalyticsActivityCoverage(state.sources, from, to) };
  }
}

let service: AccountAnalyticsActivityService | undefined;
export function getAccountAnalyticsActivity(
  query: AccountAnalyticsQuery,
  from: number,
  to: number,
  options: { tz?: string } = {}
): Promise<AccountAnalyticsActivityResult> {
  service ??= new AccountAnalyticsActivityService();
  return service.get(query, from, to, options);
}
