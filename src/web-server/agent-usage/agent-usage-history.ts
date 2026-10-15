import {
  accountAnalyticsIdentity,
  analyticsWindowIdentity,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsObservation,
} from '../services/account-analytics-history';
import type { DashboardAccount, DashboardAccountWindow } from '../services/account-dashboard-types';
import type { AgentUsageAccount } from './agent-usage-projection';

/**
 * Burn rates and the rolling history for agents, from the dashboard's own quota history (every
 * changed reading of the last hour, 5-minute buckets to a day, hourly buckets beyond) plus every
 * one-minute reading this dashboard saw in the last hour, so a window being used up within minutes
 * shows its pace at once. Only measured readings are used: a rate needs two real readings of the
 * same window in the same reset period, and nothing is extrapolated across a reset.
 */
export const HISTORY_HOURS = 7 * 24;
const HOUR_MS = 3_600_000;
/** A larger fall than this between two readings is a reset, not noise. */
const RESET_DROP_POINTS = 1;
/** Readings whose reset times differ by more than this belong to different periods. */
const RESET_SHIFT_MS = 10 * 60_000;
const HORIZONS = [
  { name: 'last5Minutes', hours: 5 / 60, minimumSpanMs: 2 * 60_000 },
  { name: 'last15Minutes', hours: 15 / 60, minimumSpanMs: 5 * 60_000 },
  { name: 'lastHour', hours: 1, minimumSpanMs: 15 * 60_000 },
  { name: 'last6Hours', hours: 6, minimumSpanMs: HOUR_MS },
  { name: 'last24Hours', hours: 24, minimumSpanMs: 4 * HOUR_MS },
] as const;
/** Below this pace (percentage points per hour) a window is treated as not rising. */
const MIN_PACE = 0.05;
/**
 * A span under an hour sets the pace only when use moved at least this much: providers report
 * whole percentages, so one step within minutes is rounding, not a burst (2 points in 5 minutes,
 * 24%/h, still shows a burst at once).
 */
const PACE_MIN_POINTS = 2;
/** Fractional percentages (Cursor reports 30.3) subtract inexactly: 32.3 - 30.3 is 1.9999999999999964. */
const POINT_TOLERANCE = 1e-9;
/** How long the one-minute readings are kept in memory. */
export const RECENT_READINGS_MS = 65 * 60_000;
const MAX_RECENT_READINGS = 180;

export interface BurnRate {
  /** Percentage points used per hour over each span, measured within the current reset period. */
  last5Minutes: number | null;
  last15Minutes: number | null;
  lastHour: number | null;
  last6Hours: number | null;
  last24Hours: number | null;
  /**
   * The pace the projection uses: the shortest span with enough readings and, under an hour, a move
   * of at least 2 points, so bursts show at once and a single rounding step does not.
   */
  pace: number | null;
  /** That span in minutes. */
  paceMinutes: number | null;
  /** When the window reaches 100% at that pace; null when it is not rising. */
  projectedFullAt: string | null;
  /** Whether that comes before the window resets; null when either time is unknown. */
  runsOutBeforeReset: boolean | null;
  /** When the current reset period's first recorded reading was taken. */
  periodStartedAt: string | null;
}

/**
 * One reading of one window: [time (ISO, minute), used percent, remaining amount, reset time (ISO)
 * or null]. Readings with different reset times belong to different periods: never average across.
 */
export type HistoryPoint = [string, number | null, number | null, string | null];

export interface HistorySeries {
  accountId: string;
  provider: string;
  label: string;
  email: string | null;
  profile: string | null;
  /** Hidden on the dashboard by its owner; the reader skips it unless asked. */
  hidden: boolean;
  window: string;
  windowLabel: string;
  pool: string | null;
  kind: string;
  unit: string | null;
  points: HistoryPoint[];
}

export interface Reading {
  at: number;
  used: number | null;
  remaining: number | null;
  resetAt: number | null;
}

function minuteIso(epoch: number): string {
  return new Date(Math.floor(epoch / 60_000) * 60_000).toISOString().replace(':00.000Z', 'Z');
}

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/** Each account's records, oldest first, keyed by the history's account identity. */
export function recordsByIdentity(
  history: AccountAnalyticsHistoryData | null
): Map<string, AccountAnalyticsObservation[]> {
  const result = new Map<string, AccountAnalyticsObservation[]>();
  for (const record of history?.records ?? []) {
    const list = result.get(record.identity) ?? [];
    list.push(record);
    result.set(record.identity, list);
  }
  for (const list of result.values())
    list.sort((a, b) => Date.parse(a.sampledAt) - Date.parse(b.sampledAt));
  return result;
}

