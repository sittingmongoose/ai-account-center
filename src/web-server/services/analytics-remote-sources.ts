import { ANALYTICS_TOOLS, type AnalyticsTool } from '../usage/analytics-tools';
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
  NO_READER_TOOLS,
  REMOTE_ANALYTICS_HOSTS,
  parseAnalyticsDiscovery,
  parseAnalyticsSessions,
  parseUnfinished,
  runAnalyticsRemoteScan,
  runLocalUsageScan,
  PYTHON_KINDS,
  localUsageScanReady,
  MAX_EXPERIMENT_ROOTS,
  fixedAnalyticsRemoteAliases,
  hasClaudeReplacementMetadata,
  resolveAnalyticsRemoteHosts,
  type AnalyticsDiscoveryKind,
  type AnalyticsDiscoveryRoot,
  type AnalyticsNoReaderTool,
  type AnalyticsPresenceTool,
  type AnalyticsReader,
  type AnalyticsRemoteAliases,
  type AnalyticsRemoteDiscovery,
  type AnalyticsRemoteFingerprint,
  type AnalyticsRemoteHost,
  type AnalyticsScanHost,
  type AnalyticsRemoteKind,
  type AnalyticsRemoteKindResult,
  type AnalyticsRemoteRequest,
  type AnalyticsRemoteResponse,
  type AnalyticsRemoteRow,
  type AnalyticsRemoteSessionRow,
  type AnalyticsUnfinishedLog,
  type AnalyticsScanFailureCause,
  type AnalyticsSessionEvidence,
} from './analytics-remote-transport';
import { readAnalyticsCaptureSettings } from './analytics-capture-settings';
import {
  loadClaudeMirrorOwners,
  mirrorOwnersFile,
  saveClaudeMirrorOwners,
} from './analytics-remote-mirror-owners';
import {
  MAX_REMOTE_CACHE_BYTES,
  fitRemoteCache,
  parseRemoteCacheDiscard,
  parseRemoteCacheTrim,
  remoteCacheNote,
  type RemoteCacheDiscard,
  type RemoteCacheDiscardCause,
  type RemoteCacheTrim,
} from './analytics-remote-cache-limit';
import { readDashboardPreferences } from './dashboard-preferences';
import { runBounded } from '../usage/collector-concurrency';
import { readExperimentRoots } from '../usage/experiment-usage-roots';
import { localClaudeLogNames, type ClaudeLogNames } from './analytics-claude-log-names';
import { cellsFromCompact } from '../usage/activity-cells';
import type { AccountAnalyticsActivityProvider } from './account-analytics-types';

export type AnalyticsSourceTool = AnalyticsTool | 'cursor' | 'devin' | AnalyticsNoReaderTool;
export type AnalyticsSourceState =
  | 'ok'
  /** Independent activity is newer than the newest measured usage by a scan cycle. */
  | 'stale'
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
  | 'scan_failed'
  /** The tool is installed here (its home, program or a T3 instance) but no log of it exists yet. */
  | 'installed_no_logs'
  /** The tool is here and Analytics has no usage reader for it (`cause` says why). */
  | 'no_reader';

/** What discovery found for one tool on one computer. */
export interface AnalyticsSourceCounts {
  roots: number;
  filesFound: number;
  filesRead: number;
  filesFailed: number;
  /** ISO time of the newest log found, null when none. */
  newestMtime: string | null;
}

export interface AnalyticsRemoteSourceState {
  tool: AnalyticsSourceTool;
  host: AnalyticsScanHost;
  state: AnalyticsSourceState;
  lastScanAt: string | null;
  rowCount: number;
  detail: string | null;
  /** One fixed word: why this source is not read now (a scan failure cause, `read_failed`). */
  cause?: string | null;
  /** Discovery's counts, and files found per root origin (no paths). */
  counts?: AnalyticsSourceCounts;
  origins?: Array<{ origin: string; files: number }>;
  /** `python-fallback` with its cause when the Rust scanner did not read this computer. */
  reader?: string | null;
}

function sourceCounts(kind: AnalyticsDiscoveryKind | undefined): {
  counts?: AnalyticsSourceCounts;
  origins?: Array<{ origin: string; files: number }>;
} {
  if (!kind) return {};
  const origins = new Map<string, number>();
  for (const root of kind.roots)
    origins.set(root.origin, (origins.get(root.origin) ?? 0) + root.counts.filesFound);
  return {
    counts: {
      ...kind.counts,
      newestMtime:
        kind.counts.newestMtime === null ? null : new Date(kind.counts.newestMtime).toISOString(),
    },
    origins: [...origins].map(([origin, files]) => ({ origin, files })),
  };
}

/** Project the native reader's coverage verdict. The dates are evidence, not guessed usage. */
function applyFreshness(
  entry: Pick<AnalyticsRemoteSourceState, 'state' | 'detail' | 'cause'>,
  found: Pick<AnalyticsDiscoveryKind, 'freshness' | 'unmeasuredFiles'> | undefined
): void {
  if (!found) return;
  const clock = found.freshness;
  if (clock?.state === 'unknown' && entry.state === 'ok') {
    entry.state = 'cached';
    entry.cause = 'freshness_unverified';
    if (!entry.detail?.includes('current usage coverage is unverified')) {
      entry.detail = [
        entry.detail,
        'the independent activity check is incomplete; current usage coverage is unverified',
      ]
        .filter(Boolean)
        .join('; ');
    }
  }
  if (
    clock?.state === 'stale' &&
    ['ok', 'no_usage', 'cached', 'stale'].includes(entry.state) &&
    !entry.cause
  ) {
    entry.state = 'stale';
    entry.cause = 'newer_activity';
    const data =
      clock.newestDataMs === null
        ? 'no measured usage date'
        : new Date(clock.newestDataMs).toISOString();
    const activity =
      clock.newestActivityMs === null ? 'unknown' : new Date(clock.newestActivityMs).toISOString();
    const note = `usage is stale: newest measured data ${data}; newer activity ${activity}; activity does not establish token counts or cost`;
    entry.detail = entry.detail ? `${entry.detail}; ${note}` : note;
  }
  if (found.unmeasuredFiles && !entry.detail?.includes('no numeric usage')) {
    const note = `${found.unmeasuredFiles} local chat data file${found.unmeasuredFiles === 1 ? '' : 's'} expose no numeric usage; their tokens and cost are not counted`;
    entry.detail = entry.detail ? `${entry.detail}; ${note}` : note;
  }
}

type SourceFreshness = NonNullable<AnalyticsDiscoveryKind['freshness']>;
type LocalFreshness = Partial<Record<AnalyticsRemoteKind, SourceFreshness>>;

/** Independent coverage is useful even before the native reader owns any numeric rows. */
function localFreshnessOf(discovery: AnalyticsRemoteDiscovery | undefined): LocalFreshness {
  const result: LocalFreshness = {};
  for (const tool of LOCAL_SCANNER_KINDS) {
    const clock = discovery?.kinds[tool]?.freshness;
    if (clock) result[tool] = { ...clock };
  }
  return result;
}

/** Reconcile native evidence only with an actual collector event, never its scan/read time. */
export function applyLocalScannerFreshness(
  entry: Pick<AnalyticsRemoteSourceState, 'state' | 'detail' | 'cause'>,
  clock: SourceFreshness | undefined,
  collectorDataMs?: number
): void {
  if (!clock) return;
  let resolved = clock;
  if (
    collectorDataMs !== undefined &&
    Number.isFinite(collectorDataMs) &&
    (clock.newestDataMs === null || collectorDataMs > clock.newestDataMs)
  ) {
    const stale =
      clock.newestActivityMs !== null && clock.newestActivityMs - collectorDataMs > 5 * 60_000;
    const incomplete =
      clock.activityComplete === false ||
      (clock.activityComplete === undefined && clock.state !== 'current');
    resolved = {
      ...clock,
      newestDataMs: collectorDataMs,
      state: stale ? 'stale' : incomplete ? 'unknown' : 'current',
    };
  }
  applyFreshness(entry, { freshness: resolved });
}

const NO_READER_LABELS: Record<AnalyticsNoReaderTool, string> = {
  'claude-desktop': 'Claude desktop',
  'codex-app': 'Codex app',
  'cursor-ide': 'Cursor IDE',
  windsurf: 'Windsurf',
  cursor: 'Cursor',
  'gemini-cli': 'Gemini CLI',
  opencode: 'OpenCode',
  qwen: 'Qwen Code',
  kimi: 'Kimi Code',
  grok: 'Grok CLI',
  kiro: 'Kiro CLI',
  pi: 'Pi',
  fx: 'fx',
};

/** The tools without a usage reader that the computer's last presence read found, as `no_reader`. */
export function noReaderStates(
  host: AnalyticsScanHost,
  tools: AnalyticsRemoteDiscovery['tools'],
  at: string | null
): AnalyticsRemoteSourceState[] {
  const states: AnalyticsRemoteSourceState[] = [];
  for (const tool of NO_READER_TOOLS) {
    const entry: AnalyticsPresenceTool | undefined = tools?.[tool];
    if (!entry?.installed) continue;
    const label = NO_READER_LABELS[tool];
    states.push({
      tool,
      host,
      state: 'no_reader',
      lastScanAt: at,
      rowCount: 0,
      detail:
        entry.cause === 'no_local_usage_records'
          ? `${label} keeps its chats on this computer without token counts; its usage is kept by ${label}, and its quota shows on Home`
          : `${label} is on this computer${entry.counts.filesFound ? ` with ${entry.counts.filesFound} session files` : ''}; Analytics has no usage reader for it yet`,
      cause: entry.cause,
      counts: {
        roots: 0,
        filesFound: entry.counts.filesFound,
        filesRead: 0,
        filesFailed: 0,
        newestMtime:
          entry.counts.newestMtime === null
            ? null
            : new Date(entry.counts.newestMtime).toISOString(),
      },
    });
  }
  return states;
}

