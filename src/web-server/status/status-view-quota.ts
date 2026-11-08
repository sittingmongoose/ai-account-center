/**
 * The Status view's `spend` section and its `quota-watch` source: quota-watch's per-stream
 * estimates, budgets, unattributed use and unscanned machines, worded for the page. Every value
 * here is an estimate (labelled so); credits and percentage points are never added together and no
 * total is made across units; an unknown stays null and says "not read", never 0.
 */
import type { QuotaPeriodId, QuotaReportState, QuotaUnit } from './quota-report';
import type { Source } from './status-view-types';
import { ageSeconds, clockText, iso, plural } from './status-view-words';

/** Three missed 10-minute passes. */
export const QUOTA_STALE_AFTER_SECONDS = 1800;

export interface SpendRowView {
  account: string;
  meter: string;
  meterLabel: string;
  unit: QuotaUnit;
  /** Rounded: credits to 0.01, percentage points to 0.1; null when not read. */
  value: number | null;
  valueText: string;
  estimate: boolean;
}

export interface SpendStreamView {
  stream: string;
  /** The stream's label from the agent reports, or the id; buckets such as "(no stream)" as written. */
  label: string;
  bucket: boolean;
  rows: SpendRowView[];
}

export interface SpendPeriodView {
  id: QuotaPeriodId;
  label: string;
  period: string | null;
  start: string | null;
  end: string | null;
  coverage: number | null;
  /** Set when coverage is under half (or unknown): "covers 9% of the week". */
  coverageText: string | null;
  streams: SpendStreamView[];
}

export interface SpendBudgetView {
  stream: string;
  streamLabel: string;
  desc: string | null;
  accounts: string[];
  unit: QuotaUnit;
  meter: string;
  meterLabel: string;
  per: string | null;
  limit: number | null;
  use: number | null;
  known: boolean;
  percent: number | null;
  period: string | null;
  coverage: number | null;
  coverageText: string | null;
  /** At or over its limit (only when both the limit and the use are known). */
  overLimit: boolean;
  estimate: boolean;
  text: string;
}

export interface SpendUnattributedView {
  account: string;
  meter: string;
  meterLabel: string;
  unit: QuotaUnit | null;
  amount: number | null;
  amountText: string;
  reason: string;
  why: string | null;
  estimate: boolean;
}

export interface SpendView {
  source: 'quota-watch';
  /** Every number in this section is an estimate unless a row says `estimate: false`. */
  estimate: true;
  asOf: string | null;
  generatedAt: string | null;
  writer: string | null;
  methodId: string | null;
  /** The report's method in words. */
  method: string | null;
  periods: SpendPeriodView[];
  budgets: SpendBudgetView[];
  unattributed: SpendUnattributedView[];
  machinesNotScanned: Array<{
    name: string;
    thisPass: boolean | null;
    error: string | null;
    runsMissed: number | null;
    runsInWindow: number | null;
  }>;
  overLimitBudgets: number;
}

const METER_LABELS: Record<string, string> = {
  five_hour: '5-hour window',
  seven_day: '7-day window',
  seven_day_opus: '7-day Opus window',
  seven_day_sonnet: '7-day Sonnet window',
  credits_balance: 'Credits',
};

export function meterLabel(meter: string): string {
  return METER_LABELS[meter] ?? meter.replace(/_/g, ' ');
}

function rounded(value: number | null, unit: QuotaUnit | null): number | null {
  if (value === null) return null;
  const scale = unit === 'percent' ? 10 : 100;
  return Math.round(value * scale) / scale;
}

/** "≈ 17.77 credits", "≈ 4.7 points of the 7-day window", "not read". */
export function amountText(value: number | null, unit: QuotaUnit | null, meter: string): string {
  if (value === null || unit === null) return 'not read';
  const r = rounded(value, unit) ?? 0;
  const tiny = value > 0 && r === 0;
  if (unit === 'credits') return tiny ? '≈ under 0.01 credits' : `≈ ${r.toFixed(2)} credits`;
  const points = tiny ? 'under 0.1 points' : `${r.toFixed(1)} ${r === 1 ? 'point' : 'points'}`;
  return `≈ ${points} of the ${meterLabel(meter)}`;
}