/**
 * When a window was observed: a retained (`cached`) optional window keeps its own original sample
 * time inside a newer record, so an old balance never counts as a fresh reading.
 */
function observedAt(window: DashboardAccountWindow, recordTime: string | null): number {
  return Date.parse(
    (window.status === 'cached' && window.sampledAt ? window.sampledAt : recordTime) ?? ''
  );
}

/** The window's readings, oldest first, one per observation time. */
export function windowReadings(
  records: AccountAnalyticsObservation[],
  window: DashboardAccountWindow,
  since: number
): Reading[] {
  const identity = analyticsWindowIdentity(window);
  const readings: Reading[] = [];
  for (const record of records) {
    const match = record.windows.find((entry) => analyticsWindowIdentity(entry) === identity);
    if (!match) continue;
    const at = observedAt(match, record.sampledAt);
    if (!Number.isFinite(at) || at < since) continue;
    const used = finite(match.usedPercent);
    const remaining = finite(match.remaining);
    const reset = Date.parse(match.resetAt ?? '');
    // A reading with only a reset time is kept: it marks a period boundary, never a rate.
    if (used === null && remaining === null && !Number.isFinite(reset)) continue;
    readings.push({ at, used, remaining, resetAt: Number.isFinite(reset) ? reset : null });
  }
  // One reading per observation: a retained window repeats its time in later records.
  const byTime = new Map<number, Reading>();
  for (const reading of readings) if (!byTime.has(reading.at)) byTime.set(reading.at, reading);
  return [...byTime.values()].sort((a, b) => a.at - b.at);
}

/** Both lists merged, oldest first, one reading per time (the later list wins a tie). */
export function mergeReadings(stored: Reading[], recent: Reading[]): Reading[] {
  const byTime = new Map<number, Reading>();
  for (const reading of [...stored, ...recent]) byTime.set(reading.at, reading);
  return [...byTime.values()].sort((a, b) => a.at - b.at);
}

/** The reading a dashboard row carries for one window, at its own sample time. */
export function rowReading(row: DashboardAccount, window: DashboardAccountWindow): Reading | null {
  if (row.status !== 'ok' && row.status !== 'cached') return null;
  const at = observedAt(window, row.sampledAt);
  const used = finite(window.usedPercent);
  const remaining = finite(window.remaining);
  const reset = Date.parse(window.resetAt ?? '');
  if (!Number.isFinite(at) || (used === null && remaining === null && !Number.isFinite(reset)))
    return null;
  return { at, used, remaining, resetAt: Number.isFinite(reset) ? reset : null };
}

/** The same account identity the quota history uses (provider, id and email), plus the window. */
function seriesKey(row: DashboardAccount, window: DashboardAccountWindow): string {
  return JSON.stringify([accountAnalyticsIdentity(row), analyticsWindowIdentity(window)]);
}

/**
 * The window without its optional unit: a provider can drop the unit from one reading (Qwen does
 * when its quota-config request fails), which puts that reading in another series, but its reset
 * time still bounds this window's periods.
 */
function boundaryIdentity(window: DashboardAccountWindow): string {
  return JSON.stringify([window.key, window.kind ?? null, window.poolId ?? null]);
}

function boundaryKey(row: DashboardAccount, window: DashboardAccountWindow): string {
  return JSON.stringify([accountAnalyticsIdentity(row), boundaryIdentity(window)]);
}

/** Every reset time the stored history reports for this window, whatever unit each reading had. */
export function storedBoundaries(
  records: AccountAnalyticsObservation[],
  window: DashboardAccountWindow,
  since: number
): number[] {
  const identity = boundaryIdentity(window);
  const found = new Set<number>();
  for (const record of records)
    for (const entry of record.windows) {
      if (boundaryIdentity(entry) !== identity) continue;
      const at = observedAt(entry, record.sampledAt);
      const reset = Date.parse(entry.resetAt ?? '');
      if (Number.isFinite(at) && at >= since && Number.isFinite(reset)) found.add(reset);
    }
  return [...found];
}

/**
 * The one-minute readings of the last hour, kept in memory by the snapshot writer. The quota
 * history stores an unchanged reading at most once per five minutes; these keep every sample, so
 * a quiet window followed by a burst still measures its pace over the right span.
 */
export class RecentReadings {
  private readonly series = new Map<string, Reading[]>();
  /** Reset times seen in the last hour, by account and window without its unit, with when seen. */
  private readonly resets = new Map<string, Map<number, number>>();

