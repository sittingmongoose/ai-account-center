// The Details fact groups the frame draws: reset rows from the usage reading, Reading, and Sign-ins.
import test from 'node:test';
import assert from 'node:assert/strict';
import { isResetWindow, resetRowsOf, resetsMode } from '../public/reset-rows.mjs';
import { detailsViewModel, readingGroup } from '../public/view-model.mjs';
import { claudeSignInGroup } from '../public/claude-signins.mjs';

const NOW = Date.parse('2026-11-11T14:47:00Z');
const H = 3600 * 1000;
const iso = (ms) => new Date(ms).toISOString();
const claude = { id: 'claude:party', provider: 'claude' };

test('resets work differently per provider', () => {
  assert.equal(resetsMode('claude'), 'claim');
  assert.equal(resetsMode('codex'), 'claim');
  assert.equal(resetsMode('zai'), 'zai');
  assert.equal(resetsMode('qwen'), 'qwen');
  assert.equal(resetsMode('cursor'), '');
});

test('each Claude grant is its own row; the total is not a row', () => {
  const windows = [
    { key: 'reset_credits_available', kind: 'balance', remaining: 2, unit: 'resets' },
    { key: 'reset_credit_available_grant_1', kind: 'balance', remaining: 1, limit: 1, unit: 'resets', expiresAt: iso(NOW + 29 * 24 * H), clears: ['seven_day', 'seven_day_overage_included'] },
    { key: 'reset_credit_used_grant_3', kind: 'balance', remaining: 0, limit: 1, unit: 'resets', expiresAt: iso(NOW + 118 * 24 * H), clears: ['seven_day_overage_included'] },
    { key: 'reset_credit_saved_grant_4', kind: 'balance', remaining: 1, limit: 1, unit: 'resets' },
    { key: 'reset_credit_available_grant_5', kind: 'balance', remaining: null, unit: 'resets' },
    { key: 'extra_usage', kind: 'extra_usage', enabled: false },
  ];
  const rows = resetRowsOf(claude, windows);
  assert.deepEqual(
    rows.map((r) => [r.state, r.refills, r.left]),
    [
      ['ready', 'Weekly, Fable included usage', 1],
      ['used', 'Fable included usage', 0],
      ['saved', '', 1],
    ],
    'a grant with no count is left out, never shown as zero'
  );
  assert.equal(rows[0].expiresAtMs, NOW + 29 * 24 * H);
  assert.equal(rows[2].expiresAtMs, null);
  assert.equal(isResetWindow(claude, windows[0]), true);
  assert.equal(isResetWindow(claude, windows[5]), false);
});

test('Codex banked groups and Z.ai cards become rows; nothing in them can be used', () => {
  const codex = resetRowsOf({ id: 'codex:a', provider: 'codex' }, [
    { key: 'banked_resets', kind: 'balance', remaining: 2, unit: 'resets', expiresAt: iso(NOW + 5 * 24 * H) },
    { key: 'banked_resets_2', kind: 'balance', remaining: 0, unit: 'resets' },
  ]);
  assert.deepEqual(codex.map((r) => [r.state, r.left, r.count]), [['ready', 2, 2]]);
  const zai = resetRowsOf({ id: 'zai:a', provider: 'zai' }, [
    { key: 'reset-packs-5h', kind: 'balance', remaining: 2, used: 1, limit: 3, unit: 'packs', expiresAt: iso(NOW + H) },
    { key: 'reset-packs-weekly', kind: 'balance', remaining: 0, used: 0, limit: 0, unit: 'packs' },
  ]);
  assert.deepEqual(zai.map((r) => [r.refills, r.state, r.count]), [['5-hour quota', 'ready', 2], ['5-hour quota', 'used', 1]]);
  for (const row of [...codex, ...zai]) assert.equal('grantKey' in row, false);
});

