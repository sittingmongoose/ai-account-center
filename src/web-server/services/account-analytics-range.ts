/**
 * Analytics ranges and local-day bucketing.
 *
 * Quota history and local CLI activity are both retained for about a month,
 * so every range, including a custom one, stays inside that window. Days are
 * bucketed in an IANA time zone the client names; hourly buckets remain UTC
 * instants that clients format locally.
 */

export type AccountAnalyticsRangePreset = '24h' | '7d' | '30d' | 'month' | 'all' | 'custom';

export const ACCOUNT_ANALYTICS_RANGE_PRESETS: readonly AccountAnalyticsRangePreset[] = [
  '24h',
  '7d',
  '30d',
  'month',
  'all',
  'custom',
];

/** Local activity is kept for 31 days; quota history for 30. Ranges never reach further back. */
export const ACCOUNT_ANALYTICS_RETAINED_MS = 31 * 86_400_000;
export const ACCOUNT_ANALYTICS_MAX_CUSTOM_DAYS = 31;

const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
const FIXED_RANGE_MS: Record<'24h' | '7d' | '30d', number> = {
  '24h': DAY_MS,
  '7d': 7 * DAY_MS,
  '30d': 30 * DAY_MS,
};

export type AccountAnalyticsQueryErrorCode =
  | 'invalid_query'
  | 'invalid_range'
  | 'invalid_tz'
  | 'invalid_provider';

/** A request the client can correct; the route answers 400 with this fixed sentence and code. */
export class AccountAnalyticsQueryError extends Error {
  constructor(
    readonly code: AccountAnalyticsQueryErrorCode,
    message: string
  ) {
    super(message);
    this.name = 'AccountAnalyticsQueryError';
  }
}

export const ANALYTICS_QUERY_ERRORS: Record<AccountAnalyticsQueryErrorCode, string> = {
  invalid_query: 'Select a valid platform, time range, provider, account, and refresh flag.',
  invalid_range:
    'Select a range of up to 31 retained days, with the start date on or before the end date.',
  invalid_tz: 'Select a valid IANA time zone.',
  invalid_provider: 'Select a provider that this server reports.',
};

export function analyticsQueryError(
  code: AccountAnalyticsQueryErrorCode
): AccountAnalyticsQueryError {
  return new AccountAnalyticsQueryError(code, ANALYTICS_QUERY_ERRORS[code]);
}

// ---------------------------------------------------------------------------
// Time zones
// ---------------------------------------------------------------------------

const MAX_FORMATTERS = 32;
const formatters = new Map<string, Intl.DateTimeFormat>();
let supportedZones: Set<string> | null | undefined;

function zoneList(): Set<string> | null {
  if (supportedZones !== undefined) return supportedZones;
  const supportedValuesOf = (
    Intl as unknown as { supportedValuesOf?: (key: 'timeZone') => string[] }
  ).supportedValuesOf;
  try {
    supportedZones =
      typeof supportedValuesOf === 'function' ? new Set(supportedValuesOf('timeZone')) : null;
  } catch {
    supportedZones = null;
  }
  return supportedZones;
}

function formatter(tz: string): Intl.DateTimeFormat {
  let cached = formatters.get(tz);
  if (!cached) {
    cached = new Intl.DateTimeFormat('en-US', {
      timeZone: tz,
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    });
    formatters.set(tz, cached);
    while (formatters.size > MAX_FORMATTERS) {
      const oldest = formatters.keys().next().value;
      if (oldest === undefined) break;
      formatters.delete(oldest);
    }
  }
  return cached;
}

/**
 * True for `UTC` and for zones `Intl.supportedValuesOf('timeZone')` lists. A
 * legacy alias the runtime accepts (for example `Asia/Calcutta`) is accepted
 * when it resolves to a listed zone, so a browser's own zone name is not refused.
 */
export function isAccountAnalyticsTimeZone(value: unknown): value is string {
  if (typeof value !== 'string' || !/^[A-Za-z][A-Za-z0-9_+\-/]{0,63}$/.test(value)) return false;
  if (value === 'UTC') return true;
  const zones = zoneList();
  if (zones?.has(value)) return true;
  try {
    const resolved = new Intl.DateTimeFormat('en-US', { timeZone: value }).resolvedOptions()
      .timeZone;
    return zones ? zones.has(resolved) || resolved === 'UTC' : true;
  } catch {
    return false;
  }
}

interface LocalParts {
  year: number;
  month: number;
  day: number;
  hour: number;
  minute: number;
  second: number;
}

function localParts(epoch: number, tz: string): LocalParts {
  const values: Record<string, number> = {};
  for (const part of formatter(tz).formatToParts(new Date(epoch)))
    if (part.type !== 'literal') values[part.type] = Number(part.value);
  return {
    year: values.year,
    month: values.month,
    day: values.day,
    // Some engines print midnight as 24 with hour12 disabled; it is still that day.
    hour: values.hour === 24 ? 0 : values.hour,
    minute: values.minute,
    second: values.second,
  };
}

/** The zone's offset from UTC at an instant, in milliseconds (east positive). */
export function zoneOffsetMs(epoch: number, tz: string): number {
  const parts = localParts(epoch, tz);
  const asUtc = Date.UTC(
    parts.year,
    parts.month - 1,
    parts.day,
    parts.hour,
    parts.minute,
    parts.second
  );
  return asUtc - Math.floor(epoch / 1000) * 1000;
}

