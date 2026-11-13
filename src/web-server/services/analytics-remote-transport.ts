import { ANALYTICS_TOOLS, type AnalyticsTool } from '../usage/analytics-tools';
import { execFile } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { NetworkError, ValidationError } from '../../errors/error-types';
import { readAnalyticsHelper } from './analytics-remote-helper';
import { HOST_OS, REMOTE_HOSTS, SSH_ALIASES, isSafeUsageSshAlias } from './dashboard-hosts';
import { sshArgv } from './ssh-policy';
import {
  UsageScanInstallError,
  binaryMarker,
  installUsageScan,
  localUsageScan,
  readUsageScanManifest,
  scanCommand,
  type UsageScanManifest,
} from './usage-scan-binary';

/**
 * The computers the packaged helper scans over ssh, in scan order: the Mac, Windows and Nas1. The
 * local Ubuntu computer is read by the local collectors, and by the same scanner run in place for
 * the kinds it owns there (`runLocalUsageScan`, DESIGN 16.0 B), never over ssh.
 */
export const REMOTE_ANALYTICS_HOSTS = REMOTE_HOSTS;
export type AnalyticsRemoteHost = (typeof REMOTE_ANALYTICS_HOSTS)[number];
/**
 * A computer the usage scanner reads: the remote ones over ssh, and (DESIGN 16.0 B) the
 * dashboard's own computer, `ubuntu`, run locally for the kinds the scanner owns there.
 */
export type AnalyticsScanHost = AnalyticsRemoteHost | 'ubuntu';
/** The ssh alias to reach each remote host, or null when none is configured. */
export type AnalyticsRemoteAliases = Record<AnalyticsRemoteHost, string | null>;
export type AnalyticsRemoteKind = AnalyticsTool;

/** Every kind the packaged helper reads. */
const REMOTE_KIND_LIST = ANALYTICS_TOOLS;
const REMOTE_KINDS: ReadonlySet<string> = new Set(REMOTE_KIND_LIST);

export {
  ANALYTICS_HELPER_SHA256,
  AnalyticsHelperIntegrityError,
  analyticsHelperPath,
} from './analytics-remote-helper';

/**
 * Why a computer's scan failed, as one fixed word (the usage-report contract's `cause`):
 * - `ssh_unreachable`: ssh could not connect (exit 255 without an authentication refusal);
 * - `ssh_auth`: the computer refused this account's key;
 * - `ssh_host_key`: this side refused the computer: its host key is unknown or changed (known_hosts);
 * - `runtime_missing`: no Python 3.9 or later; `runtime_missing:store_alias` when the only
 *   `python3.exe`/`python.exe` found is the Microsoft Store alias stub;
 * - `helper_failed`: the helper exited non-zero for another reason (its exit code is kept);
 * - `timeout`: the ssh call outlived its timeout;
 * - `bad_response`: the answer was not the helper's bounded JSON.
 */
export type AnalyticsScanFailureCause =
  | 'ssh_unreachable'
  | 'ssh_auth'
  | 'ssh_host_key'
  | 'runtime_missing'
  | 'runtime_missing:store_alias'
  | 'helper_failed'
  | 'timeout'
  | 'bad_response';

export const ANALYTICS_SCAN_FAILURE_CAUSES: readonly AnalyticsScanFailureCause[] = [
  'ssh_unreachable',
  'ssh_auth',
  'ssh_host_key',
  'runtime_missing',
  'runtime_missing:store_alias',
  'helper_failed',
  'timeout',
  'bad_response',
];

export class AnalyticsRemoteTransportError extends NetworkError {
  readonly timedOut: boolean;
  readonly cause: AnalyticsScanFailureCause;
  readonly exitCode: number | null;

  constructor(
    timedOut = false,
    cause: AnalyticsScanFailureCause = timedOut ? 'timeout' : 'bad_response',
    exitCode: number | null = null
  ) {
    super(timedOut ? 'Analytics remote request timed out.' : 'Analytics remote request failed.');
    this.name = 'AnalyticsRemoteTransportError';
    this.timedOut = timedOut;
    this.cause = cause;
    this.exitCode = exitCode;
  }
}

/** The launcher's exit code when it found no working Python (analyticsHelperCommand). */
export const HELPER_RUNTIME_MISSING_EXIT = 86;

/**
 * The fixed cause of a failed ssh helper call, from its exit code and stderr. Only fixed markers
 * are matched; stderr itself is never kept, logged or shown.
 */
export function classifyAnalyticsScanFailure(failure: {
  exitCode: number | null;
  killed?: boolean;
  signal?: string | null;
  stderr?: string;
}): AnalyticsScanFailureCause {
  const stderr = failure.stderr ?? '';
  if (failure.killed === true && failure.signal === 'SIGTERM') return 'timeout';
  if (stderr.includes('aac-helper: runtime_missing:store_alias'))
    return 'runtime_missing:store_alias';
  if (
    failure.exitCode === HELPER_RUNTIME_MISSING_EXIT ||
    stderr.includes('aac-helper: runtime_missing')
  )
    return 'runtime_missing';
  // The Store alias stub itself ("Python was not found; ... App execution aliases"): exit 9009 from
  // cmd, 49 started directly. An older launcher still meets it.
  if (/Python was not found/i.test(stderr) || failure.exitCode === 9009)
    return 'runtime_missing:store_alias';
  if (failure.exitCode === 255) {
    // known_hosts refusing the computer is not the computer refusing this account's key.
    if (
      /Host key verification failed|REMOTE HOST IDENTIFICATION HAS CHANGED|No .* host key is known/i.test(
        stderr
      )
    )
      return 'ssh_host_key';
    return /Permission denied|Too many authentication failures/i.test(stderr)
      ? 'ssh_auth'
      : 'ssh_unreachable';
  }
  return 'helper_failed';
}

const SSH_TIMEOUT_MS = 30_000;
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;
const MAX_ROWS = 100_000;

export interface AnalyticsRemoteFingerprint {
  size: number;
  mtimeMs: number;
  head?: string;
  tail?: string;
  /** Claude's basename digest and SSH mirror flag; no filenames or paths travel. */
  claudeName?: string;
  claudeMirror?: boolean;
  /** Stable copy-group identity, including across unreadable or removed owners. */
  claudeGroup?: string;
  /** An oversized usage-capable line was skipped, even on an unchanged file. */
  usageSkipped?: boolean;
  /** zcode only: the write-ahead log, where new rows wait for a checkpoint. */
  walSize?: number;
  walMtimeMs?: number;
  /**
   * A read the deadline cut (DESIGN 16.1): the decoded byte offset after its last complete line.
   * The next call reads on from there while the file is unchanged.
   */
  readThrough?: number;
  /** This answer's rows for the file continue an earlier read from this offset: add, not replace. */
  readFrom?: number;
  /** The reader's carry-over between lines: model, provider, running totals, a session digest. */
  readState?: AnalyticsReadState;
  /** The file name without its conversation id, to name a log not read to its end. */
  readName?: string;
  /** The decoded size when the file states it. */
  decodedSize?: number;
}

