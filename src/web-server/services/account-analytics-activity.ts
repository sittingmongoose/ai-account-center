import * as fs from 'fs';
import * as path from 'path';
import { createHash } from 'crypto';
import { Worker } from 'worker_threads';
import { getCcsDir } from '../../utils/config-manager';
import { getClaudeProjectsDirForAnalytics } from '../../utils/claude-config-path';
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
import { USAGE_PROVIDER_ORDER } from './account-analytics-attribution';
import { resolveOmpSessionRoots } from '../usage/omp-native-usage-collector';
import { resolveMuseSessionsDir } from '../usage/muse-native-usage-collector';
import { resolveZcodeDbPath } from '../usage/zcode-native-usage-collector';
import { antigravityUsagePresent } from '../usage/antigravity-native-usage-collector';
import {
  EXPERIMENT_ROOTS_TTL_MS,
  emptyRootSet,
  readExperimentRoots,
  type ExperimentRootSet,
  type ExperimentRootsView,
} from '../usage/experiment-usage-roots';
import { resolveT3UsageRoots, type T3UsageRoots } from '../usage/t3-usage-roots';
import { collectorConcurrency, runBounded } from '../usage/collector-concurrency';
import { startModelsDevRegistryRefresh } from '../models-dev/registry-cache';
import {
  analyticsRemoteTargets,
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
  type AnalyticsRemoteSourceState,
  type AnalyticsSourceTool,
} from './analytics-remote-sources';
import { REMOTE_ANALYTICS_HOSTS, type AnalyticsRemoteHost } from './analytics-remote-transport';
import { DASHBOARD_HOSTS } from './dashboard-hosts';
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
  local: Map<string, SourceData>;
  identities: Map<string, LocalRequestIdentity>;
  requests: AccountAnalyticsActivityRequest[];
  attempts: Map<string, number>;
  attemptSequence: number;
  fetchedAt: number;
  /** A restored snapshot starts a local refresh even when its timestamp is recent. */
  needsLocalRefresh: boolean;
  pending: Promise<void> | null;
  generation: number;
  manualRefreshPending: boolean;
  sources: SourceData[];
  partial: boolean;
  /** Per-tool, per-host collection states for the current snapshot. */
  sourceStates: AccountAnalyticsSource[];
  /** The last remote answer, carried forward while a later remote scan has not answered. */
  remote: RemoteAnswer | null;
  /** Remote hosts the current generation's scan is still waiting on. */
  remotePending: AnalyticsRemoteHost[];
  /** When the published grid first showed a still-scanning cell; null once settled. Bounds the converge cadence. */
  scanningSince: number | null;
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
  /** Re-elect mirrors against the inventory at publication, after local workers settle. */
  reconcileClaudeMirrors?: () => void;
  fetchedAt?: string;
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
};

interface LocalRequestIdentity {
  group: string;
  roots: string[];
}

/** Hash root membership separately so expanded requests can replace retained predecessors. */
function localRequestIdentity(source: AccountAnalyticsActivityRequest): LocalRequestIdentity {
  const request = { ...source.request } as Record<string, unknown>;
  delete request.activity;
  delete request.cacheDir;
  const roots: string[] = [];
  for (const field of [
    'roots',
    'experimentRoots',
    'homeRoots',
    'experimentDbs',
    'referenceRoots',
  ]) {
    const values = request[field];
    if (Array.isArray(values))
      for (const value of values)
        roots.push(
          createHash('sha256')
            .update(JSON.stringify([field, value]))
            .digest('hex')
        );
    delete request[field];
  }
  return {
    group: createHash('sha256')
      .update(JSON.stringify([source.provider, request]))
      .digest('hex'),
    roots,
  };
}

/** Request identity excludes moving scan windows and cache locations. Paths never persist. */
function localRequestKey(source: AccountAnalyticsActivityRequest): string {
  const {
    activity: _activity,
    cacheDir: _cacheDir,
    ...request
  } = source.request as UsageWorkerRequest & {
    activity?: unknown;
    cacheDir?: string;
  };
  return createHash('sha256')
    .update(JSON.stringify([source.provider, request]))
    .digest('hex');
}

function newerRemote(left: RemoteAnswer | null, right: RemoteAnswer | null): RemoteAnswer | null {
  const time = (answer: RemoteAnswer | null) =>
    Math.max(
      Date.parse(answer?.fetchedAt ?? '') || 0,
      ...(answer?.states.map((entry) => Date.parse(entry.lastScanAt ?? '') || 0) ?? [])
    );
  // A cache-only read re-elects mirrors; JSON snapshots cannot retain that callback.
  return right && time(right) >= time(left) ? right : left;
}

/**
 * Fixed entries for tools with no local usage log, on every host. Cursor usage
 * is server-side (the local chat store holds blobs and metadata only, with no
 * token-usage columns). The page uses these to say why it is missing. Every
 * other tool is scanned on every host, so its state is measured, never fixed;
 * Antigravity's usage metadata is read from its conversation databases by the
 * analytics helper (antigravity-native-usage-collector.ts).
 */
