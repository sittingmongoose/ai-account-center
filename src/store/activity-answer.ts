/**
 * Analytics usage history (docs/store.md section 12): the page's activity read back from the store
 * and shaped by the existing projection. Bucketing, coverage and gaps are computed in Rust
 * (`native/aac-store/src/activity.rs`); this only forwards.
 */
import type { AacStore } from './store-service';
import type { ActivityRange } from './usage-report';
import type { HourlyUsage, ModelBreakdown, SessionUsage } from '../web-server/usage/types';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsActivityProvider,
  AccountAnalyticsQuery,
  AccountAnalyticsSource,
  AccountAnalyticsUsageProvider,
} from '../web-server/services/account-analytics-types';
import {
  projectAccountAnalyticsActivity,
  type SourceData,
} from '../web-server/services/account-analytics-projection';
import { usageProviderFor } from '../web-server/services/account-analytics-attribution';
import type { AccountAnalyticsPricingLookup } from '../web-server/services/account-analytics-pricing';
import { getModelPricingWithSource } from '../web-server/model-pricing';
import {
  addCalendarDays,
  localDate,
  localMidnight,
} from '../web-server/services/account-analytics-range';
import type { DashboardProvider } from '../web-server/services/account-dashboard-types';
import { coverageOf, type StoreActivityCoverage } from './activity-coverage';

const HOUR = 3_600_000;
const DAY = 86_400_000;
/** Ranges up to this long are answered hourly; longer ones in local days. */
export const HOURLY_RANGE_MS = 92 * DAY;

const TOOLS: readonly AccountAnalyticsActivityProvider[] = [
  'claude',
  'codex',
  'omp',
  'muse',
  'zcode',
  'jsonl',
  'antigravity',
];

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

function hourOf(text: string): number {
  return Date.parse(`${text.replace(' ', 'T')}:00Z`);
}

/** The local midnights from the day of `from` through the one after `to`, as UTC instants. */
export function localDayStarts(from: number, to: number, tz: string): number[] {
  const starts: number[] = [];
  let date = localDate(from, tz);
  const last = localDate(to, tz);
  for (let guard = 0; guard < 20_000; guard++) {
    starts.push(localMidnight(date, tz));
    if (date > last) break;
    date = addCalendarDays(date, 1);
  }
  return starts;
}

function hourText(ms: number): string {
  return new Date(ms).toISOString().slice(0, 13).replace('T', ' ') + ':00';
}

/** A cost the store has no estimate for (a reader that logs only its own costs): priced here. */
function listed(
  route: string,
  model: string,
  tokens: [number, number, number, number]
): { cost: number; fallback: number } {
  const resolved = getModelPricingWithSource(model, { provider: route || undefined });
  const r = resolved.pricing;
  const cost =
    (tokens[0] / 1e6) * r.inputPerMillion +
    (tokens[1] / 1e6) * r.outputPerMillion +
    (tokens[2] / 1e6) * r.cacheCreationPerMillion +
    (tokens[3] / 1e6) * r.cacheReadPerMillion;
  return { cost, fallback: resolved.source === 'fallback' ? cost : 0 };
}