/** Codex's carry-over for a resumed read; no identifier (the session travels as a digest). */
export interface AnalyticsReadState {
  model?: string | null;
  provider?: string | null;
  previous?: number[] | null;
  sessionDigest?: string | null;
}

/** A log not read to its end, named for the page (no conversation id). */
export interface AnalyticsUnfinishedLog {
  name: string;
  sizeBytes: number;
  readThroughBytes: number | null;
  decodedBytes: number | null;
  reason: 'deadline' | 'unreadable';
}

export interface AnalyticsRemoteRow {
  k: AnalyticsRemoteKind;
  f: string;
  m: string;
  p?: string;
  h: string;
  i: number;
  o: number;
  cr: number;
  cw: number;
  c: number;
  n: number;
}

/**
 * One per-session-model aggregate: the session's published key plus its first
 * (`a`) and last (`z`) event in epoch milliseconds. The helper derives the key
 * on the host that read the log (`_session_key`), so no session id, path or
 * conversation content travels; the projection publishes the key unchanged.
 */
export interface AnalyticsRemoteSessionRow {
  k: AnalyticsRemoteKind;
  f: string;
  s: string;
  m: string;
  p?: string;
  a: number;
  z: number;
  i: number;
  o: number;
  cr: number;
  cw: number;
  c: number;
  n: number;
}

export interface AnalyticsRemoteKindResult {
  /** `error`: the data exists but could not be read; nothing of it was confirmed. */
  /** `pending`: the scan ran out of time before it reached the kind; none of its files were visited. */
  state: 'ok' | 'not_installed' | 'error' | 'pending';
  /** A cap, deadline or unreadable log left this kind's own scan incomplete. */
  partial?: boolean;
  /** Codex rollouts whose compressed bytes could not be decoded on this host. */
  compressedUnread?: number;
  /** Distinct logs whose fingerprint or read failed this scan. */
  readFailures?: number;
  /** Claude files atomically replaced by this answer's copy-group aggregates. */
  replacedFiles?: string[];
  fingerprints: Record<string, AnalyticsRemoteFingerprint>;
  /** zcode: an immutable open cannot read rows still in the write-ahead log. */
  walUnread?: boolean;
  /** Logs not read to their end (at most 20, largest first), and how many there are in all. */
  unfinished?: AnalyticsUnfinishedLog[];
  unfinishedCount?: number;
  /** Antigravity: a conversation database or folder could not be read (busy, damaged); the rest count. */
  unreadable?: boolean;
}

/** Where a root came from (the usage-report contract's `origin`). */
export type AnalyticsRootOrigin =
  | 'default'
  | 'env'
  | 'tool_config'
  | 't3_instance'
  | 'ccs_instance'
  | 'signature'
  | 'extra'
  | 'experiment';
const ROOT_ORIGINS: ReadonlySet<string> = new Set([
  'default',
  'env',
  'tool_config',
  't3_instance',
  'ccs_instance',
  'signature',
  'extra',
  'experiment',
]);
/** Why a computer has a tool apart from its logs. */
const SIGNALS: ReadonlySet<string> = new Set(['home', 'binary', 't3_instance']);
/** Tools Analytics has no usage reader for; the scanner reports only where they are present. */
export const NO_READER_TOOLS = [
  'cursor',
  'gemini-cli',
  'opencode',
  'qwen',
  'kimi',
  'grok',
  'kiro',
  'pi',
  'fx',
] as const;
export type AnalyticsNoReaderTool = (typeof NO_READER_TOOLS)[number];
const NO_READER: ReadonlySet<string> = new Set(NO_READER_TOOLS);
const NO_READER_CAUSES: ReadonlySet<string> = new Set(['no_local_usage_records', 'no_reader_yet']);

export interface AnalyticsDiscoveryCounts {
  roots: number;
  filesFound: number;
  filesRead: number;
  filesFailed: number;
  /** Epoch ms of the newest log found, null when none. */
  newestMtime: number | null;
}

export interface AnalyticsDiscoveryRoot {
  /** First 16 hex of sha256("aac-root-v1:" + tool + ":" + origin + ":" + path with home as ~). */
  root: string;
  origin: AnalyticsRootOrigin;
  state: 'ok' | 'installed_no_logs';
  counts: { filesFound: number; newestMtime: number | null };
}

/** One requested kind's discovery: install signals, its roots and counts. */
export interface AnalyticsDiscoveryKind {
  installed: boolean;
  signals: string[];
  roots: AnalyticsDiscoveryRoot[];
  counts: AnalyticsDiscoveryCounts;
}

/** A tool without a reader that this computer has. */
export interface AnalyticsPresenceTool {
  installed: boolean;
  signals: string[];
  cause: 'no_local_usage_records' | 'no_reader_yet';
  counts: { filesFound: number; newestMtime: number | null };
  partial?: boolean;
}

export interface AnalyticsRemoteDiscovery {
  kinds: Partial<Record<AnalyticsRemoteKind, AnalyticsDiscoveryKind>>;
  /** Present only in the answer of the call that asked for presence. */
  tools?: Partial<Record<AnalyticsNoReaderTool, AnalyticsPresenceTool>>;
}

/** Identity evidence of one session (DB audit 2): its full hash and the working folder its log states. */
export interface AnalyticsSessionEvidence {
  hash: string;
  cwd: {
    key: string;
    path: string;
    /** An inferred join only, never a measured attribution (history DESIGN 1.3). */
    identitySource: 'inferred_cwd';
    confidence: 'inferred';
  } | null;
}

/** Which reader answered: the Rust scanner, or the Python helper standing in (with why). */
export interface AnalyticsReader {
  name: 'aac-usage-scan' | 'python-fallback';
  version?: string;
  protocol?: number;
  target?: string;
  cause?: string;
}

const count = (value: unknown, max = 10_000_000): value is number =>
  Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= max;
const epoch = (value: unknown): value is number | null => value === null || count(value, 2 ** 52);
const signalsOk = (value: unknown): value is string[] =>
  Array.isArray(value) &&
  value.length <= 3 &&
  value.every((signal) => typeof signal === 'string' && SIGNALS.has(signal));

function parseDiscoveryKind(value: unknown): AnalyticsDiscoveryKind | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const entry = value as Record<string, unknown>;
  const counts = entry.counts as Record<string, unknown> | undefined;
  if (
    typeof entry.installed !== 'boolean' ||
    !signalsOk(entry.signals) ||
    !Array.isArray(entry.roots) ||
    entry.roots.length > 64 ||
    !counts ||
    typeof counts !== 'object' ||
    !count(counts.roots, 64) ||
    !count(counts.filesFound) ||
    !count(counts.filesRead) ||
    !count(counts.filesFailed) ||
    !epoch(counts.newestMtime)
  )
    return null;
  const roots: AnalyticsDiscoveryRoot[] = [];
  for (const root of entry.roots as unknown[]) {
    if (!root || typeof root !== 'object') return null;
    const item = root as Record<string, unknown>;
    const rootCounts = item.counts as Record<string, unknown> | undefined;
    if (
      typeof item.root !== 'string' ||
      !/^[0-9a-f]{16}$/.test(item.root) ||
      typeof item.origin !== 'string' ||
      !ROOT_ORIGINS.has(item.origin) ||
      (item.state !== 'ok' && item.state !== 'installed_no_logs') ||
      !rootCounts ||
      !count(rootCounts.filesFound) ||
      !epoch(rootCounts.newestMtime)
    )
      return null;
    roots.push({
      root: item.root,
      origin: item.origin as AnalyticsRootOrigin,
      state: item.state,
      counts: {
        filesFound: rootCounts.filesFound as number,
        newestMtime: rootCounts.newestMtime as number | null,
      },
    });
  }
  return {
    installed: entry.installed,
    signals: [...(entry.signals as string[])],
    roots,
    counts: {
      roots: counts.roots as number,
      filesFound: counts.filesFound as number,
      filesRead: counts.filesRead as number,
      filesFailed: counts.filesFailed as number,
      newestMtime: counts.newestMtime as number | null,
    },
  };
}