const PERIOD_NOUNS: Record<QuotaPeriodId, string> = { day: 'day', week: 'week', window: 'window' };

function coverageText(coverage: number | null, noun: string): string | null {
  if (coverage === null) return `coverage of the ${noun} not known`;
  if (coverage >= 0.5) return null;
  return `covers ${Math.round(coverage * 100)}% of the ${noun}`;
}

function periodLabel(id: QuotaPeriodId, start: string | null, end: string | null): string {
  if (id === 'day') return 'Today';
  if (id === 'week') return 'This week';
  const ms = start && end ? Date.parse(end) - Date.parse(start) : NaN;
  const hours = Number.isFinite(ms) ? Math.round(ms / 3_600_000) : null;
  return hours ? `Last ${plural(hours, 'hour')}` : 'Window';
}

function nounOfPeriod(period: string | null, per: string | null): string {
  const value = per ?? period ?? '';
  if (value.startsWith('day')) return 'day';
  if (value.startsWith('week')) return 'week';
  return 'window';
}

/** A bucket rather than a stream id: "(no stream)", "(session not in T3)" and the like. */
function isBucket(stream: string): boolean {
  return stream.startsWith('(');
}

function streamLabel(stream: string, labels: ReadonlyMap<string, string>): string {
  return isBucket(stream) ? stream : (labels.get(stream) ?? stream);
}

const PER_WORDS: Record<string, string> = {
  day: 'today',
  week: 'this week',
  window: 'in the window',
};

function number(value: number, unit: QuotaUnit): string {
  return unit === 'credits' ? value.toFixed(2) : value.toFixed(1);
}

/** "≈ 18.28 credits today; no limit set.", "at least ≈ 26.1 of 25 points of the 5-hour window in the window (104%)." */
function budgetText(b: {
  unit: QuotaUnit;
  meter: string;
  limit: number | null;
  use: number | null;
  known: boolean;
  percent: number | null;
  per: string | null;
}): string {
  if (b.use === null) return 'No reading yet.';
  const lower = b.known ? '' : 'at least ';
  const per = b.per ? ` ${PER_WORDS[b.per] ?? `per ${b.per}`}` : '';
  if (b.limit === null) return `${lower}${amountText(b.use, b.unit, b.meter)}${per}; no limit set.`;
  const what = b.unit === 'credits' ? 'credits' : `points of the ${meterLabel(b.meter)}`;
  const pct = b.percent === null ? '' : ` (${Math.round(b.percent)}%)`;
  return `${lower}≈ ${number(b.use, b.unit)} of ${number(b.limit, b.unit)} ${what}${per}${pct}.`;
}

export function emptySpend(): SpendView {
  return {
    source: 'quota-watch',
    estimate: true,
    asOf: null,
    generatedAt: null,
    writer: null,
    methodId: null,
    method: null,
    periods: [],
    budgets: [],
    unattributed: [],
    machinesNotScanned: [],
    overLimitBudgets: 0,
  };
}

/** When AAC can trust the report was written: the older of its own time and first sight. */
function seenAt(good: NonNullable<QuotaReportState['good']>): number {
  return Number.isFinite(good.writtenAtMs)
    ? Math.min(good.writtenAtMs, good.firstSeenAt)
    : good.firstSeenAt;
}

