// G14 activity-collection parity: the TS scanner-answer aggregation (toWorkerResult ->
// buildActivityReports) becomes the expected reports the Rust port (collect::worker_from_scan ->
// report::build_activity_reports) reproduces. Pricing resolution stays TS-owned: the generator
// captures the resolved facts (getModelPricingWithSource) the Rust fixture Pricing replays, so the
// comparison pins the aggregation arithmetic, not the registry. Synthetic rows only — every row is
// dated in the past so neither clock filters it. Run:
//   bun tests/spec/analytics-writers/generate-collect-vectors.mjs
import { writeFileSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { toWorkerResult } from '../../../src/web-server/services/analytics-remote-sources.ts';
import { buildActivityReports } from '../../../src/store/activity-reports.ts';
import { getModelPricingWithSource } from '../../../src/web-server/model-pricing.ts';
import { normalizeUsageProvider } from '../../../src/web-server/usage/model-identity.ts';

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, 'collect-vectors.json');

const NOW = Date.parse('2026-11-15T12:00:00Z');
const WINDOW_FROM = NOW - 31 * 86_400_000;
const iso = (ms) => new Date(ms).toISOString();
// Hour labels well in the past (the scanner's `h` is `YYYY-MM-DD HH:00`).
const H = (n) => `2026-11-14 ${String(n).padStart(2, '0')}:00`;
const MS = (n) => Date.parse(`2026-11-14T${String(n).padStart(2, '0')}:00:00Z`);

const row = (k, m, h, over = {}) => ({
  k,
  f: `file-${k}-${h}.jsonl`,
  m,
  h: H(h),
  i: 1000,
  o: 200,
  cr: 300,
  cw: 50,
  c: 0,
  n: 4,
  ...over,
});
const srow = (k, s, m, h, over = {}) => ({
  k,
  f: `file-${k}-${h}.jsonl`,
  s,
  m,
  a: MS(h),
  z: MS(h) + 600_000,
  i: 1000,
  o: 200,
  cr: 300,
  cw: 50,
  c: 0,
  n: 4,
  ...over,
});

const KEY_A = '0123456789abcdef';
const KEY_B = 'fedcba9876543210';