  add(rows: DashboardAccount[], now: number): void {
    for (const row of rows)
      for (const window of row.windows ?? []) {
        const reading = rowReading(row, window);
        if (!reading) continue;
        if (reading.resetAt !== null) {
          const seen = this.resets.get(boundaryKey(row, window)) ?? new Map<number, number>();
          seen.set(reading.resetAt, reading.at);
          this.resets.set(boundaryKey(row, window), seen);
        }
        const key = seriesKey(row, window);
        const list = this.series.get(key) ?? [];
        if (!list.some((entry) => entry.at === reading.at)) list.push(reading);
        list.sort((a, b) => a.at - b.at);
        this.series.set(key, list.slice(-MAX_RECENT_READINGS));
      }
    for (const [key, list] of this.series) {
      const kept = list.filter((reading) => reading.at >= now - RECENT_READINGS_MS);
      if (kept.length) this.series.set(key, kept);
      else this.series.delete(key);
    }
    for (const [key, seen] of this.resets) {
      for (const [reset, at] of seen) if (at < now - RECENT_READINGS_MS) seen.delete(reset);
      if (seen.size === 0) this.resets.delete(key);
    }
  }

  /** Reset times seen for this window in the last hour, whatever unit each reading had. */
  boundaries(row: DashboardAccount, window: DashboardAccountWindow): number[] {
    return [...(this.resets.get(boundaryKey(row, window))?.keys() ?? [])];
  }

  get(row: DashboardAccount, window: DashboardAccountWindow): Reading[] {
    return this.series.get(seriesKey(row, window)) ?? [];
  }
}

/** The reading names a reset still ahead of it (not one it keeps reporting after it passed). */
function upcoming(reading: Reading): reading is Reading & { resetAt: number } {
  return reading.resetAt !== null && reading.resetAt > reading.at;
}

/**
 * The readings since the last reset. Two neighbouring readings with a percentage are in different
 * periods when:
 * - any reported reset time falls between them (`earlier.at < reset <= later.at`), whichever
 *   reading reported it, one without a percentage included, so a source that omits the time cannot
 *   hide a reset;
 * - both name an upcoming reset and those differ by more than 10 minutes;
 * - use falls: by more than a point when both name the same upcoming reset (a rolling window
 *   easing), by any amount otherwise, since a reset cannot then be told from noise.
 */
function currentPeriod(readings: Reading[], extraBoundaries: number[] = []): Reading[] {
  const boundaries = [
    ...new Set([
      ...readings.flatMap((reading) => (reading.resetAt === null ? [] : [reading.resetAt])),
      ...extraBoundaries,
    ]),
  ];
  const usable = readings.filter((reading) => reading.used !== null);
  let start = usable.length - 1;
  while (start > 0) {
    const earlier = usable[start - 1];
    const later = usable[start];
    if (boundaries.some((reset) => earlier.at < reset && reset <= later.at)) break;
    const sameUpcoming = upcoming(earlier) && upcoming(later);
    if (
      sameUpcoming &&
      Math.abs((earlier.resetAt as number) - (later.resetAt as number)) > RESET_SHIFT_MS
    )
      break;
    const fall = (earlier.used as number) - (later.used as number);
    if (fall > (sameUpcoming ? RESET_DROP_POINTS : 0)) break;
    start--;
  }
  return start < 0 ? [] : usable.slice(start);
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}

/** One window of an account: its key, unit and pool, as the history tells windows apart. */
export function burnKey(key: string, unit: string | null, poolId: string | null): string {
  return JSON.stringify([key, unit, poolId]);
}

