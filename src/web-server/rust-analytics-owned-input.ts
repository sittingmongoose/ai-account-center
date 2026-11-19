/**
 * R5/A2: the owned Analytics envelope. Rust reads the quota history and the activity range from
 * the hosted store itself and composes the whole answer; this carries only what stays in TS until
 * H2 (the dashboard accounts, the live collector state, the resolved pricing facts and the proven
 * zone offsets) plus the bounds and provisional range the TS computation read, which Rust re-reads
 * and re-resolves and refuses on any disagreement. The presentation facts are empty: the owned
 * composer builds its own from the store's ISO strings and the CLDR root order.
 */
import type { AacStore } from '../store/store-service';
import { rustInputText } from './rust-read-input';
import { rustAnalyticsOwnedDashboard } from './rust-account-input';
import { defaultAccountAnalyticsPricing } from './services/account-analytics-pricing';
import type { AnalyticsProviderEntry } from './services/account-analytics-providers';
import type { ResolvedAccountAnalyticsRange } from './services/account-analytics-range';
import type {
  AccountAnalyticsActivityCoverage,
  AccountAnalyticsQuery,
  AccountAnalyticsSource,
} from './services/account-analytics-types';
import type { AnalyticsRemoteHost } from './services/analytics-remote-transport';
import type { DashboardAccount } from './services/account-dashboard-types';

const DAY = 86_400_000;
const STEP = 6 * 3_600_000;
/** The proven zone shape's segment bound (`aac_num::intl::Zone::proven`). */
const MAX_SEGMENTS = 64;
/** A pathological dictionary never builds an envelope over the input cap. */
const MAX_PRICING_PAIRS = 8_000;

/** The live collector state the owned read forwards (`storeActivityAnswer`'s `live`). */
export interface AnalyticsOwnedLive {
  sources: AccountAnalyticsSource[];
  refreshing: boolean;
  refreshingRemote: AnalyticsRemoteHost[];
}

/** The activity reader's part of the owned input: the store path, or the guarded selection. */
export type AnalyticsOwnedActivity =
  | { kind: 'store'; live: AnalyticsOwnedLive }
  | {
      kind: 'guarded';
      live: AnalyticsOwnedLive;
      /** The guarded selection's coverage of the published snapshot (the store path reads its own). */
      coverage: AccountAnalyticsActivityCoverage;
    };

/** Everything the TS computation knows before the owned read; Rust re-reads the store itself. */
export interface AnalyticsOwnedFacts {
  query: AccountAnalyticsQuery;
  scope: string;
  now: number;
  tz: string;
  available: boolean;
  bounds: { oldest: number } | null;
  provisional: ResolvedAccountAnalyticsRange;
  dashboard: { accounts: DashboardAccount[]; providers?: unknown; settings?: unknown };
  providerTable: AnalyticsProviderEntry[];
  activity: AnalyticsOwnedActivity;
}

/**
 * This engine's UTC offsets (minutes) for `timeZone` over the window, as
 * `{from, to, offsets: [[start, minutes], ...]}`: the server-side form of the page's
 * `homeZoneFacts`, bounded to the proven segment count. Null when the window holds more
 * transitions than the proof admits (the TS computation then answers).
 */
