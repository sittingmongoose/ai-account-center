/** zcode usage: local queries run through the packaged analytics helper. */
import { execFileSync } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CCSError } from '../../errors/error-types';

export const ZCODE_TARGET = 'zcode';
const MAX_HELPER_BYTES = 128 * 1024;
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;

export interface ZcodeRootOptions {
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
}

/** zcode database path, `~/.zcode/cli/db/db.sqlite` unless overridden. */
export function resolveZcodeDbPath(options: ZcodeRootOptions = {}): string {
  const env = options.env ?? process.env;
  const override = env.ZCODE_DB_PATH;
  if (override && override.trim() && path.isAbsolute(override.trim())) return override.trim();
  return path.join(options.homeDir ?? os.homedir(), '.zcode', 'cli', 'db', 'db.sqlite');
}

/** Packaged helper path (source tree and emitted `dist/` share this layout). */
export function analyticsRemoteHelperPath(): string {
  return path.resolve(__dirname, '../../../scripts/analytics-remote/analytics_usage_remote.py');
}

export interface ZcodeHelperRow {
  k: 'zcode';
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
 * One per-session-model aggregate: the session's published key — the helper's
 * sha256('aac-session-v1:zcode:<session id>')[:16], derived where the database was read — plus
 * first/last event. A database without the session column contributes none.
 */
export interface ZcodeHelperSessionRow {
  k: 'zcode';
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

/** The database file plus its write-ahead log, where zcode keeps recent rows until a checkpoint. */
export interface ZcodeFingerprint {
  size: number;
  mtimeMs: number;
  walSize?: number;
  walMtimeMs?: number;
}

/**
 * One in-window usage row in records mode: [row key, session key, model, provider, started_at ms,
 * net input, output, cache read, cache write]. Keys are digests computed by the helper.
 */
export type ZcodeRecord = [string, string, string, string, number, number, number, number, number];

export interface ZcodeHelperResult {
  /** `error`: a database exists but could not be read; it was not confirmed. */
  state: 'ok' | 'not_installed' | 'error';
  fingerprints: Record<string, ZcodeFingerprint>;
  rows: ZcodeHelperRow[];
  sessions: ZcodeHelperSessionRow[];
  truncated: boolean;
  /** Records mode only: the rows of each new or changed database, by its file key. */
  records?: Record<string, ZcodeRecord[]>;
}

function isZcodeRecord(value: unknown): value is ZcodeRecord {
  if (!Array.isArray(value) || value.length !== 9) return false;
  const [key, session, model, provider, ...numbers] = value as unknown[];
  return (
    typeof key === 'string' &&
    /^[0-9a-f]{16}$/.test(key) &&
    typeof session === 'string' &&
    session.length <= 64 &&
    typeof model === 'string' &&
    model.length > 0 &&
    model.length <= 256 &&
    typeof provider === 'string' &&
    provider.length <= 128 &&
    numbers.every((item) => typeof item === 'number' && Number.isFinite(item) && item >= 0)
  );
}

/** The helper's file key for a zcode database (`_filekey` in analytics_usage_remote.py). */
export function zcodeFileKey(dbPath: string): string {
  return createHash('sha256')
    .update(`zcode\0${path.resolve(dbPath)}`)
    .digest('hex');
}

function isRow(value: unknown): value is ZcodeHelperRow {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  return (
    row.k === 'zcode' &&
    typeof row.f === 'string' &&
    typeof row.m === 'string' &&
    (row.p === undefined || typeof row.p === 'string') &&
    typeof row.h === 'string' &&
    /^\d{4}-\d{2}-\d{2} \d{2}:00$/.test(row.h) &&
    ['i', 'o', 'cr', 'cw', 'n'].every(
      (field) =>
        typeof row[field] === 'number' &&
        Number.isFinite(row[field] as number) &&
        (row[field] as number) >= 0
    ) &&
    typeof row.c === 'number' &&
    Number.isFinite(row.c) &&
    row.c >= 0
  );
}

function isSessionRow(value: unknown): value is ZcodeHelperSessionRow {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  return (
    row.k === 'zcode' &&
    typeof row.f === 'string' &&
    typeof row.s === 'string' &&
    /^[0-9a-f]{16}$/.test(row.s) &&
    typeof row.m === 'string' &&
    (row.p === undefined || typeof row.p === 'string') &&
    typeof row.a === 'number' &&
    Number.isFinite(row.a) &&
    row.a >= 0 &&
    typeof row.z === 'number' &&
    Number.isFinite(row.z) &&
    (row.z as number) >= (row.a as number) &&
    ['i', 'o', 'cr', 'cw', 'n'].every(
      (field) =>
        typeof row[field] === 'number' &&
        Number.isFinite(row[field] as number) &&
        (row[field] as number) >= 0
    ) &&
    typeof row.c === 'number' &&
    Number.isFinite(row.c) &&
    row.c >= 0
  );
}

/**
 * Query the local zcode database through the packaged helper: one read-only
 * aggregate query (model names, hashed session keys and integers only), never
 * raw usage JSON or message tables. Local opens are `mode=ro`; only remote hosts add
 * `immutable=1`. Throws when python3 or the database is unavailable.
 */
export function queryLocalZcodeUsage(
  dbPath: string,
  minDateMs: number,
  fingerprints: Record<string, ZcodeFingerprint>,
  options: {
    pythonPath?: string;
    homeDir?: string;
    timeoutMs?: number;
    /** More databases read in the same call (experiment zcode homes). */
    extraDbs?: string[];
    /** Per-row records instead of hourly and session rows (experiment databases). */
    records?: boolean;
  } = {}
): ZcodeHelperResult {
  const helper = analyticsRemoteHelperPath();
  const source = fs.readFileSync(helper, 'utf8');
  if (source.length > MAX_HELPER_BYTES) throw new CCSError('Analytics helper is unavailable.');
  const python =
    options.pythonPath ?? (process.platform === 'win32' ? 'python.exe' : '/usr/bin/python3');
  const request = JSON.stringify({
    kinds: ['zcode'],
    minDateMs,
    immutableSqlite: false,
    fingerprints: { zcode: fingerprints },
    ...(options.extraDbs?.length ? { extraRoots: { zcode: options.extraDbs } } : {}),
    ...(options.records ? { zcodeRecords: true } : {}),
    deadlineMs: Math.min(20_000, Math.max(1000, (options.timeoutMs ?? 12_000) - 500)),
  });
  const output = execFileSync(python, [helper], {
    input: request,
    encoding: 'utf8',
    timeout: Math.min(20_000, Math.max(1000, options.timeoutMs ?? 12_000)),
    maxBuffer: MAX_RESPONSE_BYTES,
    windowsHide: true,
    env: {
      ...process.env,
      ZCODE_DB_PATH: dbPath,
      ...(options.homeDir ? { HOME: options.homeDir } : {}),
      PYTHONIOENCODING: 'utf-8',
      PYTHONUTF8: '1',
    },
  });
  const response = JSON.parse(output) as {
    kinds?: { zcode?: { state?: string; fingerprints?: Record<string, unknown> } };
    rows?: unknown[];
    srows?: unknown[];
    zrecs?: unknown;
    truncated?: boolean;
  };
  const kind = response?.kinds?.zcode;
  if (!kind || (kind.state !== 'ok' && kind.state !== 'not_installed' && kind.state !== 'error'))
    throw new CCSError('Analytics helper returned an invalid result.');
  const rows = Array.isArray(response.rows) ? response.rows : [];
  if (rows.length > 100_000 || !rows.every(isRow))
    throw new CCSError('Analytics helper result is invalid.');
  const sessions = Array.isArray(response.srows) ? response.srows : [];
  if (sessions.length > 100_000 || !sessions.every(isSessionRow))
    throw new CCSError('Analytics helper result is invalid.');
  const prints: Record<string, ZcodeFingerprint> = {};
  const count = (value: unknown): value is number =>
    typeof value === 'number' && Number.isFinite(value) && value >= 0;
  for (const [key, value] of Object.entries(kind.fingerprints ?? {})) {
    const print = value as Record<string, unknown> | null;
    if (print && typeof print === 'object' && count(print.size) && count(print.mtimeMs))
      prints[key] = {
        size: print.size,
        mtimeMs: print.mtimeMs,
        ...(count(print.walSize) && count(print.walMtimeMs)
          ? { walSize: print.walSize, walMtimeMs: print.walMtimeMs }
          : {}),
      };
  }
  let records: Record<string, ZcodeRecord[]> | undefined;
  if (options.records) {
    records = {};
    const raw = response.zrecs;
    if (!raw || typeof raw !== 'object' || Array.isArray(raw))
      throw new CCSError('Analytics helper result is invalid.');
    let total = 0;
    for (const [key, list] of Object.entries(raw as Record<string, unknown>)) {
      if (!/^[0-9a-f]{64}$/.test(key) || !Array.isArray(list) || !list.every(isZcodeRecord))
        throw new CCSError('Analytics helper result is invalid.');
      total += list.length;
      if (total > 100_000) throw new CCSError('Analytics helper result is invalid.');
      records[key] = list;
    }
  }
  return {
    state: kind.state,
    fingerprints: prints,
    rows,
    sessions,
    truncated: response.truncated === true,
    ...(records ? { records } : {}),
  };
}
