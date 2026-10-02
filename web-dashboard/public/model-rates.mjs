// Model rates for the Analytics per-type cost split, mirrored read-only from src/web-server/model-pricing.ts
// (PRICING_REGISTRY, MODEL_PRICING_ALIASES and UNKNOWN_MODEL_PRICING at ccb2dcb0, unchanged at 8fb5e3de).
// tests/model-rates.test.mjs re-reads that file and fails when a rate here drifts from it.
//
// The backend logs one estimatedCostUsd per model; it never reports the cost of each token type. The page
// multiplies each model's token types by these rates and keeps the split only when the four parts add up to
// the logged estimate (reconcile below). A model that does not reconcile shows token shares and says so.
// Lookup order follows getModelPricing() for a model id without a provider prefix: the static table (exact,
// normalised, date-stripped and aliased ids), then the models.dev rates CCS resolved for the logged models
// (MODELS_DEV below), then a known id that ends the model id, then the CCS fallback rate.

/** USD per million tokens: [input, output, cache write, cache read]. */
export const STATIC_RATES = {
  // key: [input, output, cache write, cache read] USD per million tokens
  'claude-3-haiku-20240307': [0.25, 1.25, 0.3, 0.03],
  'claude-3-5-haiku-20241022': [0.8, 4, 1, 0.08],
  'claude-3-5-haiku-latest': [0.8, 4, 1, 0.08],
  'claude-haiku-4-5-20251001': [1, 5, 1.25, 0.1],
  'claude-haiku-4-5': [1, 5, 1.25, 0.1],
  'claude-3-5-sonnet-20240620': [3, 15, 3.75, 0.3],
  'claude-3-5-sonnet-20241022': [3, 15, 3.75, 0.3],
  'claude-3-5-sonnet-latest': [3, 15, 3.75, 0.3],
  'claude-3-7-sonnet-20250219': [3, 15, 3.75, 0.3],
  'claude-3-7-sonnet-latest': [3, 15, 3.75, 0.3],
  'claude-3-opus-20240229': [15, 75, 18.75, 1.5],
  'claude-3-opus-latest': [15, 75, 18.75, 1.5],
  'claude-4-sonnet-20250514': [3, 15, 3.75, 0.3],
  'claude-sonnet-4-20250514': [3, 15, 3.75, 0.3],
  'claude-sonnet-4': [3, 15, 3.75, 0.3],
  'claude-sonnet-4-5-20250929': [3, 15, 3.75, 0.3],
  'claude-sonnet-4-5': [3, 15, 3.75, 0.3],
  'claude-sonnet-4-5-thinking': [3, 15, 3.75, 0.3],
  'claude-sonnet-4-6': [3, 15, 3.75, 0.3],
  'claude-sonnet-4-6-thinking': [3, 15, 3.75, 0.3],
  'claude-sonnet-5': [2, 10, 2.5, 0.2],
  'claude-sonnet-5-thinking': [2, 10, 2.5, 0.2],
  'claude-4-opus-20250514': [15, 75, 18.75, 1.5],
  'claude-opus-4-20250514': [15, 75, 18.75, 1.5],
  'claude-opus-4': [15, 75, 18.75, 1.5],
  'claude-opus-4-1': [15, 75, 18.75, 1.5],
  'claude-opus-4-1-20250805': [15, 75, 18.75, 1.5],
  'claude-opus-4-5-20251101': [5, 25, 6.25, 0.5],
  'claude-opus-4-5': [5, 25, 6.25, 0.5],
  'claude-opus-4-5-thinking': [5, 25, 6.25, 0.5],
  'claude-opus-4-6': [5, 25, 6.25, 0.5],
  'claude-opus-4-6-thinking': [5, 25, 6.25, 0.5],
  'claude-opus-4-7': [5, 25, 6.25, 0.5],
  'claude-opus-4-7-thinking': [5, 25, 6.25, 0.5],
  'claude-opus-4-8': [5, 25, 6.25, 0.5],
  'claude-opus-5': [5, 25, 6.25, 0.5],
  'claude-opus-5-thinking': [5, 25, 6.25, 0.5],
  'claude-fable-5': [10, 50, 12.5, 1],
  'claude-fable-5-1': [10, 50, 12.5, 0.25],
  'gpt-4o': [2.5, 10, 0, 1.25],
  'gpt-4o-2024-08-06': [2.5, 10, 0, 1.25],
  'gpt-4o-2024-11-20': [2.5, 10, 0, 1.25],
  'gpt-4o-mini': [0.15, 0.6, 0, 0.075],
  'gpt-4.1': [2, 8, 0, 0.5],
  'gpt-4.1-mini': [0.4, 1.6, 0, 0.1],
  'gpt-4.1-nano': [0.1, 0.4, 0, 0.025],
  'gpt-4.5-preview': [75, 150, 0, 37.5],
  'gpt-3.5-turbo': [1.5, 2, 0, 0],
  'gpt-3.5-turbo-0125': [0.5, 1.5, 0, 0],
  'o1-preview': [15, 60, 0, 7.5],
  'o1-mini': [3, 12, 0, 1.5],
  'o3-mini': [1.1, 4.4, 0, 0.55],
  'gpt-5': [1.25, 10, 0, 0.125],
  'gpt-5-chat': [1.25, 10, 0, 0.125],
  'gpt-5-codex': [1.25, 10, 0, 0.125],
  'gpt-5-mini': [0.25, 2, 0, 0.025],
  'gpt-5-nano': [0.05, 0.4, 0, 0.005],
  'codex-mini-latest': [1.5, 6, 0, 0.375],
  'gemini-2.5-flash': [0.3, 2.5, 0, 0.075],
  'gemini-2.5-flash-lite': [0.1, 0.4, 0, 0.025],
  'gemini-2.5-pro': [1.25, 10, 0, 0.3125],
  'gemini-2.0-flash': [0.1, 0.4, 0, 0.025],
  'gemini-2.0-flash-exp': [0, 0, 0, 0],
  'gemini-1.5-flash': [0.075, 0.3, 0, 0],
  'gemini-1.5-flash-8b': [0.0375, 0.15, 0, 0],
  'gemini-1.5-pro': [3.5, 10.5, 0, 0],
  'gemini-3-pro-preview': [2, 12, 0, 0],
  'gemini-3-pro': [2, 12, 0, 0],
  'gemini-3-pro-high': [4, 18, 0, 0],
  'glm-5.2': [1.4, 4.4, 0, 0.26],
  'glm-5': [1, 3.2, 0, 0.2],
  'glm-4.7': [0.4, 1.5, 0, 0.2],
  'glm-4.6': [0.35, 1.5, 0, 0.175],
  'glm-4.6-cc-max': [0.35, 1.5, 0, 0.175],
  'glm-4.5': [0.35, 1.55, 0, 0.175],
  'glm-4.5-air': [0.13, 0.85, 0, 0.025],
  'kimi-k2.5': [0.6, 3, 0, 0.1],
  'kimi-for-coding': [0.6, 2.5, 0, 0.15],
  'kimi-k2-0905-preview': [0.6, 2.5, 0, 0.15],
  'kimi-k2-turbo-preview': [1.15, 8, 0, 0.15],
  'kimi-k2-thinking': [0.6, 2.5, 0, 0.15],
  'kimi-k2-thinking-turbo': [1.15, 8, 0, 0.15],
  'kimi-k2': [0.6, 2.5, 0, 0.15],
  'kimi-k2-instruct': [1, 3, 0, 0],
  'kimi-latest': [2, 5, 0, 0.15],
  'kimi-latest-128k': [2, 5, 0, 0.15],
  'kimi-latest-32k': [1, 3, 0, 0.15],
  'kimi-latest-8k': [0.2, 2, 0, 0.15],
  'kimi-thinking-preview': [30, 30, 0, 0],
  'moonshot-v1-8k': [0.2, 2, 0, 0],
  'moonshot-v1-32k': [1, 3, 0, 0],
  'moonshot-v1-128k': [2, 5, 0, 0],
  'moonshot-v1-auto': [2, 5, 0, 0],
  'MiniMax-M3': [0.3, 1.2, 0, 0.06],
  'MiniMax-M2.5': [0.3, 1.2, 0.375, 0.03],
  'MiniMax-M2.5-lightning': [0.6, 2.4, 0.375, 0.03],
  'MiniMax-M2.1': [0.3, 1.2, 0.375, 0.03],
  'MiniMax-M2.1-lightning': [0.6, 2.4, 0.375, 0.03],
  'MiniMax-M2': [0.3, 1.2, 0.375, 0.03],
  'qwen3-max': [1.2, 6, 1.2, 0.24],
  'qwen3-max-2026-01-23': [1.2, 6, 1.2, 0.24],
  'qwen3-max-preview': [1.2, 6, 1.2, 0.24],
  'qwen3.5-plus': [0.4, 2.4, 0.4, 0.08],
  'qwen3.5-flash': [0.1, 0.4, 0.1, 0.02],
  'qwen3-coder-plus': [1, 5, 1, 0.2],
  'qwen3-coder-flash': [0.3, 1.5, 0.3, 0.06],
  'deepseek-chat': [0.27, 1.1, 0, 0.07],
  'deepseek-reasoner': [0.55, 2.19, 0, 0.14],
  'deepseek-coder': [0.14, 0.28, 0, 0],
  'mistral-large-latest': [2, 6, 0, 0],
  'mistral-medium-latest': [2.7, 8.1, 0, 0],
  'mistral-small-latest': [0.2, 0.6, 0, 0],
  'codestral-latest': [0.3, 0.9, 0, 0],
};