/** The store's bucketed rows as the per-tool sources the projection reads (one row per bucket). */
export function rangeToSources(
  range: ActivityRange,
  stamp: (bucket: number) => number,
  fetchedAt: string
): SourceData[] {
  const byTool = new Map<string, Map<number, HourlyUsage>>();
  const knownCounts = new Map<string, boolean>();
  for (const row of range.rows) {
    const [bucket, , ti, ri, mi, i, o, cw, cr, loggedNano, costNano, fallbackNano, events] = row;
    // flawed counts (the retired Codex cache's output): in the totals, out of model comparisons
    const quality = typeof row[13] === 'number' ? row[13] : 0;
    const tool = range.tools[ti];
    const route = range.routes[ri] ?? '';
    const model = range.models[mi] ?? 'unknown';
    let cost: number;
    let fallback: number;
    if (costNano === null) {
      // the reader logged only its own cost: the rest at list price, priced now
      const priced = listed(route, model, [i, o, cw, cr]);
      const logged = (loggedNano ?? 0) / 1e9;
      cost = logged > 0 ? logged : priced.cost;
      fallback = logged > 0 ? 0 : priced.fallback;
    } else {
      cost = costNano / 1e9;
      fallback = (fallbackNano ?? 0) / 1e9;
    }
    const at = stamp(bucket);
    const hours = byTool.get(tool) ?? new Map<number, HourlyUsage>();
    byTool.set(tool, hours);
    const hour = hours.get(at) ?? {
      hour: hourText(at),
      source: `${tool}-store`,
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      cost: 0,
      totalCost: 0,
      modelsUsed: [],
      modelBreakdowns: [],
      requestCount: 0,
    };
    hours.set(at, hour);
    const breakdown: ModelBreakdown = {
      modelName: model,
      ...(route ? { provider: route } : {}),
      inputTokens: i,
      outputTokens: o,
      cacheCreationTokens: cw,
      cacheReadTokens: cr,
      cost,
      ...(fallback > 0 ? { fallbackCost: fallback } : {}),
      ...(events !== null ? { requestCount: events } : {}),
      ...(quality ? { quality } : {}),
    };
    // one model may come in several rows (one per computer): fold it, flawed counts apart
    const prior = hour.modelBreakdowns.find(
      (b) => b.modelName === model && (b.provider ?? '') === route && (b.quality ?? 0) === quality
    );
    if (prior) {
      prior.inputTokens += i;
      prior.outputTokens += o;
      prior.cacheCreationTokens += cw;
      prior.cacheReadTokens += cr;
      prior.cost += cost;
      if (fallback > 0) prior.fallbackCost = (prior.fallbackCost ?? 0) + fallback;
      if (events === null || prior.requestCount === undefined) delete prior.requestCount;
      else prior.requestCount += events;
    } else hour.modelBreakdowns.push(breakdown);
    if (!hour.modelsUsed.includes(model)) hour.modelsUsed.push(model);
    hour.inputTokens += i;
    hour.outputTokens += o;
    hour.cacheCreationTokens += cw;
    hour.cacheReadTokens += cr;
    hour.cost += cost;
    hour.totalCost += cost;
    if (fallback > 0) hour.fallbackCost = (hour.fallbackCost ?? 0) + fallback;
    const countKey = `${tool}\0${at}`;
    if (events === null) knownCounts.set(countKey, false);
    else if (knownCounts.get(countKey) !== false) {
      knownCounts.set(countKey, true);
      hour.requestCount = (hour.requestCount ?? 0) + events;
    }
  }
  const sessionsByTool = new Map<string, SessionUsage[]>();
  for (const s of range.sessions.sample) {
    if (!s.sessionKey) continue;
    const parts = s.parts.map((p) => ({
      modelName: p.modelRaw,
      ...(p.route ? { provider: p.route } : {}),
      inputTokens: p.inputTokens,
      outputTokens: p.outputTokens,
      cacheCreationTokens: p.cacheCreationTokens,
      cacheReadTokens: p.cacheReadTokens,
      cost: p.costUsd ?? p.loggedCostUsd ?? 0,
      ...(p.fallbackCostUsd ? { fallbackCost: p.fallbackCostUsd } : {}),
    }));
    const sum = (
      f: 'inputTokens' | 'outputTokens' | 'cacheCreationTokens' | 'cacheReadTokens' | 'cost'
    ) => parts.reduce((n, p) => n + p[f], 0);
    const list = sessionsByTool.get(s.tool) ?? [];
    list.push({
      sessionId: s.sessionKey,
      projectPath: '',
      source: `${s.tool}-store`,
      inputTokens: sum('inputTokens'),
      outputTokens: sum('outputTokens'),
      cacheCreationTokens: sum('cacheCreationTokens'),
      cacheReadTokens: sum('cacheReadTokens'),
      cost: sum('cost'),
      totalCost: sum('cost'),
      firstActivity: iso(s.firstMs),
      lastActivity: iso(s.lastMs),
      versions: [],
      modelsUsed: [...new Set(parts.map((p) => p.modelName))],
      modelBreakdowns: parts,
      ...(s.target ? { target: s.target } : {}),
    });
    sessionsByTool.set(s.tool, list);
  }
  const sources: SourceData[] = [];
  for (const tool of TOOLS) {
    const hours = byTool.get(tool);
    const sessions = sessionsByTool.get(tool) ?? [];
    if (!hours && !sessions.length) continue;
    const hourly = [...(hours?.values() ?? [])];
    for (const hour of hourly)
      if (knownCounts.get(`${tool}\0${hourOf(hour.hour)}`) === false) delete hour.requestCount;
    sources.push({
      provider: tool,
      data: [
        {
          daily: [],
          monthly: [],
          hourly,
          session: sessions,
          eventCount: hourly.reduce((n, h) => n + (h.requestCount ?? 0), 0),
        },
      ],
      fetchedAt,
    });
  }
  return sources;
}

