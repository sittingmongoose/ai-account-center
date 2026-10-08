/**
 * Secret-free renewal history per saved profile: `<ccs dir>/codex-renewal-status.json`
 * (0600, atomic replace). It holds timestamps, an outcome code, a failure count
 * and a binding to the auth file version, never token data or digests of it.
 *
 * The version binding is the file's stat identity (device, inode, size and
 * nanosecond mtime). Every writer of a saved login (activation save-back, Sign
 * in again, renewal) replaces the file by rename, which yields a new inode, so a
 * recorded failure or backoff stops applying as soon as the login changes. A
 * false "changed" (a touch or restore) costs one extra attempt; a false
 * "unchanged" would need inode reuse with identical size and mtime.
 */
import * as fs from 'fs';
import * as path from 'path';
import { getCcsDir } from '../utils/config-manager';
import { replaceFileAtomically } from './codex-atomic-file';
import { CODEX_RENEWAL_RETRY_REASONS, type CodexRenewalReason } from './codex-renewal-types';

export const CODEX_RENEWAL_STATUS_FILE = 'codex-renewal-status.json';
const MAX_STATUS_BYTES = 256 * 1024;
const BACKOFF_BASE_MS = 15 * 60_000;
const BACKOFF_MAX_MS = 12 * 60 * 60_000;
/** A busy activation lock is retried after this long. */
export const CODEX_RENEWAL_BUSY_RETRY_MS = 15 * 60_000;

export interface CodexRenewalRecord {
  /** Stat identity of the saved auth.json the outcome applies to. */
  authVersion: string;
  outcome: CodexRenewalReason;
  lastAttemptAt: string;
  lastSuccessAt?: string;
  consecutiveFailures: number;
  nextAttemptAt?: string;
  accessExpiresAt?: string;
}

export type CodexRenewalRecords = Record<string, CodexRenewalRecord>;

export function getCodexRenewalStatusPath(): string {
  return path.join(getCcsDir(), CODEX_RENEWAL_STATUS_FILE);
}

/** Version of a saved auth file without reading it; null when absent. */
export function codexAuthFileVersion(file: string): string | null {
  try {
    const stat = fs.statSync(file, { bigint: true });
    return [stat.dev, stat.ino, stat.size, stat.mtimeNs].join(':');
  } catch {
    return null;
  }
}

function isIso(value: unknown): value is string {
  return typeof value === 'string' && Number.isFinite(Date.parse(value));
}

function validRecord(value: unknown): CodexRenewalRecord | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (
    typeof record.authVersion !== 'string' ||
    typeof record.outcome !== 'string' ||
    !isIso(record.lastAttemptAt) ||
    typeof record.consecutiveFailures !== 'number' ||
    !Number.isInteger(record.consecutiveFailures) ||
    record.consecutiveFailures < 0
  ) {
    return null;
  }
  for (const key of ['lastSuccessAt', 'nextAttemptAt', 'accessExpiresAt']) {
    if (record[key] !== undefined && !isIso(record[key])) return null;
  }
  return {
    authVersion: record.authVersion,
    outcome: record.outcome as CodexRenewalReason,
    lastAttemptAt: record.lastAttemptAt,
    consecutiveFailures: record.consecutiveFailures,
    ...(isIso(record.lastSuccessAt) ? { lastSuccessAt: record.lastSuccessAt } : {}),
    ...(isIso(record.nextAttemptAt) ? { nextAttemptAt: record.nextAttemptAt } : {}),
    ...(isIso(record.accessExpiresAt) ? { accessExpiresAt: record.accessExpiresAt } : {}),
  };
}

