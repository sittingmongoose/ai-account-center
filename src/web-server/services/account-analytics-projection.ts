import { createHash } from 'crypto';
import { hasPartialHourOffset, localDate } from './account-analytics-range';
import { detectAccountAnalyticsAnomalies } from './account-analytics-anomalies';
import {
  defaultAccountAnalyticsPricing,
  memoiseAccountAnalyticsPricing,
  type AccountAnalyticsPricingLookup,
} from './account-analytics-pricing';
import type { UsageWorkerResult } from '../usage/worker-client';
import type { ModelBreakdown, SessionUsage } from '../usage/types';
import { attributeActivitySources } from './account-analytics-attribution';
import { ADDITIONAL_PROVIDERS } from './account-dashboard-projection';
import type { DashboardProvider } from './account-dashboard-types';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsActivityCoverage,
  AccountAnalyticsActivityProvider,
  AccountAnalyticsActivityTotals,
  AccountAnalyticsModelRates,
  AccountAnalyticsQuery,
  AccountAnalyticsUsageProvider,
} from './account-analytics-types';

const TOOLS: readonly AccountAnalyticsActivityProvider[] = [
  'claude',
  'codex',
  'omp',
  'muse',
  'zcode',
];

/** The dashboard's provider labels; "other" is usage on a route no provider claims. */
const USAGE_PROVIDER_LABELS: Readonly<Record<string, string>> = Object.freeze(
  Object.fromEntries([
    ['claude', 'Claude'],
    ['codex', 'Codex'],
    ...ADDITIONAL_PROVIDERS,
    ['other', 'Other'],
  ])
);

/**
 * Pure projection of the retained local CLI snapshot into the Analytics
 * activity block: totals, per-type cost estimates, local-day buckets, the
 * per-day model view, a path-free session sample and the original CCS
 * anomaly rules. Nothing here reads disk or attributes activity to accounts.
 */

/** One CLI source as the activity reader retains it (all hosts merged); the projection regroups it by provider. */
export interface SourceData {
  provider: AccountAnalyticsActivityProvider;
  data: UsageWorkerResult[];
  fetchedAt: string;
}

const FIELDS = [
  'inputTokens',
  'outputTokens',
  'cacheCreationTokens',
  'cacheReadTokens',
  'estimatedCostUsd',
  'fallbackCostUsd',
] as const;

type Provider = AccountAnalyticsUsageProvider;
type TotalField = (typeof FIELDS)[number];
type TokenField = Exclude<TotalField, 'estimatedCostUsd' | 'fallbackCostUsd'>;
const TOKEN_FIELDS: readonly TokenField[] = [
  'inputTokens',
  'outputTokens',
  'cacheCreationTokens',
  'cacheReadTokens',
];
const PART_FIELDS = ['input', 'output', 'cacheWrite', 'cacheRead'] as const;
type PartField = (typeof PART_FIELDS)[number];
const RATE_KEYS = [
  'inputPerMillion',
  'outputPerMillion',
  'cacheCreationPerMillion',
  'cacheReadPerMillion',
  'source',
] as const;
const KNOWN_CLI_TARGETS: ReadonlySet<string> = new Set([
  'claude',
  'codex',
  'droid',
  'omp',
  'muse',
  'zcode',
]);
const MAX_SESSION_SAMPLE = 50;
const MAX_NAMED_DAY_MODELS = 12;
/**
 * Every model with usage in the range is published, so the page can list each one. The bound only keeps a
 * pathological snapshot finite; ranking is by estimated cost, then by tokens, so a model whose cost is logged
 * as zero or not logged at all is never dropped ahead of a costlier one with fewer tokens.
 */
const MAX_PUBLISHED_MODELS = 500;
const OTHER_MODELS = 'Other models';

export interface AccountAnalyticsProjectionOptions {
  /** IANA zone for day buckets; UTC when absent. */
  tz?: string;
  /** Pricing lookup; memoised per model for one projection. */
  pricing?: AccountAnalyticsPricingLookup;
  /** Per-tool, per-host collection states; empty when unknown. */
  sources?: AccountAnalyticsActivity['sources'];
}