export function fixedAnalyticsSourceEntries(): AccountAnalyticsSource[] {
  const entries: AccountAnalyticsSource[] = [];
  for (const host of DASHBOARD_HOSTS) {
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
  return entries;
}

/** The grid's order: tools in reading order, hosts Ubuntu, Mac, Windows then Nas1. */
const SOURCE_TOOL_ORDER = [
  'claude',
  'codex',
  'omp',
  'muse',
  'zcode',
  'jsonl',
  'antigravity',
  'cursor',
];
const SOURCE_HOST_ORDER: readonly string[] = DASHBOARD_HOSTS;
function sortSourceEntries(entries: AccountAnalyticsSource[]): AccountAnalyticsSource[] {
  return entries.sort(
    (a, b) =>
      SOURCE_TOOL_ORDER.indexOf(a.tool) - SOURCE_TOOL_ORDER.indexOf(b.tool) ||
      SOURCE_HOST_ORDER.indexOf(a.host) - SOURCE_HOST_ORDER.indexOf(b.host)
  );
}

/**
 * The grid of a cold `loading` answer, before the first publish: every tool on
 * every host is being read by the first collection, which is already running.
 * The measured states replace it seconds later, at the first publish.
 */
export function coldScanningSourceStates(): AccountAnalyticsSource[] {
  const entries: AccountAnalyticsSource[] = [];
  for (const host of DASHBOARD_HOSTS) {
    const tools =
      host === 'ubuntu'
        ? (['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'] as const)
        : analyticsRemoteTargets(host);
    for (const tool of tools)
      entries.push({
        tool,
        host,
        state: 'scanning',
        lastScanAt: null,
        rowCount: 0,
        detail: 'the first scan is running',
      });
  }
  entries.push(...fixedAnalyticsSourceEntries());
  return sortSourceEntries(entries);
}

export interface AccountAnalyticsActivityDeps {
  /** Reads one request; `budgetMs` is what is left of the collection's deadline. */
  loadWorker?: (request: UsageWorkerRequest, budgetMs?: number) => Promise<UsageWorkerResult>;
  /** Local collectors that may run at once; sized to the machine by default (collectorConcurrency). */
  concurrency?: number;
  /**
   * Keep the model price list current, off the request path: called as each collection starts. The
   * live service refreshes the models.dev registry when its cached copy is a day old, so a model
   * released after the last refresh gets a listed rate instead of reading as not logged. Tests and
   * ad-hoc readers leave it unset and never touch the network.
   */
  refreshPricing?: () => unknown;
  requests?: () => AccountAnalyticsActivityRequest[] | Promise<AccountAnalyticsActivityRequest[]>;
  remote?: (
    minDateMs: number,
    opts?: {
      onHostScan?: (host: AnalyticsRemoteHost, phase: 'start' | 'done') => void;
    }
  ) => Promise<RemoteAnswer>;
  /**
   * Saved remote aggregates, read without contacting a host, for a remote scan
   * that has not answered in time, and to rebuild mirror selection when the first
   * local pass after a restart publishes.
   * Equal-time cache answers supersede serialized answers. Defaults to the on-disk
   * remote cache when `remote` is the default.
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
  /**
   * Persist the published snapshot under the scope's cache dir and serve it after a restart.
   * Default false (tests and ad-hoc readers never touch disk); the live singleton enables it.
   */
  persistSnapshot?: boolean;
}

/** The public activity plus internal coverage facts the analytics service strips before sending. */
export type AccountAnalyticsActivityResult = AccountAnalyticsActivity & {
  coverage?: AccountAnalyticsActivityCoverage;
};

const MAX_DIRECTORIES = 24;
const MAX_ROWS = 100_000;
const MAX_WORKER_TIME_MS = 20_000;
const MAX_COLLECTION_TIME_MS = 60_000;
/** Retained sessions per published source; more than this marks the local collection incomplete. */
const MAX_RETAINED_SESSIONS = 10_000;
/**
 * Cold-start convergence: while the published grid still holds a scanning
 * cell, the next automatic collection starts after this shorter interval
 * instead of the configured refresh interval - for CONVERGE_WINDOW_MS after
 * the first such cell appeared at most, so a host whose logs stay too big to
 * finish falls back to the normal cadence instead of scanning forever.
 */
const CONVERGE_INTERVAL_MS = 20_000;
const CONVERGE_WINDOW_MS = 600_000;

/**
 * The sessions one published source keeps: inside the 31-day window, bounded, and without a
 * project path (a session's directory never reaches the page). Local and remote sources keep the
 * same shape, so Session stats covers every host that reported usage.
 */
function retainedSessions(
  sessions: UsageWorkerResult['session'],
  cutoff: number
): UsageWorkerResult['session'] {
  return sessions
    .filter((session) => Date.parse(session.lastActivity) >= cutoff)
    .slice(0, MAX_RETAINED_SESSIONS)
    .map((session) => ({ ...session, projectPath: '' }));
}

/**
 * The on-disk snapshot: the last published aggregate rows, served instantly after a restart while
 * the first collection runs. Hourly and session rows with project paths stripped (enforced on
 * load); a session row carries only its published key, never a raw id, a path or content.
 * Best-effort: a missing or invalid file behaves like a first run.
 * 2: readers key a session where its log was read, so a version 1 snapshot of raw ids is never
 * served again. 3: local request aggregates are retained separately from remote rows, keyed by
 * a digest of the request's source locations, so a sibling result never hides a failed request.
 * Optional root-membership digests let v3 snapshots retain superseded requests until collected.
 */
const SNAPSHOT_VERSION = 3;
const SNAPSHOT_FILE = 'analytics-activity-snapshot-v1.json';
const MAX_SNAPSHOT_BYTES = 8 * 1024 * 1024;
const MAX_SNAPSHOT_SESSIONS = 10_000;

interface PersistedActivitySnapshot {
  local: Array<[string, SourceData]>;
  identities?: Array<[string, LocalRequestIdentity]>;
  version: typeof SNAPSHOT_VERSION;
  fetchedAt: number;
  partial: boolean;
  sources: SourceData[];
  sourceStates: AccountAnalyticsSource[];
  remote: RemoteAnswer | null;
}

function snapshotFile(scope: string): string | null {
  // Test and ad-hoc scopes are not directories; only persist under an existing absolute scope.
  if (!path.isAbsolute(scope)) return null;
  try {
    if (!fs.statSync(scope).isDirectory()) return null;
  } catch {
    return null;
  }
  return path.join(scope, 'cache', 'account-activity-v1', SNAPSHOT_FILE);
}

function validSnapshot(value: unknown): value is PersistedActivitySnapshot {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const snap = value as Record<string, unknown>;
  if (snap.version !== SNAPSHOT_VERSION) return false;
  if (typeof snap.fetchedAt !== 'number' || !Number.isFinite(snap.fetchedAt)) return false;
  if (typeof snap.partial !== 'boolean') return false;
  if (!Array.isArray(snap.sources) || snap.sources.length > MAX_DIRECTORIES + 1) return false;
  if (!Array.isArray(snap.local) || snap.local.length > 1024) return false;
  if (
    snap.identities !== undefined &&
    (!Array.isArray(snap.identities) ||
      snap.identities.length > 1024 ||
      snap.identities.some(
        (entry) =>
          !Array.isArray(entry) ||
          !/^[a-f0-9]{64}$/.test(entry[0]) ||
          !entry[1] ||
          !/^[a-f0-9]{64}$/.test(entry[1].group) ||
          !Array.isArray(entry[1].roots) ||
          entry[1].roots.some(
            (root: unknown) => typeof root !== 'string' || !/^[a-f0-9]{64}$/.test(root)
          )
      ))
  )
    return false;
  if (!Array.isArray(snap.sourceStates) || snap.sourceStates.length > 64) return false;
  const local = snap.local as Array<[string, Record<string, unknown>]>;
  if (local.some((entry) => !Array.isArray(entry) || !/^[a-f0-9]{64}$/.test(entry[0])))
    return false;
  for (const source of [...snap.sources, ...local.map((entry) => entry[1])] as Array<
    Record<string, unknown>
  >) {
    if (!source || typeof source !== 'object') return false;
    if (
      !['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl', 'antigravity'].includes(
        source.provider as string
      )
    )
      return false;
    if (typeof source.fetchedAt !== 'string') return false;
    if (!Array.isArray(source.data)) return false;
    for (const result of source.data as Array<Record<string, unknown>>) {
      if (!result || typeof result !== 'object') return false;
      if (!Array.isArray(result.hourly) || result.hourly.length > 744) return false;
      if (!Array.isArray(result.session) || result.session.length > MAX_SNAPSHOT_SESSIONS)
        return false;
      // Persisted rows never carry project paths (stripped before publish, enforced again here).
      for (const row of [...(result.hourly as unknown[]), ...(result.session as unknown[])]) {
        const candidate = (row ?? {}) as Record<string, unknown>;
        if ('projectPath' in candidate && candidate.projectPath !== '') return false;
      }
    }
  }
  return true;
}

function loadPersistedSnapshot(scope: string): PersistedActivitySnapshot | null {
  try {
    const file = snapshotFile(scope);
    if (!file || fs.statSync(file).size > MAX_SNAPSHOT_BYTES) return null;
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as unknown;
    return validSnapshot(value) ? value : null;
  } catch {
    return null;
  }
}

function savePersistedSnapshot(scope: string, state: ActivityState): void {
  try {
    const file = snapshotFile(scope);
    if (!file) return;
    const body = JSON.stringify({
      version: SNAPSHOT_VERSION,
      fetchedAt: state.fetchedAt,
      partial: state.partial,
      sources: state.sources,
      sourceStates: state.sourceStates,
      remote: state.remote
        ? {
            ...state.remote,
            results: state.remote.results.map((entry) => ({
              ...entry,
              data: {
                ...entry.data,
                daily: [],
                monthly: [],
                session: retainedSessions(entry.data.session, state.fetchedAt - 31 * 86_400_000),
              },
            })),
          }
        : null,
      local: [...state.local],
      identities: [...state.identities].filter(([key]) => state.local.has(key)),
    } satisfies PersistedActivitySnapshot);
    if (Buffer.byteLength(body, 'utf8') > MAX_SNAPSHOT_BYTES) return;
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temporary, body, { mode: 0o600 });
    fs.renameSync(temporary, file);
    fs.chmodSync(file, 0o600);
  } catch {
    /* Snapshot writes are best-effort; the rows are still served. */
  }
}

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
      const bounded =
        !!data &&
        [data.daily, data.hourly, data.monthly, data.session].every(
          (rows) => Array.isArray(rows) && rows.length <= MAX_ROWS
        );
      // Settle only once the thread has stopped: a worker cut off by its time bound must never
      // still be writing a checkpoint when the next collection reads the same files.
      void worker
        .terminate()
        .catch(() => 0)
        .then(() => {
          if (bounded && data) resolve(data);
          else
            reject(new CCSError('Local analytics worker could not return bounded usage history'));
        });
    };
    // The collector stops reading at 12 s; leave time to aggregate and return its checkpoints.
    const timer = setTimeout(
      () => finish(),
      Math.min(MAX_WORKER_TIME_MS, Math.max(15_000, budgetMs))
    );
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
 * The local log roots to read, in two parts so the collectors start at once instead of waiting
 * for the slowest discovery step. `ready` holds the roots a few stats find (Claude Code, Codex,
 * Muse Code, zcode, Antigravity). `later` resolves once the OMP marker scan has run (it awaits every directory
 * read, so it never blocks the event loop, and it is bounded at 30 s) together with the saved
 * extras, which must know every built-in root so the same logs are never read twice. Together
 * they also say which tools are installed. `later` never rejects.
 */
