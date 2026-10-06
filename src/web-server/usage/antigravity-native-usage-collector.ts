/**
 * Antigravity usage: where it lives, and the local read through the packaged analytics helper.
 *
 * Antigravity keeps each conversation in one SQLite database, with its token usage as protobuf in
 * `gen_metadata.data` and `steps.metadata`, apart from the conversation text. The helper
 * (`scripts/analytics-remote/analytics_usage_remote.py`, `_collect_antigravity`) ports T3 Code's reader
 * (`apps/server/src/usage/antigravityUsageReader.ts`, tag v0.0.46-nightly.20261006.2735) and reads the
 * same roots on every host: `$ANTIGRAVITY_DATA_DIR`, else the `~/.gemini` stores and `~/.config/antigravity`,
 * plus each T3 Code instance's `~/.t3/userdata/providers/antigravity/<sha256(id)>/antigravity-acp`. It opens
 * databases `mode=ro` (never immutable), reads only the usage fields, and counts a record once across
 * databases and copies by its own ids. The VM runs it locally, like zcode; the Mac and Windows run the same
 * bytes over ssh. Only model names, hour buckets, token sums and hashed session keys come back.
 */
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CCSError } from '../../errors/error-types';
import { analyticsRemoteHelperPath } from './zcode-native-usage-collector';

export const ANTIGRAVITY_TARGET = 'antigravity';
const MAX_HELPER_BYTES = 128 * 1024;
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;
const MAX_ROWS = 100_000;
/** T3 instance folders looked at, like T3_MAX_HOMES for the Claude and Codex homes. */
const MAX_INSTANCES = 64;
const DEFAULT_STORES = ['antigravity', 'antigravity-cli', 'antigravity-ide', 'antigravity-backup'];

export interface AntigravityRootOptions {
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
}

/** A network mount, a UNC path or `~/PM-Experiments`: never looked at (a stat on a dead mount blocks). */
function excluded(candidate: string, homeDir: string): boolean {
  if (candidate.startsWith('\\\\') || candidate.startsWith('//')) return true;
  return ['/mnt', '/media', '/Volumes', '/net', path.join(homeDir, 'PM-Experiments')].some(
    (root) => candidate === root || candidate.startsWith(root + path.sep)
  );
}

/**
 * The roots the helper reads on this host, in its order (`_agy_dirs`), before link resolution: the
 * comma-separated `ANTIGRAVITY_DATA_DIR`, else the four `~/.gemini` stores and `~/.config/antigravity`;
 * then every T3 Code instance's `antigravity-acp` folder, found by listing (a folder exists only after
 * the instance's first sign-in, and its name is the sha256 of the instance id).
 */
export function resolveAntigravityRoots(options: AntigravityRootOptions = {}): string[] {
  const env = options.env ?? process.env;
  const homeDir = options.homeDir ?? os.homedir();
  const listed = (env.ANTIGRAVITY_DATA_DIR ?? '')
    .split(',')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) =>
      part === '~' ? homeDir : /^~[\\/]/.test(part) ? path.join(homeDir, part.slice(2)) : part
    )
    .filter((part) => path.isAbsolute(part))
    .map((part) => path.resolve(part));
  const roots = listed.length
    ? listed
    : [
        ...DEFAULT_STORES.map((name) => path.join(homeDir, '.gemini', name)),
        path.join(homeDir, '.config', 'antigravity'),
      ];
  const instances = path.join(homeDir, '.t3', 'userdata', 'providers', 'antigravity');
  let names: string[] = [];
  try {
    names = fs.readdirSync(instances).sort();
  } catch {
    /* No T3 Antigravity instance on this host. */
  }
  for (const name of names.slice(0, MAX_INSTANCES))
    roots.push(path.join(instances, name, 'antigravity-acp'));
  return roots.filter((root) => !excluded(root, homeDir));
}