export function analyticsZoneFacts(
  timeZone: string,
  windowFrom: number,
  now: number
): { from: number; to: number; offsets: [number, number][] } | null {
  const from = Math.floor(windowFrom / DAY) * DAY - DAY;
  const to = Math.floor(now / DAY) * DAY + 2 * DAY;
  if (from < 0 || to <= from) return null;
  let parts: Intl.DateTimeFormat;
  try {
    parts = new Intl.DateTimeFormat('en-US', {
      timeZone,
      hourCycle: 'h23',
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    });
  } catch {
    return null;
  }
  const offset = (ms: number): number => {
    const p = Object.fromEntries(parts.formatToParts(ms).map(({ type, value }) => [type, value]));
    const local = Date.UTC(
      Number(p.year),
      Number(p.month) - 1,
      Number(p.day),
      Number(p.hour) % 24,
      Number(p.minute),
      Number(p.second)
    );
    return Math.round((local - Math.floor(ms / 1000) * 1000) / 60_000);
  };
  const offsets: [number, number][] = [[from, offset(from)]];
  for (let t = from; t < to;) {
    const current = offsets[offsets.length - 1][1];
    const next = Math.min(t + STEP, to);
    if (offset(next) === current) {
      t = next;
      continue;
    }
    // The first second in (t, next] whose offset is not the current one.
    let lo = t / 1000;
    let hi = next / 1000;
    while (hi - lo > 1) {
      const mid = Math.floor((lo + hi) / 2);
      if (offset(mid * 1000) === current) lo = mid;
      else hi = mid;
    }
    offsets.push([hi * 1000, offset(hi * 1000)]);
    if (offsets.length > MAX_SEGMENTS) return null;
    t = hi * 1000;
  }
  return { from, to, offsets };
}

/**
 * The resolved pricing facts over the store dictionary's model/route superset: every pair the
 * owned read can ask for, answered by the same lookup the TS projection uses. Null when the
 * dictionary is too large or the read fails.
 */
async function pricingFacts(store: AacStore): Promise<Record<string, unknown> | null> {
  let dictionary: { models: string[]; routes: string[] };
  try {
    dictionary = await store.activityModels();
  } catch {
    return null;
  }
  const routes = [...dictionary.routes, ''];
  if (dictionary.models.length * routes.length > MAX_PRICING_PAIRS) return null;
  const table: Record<string, unknown> = {};
  for (const model of dictionary.models)
    for (const route of routes) {
      table[`${model}\0${route}`] = defaultAccountAnalyticsPricing(model, route || undefined);
    }
  return table;
}

/**
 * The owned envelope, or null when JSON cannot carry the input exactly, the zone is not proven,
 * or the pricing superset is unavailable: the bridge then logs the fallback and the TS
 * computation answers.
 */
export async function rustAnalyticsOwnedInput(
  facts: AnalyticsOwnedFacts,
  store: AacStore
): Promise<unknown> {
  const { query, now, tz, bounds, provisional, activity } = facts;
  if (!bounds) return null;
  const dashboard = rustAnalyticsOwnedDashboard(facts.dashboard);
  if (!dashboard) return null;
  const zone =
    tz === 'UTC' ? null : analyticsZoneFacts(tz, Math.min(bounds.oldest, now - 31 * DAY), now);
  if (tz !== 'UTC' && !zone) return null;
  const pricing = await pricingFacts(store);
  if (!pricing) return null;
  const data = {
    owned: true,
    query,
    now,
    tz,
    available: facts.available,
    bounds,
    provisional,
    guardCoverage: activity.kind === 'guarded' ? (activity.coverage ?? null) : null,
    live: activity.live,
    zone,
    pricing,
    dashboard,
    providerTable: facts.providerTable,
  };
  try {
    return {
      data: JSON.parse(rustInputText(data)),
      presentation: { dates: {}, epochs: {}, order: {} },
    };
  } catch {
    return null;
  }
}

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

const OWNED_KEYS = [
  'schemaVersion',
  'updatedAt',
  'range',
  'filters',
  'history',
  'summary',
  'providers',
  'accounts',
  'activity',
];

/** The owned reply gate: the composed answer plus its activity block, in the composed key order. */
export function isRustAnalyticsOwned(value: unknown): boolean {
  return (
    object(value) &&
    Object.keys(value).join('\n') === OWNED_KEYS.join('\n') &&
    value.schemaVersion === 1 &&
    typeof value.updatedAt === 'string' &&
    object(value.range) &&
    object(value.filters) &&
    object(value.history) &&
    object(value.summary) &&
    Array.isArray(value.providers) &&
    Array.isArray(value.accounts) &&
    object(value.activity)
  );
}
