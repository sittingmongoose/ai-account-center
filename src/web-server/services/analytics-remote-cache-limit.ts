/**
 * One size limit for a remote computer's Analytics cache, used by save and load alike (Stage 1
 * review M3, 2026-11-13). A save the next load would refuse is trimmed to fit instead of written
 * and then thrown away on every load, and a cache the load does discard says why.
 */
import type {
  AnalyticsRemoteRow,
  AnalyticsRemoteSessionRow,
  AnalyticsSessionEvidence,
} from './analytics-remote-transport';

/** The most a host's cache file may hold, on save and on load. */
export const MAX_REMOTE_CACHE_BYTES = 32 * 1024 * 1024;

export type RemoteCacheDiscardCause = 'cache_oversize' | 'cache_unreadable' | 'cache_format';

/** Why the saved cache was discarded when loaded; kept until a complete read. */
export interface RemoteCacheDiscard {
  cause: RemoteCacheDiscardCause;
  at: string;
}

/** Usage older than `before` was dropped so the cache fits its limit (file prints kept). */
export interface RemoteCacheTrim {
  cause: 'cache_oversize';
  before: string;
  at: string;
}

interface Fittable {
  rows: AnalyticsRemoteRow[];
  srows: AnalyticsRemoteSessionRow[];
  sessions?: Record<string, AnalyticsSessionEvidence>;
  trimmed?: RemoteCacheTrim;
}

const HOUR_MS = 3_600_000;
const DISCARD_CAUSES: ReadonlySet<string> = new Set([
  'cache_oversize',
  'cache_unreadable',
  'cache_format',
]);

export function remoteCacheBytes(value: unknown): number {
  return Buffer.byteLength(JSON.stringify(value));
}

function rowHour(row: AnalyticsRemoteRow): number {
  const epoch = Date.parse(`${String(row.h).replace(' ', 'T')}:00Z`);
  return Number.isFinite(epoch) ? epoch : -Infinity;
}

function sessionHour(row: AnalyticsRemoteSessionRow): number {
  return Number.isFinite(row.z) ? Math.floor(row.z / HOUR_MS) * HOUR_MS : -Infinity;
}

/**
 * A cache over the limit, made to fit. Session evidence goes first (a diagnostic), then the oldest
 * hours of usage, hourly rows and session rows alike, down to 90% of the limit so the next save
 * does not trim again at once. Every file's print stays, so the dropped hours are not read again
 * in a loop; `trimmed` names the cut. Null when even dropping all usage would not fit (the prints
 * alone are over the limit): the caller then keeps the last cache it saved.
 */
export function fitRemoteCache<T extends Fittable>(
  value: T,
  at: string,
  limit = MAX_REMOTE_CACHE_BYTES
): T | null {
  const fitted: T = { ...value, sessions: {} };
  let total = remoteCacheBytes(fitted);
  if (total <= limit) return fitted;
  const byHour = new Map<number, number>();
  const add = (hour: number, bytes: number): void => {
    byHour.set(hour, (byHour.get(hour) ?? 0) + bytes);
  };
  for (const row of fitted.rows) add(rowHour(row), remoteCacheBytes(row) + 1);
  for (const row of fitted.srows) add(sessionHour(row), remoteCacheBytes(row) + 1);
  let keepFrom = -Infinity;
  for (const hour of [...byHour.keys()].sort((a, b) => a - b)) {
    if (total <= limit * 0.9) break;
    total -= byHour.get(hour) ?? 0;
    keepFrom = hour === -Infinity ? keepFrom : hour + HOUR_MS;
  }
  const earlier = value.trimmed ? Date.parse(value.trimmed.before) : -Infinity;
  const before = Math.max(keepFrom, Number.isFinite(earlier) ? earlier : -Infinity);
  const trimmed: T = {
    ...fitted,
    rows: fitted.rows.filter((row) => rowHour(row) >= keepFrom && rowHour(row) > -Infinity),
    srows: fitted.srows.filter(
      (row) => sessionHour(row) >= keepFrom && sessionHour(row) > -Infinity
    ),
    ...(Number.isFinite(before)
      ? { trimmed: { cause: 'cache_oversize', before: new Date(before).toISOString(), at } }
      : {}),
  };
  return remoteCacheBytes(trimmed) <= limit ? trimmed : null;
}

/** A saved discard record, or undefined when absent or damaged. */
export function parseRemoteCacheDiscard(value: unknown): RemoteCacheDiscard | undefined {
  const item = value as Partial<RemoteCacheDiscard> | undefined;
  return item &&
    typeof item.cause === 'string' &&
    DISCARD_CAUSES.has(item.cause) &&
    typeof item.at === 'string' &&
    Number.isFinite(Date.parse(item.at))
    ? { cause: item.cause, at: item.at }
    : undefined;
}

/** A saved trim record, or undefined when absent or damaged. */
export function parseRemoteCacheTrim(value: unknown): RemoteCacheTrim | undefined {
  const item = value as Partial<RemoteCacheTrim> | undefined;
  return item &&
    item.cause === 'cache_oversize' &&
    typeof item.before === 'string' &&
    Number.isFinite(Date.parse(item.before)) &&
    typeof item.at === 'string' &&
    Number.isFinite(Date.parse(item.at))
    ? { cause: 'cache_oversize', before: item.before, at: item.at }
    : undefined;
}

const DISCARD_WORDS: Record<RemoteCacheDiscardCause, string> = {
  cache_oversize: 'it was over its 32 MB limit',
  cache_unreadable: 'it could not be read',
  cache_format: 'its format is unknown or damaged',
};

/** What the page adds to a host's cells about its cache, or null when there is nothing to say. */
export function remoteCacheNote(
  discarded: RemoteCacheDiscard | undefined,
  trimmed: RemoteCacheTrim | undefined,
  minDateMs: number
): string | null {
  const notes: string[] = [];
  if (discarded)
    notes.push(
      `this computer's saved Analytics cache was discarded at ${discarded.at} (${discarded.cause}: ${DISCARD_WORDS[discarded.cause]}); it is being read again from the start`
    );
  if (trimmed && Date.parse(trimmed.before) > minDateMs)
    notes.push(
      `usage before ${trimmed.before} is not kept (cache_oversize): this computer's saved Analytics cache reached its 32 MB limit`
    );
  return notes.length ? notes.join('; ') : null;
}
