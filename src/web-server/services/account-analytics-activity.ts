import * as fs from 'fs';
import * as path from 'path';
import { createHash } from 'crypto';
import { Worker } from 'worker_threads';
import { getCcsDir } from '../../utils/config-manager';
import { CCSError } from '../../errors/error-types';
import { withDeadline } from '../../utils/settle-deadline';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import {
  accountAnalyticsActivityCoverage,
  projectAccountAnalyticsActivity,
  type SourceData,
} from './account-analytics-projection';
import { USAGE_PROVIDER_ORDER } from './account-analytics-attribution';
import { EXPERIMENT_ROOTS_TTL_MS, type ExperimentRootsView } from '../usage/experiment-usage-roots';
import { collectorConcurrency, runBounded } from '../usage/collector-concurrency';
import type { PlanWorkerRequest, PlanWorkerResponse } from '../usage/analytics-plan-worker';
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
import {
  LargeCodexReadQueue,
  type LargeCodexReader,
  type LargeCodexReadSpec,
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
export { experimentActivityRequests, extraActivityRequests } from '../usage/analytics-request-plan';

interface ActivityState {
  /** In-memory scheduling only: paths never enter aggregate snapshots or page responses. */
  largeCodexReads: Map<string, LargeCodexReadSpec[]>;
  local: Map<string, SourceData>;
  /** Unpublished Claude/legacy replacements survive capped passes, but never a later failed read. */
  pendingLocal: Map<string, { source: SourceData; readAt?: string }>;
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
  /** Dedicated oversized-file worker, independent of normal collection/response budgets. */
  largeCodexReader?: LargeCodexReader;
  largeCodexTimeoutMs?: number;
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
  /**
   * Runs one part of local root discovery (analytics-plan-worker.ts); undefined when its thread
   * did not answer in time. Unset: the real thread. Ignored when `requests` is set.
   */
  planThread?: (
    request: PlanWorkerRequest,
    timeoutMs: number,
    key: string
  ) => Promise<PlanWorkerResponse | undefined>;
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
/** How long a stopped collector thread may take to exit before its disk is treated as hung. */
const THREAD_STOP_DEADLINE_MS = 5_000;
/** The plan's stats and directory reads take well under a second on a healthy disk. */
const PLAN_READY_DEADLINE_MS = 15_000;
/** The OMP marker scan is bounded at 30 s; this leaves time to start the thread and answer. */
const PLAN_LATER_DEADLINE_MS = 40_000;
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

/** Keys of threads a hung disk call still holds after their caller gave up on them. */
const lingeringThreads = new Set<string>();

/**
 * Run one collector thread and settle with its message (undefined on timeout or failure).
 *
 * It settles only once the thread has stopped: a thread cut off by its time bound must never still
 * be writing a checkpoint when the next collection reads the same files. A thread blocked in a
 * disk call that never returns (a hard-mounted share that stopped answering) cannot be stopped,
 * so past `stopDeadlineMs` the call rejects instead, and `key` is held until the thread exits:
 * the same logs are not read again meanwhile, and a long outage never piles up stuck threads.
 */
export function runAnalyticsThread(
  file: string,
  workerData: unknown,
  timeoutMs: number,
  key: string | undefined,
  stopDeadlineMs = THREAD_STOP_DEADLINE_MS
): Promise<unknown> {
  if (key !== undefined && lingeringThreads.has(key))
    return Promise.reject(new CCSError('An earlier read of these logs has not stopped yet'));
  return new Promise((resolve, reject) => {
    const worker = new Worker(file, {
      workerData,
      env: { ...process.env, CCS_DIR: getCcsDir() },
      resourceLimits: { maxOldGenerationSizeMb: 256, stackSizeMb: 4 },
    });
    let settled = false;
    const finish = (message?: unknown): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      worker.removeAllListeners();
      void withDeadline(worker.terminate(), stopDeadlineMs).then(
        () => resolve(message),
        () => {
          // A thread that later fails must not raise an unhandled 'error' on the server.
          worker.on('error', () => undefined);
          if (key !== undefined) {
            lingeringThreads.add(key);
            worker.once('exit', () => lingeringThreads.delete(key));
          }
          reject(new CCSError('A local analytics thread did not stop; its disk may be hung'));
        }
      );
    };
    const timer = setTimeout(() => finish(), timeoutMs);
    worker.once('message', (message: unknown) => finish(message));
    worker.once('error', () => finish());
    worker.once('exit', () => finish());
  });
}

/**
 * Unlike legacy merged caches, each worker keeps the native tool dimension. `key` names the logs
 * it reads (see runAnalyticsThread); without one, a thread stuck on a hung disk is not tracked.
 */
export async function loadAccountAnalyticsWorker(
  request: UsageWorkerRequest,
  budgetMs = MAX_WORKER_TIME_MS,
  key?: string
): Promise<UsageWorkerResult> {
  // The collector stops reading at 12 s; leave time to aggregate and return its checkpoints.
  const response = (await runAnalyticsThread(
    path.join(__dirname, '../usage', `native-usage-worker${path.extname(__filename)}`),
    request,
    Math.min(MAX_WORKER_TIME_MS, Math.max(15_000, budgetMs)),
    key
  )) as UsageWorkerResponse | undefined;
  const data = response?.ok === true ? response.data : undefined;
  const bounded =
    !!data &&
    [data.daily, data.hourly, data.monthly, data.session].every(
      (rows) => Array.isArray(rows) && rows.length <= MAX_ROWS
    );
  if (bounded && data) return data;
  throw new CCSError('Local analytics worker could not return bounded usage history');
}

type LocalRequestPlan = {
  ready: AccountAnalyticsActivityRequest[];
  later: Promise<AccountAnalyticsActivityRequest[]>;
  /** A discovery thread did not answer in time; the previous collection's roots stand in. */
  reused: boolean;
};

async function runPlanThread(
  request: PlanWorkerRequest,
  timeoutMs: number,
  key: string
): Promise<PlanWorkerResponse | undefined> {
  return (await runAnalyticsThread(
    path.join(__dirname, '../usage', `analytics-plan-worker${path.extname(__filename)}`),
    request,
    timeoutMs,
    key
  ).catch(() => undefined)) as PlanWorkerResponse | undefined;
}

/**
 * The local log roots to read, found on a plan thread (analytics-request-plan.ts) in two parts so
 * the collectors start at once instead of waiting for the slowest discovery step. `ready` holds
 * the roots a few stats find; `later` resolves once the OMP marker scan has run, with the saved
 * extras. A part that does not answer in time (a share that stopped answering) is replaced by
 * the roots `previous` read for it, and the plan says so: the discovery is then incomplete.
 */
async function localRequestPlan(
  previous: readonly AccountAnalyticsActivityRequest[],
  runPlan: NonNullable<AccountAnalyticsActivityDeps['planThread']> = runPlanThread
): Promise<LocalRequestPlan> {
  const ccsDir = getCcsDir();
  const activity = { minDate: Date.now() - 31 * 86_400_000, cacheDir: path.join(ccsDir, 'cache') };
  const answer = await runPlan(
    { part: 'ready', activity },
    PLAN_READY_DEADLINE_MS,
    `plan-ready:${ccsDir}`
  );
  const ready = answer?.ok === true ? answer.ready : undefined;
  if (!ready) return { ready: [...previous], later: Promise.resolve([]), reused: true };
  if (ready.experimentView) startExperimentRootsSlice(activity.cacheDir, ready.experimentView);
  const readyKeys = new Set(ready.requests.map(localRequestKey));
  const plan: LocalRequestPlan = {
    ready: ready.requests,
    later: Promise.resolve([]),
    reused: false,
  };
  plan.later = runPlan(
    { part: 'later', activity, scanned: ready.scanned },
    PLAN_LATER_DEADLINE_MS,
    `plan-later:${ccsDir}`
  ).then((later) => {
    if (later?.ok === true && later.later) return later.later;
    plan.reused = true;
    return previous.filter((entry) => !readyKeys.has(localRequestKey(entry)));
  });
  return plan;
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
  void loadAccountAnalyticsWorker(
    { kind: 'experiment-roots', cacheDir },
    MAX_WORKER_TIME_MS,
    `experiment-roots:${cacheDir}`
  )
    .catch(() => undefined)
    .finally(() => {
      experimentSliceRunning = false;
    });
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
  private readonly largeCodexReads: LargeCodexReadQueue;
  private readonly onShutdown = (): void => {
    void this.shutdown();
  };
  private closed = false;

  constructor(private readonly deps: AccountAnalyticsActivityDeps = {}) {
    this.largeCodexReads = new LargeCodexReadQueue(deps.largeCodexReader, deps.largeCodexTimeoutMs);
  }

  async shutdown(): Promise<void> {
    this.closed = true;
    process.removeListener('SIGINT', this.onShutdown);
    process.removeListener('SIGTERM', this.onShutdown);
    await this.largeCodexReads.shutdown();
  }

  private enqueueLargeCodexReads(data: UsageWorkerResult): void {
    if (this.closed || !data.scan?.largeCodexReads?.length) return;
    if (!process.listeners('SIGTERM').includes(this.onShutdown)) {
      process.once('SIGINT', this.onShutdown);
      process.once('SIGTERM', this.onShutdown);
    }
    this.largeCodexReads.enqueue(data.scan.largeCodexReads);
  }

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
    const plan: LocalRequestPlan = this.deps.requests
      ? { ...injectedRequestPlan(this.deps.requests), reused: false }
      : await localRequestPlan(state.requests, this.deps.planThread);
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
          const value = await (this.deps.loadWorker
            ? this.deps.loadWorker(source.request, budget)
            : loadAccountAnalyticsWorker(source.request, budget, localRequestKey(source)));
          settled[index] = {
            status: 'fulfilled',
            value: value.scan?.largeCodexReads
              ? { ...value, scan: { ...value.scan, largeCodexReads: undefined } }
              : value,
          };
          state.largeCodexReads.set(localRequestKey(source), value.scan?.largeCodexReads ?? []);
          this.enqueueLargeCodexReads(value);
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
    const currentKeys = new Set(requests.map(localRequestKey));
    for (const key of state.largeCodexReads.keys())
      if (!currentKeys.has(key)) state.largeCodexReads.delete(key);
    if (skipped > 0) failed = true;
    // A discovery thread missed its deadline (a hung share): the previous roots were read instead,
    // so this inventory is incomplete and says nothing about which tools are installed.
    if (plan.reused) {
      failed = true;
      discoveryFailed = true;
    }
    for (const source of requests)
      state.identities.set(localRequestKey(source), localRequestIdentity(source));
    // An unplanned legacy share has no safe request-to-request replacement.
    // Hold its provider's entire contribution until the current inventory is covered.
    const legacyProviders = new Set(
      [...state.local]
        .filter(([key]) => !state.identities.has(key))
        .map(([, source]) => source.provider)
    );
    const replacements = new Map<string, string[]>();
    const predecessors = new Map<string, SourceData>();
    for (const [key, prior] of state.local) {
      if (seenRequests.has(key)) continue;
      const identity = state.identities.get(key);
      if (!identity) continue;
      for (const source of requests) {
        const nextKey = localRequestKey(source);
        const next = state.identities.get(nextKey);
        if (!next || source.provider !== prior.provider || state.local.has(nextKey)) continue;
        if (
          identity.group !== next.group ||
          !identity.roots.every((root) => next.roots.includes(root))
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
    const presence =
      this.deps.requests === undefined && !plan.reused ? localSourcePresence(requests) : null;
    const succeeded = new Set<AccountAnalyticsActivityProvider>();
    const unfinished = new Map<
      AccountAnalyticsActivityProvider,
      'partial' | 'skipped' | 'failed'
    >();
    const attempted = new Set<AccountAnalyticsActivityProvider>();
    const localEvents = new Map<AccountAnalyticsActivityProvider, number>();
    const localReadAt = new Map<AccountAnalyticsActivityProvider, string>();
    if (discoveryComplete)
      for (const key of state.pendingLocal.keys())
        if (!seenRequests.has(key)) state.pendingLocal.delete(key);
    // Stage replacements until their legacy unit or Claude mirror election can advance.
    const nextLocal = new Map(state.local);
    requests.forEach((source, index) => {
      const key = localRequestKey(source);
      const result = settled[index];
      const provider = source.provider;
      if (!result) {
        if (!unfinished.has(provider)) unfinished.set(provider, 'skipped');
        return;
      }
      const pendingLocal = state.pendingLocal.get(key);
      // A later attempt supersedes the staged read even when it fails or cannot
      // replace rows. Only skipped requests can carry a pending replacement.
      state.pendingLocal.delete(key);
      attempted.add(provider);
      if (result.status === 'rejected') {
        unfinished.set(provider, 'failed');
        failed = true;
        return;
      }
      const data = result.value;
      const prior = pendingLocal?.source ?? state.local.get(key) ?? predecessors.get(key);
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
        (prior?.data.some((entry) => entry.eventCount > 0) ||
          (legacyProviders.has(provider) &&
            [...state.local.values()].some(
              (source) =>
                source.provider === provider && source.data.some((entry) => entry.eventCount > 0)
            )))
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
      }
      let productiveReadAt: string | undefined;
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
        productiveReadAt = readAt;
        if (!localReadAt.has(provider) || readAt > (localReadAt.get(provider) ?? ''))
          localReadAt.set(provider, readAt);
      }
      const replacement = nextLocal.get(key);
      if (
        (provider === 'claude' || legacyProviders.has(provider)) &&
        replacement &&
        replacement !== state.local.get(key)
      )
        state.pendingLocal.set(key, {
          source: replacement,
          readAt: productiveReadAt,
        });
      succeeded.add(provider);
    });
    if (generation !== state.generation) return;
    const retainLegacy = new Set(
      [...legacyProviders].filter(
        (provider) =>
          !discoveryComplete ||
          requests.some(
            (source) =>
              source.provider === provider && !state.pendingLocal.has(localRequestKey(source))
          )
      )
    );
    // Mirror ownership covers all Claude requests. Keep that contribution and
    // its election together until every request has a replacement since the last
    // publication. Healthy requests need not all fit into the same capped pass.
    const claudeCovered = !requests.some(
      (source) => source.provider === 'claude' && !state.pendingLocal.has(localRequestKey(source))
    );
    const retainClaude =
      ((hadClaude || previousRemote !== null) && !claudeCovered) ||
      (!discoveryComplete && hadClaude);
    const retainedClaudeRemote = retainClaude ? previousRemote : undefined;
    // A bare remote answer with no election metadata needs no paired local
    // retention; ordinary per-request carry-forward can still converge past the cap.
    const retainClaudeLocal =
      retainLegacy.has('claude') ||
      (retainClaude &&
        (previousRemote === null ||
          previousRemote.fetchedAt !== undefined ||
          previousRemote.reconcileClaudeMirrors !== undefined ||
          previousRemote.results.some((entry) => entry.tool === 'claude') ||
          previousRemote.states.some((entry) => entry.tool === 'claude')));
    const retainedProviders = new Set(retainLegacy);
    if (retainClaudeLocal) retainedProviders.add('claude');
    for (const provider of retainedProviders) {
      for (const [key, source] of nextLocal)
        if (source.provider === provider) nextLocal.delete(key);
      for (const [key, source] of state.local)
        if (source.provider === provider) nextLocal.set(key, source);
      // Unpublished reads cannot advance the date of the rows still being served.
      localReadAt.delete(provider);
    }
    for (const [key, pending] of state.pendingLocal) {
      const provider = pending.source.provider;
      if (retainedProviders.has(provider) || (provider === 'claude' && retainClaude)) continue;
      nextLocal.set(key, pending.source);
      if (pending.readAt && pending.readAt > (localReadAt.get(provider) ?? ''))
        localReadAt.set(provider, pending.readAt);
      if (discoveryComplete && (provider !== 'claude' || claudeCovered))
        state.pendingLocal.delete(key);
    }
    state.local = nextLocal;
    for (const [key, source] of state.local) {
      if (!seenRequests.has(key) && !retainedProviders.has(source.provider)) {
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
        largeCodexReads: new Map(),
        local: new Map(),
        pendingLocal: new Map(),
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
    let answerSourceStates =
      status === 'loading' && state.sourceStates.length === 0
        ? coldScanningSourceStates()
        : state.sourceStates;
    if (
      [...state.largeCodexReads.values()].some((specs) => this.largeCodexReads.isRunning(specs))
    ) {
      answerSourceStates = answerSourceStates.map((source) =>
        source.tool === 'codex' && source.host === 'ubuntu'
          ? { ...source, detail: 'reading 1 large compressed Codex log in the background' }
          : source
      );
    }
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