test('Reading says when, the status as the answer, and each fact once', () => {
  const account = {
    provider: 'claude', status: 'cached', sampledAt: iso(NOW - 60_000), fetchedAt: iso(NOW - 50_000),
    source: 'Verified quota', host: 'windows', message: 'Showing the last successful reading; live usage is unavailable.',
  };
  const g = readingGroup(account, 'party', NOW);
  assert.equal(g.lead, 'Cached.');
  assert.equal(g.rest, 'Showing the last successful reading; live usage is unavailable.');
  assert.match(g.head, /, 1m ago$/);
  assert.deepEqual(g.rows.map((r) => [r.label, r.value]), [['Read from', 'Verified quota, on Windows'], ['Profile', 'party']]);
  // Fetched shows only when it differs from Sampled
  const later = readingGroup({ ...account, fetchedAt: iso(NOW) , message: '' }, '', NOW);
  assert.deepEqual(later.rows.map((r) => r.label), ['Read from']);
  const apart = readingGroup({ ...account, fetchedAt: iso(NOW + 10 * 60_000) }, '', NOW);
  assert.deepEqual(apart.rows.map((r) => r.label), ['Read from', 'Fetched']);
  assert.equal(readingGroup({ status: 'ok', host: 'mac' }, '', NOW).rest, 'Read from the provider at the last refresh.');
});

const renewal = (signIns, extra = {}) => ({
  runner: 'mac', generatedAt: iso(NOW - 60_000), stale: false, needsYou: [], hosts: [], signIns,
  browserSessions: [{ email: 'a@example.com', signedIn: true, sessionEndsAt: iso(NOW + 26 * 24 * H) }],
  ...extra,
});
const signIn = (host, profile, days, extra = {}) => ({
  host, profile, email: 'a@example.com', state: 'ok', signInEndsAt: iso(NOW + days * 24 * H),
  renewFrom: iso(NOW + (days - 4) * 24 * H), autoRenew: true, ...extra,
});

test('Sign-ins: a table per computer, soonest end first, and the answer in a sentence', () => {
  const data = { claudeSignIns: renewal([signIn('nas1', 'party', 21), signIn('windows', 'party', 19), signIn('mac', 'party', 19.5)]) };
  const g = claudeSignInGroup(data, 'A@example.com', NOW);
  assert.equal(g.head, '3 of 3 good');
  assert.equal(g.lead, 'Renewed automatically');
  assert.match(g.rest, /^by the runner on the Mac\. The first ends \w+ \d+ and renews from \w+ \d+\.$/);
  assert.deepEqual(g.rows.map((r) => r.computer), ['Windows', 'Mac', 'Nas1']);
  assert.ok(g.rows.every((r) => r.problem === '' && r.profile === 'party'));
  assert.match(g.browserSession, /^Signed in until /);
});

test('Sign-ins: a problem marks its own row and comes first', () => {
  const data = {
    claudeSignIns: renewal([signIn('windows', 'party', 19), signIn('mac', 'party', 25, { state: 'logged_out' }), signIn('nas1', 'default', 20, { autoRenew: false })]),
  };
  const g = claudeSignInGroup(data, 'a@example.com', NOW);
  assert.equal(g.head, '2 of 3 good');
  assert.equal(g.lead, '1 of 3 needs a look.');
  assert.deepEqual(g.rows.map((r) => [r.computer, r.problem]), [['Mac', 'Logged out'], ['Windows', ''], ['Nas1', '']]);
  assert.equal(g.rows[2].renews, 'Not automatic');
  assert.equal(claudeSignInGroup({}, 'a@example.com', NOW), null);
});

test('Details carries the groups beside the old fields', () => {
  const data = {
    accounts: [{
      id: 'claude:party', provider: 'claude', email: 'a@example.com', status: 'ok', sampledAt: iso(NOW - 60_000), host: 'windows',
      windows: [
        { key: 'reset_credits_available', kind: 'balance', remaining: 1, unit: 'resets' },
        { key: 'reset_credit_available_grant_1', kind: 'balance', remaining: 1, limit: 1, unit: 'resets', expiresAt: iso(NOW + 24 * H), clears: ['seven_day'] },
        { key: 'extra_usage', label: 'Extra usage', kind: 'extra_usage', enabled: false },
      ],
    }],
  };
  const d = detailsViewModel(data, 'claude:party', { now: NOW });
  assert.equal(d.resetsMode, 'claim');
  assert.deepEqual(d.resetRows.map((r) => r.refills), ['Weekly']);
  assert.deepEqual(d.balances.map((b) => b.label), ['Extra usage']);
  assert.equal(d.amounts.length, 3, 'the old amounts list is unchanged');
  assert.equal(d.reading.lead, 'Live.');
  assert.equal(d.signIns, null);
});
