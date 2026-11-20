// G14 quota-sampler end-to-end parity: the TS writer (backfillUsageHistory + the sample() dedupe +
// storeObservationFromAccount + store.record) runs against the in-process NAPI store (the same Rust
// Engine the port uses), and its historyRecords become the expected answer the Rust sampler test
// reproduces on a fresh Engine. Synthetic accounts and archive texts only — no provider calls, no
// real roots or credentials. Run: bun tests/spec/analytics-writers/generate-sampler-vectors.mjs
import { writeFileSync, readFileSync, mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '../../..');
const out = join(here, 'sampler-vectors.json');
process.env.AAC_STORE_MODULE = join(repo, 'dist/native/aac-store.win32-x64.node');
process.env.AAC_STORE = 'on';

const { openAacStore, closeAacStore } = await import('../../../src/store/store-service.ts');
const { backfillUsageHistory } = await import('../../../src/store/usage-backfill.ts');
const { storeObservationFromAccount } = await import('../../../src/store/usage-observations.ts');
const { accountAnalyticsIdentity } = await import(
  '../../../src/web-server/services/account-analytics-history.ts'
);

const NOW = Date.parse('2026-11-15T12:00:00Z');
const iso = (ms) => new Date(ms).toISOString();
const sha256 = (text) => createHash('sha256').update(Buffer.from(text, 'utf8')).digest('hex');
const T = (mins) => iso(NOW - mins * 60_000);

// A valid dashboard account with one rate-limit window (key and label are whitelist-required).
const account = (provider, id, over = {}) => ({
  provider,
  id,
  email: `${id}@example.test`,
  status: 'ok',
  sampledAt: T(10),
  fetchedAt: T(10),
  source: 'cli',
  isActive: true,
  host: 'mac',
  windows: [
    {
      key: 'five_hour',
      label: '5-hour limit',
      kind: 'rate_limit',
      windowMinutes: 300,
      usedPercent: 42.5,
      resetAt: T(-60),
    },
  ],
  ...over,
});

// A valid stored observation record (the JSON history's shape).
const record = (provider, id, sampledMins, over = {}) => ({
  identity: accountAnalyticsIdentity(account(provider, id)),
  accountId: `${provider}:${id}`,
  sampledAt: T(sampledMins),
  observedAt: T(sampledMins),
  platform: 'mac',
  status: 'ok',
  source: 'cli',
  isActive: true,
  windows: [
    {
      key: 'five_hour',
      label: '5-hour limit',
      kind: 'rate_limit',
      windowMinutes: 300,
      usedPercent: 30,
      resetAt: T(sampledMins - 60),
    },
  ],
  ...over,
});

const mac = {
  accounts: [
    account('claude', 'alpha', { sampledAt: T(30), fetchedAt: T(30) }),
    account('codex', 'beta', { status: 'cached', sampledAt: T(20) }),
    account('gemini', 'gamma', { status: 'error', sampledAt: T(15) }),
  ],
};
const windows = {
  accounts: [
    // Same identity as mac alpha, newer and available: wins.
    account('claude', 'alpha', { sampledAt: T(5), fetchedAt: T(5), host: 'windows' }),
    // Same identity as mac beta, unavailable: loses to the cached reading.
    account('codex', 'beta', { status: 'unavailable', sampledAt: T(1), host: 'windows' }),
    account('qwen', 'delta', { sampledAt: T(8), host: 'windows' }),
  ],
};
const dashboards = [mac, windows];

// The sample() dedupe, exactly as account-analytics-service.ts runs it.
function merge(dashboards) {
  const unique = new Map();
  for (const dashboard of dashboards)
    for (const account of dashboard.accounts) {
      const identity = accountAnalyticsIdentity(account);
      const previous = unique.get(identity);
      const sampled = Date.parse(account.sampledAt ?? account.fetchedAt ?? '');
      const oldSampled = previous
        ? Date.parse(previous.sampledAt ?? previous.fetchedAt ?? '')
        : -Infinity;
      const available = account.status === 'ok' || account.status === 'cached';
      const previouslyAvailable = previous?.status === 'ok' || previous?.status === 'cached';
      if (
        !previous ||
        (available && !previouslyAvailable) ||
        (available === previouslyAvailable &&
          Number.isFinite(sampled) &&
          (!Number.isFinite(oldSampled) || sampled >= oldSampled))
      )
        unique.set(identity, account);
    }
  return [...unique.values()];
}

