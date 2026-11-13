import * as fs from 'fs';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import {
  aggregateRows,
  aggregateSessionAggregates,
  type CompactEntry,
  type SessionAggregateRow,
} from '../usage/account-activity-collector';
import type { UsageWorkerResult } from '../usage/worker-client';
import {
  ANALYTICS_SCAN_FAILURE_CAUSES,
  AnalyticsHelperIntegrityError,
  AnalyticsRemoteTransportError,
  REMOTE_ANALYTICS_HOSTS,
  fixedAnalyticsRemoteAliases,
  hasClaudeReplacementMetadata,
  runAnalyticsRemoteHelper,
  resolveAnalyticsRemoteHosts,
  type AnalyticsRemoteAliases,
  type AnalyticsRemoteFingerprint,
  type AnalyticsRemoteHost,
  type AnalyticsRemoteKind,
  type AnalyticsRemoteKindResult,
  type AnalyticsRemoteRequest,
  type AnalyticsRemoteResponse,
  type AnalyticsRemoteRow,
  type AnalyticsRemoteSessionRow,
  type AnalyticsScanFailureCause,
} from './analytics-remote-transport';
import { readDashboardPreferences } from './dashboard-preferences';
import { runBounded } from '../usage/collector-concurrency';
import { localClaudeLogNames, type ClaudeLogNames } from './analytics-claude-log-names';
import { cellsFromCompact } from '../usage/activity-cells';
import type { AccountAnalyticsActivityProvider } from './account-analytics-types';

export type AnalyticsSourceTool = 'claude' | 'codex' | 'omp' | 'muse' | 'zcode' | 'antigravity';
export type AnalyticsSourceState =
  | 'ok'
  | 'cached'
  | 'unavailable'
  | 'not_installed'
  /** The scan is still working on this tool: it is running now, or ran out of time before it finished. */
  | 'scanning'
  /** The tool was scanned and its logs hold no usage inside the 31-day window. */
  | 'no_usage'
  /** Its logs were found and could not be read; nothing read earlier is kept (`cause`). */
  | 'unreadable'
  /** The computer's scan failed and nothing read earlier is kept (`cause`). */
  | 'scan_failed';

export interface AnalyticsRemoteSourceState {
  tool: AnalyticsSourceTool;
  host: AnalyticsRemoteHost;
  state: AnalyticsSourceState;
  lastScanAt: string | null;
  rowCount: number;
  detail: string | null;
  /** One fixed word: why this source is not read now (a scan failure cause, `read_failed`). */
  cause?: string | null;
}

/** A host's last failed scan: its fixed cause, the helper's exit code and when it happened. */
export interface AnalyticsHostFailure {
  cause: AnalyticsScanFailureCause | 'helper_damaged';
  exitCode: number | null;
  at: string;
  /**
   * The tools this failure stopped (the ones the failed calls carried). Tools not in it were read
   * or skipped as recent and keep their own state. Absent (a failure saved before this field):
   * every tool.
   */
  kinds?: AnalyticsRemoteKind[];
}

const REMOTE_KIND_NAMES: ReadonlySet<string> = new Set([
  'claude',
  'codex',
  'omp',
  'muse',
  'zcode',
  'antigravity',
]);

const HOST_FAILURE_CAUSES: ReadonlySet<string> = new Set([
  ...ANALYTICS_SCAN_FAILURE_CAUSES,
  'helper_damaged',
]);

/** The sentence a cell's detail gives for a failure cause. */
export function scanFailureText(cause: string | null | undefined): string {
  switch (cause) {
    case 'ssh_unreachable':
      return 'the computer did not answer over ssh';
    case 'ssh_auth':
      return "the computer refused this account's ssh key";
    case 'ssh_host_key':
      return "the computer's ssh host key is not the one on record (unknown or changed), so ssh would not connect";
    case 'runtime_missing:store_alias':
      return 'no working Python 3.9 or later on this computer (only the Microsoft Store alias, which is not Python)';
    case 'runtime_missing':
      return 'no working Python 3.9 or later on this computer';
    case 'helper_failed':
      return 'the usage reader stopped with an error on this computer';
    case 'timeout':
      return 'the scan took longer than its time limit';
    case 'bad_response':
      return "the computer's answer was not a usage report";
    case 'helper_damaged':
      return 'the packaged analytics helper failed its integrity check, so no computer was scanned; reinstall AI Account Center';
    default:
      return 'the scan failed';
  }
}

export interface AnalyticsRemoteSourceDeps {
  /** An explicit refresh requests a new observation even inside the ordinary scan TTL. */
  force?: boolean;
  hosts?: () => Promise<AnalyticsRemoteAliases>;
  runHelper?: (
    sshHost: string,
    platform: AnalyticsRemoteHost,
    request: AnalyticsRemoteRequest
  ) => Promise<AnalyticsRemoteResponse>;
  now?: () => number;
  cacheDir?: string;
  /** Metadata-only, independent of when the local usage workers finish. */
  claudeLogNames?: () => ClaudeLogNames;
  /**
   * Fires when a host's scan starts and when it settles (success or failure),
   * so callers can report which remotes a refresh is waiting on.
   */
  onHostScan?: (host: AnalyticsRemoteHost, phase: 'start' | 'done') => void;
}

/**
 * Remote coverage: Claude Code, Codex, OMP, Muse, zcode and Antigravity (T3 Code's
 * Antigravity instances included) on the Mac, Windows and Nas1. A tool a host does
 * not have reports `not_installed`; the states are measured by the scan, never assumed.
 */
const REMOTE_TARGETS = Object.fromEntries(
  REMOTE_ANALYTICS_HOSTS.map((host) => [
    host,
    ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'],
  ])
) as Record<AnalyticsRemoteHost, AnalyticsRemoteKind[]>;

