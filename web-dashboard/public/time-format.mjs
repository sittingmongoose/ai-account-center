// Every displayed time follows the display time zone from Settings (America/New_York until one is
// saved). bridge.js sets it once from GET /api/accounts/preferences and after every save; formatters
// come from here so tests set the zone explicitly instead of depending on the machine's zone.
export const DEFAULT_DISPLAY_TIME_ZONE = 'America/New_York';

let displayTimeZone = DEFAULT_DISPLAY_TIME_ZONE;
const cache = new Map();

/** True for a zone Intl accepts (the server applies the same rule, plus its name shape). */
export function isDisplayTimeZone(value) {
  if (typeof value !== 'string' || !value || value.length > 64) return false;
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: value }).format(0);
    return true;
  } catch {
    return false;
  }
}

export function setDisplayTimeZone(value) {
  if (!isDisplayTimeZone(value)) return false;
  if (value !== displayTimeZone) {
    displayTimeZone = value;
    cache.clear();
  }
  return true;
}

export function getDisplayTimeZone() {
  return displayTimeZone;
}

/** A cached formatter in the display zone. `options` must be a stable key shape per call site. */
export function displayFormat(options) {
  const key = `${displayTimeZone}|${JSON.stringify(options)}`;
  let formatter = cache.get(key);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(undefined, { ...options, timeZone: displayTimeZone });
    if (cache.size > 32) cache.clear();
    cache.set(key, formatter);
  }
  return formatter;
}

/**
 * A module-level formatter that follows the display zone: call sites keep calling `.format` (or
 * `.formatToParts`), and the zone underneath is whatever `setDisplayTimeZone` last saved.
 */
export function lazyFormat(options) {
  return {
    format: (value) => displayFormat(options).format(value),
    formatToParts: (value) => displayFormat(options).formatToParts(value),
  };
}

const WEEKDAY = { Sun: 0, Mon: 1, Tue: 2, Wed: 3, Thu: 4, Fri: 5, Sat: 6 };

/** The wall clock in the display zone: { year, month, day, hour, minute, second, weekdaySun0 }. */
export function zonedParts(epoch) {
  const parts = displayFormat({
    weekday: 'short',
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: 'numeric',
    second: 'numeric',
    hour12: false,
  }).formatToParts(new Date(epoch));
  const get = (type) => parts.find((part) => part.type === type)?.value;
  return {
    year: Number(get('year')),
    month: Number(get('month')),
    day: Number(get('day')),
    hour: Number(get('hour')) % 24,
    minute: Number(get('minute')),
    second: Number(get('second') ?? 0),
    weekdaySun0: WEEKDAY[get('weekday')] ?? 0,
  };
}

/** Monday-based weekday (0..6) and hour (0..23) in the display zone. */
export function zonedWeekday(epoch) {
  return (zonedParts(epoch).weekdaySun0 + 6) % 7;
}
export function zonedHour(epoch) {
  return zonedParts(epoch).hour;
}

const sameZonedDay = (a, b) => a.year === b.year && a.month === b.month && a.day === b.day;

/**
 * Midnight starting the display-zone day holding `epoch`. The elapsed subtraction overshoots by an
 * hour on a 25-hour day (and any zone's midnight transition), so the candidate is verified to read
 * 00:00:00 on the wanted day and nudged by whole hours until it does.
 */
export function zonedDayStart(epoch) {
  const want = zonedParts(epoch);
  let start = epoch - (want.hour * 3600 + want.minute * 60 + want.second) * 1000 - (epoch % 1000);
  const midnight = (p) =>
    p.hour === 0 && p.minute === 0 && p.second === 0 && sameZonedDay(p, want);
  for (let i = 0; i < 4; i++) {
    if (midnight(zonedParts(start))) return start;
    start -= 3_600_000;
  }
  for (let i = 0; i < 4; i++) {
    start += 3_600_000;
    if (midnight(zonedParts(start))) return start;
  }
  return start - 3_600_000;
}

/**
 * `zonedDayStart` shifted by whole display-zone days: each step lands inside the next day (+25h
 * forward clears a 25-hour Sunday; -1h back lands inside the day before) and re-truncates.
 */
export function zonedAddDays(epoch, n) {
  let start = zonedDayStart(epoch);
  if (n > 0) for (let i = 0; i < n; i++) start = zonedDayStart(start + 90_000_000);
  if (n < 0) for (let i = 0; i < -n; i++) start = zonedDayStart(start - 3_600_000);
  return start;
}

/** Midnight starting the display-zone month holding `epoch`. */
export function zonedMonthStart(epoch) {
  return zonedAddDays(zonedDayStart(epoch), 1 - zonedParts(epoch).day);
}

/** The display-zone hour holding `epoch`, truncated. */
export function zonedHourStart(epoch) {
  const p = zonedParts(epoch);
  return epoch - (p.minute * 60 + p.second) * 1000 - (epoch % 1000);
}

/** The short zone name shown beside times ("EDT", "EST"); '' when it cannot be read. */
export function displayZoneName(now = Date.now()) {
  try {
    return (
      displayFormat({ timeZoneName: 'short' })
        .formatToParts(new Date(now))
        .find((part) => part.type === 'timeZoneName')?.value || ''
    );
  } catch {
    return '';
  }
}