/**
 * One cell per (host, tool): presence-only metadata (`no_reader`) never stands beside the real
 * reader's own cell for the same tool on the same computer (RECHECK-BIG-1 L2). The reader's
 * state wins; first-seen order is kept.
 */
function coalesceSourceStates(states: AnalyticsRemoteSourceState[]): AnalyticsRemoteSourceState[] {
  const byCell = new Map<string, AnalyticsRemoteSourceState>();
  for (const state of states) {
    const key = `${state.host}\0${state.tool}`;
    const kept = byCell.get(key);
    if (!kept || (kept.state === 'no_reader' && state.state !== 'no_reader'))
      byCell.set(key, state);
  }
  return [...byCell.values()];
}

/** The label a fallback answer carries into every cell of its computer. */
function readerLabel(reader: AnalyticsReader | undefined): string | null {
  return reader?.name === 'python-fallback'
    ? `python-fallback:${reader.cause ?? 'binary_failed'}`
    : null;
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
  'cursor',
  'devin',
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
    case 'unreadable_folder':
      return 'a folder searched for logs could not be read';
    case 'read_failed':
      return 'its logs were found but could not be read';
    case 'no_local_usage_records':
      return 'the tool keeps no local usage records';
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
  /** The scanner on the dashboard's own computer (DESIGN 16.0 B); tests pass their own. */
  runLocal?: (request: AnalyticsRemoteRequest) => Promise<AnalyticsRemoteResponse>;
  /** Whether the local scanner can run (built, in place, digest matching), else why not. */
  localScanner?: () => { ok: true } | { ok: false; cause: string };
  /** Codex experiment roots the background walk found on this computer (PM-Experiments). */
  localExperimentRoots?: () => Partial<Record<AnalyticsRemoteKind, string[]>>;
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
const REMOTE_TARGETS = {
  ...Object.fromEntries(
    REMOTE_ANALYTICS_HOSTS.map((host) => [host, [...ANALYTICS_TOOLS, 'cursor', 'devin']])
  ),
  ubuntu: [...ANALYTICS_TOOLS, 'cursor', 'devin'],
} as Record<AnalyticsScanHost, AnalyticsRemoteKind[]>;

/**
 * Kinds a TypeScript collector reads on the dashboard's own computer: the scanner takes each over
 * at its first complete read there (DESIGN 16.0 B). Cursor and Devin have no collector, so the
 * scanner owns them from its first answer.
 */
const TAKEOVER_KINDS: readonly AnalyticsRemoteKind[] = ANALYTICS_TOOLS;

/**
 * The kinds the scanner reads on the dashboard's own computer (DESIGN 16.0 B). Existing tools move from the
 * TypeScript collector; each kind is owned by the scanner from its first complete read there,
 * which replaces the collector's rows in one step.
 */
export const LOCAL_SCANNER_KINDS: readonly AnalyticsRemoteKind[] = REMOTE_TARGETS.ubuntu;

/**
 * G14: the activity plan the Rust writers' scheduler scans from — one computer per configured
 * remote host plus the dashboard's own computer, each with the kinds the scanner reads there.
 * The local extra/experiment roots join the ubuntu entry; fingerprints ride along when the cache
 * holds them. Root discovery and fingerprinting stay here (fed to Rust) until their own ports land.
 */
export function analyticsWriterPlan(): {
  computers: Array<{
    host: AnalyticsScanHost;
    platform: string;
    sshHost?: string;
    kinds: AnalyticsRemoteKind[];
    extraRoots?: Partial<Record<AnalyticsRemoteKind, string[]>>;
    experimentRoots?: Partial<Record<AnalyticsRemoteKind, string[]>>;
  }>;
  windowDays: number;
} {
  const cacheDir = path.join(getCcsDir(), 'cache');
  let experiments: Partial<Record<AnalyticsRemoteKind, string[]>> = {};
  try {
    experiments = localExperimentRoots(cacheDir);
  } catch {
    experiments = {};
  }
  const extra = localExtraRoots([...LOCAL_SCANNER_KINDS], experiments);
  const hosts: AnalyticsScanHost[] = [...REMOTE_ANALYTICS_HOSTS, 'ubuntu'];
  const computers = hosts.map((host) => ({
    host,
    platform: host,
    ...(host === 'ubuntu' ? {} : { sshHost: host }),
    // Kinds with more roots than the scanner takes stay with the collector (overflow).
    kinds:
      host === 'ubuntu'
        ? REMOTE_TARGETS.ubuntu.filter((kind) => !extra.overflow.includes(kind))
        : [...REMOTE_TARGETS[host]],
    ...(host === 'ubuntu' ? { extraRoots: extra.extra, experimentRoots: extra.experiments } : {}),
  }));
  return { computers, windowDays: 31 };
}
const LOCAL_ALIAS = '@local';
const EXTRA_ROOTS_MAX = 16;

/** What the local scanner did this collection (DESIGN 16.0 B), for the activity service. */
export interface LocalScannerAnswer {
  /** Kinds the scanner owns on this computer: the collector's rows for them are dropped. */
  owned: AnalyticsRemoteKind[];
  /** Kinds still catching up, with what the collector's cell should add. */
  catchingUp: Partial<Record<AnalyticsRemoteKind, string>>;
  /** Coverage evidence survives takeover, unavailable binaries and cache-only restarts. */
  freshness?: LocalFreshness;
  /** Why the scanner did not run here (the collector reads everything, labelled). */
  unavailable?: string;
  /** Kinds with more extra roots than the scanner takes: the collector keeps them. */
  tooManyRoots?: AnalyticsRemoteKind[];
}

/** Ownership after an answer: kept once taken; taken on a kind's first complete read. */
function ownedAfter(
  before: Partial<Record<AnalyticsRemoteKind, string>> | undefined,
  kinds: AnalyticsRemoteKind[],
  reads: Partial<Record<AnalyticsRemoteKind, KindRead>>,
  scannedAt: string,
  response: AnalyticsRemoteResponse,
  cut: Set<AnalyticsRemoteKind>,
  failed: Set<AnalyticsRemoteKind>
): Partial<Record<AnalyticsRemoteKind, string>> {
  const owned = { ...before };
  for (const tool of kinds) {
    if (owned[tool] || !TAKEOVER_KINDS.includes(tool)) continue;
    const read = reads[tool];
    if (
      read?.lastScanAt === scannedAt &&
      ['ok', 'no_usage', 'not_installed', 'installed_no_logs'].includes(read.state) &&
      !read.unfinished?.length &&
      !cut.has(tool) &&
      !failed.has(tool) &&
      response.kinds[tool]?.partial !== true
    )
      owned[tool] = scannedAt;
  }
  return owned;
}

/** The collector's cell while the scanner catches up on its kind. */
function catchUpText(entry: AnalyticsRemoteSourceState): string {
  const found = entry.counts;
  const progress = found
    ? `${found.filesRead.toLocaleString('en-US')} of ${found.filesFound.toLocaleString('en-US')} logs read so far`
    : 'reading';
  const unfinished = entry.detail?.includes('not read to the end') ? `; ${entry.detail}` : '';
  return `the usage scanner is taking over this tool's logs on this computer (${progress}${unfinished}); these numbers are the built-in reader's until its first complete read`;
}

/** The experiment roots the background walk found here, per kind the scanner may own. */
function localExperimentRoots(cacheDir: string): Partial<Record<AnalyticsRemoteKind, string[]>> {
  const roots = readExperimentRoots(cacheDir).roots as Partial<Record<string, string[]>>;
  return Object.fromEntries(
    LOCAL_SCANNER_KINDS.map((kind) => [kind, Array.isArray(roots[kind]) ? roots[kind] : []])
  ) as Partial<Record<AnalyticsRemoteKind, string[]>>;
}

/** The kinds the local scanner owns, from its saved cache; empty when it never finished one. */
export function localScannerOwned(
  cacheDir: string = path.join(getCcsDir(), 'cache')
): Set<AnalyticsRemoteKind> {
  const cached = loadCache(cacheFile(cacheDir, 'ubuntu'));
  // A kind with more roots than the scanner takes is not scanned here: the built-in reader keeps
  // it, even after a take-over, so it is never left without a reader (FIX-ROUND-BIG 6).
  let overflow: AnalyticsRemoteKind[] = [];
  try {
    overflow = localExtraRoots([...LOCAL_SCANNER_KINDS], localExperimentRoots(cacheDir)).overflow;
  } catch {
    overflow = [];
  }
  return new Set(
    [
      ...LOCAL_SCANNER_KINDS.filter((kind) => !TAKEOVER_KINDS.includes(kind)),
      ...Object.keys(cached.owned ?? {}).filter((kind): kind is AnalyticsRemoteKind =>
        TAKEOVER_KINDS.includes(kind as AnalyticsRemoteKind)
      ),
    ].filter((kind) => !overflow.includes(kind))
  );
}

/**
 * The local scanner's extra roots: the saved sources for this computer and the experiment roots
 * the background walk found, per kind. A kind with more than the scanner accepts is not scanned
 * here (`overflow`), so the collector keeps it and nothing is read twice.
 */
function localExtraRoots(
  kinds: AnalyticsRemoteKind[],
  found: Partial<Record<AnalyticsRemoteKind, string[]>>
): {
  extra: Partial<Record<AnalyticsRemoteKind, string[]>>;
  experiments: Partial<Record<AnalyticsRemoteKind, string[]>>;
  overflow: AnalyticsRemoteKind[];
} {
  const extra = remoteExtraRoots('ubuntu', kinds);
  const experiments: Partial<Record<AnalyticsRemoteKind, string[]>> = {};
  const overflow: AnalyticsRemoteKind[] = [];
  for (const kind of kinds) {
    const paths = [...new Set(found[kind] ?? [])];
    if ((extra[kind]?.length ?? 0) > EXTRA_ROOTS_MAX || paths.length > MAX_EXPERIMENT_ROOTS)
      overflow.push(kind);
    else if (paths.length) experiments[kind] = paths;
  }
  return { extra, experiments, overflow };
}

/** The kinds scanned on one remote host, for states when no scan answered. */
export function analyticsRemoteTargets(host: AnalyticsScanHost): AnalyticsRemoteKind[] {
  return [...REMOTE_TARGETS[host]];
}

const MAX_CACHED_ROWS = 100_000;

/**
 * 7: Codex rows are keyed by conversation and Muse rows by session (one owner per conversation
 * across every folder, FIX-ROUND-BIG 1 and 2); rows keyed by path are read again.
 * 6: Atomically replace Claude copy groups; discard accumulated duplicate owners.
 * 5: Claude copy groups, mirror tags and persistent oversized-line verdicts.
 * 4: Claude counts one API response, not one content block; rows cached by
 * version 3 are read again. (3: per-session aggregates ride with the hourly
 * rows, so remote sessions count like local ones.)
 */
const CACHE_VERSION = 8;

/** A kind's last confirmed read, and the logs it has not read to their end (DESIGN 16.1). */
interface KindRead {
  lastScanAt: string | null;
  state: AnalyticsSourceState;
  unfinished?: AnalyticsUnfinishedLog[];
  unfinishedCount?: number;
  /** Last nested-folder failure, retained through cached/failed reads until successful recovery. */
  unreadableFolders?: number;
  /** The last attempt's explanation, retained with its state between scans. */
  detail?: string | null;
  cause?: string;
}

interface RemoteCache {
  /** Per-tool last confirmed read, including measured empty/absent answers. */
  kinds?: Partial<Record<AnalyticsRemoteKind, KindRead>>;
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
  /** Where each kind's logs were found, and the tools without a reader this host has. */
  discovery?: AnalyticsRemoteDiscovery;
  /** When `discovery.tools` was read. */
  toolsAt?: string;
  /** Identity evidence per session key (full hash, inferred cwd), bounded. */
  sessions?: Record<string, AnalyticsSessionEvidence>;
  /** Which reader answered last (`python-fallback:<cause>` when the scanner did not). */
  reader?: string | null;
  /** Why the cache saved before this one was discarded at load; kept until a complete read. */
  discarded?: RemoteCacheDiscard;
  /** Usage dropped to keep this cache under its limit; named while the cut is in range. */
  trimmed?: RemoteCacheTrim;
  /** The local scanner only: when it took over each kind from the TypeScript collector. */
  owned?: Partial<Record<AnalyticsRemoteKind, string>>;
}

function cacheFile(cacheDir: string, host: AnalyticsScanHost): string {
  return path.join(cacheDir, 'analytics-remote-v1', `${host}.json`);
}

function blankCache(): RemoteCache {
  return { version: CACHE_VERSION, fingerprints: {}, rows: [], srows: [], lastScanAt: null };
}

/**
 * A host's saved cache. A missing file is a first scan; a file over the limit, unreadable or of an
 * unknown shape is discarded with its cause, so the page says why the computer is read from the start.
 */
function loadCache(file: string, at: () => string = () => new Date().toISOString()): RemoteCache {
  const discard = (cause: RemoteCacheDiscardCause): RemoteCache => ({
    ...blankCache(),
    discarded: { cause, at: at() },
  });
  let size: number;
  try {
    size = fs.statSync(file).size;
  } catch {
    return blankCache();
  }
  if (size > MAX_REMOTE_CACHE_BYTES) return discard('cache_oversize');
  try {
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as Omit<RemoteCache, 'version'> & {
      version: number;
    };
    // Versions 1/2 predate hashed session aggregates and cannot safely retain
    // their sessions or confirm their files. Unknown future formats also reset.
    if (
      ![3, 4, 5, 6, 7, CACHE_VERSION].includes(value.version) ||
      !value.fingerprints ||
      typeof value.fingerprints !== 'object' ||
      !Array.isArray(value.rows) ||
      value.rows.length > MAX_CACHED_ROWS ||
      !Array.isArray(value.srows) ||
      value.srows.length > MAX_CACHED_ROWS ||
      (value.lastScanAt !== null && typeof value.lastScanAt !== 'string') ||
      (value.settled !== undefined && typeof value.settled !== 'boolean')
    )
      return discard('cache_format');
    value.discarded = parseRemoteCacheDiscard(value.discarded);
    value.trimmed = parseRemoteCacheTrim(value.trimmed);
    // Discovery, evidence and the reader label are diagnostics: damaged entries are dropped.
    value.discovery = parseAnalyticsDiscovery(value.discovery);
    // A kind's unfinished list is a diagnostic: a malformed one is dropped.
    for (const read of Object.values(value.kinds ?? {})) {
      if (!read || typeof read !== 'object') continue;
      // Before v8 a productive partial scan overwrote the full-read clock. Its old full
      // read cannot be recovered; retain all usage, but never publish an attempt as that read.
      if (value.version < 8) {
        if (read.state === 'cached') read.lastScanAt = null;
        // Old counts included file failures and speculative signature child paths. The state
        // and rows stay; only a new scan can establish an accurate folder count.
        delete read.unreadableFolders;
      }
      if (
        !Number.isSafeInteger(read.unreadableFolders) ||
        (read.unreadableFolders ?? 0) <= 0 ||
        (read.unreadableFolders ?? 0) > 100_000
      )
        delete read.unreadableFolders;
      if (read.unfinished === undefined) continue;
      const list = parseUnfinished(read.unfinished);
      if (list?.length) {
        read.unfinished = list;
        read.unfinishedCount =
          Number.isSafeInteger(read.unfinishedCount) && (read.unfinishedCount ?? 0) >= list.length
            ? read.unfinishedCount
            : list.length;
      } else {
        delete read.unfinished;
        delete read.unfinishedCount;
      }
    }
    value.sessions = parseAnalyticsSessions(value.sessions);
    if (value.toolsAt !== undefined && !Number.isFinite(Date.parse(String(value.toolsAt))))
      value.toolsAt = undefined;
    if (
      value.reader !== undefined &&
      value.reader !== null &&
      !(typeof value.reader === 'string' && /^python-fallback:[a-z_:]{1,40}$/.test(value.reader))
    )
      value.reader = undefined;
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
            failure.kinds.length <= REMOTE_KIND_NAMES.size &&
            failure.kinds.every((kind) => REMOTE_KIND_NAMES.has(kind))
              ? { kinds: [...failure.kinds] }
              : {}),
          }
        : undefined;
    if (value.version !== CACHE_VERSION) {
      // Versions 3-6 share the row/session/fingerprint formats of the other tools.
      // Claude's response semantics and copy ownership changed in 4-6: its
      // aggregates and prints must rebuild together, without losing other tools.
      // Version 7 keys Codex by conversation and Muse by session: their rows keyed by path are read
      // again, and a take-over of them waits for that read (FIX-ROUND-BIG 1 and 2).
      const rebuilt = new Set<string>(
        value.version < 6 ? ['claude', 'codex', 'muse'] : value.version < 7 ? ['codex', 'muse'] : []
      );
      const fingerprints = { ...value.fingerprints };
      const kinds = { ...value.kinds };
      const owned = { ...value.owned };
      for (const kind of rebuilt) {
        delete fingerprints[kind];
        delete kinds[kind as AnalyticsRemoteKind];
        delete owned[kind as AnalyticsRemoteKind];
      }
      return {
        ...value,
        version: CACHE_VERSION,
        fingerprints,
        kinds,
        ...(value.owned ? { owned } : {}),
        rows: value.rows.filter((row) => !rebuilt.has(row.k)),
        srows: value.srows.filter((row) => !rebuilt.has(row.k)),
        settled: false,
      };
    }
    return { ...value, version: CACHE_VERSION };
  } catch {
    return discard('cache_unreadable');
  }
}

