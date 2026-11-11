/**
 * Parity between the pricing table split (U9) and the lookups it served before.
 *
 * The golden (model-pricing-table-parity.golden.json) was captured from
 * model-pricing.ts as it existed before the table moved to
 * model-pricing-data.ts: every registry id, every alias id and a spread of
 * normalized, prefixed, date-suffixed and unknown variants, resolved through
 * getModelPricingWithSource. This test re-resolves the same ids against the
 * split module and requires every price, model id and lookup result to be
 * identical, so the move cannot drift.
 *
 * Regenerating the golden is legitimate only together with a deliberate
 * pricing change: temporarily point this test's writer at the current
 * resolutions, or re-run the generator from the pre-split commit.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { getModelPricingWithSource } from '../../src/web-server/model-pricing';
import {
  MODEL_PRICING_ALIASES,
  PRICING_REGISTRY,
} from '../../src/web-server/model-pricing-data';

interface GoldenResolution {
  pricing: Record<string, unknown>;
  source: string;
}

interface Golden {
  registryKeys: string[];
  aliasKeys: string[];
  lookups: Record<string, GoldenResolution>;
  tierLookups: Record<string, { fast: GoldenResolution; unknownTier: GoldenResolution }>;
}

const goldenPath = path.join(import.meta.dir, 'model-pricing-table-parity.golden.json');
const golden: Golden = JSON.parse(fs.readFileSync(goldenPath, 'utf8'));

describe('model pricing table parity (U9 split)', () => {
  let originalCcsHome: string | undefined;
  let originalCcsDir: string | undefined;
  let tempRoot: string;

  beforeEach(() => {
    tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pricing-parity-'));
    originalCcsHome = process.env.CCS_HOME;
    originalCcsDir = process.env.CCS_DIR;
    process.env.CCS_HOME = tempRoot;
    delete process.env.CCS_DIR;
  });

  afterEach(() => {
    fs.rmSync(tempRoot, { recursive: true, force: true });
    if (originalCcsHome !== undefined) process.env.CCS_HOME = originalCcsHome;
    else delete process.env.CCS_HOME;
    if (originalCcsDir !== undefined) process.env.CCS_DIR = originalCcsDir;
    else delete process.env.CCS_DIR;
  });

  it('keeps every model id: registry keys and alias keys match the pre-split golden', () => {
    expect([...Object.keys(PRICING_REGISTRY)].sort()).toEqual(golden.registryKeys);
    expect([...Object.keys(MODEL_PRICING_ALIASES)].sort()).toEqual(golden.aliasKeys);
  });

  it('resolves every golden lookup id to the identical pricing and source', () => {
    expect(Object.keys(golden.lookups).length).toBeGreaterThan(600);
    for (const [model, expected] of Object.entries(golden.lookups)) {
      expect(getModelPricingWithSource(model)).toEqual(expected);
    }
  });

  it('resolves every service-tier lookup identically, fast and unknown tiers', () => {
    const tierIds = Object.keys(golden.tierLookups);
    expect(tierIds.length).toBeGreaterThan(0);
    for (const model of tierIds) {
      const expected = golden.tierLookups[model];
      expect(getModelPricingWithSource(model, { serviceTier: 'fast' })).toEqual(expected.fast);
      expect(getModelPricingWithSource(model, { serviceTier: 'no-such-tier' })).toEqual(
        expected.unknownTier
      );
    }
  });

  it('keeps the golden exhaustive: every current registry and alias id is covered', () => {
    const covered = new Set(Object.keys(golden.lookups));
    for (const key of Object.keys(PRICING_REGISTRY)) {
      expect(covered.has(key)).toBe(true);
    }
    for (const key of Object.keys(MODEL_PRICING_ALIASES)) {
      expect(covered.has(key)).toBe(true);
    }
  });
});
