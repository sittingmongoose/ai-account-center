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
  runAnalyticsRemoteHelper,
  resolveAnalyticsRemoteHosts,
  type AnalyticsRemoteFingerprint,
  type AnalyticsRemoteHost,
  type AnalyticsRemoteKind,
  type AnalyticsRemoteKindResult,
  type AnalyticsRemoteRequest,
  type AnalyticsRemoteResponse,
  type AnalyticsRemoteRow,
  type AnalyticsRemoteSessionRow,
} from './analytics-remote-transport';
import { readDashboardPreferences } from './dashboard-preferences';
import { runBounded } from '../usage/collector-concurrency';

export type AnalyticsSourceTool = 'claude' | 'codex' | 'omp' | 'muse' | 'zcode';
export type AnalyticsSourceState =
  | 'ok'
  | 'cached'
  | 'unavailable'
  | 'not_installed'
  /** The scan is still working on this tool: it is running now, or ran out of time before it finished. */
  | 'scanning'
  /** The tool was scanned and its logs hold no usage inside the 31-day window. */
  | 'no_usage';

export interface AnalyticsRemoteSourceState {
  tool: AnalyticsSourceTool;
  host: AnalyticsRemoteHost;
  state: AnalyticsSourceState;
  lastScanAt: string | null;
  rowCount: number;
  detail: string | null;
}

export interface AnalyticsRemoteSourceDeps {
  hosts?: () => Promise<{ mac: string | null; windows: string | null }>;
  runHelper?: (
    sshHost: string,
    platform: AnalyticsRemoteHost,
    request: AnalyticsRemoteRequest
  ) => Promise<AnalyticsRemoteResponse>;
  now?: () => number;
  cacheDir?: string;
  /**
   * Fires when a host's scan starts and when it settles (success or failure),
   * so callers can report which remotes a refresh is waiting on.
   */
  onHostScan?: (host: AnalyticsRemoteHost, phase: 'start' | 'done') => void;
}

/**
 * Remote coverage: Claude Code, Codex, OMP, Muse and zcode on the Mac and
 * Windows. Muse and zcode report `not_installed` on Windows until they are;
 * the states are measured by the scan, never assumed.
 */
const REMOTE_TARGETS: Record<AnalyticsRemoteHost, AnalyticsRemoteKind[]> = {
  mac: ['claude', 'codex', 'omp', 'muse', 'zcode'],
  windows: ['claude', 'codex', 'omp', 'muse', 'zcode'],
};

/** The kinds scanned on one remote host, for states when no scan answered. */
export function analyticsRemoteTargets(host: AnalyticsRemoteHost): AnalyticsRemoteKind[] {
  return [...REMOTE_TARGETS[host]];
}

const MAX_CACHED_ROWS = 100_000;

/**
 * 4: Claude counts one API response, not one content block; rows cached by
 * version 3 are read again. (3: per-session aggregates ride with the hourly
 * rows, so remote sessions count like local ones.)
 */
const CACHE_VERSION = 4;