function localRequestPlan(): {
  ready: AccountAnalyticsActivityRequest[];
  later: Promise<AccountAnalyticsActivityRequest[]>;
} {
  const ccsDir = getCcsDir();
  const activity = { minDate: Date.now() - 31 * 86_400_000, cacheDir: path.join(ccsDir, 'cache') };
  const claudeRoots = [getClaudeProjectsDirForAnalytics()];
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
  // Antigravity (and T3 Code's Antigravity instances): the helper finds and reads the databases; a few
  // stats here only say whether any of its roots exists.
  try {
    if (antigravityUsagePresent())
      requests.push({ provider: 'antigravity', request: { kind: 'antigravity', activity } });
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  // Experiment roots: what the background walk has found so far (a small cached file), read now so
  // these collectors start with the rest; the walk itself advances one slice in the background.
  // Saved extra sources count as read roots too: experiment roots inside them are left to them,
  // and Claude experiment copies are also checked against saved Claude sources.
  const readNow = new Set(unique);
  try {
    for (const source of readDashboardPreferences(ccsDir).usageLogSources) {
      if (!source || source.host !== 'ubuntu') continue;
      try {
        if (source.tool === 'claude-code' && fs.statSync(source.path).isDirectory())
          readNow.add(`claude:${fs.realpathSync(source.path)}`);
        else if (source.tool === 'codex' && fs.existsSync(path.join(source.path, 'sessions')))
          readNow.add(`codex:${fs.realpathSync(path.join(source.path, 'sessions'))}`);
        else if (source.tool === 'muse' && fs.statSync(source.path).isDirectory())
          readNow.add(`muse:${fs.realpathSync(source.path)}`);
        else if (source.tool === 'zcode' && fs.statSync(source.path).isFile())
          readNow.add(`zcode:${fs.realpathSync(source.path)}`);
      } catch {
        /* An unreadable saved source is skipped here as it is below. */
      }
    }
  } catch {
    /* Without prefs only the built-in roots count as read. */
  }
  let experimentView: ExperimentRootsView | null = null;
  try {
    experimentView = readExperimentRoots(activity.cacheDir);
  } catch {
    /* Without experiment roots the default and T3 roots still scan. */
  }
  // T3 Code account homes (a few directory reads): counted with the experiment roots, against the
  // same default roots, so a transcript or shadow-home link already read counts once.
  let t3: T3UsageRoots = { claude: [], codex: [] };
  try {
    t3 = resolveT3UsageRoots();
  } catch {
    /* Without T3 homes the other roots still scan. */
  }
  try {
    requests.push(
      ...experimentActivityRequests(
        experimentView?.roots ?? emptyRootSet(),
        {
          projectsDir: claudeRoots[0],
          codexHome,
          sessionsDir: resolveMuseSessionsDir(),
          dbPath: resolveZcodeDbPath(),
        },
        activity,
        path.join(ccsDir, 'cache'),
        readNow,
        t3,
        experimentView
      )
    );
  } catch {
    /* Without experiment roots the default roots still scan. */
  }
  if (experimentView) startExperimentRootsSlice(activity.cacheDir, experimentView);
  const later = (async (): Promise<AccountAnalyticsActivityRequest[]> => {
    const discovered: AccountAnalyticsActivityRequest[] = [];
    try {
      const ompRoots: string[] = [];
      const discovery = { truncated: false };
      for (const root of await resolveOmpSessionRoots({
        cacheDir: path.join(ccsDir, 'cache'),
        discovery,
      }))
        if (await isDirectory(root)) ompRoots.push(root);
      if (ompRoots.length || discovery.truncated) {
        for (const root of ompRoots) {
          try {
            seen('omp', fs.realpathSync(root));
          } catch {
            /* Realpath failure still scans under the resolved root. */
          }
        }
        discovered.push({
          provider: 'omp',
          request: {
            kind: 'omp',
            roots: ompRoots,
            activity: { ...activity, discoveryTruncated: discovery.truncated },
          },
        });
      }
    } catch {
      /* Absent history is not an error or measured zero. */
    }
    try {
      const prefs = readDashboardPreferences(ccsDir);
      discovered.push(...extraActivityRequests(prefs.usageLogSources, activity, ccsDir, unique));
    } catch {
      /* Without prefs the built-in roots still scan. */
    }
    return discovered;
  })();
  return { ready: requests, later };
}

let experimentSliceRunning = false;

/**
 * One background slice of the experiment-root walk (experiment-usage-roots.ts) on a collector
 * worker thread at low priority, at most one at a time per process. Never awaited by a
 * collection: the roots it finds are read from the next collection on, so the walk never delays
 * a collector or touches the event loop.
 */
function startExperimentRootsSlice(cacheDir: string, view: ExperimentRootsView): void {
  if (experimentSliceRunning) return;
  // A completed round younger than its TTL needs no slice: no worker is spawned.
  if (!view.scanning && Date.now() - view.completedAt <= EXPERIMENT_ROOTS_TTL_MS) return;
  experimentSliceRunning = true;
  void loadAccountAnalyticsWorker({ kind: 'experiment-roots', cacheDir })
    .catch(() => undefined)
    .finally(() => {
      experimentSliceRunning = false;
    });
}

/**
 * Requests for the experiment roots found so far, one per tool, each reading only its experiment
 * roots and deduplicating against the tool's default roots (collectExperimentActivity). `scanned`
 * holds `<kind>:<real path>` for every root the other requests read (built-in, instances, saved
 * sources). An experiment root that no longer exists, or that resolves to (or inside) a root of
 * its tool already read, is left out; every Claude root read is the Claude request's reference.
 * T3 Code homes (`t3`, t3-usage-roots.ts) ride in the same Claude and Codex requests as
 * `homeRoots`, under the same rule: a Codex shadow home whose `sessions` links into `~/.codex`
 * resolves inside the default root and is left out, so it is never read twice.
 */
export function experimentActivityRequests(
  found: ExperimentRootSet,
  defaults: { projectsDir: string; codexHome: string; sessionsDir: string; dbPath: string },
  activity: { minDate: number; cacheDir: string },
  cacheDir: string,
  scanned: Set<string>,
  t3: T3UsageRoots = { claude: [], codex: [] },
  discovery: ExperimentRootsView | null = null
): AccountAnalyticsActivityRequest[] {
  const readRoots = (kind: string): string[] =>
    [...scanned]
      .filter((key) => key.startsWith(`${kind}:`))
      .map((key) => key.slice(kind.length + 1));
  const pick = (kind: string, list: string[], wantFile = false): string[] => {
    const read = readRoots(kind);
    // The same folder reached another way (a bind mount) is a root already read, too.
    const readIdentities = new Set<string>();
    for (const root of read) {
      try {
        const stats = fs.statSync(root);
        readIdentities.add(`${stats.dev}:${stats.ino}`);
      } catch {
        /* A read root that is gone has no identity. */
      }
    }
    const kept = new Set<string>();
    for (const candidate of list) {
      try {
        const stats = fs.statSync(candidate);
        if (wantFile ? !stats.isFile() : !stats.isDirectory()) continue;
        if (readIdentities.has(`${stats.dev}:${stats.ino}`)) continue;
        const real = fs.realpathSync(candidate);
        const inside = read.some(
          (root) =>
            real === root || real.startsWith(root.endsWith(path.sep) ? root : root + path.sep)
        );
        if (!inside) kept.add(real);
      } catch {
        /* A root that is gone is skipped. */
      }
    }
    return [...kept];
  };
  const requests: AccountAnalyticsActivityRequest[] = [];
  const activityFor = (kind: keyof ExperimentRootSet) => ({
    ...activity,
    discoveryTruncated:
      !!discovery &&
      (discovery.truncated ||
        discovery.capped.includes(kind) ||
        (discovery.scanning && !discovery.completedAt)),
  });
  const claude = pick('claude', found.claude);
  const claudeHomes = pick('claude', t3.claude).filter((root) => !claude.includes(root));
  let projectsDir = defaults.projectsDir;
  try {
    projectsDir = fs.realpathSync(projectsDir);
  } catch {
    /* A missing default folder still names the request's cache. */
  }
  if (claude.length || claudeHomes.length || activityFor('claude').discoveryTruncated)
    requests.push({
      provider: 'claude',
      request: {
        kind: 'claude',
        projectsDir,
        experimentRoots: claude,
        ...(claudeHomes.length ? { homeRoots: claudeHomes } : {}),
        referenceRoots: [...new Set([projectsDir, ...readRoots('claude')])],
        activity: activityFor('claude'),
      },
    });
  const codex = pick('codex', found.codex);
  const codexHomes = pick('codex', t3.codex).filter((root) => !codex.includes(root));
  if (codex.length || codexHomes.length || activityFor('codex').discoveryTruncated)
    requests.push({
      provider: 'codex',
      request: {
        kind: 'codex',
        codexHome: defaults.codexHome,
        cacheDir,
        experimentRoots: codex,
        ...(codexHomes.length ? { homeRoots: codexHomes } : {}),
        activity: activityFor('codex'),
      },
    });
  const muse = pick('muse', found.muse);
  if (muse.length || activityFor('muse').discoveryTruncated)
    requests.push({
      provider: 'muse',
      request: {
        kind: 'muse',
        sessionsDir: defaults.sessionsDir,
        experimentRoots: muse,
        activity: activityFor('muse'),
      },
    });
  const zcode = pick('zcode', found.zcode, true);
  if (zcode.length || activityFor('zcode').discoveryTruncated)
    requests.push({
      provider: 'zcode',
      request: {
        kind: 'zcode',
        dbPath: defaults.dbPath,
        experimentDbs: zcode,
        activity: activityFor('zcode'),
      },
    });
  return requests;
}

/** Injected requests (tests, embedders) in the same two parts: a list is ready at once. */
function injectedRequestPlan(requests: NonNullable<AccountAnalyticsActivityDeps['requests']>): {
  ready: AccountAnalyticsActivityRequest[];
  later: Promise<AccountAnalyticsActivityRequest[]>;
} {
  try {
    const listed = requests();
    return Array.isArray(listed)
      ? { ready: listed, later: Promise.resolve([]) }
      : { ready: [], later: Promise.resolve(listed) };
  } catch (error) {
    return { ready: [], later: Promise.reject(error) };
  }
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
  let excludeRoots: string[] = [];
  const claim = (kind: string, resolved: string, mapping = ''): boolean => {
    const key = `${kind}:${resolved}${mapping}`;
    excludeRoots = [];
    const inside = (child: string, parent: string) =>
      child === parent || child.startsWith(parent.endsWith(path.sep) ? parent : parent + path.sep);
    for (const existing of scanned) {
      const colon = existing.indexOf(':');
      const tool = existing.slice(0, colon);
      const [root, priorMapping = ''] = existing.slice(colon + 1).split('\n');
      if (tool !== kind && kind !== 'jsonl' && tool !== 'jsonl') continue;
      if (tool === 'jsonl' && kind === 'jsonl' && `\n${priorMapping}` !== mapping) continue;
      if (inside(resolved, root)) return false;
      if (inside(root, resolved)) excludeRoots.push(root);
    }
    scanned.add(key);
    return true;
  };
  for (const source of sources) {
    if (!source || source.host !== 'ubuntu') continue;
    try {
      if (source.tool === 'claude-code') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('claude', real)) continue;
        requests.push({
          provider: 'claude',
          request: { kind: 'claude', projectsDir: real, activity: { ...activity, excludeRoots } },
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
            activity: { ...activity, excludeRoots },
          },
        });
      } else if (source.tool === 'omp') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('omp', real)) continue;
        requests.push({
          provider: 'omp',
          request: { kind: 'omp', roots: [real], activity: { ...activity, excludeRoots } },
        });
      } else if (source.tool === 'muse') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('muse', real)) continue;
        requests.push({
          provider: 'muse',
          request: { kind: 'muse', sessionsDir: real, activity: { ...activity, excludeRoots } },
        });
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
          request: {
            kind: 'jsonl',
            roots: [real],
            mapping,
            activity: { ...activity, excludeRoots },
          },
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
 * JSONL root, an Antigravity store). Generic JSONL has no built-in root: presence means a saved
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
    antigravity: false,
  };
  for (const entry of requests) presence[entry.provider] = true;
  return presence;
}