/** The kinds scanned on one remote host, for states when no scan answered. */
export function analyticsRemoteTargets(host: AnalyticsRemoteHost): AnalyticsRemoteKind[] {
  return [...REMOTE_TARGETS[host]];
}

const MAX_CACHED_ROWS = 100_000;

/**
 * 6: Atomically replace Claude copy groups; discard accumulated duplicate owners.
 * 5: Claude copy groups, mirror tags and persistent oversized-line verdicts.
 * 4: Claude counts one API response, not one content block; rows cached by
 * version 3 are read again. (3: per-session aggregates ride with the hourly
 * rows, so remote sessions count like local ones.)
 */
const CACHE_VERSION = 6;

interface RemoteCache {
  /** Per-tool last confirmed read, including measured empty/absent answers. */
  kinds?: Partial<
    Record<AnalyticsRemoteKind, { lastScanAt: string | null; state: AnalyticsSourceState }>
  >;
  version: typeof CACHE_VERSION;
  fingerprints: Record<string, Record<string, AnalyticsRemoteFingerprint>>;
  rows: AnalyticsRemoteRow[];
  srows: AnalyticsRemoteSessionRow[];
  lastScanAt: string | null;
  /**
   * The last scan read every kind to the end: no kind was cut short, skipped or failed. Such a
   * host is scanned with one helper call; one still catching up gets a call per kind. Absent in
   * older caches, which therefore start with a call per kind.
   */
  settled?: boolean;
  /**
   * The host's last failed scan, saved without touching rows or prints: the page names it (also
   * after a restart) until a scan answers again, which clears it.
   */
  lastFailure?: AnalyticsHostFailure;
}

function cacheFile(cacheDir: string, host: AnalyticsRemoteHost): string {
  return path.join(cacheDir, 'analytics-remote-v1', `${host}.json`);
}

function blankCache(): RemoteCache {
  return { version: CACHE_VERSION, fingerprints: {}, rows: [], srows: [], lastScanAt: null };
}

function loadCache(file: string): RemoteCache {
  try {
    if (fs.statSync(file).size > 8 * 1024 * 1024) return blankCache();
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as Omit<RemoteCache, 'version'> & {
      version: number;
    };
    // Versions 1/2 predate hashed session aggregates and cannot safely retain
    // their sessions or confirm their files. Unknown future formats also reset.
    if (
      ![3, 4, 5, CACHE_VERSION].includes(value.version) ||
      !value.fingerprints ||
      typeof value.fingerprints !== 'object' ||
      !Array.isArray(value.rows) ||
      value.rows.length > MAX_CACHED_ROWS ||
      !Array.isArray(value.srows) ||
      value.srows.length > MAX_CACHED_ROWS ||
      (value.lastScanAt !== null && typeof value.lastScanAt !== 'string') ||
      (value.settled !== undefined && typeof value.settled !== 'boolean')
    )
      return blankCache();
    // The last failure is a diagnostic: a damaged one is dropped, never trusted.
    const failure = value.lastFailure as Partial<AnalyticsHostFailure> | undefined;
    value.lastFailure =
      failure &&
      typeof failure.cause === 'string' &&
      HOST_FAILURE_CAUSES.has(failure.cause) &&
      (failure.exitCode === null || Number.isSafeInteger(failure.exitCode)) &&
      typeof failure.at === 'string' &&
      Number.isFinite(Date.parse(failure.at))
        ? {
            cause: failure.cause as AnalyticsHostFailure['cause'],
            exitCode: failure.exitCode ?? null,
            at: failure.at,
            ...(Array.isArray(failure.kinds) &&
            failure.kinds.length <= 6 &&
            failure.kinds.every((kind) => REMOTE_KIND_NAMES.has(kind))
              ? { kinds: [...failure.kinds] }
              : {}),
          }
        : undefined;
    if (value.version !== CACHE_VERSION) {
      // Versions 3-5 share the non-Claude row/session/fingerprint formats.
      // Claude's response semantics and copy ownership changed in 4-6: its
      // aggregates and prints must rebuild together, without losing other tools.
      const fingerprints = { ...value.fingerprints };
      const kinds = { ...value.kinds };
      delete fingerprints.claude;
      delete kinds.claude;
      return {
        ...value,
        version: CACHE_VERSION,
        fingerprints,
        kinds,
        rows: value.rows.filter((row) => row.k !== 'claude'),
        srows: value.srows.filter((row) => row.k !== 'claude'),
        settled: false,
      };
    }
    return { ...value, version: CACHE_VERSION };
  } catch {
    return blankCache();
  }
}

function saveCache(file: string, value: RemoteCache): void {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const body = JSON.stringify({
    ...value,
    rows: value.rows.slice(0, MAX_CACHED_ROWS),
    srows: value.srows.slice(0, MAX_CACHED_ROWS),
  });
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, body, { mode: 0o600 });
  fs.renameSync(temporary, file);
  fs.chmodSync(file, 0o600);
}

function hourEpoch(hour: string): number {
  return Date.parse(`${hour.replace(' ', 'T')}:00Z`);
}

function samePrint(
  left: AnalyticsRemoteFingerprint | undefined,
  right: AnalyticsRemoteFingerprint | undefined
): boolean {
  return (
    !!left &&
    !!right &&
    left.size === right.size &&
    left.mtimeMs === right.mtimeMs &&
    (left.head ?? null) === (right.head ?? null) &&
    (left.tail ?? null) === (right.tail ?? null) &&
    (left.walSize ?? 0) === (right.walSize ?? 0) &&
    (left.walMtimeMs ?? 0) === (right.walMtimeMs ?? 0)
  );
}

/**
 * The pricing provider of a remote row, exactly as the local reader of its tool sets it, so a row
 * prices the same on every host. The route the host logged wins. Without one, the local Claude Code
 * reader prices by model name alone; Codex falls back to OpenAI. An empty route preserves
 * that provider-less lookup while retaining the tool identity. A Codex model several providers
 * list at different prices needs its OpenAI fallback to resolve a rate.
 */