/** Missing or invalid files read as empty history; the next write replaces them. */
export function readCodexRenewalRecords(file = getCodexRenewalStatusPath()): CodexRenewalRecords {
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > MAX_STATUS_BYTES) return {};
    const parsed = JSON.parse(fs.readFileSync(file, 'utf8')) as {
      version?: unknown;
      profiles?: unknown;
    };
    if (parsed?.version !== 1 || !parsed.profiles || typeof parsed.profiles !== 'object') {
      return {};
    }
    const records: CodexRenewalRecords = {};
    for (const [name, value] of Object.entries(parsed.profiles as Record<string, unknown>)) {
      const record = validRecord(value);
      if (record) records[name] = record;
    }
    return records;
  } catch {
    return {};
  }
}

const queues = new Map<string, Promise<void>>();

/**
 * Read-modify-write one status file, serialized per file in this process.
 * Records of profiles outside `keep` are pruned. Another process racing the same
 * file can lose an update; the history is advisory (a lost failure costs one
 * extra attempt, which the guards still protect).
 */
export function updateCodexRenewalRecords(
  update: (records: CodexRenewalRecords) => void,
  keep: ReadonlySet<string> | null,
  file = getCodexRenewalStatusPath()
): Promise<void> {
  const previous = queues.get(file) ?? Promise.resolve();
  const next = previous.then(() => {
    const records = readCodexRenewalRecords(file);
    update(records);
    const profiles: CodexRenewalRecords = {};
    for (const name of Object.keys(records).sort()) {
      if (!keep || keep.has(name)) profiles[name] = records[name];
    }
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    replaceFileAtomically(file, `${JSON.stringify({ version: 1, profiles }, null, 2)}\n`);
  });
  const settled = next.catch(() => undefined);
  queues.set(file, settled);
  void settled.then(() => {
    if (queues.get(file) === settled) queues.delete(file);
  });
  return next;
}

/** What one renewal attempt concluded, as the history records it. */
export interface CodexRenewalAttemptOutcome {
  reason: CodexRenewalReason;
  /** True only when a refresh request was sent. */
  attempted: boolean;
  /** Version of the saved file the outcome applies to. */
  version: string | null;
  accessExpiresAt: number | null;
  /** Whether the renewed tokens kept a session id or sign-in time (log only). */
  familyMarkersKept?: boolean;
}

/** Exponential backoff from 15 minutes, doubling to at most 12 hours, +/- 20% jitter. */
export function codexRenewalBackoffMs(failures: number, random: () => number): number {
  const base = Math.min(BACKOFF_MAX_MS, BACKOFF_BASE_MS * 2 ** Math.max(0, failures - 1));
  return Math.round(base * (0.8 + 0.4 * random()));
}

export function nextCodexRenewalRecord(
  previous: CodexRenewalRecord | undefined,
  outcome: CodexRenewalAttemptOutcome,
  at: number,
  random: () => number
): CodexRenewalRecord {
  const sameVersion = previous !== undefined && previous.authVersion === outcome.version;
  const carried = sameVersion ? previous.consecutiveFailures : 0;
  const failures = outcome.reason === 'renewed' ? 0 : outcome.attempted ? carried + 1 : carried;
  const nextAttemptAt =
    outcome.reason === 'activation_busy'
      ? at + CODEX_RENEWAL_BUSY_RETRY_MS
      : CODEX_RENEWAL_RETRY_REASONS.has(outcome.reason)
        ? at + codexRenewalBackoffMs(failures, random)
        : null;
  const lastSuccessAt =
    outcome.reason === 'renewed' ? new Date(at).toISOString() : previous?.lastSuccessAt;
  return {
    authVersion: outcome.version ?? 'missing',
    outcome: outcome.reason,
    lastAttemptAt: new Date(at).toISOString(),
    consecutiveFailures: failures,
    ...(lastSuccessAt ? { lastSuccessAt } : {}),
    ...(nextAttemptAt !== null ? { nextAttemptAt: new Date(nextAttemptAt).toISOString() } : {}),
    ...(outcome.accessExpiresAt !== null
      ? { accessExpiresAt: new Date(outcome.accessExpiresAt).toISOString() }
      : {}),
  };
}