/** Session evidence without working folders. */
function withoutCwd(
  sessions: Record<string, AnalyticsSessionEvidence> | undefined
): Record<string, AnalyticsSessionEvidence> | undefined {
  if (!sessions) return sessions;
  return Object.fromEntries(
    Object.entries(sessions).map(([key, evidence]) => [key, { ...evidence, cwd: null }])
  );
}

function writePrivate(file: string, body: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, body, { mode: 0o600 });
  fs.renameSync(temporary, file);
  fs.chmodSync(file, 0o600);
}

/**
 * Every save goes through here. With cwd capture off, no working folder is written, whatever the
 * answer or the cache in memory held (a scan that started before the setting changed included).
 * A cache over the limit the load enforces is trimmed to fit (see `fitRemoteCache`); returns what
 * was written, or null when nothing fits (the last saved cache then stays).
 */
function saveCache(file: string, value: RemoteCache): RemoteCache | null {
  let saved: RemoteCache = {
    ...value,
    rows: value.rows.slice(0, MAX_CACHED_ROWS),
    srows: value.srows.slice(0, MAX_CACHED_ROWS),
  };
  if (!readAnalyticsCaptureSettings().cwd) saved.sessions = withoutCwd(saved.sessions);
  let body = JSON.stringify(saved);
  if (Buffer.byteLength(body) > MAX_REMOTE_CACHE_BYTES) {
    const fitted = fitRemoteCache(saved, new Date().toISOString());
    if (!fitted) return null;
    saved = fitted;
    body = JSON.stringify(saved);
  }
  writePrivate(file, body);
  return saved;
}