/**
 * The real folder at `candidate`, resolved one component at a time like the helper's `_agy_real_dir`:
 * each link's target is checked as written before anything follows it, so a link into a network mount
 * or `~/PM-Experiments` is never touched. Null when it leads there, loops, or is not a folder.
 */
export function antigravityRealDir(candidate: string, homeDir: string): string | null {
  const parsed = path.parse(path.resolve(candidate));
  const pieces = path
    .resolve(candidate)
    .slice(parsed.root.length)
    .split(/[\\/]+/)
    .filter(Boolean);
  let current = parsed.root;
  let hops = 0;
  while (pieces.length) {
    const piece = pieces.shift() as string;
    const next = path.join(current, piece);
    if (excluded(next, homeDir)) return null;
    let link: string;
    try {
      if (!fs.lstatSync(next).isSymbolicLink()) {
        current = next;
        continue;
      }
      link = fs.readlinkSync(next);
    } catch {
      return null;
    }
    if (++hops > 40 || /^\\\\\?\\UNC\\/i.test(link)) return null;
    const target = path.resolve(current, link.replace(/^\\\\\?\\/, ''));
    if (excluded(target, homeDir)) return null;
    const root = path.parse(target).root;
    pieces.unshift(
      ...target
        .slice(root.length)
        .split(/[\\/]+/)
        .filter(Boolean)
    );
    current = root;
  }
  try {
    return fs.statSync(current).isDirectory() ? current : null;
  } catch {
    return null;
  }
}

/** Whether any Antigravity root exists here: the Ubuntu source's `not_installed` state. A few stats. */
export function antigravityUsagePresent(options: AntigravityRootOptions = {}): boolean {
  const homeDir = options.homeDir ?? os.homedir();
  return resolveAntigravityRoots(options).some(
    (root) => antigravityRealDir(root, homeDir) !== null
  );
}

/** One per-model, per-hour sum of the helper (`k` is always `antigravity`; `f` is the store key). */
export interface AntigravityHelperRow {
  k: 'antigravity';
  f: string;
  m: string;
  h: string;
  i: number;
  o: number;
  cr: number;
  cw: number;
  c: number;
  n: number;
}

/** One per-session-model sum: the session's published key, first and last event. */
export interface AntigravityHelperSessionRow {
  k: 'antigravity';
  f: string;
  s: string;
  m: string;
  a: number;
  z: number;
  i: number;
  o: number;
  cr: number;
  cw: number;
  c: number;
  n: number;
}

/** The store fingerprint: every database and write-ahead log, folded into `head`. */
export interface AntigravityFingerprint {
  size: number;
  mtimeMs: number;
  head: string;
  tail: string;
}

export interface AntigravityHelperResult {
  /** `error`: databases exist but none could be read. */
  state: 'ok' | 'not_installed' | 'error';
  /** The store print, present only when every database was read; absent means read again next time. */
  fingerprints: Record<string, AntigravityFingerprint>;
  rows: AntigravityHelperRow[];
  sessions: AntigravityHelperSessionRow[];
  truncated: boolean;
  /** Some database or folder could not be read this time (busy, damaged); the rest are counted. */
  unreadable: boolean;
}

const HEX64 = /^[0-9a-f]{64}$/;
const HEX16 = /^[0-9a-f]{16}$/;
const CONTROL = /[\u0000-\u001f\u007f]/;