function finite(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0;
}
function validModelName(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 160 && !/[\u0000-\u001f\u007f]/.test(value);
}
function totals(value: {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  cost?: number;
  totalCost?: number;
  fallbackCost?: number;
}): Record<TotalField, number> {
  return {
    inputTokens: finite(value.inputTokens),
    outputTokens: finite(value.outputTokens),
    cacheCreationTokens: finite(value.cacheCreationTokens),
    cacheReadTokens: finite(value.cacheReadTokens),
    estimatedCostUsd: finite(value.totalCost ?? value.cost),
    fallbackCostUsd: finite(value.fallbackCost),
  };
}

/** Totals plus per-type cost parts; parts become unknown once any contribution has no rate. */
interface Accumulator extends Record<TotalField, number>, Record<PartField, number> {
  partsKnown: boolean;
}
function accumulator(): Accumulator {
  return {
    inputTokens: 0,
    outputTokens: 0,
    cacheCreationTokens: 0,
    cacheReadTokens: 0,
    estimatedCostUsd: 0,
    fallbackCostUsd: 0,
    input: 0,
    output: 0,
    cacheWrite: 0,
    cacheRead: 0,
    partsKnown: true,
  };
}
function addValues(target: Accumulator, values: Record<TotalField, number>): void {
  for (const field of FIELDS) {
    const next = target[field] + values[field];
    if (Number.isFinite(next)) target[field] = next;
  }
}
function addParts(target: Accumulator, parts: Record<PartField, number> | null): void {
  if (!parts) {
    target.partsKnown = false;
    return;
  }
  for (const field of PART_FIELDS) {
    const next = target[field] + parts[field];
    if (Number.isFinite(next)) target[field] = next;
    else target.partsKnown = false;
  }
}
function merge(target: Accumulator, source: Accumulator): void {
  addValues(target, source);
  addParts(target, source.partsKnown ? source : null);
}
function publish(value: Accumulator): AccountAnalyticsActivityTotals {
  const base = {
    inputTokens: value.inputTokens,
    outputTokens: value.outputTokens,
    cacheCreationTokens: value.cacheCreationTokens,
    cacheReadTokens: value.cacheReadTokens,
    estimatedCostUsd: value.estimatedCostUsd,
    fallbackCostUsd: nanoDollars(value.fallbackCostUsd),
  };
  if (!value.partsKnown) return { ...base, costByType: null, costByTypeReconciled: false };
  const sum = value.input + value.output + value.cacheWrite + value.cacheRead;
  return {
    ...base,
    // Nano-dollar rounding only trims float noise from the JSON; it never rescales.
    costByType: {
      input: nanoDollars(value.input),
      output: nanoDollars(value.output),
      cacheWrite: nanoDollars(value.cacheWrite),
      cacheRead: nanoDollars(value.cacheRead),
    },
    costByTypeReconciled:
      Math.abs(sum - base.estimatedCostUsd) <= Math.max(0.01, 0.005 * base.estimatedCostUsd),
  };
}
function nanoDollars(value: number): number {
  return Math.round(value * 1e9) / 1e9;
}
function tokenTotal(value: Record<TokenField, number>): number {
  return TOKEN_FIELDS.reduce((sum, field) => sum + value[field], 0);
}

interface PricedBreakdowns {
  /** Parts for everything the breakdowns cover, or null when a model has no rate. */
  parts: Record<PartField, number> | null;
  /** Token and cost amounts in the row that no breakdown covers. */
  residual: Record<TotalField, number> | null;
  models: Array<{
    name: string | null;
    values: Record<TotalField, number>;
    parts: Record<PartField, number> | null;
    rates: AccountAnalyticsModelRates | null;
  }>;
}

