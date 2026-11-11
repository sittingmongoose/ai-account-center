/**
 * Model pricing lookup logic.
 *
 * The price table and its aliases live in model-pricing-data.ts (untangle
 * audit U9); this file resolves a model id to rates: the table, the
 * models.dev cache, or the unknown-model fallback, in that order.
 */

import {
  getKnownModelsDevModels,
  resolveModelsDevPricing,
  type ModelsDevPricingLookupOptions,
} from './models-dev/pricing-resolver';
import { MODEL_PRICING_ALIASES, PRICING_REGISTRY } from './model-pricing-data';
import type { ModelPricing, PricingRates } from './model-pricing-data';

// Re-exported so the module's public surface is unchanged by the split.
export type { ModelPricing, PricingRates };
export interface TokenUsage {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}

export interface PricingLookupOptions extends ModelsDevPricingLookupOptions {
  /**
   * Anthropic `service_tier` (e.g. `'fast'`). When set and the resolved model
   * has matching `serviceTiers` rates, those are returned instead of the
   * base rates. Unknown tiers transparently fall through to base.
   */
  serviceTier?: string;
}
// Default pricing for unknown models
const UNKNOWN_MODEL_PRICING: ModelPricing = {
  inputPerMillion: 3.0,
  outputPerMillion: 15.0,
  cacheCreationPerMillion: 3.75,
  cacheReadPerMillion: 0.3,
};

// ============================================================================
// PRICING FUNCTIONS
// ============================================================================

/**
 * Strip provider prefixes used by routing/catalog metadata.
 * The CCS static table remains model-keyed, so static fallback normalizes
 * provider-qualified model IDs before checking aliases.
 */
function stripProviderPrefix(model: string): string {
  const trimmed = model.trim();
  const slashIndex = trimmed.indexOf('/');
  if (slashIndex <= 0) {
    return trimmed;
  }
  return trimmed.slice(slashIndex + 1);
}

/**
 * Normalize model name for matching.
 * Handles variations like provider prefixes and case differences.
 */
function normalizeModelName(model: string): string {
  return stripProviderPrefix(model).toLowerCase();
}

/**
 * Strip trailing date suffix from model name (e.g., "-20260101")
 * Claude session IDs can place dates either at the end or before "-thinking".
 */
function stripDateSuffix(model: string): string {
  if (!model.startsWith('claude-')) {
    return model;
  }

  return model.replace(/-\d{8}(?=-thinking(?:$|:))/g, '').replace(/-\d{8}(?=$|:)/g, '');
}

/**
 * Strip a trailing reasoning-effort suffix from a Claude model id (e.g. "claude-opus-5-high" ->
 * "claude-opus-5"). Effort is a request setting, not a model: Anthropic prices every effort
 * level of a model at that model's per-token rates, so the suffix must not hide the base rate.
 */
function stripEffortSuffix(model: string): string {
  if (!model.startsWith('claude-')) {
    return model;
  }

  return model.replace(/-(?:low|medium|high|xhigh|max)(?=-thinking(?:$|:)|$|:)/g, '');
}

const NORMALIZED_PRICING_REGISTRY: Record<string, ModelPricing> = Object.entries(
  PRICING_REGISTRY
).reduce<Record<string, ModelPricing>>((acc, [key, pricing]) => {
  acc[normalizeModelName(key)] = pricing;
  return acc;
}, {});

function getLookupCandidates(model: string): string[] {
  const normalized = normalizeModelName(model);
  const baseModel = normalized.split(':')[0];

  const candidates: string[] = [normalized];
  if (baseModel !== normalized) {
    candidates.push(baseModel);
  }

  // Add date-stripped variants (e.g., "claude-opus-4-6-20260101" -> "claude-opus-4-6"), then
  // effort-stripped ones (e.g., "claude-opus-5-high" -> "claude-opus-5").
  for (const value of [normalized, baseModel]) {
    const dateStripped = stripDateSuffix(value);
    for (const variant of [dateStripped, stripEffortSuffix(dateStripped)]) {
      if (!candidates.includes(variant)) {
        candidates.push(variant);
      }
    }
  }

  return candidates;
}

function getDirectOrAliasPricing(model: string): ModelPricing | undefined {
  const directPricing = PRICING_REGISTRY[model];
  if (directPricing !== undefined) {
    return directPricing;
  }

  for (const candidate of getLookupCandidates(model)) {
    const normalizedPricing = NORMALIZED_PRICING_REGISTRY[candidate];
    if (normalizedPricing !== undefined) {
      return normalizedPricing;
    }

    const alias = MODEL_PRICING_ALIASES[candidate];
    if (alias !== undefined) {
      const aliasPricing = NORMALIZED_PRICING_REGISTRY[alias];
      if (aliasPricing !== undefined) {
        return aliasPricing;
      }
    }
  }

  return undefined;
}

function getCcsStaticPricing(model: string): ModelPricing | undefined {
  const staticPricing = getDirectOrAliasPricing(model);
  if (staticPricing !== undefined) {
    return staticPricing;
  }

  const providerlessModel = stripProviderPrefix(model);
  if (providerlessModel !== model.trim()) {
    return getDirectOrAliasPricing(providerlessModel);
  }

  return undefined;
}