function parsePresence(value: unknown): AnalyticsPresenceTool | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const entry = value as Record<string, unknown>;
  const counts = entry.counts as Record<string, unknown> | undefined;
  if (
    typeof entry.installed !== 'boolean' ||
    !signalsOk(entry.signals) ||
    typeof entry.cause !== 'string' ||
    !NO_READER_CAUSES.has(entry.cause) ||
    !counts ||
    !count(counts.filesFound) ||
    !epoch(counts.newestMtime) ||
    (entry.partial !== undefined && typeof entry.partial !== 'boolean')
  )
    return null;
  return {
    installed: entry.installed,
    signals: [...(entry.signals as string[])],
    cause: entry.cause as AnalyticsPresenceTool['cause'],
    counts: {
      filesFound: counts.filesFound as number,
      newestMtime: counts.newestMtime as number | null,
    },
    ...(entry.partial === true ? { partial: true } : {}),
  };
}

/**
 * The scanner's discovery section, entry by entry: a malformed entry is dropped, never trusted,
 * and never fails the usage rows it travels with. An older helper's answer has none.
 */
export function parseAnalyticsDiscovery(value: unknown): AnalyticsRemoteDiscovery | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const raw = value as { kinds?: unknown; tools?: unknown };
  const discovery: AnalyticsRemoteDiscovery = { kinds: {} };
  if (raw.kinds && typeof raw.kinds === 'object' && !Array.isArray(raw.kinds))
    for (const [kind, entry] of Object.entries(raw.kinds as Record<string, unknown>)) {
      if (!REMOTE_KINDS.has(kind)) continue;
      const parsed = parseDiscoveryKind(entry);
      if (parsed) discovery.kinds[kind as AnalyticsRemoteKind] = parsed;
    }
  if (raw.tools && typeof raw.tools === 'object' && !Array.isArray(raw.tools)) {
    discovery.tools = {};
    for (const [tool, entry] of Object.entries(raw.tools as Record<string, unknown>)) {
      if (!NO_READER.has(tool)) continue;
      const parsed = parsePresence(entry);
      if (parsed) discovery.tools[tool as AnalyticsNoReaderTool] = parsed;
    }
  }
  return discovery;
}

/** Identity evidence keyed by session key; a malformed entry is dropped. At most 50,000. */
export function parseAnalyticsSessions(
  value: unknown
): Record<string, AnalyticsSessionEvidence> | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const result: Record<string, AnalyticsSessionEvidence> = {};
  let kept = 0;
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    if (kept >= 50_000) break;
    if (!SESSION_KEY_HEX.test(key) || !entry || typeof entry !== 'object') continue;
    const { hash, cwd } = entry as { hash?: unknown; cwd?: unknown };
    if (typeof hash !== 'string' || !SHA256_HEX.test(hash) || !hash.startsWith(key)) continue;
    let evidence: AnalyticsSessionEvidence['cwd'] = null;
    if (cwd !== null && cwd !== undefined) {
      const item = cwd as Record<string, unknown>;
      if (
        typeof item.key !== 'string' ||
        !SESSION_KEY_HEX.test(item.key) ||
        !cleanText(item.path, 1024) ||
        item.identitySource !== 'inferred_cwd' ||
        item.confidence !== 'inferred'
      )
        continue;
      evidence = {
        key: item.key,
        path: item.path,
        identitySource: 'inferred_cwd',
        confidence: 'inferred',
      };
    }
    result[key] = { hash, cwd: evidence };
    kept++;
  }
  return result;
}

function parseReader(value: unknown): AnalyticsReader | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const item = value as Record<string, unknown>;
  if (item.name !== 'aac-usage-scan') return undefined;
  if (!cleanText(item.version, 32) || !count(item.protocol, 1000) || !cleanText(item.target, 64))
    return undefined;
  return {
    name: 'aac-usage-scan',
    version: item.version,
    protocol: item.protocol,
    target: item.target,
  };
}

export interface AnalyticsRemoteResponse {
  /** v1 is readable for non-Claude tools only; v2 guarantees Claude copy-group replacements. */
  version: 1 | 2;
  /** A file, row or deadline cap stopped the scan: some files were not read. */
  truncated: boolean;
  /** Only the search for custom OMP session roots hit its bounds. */
  discoveryTruncated?: boolean;
  kinds: Record<AnalyticsRemoteKind, AnalyticsRemoteKindResult>;
  rows: AnalyticsRemoteRow[];
  srows: AnalyticsRemoteSessionRow[];
  /** Where the logs were found (the Rust scanner's answers only). */
  discovery?: AnalyticsRemoteDiscovery;
  /** Identity evidence per session key (the Rust scanner's answers only). */
  sessions?: Record<string, AnalyticsSessionEvidence>;
  /** Which reader answered. */
  reader?: AnalyticsReader;
}

/**
 * v1 predates atomic Claude ownership changes, even though some v1 readers
 * emitted replacement metadata. Require an explicit v2 declaration and the
 * metadata itself; a future reader must not silently restore file-only merges.
 * Unchanged or withheld groups legitimately emit no replacement list or rows.
 */
export function hasClaudeReplacementMetadata(response: AnalyticsRemoteResponse): boolean {
  const kind = response.kinds.claude;
  if (response.version !== 2 || !kind) return false;
  if (kind.state !== 'ok') return true;
  if (Object.values(kind.fingerprints).some((print) => !print.claudeGroup)) return false;
  const replaced = new Set(kind.replacedFiles);
  const confirmed = (row: AnalyticsRemoteRow | AnalyticsRemoteSessionRow) =>
    row.k !== 'claude' || replaced.has(row.f);
  return response.rows.every(confirmed) && response.srows.every(confirmed);
}

export interface AnalyticsRemoteRequest {
  kinds: AnalyticsRemoteKind[];
  minDateMs: number;
  fingerprints: Record<string, Record<string, AnalyticsRemoteFingerprint>>;
  deadlineMs?: number;
  /**
   * Saved extra usage-log locations for the target host, from Settings.
   * claude entries are projects directories, codex entries codex homes
   * (sessions hangs under each), omp/muse entries session-root directories,
   * zcode entries database files; all are validated absolute paths before
   * they leave this host.
   */
  extraRoots?: Partial<Record<AnalyticsRemoteKind, string[]>>;
  /**
   * Rust scanner only: log folders an experiment harness kept (DESIGN 16.5), read through the
   * copy rule with origin `experiment`. On the dashboard's own computer, the background walk's.
   */
  experimentRoots?: Partial<Record<AnalyticsRemoteKind, string[]>>;
  /** Rust scanner only: also report which tools without a reader the computer has. */
  presence?: boolean;
  /** Rust scanner only: send each session's working folder (home as ~); the cwd capture setting. */
  cwd?: boolean;
}