interface RemoteCache {
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
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as RemoteCache;
    if (
      value.version !== CACHE_VERSION ||
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
    return value;
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
 * and Codex readers leave the provider unset, so the tool's own provider prices the row
 * (`anthropic`, `openai`); the other readers record an empty route, which prices by model name
 * alone. A Codex row priced by model name alone found no rate for a model several providers list
 * at different prices, and read as "not logged".
 */
function remoteRowProvider(
  kind: AnalyticsRemoteKind,
  route: string | undefined
): { provider?: string } {
  if (route) return { provider: route };
  return kind === 'claude' || kind === 'codex' ? {} : { provider: '' };
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
  kind: AnalyticsRemoteKind
): UsageWorkerResult {
  const { hourly } = aggregateRows(toCompact(rows), `${kind}-remote`, kind);
  // Sessions aggregate from the session rows alone: the hourly rows already
  // carry the same tokens, so the two must never be aggregated together.
  const { session } = aggregateSessionAggregates(toSessionAggregates(srows), `${kind}-remote`);
  return {
    daily: [],
    monthly: [],
    hourly,
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

/** Previously read aggregates of one host, marked `cached`, or `unavailable` when none. */
function cachedHostSources(
  host: AnalyticsRemoteHost,
  cached: RemoteCache,
  minDateMs: number,
  detail: string
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
    if (kept.length || keptSessions.length) {
      results.push({ tool, data: toWorkerResult(kept, keptSessions, tool) });
      states.push({
        tool,
        host,
        state: 'cached',
        lastScanAt: cached.lastScanAt,
        rowCount: kept.reduce((sum, row) => sum + Math.floor(row.n), 0),
        detail: `${detail}; showing previously read aggregates`,
      });
    } else {
      states.push({
        tool,
        host,
        state: 'unavailable',
        lastScanAt: cached.lastScanAt,
        rowCount: 0,
        detail,
      });
    }
  }
  return { results, states };
}

/**
 * The remote aggregates saved by the last scans, without contacting a host:
 * what the page shows while a remote scan has not answered in time.
 */
export function loadAnalyticsRemoteCachedSources(
  minDateMs: number,
  deps: Pick<AnalyticsRemoteSourceDeps, 'cacheDir'> = {}
): {
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
} {
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  const results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  for (const host of ['mac', 'windows'] as const) {
    const part = cachedHostSources(
      host,
      loadCache(cacheFile(cacheDir, host)),
      minDateMs,
      'remote scan timed out'
    );
    results.push(...part.results);
    states.push(...part.states);
  }
  states.sort((a, b) => a.tool.localeCompare(b.tool) || a.host.localeCompare(b.host));
  return { results, states };
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
const REMOTE_KIND_CONCURRENCY = 5;

type RemoteHelper = NonNullable<AnalyticsRemoteSourceDeps['runHelper']>;

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
  minDateMs: number
): Promise<{
  response: AnalyticsRemoteResponse;
  cutKinds: Set<AnalyticsRemoteKind>;
  failedKinds: Set<AnalyticsRemoteKind>;
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
    } catch {
      answers.set(group, null);
    }
  });
  const merged: AnalyticsRemoteResponse = {
    version: 1,
    truncated: false,
    kinds: {} as AnalyticsRemoteResponse['kinds'],
    rows: [],
    srows: [],
  };
  const cutKinds = new Set<AnalyticsRemoteKind>();
  const failedKinds = new Set<AnalyticsRemoteKind>();
  for (const group of groups) {
    const answer = answers.get(group) ?? null;
    for (const kind of group) {
      if (!answer) {
        failedKinds.add(kind);
        merged.kinds[kind] = { state: 'error', fingerprints: {} };
        continue;
      }
      // A host's answer may leave a kind out; it then reads as scanned with nothing to report.
      const entry: AnalyticsRemoteKindResult | undefined = answer.kinds[kind];
      if (entry) merged.kinds[kind] = entry;
      if (answer.truncated) {
        cutKinds.add(kind);
        merged.truncated = true;
      }
      if (kind === 'omp' && answer.discoveryTruncated === true) merged.discoveryTruncated = true;
      merged.rows.push(...answer.rows.filter((row) => row.k === kind));
      merged.srows.push(...(answer.srows ?? []).filter((row) => row.k === kind));
    }
  }
  return failedKinds.size === kinds.length ? null : { response: merged, cutKinds, failedKinds };
}

