/**
 * G14 parity vectors: the TS observation/whitelist answers the Rust port
 * (crates/aac-store/src/observe.rs) must reproduce. Synthetic rows only.
 *   bun tests/spec/analytics-writers/generate-vectors.mjs [--check]
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  analyticsWindow,
  accountAnalyticsIdentity,
  analyticsTimestamp,
  analyticsWindowIdentity,
  sourcePlatform,
  decodeAccountAnalyticsObservation,
} from '../../../src/web-server/services/account-analytics-history';
import {
  storeWindow,
  storeObservationFromAccount,
  storeObservationFromRecord,
} from '../../../src/store/usage-observations';
import { recordsFromText } from '../../../src/store/usage-backfill';

const here = path.dirname(fileURLToPath(import.meta.url));
const NOW = Date.parse('2026-11-14T21:10:00.000Z');
const DAY = 86_400_000;
const RETENTION = 90 * DAY;

const window = (overrides = {}) => ({
  key: 'primary',
  label: 'Primary',
  usedPercent: 33.5,
  remainingPercent: 66.5,
  resetAt: '2026-11-15T02:00:00.000Z',
  windowMinutes: 300,
  used: null,
  limit: null,
  unit: 'percent',
  kind: 'rate_limit',
  ...overrides,
});

const account = (overrides = {}) => ({
  id: 'codex:work',
  provider: 'codex',
  email: 'Work@Example.com',
  plan: 'pro',
  platform: 'mac',
  source: 'Codex on Mac',
  status: 'ok',
  sampledAt: new Date(NOW - 60_000).toISOString(),
  fetchedAt: new Date(NOW - 60_000).toISOString(),
  isActive: true,
  windows: [window()],
  ...overrides,
});

const windowCases = [
  window(),
  window({ kind: 'balance', unit: 'usd', used: 1.25, limit: 20, remaining: 18.75 }),
  window({ kind: 'spend' }),
  window({ kind: 'extra_usage' }),
  window({ kind: 'unknown-kind' }),
  window({ kind: 5 }),
  window({ kind: null }),
  window({ usedPercent: -1, remainingPercent: -0.5 }),
  window({ usedPercent: 150 }),
  window({ usedPercent: Number.NaN }),
  window({ usedPercent: '50' }),
  window({ windowMinutes: Number.POSITIVE_INFINITY }),
  window({ resetAt: '2026-11-15T02:00:00+05:30' }),
  window({ resetAt: 'not-a-date' }),
  window({ resetAt: '2026-11-15' }),
  window({ expiresAt: '2026-12-01T00:00:00.000Z' }),
  window({ status: 'cached', sampledAt: new Date(NOW - DAY).toISOString() }),
  window({ status: 'cached' }),
  window({ status: 'cached', sampledAt: 'bad' }),
  window({ status: 'ok', sampledAt: new Date(NOW - DAY).toISOString() }),
  window({ clears: ['five_hour', 'weekly'] }),
  window({ clears: [] }),
  window({ clears: ['Five_Hour'] }),
  window({ clears: ['a', 'a'] }),
  window({ clears: Array.from({ length: 9 }, (_, i) => `slug${i}`) }),
  window({ clears: 'five_hour' }),
  window({ clears: [5] }),
  window({ unlimited: true, enabled: false }),
  window({ unlimited: 'yes', enabled: 1 }),
  window({ poolId: 'pool-1', poolIdSource: 'provider-id', poolLabel: ' Team pool ' }),
  window({ poolId: 'pool_1@x/y:z', poolIdSource: 'provider-bucket-membership' }),
  window({ poolId: '-bad', poolIdSource: 'other' }),
  window({ poolId: 'x'.repeat(129) }),
  window({ poolLabel: 'Bearer secret-token' }),
  window({ poolLabel: 'sk-abcdefgh12345678' }),
  window({ poolLabel: 'https://example.test' }),
  window({ poolLabel: '{json}' }),
  window({ poolLabel: '  ' }),
  window({ modelIds: ['gpt-5.1-codex', 'claude-sonnet-4-5', 'bad model!', '-lead', 'x'.repeat(161), 5, null] }),
  window({ modelIds: [] }),
  window({ modelIds: 'gpt-5' }),
  window({ key: '' }),
  window({ label: 'x'.repeat(81) }),
  window({ key: 'k'.repeat(64), label: 'l'.repeat(80) }),
  window({ label: 'bad\u0007label' }),
  window({ unit: 'u'.repeat(41) }),
];

const accountCases = [
  { account: account(), now: NOW, retentionMs: RETENTION },
  { account: account({ email: null }), now: NOW, retentionMs: RETENTION },
  { account: account({ email: undefined }), now: NOW, retentionMs: RETENTION },
  { account: account({ email: 'mixed@Case.TEST' }), now: NOW, retentionMs: RETENTION },
  { account: account({ status: 'error', windows: [window()] }), now: NOW, retentionMs: RETENTION },
  { account: account({ status: 'needs_sign_in' }), now: NOW, retentionMs: RETENTION },
  { account: account({ status: 'cached', windows: [window({ status: 'cached', sampledAt: new Date(NOW - 2 * DAY).toISOString() })] }), now: NOW, retentionMs: RETENTION },
  { account: account({ windows: [window({ status: 'cached', sampledAt: new Date(NOW + DAY).toISOString() })] }), now: NOW, retentionMs: RETENTION },
  { account: account({ windows: [window({ status: 'cached', sampledAt: new Date(NOW - 100 * DAY).toISOString() })] }), now: NOW, retentionMs: RETENTION },
  { account: account({ sampledAt: new Date(NOW + 60_000).toISOString() }), now: NOW, retentionMs: RETENTION },
  { account: account({ sampledAt: new Date(NOW - 100 * DAY).toISOString() }), now: NOW, retentionMs: RETENTION },
  { account: account({ sampledAt: undefined, fetchedAt: new Date(NOW - 120_000).toISOString() }), now: NOW, retentionMs: RETENTION },
  { account: account({ sampledAt: 'bad', fetchedAt: 'bad' }), now: NOW, retentionMs: RETENTION },
  { account: account({ status: 'error', sampledAt: 'bad' }), now: NOW, retentionMs: RETENTION },
  { account: account({ host: 'nas1', platform: 'mac' }), now: NOW, retentionMs: RETENTION },
  { account: account({ host: 'alexis', platform: 'mac' }), now: NOW, retentionMs: RETENTION },
  { account: account({ host: 'windows', platform: 'mac' }), now: NOW, retentionMs: RETENTION },
  { account: account({ host: 'unknown-host', source: 'Codex on Windows' }), now: NOW, retentionMs: RETENTION },
  { account: account({ source: 'Claude on Mac' }), now: NOW, retentionMs: RETENTION },
  { account: account({ id: 'x'.repeat(200), plan: 'p'.repeat(120), source: 's'.repeat(200) }), now: NOW, retentionMs: RETENTION },
  { account: account({ plan: '' }), now: NOW, retentionMs: RETENTION },
  { account: account({ isActive: false }), now: NOW, retentionMs: RETENTION },
  { account: account({ windows: Array.from({ length: 300 }, (_, i) => window({ key: `w${i}` })) }), now: NOW, retentionMs: RETENTION },
  { account: account({ windows: [window({ key: '' }), window()] }), now: NOW, retentionMs: RETENTION },
];

const record = (overrides = {}) => ({
  identity: accountAnalyticsIdentity(account()),
  accountId: 'codex:work',
  sampledAt: new Date(NOW - DAY).toISOString(),
  observedAt: new Date(NOW - DAY + 60_000).toISOString(),
  platform: 'mac',
  source: 'Codex on Mac',
  status: 'ok',
  isActive: true,
  windows: [window()],
  ...overrides,
});

const recordCases = [
  { record: record(), provider: 'codex', importRef: 'quota-history-v1.json#0' },
  { record: record({ windows: [window({ status: 'cached', sampledAt: new Date(NOW - 2 * DAY).toISOString() }), window({ key: '' })] }), provider: 'codex', importRef: 'archive/a.json#3' },
  { record: record({ status: 'error', isActive: false }), provider: 'unknown', importRef: 'x#1' },
  { record: record({ identity: 'ABCDEF' + '0'.repeat(58) }), provider: 'codex', importRef: 'x#2' },
  { record: record({ sampledAt: '2026-11-13' }), provider: 'codex', importRef: 'x#3' },
  { record: record({ platform: 'linux' }), provider: 'codex', importRef: 'x#4' },
  { record: record({ status: 'bogus' }), provider: 'codex', importRef: 'x#5' },
  { record: record({ accountId: '' }), provider: 'codex', importRef: 'x#6' },
];

const textCases = [
  '',
  '   ',
  '[]',
  '{"records": [1, 2]}',
  '{"schemaVersion": 1, "records": []}',
  '{"identity": "x"}',
  '{"a": 1}\n{"b": 2}\n',
  '{"a": 1}\nnot json\n{"b": 2}',
  'not json at all',
  '"scalar"',
  '42',
  '{"records": "nope"}',
];
const vectors = {
  note: 'G14 observe-port parity: TS answers for the Rust port (synthetic rows only).',
  now: NOW,
  windows: windowCases.map((input) => ({
    input,
    expected: analyticsWindow(input),
    stored: storeWindow(input),
    identity: analyticsWindow(input) ? analyticsWindowIdentity(analyticsWindow(input)) : null,
  })),
  accounts: accountCases.map(({ account: row, now, retentionMs }) => ({
    account: row,
    now,
    retentionMs,
    expected: storeObservationFromAccount(row, now, retentionMs),
    identity: accountAnalyticsIdentity(row),
    platform: sourcePlatform(row),
  })),
  records: recordCases.map(({ record: row, provider, importRef }) => {
    const decoded = decodeAccountAnalyticsObservation(row);
    return {
      record: row,
      provider,
      importRef,
      decoded: decoded
        ? {
            identity: decoded.identity,
            accountId: decoded.accountId,
            sampledAt: decoded.sampledAt,
            observedAt: decoded.observedAt,
            platform: decoded.platform,
            source: decoded.source,
            status: decoded.status,
            isActive: decoded.isActive,
            windows: decoded.windows,
          }
        : null,
      expected: decoded ? storeObservationFromRecord(decoded, provider, importRef) : null,
    };
  }),
  texts: textCases.map((text) => ({ text, ...recordsFromText(text) })),
  timestamps: [
    '2026-11-14T21:10:00.000Z',
    '2026-11-14T21:10:00Z',
    '2026-11-15T02:00:00+05:30',
    '2026-11-14',
    '2026-11-14T25:00:00Z',
    'not-a-date',
    '',
  ].map((value) => ({ value, expected: analyticsTimestamp(value) })),
};

const file = path.join(here, 'vectors.json');
const body = JSON.stringify(vectors, null, 1) + '\n';
if (process.argv.includes('--check')) {
  if (fs.readFileSync(file, 'utf8') !== body) {
    console.error('vectors.json differs from the TS answers; regenerate');
    process.exit(1);
  }
  console.log('vectors.json matches the TS answers');
} else {
  fs.writeFileSync(file, body);
  console.log(
    `wrote ${file}: ${vectors.windows.length} windows, ${vectors.accounts.length} accounts, ${vectors.records.length} records, ${vectors.texts.length} texts`,
  );
}