function quoteShell(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

const MAX_EXTRA_ROOTS = 16;

function isValidExtraRoot(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= 1024 &&
    !value.includes('\0') &&
    !value.includes('..') &&
    (path.posix.isAbsolute(value) || /^[A-Za-z]:[\\/]/.test(value))
  );
}

/** At most this many experiment folders per kind travel in one request. */
export const MAX_EXPERIMENT_ROOTS = 256;

function isValidExperimentRoots(
  value: unknown
): value is Partial<Record<AnalyticsRemoteKind, string[]>> {
  if (value === undefined) return true;
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  return Object.entries(value as Record<string, unknown>).every(
    ([kind, roots]) =>
      (kind === 'claude' || kind === 'codex' || kind === 'omp' || kind === 'muse') &&
      Array.isArray(roots) &&
      roots.length <= MAX_EXPERIMENT_ROOTS &&
      (roots as unknown[]).every(isValidExtraRoot)
  );
}

function isValidExtraRoots(
  value: unknown
): value is Partial<Record<AnalyticsRemoteKind, string[]>> {
  if (value === undefined) return true;
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  return Object.entries(value as Record<string, unknown>).every(
    ([kind, roots]) =>
      // Antigravity takes no saved extra roots: the helper reads ANTIGRAVITY_DATA_DIR itself.
      (kind === 'claude' ||
        kind === 'codex' ||
        kind === 'omp' ||
        kind === 'muse' ||
        kind === 'zcode') &&
      Array.isArray(roots) &&
      roots.length <= MAX_EXTRA_ROOTS &&
      (roots as unknown[]).every(isValidExtraRoot)
  );
}

/** File keys and head/tail fingerprints are SHA-256 hex digests on the helper side; anything else is refused. */
const SHA256_HEX = /^[0-9a-f]{64}$/;
const CONTROL = /[\u0000-\u001f\u007f]/;
/** A session key is the helper's truncated digest of a session id; the id itself never travels. */
const SESSION_KEY_HEX = /^[0-9a-f]{16}$/;

function cleanText(value: unknown, max: number): value is string {
  return (
    typeof value === 'string' && value.length > 0 && value.length <= max && !CONTROL.test(value)
  );
}

function nonNegative(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function validRow(value: unknown): value is AnalyticsRemoteRow {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  return (
    typeof row.k === 'string' &&
    REMOTE_KINDS.has(row.k) &&
    typeof row.f === 'string' &&
    SHA256_HEX.test(row.f) &&
    cleanText(row.m, 160) &&
    (row.p === undefined || cleanText(row.p, 64)) &&
    typeof row.h === 'string' &&
    /^\d{4}-\d{2}-\d{2} \d{2}:00$/.test(row.h) &&
    nonNegative(row.i) &&
    nonNegative(row.o) &&
    nonNegative(row.cr) &&
    nonNegative(row.cw) &&
    nonNegative(row.c) &&
    nonNegative(row.n)
  );
}

function validSessionRow(value: unknown): value is AnalyticsRemoteSessionRow {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  return (
    typeof row.k === 'string' &&
    REMOTE_KINDS.has(row.k) &&
    typeof row.f === 'string' &&
    SHA256_HEX.test(row.f) &&
    typeof row.s === 'string' &&
    SESSION_KEY_HEX.test(row.s) &&
    cleanText(row.m, 160) &&
    (row.p === undefined || cleanText(row.p, 64)) &&
    nonNegative(row.a) &&
    nonNegative(row.z) &&
    (row.z as number) >= (row.a as number) &&
    nonNegative(row.i) &&
    nonNegative(row.o) &&
    nonNegative(row.cr) &&
    nonNegative(row.cw) &&
    nonNegative(row.c) &&
    nonNegative(row.n)
  );
}

function validFingerprint(value: unknown): value is AnalyticsRemoteFingerprint {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const print = value as Record<string, unknown>;
  return (
    nonNegative(print.size) &&
    nonNegative(print.mtimeMs) &&
    (print.head === undefined || (typeof print.head === 'string' && SHA256_HEX.test(print.head))) &&
    (print.tail === undefined || (typeof print.tail === 'string' && SHA256_HEX.test(print.tail))) &&
    (print.claudeName === undefined ||
      (typeof print.claudeName === 'string' && SHA256_HEX.test(print.claudeName))) &&
    (print.claudeMirror === undefined || typeof print.claudeMirror === 'boolean') &&
    (print.claudeGroup === undefined ||
      (typeof print.claudeGroup === 'string' && SHA256_HEX.test(print.claudeGroup))) &&
    (print.usageSkipped === undefined || typeof print.usageSkipped === 'boolean') &&
    (print.walSize === undefined || nonNegative(print.walSize)) &&
    (print.walMtimeMs === undefined || nonNegative(print.walMtimeMs)) &&
    (print.readThrough === undefined || nonNegative(print.readThrough)) &&
    (print.readFrom === undefined || nonNegative(print.readFrom)) &&
    (print.readState === undefined || validReadState(print.readState)) &&
    (print.readName === undefined || validLogName(print.readName)) &&
    (print.decodedSize === undefined || nonNegative(print.decodedSize))
  );
}

const UUID_IN_TEXT = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

/** A log's display name: printable ASCII, at most 120 characters, never a conversation id. */
function validLogName(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= 120 &&
    /^[\x21-\x7e]+$/.test(value) &&
    !UUID_IN_TEXT.test(value)
  );
}

function validReadState(value: unknown): value is AnalyticsReadState {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const state = value as Record<string, unknown>;
  const text = (item: unknown, limit: number) =>
    item === undefined || item === null || cleanText(item, limit);
  return (
    Object.keys(state).every((key) =>
      ['model', 'provider', 'previous', 'sessionDigest'].includes(key)
    ) &&
    text(state.model, 200) &&
    text(state.provider, 64) &&
    (state.previous === undefined ||
      state.previous === null ||
      (Array.isArray(state.previous) &&
        state.previous.length === 4 &&
        state.previous.every((n) => typeof n === 'number' && Number.isFinite(n) && n >= 0))) &&
    (state.sessionDigest === undefined ||
      state.sessionDigest === null ||
      (typeof state.sessionDigest === 'string' && /^[0-9a-f]{16}$/.test(state.sessionDigest)))
  );
}

