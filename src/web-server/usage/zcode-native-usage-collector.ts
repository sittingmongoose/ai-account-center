/** zcode usage: local queries run through the packaged analytics helper. */
import { execFileSync } from 'child_process';
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
  /** sha256('aac-session-v1:zcode:<session id>')[:16]; absent with no session column. */
  s?: string;
}

/** The database file plus its write-ahead log, where zcode keeps recent rows until a checkpoint. */
export interface ZcodeFingerprint {
  size: number;
  mtimeMs: number;
  walSize?: number;
  walMtimeMs?: number;
}

export interface ZcodeHelperResult {
  /** `error`: the database exists but could not be read; nothing was confirmed. */
  state: 'ok' | 'not_installed' | 'error';
  fingerprints: Record<string, ZcodeFingerprint>;
  rows: ZcodeHelperRow[];
  truncated: boolean;
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
    row.c >= 0 &&
    (row.s === undefined || (typeof row.s === 'string' && /^[0-9a-f]{16}$/.test(row.s)))
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
  options: { pythonPath?: string; homeDir?: string; timeoutMs?: number } = {}
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
    truncated?: boolean;
  };
  const kind = response?.kinds?.zcode;
  if (!kind || (kind.state !== 'ok' && kind.state !== 'not_installed' && kind.state !== 'error'))
    throw new CCSError('Analytics helper returned an invalid result.');
  const rows = Array.isArray(response.rows) ? response.rows : [];
  if (rows.length > 100_000 || !rows.every(isRow))
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
  return { state: kind.state, fingerprints: prints, rows, truncated: response.truncated === true };
}