// Archive texts: JSONL with a mix of valid and refused records; one object-shaped file.
const archiveA = [
  JSON.stringify(record('claude', 'alpha', 200)),
  JSON.stringify(record('claude', 'alpha', 180)),
  JSON.stringify({ nope: true }), // refused: no identity
  JSON.stringify(record('codex', 'beta', 170)),
].join('\n');
const archiveB = JSON.stringify([
  record('gemini', 'gamma', 160),
  record('qwen', 'delta', 150),
  { identity: 'short', accountId: 'x' }, // refused: bad identity shape
]);
const mainRecords = [record('claude', 'alpha', 140), record('codex', 'beta', 130)];
const mainText = `${JSON.stringify(
  { schemaVersion: 1, collectedSince: T(200), records: mainRecords },
  null,
  2
)}\n`;

const ccsDir = join(tmpdir(), `aac-g14-ccs-${process.pid}`);
const storeDir = join(tmpdir(), `aac-g14-store-${process.pid}`);
mkdirSync(join(ccsDir, 'account-analytics', 'archive', 'nested'), { recursive: true });
mkdirSync(storeDir, { recursive: true });
const archiveDir = join(ccsDir, 'account-analytics', 'archive');
writeFileSync(join(archiveDir, 'a.jsonl'), archiveA);
writeFileSync(join(archiveDir, 'nested', 'b.json'), archiveB);
writeFileSync(join(ccsDir, 'account-analytics', 'quota-history-v1.json'), mainText);

const opened = await openAacStore({ dir: storeDir });
if (!opened.store) throw new Error(`store did not open: ${opened.reason}`);
const store = opened.store;

const merged = merge(dashboards);
const providers = new Map(merged.map((a) => [accountAnalyticsIdentity(a), a.provider]));
const report = await backfillUsageHistory(store, ccsDir, providers);
const observations = merged
  .slice(0, 64)
  .map((a) => storeObservationFromAccount(a, NOW, Number.POSITIVE_INFINITY))
  .filter(Boolean);
await store.record(observations);
const history = await store.historyRecords({
  fromMs: 0,
  toMs: NOW + 5 * 60_000,
  nowMs: NOW,
  thinning: { kind: 'history' },
});

// The sources the Rust port reconstructs (label, content hash, raw text).
const sources = [
  { label: 'archive/a.jsonl', sha256: sha256(archiveA), text: archiveA },
  { label: 'archive/nested/b.json', sha256: sha256(archiveB), text: archiveB },
  { label: 'quota-history-v1.json', sha256: sha256(mainText), text: mainText },
];

const vectors = {
  note: 'G14 sampler end-to-end parity: TS writer + NAPI Engine answers (synthetic only).',
  now: NOW,
  dashboards,
  sources,
  expectedMerged: merged.map((a) => accountAnalyticsIdentity(a)),
  expectedReport: report,
  expectedHistory: history,
};

await closeAacStore();
rmSync(ccsDir, { recursive: true, force: true });
rmSync(storeDir, { recursive: true, force: true });

const json = `${JSON.stringify(vectors, null, 2)}\n`;
if (process.argv.includes('--check')) {
  if (readFileSync(out, 'utf8') !== json) {
    console.error('sampler-vectors.json is stale; regenerate without --check');
    process.exit(1);
  }
  console.log('sampler-vectors.json up to date');
} else {
  writeFileSync(out, json);
  console.log(
    `wrote ${out}: ${merged.length} merged, ${history.length} history records, report ${JSON.stringify(report)}`
  );
}