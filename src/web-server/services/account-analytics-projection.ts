import { createHash } from 'crypto';
import { getModelPricingWithSource } from '../model-pricing';
import { hasPartialHourOffset, localDate } from './account-analytics-range';
import { detectAccountAnalyticsAnomalies } from './account-analytics-anomalies';
import type { UsageWorkerResult } from '../usage/worker-client';
import type { ModelBreakdown, SessionUsage } from '../usage/types';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsActivityCoverage,
  AccountAnalyticsActivityProvider,
  AccountAnalyticsActivityTotals,
  AccountAnalyticsModelRates,
  AccountAnalyticsQuery,
} from './account-analytics-types';

/**
 * Pure projection of the retained local CLI snapshot into the Analytics
 * activity block: totals, per-type cost estimates, local-day buckets, the
 * per-day model view, a path-free session sample and the original CCS
 * anomaly rules. Nothing here reads disk or attributes activity to accounts.
 */

/** One local CLI source as the activity reader retains it. */
export interface SourceData {
  provider: 'claude' | 'codex';
  data: UsageWorkerResult[];
  fetchedAt: string;
}

const FIELDS = [
  'inputTokens',
  'outputTokens',
  'cacheCreationTokens',
  'cacheReadTokens',
  'estimatedCostUsd',
] as const;

type Provider = AccountAnalyticsActivityProvider;
type TotalField = (typeof FIELDS)[number];
type TokenField = Exclude<TotalField, 'estimatedCostUsd'>;
const TOKEN_FIELDS: readonly TokenField[] = [
  'inputTokens',
  'outputTokens',
  'cacheCreationTokens',
  'cacheReadTokens',
];
const PART_FIELDS = ['input', 'output', 'cacheWrite', 'cacheRead'] as const;
type PartField = (typeof PART_FIELDS)[number];
const RATE_FOR_PART: Record<
  PartField,
  [TokenField, keyof Omit<AccountAnalyticsModelRates, 'source'>]
> = {
  input: ['inputTokens', 'inputPerMillion'],
  output: ['outputTokens', 'outputPerMillion'],
  cacheWrite: ['cacheCreationTokens', 'cacheCreationPerMillion'],
  cacheRead: ['cacheReadTokens', 'cacheReadPerMillion'],
};
const RATE_KEYS = [
  'inputPerMillion',
  'outputPerMillion',
  'cacheCreationPerMillion',
  'cacheReadPerMillion',
  'source',
] as const;
const KNOWN_CLI_TARGETS: ReadonlySet<string> = new Set(['claude', 'codex', 'droid']);
const MAX_SESSION_SAMPLE = 50;
const MAX_NAMED_DAY_MODELS = 12;
const OTHER_MODELS = 'Other models';

/** Rates for one model, or null when no usable rate exists. */
export type AccountAnalyticsPricingLookup = (
  model: string,
  provider: string | undefined
) => AccountAnalyticsModelRates | null;

export interface AccountAnalyticsProjectionOptions {
  /** IANA zone for day buckets; UTC when absent. */
  tz?: string;
  /** Pricing lookup; memoised per model for one projection. */
  pricing?: AccountAnalyticsPricingLookup;
}

/** The same list rates the native worker priced each model with, plus where they came from. */
export function defaultAccountAnalyticsPricing(
  model: string,
  provider: string | undefined
): AccountAnalyticsModelRates | null {
  try {
    const { pricing, source } = getModelPricingWithSource(model, { provider });
    const rates: AccountAnalyticsModelRates = {
      inputPerMillion: pricing.inputPerMillion,
      outputPerMillion: pricing.outputPerMillion,
      cacheCreationPerMillion: pricing.cacheCreationPerMillion,
      cacheReadPerMillion: pricing.cacheReadPerMillion,
      source,
    };
    return PART_FIELDS.every((part) => {
      const value = rates[RATE_FOR_PART[part][1]];
      return typeof value === 'number' && Number.isFinite(value) && value >= 0;
    })
      ? rates
      : null;
  } catch {
    return null;
  }
}

