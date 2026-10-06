import { execFile } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { NetworkError, ValidationError } from '../../errors/error-types';
import { isSafeUsageSshAlias } from './additional-usage-transport';
import { listClaudeDesktopProfiles } from './claude-desktop-profile-service';

export type AnalyticsRemoteHost = 'mac' | 'windows';
export type AnalyticsRemoteKind = 'claude' | 'codex' | 'omp' | 'muse' | 'zcode' | 'antigravity';

/** Every kind the packaged helper reads. */
const REMOTE_KIND_LIST = ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'];
const REMOTE_KINDS: ReadonlySet<string> = new Set(REMOTE_KIND_LIST);

export class AnalyticsRemoteTransportError extends NetworkError {
  readonly timedOut: boolean;

  constructor(timedOut = false) {
    super(timedOut ? 'Analytics remote request timed out.' : 'Analytics remote request failed.');
    this.name = 'AnalyticsRemoteTransportError';
    this.timedOut = timedOut;
  }
}

const SSH_TIMEOUT_MS = 30_000;
const MAX_HELPER_BYTES = 128 * 1024;
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;
const MAX_ROWS = 100_000;
/**
 * The packaged helper's SHA-256, pinned like the Claude history writer: only these exact bytes are ever run on the
 * Mac and Windows. Update it together with `scripts/analytics-remote/analytics_usage_remote.py`.
 */
export const ANALYTICS_HELPER_SHA256 =
  'da65d7d1a771d7cd63f82d090a797dce3bc6aa41c2acbdc5ab7317493125e9a6';

export interface AnalyticsRemoteFingerprint {
  size: number;
  mtimeMs: number;
  head?: string;
  tail?: string;
  /** zcode only: the write-ahead log, where new rows wait for a checkpoint. */
  walSize?: number;
  walMtimeMs?: number;
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
  /** A cap or the deadline cut this kind's own scan short: its numbers are incomplete. */
  partial?: boolean;
  fingerprints: Record<string, AnalyticsRemoteFingerprint>;
  /** zcode: an immutable open cannot read rows still in the write-ahead log. */
  walUnread?: boolean;
  /** Antigravity: a conversation database or folder could not be read (busy, damaged); the rest count. */
  unreadable?: boolean;
}

export interface AnalyticsRemoteResponse {
  version: 1;
  /** A file, row or deadline cap stopped the scan: some files were not read. */
  truncated: boolean;
  /** Only the search for custom OMP session roots hit its bounds. */
  discoveryTruncated?: boolean;
  kinds: Record<AnalyticsRemoteKind, AnalyticsRemoteKindResult>;
  rows: AnalyticsRemoteRow[];
  srows: AnalyticsRemoteSessionRow[];
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
}

export function analyticsHelperPath(): string {
  return path.resolve(__dirname, '../../../scripts/analytics-remote/analytics_usage_remote.py');
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
    (print.walSize === undefined || nonNegative(print.walSize)) &&
    (print.walMtimeMs === undefined || nonNegative(print.walMtimeMs))
  );
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
    response.version !== 1 ||
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
      fingerprints?: unknown;
      walUnread?: unknown;
      unreadable?: unknown;
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
    if (entry.walUnread !== undefined && typeof entry.walUnread !== 'boolean')
      throw new AnalyticsRemoteTransportError();
    if (entry.unreadable !== undefined && typeof entry.unreadable !== 'boolean')
      throw new AnalyticsRemoteTransportError();
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
      fingerprints: prints as Record<string, AnalyticsRemoteFingerprint>,
      ...(entry.walUnread === true ? { walUnread: true } : {}),
      ...(entry.unreadable === true ? { unreadable: true } : {}),
    };
  }
  return {
    version: 1,
    truncated: response.truncated,
    ...(response.discoveryTruncated === true ? { discoveryTruncated: true } : {}),
    kinds: parsed,
    rows: rows as AnalyticsRemoteRow[],
    srows: (Array.isArray(srows) ? srows : []) as AnalyticsRemoteSessionRow[],
  };
}

/**
 * The fixed ssh command that runs the packaged helper on a host. Only this process's helper source,
 * streamed on stdin with the request, is executed; aggregates travel back on stdout without
 * temporary files. Python reads the request from stdin itself on every host. On Windows, PowerShell
 * only finds Python and starts it: when PowerShell read a request of the helper's size from stdin
 * (`[Console]::In.ReadToEnd()`) over Windows OpenSSH, about half the reads never saw the end of
 * the input and hung until the ssh timeout (measured 2026-10-06: 4 of 8 80 KB reads hung, against
 * 12 of 12 that completed when Python read the same input). Reading the bytes directly also keeps
 * them UTF-8, where PowerShell's pipe re-encoded them.
 */
export function analyticsHelperCommand(platform: AnalyticsRemoteHost): string {
  const code =
    "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));_s=_p['helperSource'];sys.stdin=io.TextIOWrapper(io.BytesIO(json.dumps(_p['request']).encode('utf-8')),encoding='utf-8');exec(compile(_s,'managed-analytics-helper','exec'))";
  if (platform === 'mac') return `/usr/bin/python3 -c ${quoteShell(code)}`;
  const script = [
    "$ErrorActionPreference = 'Stop'",
    "$ProgressPreference = 'SilentlyContinue'",
    "$env:PYTHONIOENCODING = 'utf-8'",
    "$env:PYTHONUTF8 = '1'",
    '$python = (Get-Command python3.exe,python.exe -ErrorAction SilentlyContinue | Select-Object -First 1).Source',
    'if (-not $python) { exit 1 }',
    // No pipeline input: Python inherits the ssh session's stdin and reads the request itself.
    `& $python -c '${code.replace(/'/g, "''")}'`,
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
  deps: { timeoutMs?: number } = {}
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
  const helperBytes = await fs.promises.readFile(analyticsHelperPath());
  if (
    helperBytes.length > MAX_HELPER_BYTES ||
    createHash('sha256').update(helperBytes).digest('hex') !== ANALYTICS_HELPER_SHA256
  )
    throw new ValidationError('Analytics remote helper is unavailable.');
  const helper = helperBytes.toString('utf8');
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
        command,
      ],
      {
        encoding: 'utf8',
        timeout: deps.timeoutMs ?? SSH_TIMEOUT_MS,
        maxBuffer: MAX_RESPONSE_BYTES,
        windowsHide: true,
      },
      (error, result) => {
        if (error) {
          reject(
            new AnalyticsRemoteTransportError(
              error.killed === true &&
                (error as NodeJS.ErrnoException & { signal?: string }).signal === 'SIGTERM'
            )
          );
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

/**
 * The same ssh host aliases the server already uses for the Mac and Windows:
 * the first Claude desktop launcher on each host that carries one. Null when
 * no launcher configures that host.
 */
export async function resolveAnalyticsRemoteHosts(): Promise<{
  mac: string | null;
  windows: string | null;
}> {
  let profiles: Array<{ mac?: { sshHost?: string }; windows?: { sshHost?: string } }> = [];
  try {
    profiles = await listClaudeDesktopProfiles();
  } catch {
    return { mac: null, windows: null };
  }
  let mac: string | null = null;
  let windows: string | null = null;
  for (const profile of profiles) {
    if (!mac && profile.mac?.sshHost && isSafeUsageSshAlias(profile.mac.sshHost))
      mac = profile.mac.sshHost;
    if (!windows && profile.windows?.sshHost && isSafeUsageSshAlias(profile.windows.sshHost))
      windows = profile.windows.sshHost;
    if (mac && windows) break;
  }
  return { mac, windows };
}