/** Per-provider session counts from the store's signatures (a session counts under each provider that served it). */
export function sessionCounts(range: ActivityRange): Map<AccountAnalyticsUsageProvider, number> {
  const counts = new Map<AccountAnalyticsUsageProvider, number>();
  for (const sig of range.sessions.signatures) {
    const tool = sig.tool as AccountAnalyticsActivityProvider;
    const providers = new Set(
      sig.pairs.map(([route, model]) => usageProviderFor(tool, route, model))
    );
    if (!providers.size) providers.add(usageProviderFor(tool, null, null));
    for (const p of providers) counts.set(p, (counts.get(p) ?? 0) + sig.count);
  }
  return counts;
}

/**
 * The page's activity, from the store only: never waits on a collection. `live` carries what the
 * collector knows right now (whether a pass runs, the source grid, pricing).
 */
export async function storeActivityAnswer(
  store: AacStore,
  query: AccountAnalyticsQuery,
  from: number,
  to: number,
  live: {
    tz: string;
    pricing?: AccountAnalyticsPricingLookup;
    sources: AccountAnalyticsSource[];
    refreshing: boolean;
    refreshingRemote: AccountAnalyticsActivity['refreshingRemote'];
  }
): Promise<{
  activity: AccountAnalyticsActivity & {
    history: StoreActivityCoverage;
    resolution: 'hour' | 'day';
  };
  oldestHourAt: number | null;
  providersWithActivity: DashboardProvider[];
}> {
  const daily = to - from > HOURLY_RANGE_MS;
  const dayStarts = daily ? localDayStarts(from, to, live.tz) : [];
  const [range, sourcesState] = await Promise.all([
    store.activityRange({
      fromMs: from,
      toMs: to,
      resolution: daily ? 'day' : 'hour',
      ...(daily ? { dayStarts } : {}),
      sessionSample: 200,
    }),
    store.activitySources({ fromMs: from, toMs: to }),
  ]);
  const coverage = coverageOf(sourcesState);
  const lastRead = sourcesState.computers.reduce((max, c) => Math.max(max, c.lastReadMs ?? 0), 0);
  const fetchedAt = iso(lastRead || Date.now());
  // A day bucket is labelled by the first whole hour of its local day, so the projection files it
  // under that day in the page's zone.
  const stamp = (bucket: number) => (daily ? Math.ceil(bucket / HOUR) * HOUR : bucket);
  const sources = rangeToSources(range, stamp, fetchedAt);
  const stored = sourcesState.computers.some((c) => c.oldestMs !== null || c.lastReadMs !== null);
  const status: AccountAnalyticsActivity['status'] = sources.length
    ? live.refreshing
      ? 'cached'
      : 'ok'
    : stored
      ? 'ok'
      : live.refreshing
        ? 'loading'
        : 'unavailable';
  const projectFrom = daily ? Math.min(from, dayStarts[0] ?? from) : from;
  const projectTo = daily ? Math.max(to, stamp(dayStarts[dayStarts.length - 2] ?? to)) : to;
  const message =
    status === 'loading'
      ? 'Usage history is being read for the first time. Quota history is available immediately.'
      : `CLI activity kept in AAC's history (${daily ? 'daily buckets in ' + live.tz : 'UTC hourly buckets; days are grouped in ' + live.tz}). Cost is an API-equivalent estimate, not a subscription charge.`;
  const projected = projectAccountAnalyticsActivity(
    sources,
    query,
    projectFrom,
    projectTo,
    status,
    message,
    {
      tz: live.tz,
      ...(live.pricing ? { pricing: live.pricing } : {}),
      sources: live.sources,
      refreshing: live.refreshing,
      refreshingRemote: live.refreshingRemote,
    }
  );
  // Session counts cover every session in the range, not only the sample the projection saw.
  const counts = sessionCounts(range);
  for (const p of projected.providers) p.sessionCount = counts.get(p.provider) ?? 0;
  if (projected.sessions)
    projected.sessions = {
      ...projected.sessions,
      total:
        query.provider === 'all'
          ? range.sessions.total
          : (counts.get(query.provider as AccountAnalyticsUsageProvider) ?? 0),
      truncated: range.sessions.truncated || projected.sessions.truncated,
    };
  const oldest = sourcesState.computers.reduce<number | null>(
    (min, c) => (c.oldestMs === null ? min : min === null ? c.oldestMs : Math.min(min, c.oldestMs)),
    null
  );
  // usage in the range whose counts have a known flaw, for the page's note
  const quality = (range.quality ?? []).map((q) => ({
    tool: q.tool,
    flags: [...q.flags],
    from: iso(q.fromMs),
    to: iso(q.toMs),
  }));
  return {
    activity: {
      ...projected,
      history: { ...coverage, quality },
      resolution: daily ? 'day' : 'hour',
    },
    oldestHourAt: oldest,
    providersWithActivity: projected.providers
      .map((p) => p.provider)
      .filter((p): p is DashboardProvider => p !== 'other'),
  };
}
