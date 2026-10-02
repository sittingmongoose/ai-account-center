import { getModelPricingWithSource } from '../model-pricing';
import type { AccountAnalyticsModelRates } from './account-analytics-types';

/** Rates for one model, or null when no usable rate exists. */
export type AccountAnalyticsPricingLookup = (
  model: string,
  provider: string | undefined
) => AccountAnalyticsModelRates | null;

const RATE_FIELDS = [
  'inputPerMillion',
  'outputPerMillion',
  'cacheCreationPerMillion',
  'cacheReadPerMillion',
] as const;

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
    return RATE_FIELDS.every(
      (field) =>
        typeof rates[field] === 'number' && Number.isFinite(rates[field]) && rates[field] >= 0
    )
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