export const ALIASES = {
  'qwen3-coder': 'qwen3-coder-plus',
  'qwen3-235b': 'qwen3-max',
  'qwen3-vl-plus': 'qwen3.5-plus',
  'qwen3-32b': 'qwen3.5-plus',
  'gemini-3-flash-preview': 'gemini-2.5-flash',
  'gemini-3-flash-preview-customtools': 'gemini-2.5-flash',
  'gemini-3.1-pro-preview': 'gemini-3-pro-preview',
  'gemini-3.1-flash-preview': 'gemini-2.5-flash',
  'gemini-3.1-pro-preview-customtools': 'gemini-3-pro-preview',
  'gemini-3.1-flash-preview-customtools': 'gemini-2.5-flash',
  'gemini-3-1-pro-preview': 'gemini-3-pro-preview',
  'gemini-3-1-flash-preview': 'gemini-2.5-flash',
  'gemini-3-1-pro-preview-customtools': 'gemini-3-pro-preview',
  'gemini-3-1-flash-preview-customtools': 'gemini-2.5-flash',
};

/** UNKNOWN_MODEL_PRICING: the rate CCS uses for a model it does not list. */
export const FALLBACK_RATES = [3, 15, 3.75, 0.3];

/**
 * The models.dev rates CCS resolved for the Codex models in the logs (resolveModelsDevPricing, base tier under
 * 272K context, ~/.ccs/models-dev-registry-cache.json fetched 2026-09-30), as recorded by the approved concept.
 * They are not in the static table; when the cache moves, reconcile() fails and the page shows token shares.
 */