export function quotaSource(
  quota: QuotaReportState | undefined,
  now: number,
  timeZone: string
): Source {
  const base = { id: 'quota-watch' as const, label: 'Quota watch (estimates)' };
  if (!quota)
    return {
      ...base,
      state: 'unconfigured',
      asOf: null,
      ageSeconds: null,
      staleAfterSeconds: null,
      detail: 'Per-stream quota attribution is not connected on this server.',
    };
  const at = quota.good ? seenAt(quota.good) : null;
  const timing = {
    asOf: iso(at),
    ageSeconds: ageSeconds(now, at),
    staleAfterSeconds: QUOTA_STALE_AFTER_SECONDS,
  };
  const showing = at !== null ? ` Showing the report from ${clockText(at, timeZone)}.` : '';
  if (quota.scannedAt === null)
    return { ...base, ...timing, state: 'missing', detail: 'Waiting for the first scan.' };
  if (quota.state === 'error')
    return {
      ...base,
      ...timing,
      state: 'error',
      detail: `The quota folder could not be listed.${showing}`,
    };
  if (quota.problem)
    return {
      ...base,
      ...timing,
      state: 'unreadable',
      detail: `quota/report.json unreadable since ${clockText(quota.problem.since, timeZone)} (${quota.problem.reason}).${showing}`,
    };
  if (!quota.good)
    return {
      ...base,
      ...timing,
      state: 'missing',
      detail: 'No quota-watch report at v1/quota/report.json.',
    };
  if ((timing.ageSeconds ?? 0) > QUOTA_STALE_AFTER_SECONDS)
    return {
      ...base,
      ...timing,
      state: 'stale',
      detail: `No new quota report since ${clockText(at ?? now, timeZone)} (quota-watch writes every 10 minutes).`,
    };
  return {
    ...base,
    ...timing,
    state: 'ok',
    detail: `Report from ${clockText(at ?? now, timeZone)}. Every number is an estimate.`,
  };
}

export function buildSpend(
  quota: QuotaReportState | undefined,
  labels: ReadonlyMap<string, string>
): SpendView {
  const good = quota?.good;
  if (!good) return emptySpend();
  const { doc } = good;
  const periods: SpendPeriodView[] = [];
  for (const id of ['day', 'week', 'window'] as const) {
    const p = doc.periods[id];
    if (!p) continue;
    const streams = new Map<string, SpendStreamView>();
    for (const row of p.rows) {
      const stream = streams.get(row.stream) ?? {
        stream: row.stream,
        label: streamLabel(row.stream, labels),
        bucket: isBucket(row.stream),
        rows: [],
      };
      stream.rows.push({
        account: row.account,
        meter: row.meter,
        meterLabel: meterLabel(row.meter),
        unit: row.unit,
        value: rounded(row.value, row.unit),
        valueText: amountText(row.value, row.unit, row.meter),
        estimate: row.estimate,
      });
      streams.set(row.stream, stream);
    }
    periods.push({
      id,
      label: periodLabel(id, p.start, p.end),
      period: p.period,
      start: p.start,
      end: p.end,
      coverage: p.coverage,
      coverageText: coverageText(p.coverage, PERIOD_NOUNS[id]),
      streams: [...streams.values()].sort(
        (a, b) => Number(a.bucket) - Number(b.bucket) || a.label.localeCompare(b.label)
      ),
    });
  }
  const budgets = doc.budgets.map((b): SpendBudgetView => {
    const overLimit =
      (b.percent !== null && b.percent >= 100) ||
      (b.limit !== null && b.use !== null && b.use >= b.limit);
    return {
      stream: b.stream,
      streamLabel: streamLabel(b.stream, labels),
      desc: b.desc,
      accounts: b.accounts,
      unit: b.unit,
      meter: b.meter,
      meterLabel: meterLabel(b.meter),
      per: b.per,
      limit: b.limit,
      use: b.use,
      known: b.known,
      percent: b.percent,
      period: b.period,
      coverage: b.coverage,
      coverageText: coverageText(b.coverage, nounOfPeriod(b.period, b.per)),
      overLimit,
      estimate: b.estimate,
      text: budgetText(b),
    };
  });
  return {
    source: 'quota-watch',
    estimate: true,
    asOf: iso(seenAt(good)),
    generatedAt: doc.generatedAt,
    writer: doc.writer,
    methodId: doc.methodId,
    method: doc.method,
    periods,
    budgets,
    unattributed: doc.unattributed.map((u) => ({
      account: u.account,
      meter: u.meter,
      meterLabel: meterLabel(u.meter),
      unit: u.unit,
      amount: rounded(u.amount, u.unit),
      amountText: amountText(u.amount, u.unit, u.meter),
      reason: u.reason,
      why: u.why,
      estimate: u.estimate,
    })),
    machinesNotScanned: doc.machinesNotScanned,
    overLimitBudgets: budgets.filter((b) => b.overLimit).length,
  };
}