const cases = [
  {
    name: 'claude-anthropic-priced',
    kind: 'claude',
    host: 'ubuntu',
    rows: [
      row('claude', 'claude-sonnet-4-5', 10, { p: 'anthropic' }),
      row('claude', 'claude-sonnet-4-5', 11, { p: 'anthropic', i: 2000, n: 6 }),
      row('claude', 'claude-sonnet-4-5', 10, { p: 'anthropic', f: 'other.jsonl', n: 2 }),
    ],
    srows: [
      srow('claude', KEY_A, 'claude-sonnet-4-5', 10, { p: 'anthropic' }),
      srow('claude', KEY_A, 'claude-sonnet-4-5', 11, { p: 'anthropic', i: 2000, n: 6 }),
    ],
  },
  {
    name: 'codex-no-provider-logged-cost',
    kind: 'codex',
    host: 'mac',
    rows: [
      row('codex', 'gpt-5-codex', 9, { c: 0.05 }),
      row('codex', 'gpt-5-codex', 9, { c: 0.02, f: 'file-codex-9.jsonl' }),
      row('codex', 'gpt-5-codex', 12),
    ],
    srows: [srow('codex', KEY_B, 'gpt-5-codex', 9, { c: 0.07 })],
  },
  {
    name: 'unknown-model-fallback',
    kind: 'cursor',
    host: 'ubuntu',
    rows: [
      row('cursor', 'mystery-model-xyz', 14, { p: 'anthropic', i: 5000, o: 1000 }),
      row('cursor', 'mystery-model-xyz', 15, { p: 'anthropic' }),
    ],
    srows: [srow('cursor', KEY_A, 'mystery-model-xyz', 14, { p: 'anthropic', i: 5000, o: 1000 })],
  },
  {
    name: 'reported-and-unknown-basis',
    kind: 'cursor',
    host: 'windows',
    rows: [
      row('cursor', 'claude-sonnet-4-5', 8, { p: 'anthropic', c: 0.1, costBasis: 'reported' }),
      row('cursor', 'claude-sonnet-4-5', 8, { p: 'anthropic', c: 0, costBasis: 'reported' }),
      row('cursor', 'gpt-5-codex', 9, { p: 'openai', costBasis: 'unknown', i: 3000 }),
    ],
    srows: [
      srow('cursor', KEY_B, 'claude-sonnet-4-5', 8, { p: 'anthropic', c: 0.1 }),
      srow('cursor', KEY_B, 'gpt-5-codex', 9, { p: 'openai', i: 3000 }),
    ],
  },
  {
    name: 'mixed-providers-sessions',
    kind: 'claude',
    host: 'ubuntu',
    rows: [
      row('claude', 'claude-sonnet-4-5', 10, { p: 'anthropic' }),
      row('claude', 'gpt-5-codex', 10, { p: 'openai', f: 'file-claude-10.jsonl' }),
      row('claude', 'claude-sonnet-4-5', 11, { p: 'anthropic', f: 'file-claude-11.jsonl' }),
    ],
    srows: [
      srow('claude', KEY_A, 'claude-sonnet-4-5', 10, { p: 'anthropic' }),
      srow('claude', KEY_A, 'gpt-5-codex', 10, { p: 'openai', f: 'file-claude-10.jsonl' }),
      srow('claude', KEY_B, 'claude-sonnet-4-5', 11, { p: 'anthropic', f: 'file-claude-11.jsonl' }),
    ],
  },
];

// The pricing facts the Rust fixture replays: every (model, normalized provider) the aggregation
// queries, plus the no-provider form. Resolved once here, TS-owned.
const models = ['claude-sonnet-4-5', 'gpt-5-codex', 'mystery-model-xyz'];
const providers = ['anthropic', 'openai', 'google', undefined];
const facts = {};
for (const model of models)
  for (const provider of providers) {
    const resolved = getModelPricingWithSource(model, { provider });
    facts[`${model}\u0000${provider ?? ''}`] = {
      rates: {
        input: resolved.pricing.inputPerMillion,
        output: resolved.pricing.outputPerMillion,
        cacheCreation: resolved.pricing.cacheCreationPerMillion,
        cacheRead: resolved.pricing.cacheReadPerMillion,
      },
      source: resolved.source,
    };
  }

const vectors = {
  note: 'G14 activity-collection parity: TS aggregation + fed pricing facts (synthetic rows only).',
  now: NOW,
  windowFromMs: WINDOW_FROM,
  facts,
  cases: cases.map((c) => {
    const worker = toWorkerResult(c.rows, c.srows, c.kind, c.host);
    const reports = buildActivityReports({
      local: new Map(),
      remote: [{ tool: c.kind, host: c.host, data: worker }],
      states: [{ tool: c.kind, host: c.host, state: 'ok', lastScanAt: iso(NOW) }],
      windowFromMs: WINDOW_FROM,
      now: NOW,
      localComplete: () => false,
      scannerOwned: new Set(),
    });
    return {
      name: c.name,
      kind: c.kind,
      host: c.host,
      rows: c.rows,
      srows: c.srows,
      expected: reports,
    };
  }),
};

const json = `${JSON.stringify(vectors, null, 2)}\n`;
if (process.argv.includes('--check')) {
  if (readFileSync(out, 'utf8') !== json) {
    console.error('collect-vectors.json is stale; regenerate without --check');
    process.exit(1);
  }
  console.log('collect-vectors.json up to date');
} else {
  writeFileSync(out, json);
  console.log(`wrote ${out}: ${cases.length} cases, ${Object.keys(facts).length} pricing facts`);
}