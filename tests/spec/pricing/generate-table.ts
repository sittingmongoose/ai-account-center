/**
 * Generates the Rust pricing table (crates/aac-views/src/pricing/data.json) from the TypeScript
 * table (src/web-server/model-pricing-data.ts), so gap G15's Rust resolver reads rates that are
 * byte-identical doubles by construction. Re-run only together with a deliberate TS table change
 * (the U9 golden test and the G15 parity corpus both pin the lookups):
 *
 *   bun tests/spec/pricing/generate-table.ts
 */
import * as fs from 'fs';
import * as path from 'path';
import { MODEL_PRICING_ALIASES, PRICING_REGISTRY } from '../../../src/web-server/model-pricing-data';

const out = path.join(import.meta.dir, '../../../crates/aac-views/src/pricing/data.json');
const data = JSON.stringify({ registry: PRICING_REGISTRY, aliases: MODEL_PRICING_ALIASES });
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, data);
console.log(`wrote ${out} (${Object.keys(PRICING_REGISTRY).length} models)`);
