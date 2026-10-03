import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { STATIC_RATES, ALIASES, FALLBACK_RATES, modelRates, reconcile } from '../public/model-rates.mjs';

// The mirror must equal src/web-server/model-pricing.ts (read only): the page splits each model's logged cost by
// these rates, so a drifted rate would draw a wrong split.
const source = fs.readFileSync(new URL('../../src/web-server/model-pricing.ts', import.meta.url), 'utf8');

test('the static rate table mirrors PRICING_REGISTRY entry for entry', () => {
  const registry = source.slice(source.indexOf('const PRICING_REGISTRY'), source.indexOf('const MODEL_PRICING_ALIASES'));
  const entries = [...registry.matchAll(/'([^']+)': \{\s*inputPerMillion: ([\d.]+),\s*outputPerMillion: ([\d.]+),\s*cacheCreationPerMillion: ([\d.]+),\s*cacheReadPerMillion: ([\d.]+),/g)]
    .map(m => [m[1], [Number(m[2]), Number(m[3]), Number(m[4]), Number(m[5])]]);
  assert.ok(entries.length > 100);
  assert.equal([...registry.matchAll(/^ {2}'[^']+': \{/gm)].length, entries.length, 'every registry entry starts with its four base rates');
  assert.deepEqual(Object.fromEntries(entries), STATIC_RATES);
});

test('aliases and the unknown-model fallback mirror model-pricing.ts', () => {
  const block = source.slice(source.indexOf('const MODEL_PRICING_ALIASES'), source.indexOf('// Default pricing for unknown models'));
  assert.deepEqual(Object.fromEntries([...block.matchAll(/'([^']+)': '([^']+)'/g)].map(m => [m[1], m[2]])), ALIASES);
  const unknown = /UNKNOWN_MODEL_PRICING: ModelPricing = \{\s*inputPerMillion: ([\d.]+),\s*outputPerMillion: ([\d.]+),\s*cacheCreationPerMillion: ([\d.]+),\s*cacheReadPerMillion: ([\d.]+),/.exec(source);
  assert.deepEqual(unknown.slice(1).map(Number), FALLBACK_RATES);
});

test('lookup follows getModelPricing: exact, normalised, date-stripped, aliased, models.dev, suffix, fallback', () => {
  assert.deepEqual(modelRates('claude-fable-5-1'), { in: 10, out: 50, cw: 12.5, cr: 0.25, source: 'builtin' });
  assert.equal(modelRates('Claude-Haiku-4-5').source, 'builtin');
  assert.equal(modelRates('claude-opus-4-6-20260101').in, 5);
  assert.equal(modelRates('anthropic/claude-sonnet-4').in, 3);
  assert.deepEqual(modelRates('qwen3-coder'), { ...modelRates('qwen3-coder-plus'), source: 'builtin' });
  assert.equal(modelRates('gpt-6-sol').source, 'models-dev');
  assert.equal(modelRates('claude-aac-unlisted-9').source, 'fallback');
  assert.equal(modelRates('<synthetic>').source, 'fallback');
  assert.equal(modelRates('').source, 'fallback');
});

test('a split is kept only when its four parts add up to the logged estimate; parts are never rescaled', () => {
  const row = { model: 'claude-fable-5-1', inputTokens: 1_000_000, outputTokens: 1_000_000, cacheCreationTokens: 1_000_000, cacheReadTokens: 1_000_000 };
  const good = reconcile({ ...row, estimatedCostUsd: 10 + 50 + 12.5 + 0.25 });
  assert.equal(good.reconciled, true);
  assert.deepEqual(good.cost, { in: 10, out: 50, cw: 12.5, cr: 0.25 });
  const off = reconcile({ ...row, estimatedCostUsd: 74.5 });
  assert.equal(off.reconciled, false);
  assert.deepEqual(off.cost, good.cost);
  assert.equal(reconcile({ ...row, estimatedCostUsd: null }).reconciled, false);
});

test('Opus 5.5, Sonnet 5.5, GLM-5.3 and Claude effort suffixes resolve to listed rates, not the fallback', () => {
  // platform.claude.com/docs/en/about-claude/pricing and docs.z.ai/guides/overview/pricing, read 2026-10-03
  assert.deepEqual(modelRates('claude-opus-5-5'), { in: 4, out: 20, cw: 5, cr: 0.2, source: 'builtin' });
  assert.deepEqual(modelRates('claude-sonnet-5-5'), { in: 2, out: 10, cw: 2.5, cr: 0.2, source: 'builtin' });
  assert.deepEqual(modelRates('glm-5.3'), { in: 1.4, out: 4.4, cw: 0, cr: 0.26, source: 'builtin' });
  assert.deepEqual(modelRates('GLM-5.3-Flash'), { in: 0.15, out: 0.5, cw: 0, cr: 0.03, source: 'builtin' });
  assert.deepEqual(modelRates('claude-opus-5-high'), modelRates('claude-opus-5'));
  assert.deepEqual(modelRates('claude-opus-5-5-xhigh'), modelRates('claude-opus-5-5'));
  assert.deepEqual(modelRates('claude-opus-4-6-medium-thinking'), modelRates('claude-opus-4-6-thinking'));
  assert.equal(modelRates('gemini-3-pro-high').in, 4, 'only Claude ids lose an effort suffix');
  const row = { model: 'claude-opus-5-5', inputTokens: 1e6, outputTokens: 1e6, cacheCreationTokens: 1e6, cacheReadTokens: 1e6 };
  assert.equal(reconcile({ ...row, estimatedCostUsd: 4 + 20 + 5 + 0.2 }).reconciled, true);
});
