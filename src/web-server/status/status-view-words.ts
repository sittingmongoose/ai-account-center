/** Shared words, numbers and limits of the Status tab view. */

export const STATUS_VIEW_VERSION = 1;
export const POLL_SECONDS = 30;
/** The shared machine floors. */
export const DISK_FLOOR_GB = 50;
export const MEMORY_FLOOR_GB = 6;
/** Threads quiet for longer are left out unless they report `blocked`. */
export const QUIET_DAY_MS = 86_400_000;
/** A pm-mail-only thread counts as stale after this long without a pm-mail call. */
export const PM_MAIL_STALE_MS = 30 * 60_000;
/** The staleness limit a null `ttlSeconds` uses, except while waiting on the owner (F10): one day. */
export const NULL_TTL_DEFAULT_SECONDS = 86_400;
/** A stream board (tasks.json) not rewritten for this long shows as stale. */
export const TASKS_STALE_MS = 86_400_000;

export const STALE_AFTER_SECONDS = Object.freeze({
  'agent-reports': 60,
  'pm-mail': 120,
  machines: 180,
  browsers: 180,
  simlock: 180,
});

export const GIB = 1024 ** 3;

// ---------------------------------------------------------------------------------------------
// Words

export function iso(ms: number | null): string | null {
  return ms === null || !Number.isFinite(ms) ? null : new Date(ms).toISOString();
}

export function ageSeconds(now: number, ms: number | null): number | null {
  return ms === null || !Number.isFinite(ms) ? null : Math.max(0, Math.round((now - ms) / 1000));
}

function format(ms: number, timeZone: string, options: Intl.DateTimeFormatOptions): string {
  let text: string;
  try {
    text = new Intl.DateTimeFormat('en-US', { timeZone, ...options }).format(ms);
  } catch {
    text = new Intl.DateTimeFormat('en-US', { timeZone: 'UTC', ...options }).format(ms);
  }
  return text.replace(/[  ]/g, ' ');
}

/** "4:02 PM" in the dashboard's time zone. */
export function clockText(ms: number, timeZone: string): string {
  return format(ms, timeZone, { hour: 'numeric', minute: '2-digit' });
}

/** "Nov 12" in the dashboard's time zone. */
export function dayText(ms: number, timeZone: string): string {
  return format(ms, timeZone, { month: 'short', day: 'numeric' });
}

/** "1d 11h", "2h 35m", "4m", "now". */
export function spanText(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return 'now';
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return `${Math.floor(seconds)}s`;
}

export function plural(count: number, word: string, many = `${word}s`): string {
  return `${count} ${count === 1 ? word : many}`;
}

export function gb(bytes: number | null): number | null {
  return bytes === null || !Number.isFinite(bytes) ? null : Math.round((bytes / GIB) * 10) / 10;
}

export function round1(value: number | null): number | null {
  return value === null || !Number.isFinite(value) ? null : Math.round(value * 10) / 10;
}
