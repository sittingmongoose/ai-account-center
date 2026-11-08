/**
 * The store's share exports for agents (docs/store.md 8.3): `history-hourly.json` (90 days of
 * hourly rollups) and `history-daily.json` (every day kept), written once an hour beside
 * `usage.json`. Measured numbers only: a missing hour is absent, never zero.
 */
import { accountAnalyticsIdentity } from '../web-server/services/account-analytics-history';
import type { DashboardAccount } from '../web-server/services/account-dashboard-types';
import type { AgentUsageAccount } from '../web-server/agent-usage/agent-usage-projection';
import { CCSError } from '../errors/error-types';
import type { AacStore, StoreSeriesPoints } from './store-service';

export const SNAPSHOT_HISTORY_HOURLY = 'history-hourly.json';
export const SNAPSHOT_HISTORY_DAILY = 'history-daily.json';
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
const HOURLY_DAYS = 90;
const DAILY_DAYS = 3650;

export interface RollupExportFile {
  version: 1;
  generatedAt: string;
  resolution: 'hour' | 'day';
  /** Each point: [start, consumed points, last used %, highest used %, covered minutes, readings, resets, credits spent, credits added]. */
  columns: string[];
  series: Array<{
    accountId: string;
    provider: string;
    label: string | null;
    email: string | null;
    profile: string | null;
    hidden: boolean;
    window: string;
    windowLabel: string;
    pool: string | null;
    kind: string | null;
    unit: string | null;
    points: Array<Array<number | string | null>>;
  }>;
}

function toFile(
  points: StoreSeriesPoints,
  resolution: 'hour' | 'day',
  rows: DashboardAccount[],
  projected: AgentUsageAccount[],
  now: number
): RollupExportFile {
  const byIdentity = new Map<string, AgentUsageAccount>();
  for (const row of rows) {
    const account = projected.find((candidate) => candidate.id === row.id);
    if (account) byIdentity.set(accountAnalyticsIdentity(row), account);
  }
  return {
    version: 1,
    generatedAt: new Date(now).toISOString(),
    resolution,
    columns: [
      'start',
      'consumed',
      'lastUsed',
      'maxUsed',
      'coveredMinutes',
      'readings',
      'resets',
      'creditsSpent',
      'creditsAdded',
    ],
    series: points.series.map((series) => {
      const account = byIdentity.get(series.identity);
      return {
        accountId: series.accountId,
        provider: series.provider,
        label: account?.label ?? null,
        email: account?.email ?? null,
        profile: account?.profile ?? null,
        hidden: account?.hidden ?? false,
        window: series.window.key,
        windowLabel: series.window.label,
        pool: series.window.poolLabel,
        kind: series.window.kind,
        unit: series.window.unit,
        points: series.points.map(([start, ...rest]) => [
          new Date(Number(start)).toISOString(),
          ...(rest as Array<number | null>),
        ]),
      };
    }),
  };
}

export async function buildRollupExports(
  store: AacStore,
  rows: DashboardAccount[],
  projected: AgentUsageAccount[],
  now: number
): Promise<{ hourly: RollupExportFile; daily: RollupExportFile }> {
  // Enumerate metadata first so a capped point query cannot hide entire accounts.
  async function complete(resolution: 'hour' | 'day', days: number): Promise<StoreSeriesPoints> {
    const step = resolution === 'hour' ? HOUR_MS : DAY_MS;
    const fromMs = Math.floor((now - days * DAY_MS) / step) * step;
    const all: StoreSeriesPoints = { resolution, columns: [], series: [], truncated: false };
    const series = await store.seriesList({ fromMs, toMs: now });
    for (const meta of series) {
      const points: StoreSeriesPoints['series'][number]['points'] = [];
      async function page(from: number, to: number): Promise<void> {
        const result = await store.seriesPoints({
          identities: [meta.identity],
          windowKey: meta.window.key,
          fromMs: from,
          toMs: to,
          resolution,
        });
        if (result.truncated) {
          const buckets = Math.floor(to / step) - Math.floor(from / step) + 1;
          if (buckets <= 1)
            throw new CCSError('History export incomplete: a single bucket exceeds the query cap');
          const middle = (Math.floor(from / step) + Math.floor(buckets / 2)) * step;
          await page(from, middle - 1);
          await page(middle, to);
          return;
        }
        all.columns = result.columns;
        for (const item of result.series) {
          if (item.window.identity === meta.window.identity) points.push(...item.points);
        }
      }
      // Pages contain at most 1,000 buckets for the selected account/window.
      for (let from = fromMs; from <= now; from += 1000 * step) {
        await page(from, Math.min(now, from + 1000 * step - 1));
      }
      if (points.length) all.series.push({ ...meta, points });
    }
    return all;
  }
  const [hours, days] = await Promise.all([
    complete('hour', HOURLY_DAYS),
    complete('day', DAILY_DAYS),
  ]);
  return {
    hourly: toFile(hours, 'hour', rows, projected, now),
    daily: toFile(days, 'day', rows, projected, now),
  };
}

/** The hour a file set belongs to: exports are rewritten once per hour. */
export function exportHour(now: number): number {
  return Math.floor(now / HOUR_MS);
}