function remoteRowProvider(
  kind: AnalyticsRemoteKind,
  route: string | undefined
): { provider?: string } {
  if (route) return { provider: route };
  return kind === 'codex' ? {} : { provider: '' };
}

function toCompact(rows: AnalyticsRemoteRow[]): CompactEntry[] {
  return rows.map((row) => ({
    entry: {
      inputTokens: Math.floor(row.i),
      outputTokens: Math.floor(row.o),
      cacheCreationTokens: Math.floor(row.cw),
      cacheReadTokens: Math.floor(row.cr),
      model: row.m,
      // Remote sessions ride in `srows`, already keyed on the host that read them.
      sessionId: '',
      timestamp: `${row.h.replace(' ', 'T')}:00Z`,
      projectPath: '',
      // The tool that logged the row; its routing provider prices it.
      target: row.k,
      ...remoteRowProvider(row.k, row.p),
      // The helper never mixes logged and unlogged events in one row.
      ...(row.c > 0 ? { costUsd: row.c } : {}),
    },
    events: Math.floor(row.n),
  }));
}

/** The helper hashed each session id on the host it read, so these keys pass through unchanged. */
function toSessionAggregates(srows: AnalyticsRemoteSessionRow[]): SessionAggregateRow[] {
  return srows.map((row) => ({
    sessionId: row.s,
    model: row.m,
    ...remoteRowProvider(row.k, row.p),
    target: row.k,
    firstMs: Math.floor(row.a),
    lastMs: Math.floor(row.z),
    inputTokens: Math.floor(row.i),
    outputTokens: Math.floor(row.o),
    cacheCreationTokens: Math.floor(row.cw),
    cacheReadTokens: Math.floor(row.cr),
    ...(row.c > 0 ? { cost: row.c } : {}),
    events: Math.floor(row.n),
  }));
}

function toWorkerResult(
  rows: AnalyticsRemoteRow[],
  srows: AnalyticsRemoteSessionRow[],
  kind: AnalyticsRemoteKind,
  // the computer read: a remote host, or this one when the usage scanner read it
  host: AnalyticsRemoteHost | 'ubuntu'
): UsageWorkerResult {
  const { hourly } = aggregateRows(toCompact(rows), `${kind}-remote`, kind);
  // Session-hour cells for the history store: a row's file names its session through the
  // session rows (the helper keyed it on the host it read).
  const sessionOf = new Map<string, string>();
  for (const row of srows) if (row.f && row.s) sessionOf.set(row.f, row.s);
  // a host whose clock runs ahead: its records dated after this clock are counted, not written
  const future = { count: 0 };
  const cells = cellsFromCompact(
    toCompact(rows).map((entry, index) => ({
      ...entry,
      entry: { ...entry.entry, sessionId: sessionOf.get(rows[index].f) ?? '' },
    })),
    kind as AccountAnalyticsActivityProvider,
    true,
    { future }
  );
  // Sessions aggregate from the session rows alone: the hourly rows already
  // carry the same tokens, so the two must never be aggregated together.
  const { session } = aggregateSessionAggregates(toSessionAggregates(srows), `${kind}-remote`);
  return {
    daily: [],
    monthly: [],
    hourly,
    cells,
    ...(future.count > 0 ? { futureDated: future.count } : {}),
    host,
    session,
    eventCount: rows.reduce((sum, row) => sum + Math.floor(row.n), 0),
    scan: {
      complete: true,
      completedFiles: 0,
      totalFiles: 0,
      skippedLines: 0,
      failedFiles: 0,
      readBytes: 0,
      unfinishedFiles: 0,
    },
  };
}

function inWindow(row: AnalyticsRemoteRow, minDateMs: number): boolean {
  const epoch = hourEpoch(row.h);
  return Number.isFinite(epoch) && epoch >= minDateMs;
}

/** A session row stays while its last event is in the retained window. */
function sessionInWindow(row: AnalyticsRemoteSessionRow, minDateMs: number): boolean {
  return Number.isFinite(row.z) && row.z >= minDateMs;
}

const READ_WORDS: Partial<Record<AnalyticsSourceState, string>> = {
  ok: 'usage',
  no_usage: 'no usage in range',
  not_installed: 'no usage logs',
};

/**
 * The failure that stands for a kind: this attempt's, else the host's saved last failure when it
 * is newer than the kind's last read, so a restarted server still says the scan is failing.
 */
function standingFailure(
  cached: RemoteCache,
  tool: AnalyticsRemoteKind,
  failure: AnalyticsHostFailure | undefined
): AnalyticsHostFailure | undefined {
  // Only the tools a failure stopped: one read or skipped as recent keeps its own state.
  const stopped = (entry: AnalyticsHostFailure) => !entry.kinds || entry.kinds.includes(tool);
  if (failure) return stopped(failure) ? failure : undefined;
  const last = cached.lastFailure;
  const read = cached.kinds?.[tool]?.lastScanAt;
  return last && stopped(last) && (!read || Date.parse(last.at) > Date.parse(read))
    ? last
    : undefined;
}

/**
 * Previously read aggregates of one host, marked `cached`. A kind with nothing kept is `scan_failed`
 * while a failure stands (with its cause, and what the last read that worked found), never that
 * read's "no usage" or "not installed" as if it were current; without a failure, its last state.
 */