export function burnRate(
  readings: Reading[],
  resetAt: string | null,
  extraBoundaries: number[] = []
): BurnRate {
  const period = currentPeriod(readings, extraBoundaries);
  const rates: Record<(typeof HORIZONS)[number]['name'], number | null> = {
    last5Minutes: null,
    last15Minutes: null,
    lastHour: null,
    last6Hours: null,
    last24Hours: null,
  };
  let pace: number | null = null;
  let paceMinutes: number | null = null;
  const latest = period[period.length - 1];
  if (latest) {
    for (const horizon of HORIZONS) {
      const earliest = period.find((reading) => reading.at >= latest.at - horizon.hours * HOUR_MS);
      if (!earliest || latest.at - earliest.at < horizon.minimumSpanMs) continue;
      const rate = round(
        ((latest.used as number) - (earliest.used as number)) /
          ((latest.at - earliest.at) / HOUR_MS)
      );
      rates[horizon.name] = rate;
      const moved = Math.abs((latest.used as number) - (earliest.used as number));
      if (
        pace === null &&
        (moved >= PACE_MIN_POINTS - POINT_TOLERANCE || latest.at - earliest.at >= HOUR_MS)
      ) {
        pace = rate;
        paceMinutes = Math.round(horizon.hours * 60);
      }
    }
  }
  let projectedFullAt: string | null = null;
  if (latest && (latest.used as number) >= 100) projectedFullAt = new Date(latest.at).toISOString();
  else if (latest && pace !== null && pace >= MIN_PACE)
    projectedFullAt = new Date(
      latest.at + ((100 - (latest.used as number)) / pace) * HOUR_MS
    ).toISOString();
  const reset = Date.parse(resetAt ?? '');
  return {
    ...rates,
    pace,
    paceMinutes,
    projectedFullAt,
    runsOutBeforeReset:
      projectedFullAt === null || !Number.isFinite(reset)
        ? null
        : Date.parse(projectedFullAt) < reset,
    periodStartedAt: period.length > 0 ? new Date(period[0].at).toISOString() : null,
  };
}

/**
 * Burn rates for every percent window of the projected accounts, keyed by account id, then window
 * key, plus the rolling history series for every window (percent or amount).
 */
export function usageHistory(
  rows: DashboardAccount[],
  accounts: AgentUsageAccount[],
  history: AccountAnalyticsHistoryData | null,
  now: number,
  recent: RecentReadings | null = null
): { rates: Map<string, Map<string, BurnRate>>; series: HistorySeries[] } {
  const byIdentity = recordsByIdentity(history);
  const since = now - HISTORY_HOURS * HOUR_MS;
  // The newest row per id, as the projection publishes it.
  const rowById = new Map<string, DashboardAccount>();
  for (const row of rows) {
    const existing = rowById.get(row.id);
    if (!existing || Date.parse(row.sampledAt ?? '') > Date.parse(existing.sampledAt ?? ''))
      rowById.set(row.id, row);
  }
  const rates = new Map<string, Map<string, BurnRate>>();
  const series: HistorySeries[] = [];
  for (const account of accounts) {
    const row = rowById.get(account.id);
    if (!row) continue;
    const records = byIdentity.get(accountAnalyticsIdentity(row)) ?? [];
    const perWindow = new Map<string, BurnRate>();
    // Every window measured in the last 7 days, so an outage or a window the provider stopped
    // reporting keeps its history; only windows in the current reading get a reading and a pace.
    const windows = new Map<string, DashboardAccountWindow>();
    for (const record of records)
      if (Date.parse(record.sampledAt) >= since)
        for (const window of record.windows) windows.set(analyticsWindowIdentity(window), window);
    const currentIds = new Set<string>();
    for (const window of row.windows ?? []) {
      const id = analyticsWindowIdentity(window);
      currentIds.add(id);
      windows.set(id, window);
    }
    for (const [id, window] of windows) {
      const live = currentIds.has(id);
      const current = live ? rowReading(row, window) : null;
      const readings = mergeReadings(windowReadings(records, window, since), [
        ...(recent?.get(row, window) ?? []),
        ...(current ? [current] : []),
      ]).filter((reading) => reading.at >= since && reading.at <= now);
      if (
        live &&
        finite(window.usedPercent) !== null &&
        (window.kind ?? 'rate_limit') === 'rate_limit'
      )
        perWindow.set(
          burnKey(window.key, window.unit ?? null, window.poolId ?? null),
          burnRate(readings, window.resetAt, [
            ...storedBoundaries(records, window, since),
            ...(recent?.boundaries(row, window) ?? []),
          ])
        );
      // The published series holds measured numbers; reset-only readings only split periods.
      const measured = readings.filter(
        (reading) => reading.used !== null || reading.remaining !== null
      );
      if (measured.length === 0) continue;
      series.push({
        accountId: account.id,
        provider: account.provider,
        label: account.label,
        email: account.email,
        profile: account.profile,
        hidden: account.hidden,
        window: window.key,
        windowLabel: window.label,
        pool: window.poolLabel ?? null,
        kind: window.kind ?? 'rate_limit',
        unit: window.unit ?? null,
        points: measured.map((reading) => [
          minuteIso(reading.at),
          reading.used,
          reading.remaining,
          reading.resetAt === null ? null : new Date(reading.resetAt).toISOString(),
        ]),
      });
    }
    rates.set(account.id, perWindow);
  }
  return { rates, series };
}
