import * as fs from 'fs';

/**
 * Who holds an Antigravity lock, claim or sign-in marker, and whether that
 * holder still runs. A record names the holder by its pid, the kernel's start
 * time of that process (`/proc/<pid>/stat` field 22) and the boot it ran in,
 * so a dead holder is recognised at once, without the wall clock, even when
 * its pid has been reused. Records written before these fields existed are
 * judged by the pid alone.
 */
export const HOLDER_RECORD_BYTES = 512;

export interface HolderRecord {
  pid: number;
  /** Wall-clock time the record was written. */
  startedAt: number;
  /** Clock ticks since boot at which the holder process started; null when unknown. */
  processStart: string | null;
  bootId: string | null;
  /** The record file's mtime: a live lock holder refreshes it as its heartbeat. */
  heartbeatMs: number;
}

let ownStart: string | null | undefined;
let bootId: string | null | undefined;

/** This boot's id; null off Linux or when unreadable. */
export function currentBootId(): string | null {
  if (bootId !== undefined) return bootId;
  bootId = null;
  if (process.platform === 'linux') {
    try {
      const value = fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8').trim();
      if (/^[0-9a-f-]{36}$/.test(value)) bootId = value;
    } catch {
      bootId = null;
    }
  }
  return bootId;
}

/**
 * The start time of `pid` in clock ticks since boot: 'gone' when no such
 * process exists, null when it cannot be told (off Linux, or unreadable).
 */
export function processStartTime(pid: number): string | null | 'gone' {
  if (process.platform !== 'linux' || !Number.isSafeInteger(pid) || pid <= 0) return null;
  let text: string;
  try {
    text = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT' ? 'gone' : null;
  }
  // The command name (field 2) is in parentheses and may hold spaces or ')'.
  const close = text.lastIndexOf(')');
  if (close < 0) return null;
  const fields = text
    .slice(close + 1)
    .trim()
    .split(/\s+/);
  // fields[0] is field 3 (the state), so field 22 is fields[19].
  const value = fields[19];
  return value !== undefined && /^\d{1,20}$/.test(value) ? value : null;
}

/** The record this process writes for a lock, claim or marker it takes now. */
export function ownHolderRecord(now: number = Date.now()): string {
  if (ownStart === undefined) {
    const start = processStartTime(process.pid);
    ownStart = start === 'gone' ? null : start;
  }
  return JSON.stringify({
    pid: process.pid,
    startedAt: new Date(now).toISOString(),
    processStart: ownStart,
    bootId: currentBootId(),
  });
}

/** Parse a record's text; null when it is not a well-formed record. */
export function parseHolderRecord(text: string, heartbeatMs: number): HolderRecord | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
  const record = parsed as Record<string, unknown>;
  const { pid, startedAt } = record;
  if (!Number.isSafeInteger(pid) || (pid as number) <= 0 || typeof startedAt !== 'string')
    return null;
  const started = Date.parse(startedAt);
  if (!Number.isFinite(started)) return null;
  const optional = (value: unknown, pattern: RegExp): string | null | undefined =>
    value === undefined || value === null
      ? null
      : typeof value === 'string' && pattern.test(value)
        ? value
        : undefined;
  const processStart = optional(record.processStart, /^\d{1,20}$/);
  const boot = optional(record.bootId, /^[0-9a-f-]{36}$/);
  if (processStart === undefined || boot === undefined) return null;
  return {
    pid: pid as number,
    startedAt: started,
    processStart,
    bootId: boot,
    heartbeatMs: Number.isFinite(heartbeatMs) ? heartbeatMs : started,
  };
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // ESRCH proves exit; anything else (for example EPERM) still counts as live.
    return (error as NodeJS.ErrnoException).code !== 'ESRCH';
  }
}

/** One-line reason when the holder process provably no longer runs, otherwise null. */
export function holderGoneReason(holder: HolderRecord): string | null {
  const boot = currentBootId();
  if (holder.bootId && boot && holder.bootId !== boot)
    return `holder pid ${holder.pid} ran before this boot`;
  if (!pidAlive(holder.pid)) return `holder pid ${holder.pid} is dead`;
  if (holder.processStart !== null) {
    const start = processStartTime(holder.pid);
    if (start === 'gone') return `holder pid ${holder.pid} is dead`;
    if (start !== null && start !== holder.processStart)
      return `holder pid ${holder.pid} now belongs to another process`;
  }
  return null;
}

/**
 * One-line reason when the holder is gone, or alive but silent for longer
 * than `silentAfterMs` (no heartbeat since then); otherwise null.
 */
export function holderStaleReason(
  holder: HolderRecord,
  now: number,
  silentAfterMs: number
): string | null {
  const gone = holderGoneReason(holder);
  if (gone) return gone;
  const beat = Math.max(holder.startedAt, holder.heartbeatMs);
  if (now - beat > silentAfterMs)
    return `holder pid ${holder.pid} sent no heartbeat for ${Math.round(now - beat)}ms`;
  return null;
}