export const MODELS_DEV = {
  'gpt-6-astra': [10, 50, 12.5, 1],
  'gpt-6-sol': [2, 10, 2.5, 0.2],
  'gpt-6.1-sol': [2, 10, 2.5, 0.1],
  'gpt-6-luna': [0.1, 0.5, 0.125, 0.01],
};

const stripProvider = model => { const t = model.trim(); const i = t.indexOf('/'); return i <= 0 ? t : t.slice(i + 1); };
const normalise = model => stripProvider(model).toLowerCase();
const stripDate = model => model.startsWith('claude-') ? model.replace(/-\d{8}(?=-thinking(?:$|:))/g, '').replace(/-\d{8}(?=$|:)/g, '') : model;
const NORMALISED = Object.fromEntries(Object.entries(STATIC_RATES).map(([key, rate]) => [normalise(key), rate]));
function candidates(model) {
  const normalised = normalise(model), base = normalised.split(':')[0];
  const list = [normalised];
  if (base !== normalised) list.push(base);
  for (const value of [stripDate(normalised), stripDate(base)]) if (!list.includes(value)) list.push(value);
  return list;
}
function direct(model) {
  if (STATIC_RATES[model]) return STATIC_RATES[model];
  for (const candidate of candidates(model)) {
    if (NORMALISED[candidate]) return NORMALISED[candidate];
    const alias = ALIASES[candidate];
    if (alias && NORMALISED[alias]) return NORMALISED[alias];
  }
  return null;
}
const named = (rate, source) => ({ in: rate[0], out: rate[1], cw: rate[2], cr: rate[3], source });

/** The rate CCS prices a model at, with where it came from: 'builtin' | 'models-dev' | 'fallback'. */
export function modelRates(model) {
  if (typeof model !== 'string' || !model.trim()) return named(FALLBACK_RATES, 'fallback');
  const found = direct(model) || (stripProvider(model) !== model.trim() ? direct(stripProvider(model)) : null);
  if (found) return named(found, 'builtin');
  const dev = MODELS_DEV[normalise(model)];
  if (dev) return named(dev, 'models-dev');
  for (const candidate of candidates(model)) {
    for (const [key, rate] of Object.entries(NORMALISED)) if (candidate.endsWith(key)) return named(rate, 'builtin');
  }
  return named(FALLBACK_RATES, 'fallback');
}

const TYPES = [['in', 'inputTokens'], ['out', 'outputTokens'], ['cw', 'cacheCreationTokens'], ['cr', 'cacheReadTokens']];
/**
 * Per-type costs of one logged model row at its rates. `reconciled` is true only when the four parts add up to the
 * logged estimatedCostUsd (to half a cent, or one part in a million of large totals); the parts are never rescaled.
 */
export function reconcile(row) {
  const rate = modelRates(row?.model);
  const cost = Object.fromEntries(TYPES.map(([key, field]) => [key, (Number(row?.[field]) || 0) * rate[key] / 1e6]));
  const sum = cost.in + cost.out + cost.cw + cost.cr;
  const logged = typeof row?.estimatedCostUsd === 'number' && Number.isFinite(row.estimatedCostUsd) ? row.estimatedCostUsd : null;
  const reconciled = logged !== null && Math.abs(sum - logged) <= Math.max(0.005, logged * 1e-6);
  return { rate, cost, sum, logged, reconciled };
}