export class AccountAnalyticsActivityService {
  private readonly states = new Map<string, ActivityState>();
  constructor(private readonly deps: AccountAnalyticsActivityDeps = {}) {}

  private cachedRemote(cutoff: number, scope: string): RemoteAnswer | null {
    if (this.deps.remoteCached) return this.deps.remoteCached(cutoff);
    return this.deps.remote === undefined
      ? loadAnalyticsRemoteCachedSources(cutoff, { cacheDir: path.join(scope, 'cache') })
      : null;
  }

  private refreshIntervalMs(): number {
    const seconds = (this.deps.refreshIntervalSeconds ?? getAccountRefreshIntervalSeconds)();
    return (Number.isFinite(seconds) ? Math.min(3600, Math.max(30, seconds)) : 60) * 1000;
  }

  private async collect(state: ActivityState, generation: number): Promise<void> {
    const deadline = Date.now() + MAX_COLLECTION_TIME_MS;
    const cutoff = (this.deps.now ?? Date.now)() - 31 * 86_400_000;
    const hadClaude = [...state.local.values()].some((source) => source.provider === 'claude');
    const previousRemote = state.remote;
    try {
      void this.deps.refreshPricing?.();
    } catch {
      /* A price-list refresh never holds up or fails a collection. */
    }
    // Remote scans start first and run alongside the local workers; a timeout or failure only
    // marks those sources, never the whole collection. The page names the
    // hosts a refresh is waiting on, so a slow Windows scan reads as
    // "Refreshing Mac and Windows…" instead of a stuck spinner.
    const onHostScan = (host: AnalyticsRemoteHost, phase: 'start' | 'done'): void => {
      if (generation !== state.generation) return;
      state.remotePending =
        phase === 'start'
          ? state.remotePending.includes(host)
            ? state.remotePending
            : [...state.remotePending, host]
          : state.remotePending.filter((pending) => pending !== host);
    };
    state.remotePending = [];
    const loadRemote =
      this.deps.remote ??
      ((ms: number, opts?: { onHostScan?: typeof onHostScan }) =>
        loadAnalyticsRemoteSources(ms, opts ?? {}));
    const remotePromise = loadRemote(cutoff, { onHostScan }).catch(() => null);
    let settledRemote: RemoteAnswer | null = null;
    void remotePromise.then((answer) => {
      settledRemote = answer;
    });
    const collected = new Map<AccountAnalyticsActivityProvider, UsageWorkerResult[]>();
    let failed = false;
    // The roots a few stats find start reading at once; the OMP marker scan's roots join the queue
    // when that scan finishes, without holding the others back.
    const plan = this.deps.requests ? injectedRequestPlan(this.deps.requests) : localRequestPlan();
    let discoveryFailed = false;
    const later = plan.later.catch((): AccountAnalyticsActivityRequest[] => {
      failed = true;
      discoveryFailed = true;
      return [];
    });
    // Every collector runs at once up to a bound sized to the machine; each starts the moment a
    // slot frees, so a slow one never holds back the next. Each keeps its own scan budget, and
    // results are read back in request order, however the workers finish.
    const settled: Array<PromiseSettledResult<UsageWorkerResult> | undefined> = [];
    const seenRequests = new Set<string>();
    const uniqueRequests = (entries: readonly AccountAnalyticsActivityRequest[]) =>
      entries.filter((entry) => {
        const key = localRequestKey(entry);
        if (seenRequests.has(key)) return false;
        seenRequests.add(key);
        return true;
      });
    const { items: requests, skipped } = await runBounded(
      state.requests.some(
        (previous) =>
          !plan.ready.some((entry) => localRequestKey(entry) === localRequestKey(previous))
      )
        ? [later.then((entries) => uniqueRequests([...plan.ready, ...entries]))]
        : [uniqueRequests(plan.ready), later.then(uniqueRequests)],
      this.deps.concurrency ?? collectorConcurrency(),
      async (source, index) => {
        const budget = Math.max(15_000, Math.min(MAX_WORKER_TIME_MS, deadline - Date.now()));
        state.attempts.set(localRequestKey(source), ++state.attemptSequence);
        try {
          settled[index] = {
            status: 'fulfilled',
            value: await (this.deps.loadWorker
              ? this.deps.loadWorker(source.request, budget)
              : loadAccountAnalyticsWorker(source.request, budget)),
          };
        } catch (reason) {
          settled[index] = { status: 'rejected', reason };
        }
      },
      {
        // A manual refresh supersedes the old read without starting overlapping generations:
        // running workers finish, queued ones never start, then the new generation scans afresh.
        stop: () => generation !== state.generation || Date.now() >= deadline,
        maxItems: MAX_DIRECTORIES + 1,
        priority: (source) => state.attempts.get(localRequestKey(source)) ?? 0,
      }
    );
    // Roots the deadline left unread make the collection partial.
    if (generation !== state.generation) return;
    if (skipped > 0) failed = true;
    for (const source of requests)
      state.identities.set(localRequestKey(source), localRequestIdentity(source));
    const replacements = new Map<string, string[]>();
    const predecessors = new Map<string, SourceData>();
    for (const [key, prior] of state.local) {
      if (seenRequests.has(key)) continue;
      const identity = state.identities.get(key);
      for (const source of requests) {
        const nextKey = localRequestKey(source);
        const next = state.identities.get(nextKey);
        if (!next || source.provider !== prior.provider || state.local.has(nextKey)) continue;
        // Legacy v3 snapshots have no signatures. Only new identities can replace
        // their unplanned share; successful unchanged siblings cannot retire it.
        if (
          identity &&
          (identity.group !== next.group ||
            !identity.roots.every((root) => next.roots.includes(root)))
        )
          continue;
        replacements.set(key, [...(replacements.get(key) ?? []), nextKey]);
        predecessors.set(nextKey, prior);
      }
    }
    state.requests = requests;
    // runBounded returns every enumerated request, including unstarted ones.
    // A worker cap does not make that inventory incomplete.
    const discoveryComplete = !discoveryFailed;
    const presence = this.deps.requests === undefined ? localSourcePresence(requests) : null;
    const succeeded = new Set<AccountAnalyticsActivityProvider>();
    const unfinished = new Map<
      AccountAnalyticsActivityProvider,
      'partial' | 'skipped' | 'failed'
    >();
    const attempted = new Set<AccountAnalyticsActivityProvider>();
    const localEvents = new Map<AccountAnalyticsActivityProvider, number>();
    const localReadAt = new Map<AccountAnalyticsActivityProvider, string>();
    const replacedLocal = new Set<string>();
    // Stage replacements until we know whether their Claude mirror election can
    // advance. A successful sibling must not mutate the saved contribution early.
    const nextLocal = new Map(state.local);
    requests.forEach((source, index) => {
      const key = localRequestKey(source);
      const result = settled[index];
      const provider = source.provider;
      if (!result) {
        if (!unfinished.has(provider)) unfinished.set(provider, 'skipped');
        return;
      }
      attempted.add(provider);
      if (result.status === 'rejected') {
        unfinished.set(provider, 'failed');
        failed = true;
        return;
      }
      const data = result.value;
      const prior = state.local.get(key) ?? predecessors.get(key);
      const authoritativeEmpty =
        data.scan?.complete === true &&
        data.scan.failedFiles === 0 &&
        !('activity' in source.request && source.request.activity?.discoveryTruncated);
      if (
        (data.scan && !data.scan.complete) ||
        ('activity' in source.request && source.request.activity?.discoveryTruncated)
      ) {
        failed = true;
        if ((data.scan?.failedFiles ?? 0) > 0) unfinished.set(provider, 'failed');
        else if (unfinished.get(provider) !== 'failed') unfinished.set(provider, 'partial');
      }
      if (
        data.eventCount === 0 &&
        !authoritativeEmpty &&
        prior?.data.some((entry) => entry.eventCount > 0)
      ) {
        failed = true;
        if (unfinished.get(provider) !== 'failed') unfinished.set(provider, 'partial');
        return;
      }
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
      if (recentSessions.length > MAX_RETAINED_SESSIONS) {
        failed = true;
        if (unfinished.get(provider) !== 'failed') unfinished.set(provider, 'partial');
      }
      const retained = {
        ...data,
        daily: [],
        monthly: [],
        hourly,
        session: retainedSessions(recentSessions, cutoff),
      };
      const newRows = !prior || JSON.stringify(prior.data[0]?.hourly) !== JSON.stringify(hourly);
      // Collectors retain failed files' checkpoint rows alongside fresh files.
      // Only a complete empty scan can retire a previously measured share.
      if (data.eventCount > 0 || authoritativeEmpty || !state.local.has(key)) {
        nextLocal.set(key, {
          provider,
          data: [retained],
          fetchedAt: data.readAt ?? new Date((this.deps.now ?? Date.now)()).toISOString(),
        });
        replacedLocal.add(key);
      }
      if (
        data.eventCount > 0 &&
        (data.readAt || !data.scan || data.scan.failedFiles === 0 || data.scan.readBytes > 0) &&
        (data.readAt ||
          !data.scan ||
          data.scan.completedFiles > 0 ||
          data.scan.readBytes > 0 ||
          newRows)
      ) {
        const readAt = data.readAt ?? new Date((this.deps.now ?? Date.now)()).toISOString();
        if (!localReadAt.has(provider) || readAt > (localReadAt.get(provider) ?? ''))
          localReadAt.set(provider, readAt);
      }
      succeeded.add(provider);
    });
    if (generation !== state.generation) return;
    // Mirror ownership covers all Claude requests. Keep that contribution and
    // its election together if any request still carries rows from the old pass.
    const retainClaude =
      ((hadClaude || previousRemote !== null) &&
        requests.some(
          (source) => source.provider === 'claude' && !replacedLocal.has(localRequestKey(source))
        )) ||
      (!discoveryComplete && hadClaude);
    const retainedClaudeRemote = retainClaude ? previousRemote : undefined;
    // A bare remote answer with no election metadata needs no paired local
    // retention; ordinary per-request carry-forward can still converge past the cap.
    const retainClaudeLocal =
      retainClaude &&
      (previousRemote === null ||
        previousRemote.fetchedAt !== undefined ||
        previousRemote.reconcileClaudeMirrors !== undefined ||
        previousRemote.results.some((entry) => entry.tool === 'claude') ||
        previousRemote.states.some((entry) => entry.tool === 'claude'));
    if (retainClaudeLocal) {
      for (const [key, source] of nextLocal)
        if (source.provider === 'claude') nextLocal.delete(key);
      for (const [key, source] of state.local)
        if (source.provider === 'claude') nextLocal.set(key, source);
      // Discarded reads cannot advance the date of the rows still being served.
      localReadAt.delete('claude');
    }
    state.local = nextLocal;
    for (const [key, source] of state.local) {
      if (!seenRequests.has(key) && !(retainClaudeLocal && source.provider === 'claude')) {
        const next = replacements.get(key) ?? [];
        if (
          next.some((replacement) => state.local.has(replacement)) ||
          (!next.length && discoveryComplete)
        ) {
          state.local.delete(key);
          state.identities.delete(key);
          continue;
        }
      }
      const entries = collected.get(source.provider) ?? [];
      source.data = source.data.map((data) => {
        const hourly = data.hourly.filter(
          (hour) => Date.parse(`${hour.hour.replace(' ', 'T')}:00Z`) >= cutoff
        );
        return {
          ...data,
          hourly,
          session: retainedSessions(data.session, cutoff),
          eventCount:
            hourly.length === data.hourly.length
              ? data.eventCount
              : hourly.reduce((sum, hour) => sum + (hour.requestCount ?? 0), 0),
        };
      });
      entries.push(...source.data);
      collected.set(source.provider, entries);
      localEvents.set(
        source.provider,
        (localEvents.get(source.provider) ?? 0) +
          source.data.reduce((sum, data) => sum + data.eventCount, 0)
      );
    }
    // JSON (and a retained election) drops the reconciler. Rebuild it only when
    // every Claude request published a replacement, never after a rejected pass.
    if (!retainClaude && !state.remote?.reconcileClaudeMirrors) {
      try {
        state.remote = newerRemote(
          state.remote,
          this.cachedRemote(cutoff, (this.deps.scope ?? getCcsDir)())
        );
      } catch {
        failed = true;
      }
    }
    // Phase 1: publish the fresh local rows now, merged with the previous remote answer, so a
    // slow or timing-out remote scan (Windows) never holds fresh local data hostage. The remote
    // merge below publishes a second time when it answers.
    this.publish(state, {
      scope: (this.deps.scope ?? getCcsDir)(),
      collected,
      retainedClaudeRemote,
      requests,
      succeeded,
      unfinished,
      discoveryComplete,
      attempted,
      remoteStates: state.remote ? state.remote.states : null,
      remotePending: true,
      pendingHosts: [...state.remotePending],
      localEvents,
      localReadAt,
      presence,
      failed,
    });
    const remaining = deadline - Date.now();
    let remote: Awaited<typeof remotePromise> = settledRemote;
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
      const previousRemote = state.remote;
      try {
        remoteAnswer = newerRemote(
          state.remote,
          this.cachedRemote(cutoff, (this.deps.scope ?? getCcsDir)())
        );
      } catch {
        remoteAnswer = state.remote;
      }
      if (remoteAnswer && remoteAnswer !== previousRemote) remoteStates = remoteAnswer.states;
      state.remote = remoteAnswer;
    }
    if (generation !== state.generation) return;
    // Phase 2: merge the remote answer (fresh, carried, or cached) and publish again.
    this.publish(state, {
      scope: (this.deps.scope ?? getCcsDir)(),
      collected,
      retainedClaudeRemote,
      requests,
      succeeded,
      unfinished,
      discoveryComplete,
      attempted,
      remoteStates,
      remoteAnswer,
      remotePending: false,
      pendingHosts: [...state.remotePending],
      localEvents,
      localReadAt,
      presence,
      failed,
      cutoff,
    });
    if (!remote)
      void remotePromise.then((answer) => {
        if (!answer || generation !== state.generation) return;
        state.remote = answer;
        this.publish(state, {
          scope: (this.deps.scope ?? getCcsDir)(),
          collected,
          retainedClaudeRemote,
          requests,
          succeeded,
          unfinished,
          discoveryComplete,
          attempted,
          remoteStates: answer.states,
          remoteAnswer: answer,
          remotePending: false,
          pendingHosts: [],
          localEvents,
          localReadAt,
          presence,
          failed: unfinished.size > 0 || !discoveryComplete,
          cutoff,
        });
      });
  }

  /**
   * Publish one snapshot from local rows plus a remote answer, without mutating either input, then
   * persist it for the next restart. While the remote scan is in flight (`remotePending`), previous
   * remote states stand untouched (they are not timed out yet) and a host without a previous answer
   * says "scanning"; `pendingHosts` names the hosts whose scan outlived this collection's deadline.
   */
  private publish(
    state: ActivityState,
    args: {
      scope: string;
      collected: Map<AccountAnalyticsActivityProvider, UsageWorkerResult[]>;
      /** The election paired with retained Claude local rows, including no remote rows. */
      retainedClaudeRemote?: RemoteAnswer | null;
      /** This collection's local requests; a root-discovery bound on one is named on its tool. */
      requests: AccountAnalyticsActivityRequest[];
      succeeded: Set<AccountAnalyticsActivityProvider>;
      unfinished: Map<AccountAnalyticsActivityProvider, 'partial' | 'skipped' | 'failed'>;
      discoveryComplete: boolean;
      attempted: Set<AccountAnalyticsActivityProvider>;
      remoteStates: AnalyticsRemoteSourceState[] | null;
      remoteAnswer?: RemoteAnswer | null;
      remotePending: boolean;
      pendingHosts: readonly AnalyticsRemoteHost[];
      localEvents: Map<AccountAnalyticsActivityProvider, number>;
      localReadAt: Map<AccountAnalyticsActivityProvider, string>;
      presence: Record<AccountAnalyticsActivityProvider, boolean> | null;
      failed: boolean;
      cutoff?: number;
    }
  ): void {
    const {
      scope,
      collected,
      requests,
      succeeded,
      unfinished,
      discoveryComplete,
      attempted,
      remoteStates,
      remoteAnswer,
      remotePending,
      pendingHosts,
      localEvents,
      localReadAt,
      presence,
      failed,
    } = args;
    const cutoff = args.cutoff ?? (this.deps.now ?? Date.now)() - 31 * 86_400_000;
    const merged = new Map<AccountAnalyticsActivityProvider, UsageWorkerResult[]>();
    for (const [provider, data] of collected) merged.set(provider, [...data]);
    let answer = remoteAnswer === undefined ? state.remote : remoteAnswer;
    const retainingClaude = args.retainedClaudeRemote !== undefined;
    if (retainingClaude) {
      answer = {
        fetchedAt: answer?.fetchedAt,
        results: [
          ...(answer?.results.filter((entry) => entry.tool !== 'claude') ?? []),
          ...(args.retainedClaudeRemote?.results.filter((entry) => entry.tool === 'claude') ?? []),
        ],
        states: [
          ...(answer?.states.filter((entry) => entry.tool !== 'claude') ?? []),
          ...(args.retainedClaudeRemote?.states.filter((entry) => entry.tool === 'claude') ?? []),
        ],
      };
      // Persist the same pair we serve, so another restart during failure keeps it.
      state.remote = answer;
    } else answer?.reconcileClaudeMirrors?.();
    if (answer) {
      for (const entry of answer.results) {
        const existing = merged.get(entry.tool) ?? [];
        const data = entry.data;
        const hourly = data.hourly
          .filter((hour) => Date.parse(`${hour.hour.replace(' ', 'T')}:00Z`) >= cutoff)
          .slice(0, 744);
        // Remote sessions merge like local ones: whole retained rows, paths stripped. A remote
        // row's key was derived on the host that read it, so it counts exactly like a local one.
        const session = retainedSessions(data.session, cutoff);
        if (hourly.length || session.length || data.eventCount === 0) {
          existing.push({ ...data, daily: [], monthly: [], hourly, session });
          merged.set(entry.tool, existing);
        }
      }
    }
    const now = (this.deps.now ?? Date.now)();
    const fetchedAt = new Date(now).toISOString();
    const updated: SourceData[] = [];
    for (const provider of [
      'claude',
      'codex',
      'omp',
      'muse',
      'zcode',
      'jsonl',
      'antigravity',
    ] as const) {
      const data = merged.get(provider);
      if (data) updated.push({ provider, data, fetchedAt });
    }
    state.sources = updated;
    state.sourceStates = this.buildSourceStates(
      state,
      succeeded,
      unfinished,
      discoveryComplete,
      attempted,
      retainingClaude ? (answer?.states ?? null) : remoteStates,
      localEvents,
      fetchedAt,
      presence,
      remotePending,
      pendingHosts,
      localReadAt
    );
    for (const source of state.sourceStates) {
      if (
        source.host === 'ubuntu' &&
        requests.some(
          (entry) =>
            entry.provider === source.tool &&
            'activity' in entry.request &&
            entry.request.activity?.discoveryTruncated
        )
      )
        source.detail =
          'usage-root discovery is incomplete or hit its depth or scan budget; records from roots found so far are shown';
      // A collector's own reason why its scan stays partial (a compressed Codex log too large to
      // finish in one pass) is named on the cell, so a lasting partial state says why.
      if (source.host !== 'ubuntu') continue;
      const notes = new Set<string>();
      for (const data of collected.get(source.tool as AccountAnalyticsActivityProvider) ?? []) {
        const note = (data.scan as { detail?: unknown } | undefined)?.detail;
        if (typeof note === 'string' && note && data.scan?.complete === false) notes.add(note);
      }
      if (notes.size) source.detail = [source.detail, ...notes].filter(Boolean).join('; ');
    }
    // Convergence tracking: a grid that still holds a scanning cell re-collects
    // on the shorter converge cadence (bounded; see get()).
    if (state.sourceStates.some((entry) => entry.state === 'scanning')) state.scanningSince ??= now;
    else state.scanningSince = null;
    state.pricing = this.snapshotPricing();
    state.partial = failed;
    state.fetchedAt = now;
    state.needsLocalRefresh = false;
    if (this.deps.persistSnapshot === true) savePersistedSnapshot(scope, state);
  }

  private buildSourceStates(
    state: ActivityState,
    succeeded: Set<AccountAnalyticsActivityProvider>,
    unfinished: Map<AccountAnalyticsActivityProvider, 'partial' | 'skipped' | 'failed'>,
    discoveryComplete: boolean,
    attempted: Set<AccountAnalyticsActivityProvider>,
    remote: AnalyticsRemoteSourceState[] | null,
    localEvents: Map<AccountAnalyticsActivityProvider, number>,
    fetchedAt: string,
    presence: Record<AccountAnalyticsActivityProvider, boolean> | null,
    remotePending = false,
    pendingHosts: readonly AnalyticsRemoteHost[] = [],
    localReadAt = new Map<AccountAnalyticsActivityProvider, string>()
  ): AccountAnalyticsSource[] {
    const entries: AccountAnalyticsSource[] = [];
    const previous = new Map(
      state.sourceStates.map((entry) => [`${entry.tool}\0${entry.host}`, entry])
    );
    for (const tool of [
      'claude',
      'codex',
      'omp',
      'muse',
      'zcode',
      'jsonl',
      'antigravity',
    ] as const) {
      const key = `${tool}\0ubuntu`;
      const old = previous.get(key);
      const pending = unfinished.get(tool);
      if (!discoveryComplete && !attempted.has(tool) && !pending) {
        entries.push({
          tool,
          host: 'ubuntu',
          state: 'scanning',
          lastScanAt: old?.lastScanAt ?? null,
          rowCount: old?.rowCount ?? 0,
          detail: 'local discovery is incomplete; the next check continues looking for this tool',
        });
      } else if (pending) {
        const events = localEvents.get(tool) ?? old?.rowCount ?? 0;
        entries.push({
          tool,
          host: 'ubuntu',
          state:
            pending === 'skipped'
              ? 'scanning'
              : events > 0
                ? 'cached'
                : pending === 'failed'
                  ? 'unavailable'
                  : 'scanning',
          // Only productive partial reads advance the date; replayed aggregates carry their
          // saved time. Failed files alone never advance the date, but a
          // productive sibling may still have supplied fresh rows.
          lastScanAt: localReadAt.get(tool) ?? old?.lastScanAt ?? null,
          rowCount: events,
          detail:
            pending === 'failed'
              ? events > 0
                ? 'local scan failed; showing records read so far'
                : 'local scan failed'
              : pending === 'skipped'
                ? 'the local scan ran out of time before it reached this tool; it continues on the next check'
                : 'partial local scan; records read so far are shown and remaining logs continue on the next check',
        });
      } else if (succeeded.has(tool)) {
        const events = localEvents.get(tool) ?? 0;
        entries.push({
          tool,
          host: 'ubuntu',
          // A tool whose logs were read and hold nothing in the window says so,
          // exactly like a remote host in the same situation.
          state: events > 0 ? 'ok' : 'no_usage',
          lastScanAt: localReadAt.get(tool) ?? fetchedAt,
          rowCount: events,
          detail: events > 0 ? null : 'no usage recorded in the last 31 days',
        });
      } else if (old && attempted.has(tool)) {
        entries.push({
          ...old,
          state: old.rowCount > 0 ? 'cached' : 'unavailable',
          detail:
            old.rowCount > 0
              ? 'local scan failed; showing previously read records'
              : 'local scan failed',
        });
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
      for (const host of REMOTE_ANALYTICS_HOSTS) {
        // A scan that outlived the collection deadline is still running, but
        // its answer is gone: the tools honestly go back to "scanning" (the
        // next refresh continues), carrying their previous counts.
        if (pendingHosts.includes(host)) {
          for (const tool of analyticsRemoteTargets(host)) {
            const old = previous.get(`${tool}\0${host}`);
            entries.push({
              tool,
              host,
              state: 'scanning',
              lastScanAt: old?.lastScanAt ?? null,
              rowCount: old?.rowCount ?? 0,
              detail: 'the scan is taking longer than one refresh; it continues on the next one',
            });
          }
          continue;
        }
        if (remotePending) {
          // The remote scan is still running behind this publish: previous
          // remote states stand untouched (they are not timed out yet), and a
          // host with no previous answer says "scanning", so a cold load
          // never shows an absent or failed remote grid.
          for (const tool of analyticsRemoteTargets(host)) {
            const old = previous.get(`${tool}\0${host}`);
            if (old) entries.push({ ...old });
            else
              entries.push({
                tool,
                host,
                state: 'scanning',
                lastScanAt: null,
                rowCount: 0,
                detail: 'the first scan is running',
              });
          }
          continue;
        }
        // The remote scans never answered; the previous remote aggregates are
        // still in the totals, and are marked. Every scanned kind is listed,
        // so Claude Code and Codex remotes are never silently absent.
        for (const tool of analyticsRemoteTargets(host)) {
          const old = previous.get(`${tool}\0${host}`);
          if (old && (old.rowCount > 0 || old.state !== 'scanning'))
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
    return sortSourceEntries(entries);
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
        local: new Map(),
        identities: new Map(),
        requests: [],
        attempts: new Map(),
        attemptSequence: 0,
        fetchedAt: -Infinity,
        needsLocalRefresh: false,
        pending: null,
        generation: 0,
        manualRefreshPending: false,
        sources: [],
        partial: false,
        sourceStates: [],
        remote: null,
        remotePending: [],
        scanningSince: null,
        pricing: this.snapshotPricing(),
        projectionMs: 0,
      };
      // A restarted server serves the last persisted snapshot instantly while its first
      // collection runs, instead of answering 'loading' with no data.
      const persisted = this.deps.persistSnapshot === true ? loadPersistedSnapshot(scope) : null;
      if (persisted && persisted.sources.length > 0) {
        state.sources = persisted.sources;
        state.sourceStates = persisted.sourceStates;
        state.remote = persisted.remote;
        state.local = new Map(persisted.local);
        state.identities = new Map(persisted.identities ?? []);
        state.partial = persisted.partial;
        state.fetchedAt = persisted.fetchedAt;
        // Serve the saved local rows and mirror election together, even when recent.
        // A local pass that replaces the Claude contribution can replace its election.
        state.needsLocalRefresh = true;
      }
      this.states.set(scope, state);
      while (this.states.size > 8) {
        const oldest = this.states.keys().next().value;
        if (oldest === undefined) break;
        this.states.delete(oldest);
      }
    }
    // The filter names the provider that served the usage (post-attribution),
    // so every usage provider selects, not just the tools' own providers.
    const activityFilters: readonly string[] = ['all', ...USAGE_PROVIDER_ORDER];
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
          {
            tz,
            sources: state.sourceStates,
            refreshing: state.pending !== null,
            refreshingRemote: [...state.remotePending],
          }
        ),
        // Already-read history still says which providers have activity, so
        // the provider list stays stable while a quota-only filter is chosen.
        coverage: accountAnalyticsActivityCoverage(state.sources, from, to),
      };
    if (query.refresh === true && !state.manualRefreshPending) {
      state.generation++;
      state.manualRefreshPending = true;
      if (!state.pending) this.startCollection(state);
    } else if (!state.pending) {
      const now = (this.deps.now ?? Date.now)();
      const converging =
        state.scanningSince !== null && now - state.scanningSince < CONVERGE_WINDOW_MS;
      const interval = converging
        ? Math.min(this.refreshIntervalMs(), CONVERGE_INTERVAL_MS)
        : this.refreshIntervalMs();
      if (state.needsLocalRefresh || now - state.fetchedAt >= interval) {
        state.generation++;
        this.startCollection(state);
      }
    }
    const budget = this.deps.responseBudgetMs ?? 1500;
    const elapsed = this.deps.elapsedMs ?? (() => performance.now());
    const wait = Math.max(0, budget - state.projectionMs);
    let timer: ReturnType<typeof setTimeout> | undefined;
    // A cached snapshot is served instantly while the refresh runs behind it; only a first run
    // with nothing to serve waits for the collection (bounded by the response budget).
    if (state.pending && state.sources.length === 0) {
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
    // A cold answer while the first collection is still running: the grid
    // names every tool as scanning, so the loading page shows per-host
    // progress from the very first answer instead of an empty grid.
    const answerSourceStates =
      status === 'loading' && state.sourceStates.length === 0
        ? coldScanningSourceStates()
        : state.sourceStates;
    const scans = state.sources.flatMap((source) =>
      source.data
        .map((data) => data.scan)
        .filter((scan): scan is NonNullable<UsageWorkerResult['scan']> => scan !== undefined)
    );
    const totalFiles = scans.reduce((sum, scan) => sum + scan.totalFiles, 0);
    const completedFiles = scans.reduce((sum, scan) => sum + scan.completedFiles, 0);
    const unfinishedFiles = scans.reduce((sum, scan) => sum + (scan.unfinishedFiles ?? 0), 0);
    const failedFiles = scans.reduce((sum, scan) => sum + scan.failedFiles, 0);
    const scanProgress = scans.some((scan) => !scan.complete)
      ? completedFiles + unfinishedFiles === totalFiles && unfinishedFiles > 0
        ? ` All available records have been read; ${unfinishedFiles} local log files end with unfinished records and will be checked for changes.`
        : completedFiles < totalFiles
          ? ` ${completedFiles} of ${totalFiles} local log files have been read; remaining files resume from saved positions on the next check.`
          : ` All ${totalFiles} local log files have been read; ${failedFiles > 0 ? `${failedFiles} local scan failures will be retried` : 'the scan could not confirm complete discovery'}.`
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
            ? 'CLI activity is refreshing. Records read so far are shown; the Included usage grid says what each source contributed. Cost is an API-equivalent estimate, not a subscription charge.'
            : state.partial
              ? 'Records read so far are shown while a bounded scan completes in the background; the Included usage grid says what each source contributed. Cost is an API-equivalent estimate, not a subscription charge.' +
                scanProgress
              : `CLI activity from Ubuntu, Mac, Windows and Nas1 (Claude Code, Codex, OMP, Muse and zcode), across accounts, for UTC hourly buckets starting in this range; days are grouped in ${tz}. Cost is an API-equivalent estimate, not a subscription charge. Usage events are parsed log entries; session counts mean sessions active in this range.`,
      {
        tz,
        pricing: state.pricing,
        sources: answerSourceStates,
        refreshing: state.pending !== null,
        refreshingRemote: [...state.remotePending],
      }
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
  service ??= new AccountAnalyticsActivityService({
    persistSnapshot: true,
    refreshPricing: startModelsDevRegistryRefresh,
  });
  return service.get(query, from, to, options);
}