function cachedHostSources(
  host: AnalyticsRemoteHost,
  cached: RemoteCache,
  minDateMs: number,
  detail: string,
  failure?: AnalyticsHostFailure
): {
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
} {
  const results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  for (const tool of REMOTE_TARGETS[host]) {
    const kept = cached.rows.filter((row) => row.k === tool && inWindow(row, minDateMs));
    const keptSessions = cached.srows.filter(
      (row) => row.k === tool && sessionInWindow(row, minDateMs)
    );
    const read = cached.kinds?.[tool];
    const failed = standingFailure(cached, tool, failure);
    const why = failed ? `${scanFailureText(failed.cause)} (${failed.at})` : detail;
    if (kept.length || keptSessions.length) {
      results.push({ tool, data: toWorkerResult(kept, keptSessions, tool, host) });
      states.push({
        tool,
        host,
        state: 'cached',
        lastScanAt: read?.lastScanAt ?? null,
        rowCount: kept.reduce((sum, row) => sum + Math.floor(row.n), 0),
        detail: `${why}; showing previously read aggregates`,
        ...(failed ? { cause: failed.cause } : {}),
      });
    } else if (failed) {
      const words = read ? READ_WORDS[read.state] : undefined;
      states.push({
        tool,
        host,
        state: 'scan_failed',
        lastScanAt: read?.lastScanAt ?? null,
        rowCount: 0,
        detail:
          words && read?.lastScanAt
            ? `${why}; the last scan that worked (${read.lastScanAt}) found ${words}`
            : why,
        cause: failed.cause,
      });
    } else {
      states.push({
        tool,
        host,
        state:
          read?.state === 'no_usage'
            ? 'no_usage'
            : read?.state === 'not_installed'
              ? 'not_installed'
              : 'unavailable',
        lastScanAt: read?.lastScanAt ?? null,
        rowCount: 0,
        detail,
      });
    }
  }
  return { results, states };
}

type HostRows = Pick<RemoteCache, 'rows' | 'srows' | 'fingerprints'>;

/** Local workers may finish after the remote scan; re-elect before publishing. */
function mirrorReconciler(
  parts: Map<AnalyticsRemoteHost, HostRows>,
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>,
  states: AnalyticsRemoteSourceState[],
  inventory: () => ClaudeLogNames
): () => void {
  const baseline = states.map((state) => ({ ...state }));
  return () => {
    states.splice(0, states.length, ...baseline.map((state) => ({ ...state })));
    selectClaudeMirrors(parts, results, states, inventory());
    states.sort((a, b) => a.tool.localeCompare(b.tool) || a.host.localeCompare(b.host));
  };
}

/**
 * Caches always retain RAW mirror rows. Select only after every host has settled,
 * including cache fallbacks, so scan order cannot change totals. Hashing basenames
 * matches session and agent logs without exporting private paths. Re-selecting on
 * every read (including cache-only reads) makes a pruned original's mirror count
 * immediately, even when the helper's mirror fingerprint has not changed.
 */
function selectClaudeMirrors(
  parts: Map<AnalyticsRemoteHost, HostRows>,
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>,
  states: AnalyticsRemoteSourceState[],
  local: ClaudeLogNames
): void {
  const originals = new Set(local.originals);
  for (const part of parts.values())
    for (const print of Object.values(part.fingerprints.claude ?? {}))
      if (print.claudeName && print.claudeMirror === false) originals.add(print.claudeName);
  const owners = new Map<string, AnalyticsRemoteHost>();
  // Fixed host order elects one orphan mirror; network completion order never wins.
  for (const host of REMOTE_ANALYTICS_HOSTS) {
    const part = parts.get(host);
    if (!part) continue;
    for (const row of part.rows.filter((row) => row.k === 'claude')) {
      const print = part.fingerprints.claude?.[row.f];
      if (print?.claudeName && print.claudeMirror && !owners.has(print.claudeName))
        owners.set(print.claudeName, host);
    }
  }
  for (let index = results.length - 1; index >= 0; index--)
    if (results[index].tool === 'claude') results.splice(index, 1);
  for (const [host, part] of parts) {
    const keep = (row: { f: string }): boolean => {
      const print = part.fingerprints.claude?.[row.f];
      return (
        !print?.claudeMirror ||
        !print.claudeName ||
        (!originals.has(print.claudeName) && owners.get(print.claudeName) === host)
      );
    };
    const rows = part.rows.filter((row) => row.k === 'claude' && keep(row));
    const srows = part.srows.filter((row) => row.k === 'claude' && keep(row));
    if (rows.length || srows.length)
      results.push({ tool: 'claude', data: toWorkerResult(rows, srows, 'claude', host) });
    const state = states.find((state) => state.host === host && state.tool === 'claude');
    if (!state) continue;
    state.rowCount = rows.reduce((sum, row) => sum + Math.floor(row.n), 0);
    const suppressed = part.rows.some((row) => row.k === 'claude' && !keep(row));
    if (state.state === 'ok' && !rows.length && !srows.length) state.state = 'no_usage';
    if (suppressed)
      state.detail = [
        state.detail,
        'SSH mirror logs already counted on another computer are excluded',
      ]
        .filter(Boolean)
        .join('; ');
    if (!local.complete && state.state !== 'not_installed' && state.state !== 'unavailable') {
      state.state = rows.length ? 'cached' : 'scanning';
      state.detail =
        'the local Claude log inventory is incomplete; SSH mirror selection is partial';
    }
  }
}

/**
 * The remote aggregates saved by the last scans, without contacting a host:
 * what the page shows while a remote scan has not answered in time.
 */