/** A list of logs not read to their end, or undefined when absent or malformed. */
export function parseUnfinished(value: unknown): AnalyticsUnfinishedLog[] | undefined {
  if (!Array.isArray(value) || value.length > 20) return undefined;
  const list: AnalyticsUnfinishedLog[] = [];
  for (const item of value) {
    const entry = item as Record<string, unknown> | null;
    if (
      !entry ||
      typeof entry !== 'object' ||
      !validLogName(entry.name) ||
      !nonNegative(entry.sizeBytes) ||
      (entry.readThroughBytes !== null && !nonNegative(entry.readThroughBytes)) ||
      (entry.decodedBytes !== null && !nonNegative(entry.decodedBytes)) ||
      (entry.reason !== 'deadline' && entry.reason !== 'unreadable')
    )
      return undefined;
    list.push({
      name: entry.name,
      sizeBytes: entry.sizeBytes as number,
      readThroughBytes: (entry.readThroughBytes as number | null) ?? null,
      decodedBytes: (entry.decodedBytes as number | null) ?? null,
      reason: entry.reason,
    });
  }
  return list;
}

/** Strict shape check: aggregates only, bounded, never trusted blindly. */
export function parseAnalyticsRemoteResponse(stdout: string | Buffer): AnalyticsRemoteResponse {
  const text = Buffer.isBuffer(stdout) ? stdout.toString('utf8') : stdout;
  if (Buffer.byteLength(text, 'utf8') > MAX_RESPONSE_BYTES)
    throw new AnalyticsRemoteTransportError();
  let response: Record<string, unknown>;
  try {
    response = JSON.parse(text) as Record<string, unknown>;
  } catch {
    throw new AnalyticsRemoteTransportError();
  }
  if (
    (response.version !== 1 && response.version !== 2) ||
    typeof response.truncated !== 'boolean' ||
    (response.discoveryTruncated !== undefined && typeof response.discoveryTruncated !== 'boolean')
  ) {
    throw new AnalyticsRemoteTransportError();
  }
  const kinds = response.kinds as Record<string, unknown> | undefined;
  const rows = response.rows as unknown;
  const srows = response.srows as unknown;
  if (!kinds || typeof kinds !== 'object' || !Array.isArray(rows) || rows.length > MAX_ROWS) {
    throw new AnalyticsRemoteTransportError();
  }
  if (!rows.every(validRow)) throw new AnalyticsRemoteTransportError();
  // Session aggregates travel with the hourly rows; an answer without them
  // (an older helper in a test fixture) carries hourly usage only.
  if (srows !== undefined && (!Array.isArray(srows) || srows.length > MAX_ROWS))
    throw new AnalyticsRemoteTransportError();
  if (Array.isArray(srows) && !srows.every(validSessionRow))
    throw new AnalyticsRemoteTransportError();
  const parsed: AnalyticsRemoteResponse['kinds'] = {} as AnalyticsRemoteResponse['kinds'];
  for (const [kind, value] of Object.entries(kinds)) {
    if (!REMOTE_KINDS.has(kind)) throw new AnalyticsRemoteTransportError();
    const entry = value as {
      state?: unknown;
      partial?: unknown;
      compressedUnread?: unknown;
      readFailures?: unknown;
      replacedFiles?: unknown;
      fingerprints?: unknown;
      walUnread?: unknown;
      unreadable?: unknown;
      unfinished?: unknown;
      unfinishedCount?: unknown;
    };
    if (
      entry.state !== 'ok' &&
      entry.state !== 'not_installed' &&
      entry.state !== 'error' &&
      entry.state !== 'pending' &&
      entry.state !== undefined
    )
      throw new AnalyticsRemoteTransportError();
    if (entry.partial !== undefined && typeof entry.partial !== 'boolean')
      throw new AnalyticsRemoteTransportError();
    if (
      entry.compressedUnread !== undefined &&
      (kind !== 'codex' ||
        !Number.isSafeInteger(entry.compressedUnread) ||
        (entry.compressedUnread as number) < 1 ||
        (entry.compressedUnread as number) > 20_000 ||
        entry.partial !== true)
    )
      throw new AnalyticsRemoteTransportError();
    if (entry.walUnread !== undefined && typeof entry.walUnread !== 'boolean')
      throw new AnalyticsRemoteTransportError();
    if (
      entry.readFailures !== undefined &&
      (!Number.isSafeInteger(entry.readFailures) ||
        (entry.readFailures as number) < 1 ||
        (entry.readFailures as number) > 20_000 ||
        entry.partial !== true)
    )
      throw new AnalyticsRemoteTransportError();
    if (
      entry.replacedFiles !== undefined &&
      (kind !== 'claude' ||
        !Array.isArray(entry.replacedFiles) ||
        entry.replacedFiles.length > 40_000 ||
        !entry.replacedFiles.every((key) => typeof key === 'string' && SHA256_HEX.test(key)) ||
        new Set(entry.replacedFiles).size !== entry.replacedFiles.length ||
        entry.state !== 'ok')
    )
      throw new AnalyticsRemoteTransportError();
    if (entry.unreadable !== undefined && typeof entry.unreadable !== 'boolean')
      throw new AnalyticsRemoteTransportError();
    // Naming is a diagnostic: a malformed list is dropped, the usage kept.
    const unfinished = parseUnfinished(entry.unfinished);
    const unfinishedCount =
      unfinished &&
      Number.isSafeInteger(entry.unfinishedCount) &&
      (entry.unfinishedCount as number) >= unfinished.length &&
      (entry.unfinishedCount as number) <= 20_000
        ? (entry.unfinishedCount as number)
        : unfinished?.length;
    const prints = entry.fingerprints as Record<string, unknown> | undefined;
    if (!prints || typeof prints !== 'object' || Array.isArray(prints))
      throw new AnalyticsRemoteTransportError();
    const entries = Object.entries(prints);
    if (
      entries.length > 20_000 ||
      !entries.every(([key, print]) => SHA256_HEX.test(key) && validFingerprint(print))
    )
      throw new AnalyticsRemoteTransportError();
    parsed[kind as AnalyticsRemoteKind] = {
      state:
        entry.state === 'not_installed'
          ? 'not_installed'
          : entry.state === 'error'
            ? 'error'
            : entry.state === 'pending'
              ? 'pending'
              : 'ok',
      ...(entry.partial === true ? { partial: true } : {}),
      ...(entry.compressedUnread !== undefined
        ? { compressedUnread: entry.compressedUnread as number }
        : {}),
      fingerprints: prints as Record<string, AnalyticsRemoteFingerprint>,
      ...(entry.readFailures !== undefined ? { readFailures: entry.readFailures as number } : {}),
      ...(entry.replacedFiles !== undefined
        ? { replacedFiles: entry.replacedFiles as string[] }
        : {}),
      ...(entry.walUnread === true ? { walUnread: true } : {}),
      ...(entry.unreadable === true ? { unreadable: true } : {}),
      ...(unfinished?.length ? { unfinished, unfinishedCount } : {}),
    };
  }
  const discovery = parseAnalyticsDiscovery(response.discovery);
  const sessions = parseAnalyticsSessions(response.sessions);
  const reader = parseReader(response.reader);
  return {
    version: response.version,
    truncated: response.truncated,
    ...(response.discoveryTruncated === true ? { discoveryTruncated: true } : {}),
    kinds: parsed,
    rows: rows as AnalyticsRemoteRow[],
    srows: (Array.isArray(srows) ? srows : []) as AnalyticsRemoteSessionRow[],
    ...(discovery ? { discovery } : {}),
    ...(sessions ? { sessions } : {}),
    ...(reader ? { reader } : {}),
  };
}