/** Price each model breakdown of a worker row at its list rates. */
function priceBreakdowns(
  row: { modelBreakdowns?: unknown } & Parameters<typeof totals>[0],
  pricing: AccountAnalyticsPricingLookup
): PricedBreakdowns {
  const rowValues = totals(row);
  const covered = accumulator();
  const sum: Record<PartField, number> = { input: 0, output: 0, cacheWrite: 0, cacheRead: 0 };
  let known = true;
  const models: PricedBreakdowns['models'] = [];
  const breakdowns = Array.isArray(row.modelBreakdowns) ? row.modelBreakdowns : [];
  for (const breakdown of breakdowns as ModelBreakdown[]) {
    if (typeof breakdown !== 'object' || breakdown === null) continue;
    const values = totals(breakdown);
    addValues(covered, values);
    const name = validModelName(breakdown.modelName) ? breakdown.modelName : null;
    const rates =
      name === null
        ? null
        : pricing(name, typeof breakdown.provider === 'string' ? breakdown.provider : undefined);
    // The unknown-model fallback is published for the popover but is no known rate.
    const listRate = rates?.source === 'fallback' ? null : rates;
    const parts = listRate
      ? {
          input: (values.inputTokens / 1_000_000) * listRate.inputPerMillion,
          output: (values.outputTokens / 1_000_000) * listRate.outputPerMillion,
          cacheWrite: (values.cacheCreationTokens / 1_000_000) * listRate.cacheCreationPerMillion,
          cacheRead: (values.cacheReadTokens / 1_000_000) * listRate.cacheReadPerMillion,
        }
      : null;
    models.push({ name, values, parts, rates });
    if (!parts) known = false;
    else for (const field of PART_FIELDS) sum[field] += parts[field];
  }
  // Tokens that no model breakdown names cannot be priced. Rounding in the
  // worker's cost alone is not a residual.
  const uncovered = TOKEN_FIELDS.some((field) => rowValues[field] > covered[field]);
  return {
    parts: known && !uncovered ? sum : null,
    residual: uncovered
      ? {
          inputTokens: Math.max(0, rowValues.inputTokens - covered.inputTokens),
          outputTokens: Math.max(0, rowValues.outputTokens - covered.outputTokens),
          cacheCreationTokens: Math.max(
            0,
            rowValues.cacheCreationTokens - covered.cacheCreationTokens
          ),
          cacheReadTokens: Math.max(0, rowValues.cacheReadTokens - covered.cacheReadTokens),
          estimatedCostUsd: Math.max(0, rowValues.estimatedCostUsd - covered.estimatedCostUsd),
          fallbackCostUsd: Math.max(0, rowValues.fallbackCostUsd - covered.fallbackCostUsd),
        }
      : null,
    models,
  };
}

function sessionKey(tool: AccountAnalyticsActivityProvider, sessionId: string): string {
  return createHash('sha256')
    .update(`aac-session-v1:${tool}:${sessionId}`)
    .digest('hex')
    .slice(0, 16);
}

/** Compacted rows keep each hour's last event, so first activity counts from its UTC hour. */
function sessionSpan(session: SessionUsage): { first: number; last: number } {
  const last = Date.parse(session.lastActivity);
  const first = Date.parse(session.firstActivity ?? '');
  return { first: first <= last ? first - (first % 3_600_000) : last, last };
}

function hourEpoch(hour: unknown): number {
  if (typeof hour !== 'string' || !/^\d{4}-\d{2}-\d{2} \d{2}:00$/.test(hour)) return NaN;
  return Date.parse(`${hour.replace(' ', 'T')}:00Z`);
}

/** Internal coverage of the retained snapshot, independent of the provider filter. */
export function accountAnalyticsActivityCoverage(
  sources: SourceData[],
  from: number,
  to: number
): AccountAnalyticsActivityCoverage {
  let oldest = Infinity;
  const active: DashboardProvider[] = [];
  for (const source of attributeActivitySources(sources))
    for (const result of source.data)
      for (const hour of result.hourly) {
        const epoch = hourEpoch(hour.hour);
        if (!Number.isFinite(epoch)) continue;
        if (epoch < oldest) oldest = epoch;
        if (
          epoch >= from &&
          epoch <= to &&
          source.provider !== 'other' &&
          !active.includes(source.provider)
        )
          active.push(source.provider);
      }
  return {
    oldestHourAt: Number.isFinite(oldest) ? oldest : null,
    providersWithActivity: active,
  };
}

interface DayModelRow {
  date: string;
  provider: Provider;
  model: string | null;
  totals: Accumulator;
}