export function loadAnalyticsRemoteCachedSources(
  minDateMs: number,
  deps: Pick<AnalyticsRemoteSourceDeps, 'cacheDir' | 'claudeLogNames'> = {}
): {
  fetchedAt: string | undefined;
  reconcileClaudeMirrors: () => void;
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
} {
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  const results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  let fetchedAt: string | undefined;
  const parts = new Map<AnalyticsRemoteHost, HostRows>();
  for (const host of REMOTE_ANALYTICS_HOSTS) {
    const cached = loadCache(cacheFile(cacheDir, host));
    if (cached.lastScanAt && (!fetchedAt || cached.lastScanAt > fetchedAt))
      fetchedAt = cached.lastScanAt;
    parts.set(host, {
      ...cached,
      rows: cached.rows.filter((row) => inWindow(row, minDateMs)),
      srows: cached.srows.filter((row) => sessionInWindow(row, minDateMs)),
    });
    const part = cachedHostSources(host, cached, minDateMs, 'remote scan timed out');
    results.push(...part.results);
    states.push(...part.states);
  }
  const reconcileClaudeMirrors = mirrorReconciler(
    parts,
    results,
    states,
    () => deps.claudeLogNames?.() ?? localClaudeLogNames(cacheDir, minDateMs)
  );
  reconcileClaudeMirrors();
  return { results, states, fetchedAt, reconcileClaudeMirrors };
}

/**
 * Collect remote usage over ssh, one helper invocation per host. A timeout or
 * failure leaves that source `cached` (previous aggregates) or `unavailable`,
 * never failing the page.
 */
/**
 * Saved extra usage-log locations for one remote host, per scanned kind.
 * Only scanned kinds' extras travel (Settings names Claude Code's tool
 * `claude-code`); a prefs read failure scans the built-in roots alone.
 */
export function remoteExtraRoots(
  host: AnalyticsRemoteHost,
  kinds: AnalyticsRemoteKind[]
): Partial<Record<AnalyticsRemoteKind, string[]>> {
  const extra: Partial<Record<AnalyticsRemoteKind, string[]>> = {};
  let sources: ReturnType<typeof readDashboardPreferences>['usageLogSources'] = [];
  try {
    sources = readDashboardPreferences().usageLogSources;
  } catch {
    return extra;
  }
  for (const source of sources) {
    if (source.host !== host) continue;
    const kind: AnalyticsRemoteKind | null =
      source.tool === 'claude-code'
        ? 'claude'
        : source.tool === 'codex' ||
            source.tool === 'omp' ||
            source.tool === 'muse' ||
            source.tool === 'zcode'
          ? source.tool
          : null;
    if (!kind || !kinds.includes(kind)) continue;
    const paths = extra[kind] ?? [];
    if (paths.length >= 16 || paths.includes(source.path)) continue;
    paths.push(source.path);
    extra[kind] = paths;
  }
  return extra;
}

/**
 * Give the server's event loop a turn between the steps of merging a host's answer (parsing,
 * filtering, saving and converting a large host each take tens of milliseconds), so dashboard
 * and analytics requests are answered in between instead of after one long stall.
 */
const yieldTurn = (): Promise<void> => new Promise((resolve) => setImmediate(resolve));

/** Helper calls one host runs at once: one per kind, so every kind at once. */
const REMOTE_KIND_CONCURRENCY = 6;

type RemoteHelper = NonNullable<AnalyticsRemoteSourceDeps['runHelper']>;

/** What one host's helper calls said when they failed: the first cause, and each failed kind's. */
interface ScanFailure {
  damaged: boolean;
  cause?: AnalyticsScanFailureCause;
  exitCode?: number | null;
  byKind: Map<AnalyticsRemoteKind, AnalyticsScanFailureCause>;
}

/**
 * Why a host's helper calls failed, when every one did. A damaged packaged helper fails before any
 * ssh session, on every host alike, so it is named instead of reading as a host that did not answer.
 */
const HELPER_DAMAGED =
  'the packaged analytics helper failed its integrity check, so no computer was scanned; reinstall AI Account Center';

/**
 * One host's scan. A host still catching up (a cold cache, or a last scan that a kind's size cut
 * short) is scanned with one helper call per kind, all at once: each kind gets the helper's whole
 * time budget, so a big Claude Code history no longer leaves the kinds after it unread until a
 * later scan. A settled host, whose scan takes a couple of seconds, gets one call for every kind,
 * so a quiet host is not asked for five ssh sessions a minute. Each call carries only its kind's
 * fingerprints; the saved extra roots travel whole (the helper reads only its own kinds').
 *
 * The answers merge into one response holding each call's own kinds only. `cutKinds` are the kinds
 * whose call stopped early (a per-kind `truncated`), `failedKinds` those whose call failed: they
 * read as a kind the host could not read, keeping their earlier rows. Null when every call failed.
 */