/**
 * Turning cwd capture off removes the working folders already saved, not only new ones: every
 * host's cache is rewritten without them. A cache the loader would discard anyway (over the limit
 * or unreadable) is removed, since it may hold folders too. Returns how many folders were cleared
 * and how many such caches were removed.
 */
export function clearAnalyticsRemoteCwds(cacheDir: string = path.join(getCcsDir(), 'cache')): {
  folders: number;
  caches: number;
} {
  let cleared = 0;
  let removed = 0;
  for (const host of [...REMOTE_ANALYTICS_HOSTS, 'ubuntu' as const]) {
    const file = cacheFile(cacheDir, host);
    let size: number;
    try {
      const stat = fs.lstatSync(file);
      if (!stat.isFile()) continue;
      size = stat.size;
    } catch {
      continue;
    }
    let value: unknown = null;
    if (size <= MAX_REMOTE_CACHE_BYTES)
      try {
        value = JSON.parse(fs.readFileSync(file, 'utf8'));
      } catch {
        value = null;
      }
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      fs.rmSync(file, { force: true });
      removed++;
      continue;
    }
    const sessions = (value as { sessions?: unknown }).sessions;
    if (!sessions || typeof sessions !== 'object') continue;
    let changed = 0;
    for (const evidence of Object.values(sessions as Record<string, unknown>))
      if (evidence && typeof evidence === 'object') {
        const item = evidence as { cwd?: unknown };
        if (item.cwd !== null && item.cwd !== undefined) {
          item.cwd = null;
          changed++;
        }
      }
    if (!changed) continue;
    writePrivate(file, JSON.stringify(value));
    cleared += changed;
  }
  return { folders: cleared, caches: removed };
}

function hourEpoch(hour: string): number {
  return Date.parse(`${hour.replace(' ', 'T')}:00Z`);
}

/**
 * Files whose answer continues an earlier read (`readFrom`): `appended` when it starts where the
 * cached print stopped on the same file, `unmatched` when it does not (the rows lack the file's
 * start). DESIGN 16.1.
 */
function resumedFiles(
  kinds: AnalyticsRemoteKind[],
  freshPrints: Record<string, Record<string, AnalyticsRemoteFingerprint>>,
  cachedPrints: Record<string, Record<string, AnalyticsRemoteFingerprint>>
): {
  appended: Map<AnalyticsRemoteKind, Set<string>>;
  unmatched: Map<AnalyticsRemoteKind, Set<string>>;
} {
  const appended = new Map<AnalyticsRemoteKind, Set<string>>();
  const unmatched = new Map<AnalyticsRemoteKind, Set<string>>();
  for (const tool of kinds)
    for (const [key, print] of Object.entries(freshPrints[tool] ?? {})) {
      if (print.readFrom === undefined) continue;
      const before = cachedPrints[tool]?.[key];
      const target =
        before?.readThrough === print.readFrom && samePrint(print, before) ? appended : unmatched;
      if (!target.has(tool)) target.set(tool, new Set());
      target.get(tool)?.add(key);
    }
  return { appended, unmatched };
}

/** Hourly rows with the same file, model, route and hour summed into one (a resumed file's). */
function foldRows(rows: AnalyticsRemoteRow[]): AnalyticsRemoteRow[] {
  const byKey = new Map<string, AnalyticsRemoteRow>();
  for (const row of rows) {
    const key = JSON.stringify([row.k, row.f, row.m, row.p ?? '', row.h, row.c > 0]);
    const seen = byKey.get(key);
    if (!seen) {
      byKey.set(key, { ...row });
      continue;
    }
    seen.i += row.i;
    seen.o += row.o;
    seen.cr += row.cr;
    seen.cw += row.cw;
    seen.c += row.c;
    seen.n += row.n;
  }
  return [...byKey.values()];
}