export function projectAccountAnalyticsActivity(
  sources: SourceData[],
  query: AccountAnalyticsQuery,
  from: number,
  to: number,
  status: AccountAnalyticsActivity['status'],
  message: string,
  options: AccountAnalyticsProjectionOptions = {}
): AccountAnalyticsActivity {
  const tz = options.tz ?? 'UTC';
  const base: AccountAnalyticsActivity = {
    status,
    scope: 'multi-host-cli',
    timezone: tz,
    accountAttribution: 'unavailable',
    costBasis: 'estimated-api-equivalent',
    fetchedAt: sources.map((source) => source.fetchedAt).sort()[0] ?? null,
    message,
    totals: null,
    providers: [],
    byDay: [],
    byHour: [],
    models: [],
    byDayModel: [],
    sessions: null,
    anomalies: null,
    sources: options.sources ?? [],
  };
  if (query.account !== 'all')
    return {
      ...base,
      status: 'unavailable',
      message:
        'Local CLI logs do not reliably identify a subscription account. Select all accounts to view local activity; account quota history remains available.',
    };
  // Usage is grouped by the provider that served it (the logged route), not by the tool that logged it.
  const selected = attributeActivitySources(sources).filter(
    (source) => query.provider === 'all' || query.provider === source.provider
  );
  if (selected.length === 0)
    return {
      ...base,
      status: status === 'loading' ? 'loading' : 'unavailable',
      message:
        query.provider === 'all'
          ? message
          : 'No CLI usage log in this range was served by this provider; its quota and balance observations remain available.',
    };
  base.fetchedAt = selected.map((source) => source.fetchedAt).sort()[0] ?? null;
  if (tz !== 'UTC' && hasPartialHourOffset(tz, from, to))
    base.message = `${message} Days are built from UTC hours, so a day boundary in this time zone can be off by up to 45 minutes.`;
  const pricing = memoiseAccountAnalyticsPricing(options.pricing ?? defaultAccountAnalyticsPricing);
  const dates = new Map<number, string>();
  const dateOf = (epoch: number): string => {
    let date = dates.get(epoch);
    if (date === undefined) {
      date = tz === 'UTC' ? new Date(epoch).toISOString().slice(0, 10) : localDate(epoch, tz);
      dates.set(epoch, date);
    }
    return date;
  };
  interface Bucket {
    totals: Accumulator;
    requests: number;
    requestsKnown: boolean;
  }
  const dayBuckets = new Map<string, Bucket & { date: string; provider: Provider }>();
  const hourBuckets = new Map<string, Bucket & { hour: string; provider: Provider }>();
  const modelBuckets = new Map<
    string,
    {
      model: string;
      provider: Provider;
      totals: Accumulator;
      rates: AccountAnalyticsModelRates | null;
      ratesSeen: boolean;
      tools: Set<AccountAnalyticsActivityProvider>;
    }
  >();
  const dayModels = new Map<string, DayModelRow>();
  const sessionCandidates = new Map<
    string,
    {
      provider: Provider;
      tool: AccountAnalyticsActivityProvider;
      lastActivity: number;
      session: SessionUsage;
    }
  >();
  // A session that several providers served counts under each of them, and once in the total.
  const distinctSessions = new Set<string>();
  const combined = accumulator();
  const addBucket = (bucket: Bucket, values: Accumulator, requestCount: unknown): void => {
    merge(bucket.totals, values);
    if (typeof requestCount === 'number' && Number.isFinite(requestCount) && requestCount >= 0)
      bucket.requests += requestCount;
    else bucket.requestsKnown = false;
  };
  const addDayModel = (
    date: string,
    provider: Provider,
    model: string | null,
    values: Record<TotalField, number>,
    parts: Record<PartField, number> | null
  ): void => {
    const key = `${date}\0${provider}\0${model ?? ''}\0${model === null ? 'unnamed' : 'named'}`;
    const row = dayModels.get(key) ?? { date, provider, model, totals: accumulator() };
    addValues(row.totals, values);
    addParts(row.totals, parts);
    dayModels.set(key, row);
  };
  for (const source of selected) {
    const sourceTotals = accumulator();
    let usageEvents = 0;
    const sessions = new Set<string>();
    const tools = new Set<AccountAnalyticsActivityProvider>();
    for (const result of source.data) {
      for (const hour of result.hourly) {
        const epoch = hourEpoch(hour.hour);
        if (!Number.isFinite(epoch) || epoch < from || epoch > to) continue;
        tools.add(result.tool);
        const date = dateOf(epoch);
        const priced = priceBreakdowns(hour, pricing);
        const values = accumulator();
        addValues(values, totals(hour));
        addParts(values, priced.parts);
        merge(sourceTotals, values);
        const dayKey = `${source.provider}:${date}`;
        const day = dayBuckets.get(dayKey) ?? {
          date,
          provider: source.provider,
          totals: accumulator(),
          requests: 0,
          requestsKnown: true,
        };
        addBucket(day, values, hour.requestCount);
        dayBuckets.set(dayKey, day);
        const hourKey = `${source.provider}:${hour.hour}`;
        const bucket = hourBuckets.get(hourKey) ?? {
          hour: `${hour.hour.replace(' ', 'T')}:00Z`,
          provider: source.provider,
          totals: accumulator(),
          requests: 0,
          requestsKnown: true,
        };
        addBucket(bucket, values, hour.requestCount);
        hourBuckets.set(hourKey, bucket);
        usageEvents += finite(hour.requestCount);
        for (const model of priced.models) {
          addDayModel(date, source.provider, model.name, model.values, model.parts);
          if (model.name === null) continue;
          const key = `${source.provider}:${model.name}`;
          const existing = modelBuckets.get(key) ?? {
            model: model.name,
            provider: source.provider,
            totals: accumulator(),
            rates: null,
            ratesSeen: false,
            tools: new Set<AccountAnalyticsActivityProvider>(),
          };
          existing.tools.add(result.tool);
          addValues(existing.totals, model.values);
          addParts(existing.totals, model.parts);
          // One row can mix routing providers; publish rates only when they agree.
          if (!existing.ratesSeen) existing.rates = model.rates;
          else if (
            existing.rates !== null &&
            (model.rates === null ||
              RATE_KEYS.some((rate) => existing.rates?.[rate] !== model.rates?.[rate]))
          )
            existing.rates = null;
          existing.ratesSeen = true;
          modelBuckets.set(key, existing);
        }
        if (priced.residual) addDayModel(date, source.provider, null, priced.residual, null);
      }
      for (const { part, whole, dominant } of result.session) {
        // Active in range: its activity overlaps the range, wherever it ends.
        const { first, last: lastActivity } = sessionSpan(part);
        if (first <= to && lastActivity >= from && typeof part.sessionId === 'string') {
          // Internal dedupe only; the published key is hashed for the sample alone.
          const key = `${result.tool}\0${part.sessionId}`;
          sessions.add(key);
          distinctSessions.add(key);
          // The sample lists each session once, under the provider that served most of it.
          const previous = sessionCandidates.get(key);
          if (dominant && (!previous || lastActivity > previous.lastActivity))
            sessionCandidates.set(key, {
              provider: source.provider,
              tool: result.tool,
              lastActivity,
              session: whole,
            });
        }
      }
    }
    merge(combined, sourceTotals);
    base.providers.push({
      provider: source.provider,
      label: USAGE_PROVIDER_LABELS[source.provider] ?? source.provider,
      totals: publish(sourceTotals),
      usageEvents,
      sessionCount: sessions.size,
      tools: TOOLS.filter((tool) => tools.has(tool)),
    });
  }
  const bucketRow = (bucket: Bucket) => ({
    ...publish(bucket.totals),
    requestCount: bucket.requestsKnown ? bucket.requests : null,
  });
  const byDay = [...dayBuckets.values()]
    .sort((a, b) => a.date.localeCompare(b.date) || a.provider.localeCompare(b.provider))
    .map((bucket) => ({ date: bucket.date, provider: bucket.provider, ...bucketRow(bucket) }));
  const byHour = [...hourBuckets.values()]
    .sort((a, b) => a.hour.localeCompare(b.hour) || a.provider.localeCompare(b.provider))
    .map((bucket) => ({ hour: bucket.hour, provider: bucket.provider, ...bucketRow(bucket) }));

  // Per-day model rows: the twelve costliest named models keep their name.
  const modelCost = new Map<string, number>();
  for (const row of dayModels.values())
    if (row.model !== null && row.model !== OTHER_MODELS) {
      const key = `${row.provider}\0${row.model}`;
      modelCost.set(key, (modelCost.get(key) ?? 0) + row.totals.estimatedCostUsd);
    }
  const named = new Set(
    [...modelCost.entries()]
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .slice(0, MAX_NAMED_DAY_MODELS)
      .map(([key]) => key)
  );
  const folded = new Map<string, DayModelRow & { model: string }>();
  for (const row of dayModels.values()) {
    const model =
      row.model !== null && named.has(`${row.provider}\0${row.model}`) ? row.model : OTHER_MODELS;
    const key = `${row.date}\0${row.provider}\0${model}`;
    const target = folded.get(key) ?? {
      date: row.date,
      provider: row.provider,
      model,
      totals: accumulator(),
    };
    merge(target.totals, row.totals);
    folded.set(key, target);
  }
  const byDayModel = [...folded.values()]
    .sort(
      (a, b) =>
        a.date.localeCompare(b.date) ||
        a.provider.localeCompare(b.provider) ||
        b.totals.estimatedCostUsd - a.totals.estimatedCostUsd ||
        a.model.localeCompare(b.model)
    )
    .map((row) => ({
      date: row.date,
      provider: row.provider,
      model: row.model,
      ...publish(row.totals),
    }));

  const usable = status === 'ok' || status === 'cached';
  const sample = [...sessionCandidates.entries()]
    .sort((a, b) => b[1].lastActivity - a[1].lastActivity || a[0].localeCompare(b[0]))
    .slice(0, MAX_SESSION_SAMPLE)
    .map(([, candidate]) => {
      const session = candidate.session;
      const key = sessionKey(candidate.tool, session.sessionId);
      const priced = priceBreakdowns(session, pricing);
      const values = accumulator();
      addValues(values, totals(session));
      addParts(values, priced.parts);
      const fromBreakdowns = priced.models
        .map((model) => model.name)
        .filter((name): name is string => name !== null && name.length > 0);
      const fromUsed = Array.isArray(session.modelsUsed)
        ? session.modelsUsed.filter(
            (name): name is string => validModelName(name) && name.length > 0
          )
        : [];
      return {
        key,
        provider: candidate.provider,
        lastActivity: new Date(candidate.lastActivity).toISOString(),
        models: [...new Set(fromBreakdowns.length > 0 ? fromBreakdowns : fromUsed)].slice(0, 5),
        target:
          typeof session.target === 'string' && KNOWN_CLI_TARGETS.has(session.target)
            ? session.target
            : null,
        ...publish(values),
      };
    });
  const sessionTotal = distinctSessions.size;
  if (usable && sample.length > 0)
    base.message = `${base.message} Session rows show each session's whole retained totals, which can include activity outside this range.`;

  const dayCosts = new Map<string, number>();
  for (const row of byDay)
    dayCosts.set(row.date, (dayCosts.get(row.date) ?? 0) + row.estimatedCostUsd);
  const anomalies = usable
    ? detectAccountAnalyticsAnomalies(
        [...dayCosts.entries()].map(([date, cost]) => ({ date, cost })),
        [...dayModels.values()]
          .filter(
            (row): row is DayModelRow & { model: string } =>
              row.model !== null && row.model.length > 0
          )
          .sort(
            (a, b) =>
              a.date.localeCompare(b.date) ||
              a.provider.localeCompare(b.provider) ||
              b.totals.estimatedCostUsd - a.totals.estimatedCostUsd ||
              a.model.localeCompare(b.model)
          )
          .map((row) => ({
            date: row.date,
            provider: row.provider,
            model: row.model,
            inputTokens: row.totals.inputTokens,
            outputTokens: row.totals.outputTokens,
            cacheReadTokens: row.totals.cacheReadTokens,
          }))
      )
    : null;

  return {
    ...base,
    totals: publish(combined),
    byDay,
    byHour,
    models: [...modelBuckets.values()]
      .sort(
        (a, b) =>
          b.totals.estimatedCostUsd - a.totals.estimatedCostUsd ||
          tokenTotal(b.totals) - tokenTotal(a.totals) ||
          a.provider.localeCompare(b.provider) ||
          a.model.localeCompare(b.model)
      )
      .slice(0, MAX_PUBLISHED_MODELS)
      .map((row) => ({
        model: row.model,
        provider: row.provider,
        ...publish(row.totals),
        rates: row.rates,
        tools: TOOLS.filter((tool) => row.tools.has(tool)),
      })),
    byDayModel,
    sessions: usable
      ? { total: sessionTotal, sample, truncated: sessionTotal > sample.length }
      : null,
    anomalies,
  };
}