function pad(value: number): string {
  return String(value).padStart(2, '0');
}

/** `YYYY-MM-DD` of an instant in a zone. */
export function localDate(epoch: number, tz: string): string {
  const parts = localParts(epoch, tz);
  return `${String(parts.year).padStart(4, '0')}-${pad(parts.month)}-${pad(parts.day)}`;
}

/** A real calendar date in `YYYY-MM-DD` form. */
export function isCalendarDate(value: unknown): value is string {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const [year, month, day] = value.split('-').map(Number);
  const epoch = Date.UTC(year, month - 1, day);
  return (
    year >= 1970 &&
    new Date(epoch).getUTCFullYear() === year &&
    new Date(epoch).getUTCMonth() === month - 1 &&
    new Date(epoch).getUTCDate() === day
  );
}

function dayNumber(date: string): number {
  const [year, month, day] = date.split('-').map(Number);
  return Math.round(Date.UTC(year, month - 1, day) / DAY_MS);
}

export function addCalendarDays(date: string, days: number): string {
  return new Date((dayNumber(date) + days) * DAY_MS).toISOString().slice(0, 10);
}

/** The first instant of a local calendar day, including days whose midnight a DST change skips. */
export function localMidnight(date: string, tz: string): number {
  const guess = dayNumber(date) * DAY_MS;
  let epoch = guess - zoneOffsetMs(guess, tz);
  epoch = guess - zoneOffsetMs(epoch, tz);
  const step = 15 * 60_000;
  for (let index = 0; index < 8 && localDate(epoch - step, tz) === date; index++) epoch -= step;
  for (let index = 0; index < 8 && localDate(epoch, tz) < date; index++) epoch += step;
  return epoch;
}

/** True when the zone is not a whole number of hours from UTC anywhere in the range. */
export function hasPartialHourOffset(tz: string, from: number, to: number): boolean {
  const samples = [from, to, from + (to - from) / 2];
  return samples.some((epoch) => Math.abs(zoneOffsetMs(epoch, tz)) % HOUR_MS !== 0);
}

// ---------------------------------------------------------------------------
// Ranges
// ---------------------------------------------------------------------------

export interface AccountAnalyticsRangeRequest {
  range: AccountAnalyticsRangePreset;
  tz?: string;
  from?: string;
  to?: string;
}

export interface ResolvedAccountAnalyticsRange {
  preset: AccountAnalyticsRangePreset;
  from: number;
  to: number;
  tz: string;
  bucketMinutes: number;
}

/** Quota points keep today's resolution: 10 minutes for a day, hourly for a week, 3 hours beyond. */
export function analyticsBucketMinutes(spanMs: number): number {
  if (spanMs <= DAY_MS + 2 * HOUR_MS) return 10;
  if (spanMs <= 7 * DAY_MS + 2 * HOUR_MS) return 60;
  return 180;
}

/** Checks the parts of a range that do not depend on the clock. */
export function validateAccountAnalyticsRangeShape(request: AccountAnalyticsRangeRequest): void {
  if (!ACCOUNT_ANALYTICS_RANGE_PRESETS.includes(request.range))
    throw analyticsQueryError('invalid_range');
  if (request.tz !== undefined && !isAccountAnalyticsTimeZone(request.tz))
    throw analyticsQueryError('invalid_tz');
  if (request.range === 'custom') {
    if (!isCalendarDate(request.from) || !isCalendarDate(request.to))
      throw analyticsQueryError('invalid_range');
    if (request.from > request.to) throw analyticsQueryError('invalid_range');
    if (dayNumber(request.to) - dayNumber(request.from) + 1 > ACCOUNT_ANALYTICS_MAX_CUSTOM_DAYS)
      throw analyticsQueryError('invalid_range');
  } else if (request.from !== undefined || request.to !== undefined) {
    throw analyticsQueryError('invalid_range');
  }
}

/**
 * Resolve a range to instants. `availableFrom` is the oldest retained quota
 * sample or activity hour (already bounded to the retained window); `all`
 * starts there.
 */
export function resolveAccountAnalyticsRange(
  request: AccountAnalyticsRangeRequest,
  now: number,
  availableFrom: number
): ResolvedAccountAnalyticsRange {
  validateAccountAnalyticsRangeShape(request);
  const tz = request.tz ?? 'UTC';
  let from: number;
  let to = now;
  switch (request.range) {
    case '24h':
    case '7d':
    case '30d':
      from = now - FIXED_RANGE_MS[request.range];
      break;
    case 'month':
      from = localMidnight(`${localDate(now, tz).slice(0, 7)}-01`, tz);
      break;
    case 'all':
      from = Math.min(now, Math.max(now - ACCOUNT_ANALYTICS_RETAINED_MS, availableFrom));
      break;
    case 'custom': {
      const first = request.from as string;
      const last = request.to as string;
      if (last > localDate(now, tz) || first < localDate(now - ACCOUNT_ANALYTICS_RETAINED_MS, tz))
        throw analyticsQueryError('invalid_range');
      from = localMidnight(first, tz);
      to = Math.min(now, localMidnight(addCalendarDays(last, 1), tz) - 1000);
      break;
    }
  }
  return {
    preset: request.range,
    from,
    to,
    tz,
    bucketMinutes: analyticsBucketMinutes(to - from),
  };
}