/** Where the POSIX launcher looks for Python, in order ("" when `python3` is not on PATH). */
export const POSIX_PYTHON_CANDIDATES = [
  '/usr/bin/python3',
  '/opt/homebrew/bin/python3',
  '/usr/local/bin/python3',
  '"$(command -v python3 2>/dev/null)"',
] as const;

/** Exits 0 under Python 3.9 or later; the launcher's probe, which never reads stdin. */
export const PYTHON_PROBE = 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 3)';

/**
 * The fixed ssh command that runs the packaged helper on a host. Only this process's helper source,
 * streamed on stdin with the request, is executed; aggregates travel back on stdout without
 * temporary files. Python reads the request from stdin itself on every host.
 *
 * The launcher finds a working Python 3.9 or later instead of trusting the first name on PATH:
 * each candidate runs PYTHON_PROBE (which never reads stdin) and the first that exits 0 runs the
 * helper. Without one, it exits HELPER_RUNTIME_MISSING_EXIT with a fixed marker on stderr.
 *
 * - POSIX (the Mac, Nas1 and other Linux computers, by OS kind): `/usr/bin/python3`, Homebrew's,
 *   `/usr/local`'s, then `python3` on PATH. A Mac without the Command Line Tools has a
 *   `/usr/bin/python3` stub that now fails the probe instead of the scan.
 * - Windows: PowerShell only finds Python and starts it. Candidates are `py.exe -3` (the Python
 *   launcher, which starts only a real install), then every `python.exe` and `python3.exe` on PATH
 *   (`Get-Command -All`), in PATH order. The Microsoft Store's App Execution Alias stub in
 *   `%LOCALAPPDATA%\Microsoft\WindowsApps` prints "Python was not found" and exits 9009: it fails
 *   the probe and is skipped (Alexis PC, 2026-11-13: it was first on PATH, so every scan failed),
 *   while a real Store install there passes (the owner's other PC). When every candidate failed and one was
 *   such a stub, the marker says `store_alias`. The probe loop runs under 'Continue': Windows
 *   PowerShell 5.1 turns a redirected native stderr line into a terminating error under 'Stop'.
 *   When PowerShell read a request of the helper's size from stdin (`[Console]::In.ReadToEnd()`)
 *   over Windows OpenSSH, about half the reads never saw the end of the input and hung until the
 *   ssh timeout (measured 2026-10-06), so Python reads the bytes itself, which keeps them UTF-8.
 */
export function analyticsHelperCommand(
  platform: AnalyticsRemoteHost,
  /** POSIX candidates in order; only tests pass their own (stubs in a temporary folder). */
  posixCandidates: readonly string[] = POSIX_PYTHON_CANDIDATES
): string {
  const code =
    "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));_s=_p['helperSource'];sys.stdin=io.TextIOWrapper(io.BytesIO(json.dumps(_p['request']).encode('utf-8')),encoding='utf-8');exec(compile(_s,'managed-analytics-helper','exec'))";
  if (HOST_OS[platform] !== 'windows') {
    const script = [
      `probe=${quoteShell(PYTHON_PROBE)}`,
      `for p in ${posixCandidates.join(' ')}; do`,
      '  if [ -n "$p" ] && [ -x "$p" ] && "$p" -c "$probe" </dev/null >/dev/null 2>&1; then',
      `    exec "$p" -c ${quoteShell(code)}`,
      '  fi',
      'done',
      "echo 'aac-helper: runtime_missing' >&2",
      `exit ${HELPER_RUNTIME_MISSING_EXIT}`,
    ].join('\n');
    return `/bin/sh -c ${quoteShell(script)}`;
  }
  const script = [
    "$ErrorActionPreference = 'Continue'",
    "$ProgressPreference = 'SilentlyContinue'",
    "$env:PYTHONIOENCODING = 'utf-8'",
    "$env:PYTHONUTF8 = '1'",
    `$probe = '${PYTHON_PROBE.replace(/'/g, "''")}'`,
    '$candidates = @()',
    '$launcher = Get-Command py.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1',
    "if ($launcher) { $candidates += ,@($launcher.Source, '-3') }",
    'foreach ($c in @(Get-Command python.exe,python3.exe -All -CommandType Application -ErrorAction SilentlyContinue)) { $candidates += ,@($c.Source) }',
    '$python = $null',
    '$prefix = @()',
    '$stub = $false',
    'foreach ($c in $candidates) {',
    '  $out = & $c[0] @($c | Select-Object -Skip 1) -c $probe 2>&1 | Out-String',
    '  if ($LASTEXITCODE -eq 0) { $python = $c[0]; $prefix = @($c | Select-Object -Skip 1); break }',
    "  if ($out -match 'Python was not found' -or $c[0] -like '*\\WindowsApps\\*') { $stub = $true }",
    '}',
    `if (-not $python) { if ($stub) { [Console]::Error.WriteLine('aac-helper: runtime_missing:store_alias') } else { [Console]::Error.WriteLine('aac-helper: runtime_missing') }; exit ${HELPER_RUNTIME_MISSING_EXIT} }`,
    // No pipeline input: Python inherits the ssh session's stdin and reads the request itself.
    `& $python @prefix -c '${code.replace(/'/g, "''")}'`,
    'exit $LASTEXITCODE',
  ].join('; ');
  return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
}

/**
 * ONE fixed packaged helper, streamed on ssh stdin with a JSON request (the
 * `runClaudeHistoryHelper` pattern): no installed copies on the hosts. Roots
 * are fixed defaults resolved on the host plus the saved extra usage-log
 * locations for that host from Settings, all validated absolute paths.
 * Returns per-model, per-hour aggregates plus per-file fingerprints.
 */
