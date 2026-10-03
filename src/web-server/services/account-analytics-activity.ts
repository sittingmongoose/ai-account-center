import * as fs from 'fs';
import * as path from 'path';
import { Worker } from 'worker_threads';
import { getCcsDir } from '../../utils/config-manager';
import { getDefaultClaudeConfigDir } from '../../utils/claude-config-path';
import { listAccountInstancePaths } from '../../management/instance-directory';
import { CCSError } from '../../errors/error-types';
import { resolveCodexConfigPaths } from './compatible-cli-config-paths';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { readDashboardPreferences, type UsageLogSource } from './dashboard-preferences';
import {
  accountAnalyticsActivityCoverage,
  projectAccountAnalyticsActivity,
  type SourceData,
} from './account-analytics-projection';
import { resolveOmpSessionRoots } from '../usage/omp-native-usage-collector';
import { resolveMuseSessionsDir } from '../usage/muse-native-usage-collector';
import { resolveZcodeDbPath } from '../usage/zcode-native-usage-collector';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
  type AnalyticsRemoteSourceState,
} from './analytics-remote-sources';
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
  AccountAnalyticsActivityProvider,
  AccountAnalyticsQuery,
  AccountAnalyticsSource,
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
  /** Per-tool, per-host collection states for the current snapshot. */
  sourceStates: AccountAnalyticsSource[];
  /** The last remote answer, carried forward while a later remote scan has not answered. */
  remote: RemoteAnswer | null;
  /** Rates memoised for the current snapshot; replaced whenever the snapshot is. */
  pricing: AccountAnalyticsPricingLookup;
  /** Recent cost of projecting this snapshot, reserved out of the response budget. */
  projectionMs: number;
}

export type AccountAnalyticsActivityRequest = {
  provider: AccountAnalyticsActivityProvider;
  request: UsageWorkerRequest;
};