async function scanHost(
  runHelper: RemoteHelper,
  alias: string,
  host: AnalyticsRemoteHost,
  kinds: AnalyticsRemoteKind[],
  cached: RemoteCache,
  extraRoots: Partial<Record<AnalyticsRemoteKind, string[]>>,
  minDateMs: number,
  failure: ScanFailure
): Promise<{
  response: AnalyticsRemoteResponse;
  cutKinds: Set<AnalyticsRemoteKind>;
  failedKinds: Set<AnalyticsRemoteKind>;
  olderClaude: boolean;
} | null> {
  const groups = cached.settled === true ? [kinds] : kinds.map((kind) => [kind]);
  const answers = new Map<AnalyticsRemoteKind[], AnalyticsRemoteResponse | null>();
  await runBounded([groups], REMOTE_KIND_CONCURRENCY, async (group) => {
    const fingerprints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {};
    for (const kind of group)
      if (cached.fingerprints[kind]) fingerprints[kind] = cached.fingerprints[kind];
    try {
      answers.set(
        group,
        await runHelper(alias, host, { kinds: group, minDateMs, fingerprints, extraRoots })
      );
    } catch (error) {
      if (error instanceof AnalyticsHelperIntegrityError) failure.damaged = true;
      const cause: AnalyticsScanFailureCause =
        error instanceof AnalyticsRemoteTransportError ? error.cause : 'helper_failed';
      failure.cause ??= cause;
      if (failure.exitCode === undefined)
        failure.exitCode = error instanceof AnalyticsRemoteTransportError ? error.exitCode : null;
      for (const kind of group) failure.byKind.set(kind, cause);
      answers.set(group, null);
    }
  });
  const merged: AnalyticsRemoteResponse = {
    version: 2,
    truncated: false,
    kinds: {} as AnalyticsRemoteResponse['kinds'],
    rows: [],
    srows: [],
  };
  const cutKinds = new Set<AnalyticsRemoteKind>();
  const failedKinds = new Set<AnalyticsRemoteKind>();
  let olderClaude = false;
  for (const group of groups) {
    const answer = answers.get(group) ?? null;
    for (const kind of group) {
      if (!answer) {
        failedKinds.add(kind);
        merged.kinds[kind] = { state: 'error', fingerprints: {} };
        continue;
      }
      // An omitted requested kind never confirms that its earlier rows disappeared.
      const entry: AnalyticsRemoteKindResult | undefined = answer.kinds[kind];
      merged.kinds[kind] = entry ?? { state: 'error', fingerprints: {} };
      if (kind === 'claude' && entry && !hasClaudeReplacementMetadata(answer)) olderClaude = true;
      if (answer.truncated) {
        cutKinds.add(kind);
        merged.truncated = true;
      }
      if (kind === 'omp' && answer.discoveryTruncated === true) merged.discoveryTruncated = true;
      merged.rows.push(...answer.rows.filter((row) => row.k === kind));
      merged.srows.push(...(answer.srows ?? []).filter((row) => row.k === kind));
    }
  }
  return failedKinds.size === kinds.length
    ? null
    : { response: merged, cutKinds, failedKinds, olderClaude };
}