/** Session rows of the same file, session, model and route summed, their span widened. */
function foldSessionRows(rows: AnalyticsRemoteSessionRow[]): AnalyticsRemoteSessionRow[] {
  const byKey = new Map<string, AnalyticsRemoteSessionRow>();
  for (const row of rows) {
    const key = JSON.stringify([row.k, row.f, row.s, row.m, row.p ?? '', row.c > 0]);
    const seen = byKey.get(key);
    if (!seen) {
      byKey.set(key, { ...row });
      continue;
    }
    seen.a = Math.min(seen.a, row.a);
    seen.z = Math.max(seen.z, row.z);
    seen.i += row.i;
    seen.o += row.o;
    seen.cr += row.cr;
    seen.cw += row.cw;
    seen.c += row.c;
    seen.n += row.n;
  }
  return [...byKey.values()];
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
      // The helper never mixes logged and unlogged events in one row. A row whose source
      // reported its cost carries it whole (a reported 0 stays 0); a row whose source gave
      // none never takes a list price and marks its bucket partial.
      ...(row.costBasis === 'unknown'
        ? { costBasis: 'unknown' as const }
        : row.costBasis === 'reported'
          ? { costUsd: row.c, costBasis: 'reported' as const }
          : row.c > 0
            ? { costUsd: row.c }
            : {}),
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

/**
 * Rows in the remote rows' shape as one tool's usage, read on `host`. The Cursor account's
 * rows keep this shape for the cost-basis checks; nothing of them is merged into a provider's
 * usage (they are the authoritative account total, never added).
 */
export function analyticsRowsResult(
  rows: AnalyticsRemoteRow[],
  kind: AnalyticsRemoteKind,
  host: AnalyticsRemoteHost | 'ubuntu'
): UsageWorkerResult {
  return toWorkerResult(rows, [], kind, host);
}

export function toWorkerResult(
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
  installed_no_logs: 'the tool but no logs yet',
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

function sizeText(bytes: number): string {
  return bytes >= 1024 ** 3
    ? `${(bytes / 1024 ** 3).toFixed(1)} GB`
    : `${Math.max(1, Math.round(bytes / 1024 ** 2))} MB`;
}

/**
 * Names the logs a kind has not read to their end (DESIGN 16.1), never a silent total: "2 logs not
 * read to the end yet: rollout-2026-10-31T08-06-21.jsonl.zst (3.2 GB, partly read: 1,450,000,000
 * of 4,637,763,647 bytes), ...". Names carry no conversation id; at most five are spelled out.
 */
/** The words for log folders that are there but could not be read, by why (FIX-ROUND-BIG 4). */
export function rootFailureText(roots: AnalyticsDiscoveryRoot[]): string {
  const count = roots.length;
  const causes = new Set(
    roots.map(
      (root) => root.cause ?? (root.state === 'link_unreachable' ? 'did_not_answer' : 'io_error')
    )
  );
  const why = causes.has('did_not_answer')
    ? 'did not answer (a network mount, say)'
    : causes.has('target_missing')
      ? 'point at a folder that is gone'
      : causes.has('permission_denied')
        ? 'could not be read (permission denied)'
        : 'could not be read';
  const linked = roots.every((root) => root.state.startsWith('link_'));
  if (
    roots.every(
      (root) => ['signature', 'experiment'].includes(root.origin) && root.counts.filesFound === 0
    )
  )
    return `${count} folder${count === 1 ? '' : 's'} searched for logs ${why}; discovery is incomplete, while readable log folders are still counted`;
  return `${count} ${linked ? 'linked ' : ''}log folder${count === 1 ? '' : 's'} ${count === 1 ? why.replace('point at', 'points at') : why}, so ${count === 1 ? 'its' : 'their'} logs are not counted`;
}

export function unfinishedText(list: AnalyticsUnfinishedLog[], count?: number): string {
  const total = Math.max(count ?? list.length, list.length);
  const bytes = (value: number) => value.toLocaleString('en-US');
  const named = list.slice(0, 5).map((log) => {
    if (log.reason === 'unreadable')
      return `${log.name} (${sizeText(log.sizeBytes)}, could not be decoded)`;
    if (log.reason === 'no_session_id')
      return `${log.name} (${sizeText(log.sizeBytes)}, partly read: no session id)`;
    const read = log.readThroughBytes ?? 0;
    const part = log.decodedBytes
      ? `partly read: ${bytes(read)} of ${bytes(log.decodedBytes)} bytes`
      : `partly read: ${bytes(read)} bytes, full size not stated in the file`;
    return `${log.name} (${sizeText(log.sizeBytes)}, ${part})`;
  });
  const more = total > named.length ? ` and ${total - named.length} more` : '';
  return `${total} log${total === 1 ? '' : 's'} not read to the end yet: ${named.join(', ')}${more}; each scan reads on from where the last one stopped`;
}

/** A host's cells with what its cache says about itself (discarded at load, or trimmed to fit). */
function withCacheNote(
  states: AnalyticsRemoteSourceState[],
  cache: Pick<RemoteCache, 'discarded' | 'trimmed'>,
  minDateMs: number
): AnalyticsRemoteSourceState[] {
  const note = remoteCacheNote(cache.discarded, cache.trimmed, minDateMs);
  if (!note) return states;
  return states.map((state) =>
    REMOTE_KIND_NAMES.has(state.tool)
      ? { ...state, detail: state.detail ? `${state.detail}; ${note}` : note }
      : state
  );
}

function unreadableFoldersText(count: number): string {
  return `${count.toLocaleString('en-US')} folder${count === 1 ? '' : 's'} inside its log folders could not be listed (permission denied, say), so ${count === 1 ? 'its' : 'their'} logs are not counted`;
}

/**
 * Previously read aggregates of one host, marked `cached`. A kind with nothing kept is `scan_failed`
 * while a failure stands (with its cause, and what the last read that worked found), never that
 * read's "no usage" or "not installed" as if it were current; without a failure, its last state.
 */
function cachedHostSources(
  host: AnalyticsScanHost,
  cached: RemoteCache,
  minDateMs: number,
  detail: string,
  failure?: AnalyticsHostFailure
): {
  results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
} {
  const results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  for (const tool of REMOTE_TARGETS[host]) {
    const kept = cached.rows.filter((row) => row.k === tool && inWindow(row, minDateMs));
    const keptSessions = cached.srows.filter(
      (row) => row.k === tool && sessionInWindow(row, minDateMs)
    );
    const read = cached.kinds?.[tool];
    const failedRoots = (cached.discovery?.kinds[tool]?.roots ?? []).filter(
      (root) => root.state === 'unreadable' || root.state.startsWith('link_')
    );
    const unreadableFolders = read?.unreadableFolders ?? 0;
    // Cursor without a run store is told by presence, one cell only (DESIGN 16.2).
    if (
      tool === 'cursor' &&
      !kept.length &&
      !keptSessions.length &&
      (!read || read.state === 'not_installed') &&
      !failedRoots.length &&
      !unreadableFolders
    )
      continue;
    const failed = standingFailure(cached, tool, failure);
    const unfinished = read?.unfinished?.length
      ? `; ${unfinishedText(read.unfinished, read.unfinishedCount)}`
      : '';
    const why = failed
      ? `${scanFailureText(failed.cause)} (${failed.at})${unfinished}`
      : (read?.detail ?? `${detail}${unfinished}`);
    const found = { ...sourceCounts(cached.discovery?.kinds[tool]), reader: cached.reader ?? null };
    // The last known per-root quality rides along (RECHECK-BIG-1 M5): a log folder the last real
    // scan could not read stays named on cached cells, never plain "cached". The saved discovery
    // is replaced by the next scan of this kind, so a successful scan clears the cause.
    if (failedRoots.length || unreadableFolders) {
      const causes = [
        ...(failedRoots.length ? [rootFailureText(failedRoots)] : []),
        ...(unreadableFolders ? [unreadableFoldersText(unreadableFolders)] : []),
      ].join('; ');
      if (kept.length || keptSessions.length)
        results.push({ tool, data: toWorkerResult(kept, keptSessions, tool, host) });
      states.push({
        tool,
        host,
        state: kept.length || keptSessions.length ? 'cached' : 'unreadable',
        lastScanAt: read?.lastScanAt ?? null,
        rowCount: kept.reduce((sum, row) => sum + Math.floor(row.n), 0),
        detail: `${causes}${failed ? `; ${scanFailureText(failed.cause)} (${failed.at})` : ''}; ${kept.length || keptSessions.length ? 'usage from readable logs is included' : 'nothing is counted from them'}`,
        cause: 'unreadable_folder',
        ...found,
      });
      continue;
    }
    if (kept.length || keptSessions.length) {
      results.push({ tool, data: toWorkerResult(kept, keptSessions, tool, host) });
      states.push({
        tool,
        host,
        state:
          !failed && read?.state === 'stale'
            ? 'stale'
            : !failed && read?.state === 'ok'
              ? 'ok'
              : 'cached',
        lastScanAt: read?.lastScanAt ?? null,
        rowCount: kept.reduce((sum, row) => sum + Math.floor(row.n), 0),
        detail:
          !failed && read?.state === 'ok'
            ? (read.detail ?? null)
            : read?.detail && !failed
              ? why
              : `${why}; showing previously read aggregates`,
        ...(failed ? { cause: failed.cause } : read?.cause ? { cause: read.cause } : {}),
        ...found,
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
        ...found,
      });
    } else {
      states.push({
        tool,
        host,
        state:
          read?.state === 'no_usage' ||
          read?.state === 'stale' ||
          read?.state === 'not_installed' ||
          read?.state === 'installed_no_logs' ||
          read?.state === 'unreadable' ||
          read?.state === 'scanning' ||
          read?.state === 'scan_failed'
            ? read.state
            : read?.state === 'cached'
              ? 'scanning'
              : 'unavailable',
        lastScanAt: read?.lastScanAt ?? null,
        rowCount: 0,
        detail:
          read?.detail ??
          (read?.state === 'unreadable'
            ? 'its logs were found but could not be read'
            : read?.state === 'scanning' || read?.state === 'cached'
              ? 'the scan has not finished'
              : read
                ? null
                : detail),
        ...(read?.state === 'unreadable'
          ? { cause: 'read_failed' }
          : read?.cause
            ? { cause: read.cause }
            : {}),
        ...found,
      });
    }
  }
  for (const entry of states)
    applyFreshness(entry, cached.discovery?.kinds[entry.tool as AnalyticsRemoteKind]);
  return { results, states };
}

/**
 * One host's rows for mirror selection. `lastScanAt` says whether the part is a confirmed read
 * (a cache file that exists, or a fresh scan): a blank cache from a missing file keeps it null,
 * so a saved mirror owner whose host is merely unread is never treated as having given it up.
 */
type HostRows = Pick<RemoteCache, 'rows' | 'srows' | 'fingerprints' | 'lastScanAt'>;

/** Local workers may finish after the remote scan; re-select before publishing. */
function mirrorReconciler(
  parts: Map<AnalyticsScanHost, HostRows>,
  results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }>,
  states: AnalyticsRemoteSourceState[],
  inventory: () => ClaudeLogNames,
  owners: ReadonlyMap<string, AnalyticsScanHost>
): () => void {
  const baseline = states.map((state) => ({ ...state }));
  return () => {
    states.splice(0, states.length, ...baseline.map((state) => ({ ...state })));
    selectClaudeMirrors(parts, results, states, inventory(), owners);
    states.sort((a, b) => a.tool.localeCompare(b.tool) || a.host.localeCompare(b.host));
  };
}

/**
 * Elects one owner per orphan Claude mirror, fixed host order among the hosts that hold it.
 * A saved owner (the owners file beside the caches) survives while its host is unread — a cache
 * file that is missing (or was discarded) leaves the mirror counted nowhere rather than moving
 * it to the next host, whose hours would double with the owner's once it returns (history-L5).
 * A saved owner whose host confirmed a read without the mirror gave it up (pruned there), so the
 * fixed order re-elects; a name no host holds anymore is dropped. The table is saved best-effort.
 */
function electClaudeMirrorOwners(
  cacheDir: string,
  parts: Map<AnalyticsScanHost, HostRows>
): Map<string, AnalyticsScanHost> {
  const holdsMirror = (part: HostRows, name: string): boolean =>
    part.rows.some(
      (row) => row.k === 'claude' && part.fingerprints.claude?.[row.f]?.claudeName === name
    );
  const owners = new Map<string, AnalyticsScanHost>();
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
  const file = mirrorOwnersFile(cacheDir);
  const saved = loadClaudeMirrorOwners(file);
  for (const [name, host] of saved) {
    // Gone from every host: the mirror is history, and a returning copy elects fresh.
    if (![...parts.values()].some((part) => holdsMirror(part, name))) continue;
    // The owner confirmed a read without the mirror: pruned there, so the next holder wins.
    const part = parts.get(host);
    if (part?.lastScanAt && !holdsMirror(part, name)) continue;
    owners.set(name, host);
  }
  const same =
    saved.size === owners.size && [...saved].every(([name, host]) => owners.get(name) === host);
  if (!same) saveClaudeMirrorOwners(file, owners);
  return owners;
}

/**
 * Caches always retain RAW mirror rows. Select only after every host has settled,
 * including cache fallbacks, so scan order cannot change totals. Hashing basenames
 * matches session and agent logs without exporting private paths. Re-selecting on
 * every read (including cache-only reads) makes a pruned original's mirror count
 * immediately, even when the helper's mirror fingerprint has not changed. `owners`
 * is the election above: fixed-order, saved owners kept through a missing cache file.
 */
function selectClaudeMirrors(
  parts: Map<AnalyticsScanHost, HostRows>,
  results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }>,
  states: AnalyticsRemoteSourceState[],
  local: ClaudeLogNames,
  owners: ReadonlyMap<string, AnalyticsScanHost>
): void {
  const originals = new Set(local.originals);
  for (const part of parts.values())
    for (const print of Object.values(part.fingerprints.claude ?? {}))
      if (print.claudeName && print.claudeMirror === false) originals.add(print.claudeName);
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
  deps: Pick<AnalyticsRemoteSourceDeps, 'cacheDir' | 'claudeLogNames' | 'localScanner'> = {}
): {
  fetchedAt: string | undefined;
  reconcileClaudeMirrors: () => void;
  results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
  local: LocalScannerAnswer;
} {
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  const results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  let fetchedAt: string | undefined;
  const parts = new Map<AnalyticsScanHost, HostRows>();
  // The local scanner's cache counts only for the kinds it owns, and only while it can run: else
  // the collectors read those kinds again and its rows would count twice (DESIGN 16.0 B).
  let ready: { ok: boolean };
  try {
    ready = (deps.localScanner ?? localUsageScanReady)();
  } catch {
    ready = { ok: false };
  }
  const owned = ready.ok ? localScannerOwned(cacheDir) : new Set<AnalyticsRemoteKind>();
  for (const host of [...REMOTE_ANALYTICS_HOSTS, 'ubuntu' as const]) {
    if (host === 'ubuntu' && owned.size === 0) continue;
    const cached = loadCache(cacheFile(cacheDir, host));
    if (cached.lastScanAt && (!fetchedAt || cached.lastScanAt > fetchedAt))
      fetchedAt = cached.lastScanAt;
    parts.set(host, {
      ...cached,
      rows: cached.rows.filter((row) => inWindow(row, minDateMs)),
      srows: cached.srows.filter((row) => sessionInWindow(row, minDateMs)),
    });
    const part = cachedHostSources(host, cached, minDateMs, 'remote scan timed out');
    const counted = (tool: string) => host !== 'ubuntu' || owned.has(tool as AnalyticsRemoteKind);
    results.push(...part.results.filter((entry) => counted(entry.tool)));
    states.push(
      ...withCacheNote(
        part.states
          .filter((entry) => counted(entry.tool))
          .map((entry) =>
            host === 'ubuntu'
              ? entry
              : {
                  ...entry,
                  detail: entry.detail?.includes('remote scan timed out')
                    ? entry.detail
                    : [entry.detail, 'remote scan timed out'].filter(Boolean).join('; '),
                }
          ),
        cached,
        minDateMs
      )
    );
    states.push(...noReaderStates(host, cached.discovery?.tools, cached.toolsAt ?? null));
  }
  states.splice(0, states.length, ...coalesceSourceStates(states));
  const owners = electClaudeMirrorOwners(cacheDir, parts);
  const reconcileClaudeMirrors = mirrorReconciler(
    parts,
    results,
    states,
    () => deps.claudeLogNames?.() ?? localClaudeLogNames(cacheDir, minDateMs),
    owners
  );
  reconcileClaudeMirrors();
  return {
    results,
    states,
    fetchedAt,
    reconcileClaudeMirrors,
    local: {
      owned: [...owned],
      catchingUp: {},
      freshness: localFreshnessOf(loadCache(cacheFile(cacheDir, 'ubuntu')).discovery),
    },
  };
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
  host: AnalyticsScanHost,
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

type RemoteHelper = (
  alias: string,
  host: AnalyticsScanHost,
  request: AnalyticsRemoteRequest
) => Promise<AnalyticsRemoteResponse>;

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
  host: AnalyticsScanHost,
  kinds: AnalyticsRemoteKind[],
  cached: RemoteCache,
  extraRoots: Partial<Record<AnalyticsRemoteKind, string[]>>,
  minDateMs: number,
  failure: ScanFailure,
  captureCwd = true,
  experimentRoots: Partial<Record<AnalyticsRemoteKind, string[]>> = {}
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
        await runHelper(alias, host, {
          kinds: group,
          minDateMs,
          fingerprints,
          extraRoots,
          ...(Object.keys(experimentRoots).length ? { experimentRoots } : {}),
          // One call per scan also says which tools without a reader the computer has.
          ...(group === groups[0] ? { presence: true } : {}),
          ...(captureCwd ? {} : { cwd: false }),
        })
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
    discovery: { kinds: {} },
    sessions: {},
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
      const found = answer.discovery?.kinds[kind];
      if (found && merged.discovery) merged.discovery.kinds[kind] = found;
      if (answer.discovery?.tools && merged.discovery)
        merged.discovery.tools = answer.discovery.tools;
      if (answer.sessions) Object.assign(merged.sessions ?? {}, answer.sessions);
      // One fallback answer labels the computer: its other kinds are read the older way too.
      if (answer.reader && (!merged.reader || answer.reader.name === 'python-fallback'))
        merged.reader = answer.reader;
      // What the computer itself said (FIX-ROUND-BIG 9): a mount that did not answer, a stall.
      if (answer.unansweredMounts)
        merged.unansweredMounts = Math.max(merged.unansweredMounts ?? 0, answer.unansweredMounts);
      if (answer.stalled) merged.stalled = true;
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
  results: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
  local: LocalScannerAnswer;
}> {
  const now = deps.now ?? Date.now;
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  let localFreshness = localFreshnessOf(loadCache(cacheFile(cacheDir, 'ubuntu')).discovery);
  const hosts = await (deps.hosts ?? resolveAnalyticsRemoteHosts)().catch(() =>
    fixedAnalyticsRemoteAliases()
  );
  const remoteRun = deps.runHelper ?? runAnalyticsRemoteScan;
  const localRun = deps.runLocal ?? runLocalUsageScan;
  const runHelper: RemoteHelper = (alias, host, request) =>
    host === 'ubuntu' ? localRun(request) : remoteRun(alias, host, request);
  // The dashboard's own computer joins while its scanner can run (DESIGN 16.0 B).
  let localReady: { ok: true } | { ok: false; cause: string };
  try {
    localReady = (deps.localScanner ?? localUsageScanReady)();
  } catch {
    localReady = { ok: false, cause: 'binary_missing' };
  }
  let experiments: Partial<Record<AnalyticsRemoteKind, string[]>> = {};
  try {
    experiments = (deps.localExperimentRoots ?? (() => localExperimentRoots(cacheDir)))();
  } catch {
    experiments = {};
  }
  const local = localExtraRoots([...LOCAL_SCANNER_KINDS], experiments);
  const scanHosts: AnalyticsScanHost[] = [
    ...REMOTE_ANALYTICS_HOSTS,
    ...(localReady.ok && local.overflow.length < LOCAL_SCANNER_KINDS.length
      ? (['ubuntu'] as const)
      : []),
  ];
  // The local scanner's answer is held apart: a kind joins the totals only once the scanner owns it.
  const sharedResults: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }> = [];
  const sharedStates: AnalyticsRemoteSourceState[] = [];
  const localResults: Array<{ tool: AnalyticsRemoteKind; data: UsageWorkerResult }> = [];
  const localStates: AnalyticsRemoteSourceState[] = [];
  let localOwned = new Set<AnalyticsRemoteKind>();
  let captureCwd = true;
  try {
    captureCwd = readAnalyticsCaptureSettings().cwd;
  } catch {
    /* Unreadable settings keep the default. */
  }
  const results = sharedResults;
  const states = sharedStates;
  const jobs: Array<Promise<void>> = [];
  const parts = new Map<AnalyticsScanHost, HostRows>();
  // What each host's cache says about itself, named on its cells once every host is done.
  const cacheMarks = new Map<AnalyticsScanHost, Pick<RemoteCache, 'discarded' | 'trimmed'>>();
  for (const host of scanHosts) {
    jobs.push(
      (async (): Promise<void> => {
        const results = host === 'ubuntu' ? localResults : sharedResults;
        const states = host === 'ubuntu' ? localStates : sharedStates;
        // A kind over the scanner's root limit is the built-in reader's this collection
        // (FIX-ROUND-BIG 6), whatever was taken over before.
        if (host === 'ubuntu')
          localOwned = new Set(
            [...localScannerOwned(cacheDir)].filter((kind) => !local.overflow.includes(kind))
          );
        const allKinds = REMOTE_TARGETS[host].filter(
          (kind) => host !== 'ubuntu' || !local.overflow.includes(kind)
        );
        const file = cacheFile(cacheDir, host);
        const cached = loadCache(file, () => new Date(now()).toISOString());
        cacheMarks.set(host, cached);
        // A successful disk answer remains authoritative across process restarts.
        // Each kind has its own last scan; one recent kind cannot hide an old one.
        const recentKinds = allKinds.filter((kind) => {
          const read = cached.kinds?.[kind];
          const age = now() - Date.parse(read?.lastScanAt ?? '');
          return (
            !deps.force &&
            read &&
            ['ok', 'no_usage', 'not_installed', 'installed_no_logs'].includes(read.state) &&
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
            (source) => recentKinds.includes(source.tool as AnalyticsRemoteKind)
          )
        );
        if (kinds.length === 0) {
          parts.set(host, {
            ...cached,
            rows: cached.rows.filter((row) => inWindow(row, minDateMs)),
            srows: cached.srows.filter((row) => sessionInWindow(row, minDateMs)),
          });
          states.push(...noReaderStates(host, cached.discovery?.tools, cached.toolsAt ?? null));
          return;
        }
        const alias = host === 'ubuntu' ? LOCAL_ALIAS : hosts[host];
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
        const onHostScan = (scanned: AnalyticsScanHost, phase: 'start' | 'done') => {
          if (scanned !== 'ubuntu') deps.onHostScan?.(scanned, phase);
        };
        onHostScan(host, 'start');
        const failure: ScanFailure = { damaged: false, byKind: new Map() };
        const scan = await scanHost(
          runHelper,
          alias,
          host,
          kinds,
          cached,
          host === 'ubuntu' ? local.extra : remoteExtraRoots(host, kinds),
          minDateMs,
          failure,
          captureCwd,
          host === 'ubuntu' ? local.experiments : {}
        );
        if (!scan) {
          parts.set(host, {
            ...cached,
            rows: cached.rows.filter((row) => inWindow(row, minDateMs)),
            srows: cached.srows.filter((row) => sessionInWindow(row, minDateMs)),
          });
          onHostScan(host, 'done');
          const lastFailure: AnalyticsHostFailure = {
            cause: failure.damaged ? 'helper_damaged' : (failure.cause ?? 'helper_failed'),
            exitCode: failure.exitCode ?? null,
            at: new Date(now()).toISOString(),
            kinds: [...kinds],
          };
          // Saved with the host, rows and prints untouched, so the failure is named until a scan
          // answers again, also after a restart. A damaged package fails every host alike and is
          // named by the page without touching any host's cache. A file the load discarded (a
          // newer format after a rollback, say) is not replaced by a blank while scans fail.
          if (!failure.damaged && !cached.discarded)
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
          states.push(
            ...part.states.filter(
              (source) => !recentKinds.includes(source.tool as AnalyticsRemoteKind)
            )
          );
          states.push(...noReaderStates(host, cached.discovery?.tools, cached.toolsAt ?? null));
          return;
        }
        const { response, cutKinds, failedKinds, olderClaude } = scan;
        await yieldTurn();
        const errored = new Set(kinds.filter((tool) => response.kinds[tool]?.state === 'error'));
        // The scanner and the Python fallback key files apart (Codex and Muse by conversation in
        // the scanner, by path in Python): after a switch, rows of the other reader are never kept
        // beside the new ones, partial or not (FIX-ROUND-BIG 1).
        const readerChanged =
          cached.lastScanAt !== null &&
          (cached.reader ?? '').startsWith('python-fallback') !==
            (readerLabel(response.reader) ?? '').startsWith('python-fallback');
        // Files a scan did not reach keep their rows and prints until a scan
        // reaches them: after a kind's scan was cut, a kind that could not be
        // read, or (OMP) a root search that hit its bounds.
        const keepsUnvisited = (tool: AnalyticsRemoteKind): boolean =>
          !readerChanged &&
          ((tool === 'claude' && olderClaude) ||
            cutKinds.has(tool) ||
            (tool === 'codex' && response.kinds.codex?.partial === true) ||
            errored.has(tool) ||
            response.kinds[tool]?.state === 'pending' ||
            response.kinds[tool]?.partial === true ||
            (tool === 'antigravity' && response.kinds[tool]?.unreadable === true) ||
            (tool === 'omp' && response.discoveryTruncated === true));
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
        // A resumed read (DESIGN 16.1) adds to the rows kept for its file when it starts where the
        // cached read stopped on the same file. Otherwise its rows lack the file's start: they and
        // the print are dropped, and the next call reads the file from 0.
        const { appended, unmatched } = resumedFiles(kinds, freshPrints, cached.fingerprints);
        for (const [tool, keys] of unmatched)
          for (const key of keys) delete freshPrints[tool]?.[key];
        // Fully read groups explicitly replace all current and previous owners.
        // Emitted rows also replace their own file even if its print is unchanged.
        const replaced = new Map(kinds.map((tool) => [tool, new Set<string>()]));
        if (!discarded('claude'))
          for (const key of response.kinds.claude?.replacedFiles ?? [])
            replaced.get('claude')?.add(key);
        for (const row of [...response.rows, ...(response.srows ?? [])])
          if (!discarded(row.k) && !appended.get(row.k)?.has(row.f))
            replaced.get(row.k)?.add(row.f);
        const unmatchedRow = (row: { k: AnalyticsRemoteKind; f: string }) =>
          unmatched.get(row.k)?.has(row.f) === true;
        const kept = cached.rows.filter((row) => {
          if (!inWindow(row, minDateMs)) return false;
          if (!kinds.includes(row.k)) return true;
          if (replaced.get(row.k)?.has(row.f)) return false;
          const print = freshPrints[row.k]?.[row.f];
          if (print === undefined) return keepsUnvisited(row.k);
          return samePrint(print, cached.fingerprints[row.k]?.[row.f]);
        });
        const fresh = response.rows.filter(
          (row) =>
            kinds.includes(row.k) &&
            !discarded(row.k) &&
            !unmatchedRow(row) &&
            inWindow(row, minDateMs)
        );
        const merged = foldRows([...kept, ...fresh]).slice(0, MAX_CACHED_ROWS);
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
          (row) =>
            kinds.includes(row.k) &&
            !discarded(row.k) &&
            !unmatchedRow(row) &&
            sessionInWindow(row, minDateMs)
        );
        const mergedSessions = foldSessionRows([...keptSessions, ...freshSessions]).slice(
          0,
          MAX_CACHED_ROWS
        );
        const scannedAt = new Date(now()).toISOString();
        const kindReads = { ...cached.kinds };
        for (const tool of kinds) {
          const kind = response.kinds[tool];
          if (!kind) continue;
          const partial =
            cutKinds.has(tool) || kind.partial || (tool === 'omp' && response.discoveryTruncated);
          if (discarded(tool) || (partial && Object.keys(freshPrints[tool] ?? {}).length === 0)) {
            kindReads[tool] = {
              ...cached.kinds?.[tool],
              lastScanAt: cached.kinds?.[tool]?.lastScanAt ?? null,
              state:
                kind.state === 'pending'
                  ? 'scanning'
                  : kind.state === 'error' && !merged.some((row) => row.k === tool)
                    ? 'unreadable'
                    : 'cached',
            };
            continue;
          }
          kindReads[tool] = {
            lastScanAt: partial ? (cached.kinds?.[tool]?.lastScanAt ?? null) : scannedAt,
            state: partial
              ? 'cached'
              : kind.state === 'not_installed'
                ? response.discovery?.kinds[tool]?.installed === true
                  ? 'installed_no_logs'
                  : 'not_installed'
                : merged.some((row) => row.k === tool)
                  ? 'ok'
                  : 'no_usage',
          };
        }
        // The logs a kind has not read to its end, as its last answer named them (DESIGN 16.1).
        for (const tool of kinds) {
          const kind = response.kinds[tool];
          if (!kind) continue;
          let read = kindReads[tool];
          const unreadableFolders =
            kind.unreadableFolders ??
            (discarded(tool) || kind.partial || cutKinds.has(tool)
              ? cached.kinds?.[tool]?.unreadableFolders
              : undefined);
          if (unreadableFolders) {
            read = { ...(read ?? { lastScanAt: null, state: 'cached' }), unreadableFolders };
            kindReads[tool] = read;
          } else if (read) {
            delete read.unreadableFolders;
          }
          if (discarded(tool)) continue;
          if (kind.unfinished?.length)
            kindReads[tool] = {
              ...(read ?? { lastScanAt: null, state: 'cached' }),
              unfinished: kind.unfinished,
              unfinishedCount: kind.unfinishedCount ?? kind.unfinished.length,
            };
          else if (read) {
            const { unfinished: _list, unfinishedCount: _count, ...rest } = read;
            void _list;
            void _count;
            kindReads[tool] = rest;
          }
        }
        // Discovery of the kinds this scan visited replaces theirs; the rest keep their last.
        const discovery: AnalyticsRemoteDiscovery = {
          kinds: { ...cached.discovery?.kinds },
          ...(cached.discovery?.tools ? { tools: cached.discovery.tools } : {}),
        };
        for (const tool of kinds) {
          const found = response.discovery?.kinds[tool];
          if (found) discovery.kinds[tool] = found;
        }
        if (host === 'ubuntu') localFreshness = localFreshnessOf(discovery);
        let toolsAt = cached.toolsAt;
        if (response.discovery?.tools) {
          discovery.tools = response.discovery.tools;
          toolsAt = scannedAt;
        }
        const reader = readerLabel(response.reader);
        // `readFrom` describes one answer; the saved print keeps only how far the file was read.
        const prints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {};
        for (const [tool, entries] of Object.entries(freshPrints)) {
          prints[tool] = {};
          for (const [key, print] of Object.entries(entries)) {
            const { readFrom: _from, ...kept } = print;
            void _from;
            prints[tool][key] = kept;
          }
        }
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
        parts.set(host, {
          rows: merged,
          srows: mergedSessions,
          fingerprints: prints,
          // A fresh scan is a confirmed read, so a saved mirror owner that lost its mirror
          // here is treated as pruned (the election may hand it to the next holder).
          lastScanAt: scannedAt,
        });
        // Identity evidence of the sessions still kept, newest answer first, bounded like the rows.
        const keptKeys = new Set(mergedSessions.map((row) => row.s));
        const sessions: Record<string, AnalyticsSessionEvidence> = {};
        for (const source of [response.sessions ?? {}, cached.sessions ?? {}])
          for (const [key, evidence] of Object.entries(source))
            if (keptKeys.has(key) && !(key in sessions)) sessions[key] = evidence;
        const settled =
          !olderClaude &&
          cutKinds.size === 0 &&
          failedKinds.size === 0 &&
          kinds.every(
            (tool) =>
              response.kinds[tool]?.state !== 'pending' &&
              response.kinds[tool]?.state !== 'error' &&
              response.kinds[tool]?.partial !== true &&
              !(tool === 'antigravity' && response.kinds[tool]?.unreadable)
          );
        // The local scanner owns a kind from its first complete read (DESIGN 16.0 B).
        // A kind that overflowed is handed back to the built-in reader, and is taken over again only
        // after its next complete read (FIX-ROUND-BIG 6).
        const owned =
          host === 'ubuntu'
            ? Object.fromEntries(
                Object.entries(
                  ownedAfter(
                    cached.owned,
                    kinds,
                    kindReads,
                    scannedAt,
                    response,
                    cutKinds,
                    failedKinds
                  )
                ).filter(([kind]) => !local.overflow.includes(kind as AnalyticsRemoteKind))
              )
            : undefined;
        if (owned)
          localOwned = new Set([
            ...LOCAL_SCANNER_KINDS.filter((kind) => !TAKEOVER_KINDS.includes(kind)),
            ...(Object.keys(owned) as AnalyticsRemoteKind[]),
          ]);
        const firstState = states.length;
        for (const tool of kinds) {
          await yieldTurn();
          const rows = merged.filter((row) => row.k === tool);
          const srows = mergedSessions.filter((row) => row.k === tool);
          const kind = response.kinds[tool];
          const kindState = kind?.state ?? 'error';
          const found = { ...sourceCounts(discovery.kinds[tool]), reader };
          // The older Python reader has no Cursor or Devin reader: no cell of its own (Cursor's
          // fixed "not measured" row stands in); what the scanner read before still counts.
          if (!PYTHON_KINDS.has(tool) && response.reader?.name === 'python-fallback') {
            if (rows.length || srows.length) {
              results.push({ tool, data: toWorkerResult(rows, srows, tool, host) });
              states.push({
                tool,
                host,
                state: 'cached',
                lastScanAt: cached.kinds?.[tool]?.lastScanAt ?? null,
                rowCount: rows.reduce((sum, row) => sum + Math.floor(row.n), 0),
                detail:
                  'this computer was read by the older Python reader, which has no reader for this tool; showing what the usage scanner read before',
                reader,
              });
            }
            continue;
          }
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
          // A kind whose roots are there but could not be read is partial, never "not installed"
          // or "no logs" (FIX-ROUND-BIG 4): it takes the partial branch below.
          const notInstalled = kindState === 'not_installed' && kind?.partial !== true;
          // Cursor without a run store is told by presence ("No local usage records"), one cell only.
          if (tool === 'cursor' && notInstalled) continue;
          if (notInstalled) {
            const installed = response.discovery?.kinds[tool]?.installed === true;
            states.push({
              tool,
              host,
              state: installed ? 'installed_no_logs' : 'not_installed',
              lastScanAt: scannedAt,
              rowCount: 0,
              detail: installed
                ? 'the tool is on this computer, but no log of it was found'
                : 'no usage logs found on this host',
              ...found,
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
              detail: response.stalled
                ? `the scan did not answer within its time (a folder that hangs, say), so nothing was read this time; ${kept ? 'records from earlier scans stay included' : 'the next scan tries again'}`
                : kept
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
            // A folder that is there but cannot be read is "unreadable", whatever else the kind
            // has: never "no usage" or "read" (FIX-ROUND-BIG 4). Its earlier rows stay.
            const failedRoots = (response.discovery?.kinds[tool]?.roots ?? []).filter(
              (root) => root.state === 'unreadable' || root.state.startsWith('link_')
            );
            const unreadableFolders = kind?.unreadableFolders ?? 0;
            if (failedRoots.length || unreadableFolders) {
              const parts: string[] = [];
              if (failedRoots.length) parts.push(rootFailureText(failedRoots));
              if (unreadableFolders) parts.push(unreadableFoldersText(unreadableFolders));
              states.push({
                tool,
                host,
                state: kept || srows.length ? 'cached' : 'unreadable',
                lastScanAt: cached.kinds?.[tool]?.lastScanAt ?? null,
                rowCount: events,
                detail: `${parts.join('; ')}; ${kept || srows.length ? 'usage from readable logs is included' : 'nothing is counted from them'}`,
                cause: 'unreadable_folder',
              });
              continue;
            }
            const limit = kind?.searchLimit;
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
                  : kind?.unfinished?.length
                    ? `${unfinishedText(kind.unfinished, kind.unfinishedCount)}; ${kept ? 'showing what was read so far' : 'nothing counted yet'}`
                    : kind?.unreadableRecords
                      ? `${kind.unreadableRecords.toLocaleString('en-US')} ${tool === 'cursor' ? 'Cursor runs' : 'replies'} could not be read (${tool === 'cursor' ? "their token counts fit neither of the SDK's two conventions, or their usage JSON is damaged" : 'their token counts or time are missing'}); they are counted as unreadable, never guessed`
                      : limit
                        ? `search limit reached: ${limit.found === null ? 'additional folders may remain' : `${limit.read.toLocaleString('en-US')} of ${limit.found.toLocaleString('en-US')} folders checked`}; discovery is incomplete; ${kept ? 'showing what was read' : 'nothing counted yet'}`
                        : tool === 'codex' && kind?.compressedUnread
                          ? `${kind.compressedUnread} compressed Codex logs could not be read on this computer (needs Python 3.14 or the zstd tool); ${kept ? 'showing previously read aggregates' : 'waiting for readable logs'}`
                          : kind?.readFailures
                            ? `${kind.readFailures} logs could not be read on this computer; ${kept ? 'showing previously read usage' : 'waiting for readable logs'}`
                            : oversized
                              ? 'oversized log lines may contain usage that could not be read; showing partial aggregates'
                              : kind?.unconfirmedFiles
                                ? `${kind.unconfirmedFiles.toLocaleString('en-US')} previously read log${kind.unconfirmedFiles === 1 ? ' was' : 's were'} not confirmed this time; their earlier usage remains included`
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
            detail: response.kinds[tool]?.optionalSearchLimited
              ? 'known log folders were read; the optional search for additional experiment folders hit its limit, so other locations may exist'
              : tool === 'omp' && response.discoveryTruncated === true
                ? 'the search for custom OMP session folders hit its bounds; folders it did not reach are not read'
                : (tool === 'zcode' || tool === 'cursor' || tool === 'devin') &&
                    response.kinds[tool]?.walUnread === true
                  ? `${tool === 'zcode' ? "zcode's" : tool === 'cursor' ? "Cursor's" : "Devin's"} newest usage waits in its write-ahead log, which a read-only open here cannot read; it appears once the tool checkpoints it`
                  : tool === 'antigravity' && response.kinds.antigravity?.unreadable === true
                    ? 'some Antigravity conversation databases could not be read (busy or damaged); the rest are counted and the next scan reads them again'
                    : kept
                      ? null
                      : Object.keys(freshPrints[tool] ?? {}).length > 0
                        ? 'usage logs found but no usage in the last 31 days'
                        : 'no usage recorded in the last 31 days',
          });
        }
        // Every cell of this computer carries what discovery found for its tool, and the reader.
        for (const entry of states.slice(firstState)) {
          if (entry.host !== host) continue;
          const found = discovery.kinds[entry.tool as AnalyticsRemoteKind];
          applyFreshness(entry, found);
          if (!entry.counts) Object.assign(entry, sourceCounts(found), { reader });
          // A log folder that could not be reached is said, never silently left out (16.4).
          const links = (found?.roots ?? []).filter(
            (root) => root.state === 'unreadable' || root.state.startsWith('link_')
          );
          if (links.length && entry.state !== 'unreadable' && entry.cause !== 'unreadable_folder') {
            const note = rootFailureText(links);
            entry.detail = entry.detail ? `${entry.detail}; ${note}` : note;
          }
          if (
            response.kinds[entry.tool as AnalyticsRemoteKind]?.optionalSearchLimited &&
            !entry.detail?.includes('optional search')
          ) {
            const note =
              'the optional search for additional experiment folders hit its limit, so other locations may exist';
            entry.detail = entry.detail ? `${entry.detail}; ${note}` : note;
          }
          // A network folder that did not answer may hold logs a walk from home would have found.
          if (response.unansweredMounts) {
            const count = response.unansweredMounts;
            const note = `${count} network folder${count === 1 ? '' : 's'} on this computer did not answer, so logs there are not counted`;
            entry.detail = entry.detail ? `${entry.detail}; ${note}` : note;
          }
        }
        for (const entry of states.slice(firstState)) {
          // Hosts scan concurrently into one list: save only this computer's own notes.
          if (entry.host !== host) continue;
          const read = kindReads[entry.tool as AnalyticsRemoteKind];
          if (read) {
            if (entry.state === 'stale') read.state = 'stale';
            read.detail = entry.detail;
            read.cause = entry.cause ?? undefined;
          }
        }
        try {
          const saved = saveCache(file, {
            ...(owned && Object.keys(owned).length ? { owned } : {}),
            version: CACHE_VERSION,
            fingerprints: prints,
            rows: merged,
            srows: mergedSessions,
            lastScanAt: scannedAt,
            kinds: kindReads,
            discovery,
            ...(toolsAt ? { toolsAt } : {}),
            sessions,
            reader,
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
            settled,
            // A discard is named until a complete read; a trim while its cut is in range.
            ...(cached.discarded && !settled ? { discarded: cached.discarded } : {}),
            ...(cached.trimmed && Date.parse(cached.trimmed.before) > minDateMs
              ? { trimmed: cached.trimmed }
              : {}),
          });
          if (saved) cacheMarks.set(host, { ...saved, discarded: cached.discarded });
        } catch {
          /* Cache writes are best-effort; the rows are still served. */
        }
        states.push(...noReaderStates(host, discovery.tools, toolsAt ?? null));
        onHostScan(host, 'done');
      })()
    );
  }
  await Promise.all(jobs);
  for (const list of [sharedStates, localStates])
    for (const [host, marks] of cacheMarks) {
      const noted = withCacheNote(
        list.filter((state) => state.host === host),
        marks,
        minDateMs
      );
      let next = 0;
      for (let index = 0; index < list.length; index++)
        if (list[index].host === host) list[index] = noted[next++];
    }
  // The local scanner's kinds join only once owned; until then the collector's numbers stand and
  // its cell says the scanner is catching up. One step: the collection that sees the first complete
  // read serves the scanner's rows, and the caller drops the collector's.
  const catchingUp: Partial<Record<AnalyticsRemoteKind, string>> = {};
  for (const entry of localResults) if (localOwned.has(entry.tool)) sharedResults.push(entry);
  for (const entry of localStates) {
    const tool = entry.tool as AnalyticsRemoteKind;
    if (!LOCAL_SCANNER_KINDS.includes(tool) || localOwned.has(tool)) sharedStates.push(entry);
    else catchingUp[tool] = catchUpText(entry);
  }
  states.splice(0, states.length, ...coalesceSourceStates(states));
  const owners = electClaudeMirrorOwners(cacheDir, parts);
  const reconcileClaudeMirrors = mirrorReconciler(
    parts,
    results,
    states,
    () => deps.claudeLogNames?.() ?? localClaudeLogNames(cacheDir, minDateMs),
    owners
  );
  reconcileClaudeMirrors();
  return {
    results,
    states,
    fetchedAt: new Date(now()).toISOString(),
    reconcileClaudeMirrors,
    local: localReady.ok
      ? {
          owned: [...localOwned],
          catchingUp,
          freshness: localFreshness,
          ...(local.overflow.length ? { tooManyRoots: local.overflow } : {}),
        }
      : { owned: [], catchingUp: {}, unavailable: localReady.cause, freshness: localFreshness },
  };
}