/** Memoise a lookup per model and provider; rates are stable for one read of the snapshot. */
export function memoiseAccountAnalyticsPricing(
  lookup: AccountAnalyticsPricingLookup
): AccountAnalyticsPricingLookup {
  const cache = new Map<string, AccountAnalyticsModelRates | null>();
  return (model, provider) => {
    const key = `${provider ?? ''}\0${model}`;
    if (!cache.has(key)) {
      if (cache.size >= 4096) cache.clear();
      cache.set(key, lookup(model, provider));
    }
    const rates = cache.get(key) ?? null;
    return rates ? { ...rates } : null;
  };
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
}): Record<TotalField, number> {
  return {
    inputTokens: finite(value.inputTokens),
    outputTokens: finite(value.outputTokens),
    cacheCreationTokens: finite(value.cacheCreationTokens),
    cacheReadTokens: finite(value.cacheReadTokens),
    estimatedCostUsd: finite(value.totalCost ?? value.cost),
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
    const parts = rates
      ? {
          input: (values.inputTokens / 1_000_000) * rates.inputPerMillion,
          output: (values.outputTokens / 1_000_000) * rates.outputPerMillion,
          cacheWrite: (values.cacheCreationTokens / 1_000_000) * rates.cacheCreationPerMillion,
          cacheRead: (values.cacheReadTokens / 1_000_000) * rates.cacheReadPerMillion,
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
        }
      : null,
    models,
  };
}

function sessionKey(provider: Provider, sessionId: string): string {
  return createHash('sha256')
    .update(`aac-session-v1:${provider}:${sessionId}`)
    .digest('hex')
    .slice(0, 16);
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
  const active = new Set<Provider>();
  for (const source of sources)
    for (const result of source.data)
      for (const hour of result.hourly) {
        const epoch = hourEpoch(hour.hour);
        if (!Number.isFinite(epoch)) continue;
        if (epoch < oldest) oldest = epoch;
        if (epoch >= from && epoch <= to) active.add(source.provider);
      }
  return {
    oldestHourAt: Number.isFinite(oldest) ? oldest : null,
    providersWithActivity: (['claude', 'codex'] as const).filter((provider) =>
      active.has(provider)
    ),
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
    scope: 'ubuntu-local-cli',
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
  };
  if (query.account !== 'all')
    return {
      ...base,
      status: 'unavailable',
      message:
        'Local CLI logs do not reliably identify a subscription account. Select all accounts to view local activity; account quota history remains available.',
    };
  const selected = sources.filter(
    (source) => query.provider === 'all' || query.provider === source.provider
  );
  if (selected.length === 0)
    return {
      ...base,
      status: status === 'loading' ? 'loading' : 'unavailable',
      message:
        query.provider !== 'all' && query.provider !== 'claude' && query.provider !== 'codex'
          ? 'This provider reports quota and balance observations; local token and session history is not available.'
          : message,
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
    }
  >();
  const dayModels = new Map<string, DayModelRow>();
  const sessionCandidates = new Map<
    string,
    { provider: Provider; lastActivity: number; session: SessionUsage }
  >();
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
    for (const result of source.data) {
      for (const hour of result.hourly) {
        const epoch = hourEpoch(hour.hour);
        if (!Number.isFinite(epoch) || epoch < from || epoch > to) continue;
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
          };
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
      for (const session of result.session) {
        const lastActivity = Date.parse(session.lastActivity);
        if (lastActivity >= from && lastActivity <= to && typeof session.sessionId === 'string') {
          sessions.add(session.sessionId);
          // Internal dedupe only; the published key is hashed for the sample alone.
          const key = `${source.provider}\0${session.sessionId}`;
          const previous = sessionCandidates.get(key);
          if (!previous || lastActivity > previous.lastActivity)
            sessionCandidates.set(key, { provider: source.provider, lastActivity, session });
        }
      }
    }
    merge(combined, sourceTotals);
    base.providers.push({
      provider: source.provider,
      label: source.provider === 'claude' ? 'Claude Code logs' : 'Codex logs',
      totals: publish(sourceTotals),
      usageEvents,
      sessionCount: sessions.size,
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
      const key = sessionKey(candidate.provider, session.sessionId);
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
  const sessionTotal = base.providers.reduce((sum, row) => sum + row.sessionCount, 0);

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
      .sort((a, b) => b.totals.estimatedCostUsd - a.totals.estimatedCostUsd)
      .slice(0, 30)
      .map((row) => ({
        model: row.model,
        provider: row.provider,
        ...publish(row.totals),
        rates: row.rates,
      })),
    byDayModel,
    sessions: usable
      ? { total: sessionTotal, sample, truncated: sessionTotal > sample.length }
      : null,
    anomalies,
  };
}