export async function loadAnalyticsRemoteSources(
  minDateMs: number,
  deps: AnalyticsRemoteSourceDeps = {}
): Promise<{
  fetchedAt: string;
  reconcileClaudeMirrors: () => void;
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
}> {
  const now = deps.now ?? Date.now;
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  const hosts = await (deps.hosts ?? resolveAnalyticsRemoteHosts)().catch(() =>
    fixedAnalyticsRemoteAliases()
  );
  const runHelper = deps.runHelper ?? runAnalyticsRemoteHelper;
  const results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  const jobs: Array<Promise<void>> = [];
  const parts = new Map<AnalyticsRemoteHost, HostRows>();
  for (const host of REMOTE_ANALYTICS_HOSTS) {
    jobs.push(
      (async (): Promise<void> => {
        const allKinds = REMOTE_TARGETS[host];
        const file = cacheFile(cacheDir, host);
        const cached = loadCache(file);
        // A successful disk answer remains authoritative across process restarts.
        // Each kind has its own last scan; one recent kind cannot hide an old one.
        const recentKinds = allKinds.filter((kind) => {
          const read = cached.kinds?.[kind];
          const age = now() - Date.parse(read?.lastScanAt ?? '');
          return (
            !deps.force &&
            read &&
            ['ok', 'no_usage', 'not_installed'].includes(read.state) &&
            age >= 0 &&
            age < 5 * 60_000
          );
        });
        const kinds = allKinds.filter((kind) => !recentKinds.includes(kind));
        results.push(
          ...cachedHostSources(host, cached, minDateMs, 'cached remote scan').results.filter(
            (source) => recentKinds.includes(source.tool)
          )
        );
        states.push(
          ...cachedHostSources(host, cached, minDateMs, 'cached remote scan').states.filter(
            (source) => recentKinds.includes(source.tool)
          )
        );
        if (kinds.length === 0) {
          parts.set(host, {
            ...cached,
            rows: cached.rows.filter((row) => inWindow(row, minDateMs)),
            srows: cached.srows.filter((row) => sessionInWindow(row, minDateMs)),
          });
          return;
        }
        const alias = hosts[host];
        if (!alias) {
          for (const tool of kinds)
            states.push({
              tool,
              host,
              state: 'unavailable',
              lastScanAt: cached.lastScanAt,
              rowCount: 0,
              detail: 'remote host is not configured',
            });
          return;
        }
        const onHostScan = deps.onHostScan;
        onHostScan?.(host, 'start');
        const failure: ScanFailure = { damaged: false, byKind: new Map() };
        const scan = await scanHost(
          runHelper,
          alias,
          host,
          kinds,
          cached,
          remoteExtraRoots(host, kinds),
          minDateMs,
          failure
        );
        if (!scan) {
          parts.set(host, {
            ...cached,
            rows: cached.rows.filter((row) => inWindow(row, minDateMs)),
            srows: cached.srows.filter((row) => sessionInWindow(row, minDateMs)),
          });
          onHostScan?.(host, 'done');
          const lastFailure: AnalyticsHostFailure = {
            cause: failure.damaged ? 'helper_damaged' : (failure.cause ?? 'helper_failed'),
            exitCode: failure.exitCode ?? null,
            at: new Date(now()).toISOString(),
            kinds: [...kinds],
          };
          // Saved with the host, rows and prints untouched, so the failure is named until a scan
          // answers again, also after a restart. A damaged package fails every host alike and is
          // named by the page without touching any host's cache.
          if (!failure.damaged)
            try {
              saveCache(file, { ...cached, lastFailure });
            } catch {
              /* Cache writes are best-effort. */
            }
          // Previous aggregates stay available; nothing remote fails the page.
          const part = cachedHostSources(
            host,
            cached,
            minDateMs,
            failure.damaged ? HELPER_DAMAGED : 'remote scan failed',
            failure.damaged ? undefined : lastFailure
          );
          results.push(...part.results.filter((source) => !recentKinds.includes(source.tool)));
          states.push(...part.states.filter((source) => !recentKinds.includes(source.tool)));
          return;
        }
        const { response, cutKinds, failedKinds, olderClaude } = scan;
        await yieldTurn();
        const errored = new Set(kinds.filter((tool) => response.kinds[tool]?.state === 'error'));
        // Files a scan did not reach keep their rows and prints until a scan
        // reaches them: after a kind's scan was cut, a kind that could not be
        // read, or (OMP) a root search that hit its bounds.
        const keepsUnvisited = (tool: AnalyticsRemoteKind): boolean =>
          (tool === 'claude' && olderClaude) ||
          cutKinds.has(tool) ||
          (tool === 'codex' && response.kinds.codex?.partial === true) ||
          errored.has(tool) ||
          response.kinds[tool]?.state === 'pending' ||
          response.kinds[tool]?.partial === true ||
          (tool === 'antigravity' && response.kinds[tool]?.unreadable === true) ||
          (tool === 'omp' && response.discoveryTruncated === true);
        const freshPrints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {};
        const discarded = (tool: AnalyticsRemoteKind) =>
          // Reject the whole Claude kind, even a seemingly complete old scan:
          // file-only answers cannot prove which cached copy owners to replace.
          // Retaining rows, prints and their date is safer than inferring a rebuild.
          (tool === 'claude' && olderClaude) ||
          errored.has(tool) ||
          response.kinds[tool]?.state === 'pending' ||
          (tool === 'antigravity' && response.kinds[tool]?.unreadable === true);
        // Rows and prints are adopted together. A failed kind retries every changed database.
        for (const tool of kinds)
          freshPrints[tool] = discarded(tool) ? {} : (response.kinds[tool]?.fingerprints ?? {});
        // Fully read groups explicitly replace all current and previous owners.
        // Emitted rows also replace their own file even if its print is unchanged.
        const replaced = new Map(kinds.map((tool) => [tool, new Set<string>()]));
        if (!discarded('claude'))
          for (const key of response.kinds.claude?.replacedFiles ?? [])
            replaced.get('claude')?.add(key);
        for (const row of [...response.rows, ...(response.srows ?? [])])
          if (!discarded(row.k)) replaced.get(row.k)?.add(row.f);
        const kept = cached.rows.filter((row) => {
          if (!inWindow(row, minDateMs)) return false;
          if (!kinds.includes(row.k)) return true;
          if (replaced.get(row.k)?.has(row.f)) return false;
          const print = freshPrints[row.k]?.[row.f];
          if (print === undefined) return keepsUnvisited(row.k);
          return samePrint(print, cached.fingerprints[row.k]?.[row.f]);
        });
        const fresh = response.rows.filter(
          (row) => kinds.includes(row.k) && !discarded(row.k) && inWindow(row, minDateMs)
        );
        const merged = [...kept, ...fresh].slice(0, MAX_CACHED_ROWS);
        // Session aggregates merge under the same file fingerprints as the
        // hourly rows: a re-read file replaces both, an unvisited one keeps both.
        const keptSessions = cached.srows.filter((row) => {
          if (!sessionInWindow(row, minDateMs)) return false;
          if (!kinds.includes(row.k)) return true;
          if (replaced.get(row.k)?.has(row.f)) return false;
          const print = freshPrints[row.k]?.[row.f];
          if (print === undefined) return keepsUnvisited(row.k);
          return samePrint(print, cached.fingerprints[row.k]?.[row.f]);
        });
        const freshSessions = (response.srows ?? []).filter(
          (row) => kinds.includes(row.k) && !discarded(row.k) && sessionInWindow(row, minDateMs)
        );
        const mergedSessions = [...keptSessions, ...freshSessions].slice(0, MAX_CACHED_ROWS);
        const scannedAt = new Date(now()).toISOString();
        const kindReads = { ...cached.kinds };
        for (const tool of kinds) {
          const kind = response.kinds[tool];
          if (!kind || discarded(tool)) continue;
          const partial =
            cutKinds.has(tool) || kind.partial || (tool === 'omp' && response.discoveryTruncated);
          if (partial && Object.keys(freshPrints[tool] ?? {}).length === 0) continue;
          kindReads[tool] = {
            lastScanAt: scannedAt,
            state: partial
              ? 'cached'
              : kind.state === 'not_installed'
                ? 'not_installed'
                : merged.some((row) => row.k === tool)
                  ? 'ok'
                  : 'no_usage',
          };
        }
        const prints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {
          ...freshPrints,
        };
        for (const [kind, entries] of Object.entries(cached.fingerprints)) {
          if (!kinds.includes(kind as AnalyticsRemoteKind)) {
            prints[kind] = entries;
            continue;
          }
          if (!keepsUnvisited(kind as AnalyticsRemoteKind)) continue;
          // Keep group metadata for unreadable members even after their rows
          // were replaced, so renamed aliases still join on the next scan.
          for (const [key, print] of Object.entries(entries)) {
            prints[kind] ??= {};
            prints[kind][key] ??= print;
          }
        }
        await yieldTurn();
        parts.set(host, { rows: merged, srows: mergedSessions, fingerprints: prints });
        try {
          saveCache(file, {
            version: CACHE_VERSION,
            fingerprints: prints,
            rows: merged,
            srows: mergedSessions,
            lastScanAt: scannedAt,
            kinds: kindReads,
            // A kind whose call failed keeps the failure on record; a full answer clears it.
            ...(failedKinds.size
              ? {
                  lastFailure: {
                    cause: failure.cause ?? 'helper_failed',
                    exitCode: failure.exitCode ?? null,
                    at: scannedAt,
                    kinds: [...failedKinds],
                  },
                }
              : {}),
            settled:
              !olderClaude &&
              cutKinds.size === 0 &&
              failedKinds.size === 0 &&
              kinds.every(
                (tool) =>
                  response.kinds[tool]?.state !== 'pending' &&
                  response.kinds[tool]?.state !== 'error' &&
                  response.kinds[tool]?.partial !== true &&
                  !(tool === 'antigravity' && response.kinds[tool]?.unreadable)
              ),
          });
        } catch {
          /* Cache writes are best-effort; the rows are still served. */
        }
        for (const tool of kinds) {
          await yieldTurn();
          const rows = merged.filter((row) => row.k === tool);
          const srows = mergedSessions.filter((row) => row.k === tool);
          const kind = response.kinds[tool];
          const kindState = kind?.state ?? 'error';
          if (tool === 'claude' && olderClaude) {
            if (rows.length || srows.length)
              results.push({ tool, data: toWorkerResult(rows, srows, tool, host) });
            states.push({
              tool,
              host,
              state: rows.length || srows.length ? 'cached' : 'scanning',
              lastScanAt: cached.kinds?.[tool]?.lastScanAt ?? null,
              rowCount: rows.reduce((sum, row) => sum + Math.floor(row.n), 0),
              detail:
                rows.length || srows.length
                  ? 'the computer answered with an older reader; showing previously read usage'
                  : 'the computer answered with an older reader; waiting for a compatible reader',
            });
            continue;
          }
          if (kindState === 'not_installed') {
            states.push({
              tool,
              host,
              state: 'not_installed',
              lastScanAt: scannedAt,
              rowCount: 0,
              detail: 'no usage logs found on this host',
            });
            continue;
          }
          if (rows.length || srows.length)
            results.push({ tool, data: toWorkerResult(rows, srows, tool, host) });
          const events = rows.reduce((sum, row) => sum + Math.floor(row.n), 0);
          const kept = rows.length > 0;
          if (kindState === 'pending') {
            // The scan ran out of time before it reached this tool. That is a
            // step of a cold start, never a failure: the next scan continues
            // from the saved fingerprints, so the cell says "scanning".
            states.push({
              tool,
              host,
              state: 'scanning',
              lastScanAt: cached.kinds?.[tool]?.lastScanAt ?? null,
              rowCount: events,
              detail: kept
                ? 'the scan ran out of time before it reached this tool again; records from earlier scans stay included'
                : 'the scan ran out of time before it reached this tool; the next scan continues',
            });
            continue;
          }
          if (kindState === 'error') {
            const scanFailed = failedKinds.has(tool);
            const cause = scanFailed
              ? (failure.byKind.get(tool) ?? 'helper_failed')
              : 'read_failed';
            const why = scanFailed
              ? scanFailureText(cause)
              : 'its logs were found but could not be read';
            states.push({
              tool,
              host,
              // A read that failed with earlier records in hand is a cached result; with nothing to
              // show it is a scan that failed, or logs found and not readable. Never a zero.
              state: kept ? 'cached' : scanFailed ? 'scan_failed' : 'unreadable',
              lastScanAt: cached.kinds?.[tool]?.lastScanAt ?? null,
              rowCount: events,
              detail: kept ? `${why}; showing previously read aggregates` : why,
              cause,
            });
            continue;
          }
          if (kind?.partial === true || (tool === 'antigravity' && kind?.unreadable)) {
            // Incomplete scans retain earlier usage. Only confirmed files
            // advance the read date; failures and bounds have distinct details.
            const oversized = Object.values(freshPrints[tool] ?? {}).some(
              (print) => print.usageSkipped === true
            );
            states.push({
              tool,
              host,
              state: kept ? 'cached' : 'scanning',
              lastScanAt:
                tool === 'antigravity' && kind?.unreadable
                  ? (cached.kinds?.[tool]?.lastScanAt ?? null)
                  : (kindReads[tool]?.lastScanAt ?? null),
              rowCount: events,
              detail:
                tool === 'antigravity' && kind?.unreadable
                  ? 'some Antigravity conversation databases could not be read (busy or damaged); showing previously read aggregates'
                  : tool === 'codex' && kind?.compressedUnread
                    ? `${kind.compressedUnread} compressed Codex logs could not be read on this computer (needs Python 3.14 or the zstd tool); ${kept ? 'showing previously read aggregates' : 'waiting for readable logs'}`
                    : kind?.readFailures
                      ? `${kind.readFailures} logs could not be read on this computer; ${kept ? 'showing previously read usage' : 'waiting for readable logs'}`
                      : oversized
                        ? 'oversized log lines may contain usage that could not be read; showing partial aggregates'
                        : kept
                          ? 'the remote scan hit its time bound partway through this tool; showing partial aggregates'
                          : 'the remote scan hit its time bound partway through this tool; the next scan continues',
            });
            continue;
          }
          states.push({
            tool,
            host,
            state: kept ? 'ok' : 'no_usage',
            lastScanAt: scannedAt,
            rowCount: events,
            detail:
              tool === 'omp' && response.discoveryTruncated === true
                ? 'the search for custom OMP session folders hit its bounds; folders it did not reach are not read'
                : tool === 'zcode' && response.kinds.zcode?.walUnread === true
                  ? "zcode's newest usage waits in its write-ahead log, which a read-only open here cannot read; it appears once zcode checkpoints it"
                  : tool === 'antigravity' && response.kinds.antigravity?.unreadable === true
                    ? 'some Antigravity conversation databases could not be read (busy or damaged); the rest are counted and the next scan reads them again'
                    : kept
                      ? null
                      : Object.keys(freshPrints[tool] ?? {}).length > 0
                        ? 'usage logs found but no usage in the last 31 days'
                        : 'no usage recorded in the last 31 days',
          });
        }
        onHostScan?.(host, 'done');
      })()
    );
  }
  await Promise.all(jobs);
  const reconcileClaudeMirrors = mirrorReconciler(
    parts,
    results,
    states,
    () => deps.claudeLogNames?.() ?? localClaudeLogNames(cacheDir, minDateMs)
  );
  reconcileClaudeMirrors();
  return { results, states, fetchedAt: new Date(now()).toISOString(), reconcileClaudeMirrors };
}