function getCcsPolicyOverridePricing(model: string): ModelPricing | undefined {
  const providerlessModel = stripProviderPrefix(model);
  const normalized = normalizeModelName(providerlessModel);

  for (const candidate of getLookupCandidates(providerlessModel)) {
    const alias = MODEL_PRICING_ALIASES[candidate];
    if (alias !== undefined) {
      const aliasPricing = NORMALIZED_PRICING_REGISTRY[alias];
      if (aliasPricing !== undefined) {
        return aliasPricing;
      }
    }

    if (candidate !== normalized) {
      const variantPricing = NORMALIZED_PRICING_REGISTRY[candidate];
      if (variantPricing !== undefined) {
        return variantPricing;
      }
    }
  }

  return undefined;
}

function hasProviderContext(model: string, options: PricingLookupOptions): boolean {
  return Boolean(options.provider || /^[^/]+\//.test(model.trim()));
}

/**
 * Apply per-service-tier rates if the resolved model declares them and the
 * caller requested a matching tier. Unknown tiers transparently fall through
 * to the base rates so existing callers stay unaffected.
 *
 * TODO(opus-fast-mode): No production caller currently passes `serviceTier`.
 * Anthropic's `service_tier` is not yet captured on CliproxyRequestDetail, so
 * usage transformers (cliproxy-usage-transformer.ts, data-aggregator.ts) bill
 * fast-mode requests at the standard rate — i.e. fast Opus is under-reported by
 * the tier premium ($10/$50 vs $5/$25). Wire `serviceTier` through once the
 * usage pipeline records it. The schema below is ready for that integration.
 */
function applyServiceTier(pricing: ModelPricing, tier: string | undefined): ModelPricing {
  if (!tier) return pricing;
  const tierRates = pricing.serviceTiers?.[tier];
  if (!tierRates) return pricing;
  return { ...tierRates, serviceTiers: pricing.serviceTiers };
}

/** Where a resolved rate came from: the CCS table, the models.dev cache, or the unknown-model fallback. */
export type ModelPricingSource = 'builtin' | 'models-dev' | 'fallback';

export interface ModelPricingResolution {
  pricing: ModelPricing;
  source: ModelPricingSource;
}

/**
 * Get pricing for a model with narrow fuzzy matching fallback.
 * Unknown future model families should fall back instead of inheriting the
 * first known family tier that happens to share a prefix.
 */
export function getModelPricing(model: string, options: PricingLookupOptions = {}): ModelPricing {
  return getModelPricingWithSource(model, options).pricing;
}

/** The same lookup as `getModelPricing`, also saying which table supplied the rates. */
export function getModelPricingWithSource(
  model: string,
  options: PricingLookupOptions = {}
): ModelPricingResolution {
  const resolved = resolveBasePricing(model, options);
  return {
    pricing: applyServiceTier(resolved.pricing, options.serviceTier),
    source: resolved.source,
  };
}

function resolveBasePricing(model: string, options: PricingLookupOptions): ModelPricingResolution {
  if (hasProviderContext(model, options)) {
    const ccsOverridePricing = getCcsPolicyOverridePricing(model);
    if (ccsOverridePricing !== undefined) {
      return { pricing: ccsOverridePricing, source: 'builtin' };
    }

    const providerPricing = resolveModelsDevPricing(model, options);
    if (providerPricing !== undefined) {
      return { pricing: providerPricing.pricing, source: 'models-dev' };
    }
  }

  const ccsStaticPricing = getCcsStaticPricing(model);
  if (ccsStaticPricing !== undefined) {
    return { pricing: ccsStaticPricing, source: 'builtin' };
  }

  const modelsDevPricing = resolveModelsDevPricing(model, options);
  if (modelsDevPricing !== undefined) {
    return { pricing: modelsDevPricing.pricing, source: 'models-dev' };
  }

  for (const candidate of getLookupCandidates(model)) {
    // Allow provider/routing wrappers to suffix a canonical model ID.
    for (const [key, pricing] of Object.entries(NORMALIZED_PRICING_REGISTRY)) {
      if (candidate.endsWith(key)) {
        return { pricing, source: 'builtin' };
      }
    }
  }

  // Fallback to unknown model pricing
  return { pricing: UNKNOWN_MODEL_PRICING, source: 'fallback' };
}

/**
 * Calculate cost in USD from token usage and model
 * @param usage - Token counts (input, output, cache creation, cache read)
 * @param model - Model name for pricing lookup
 * @returns Cost in USD
 */
export function calculateCost(
  usage: TokenUsage,
  model: string,
  options: PricingLookupOptions = {}
): number {
  const pricing = getModelPricing(model, options);

  const inputCost = (usage.inputTokens / 1_000_000) * pricing.inputPerMillion;
  const outputCost = (usage.outputTokens / 1_000_000) * pricing.outputPerMillion;
  const cacheCreationCost =
    (usage.cacheCreationTokens / 1_000_000) * pricing.cacheCreationPerMillion;
  const cacheReadCost = (usage.cacheReadTokens / 1_000_000) * pricing.cacheReadPerMillion;

  return inputCost + outputCost + cacheCreationCost + cacheReadCost;
}

/**
 * Get list of all known models for UI display
 */
export function getKnownModels(): string[] {
  return [...new Set([...Object.keys(PRICING_REGISTRY), ...getKnownModelsDevModels()])];
}

/**
 * Check if a model has custom pricing (not using fallback)
 */
export function hasCustomPricing(model: string, options: PricingLookupOptions = {}): boolean {
  return (
    getCcsStaticPricing(model) !== undefined ||
    resolveModelsDevPricing(model, options) !== undefined
  );
}