export async function runAnalyticsRemoteHelper(
  sshHost: string,
  platform: AnalyticsRemoteHost,
  request: AnalyticsRemoteRequest,
  deps: { timeoutMs?: number; helperPath?: string } = {}
): Promise<AnalyticsRemoteResponse> {
  if (!isSafeUsageSshAlias(sshHost)) {
    throw new ValidationError('Analytics remote SSH alias is not configured safely.');
  }
  if (
    !Array.isArray(request.kinds) ||
    request.kinds.length === 0 ||
    request.kinds.some((kind) => !REMOTE_KINDS.has(kind)) ||
    !Number.isFinite(request.minDateMs) ||
    request.minDateMs < 0 ||
    !isValidExtraRoots(request.extraRoots)
  ) {
    throw new ValidationError('Analytics remote request is invalid.');
  }
  const helper = await readAnalyticsHelper(deps.helperPath);
  const input = Buffer.from(
    JSON.stringify({
      helperSource: helper,
      request: {
        kinds: [...new Set(request.kinds)],
        minDateMs: Math.floor(request.minDateMs),
        immutableSqlite: true,
        fingerprints: request.fingerprints,
        extraRoots: request.extraRoots ?? {},
        deadlineMs: 20_000,
      },
    })
  );
  if (input.length > MAX_RESPONSE_BYTES)
    throw new ValidationError('Analytics remote request is invalid.');
  const command = analyticsHelperCommand(platform);
  const stdout = await new Promise<string>((resolve, reject) => {
    const child = execFile(
      'ssh',
      sshArgv(sshHost, command, { interval: 5, countMax: 6 }),
      {
        encoding: 'utf8',
        timeout: deps.timeoutMs ?? SSH_TIMEOUT_MS,
        maxBuffer: MAX_RESPONSE_BYTES,
        windowsHide: true,
      },
      (error, result, stderr) => {
        if (error) {
          const failure = error as NodeJS.ErrnoException & {
            signal?: string | null;
            code?: number | string;
          };
          const timedOut = error.killed === true && failure.signal === 'SIGTERM';
          const exitCode = typeof failure.code === 'number' ? failure.code : null;
          // A maxBuffer overflow is an answer too large to be the helper's, never a crash.
          const cause =
            failure.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER'
              ? 'bad_response'
              : classifyAnalyticsScanFailure({
                  exitCode,
                  killed: error.killed,
                  signal: failure.signal ?? null,
                  stderr: typeof stderr === 'string' ? stderr.slice(-4096) : '',
                });
          reject(new AnalyticsRemoteTransportError(timedOut, cause, exitCode));
          return;
        }
        if (typeof result !== 'string') {
          reject(new AnalyticsRemoteTransportError());
          return;
        }
        resolve(result);
      }
    );
    child.stdin?.on('error', () => {
      /* SSH exit is handled without remote text. */
    });
    child.stdin?.end(input);
  });
  return parseAnalyticsRemoteResponse(stdout);
}

/** How long a computer whose scanner failed is read by the Python helper before the scanner is retried. */
export const SCANNER_RETRY_MS = 6 * 60 * 60_000;

/** Per computer: why its scanner is not used, until when (memory only; a restart retries). */
const scannerDown = new Map<string, { cause: string; until: number }>();
/** One install per computer at a time; a second caller waits for the first. */
const installing = new Map<string, Promise<void>>();

export interface AnalyticsRemoteScanDeps {
  timeoutMs?: number;
  helperPath?: string;
  /** The packaged scanners; null runs the Python helper everywhere. Default: dist/native/aac-usage. */
  manifest?: UsageScanManifest | null;
  install?: typeof installUsageScan;
  now?: () => number;
  /** The Python helper (tests stand it in). */
  python?: typeof runAnalyticsRemoteHelper;
}

/** Forget scanner failures (tests). */
export function resetScannerState(): void {
  scannerDown.clear();
  installing.clear();
}

class ScannerLaunchError extends Error {
  constructor(readonly marker: ReturnType<typeof binaryMarker>) {
    super('The usage-log scanner is not in place.');
  }
}

/** One scanner run over ssh; ScannerLaunchError when the copy is missing or has no build here. */
async function runScanner(
  sshHost: string,
  platform: AnalyticsRemoteHost,
  request: AnalyticsRemoteRequest,
  manifest: UsageScanManifest,
  timeoutMs: number
): Promise<AnalyticsRemoteResponse> {
  const input = Buffer.from(
    JSON.stringify({
      kinds: [...new Set(request.kinds)],
      minDateMs: Math.floor(request.minDateMs),
      immutableSqlite: true,
      fingerprints: request.fingerprints,
      extraRoots: request.extraRoots ?? {},
      ...(request.experimentRoots && isValidExperimentRoots(request.experimentRoots)
        ? { experimentRoots: request.experimentRoots }
        : {}),
      deadlineMs: 20_000,
      ...(request.presence === true ? { presence: true } : {}),
      ...(request.cwd === false ? { cwd: false } : {}),
    })
  );
  if (input.length > MAX_RESPONSE_BYTES)
    throw new ValidationError('Analytics remote request is invalid.');
  const stdout = await new Promise<string>((resolve, reject) => {
    const child = execFile(
      'ssh',
      [
        '-T',
        '-o',
        'BatchMode=yes',
        '-o',
        'ConnectTimeout=5',
        '-o',
        'ConnectionAttempts=1',
        '-o',
        'ServerAliveInterval=5',
        '-o',
        'ServerAliveCountMax=6',
        '--',
        sshHost,
        scanCommand(platform, manifest),
      ],
      { encoding: 'utf8', timeout: timeoutMs, maxBuffer: MAX_RESPONSE_BYTES, windowsHide: true },
      (error, result, stderr) => {
        if (error) {
          const failure = error as NodeJS.ErrnoException & {
            signal?: string | null;
            code?: number | string;
          };
          const exitCode = typeof failure.code === 'number' ? failure.code : null;
          const tail = typeof stderr === 'string' ? stderr.slice(-4096) : '';
          const marker = binaryMarker(exitCode, tail);
          if (marker) {
            reject(new ScannerLaunchError(marker));
            return;
          }
          const timedOut = error.killed === true && failure.signal === 'SIGTERM';
          reject(
            new AnalyticsRemoteTransportError(
              timedOut,
              failure.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER'
                ? 'bad_response'
                : classifyAnalyticsScanFailure({
                    exitCode,
                    killed: error.killed,
                    signal: failure.signal ?? null,
                    stderr: tail,
                  }),
              exitCode
            )
          );
          return;
        }
        resolve(typeof result === 'string' ? result : '');
      }
    );
    child.stdin?.on('error', () => {
      /* ssh's exit carries the outcome. */
    });
    child.stdin?.end(input);
  });
  const parsed = parseAnalyticsRemoteResponse(stdout);
  if (parsed.reader?.name !== 'aac-usage-scan')
    throw new AnalyticsRemoteTransportError(false, 'bad_response');
  return parsed;
}

/**
 * Scan one computer with the Rust scanner when this package carries one for its target, copying it
 * there first when it is missing (DESIGN 12.2); else, or when the scanner fails, with the Python
 * helper, whose answer is labelled `reader: python-fallback` with the cause. A failure of the
 * computer itself (ssh, timeout) is never blamed on the scanner: it is thrown as it is.
 */