type RemoteAnswer = {
  results: Array<{ tool: 'omp' | 'muse' | 'zcode'; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
};

/**
 * Fixed entries for tools with no local usage log, on every host. Antigravity
 * keeps token counts only inside sqlite protobuf BLOBs mixed with conversation
 * content; Cursor usage is server-side. The page uses these to say why they
 * are missing. Muse and zcode are not installed on Windows and are skipped.
 */
export function fixedAnalyticsSourceEntries(): AccountAnalyticsSource[] {
  const entries: AccountAnalyticsSource[] = [];
  for (const host of ['ubuntu', 'mac', 'windows'] as const) {
    entries.push({
      tool: 'antigravity',
      host,
      state: 'unavailable',
      lastScanAt: null,
      rowCount: 0,
      detail:
        'no local usage log: token counts exist only inside sqlite protobuf BLOBs mixed with conversation content',
    });
    entries.push({
      tool: 'cursor',
      host,
      state: 'unavailable',
      lastScanAt: null,
      rowCount: 0,
      detail:
        'no local usage log: usage is server-side; the local state database has no token-usage columns',
    });
  }
  for (const tool of ['muse', 'zcode'] as const) {
    entries.push({
      tool,
      host: 'windows',
      state: 'not_installed',
      lastScanAt: null,
      rowCount: 0,
      detail: 'not installed on this host',
    });
  }
  return entries;
}

export interface AccountAnalyticsActivityDeps {
  loadWorker?: (request: UsageWorkerRequest) => Promise<UsageWorkerResult>;
  requests?: () => AccountAnalyticsActivityRequest[] | Promise<AccountAnalyticsActivityRequest[]>;
  remote?: (minDateMs: number) => Promise<RemoteAnswer>;
  /**
   * Saved remote aggregates, read without contacting a host, for a remote scan
   * that has not answered in time and no earlier answer in memory. Defaults
   * to the on-disk remote cache when `remote` is the default.
   */
  remoteCached?: (minDateMs: number) => RemoteAnswer | null;
  now?: () => number;
  scope?: () => string;
  /**
   * End-to-end time for one activity answer: waiting for a running collection
   * plus projecting the snapshot. The wait gives up what recent projections
   * of this snapshot cost. Defaults to 1.5 s.
   */
  responseBudgetMs?: number;
  /** Monotonic milliseconds for measuring projections; `performance.now` by default. */
  elapsedMs?: () => number;
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

async function isDirectory(directory: string): Promise<boolean> {
  try {
    return (await fs.promises.stat(directory)).isDirectory();
  } catch {
    return false;
  }
}

/**
 * The local log roots to read. The OMP marker scan awaits every directory
 * read, so it never blocks the server's event loop; it runs once per
 * collection, and the tools it returns are also what says which are installed.
 */
async function localRequests(): Promise<AccountAnalyticsActivityRequest[]> {
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
  const seen = (kind: string, resolved: string): boolean => {
    const key = `${kind}:${resolved}`;
    if (unique.has(key)) return true;
    unique.add(key);
    return false;
  };
  const requests: AccountAnalyticsActivityRequest[] = [];
  for (const directory of claudeRoots) {
    try {
      if (!fs.statSync(directory).isDirectory()) continue;
      const real = fs.realpathSync(directory);
      if (seen('claude', real)) continue;
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
  if (fs.existsSync(path.join(codexHome, 'sessions'))) {
    try {
      seen('codex', fs.realpathSync(path.join(codexHome, 'sessions')));
    } catch {
      /* Realpath failure still scans under the configured home. */
    }
    requests.push({
      provider: 'codex',
      request: { kind: 'codex', codexHome, cacheDir: path.join(ccsDir, 'cache'), activity },
    });
  }
  try {
    const ompRoots: string[] = [];
    for (const root of await resolveOmpSessionRoots({ cacheDir: path.join(ccsDir, 'cache') }))
      if (await isDirectory(root)) ompRoots.push(root);
    if (ompRoots.length) {
      for (const root of ompRoots) {
        try {
          seen('omp', fs.realpathSync(root));
        } catch {
          /* Realpath failure still scans under the resolved root. */
        }
      }
      requests.push({ provider: 'omp', request: { kind: 'omp', roots: ompRoots, activity } });
    }
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  try {
    const sessionsDir = resolveMuseSessionsDir();
    if (fs.statSync(sessionsDir).isDirectory()) {
      try {
        seen('muse', fs.realpathSync(sessionsDir));
      } catch {
        /* Realpath failure still scans under the resolved directory. */
      }
      requests.push({ provider: 'muse', request: { kind: 'muse', sessionsDir, activity } });
    }
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  try {
    const dbPath = resolveZcodeDbPath();
    if (fs.statSync(dbPath).isFile()) {
      try {
        seen('zcode', fs.realpathSync(dbPath));
      } catch {
        /* Realpath failure still scans the resolved database. */
      }
      requests.push({ provider: 'zcode', request: { kind: 'zcode', dbPath, activity } });
    }
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  try {
    const prefs = readDashboardPreferences(ccsDir);
    requests.push(...extraActivityRequests(prefs.usageLogSources, activity, ccsDir, unique));
  } catch {
    /* Without prefs the built-in roots still scan. */
  }
  return requests;
}

/**
 * Saved extra usage-log locations for this host (Ubuntu) as worker requests.
 * The built-in roots stay the defaults; each extra adds one request. An extra
 * that resolves to a root already scanned (built-in or an earlier extra) is
 * skipped, so the same logs are never counted twice; two generic JSONL
 * sources may share a root only with different field mappings. Unreadable or
 * unmapped extras are skipped, never a failed collection.
 */
export function extraActivityRequests(
  sources: UsageLogSource[],
  activity: { minDate: number; cacheDir: string },
  ccsDir: string,
  scanned: Set<string>
): AccountAnalyticsActivityRequest[] {
  const requests: AccountAnalyticsActivityRequest[] = [];
  const claim = (kind: string, resolved: string, mapping = ''): boolean => {
    const key = `${kind}:${resolved}${mapping}`;
    if (scanned.has(key)) return false;
    scanned.add(key);
    return true;
  };
  for (const source of sources) {
    if (requests.length >= MAX_DIRECTORIES) break;
    if (!source || source.host !== 'ubuntu') continue;
    try {
      if (source.tool === 'claude-code') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('claude', real)) continue;
        requests.push({
          provider: 'claude',
          request: { kind: 'claude', projectsDir: real, activity },
        });
      } else if (source.tool === 'codex') {
        if (!fs.existsSync(path.join(source.path, 'sessions'))) continue;
        const real = fs.realpathSync(path.join(source.path, 'sessions'));
        if (!claim('codex', real)) continue;
        requests.push({
          provider: 'codex',
          request: {
            kind: 'codex',
            codexHome: source.path,
            cacheDir: path.join(ccsDir, 'cache'),
            activity,
          },
        });
      } else if (source.tool === 'omp') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('omp', real)) continue;
        requests.push({ provider: 'omp', request: { kind: 'omp', roots: [real], activity } });
      } else if (source.tool === 'muse') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('muse', real)) continue;
        requests.push({ provider: 'muse', request: { kind: 'muse', sessionsDir: real, activity } });
      } else if (source.tool === 'zcode') {
        if (!fs.statSync(source.path).isFile()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('zcode', real)) continue;
        requests.push({ provider: 'zcode', request: { kind: 'zcode', dbPath: real, activity } });
      } else if (source.tool === 'jsonl') {
        const fieldMapping = source.fieldMapping;
        if (!fieldMapping || typeof fieldMapping.timestamp !== 'string') continue;
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        const mapping = {
          timestamp: fieldMapping.timestamp,
          ...(typeof fieldMapping.model === 'string' ? { model: fieldMapping.model } : {}),
          ...(typeof fieldMapping.inputTokens === 'string'
            ? { inputTokens: fieldMapping.inputTokens }
            : {}),
          ...(typeof fieldMapping.outputTokens === 'string'
            ? { outputTokens: fieldMapping.outputTokens }
            : {}),
          ...(typeof fieldMapping.cost === 'string' ? { cost: fieldMapping.cost } : {}),
        };
        if (!claim('jsonl', real, `\n${JSON.stringify(mapping)}`)) continue;
        requests.push({
          provider: 'jsonl',
          request: { kind: 'jsonl', roots: [real], mapping, activity },
        });
      }
    } catch {
      /* An unreadable extra is skipped, never a failed collection. */
    }
  }
  return requests;
}

/**
 * Whether each tool's Ubuntu logs exist, for `not_installed` states: the
 * tools that have a local request (a Claude Code projects folder, Codex
 * sessions, an OMP session root, Muse sessions, the zcode database, a generic
 * JSONL root). Generic JSONL has no built-in root: presence means a saved
 * `jsonl` extra for this host resolved to a readable directory.
 */
function localSourcePresence(
  requests: AccountAnalyticsActivityRequest[]
): Record<AccountAnalyticsActivityProvider, boolean> {
  const presence: Record<AccountAnalyticsActivityProvider, boolean> = {
    claude: false,
    codex: false,
    omp: false,
    muse: false,
    zcode: false,
    jsonl: false,
  };
  for (const entry of requests) presence[entry.provider] = true;
  return presence;
}

export class AccountAnalyticsActivityService {
  private readonly states = new Map<string, ActivityState>();
  constructor(private readonly deps: AccountAnalyticsActivityDeps = {}) {}

  private refreshIntervalMs(): number {
    const seconds = (this.deps.refreshIntervalSeconds ?? getAccountRefreshIntervalSeconds)();
    return (Number.isFinite(seconds) ? Math.min(3600, Math.max(30, seconds)) : 60) * 1000;
  }

  private async collect(state: ActivityState, generation: number): Promise<void> {
    const requests = (await (this.deps.requests ?? localRequests)()).slice(0, MAX_DIRECTORIES + 1);
    const presence = this.deps.requests === undefined ? localSourcePresence(requests) : null;
    const collected = new Map<AccountAnalyticsActivityProvider, UsageWorkerResult[]>();
    let failed = false;
    const deadline = Date.now() + MAX_COLLECTION_TIME_MS;
    const cutoff = (this.deps.now ?? Date.now)() - 31 * 86_400_000;
    // Remote scans run alongside the local workers; a timeout or failure only
    // marks those sources, never the whole collection.
    const remotePromise = (this.deps.remote ?? loadAnalyticsRemoteSources)(cutoff).catch(
      () => null
    );
    const succeeded = new Set<AccountAnalyticsActivityProvider>();
    const attempted = new Set<AccountAnalyticsActivityProvider>();
    const localEvents = new Map<AccountAnalyticsActivityProvider, number>();
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
        const provider = batch[position].provider;
        attempted.add(provider);
        if (result.status === 'rejected') {
          failed = true;
          return;
        }
        const existing = collected.get(provider) ?? [];
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
        succeeded.add(provider);
        localEvents.set(provider, (localEvents.get(provider) ?? 0) + data.eventCount);
      });
    }
    if (generation !== state.generation) return;
    const remaining = deadline - Date.now();
    let remote: Awaited<typeof remotePromise> = null;
    if (remaining > 0) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        remote = await Promise.race([
          remotePromise,
          new Promise<null>((resolve) => {
            timer = setTimeout(() => resolve(null), remaining);
          }),
        ]);
      } finally {
        // A lingering deadline must never hold the caller's event loop open.
        clearTimeout(timer);
      }
    }
    // A remote scan that has not answered in time keeps the remote part of
    // the totals: the last answer in memory, else the saved remote cache.
    let remoteStates: AnalyticsRemoteSourceState[] | null = null;
    let remoteAnswer: RemoteAnswer | null = remote;
    if (remote) {
      state.remote = remote;
      remoteStates = remote.states;
    } else {
      failed = true;
      if (state.remote) remoteAnswer = state.remote;
      else {
        const cached =
          this.deps.remoteCached ??
          (this.deps.remote === undefined ? loadAnalyticsRemoteCachedSources : undefined);
        try {
          remoteAnswer = cached ? cached(cutoff) : null;
        } catch {
          remoteAnswer = null;
        }
        if (remoteAnswer) remoteStates = remoteAnswer.states;
      }
    }
    if (remoteAnswer) {
      for (const entry of remoteAnswer.results) {
        const existing = collected.get(entry.tool) ?? [];
        const data = entry.data;
        const hourly = data.hourly
          .filter((hour) => Date.parse(`${hour.hour.replace(' ', 'T')}:00Z`) >= cutoff)
          .slice(0, 744);
        if (hourly.length || data.eventCount === 0) {
          existing.push({ ...data, daily: [], monthly: [], hourly, session: [] });
          collected.set(entry.tool, existing);
        }
      }
    }
    if (generation !== state.generation) return;
    const now = (this.deps.now ?? Date.now)();
    const fetchedAt = new Date(now).toISOString();
    const updated: SourceData[] = [];
    for (const provider of ['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl'] as const) {
      const data = collected.get(provider);
      if (data) updated.push({ provider, data, fetchedAt });
      else {
        const previous = state.sources.find((source) => source.provider === provider);
        if (previous && requests.some((source) => source.provider === provider))
          updated.push(previous);
      }
    }
    state.sources = updated;
    state.sourceStates = this.buildSourceStates(
      state,
      succeeded,
      attempted,
      remoteStates,
      localEvents,
      fetchedAt,
      presence
    );
    state.pricing = this.snapshotPricing();
    state.partial = failed || requests.length >= MAX_DIRECTORIES + 1;
    state.fetchedAt = now;
  }

  private buildSourceStates(
    state: ActivityState,
    succeeded: Set<AccountAnalyticsActivityProvider>,
    attempted: Set<AccountAnalyticsActivityProvider>,
    remote: AnalyticsRemoteSourceState[] | null,
    localEvents: Map<AccountAnalyticsActivityProvider, number>,
    fetchedAt: string,
    presence: Record<AccountAnalyticsActivityProvider, boolean> | null
  ): AccountAnalyticsSource[] {
    const entries: AccountAnalyticsSource[] = [];
    const previous = new Map(
      state.sourceStates.map((entry) => [`${entry.tool}\0${entry.host}`, entry])
    );
    for (const tool of ['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl'] as const) {
      const key = `${tool}\0ubuntu`;
      const old = previous.get(key);
      if (succeeded.has(tool)) {
        entries.push({
          tool,
          host: 'ubuntu',
          state: 'ok',
          lastScanAt: fetchedAt,
          rowCount: localEvents.get(tool) ?? 0,
          detail: null,
        });
      } else if (old && attempted.has(tool)) {
        entries.push({ ...old, state: old.rowCount > 0 ? 'cached' : 'unavailable' });
      } else if (presence && !presence[tool]) {
        entries.push({
          tool,
          host: 'ubuntu',
          state: 'not_installed',
          lastScanAt: old?.lastScanAt ?? null,
          rowCount: 0,
          detail: 'no usage logs found on this host',
        });
      } else if (old) {
        entries.push({ ...old, state: old.rowCount > 0 ? 'cached' : old.state });
      } else {
        entries.push({
          tool,
          host: 'ubuntu',
          state: attempted.has(tool) ? 'unavailable' : 'not_installed',
          lastScanAt: null,
          rowCount: 0,
          detail: attempted.has(tool) ? 'local scan failed' : 'no usage logs found on this host',
        });
      }
    }
    if (remote) {
      for (const entry of remote) entries.push({ ...entry });
    } else {
      // The remote scans never answered; the previous remote aggregates are
      // still in the totals, and are marked.
      for (const tool of ['omp', 'muse', 'zcode'] as const) {
        for (const host of ['mac', 'windows'] as const) {
          if (tool !== 'omp' && host === 'windows') continue;
          const old = previous.get(`${tool}\0${host}`);
          if (old)
            entries.push(
              old.rowCount > 0
                ? {
                    ...old,
                    state: 'cached',
                    detail: 'remote scan timed out; showing previously read aggregates',
                  }
                : old
            );
          else
            entries.push({
              tool,
              host,
              state: 'unavailable',
              lastScanAt: null,
              rowCount: 0,
              detail: 'remote scan timed out',
            });
        }
      }
    }
    entries.push(...fixedAnalyticsSourceEntries());
    const order = (tool: string): number =>
      ['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl', 'antigravity', 'cursor'].indexOf(tool);
    const hostOrder = (host: string): number => ['ubuntu', 'mac', 'windows'].indexOf(host);
    entries.sort((a, b) => order(a.tool) - order(b.tool) || hostOrder(a.host) - hostOrder(b.host));
    return entries;
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
        sourceStates: [],
        remote: null,
        pricing: this.snapshotPricing(),
        projectionMs: 0,
      };
      this.states.set(scope, state);
      while (this.states.size > 8) {
        const oldest = this.states.keys().next().value;
        if (oldest === undefined) break;
        this.states.delete(oldest);
      }
    }
    // Dashboard quota-provider ids overlap the activity tools only for
    // claude/codex/muse; omp/zcode filters can only arrive via `all`.
    const activityFilters: readonly string[] = ['all', 'claude', 'codex', 'omp', 'muse', 'zcode'];
    if (
      query.refresh !== true &&
      (query.account !== 'all' || !activityFilters.includes(query.provider))
    )
      return {
        ...projectAccountAnalyticsActivity(
          [],
          query,
          from,
          to,
          'unavailable',
          'Local activity is unavailable for this selection.',
          { tz, sources: state.sourceStates }
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
    const budget = this.deps.responseBudgetMs ?? 1500;
    const elapsed = this.deps.elapsedMs ?? (() => performance.now());
    const wait = Math.max(0, budget - state.projectionMs);
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (state.pending) {
      try {
        await Promise.race([
          state.pending,
          new Promise<void>((resolve) => {
            timer = setTimeout(resolve, wait);
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
    const projectionStart = elapsed();
    const activity = projectAccountAnalyticsActivity(
      state.sources,
      query,
      from,
      to,
      status,
      status === 'loading'
        ? 'CLI history is being read. Quota history is available immediately.'
        : status === 'unavailable'
          ? 'No usable Claude Code, Codex, OMP, Muse or zcode history is available.'
          : state.pending
            ? 'CLI activity is refreshing. Previously read records are shown until the bounded scan completes; cost is an API-equivalent estimate, not a subscription charge.'
            : state.partial
              ? 'The available records are shown while the bounded history scan continues; some sources may be unavailable. Cost is an API-equivalent estimate, not a subscription charge.' +
                scanProgress
              : `CLI activity from Ubuntu, Mac and Windows (Claude Code, Codex, OMP, Muse and zcode), across accounts, for UTC hourly buckets starting in this range; days are grouped in ${tz}. Cost is an API-equivalent estimate, not a subscription charge. Usage events are parsed log entries; session counts mean sessions active in this range.`,
      { tz, pricing: state.pricing, sources: state.sourceStates }
    );
    const coverage = accountAnalyticsActivityCoverage(state.sources, from, to);
    // Keep the dearest recent projection (halving older ones), capped at the
    // budget, so a cheap 24h answer does not starve the next 31-day one.
    state.projectionMs = Math.min(
      budget,
      Math.max(elapsed() - projectionStart, state.projectionMs / 2)
    );
    return { ...activity, coverage };
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