export async function loadAnalyticsRemoteSources(
  minDateMs: number,
  deps: AnalyticsRemoteSourceDeps = {}
): Promise<{
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
}> {
  const now = deps.now ?? Date.now;
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  const hosts = await (deps.hosts ?? resolveAnalyticsRemoteHosts)().catch(() => ({
    mac: null as string | null,
    windows: null as string | null,
  }));
  const runHelper = deps.runHelper ?? runAnalyticsRemoteHelper;
  const results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  const jobs: Array<Promise<void>> = [];
  for (const host of ['mac', 'windows'] as const) {
    jobs.push(
      (async (): Promise<void> => {
        const kinds = REMOTE_TARGETS[host];
        const file = cacheFile(cacheDir, host);
        const cached = loadCache(file);
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
        const scan = await scanHost(
          runHelper,
          alias,
          host,
          kinds,
          cached,
          remoteExtraRoots(host, kinds),
          minDateMs
        );
        if (!scan) {
          onHostScan?.(host, 'done');
          // Previous aggregates stay available; nothing remote fails the page.
          const part = cachedHostSources(host, cached, minDateMs, 'remote scan failed');
          results.push(...part.results);
          states.push(...part.states);
          return;
        }
        const { response, cutKinds, failedKinds } = scan;
        await yieldTurn();
        const errored = new Set(kinds.filter((tool) => response.kinds[tool]?.state === 'error'));
        // Files a scan did not reach keep their rows and prints until a scan
        // reaches them: after a kind's scan was cut, a kind that could not be
        // read, or (OMP) a root search that hit its bounds.
        const keepsUnvisited = (tool: AnalyticsRemoteKind): boolean =>
          cutKinds.has(tool) ||
          errored.has(tool) ||
          (tool === 'omp' && response.discoveryTruncated === true);
        const freshPrints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {};
        for (const tool of kinds) freshPrints[tool] = response.kinds[tool]?.fingerprints ?? {};
        const kept = cached.rows.filter((row) => {
          if (!kinds.includes(row.k) || !inWindow(row, minDateMs)) return false;
          const print = freshPrints[row.k]?.[row.f];
          if (print === undefined) return keepsUnvisited(row.k);
          return samePrint(print, cached.fingerprints[row.k]?.[row.f]);
        });
        const fresh = response.rows.filter(
          (row) => kinds.includes(row.k) && !errored.has(row.k) && inWindow(row, minDateMs)
        );
        const merged = [...kept, ...fresh].slice(0, MAX_CACHED_ROWS);
        // Session aggregates merge under the same file fingerprints as the
        // hourly rows: a re-read file replaces both, an unvisited one keeps both.
        const keptSessions = cached.srows.filter((row) => {
          if (!kinds.includes(row.k) || !sessionInWindow(row, minDateMs)) return false;
          const print = freshPrints[row.k]?.[row.f];
          if (print === undefined) return keepsUnvisited(row.k);
          return samePrint(print, cached.fingerprints[row.k]?.[row.f]);
        });
        const freshSessions = (response.srows ?? []).filter(
          (row) => kinds.includes(row.k) && !errored.has(row.k) && sessionInWindow(row, minDateMs)
        );
        const mergedSessions = [...keptSessions, ...freshSessions].slice(0, MAX_CACHED_ROWS);
        const scannedAt = new Date(now()).toISOString();
        const prints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {
          ...freshPrints,
        };
        for (const [kind, entries] of Object.entries(cached.fingerprints)) {
          if (!kinds.includes(kind as AnalyticsRemoteKind)) continue;
          if (!keepsUnvisited(kind as AnalyticsRemoteKind)) continue;
          for (const [key, print] of Object.entries(entries)) {
            prints[kind] ??= {};
            prints[kind][key] ??= print;
          }
        }
        await yieldTurn();
        try {
          saveCache(file, {
            version: CACHE_VERSION,
            fingerprints: prints,
            rows: merged,
            srows: mergedSessions,
            lastScanAt: scannedAt,
            settled:
              cutKinds.size === 0 &&
              failedKinds.size === 0 &&
              kinds.every(
                (tool) =>
                  response.kinds[tool]?.state !== 'pending' &&
                  response.kinds[tool]?.state !== 'error' &&
                  response.kinds[tool]?.partial !== true
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
          const kindState = kind?.state ?? 'ok';
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
            results.push({ tool, data: toWorkerResult(rows, srows, tool) });
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
              lastScanAt: scannedAt,
              rowCount: events,
              detail: kept
                ? 'the scan ran out of time before it reached this tool again; records from earlier scans stay included'
                : 'the scan ran out of time before it reached this tool; the next scan continues',
            });
            continue;
          }
          if (kindState === 'error') {
            states.push({
              tool,
              host,
              // A read that failed with earlier records in hand is a cached
              // result; with nothing to show it is a real failure.
              state: kept ? 'cached' : 'unavailable',
              lastScanAt: scannedAt,
              rowCount: events,
              detail: failedKinds.has(tool)
                ? kept
                  ? 'remote scan failed; showing previously read aggregates'
                  : 'remote scan failed'
                : kept
                  ? 'remote read failed; showing previously read aggregates'
                  : 'remote read failed',
            });
            continue;
          }
          if (kind?.partial === true) {
            // A cap or the deadline cut this tool's own scan short: partial
            // numbers are shown until a later scan finishes it, and with
            // nothing read yet it is still scanning, not failed.
            states.push({
              tool,
              host,
              state: kept ? 'cached' : 'scanning',
              lastScanAt: scannedAt,
              rowCount: events,
              detail: kept
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
  states.sort((a, b) => a.tool.localeCompare(b.tool) || a.host.localeCompare(b.host));
  return { results, states };
}