export async function runAnalyticsRemoteScan(
  sshHost: string,
  platform: AnalyticsRemoteHost,
  request: AnalyticsRemoteRequest,
  deps: AnalyticsRemoteScanDeps = {}
): Promise<AnalyticsRemoteResponse> {
  if (!isSafeUsageSshAlias(sshHost))
    throw new ValidationError('Analytics remote SSH alias is not configured safely.');
  const now = deps.now ?? Date.now;
  const manifest = deps.manifest === undefined ? readUsageScanManifest() : deps.manifest;
  const python = deps.python ?? runAnalyticsRemoteHelper;
  const timeoutMs = deps.timeoutMs ?? SSH_TIMEOUT_MS;
  let cause = manifest ? undefined : 'binary_not_built';
  const down = scannerDown.get(platform);
  if (down && down.until > now()) cause = down.cause;
  if (manifest && !cause) {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const answer = await runScanner(sshHost, platform, request, manifest, timeoutMs);
        scannerDown.delete(platform);
        return answer;
      } catch (error) {
        if (!(error instanceof ScannerLaunchError)) {
          // ssh or the computer failed: the same failure would stop the Python helper too.
          if (error instanceof AnalyticsRemoteTransportError && error.cause.startsWith('ssh_'))
            throw error;
          if (error instanceof AnalyticsRemoteTransportError && error.cause === 'timeout')
            throw error;
          cause = 'binary_failed';
          break;
        }
        const marker = error.marker;
        if (!marker || 'unavailable' in marker || attempt > 0) {
          cause = marker && 'unavailable' in marker ? 'binary_unavailable:target' : 'binary_failed';
          break;
        }
        const target = marker.missing;
        if (!manifest.targets[target]) {
          cause = 'binary_unavailable:target';
          break;
        }
        try {
          let pending = installing.get(platform);
          if (!pending) {
            pending = (deps.install ?? installUsageScan)(
              sshHost,
              platform,
              target,
              manifest
            ).finally(() => installing.delete(platform));
            installing.set(platform, pending);
          }
          await pending;
        } catch (failure) {
          cause = `binary_install:${failure instanceof UsageScanInstallError ? failure.step : 'copy'}`;
          break;
        }
      }
    }
    if (cause) scannerDown.set(platform, { cause, until: now() + SCANNER_RETRY_MS });
  }
  // The Python helper refuses fields it does not know; it reads the defaults and T3 homes only.
  // It cannot resume a part-read log: given that print it would take the file as read to its end,
  // so such prints are left out and Python reads the file whole (DESIGN 16.1).
  const { presence: _presence, cwd: _cwd, experimentRoots: _experiments, ...older } = request;
  void _presence;
  void _cwd;
  void _experiments;
  older.fingerprints = withoutPartialPrints(request.fingerprints);
  const answer = await python(sshHost, platform, older, {
    ...(deps.timeoutMs !== undefined ? { timeoutMs: deps.timeoutMs } : {}),
    ...(deps.helperPath !== undefined ? { helperPath: deps.helperPath } : {}),
  });
  return { ...answer, reader: { name: 'python-fallback', cause: cause ?? 'binary_failed' } };
}

/** The local scanner's budget: inside the 60 s collection, above one decode of the largest log. */
export const LOCAL_SCAN_DEADLINE_MS = 40_000;

const localDigests = new Map<string, string>();

/** The SHA-256 of the local binary, re-read whenever its size or mtime changes. */
function localDigest(file: string): string | null {
  try {
    const stat = fs.statSync(file);
    const key = `${file}\0${stat.size}\0${stat.mtimeMs}`;
    const known = localDigests.get(key);
    if (known) return known;
    const digest = createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    localDigests.clear();
    localDigests.set(key, digest);
    return digest;
  } catch {
    return null;
  }
}

/**
 * Whether the packaged scanner can read this computer (DESIGN 16.0 B): built for its target, in
 * place, and matching its pinned digest. Otherwise the cause, which labels the TypeScript fallback.
 */
export function localUsageScanReady(
  scanner: ReturnType<typeof localUsageScan> = localUsageScan()
): { ok: true } | { ok: false; cause: 'binary_not_built' | 'binary_missing' | 'binary_damaged' } {
  if (!scanner.ok) return scanner;
  return localDigest(scanner.file) === scanner.sha256
    ? { ok: true }
    : { ok: false, cause: 'binary_damaged' };
}

/**
 * The scanner on the dashboard's own computer: the same request and answer as a remote scan, run
 * in place at low priority, with no ssh and no Python fallback (the TypeScript collectors are the
 * fallback there). DESIGN 16.0 B.
 */
export async function runLocalUsageScan(
  request: AnalyticsRemoteRequest,
  deps: { scanner?: ReturnType<typeof localUsageScan>; timeoutMs?: number } = {}
): Promise<AnalyticsRemoteResponse> {
  const scanner = deps.scanner ?? localUsageScan();
  if (!scanner.ok || !localUsageScanReady(scanner).ok)
    throw new AnalyticsRemoteTransportError(false, 'helper_failed');
  const input = Buffer.from(
    JSON.stringify({
      kinds: [...new Set(request.kinds)],
      minDateMs: Math.floor(request.minDateMs),
      immutableSqlite: true,
      fingerprints: request.fingerprints,
      extraRoots: request.extraRoots ?? {},
      ...(request.experimentRoots && isValidExperimentRoots(request.experimentRoots)
        ? { experimentRoots: request.experimentRoots }
        : {}),
      deadlineMs: LOCAL_SCAN_DEADLINE_MS,
      ...(request.presence === true ? { presence: true } : {}),
      ...(request.cwd === false ? { cwd: false } : {}),
    })
  );
  if (input.length > MAX_RESPONSE_BYTES)
    throw new ValidationError('Analytics remote request is invalid.');
  const stdout = await new Promise<string>((resolve, reject) => {
    const child = execFile(
      scanner.file,
      [],
      {
        encoding: 'utf8',
        timeout: deps.timeoutMs ?? LOCAL_SCAN_DEADLINE_MS + 20_000,
        maxBuffer: MAX_RESPONSE_BYTES,
        windowsHide: true,
      },
      (error, result) => {
        if (error) {
          const failure = error as NodeJS.ErrnoException & { signal?: string | null };
          const timedOut = error.killed === true && failure.signal === 'SIGTERM';
          reject(
            new AnalyticsRemoteTransportError(
              timedOut,
              failure.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER'
                ? 'bad_response'
                : timedOut
                  ? 'timeout'
                  : 'helper_failed',
              typeof failure.code === 'number' ? failure.code : null
            )
          );
          return;
        }
        resolve(typeof result === 'string' ? result : '');
      }
    );
    try {
      if (child.pid) os.setPriority(child.pid, 10);
    } catch {
      /* Priority is a courtesy; the scan runs either way. */
    }
    child.stdin?.on('error', () => {
      /* The exit is handled above. */
    });
    child.stdin?.end(input);
  });
  const answer = parseAnalyticsRemoteResponse(stdout);
  return answer.reader ? answer : { ...answer, reader: { name: 'aac-usage-scan' } };
}

/** Prints of logs read to their end only, without the resume fields the Python helper lacks. */
export function withoutPartialPrints(
  fingerprints: Record<string, Record<string, AnalyticsRemoteFingerprint>>
): Record<string, Record<string, AnalyticsRemoteFingerprint>> {
  const out: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {};
  for (const [kind, prints] of Object.entries(fingerprints)) {
    out[kind] = {};
    for (const [key, print] of Object.entries(prints)) {
      if (print.readThrough !== undefined) continue;
      const {
        readFrom: _from,
        readState: _state,
        readName: _name,
        decodedSize: _decoded,
        ...plain
      } = print;
      void _from;
      void _state;
      void _name;
      void _decoded;
      out[kind][key] = plain;
    }
  }
  return out;
}

/** All remote scans use only aliases in the packaged computer registry. */
export function fixedAnalyticsRemoteAliases(): AnalyticsRemoteAliases {
  return { ...SSH_ALIASES };
}

/** The optional deadline remains for existing callers; discovery performs no disk/profile read. */
export async function resolveAnalyticsRemoteHosts(
  _deadlineMs?: number
): Promise<AnalyticsRemoteAliases> {
  return fixedAnalyticsRemoteAliases();
}