function count(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function model(value: unknown): value is string {
  return (
    typeof value === 'string' && value.length > 0 && value.length <= 160 && !CONTROL.test(value)
  );
}

function isRow(value: unknown): value is AntigravityHelperRow {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  return (
    row.k === 'antigravity' &&
    typeof row.f === 'string' &&
    HEX64.test(row.f) &&
    model(row.m) &&
    row.p === undefined &&
    typeof row.h === 'string' &&
    /^\d{4}-\d{2}-\d{2} \d{2}:00$/.test(row.h) &&
    ['i', 'o', 'cr', 'cw', 'c', 'n'].every((field) => count(row[field]))
  );
}

function isSessionRow(value: unknown): value is AntigravityHelperSessionRow {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  return (
    row.k === 'antigravity' &&
    typeof row.f === 'string' &&
    HEX64.test(row.f) &&
    typeof row.s === 'string' &&
    HEX16.test(row.s) &&
    model(row.m) &&
    row.p === undefined &&
    count(row.a) &&
    count(row.z) &&
    (row.z as number) >= (row.a as number) &&
    ['i', 'o', 'cr', 'cw', 'c', 'n'].every((field) => count(row[field]))
  );
}

function isPrint(value: unknown): value is AntigravityFingerprint {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const print = value as Record<string, unknown>;
  return (
    count(print.size) &&
    count(print.mtimeMs) &&
    typeof print.head === 'string' &&
    HEX64.test(print.head) &&
    typeof print.tail === 'string' &&
    HEX64.test(print.tail)
  );
}

/**
 * Read this host's Antigravity usage through the packaged helper, sending the last store print so an
 * unchanged store costs a few stats and returns no rows. Strictly validated: model names, hour labels,
 * digests and counts only. Throws when python3 or the helper is unavailable or the answer is malformed.
 */
export function queryLocalAntigravityUsage(
  minDateMs: number,
  fingerprints: Record<string, AntigravityFingerprint>,
  options: {
    pythonPath?: string;
    homeDir?: string;
    timeoutMs?: number;
    env?: NodeJS.ProcessEnv;
  } = {}
): AntigravityHelperResult {
  const helper = analyticsRemoteHelperPath();
  if (fs.statSync(helper).size > MAX_HELPER_BYTES)
    throw new CCSError('Analytics helper is unavailable.');
  const python =
    options.pythonPath ?? (process.platform === 'win32' ? 'python.exe' : '/usr/bin/python3');
  const timeout = Math.min(20_000, Math.max(1000, options.timeoutMs ?? 12_000));
  const output = execFileSync(python, [helper], {
    input: JSON.stringify({
      kinds: ['antigravity'],
      minDateMs: Math.max(0, Math.floor(minDateMs)),
      immutableSqlite: false,
      fingerprints: { antigravity: fingerprints },
      deadlineMs: Math.max(1000, timeout - 500),
    }),
    encoding: 'utf8',
    timeout,
    maxBuffer: MAX_RESPONSE_BYTES,
    windowsHide: true,
    env: {
      ...(options.env ?? process.env),
      ...(options.homeDir ? { HOME: options.homeDir, USERPROFILE: options.homeDir } : {}),
      PYTHONIOENCODING: 'utf-8',
      PYTHONUTF8: '1',
      PYTHONDONTWRITEBYTECODE: '1',
    },
  });
  const response = JSON.parse(output) as {
    kinds?: { antigravity?: Record<string, unknown> };
    rows?: unknown;
    srows?: unknown;
    truncated?: unknown;
  };
  const kind = response?.kinds?.antigravity;
  const state = kind?.state;
  if (!kind || (state !== 'ok' && state !== 'not_installed' && state !== 'error'))
    throw new CCSError('Analytics helper returned an invalid result.');
  const rows = Array.isArray(response.rows) ? response.rows : [];
  const sessions = Array.isArray(response.srows) ? response.srows : [];
  if (rows.length > MAX_ROWS || !rows.every(isRow))
    throw new CCSError('Analytics helper result is invalid.');
  if (sessions.length > MAX_ROWS || !sessions.every(isSessionRow))
    throw new CCSError('Analytics helper result is invalid.');
  const prints: Record<string, AntigravityFingerprint> = {};
  const listed = kind.fingerprints;
  if (listed && typeof listed === 'object' && !Array.isArray(listed))
    for (const [key, print] of Object.entries(listed as Record<string, unknown>))
      if (HEX64.test(key) && isPrint(print)) prints[key] = print;
  return {
    state,
    fingerprints: prints,
    rows,
    sessions,
    truncated: response.truncated === true || kind.partial === true,
    unreadable: kind.unreadable === true,
  };
}
