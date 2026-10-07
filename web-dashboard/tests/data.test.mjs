import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { usageView, dashboardView, allDetailWindows, timeLabel } from '../public/accounts-data.mjs';
test('compact reset captions preserve the local date, exact minute and timezone through daylight saving time', () => {
  const moduleUrl = new URL('../public/accounts-data.mjs', import.meta.url).href;
  const child = spawnSync(process.execPath, ['--input-type=module', '-e', `
    import { usageView } from ${JSON.stringify(moduleUrl)};
    console.log(JSON.stringify(['2026-10-02T05:30:00Z', '2026-11-02T06:30:00Z'].map(resetAt => usageView({ resetAt }))));
  `], { encoding: 'utf8', env: { ...process.env, TZ: 'America/New_York' } });
  assert.equal(child.status, 0, child.stderr);
  const [summer, winter] = JSON.parse(child.stdout);
  assert.equal(summer.resetCompact, 'Resets Oct 2\n1:30 AM EDT');
  assert.equal(winter.resetCompact, 'Resets Nov 2\n1:30 AM EST');
  assert.match(summer.reset, /Oct 2, 1:30 AM EDT$/);
  assert.match(winter.reset, /Nov 2, 1:30 AM EST$/);
});
test('missing and invalid reset dates do not invent a compact caption or timezone', () => {
  for (const resetAt of [undefined, null, '', 'not-a-date']) {
    const view = usageView({ resetAt });
    assert.equal(view.reset, '');
    assert.equal(view.resetCompact, '');
  }
});
test('unknown quota never becomes genuine zero', () => {
  assert.equal(usageView({ usedPercent: null }).hasPercent, false);
  assert.equal(usageView({ usedPercent: 0 }).amount, '0% used');
  for (const value of [NaN, Infinity, -1]) assert.equal(usageView({ usedPercent: value }).hasPercent, false);
});
test('reported overage stays visible without changing the raw percentage', () => {
  const view = usageView({ usedPercent: 129.6, used: 648, limit: 500, unit: 'requests' });
  assert.equal(view.hasPercent, true);
  assert.equal(view.percent, 129.6);
  assert.equal(view.amount, '648 / 500 requests used · 129.6% used');
  assert.equal(usageView({ usedPercent: 129.6 }, true).amount, '129.6% used');
  assert.equal(usageView({ remainingPercent: 101 }).hasPercent, false);
  const auto = dashboardView({ accounts: [], codexAutoSwitch: { thresholdPercent: 101 } });
  assert.equal(auto.autoAvailable, false);
});
test('balances preserve units and expiration separately from resets', () => {
  const view = usageView({ kind: 'balance', label: 'Credits', remaining: 62500, unit: 'credits', expiresAt: '2026-11-01T00:00:00Z' });
  assert.equal(view.amount, '62,500 credits remaining'); assert.equal(view.hasPercent, false);
  assert.equal(view.reset, ''); assert.match(view.expiration, /^Expires /);
});
test('percentage-only extra usage remains visible with its actual expiration', () => {
  const view = usageView({ kind: 'extra_usage', label: 'Extra usage', usedPercent: 112.25, expiresAt: '2026-11-01T00:00:00Z' });
  assert.equal(view.amount, '112.25% used');
  assert.equal(view.hasPercent, true);
  assert.equal(view.percent, 112.25);
  assert.match(view.expiration, /Expires/);
  assert.equal(view.reset, '');
});
test('extra usage, enabled and unlimited values survive without percentage', () => {
  const view = usageView({ kind: 'extra_usage', used: 32, unit: 'requests', enabled: false, unlimited: true });
  assert.equal(view.amount, '32 requests used'); assert.equal(view.meta, 'Unlimited · Disabled'); assert.equal(view.hasPercent, false);
});
test('Codex core displays percent only while details preserve every actual window', () => {
  const account = { id: 'codex:gmail', provider: 'codex', email: 'a@example.test', status: 'ok', capabilities: { codexProfile: 'gmail' }, windows: [
    { key: 'five_hour', label: '5-hour', usedPercent: 92, used: 32, limit: 50, resetAt: '2026-10-01T12:00:00Z' },
    { key: 'seven_day', label: 'Weekly', usedPercent: 24 }, { key: 'balance', label: 'Credits', kind: 'balance', remaining: 62500, unit: 'credits' },
  ] };
  const data = { accounts: [account], codexAutoSwitch: { enabled: true, thresholdPercent: 5, pollIntervalSeconds: 60 } };
  const view = dashboardView(data); assert.equal(view.codex[0].five.amount, '92% used');
  assert.equal(view.autoSetting, '95% used · 60s'); assert.equal(allDetailWindows(data, account.id).length, 3);
});
test('full Codex account leaves missing canonical five-hour usage empty while retaining additional 300-minute limits in Details', () => {
  const account = {
    id: 'codex:party', provider: 'codex', providerLabel: 'Codex', label: 'Party',
    email: 'party@example.test', plan: 'Pro', platform: 'ubuntu', source: 'Native Codex quota',
    status: 'ok', message: null, fetchedAt: '2026-10-01T15:15:19Z', sampledAt: '2026-10-01T15:15:19Z', isActive: true,
    windows: [
      { key: 'seven_day', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 24, remainingPercent: 76, resetAt: '2026-10-08T12:00:00Z', windowMinutes: 10080, used: null, limit: null, unit: null },
      { key: 'extra_additional_1', label: 'Additional rate limit', kind: 'rate_limit', usedPercent: 92.125, remainingPercent: 7.875, resetAt: '2026-10-01T17:00:00Z', windowMinutes: 300, used: null, limit: null, unit: null },
    ],
    capabilities: { codexProfile: 'party', claudeProfileId: null, claudePlatforms: [] },
  };
  const raw = structuredClone(account);
  const data = { schemaVersion: 1, updatedAt: '2026-10-01T15:15:19Z', settings: { refreshIntervalSeconds: 60 }, accounts: [account],
    codexAutoSwitch: { enabled: true, thresholdPercent: 5, pollIntervalSeconds: 60, outcome: 'healthy', message: 'Configured account switching is enabled.', activationInProgress: false } };
  assert.equal(account.windows.some(window => window.key === 'five_hour'), false);
  const view = dashboardView(data);
  assert.equal(view.codex[0].five.hasPercent, false);
  assert.equal(view.codex[0].five.amount, 'Usage unavailable');
  assert.equal(view.codex[0].five.reset, '');
  assert.equal(view.codex[0].weekly.amount, '24% used');
  assert.equal(view.codex[0].active, true);
  assert.equal(view.activeCodexEmail, account.email);
  assert.equal(view.autoSetting, '95% used · 60s');
  assert.equal(view.thresholdUsed, 95);
  assert.deepEqual(allDetailWindows(data, account.id).map(window => window.label), ['Weekly usage', 'Additional rate limit']);
  assert.equal(allDetailWindows(data, account.id)[1].amount, '92.13% used');
  assert.equal(allDetailWindows(data, account.id)[1].reset, timeLabel(account.windows[1].resetAt, 'Resets'));
  assert.deepEqual(account, raw, 'canonical display selection leaves all raw keys, semantics and precision intact');
});
test('Codex canonical zero quotas win over additional quotas independently of window ordering', () => {
  const account = {
    id: 'codex:lex', provider: 'codex', providerLabel: 'Codex', label: 'Lex',
    email: 'lex@example.test', plan: 'Plus', platform: 'ubuntu', source: 'Native Codex quota',
    status: 'ok', message: null, fetchedAt: '2026-10-01T15:15:19Z', sampledAt: '2026-10-01T15:15:19Z', isActive: false,
    windows: [
      { key: 'extra_additional_1', label: 'Additional 5-hour rate limit', kind: 'rate_limit', usedPercent: 92.125, remainingPercent: 7.875, resetAt: '2026-10-01T17:00:00Z', windowMinutes: 300, used: null, limit: null, unit: null },
      { key: 'extra_additional_2', label: 'Additional weekly rate limit', kind: 'rate_limit', usedPercent: 74, remainingPercent: 26, resetAt: '2026-10-07T12:00:00Z', windowMinutes: 10080, used: null, limit: null, unit: null },
      { key: 'five_hour', label: '5-hour usage', kind: 'rate_limit', usedPercent: 0, remainingPercent: 100, resetAt: '2026-10-01T18:00:00Z', windowMinutes: 300, used: null, limit: null, unit: null },
      { key: 'seven_day', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 0, remainingPercent: 100, resetAt: '2026-10-08T12:00:00Z', windowMinutes: 10080, used: null, limit: null, unit: null },
    ],
    capabilities: { codexProfile: 'lex', claudeProfileId: null, claudePlatforms: [] },
  };
  const raw = structuredClone(account);
  for (const windows of [account.windows, [...account.windows].reverse()]) {
    const data = { schemaVersion: 1, accounts: [{ ...account, windows }] };
    const view = dashboardView(data).codex[0];
    assert.equal(view.five.hasPercent, true);
    assert.equal(view.five.amount, '0% used');
    assert.equal(view.five.reset, timeLabel(account.windows[2].resetAt, 'Resets'));
    assert.equal(view.weekly.hasPercent, true);
    assert.equal(view.weekly.amount, '0% used');
    assert.equal(view.weekly.reset, timeLabel(account.windows[3].resetAt, 'Resets'));
    assert.equal(allDetailWindows(data, account.id).length, 4);
  }
  const additionalOnly = dashboardView({ accounts: [{ ...account, windows: account.windows.slice(0, 2) }] }).codex[0];
  assert.equal(additionalOnly.five.hasPercent, false);
  assert.equal(additionalOnly.five.reset, '');
  assert.equal(additionalOnly.weekly.hasPercent, false);
  assert.equal(additionalOnly.weekly.reset, '');
  assert.deepEqual(account, raw);
});
test('Claude primary selection retains its provider-specific duration heuristics', () => {
  const view = dashboardView({ accounts: [{ id: 'claude:a', provider: 'claude', plan: 'Max', windows: [
    { key: 'session_limit', label: 'Session usage', kind: 'rate_limit', windowMinutes: 300, usedPercent: 0 },
    { key: 'all_models_limit', label: 'All models usage', kind: 'rate_limit', windowMinutes: 10080, usedPercent: 24 },
  ] }] }).claude[0];
  assert.equal(view.five.amount, '0% used');
  assert.equal(view.weekly.amount, '24% used');
});
test('all provider rows are retained for details despite compact card preview', () => {
  const data = { accounts: [{ id: 'zai:a', provider: 'zai', windows: Array.from({ length: 6 }, (_, i) => ({ label: `Window ${i}`, used: i })) }] };
  assert.equal(dashboardView(data).providers.find(row => row.id === 'zai').windows.length, 3);
  assert.equal(allDetailWindows(data, 'zai').length, 6);
});

test('signed balances and adjustment counters retain their actual sign', () => {
  const debt = usageView({ kind: 'balance', remaining: -2.5, unit: 'credits' });
  assert.equal(debt.amount, '-2.5 credits remaining'); assert.equal(debt.hasPercent, false);
  assert.equal(usageView({ kind: 'spend', used: -1, limit: 50, unit: 'USD' }).amount, '-1 / 50 USD used');
});
test('Lex active state is explicit and uses reported primary percentages', () => {
  const data = { accounts: [{ id: 'codex:lex', provider: 'codex', email: 'lime@example.com', plan: 'Plus', isActive: true, capabilities: { codexProfile: 'lime' }, windows: [{ key: 'five_hour', label: '5-hour', usedPercent: 5 }, { key: 'seven_day', label: 'Weekly', usedPercent: 9 }] }] };
  const view = dashboardView(data);
  assert.equal(view.activeCodexEmail, 'lime@example.com');
  assert.equal(view.codex[0].active, true);
  assert.equal(view.codex[0].status, '✓ Active on Ubuntu');
  assert.equal(view.codex[0].five.amount, '5% used');
  assert.equal(view.codex[0].weekly.amount, '9% used');
});

test('display fractions round to at most two decimals without reducing source precision', () => {
  const window = { usedPercent: 6.340123, used: 19020266200.12345, limit: 300000000000, remaining: -2.55555, unit: 'credits' };
  const view = usageView(window);
  assert.equal(view.amount, '19,020,266,200.12 / 300,000,000,000 credits used · -2.56 credits remaining · 6.34% used');
  assert.equal(view.percent, 6.340123);
  assert.equal(window.used, 19020266200.12345);
});

test('cached Muse card shows percentages while Details retain real weighted tokens and percentage', () => {
  const account = { id: 'muse:usage', provider: 'muse', status: 'cached', source: 'Account on Mac', fetchedAt: '2026-10-01T15:15:19Z', sampledAt: '2026-10-01T13:20:06Z', plan: 'Muse Code High Usage', windows: [
    { key: 'window', label: '5-hour usage', kind: 'rate_limit', windowMinutes: 300, used: 0, limit: 100000000000, unit: 'weighted tokens', usedPercent: 0, resetAt: null },
    { key: 'weekly', label: 'Weekly usage', kind: 'rate_limit', windowMinutes: 10080, used: 19020266200, limit: 300000000000, unit: 'weighted tokens', usedPercent: 6.3401, resetAt: '2026-10-05T00:00:00Z', expiresAt: '2026-11-01T00:00:00Z' },
  ] };
  const data = { accounts: [account] };
  const card = dashboardView(data).providers.find(row => row.id === 'muse');
  assert.equal(card.status, 'Cached');
  assert.equal(card.source, `Account on Mac · ${timeLabel(account.sampledAt, 'Sampled')}`);
  assert.doesNotMatch(card.source, /Fetched/);
  assert.deepEqual(card.windows.map(row => row.amount), ['0% used', '6.34% used']);
  assert.equal(card.windows[1].percent, 6.3401);
  assert.match(card.windows[1].reset, /^Resets /);
  assert.match(card.windows[1].expiration, /^Expires /);
  const details = allDetailWindows(data, 'muse');
  assert.equal(details[0].amount, '0 / 100,000,000,000 weighted tokens used · 0% used');
  assert.equal(details[1].amount, '19,020,266,200 / 300,000,000,000 weighted tokens used · 6.34% used');
  assert.equal(details[1].reset, card.windows[1].reset);
  assert.equal(details[1].expiration, card.windows[1].expiration);
  assert.equal(account.windows[1].usedPercent, 6.3401);
  assert.equal(account.sampledAt, '2026-10-01T13:20:06Z');
  assert.equal(account.fetchedAt, '2026-10-01T15:15:19Z');
});

test('cached Muse retrieval time is labeled Fetched when no usable original sample time is returned', () => {
  const fetchedAt = '2026-10-01T15:15:19Z';
  const cardFor = account => dashboardView({ accounts: [{ id: 'muse:usage', provider: 'muse', source: 'Account on Mac', status: 'cached', ...account }] }).providers.find(row => row.id === 'muse');
  for (const sampledAt of [undefined, '', 'not-an-ISO-date']) {
    const card = cardFor({ sampledAt, fetchedAt });
    assert.equal(card.source, `Account on Mac · ${timeLabel(fetchedAt, 'Fetched')}`);
    assert.doesNotMatch(card.source, /Sampled/);
  }
  assert.equal(cardFor({}).source, 'Account on Mac');
  assert.equal(cardFor({ sampledAt: 'invalid', fetchedAt: 'invalid' }).source, 'Account on Mac');
  assert.equal(cardFor({ status: 'ok', sampledAt: '2026-10-01T13:20:06Z', fetchedAt }).source, 'Account on Mac');
});

test('Muse percentages unavailable from the provider are not fabricated from token budgets', () => {
  const data = { accounts: [{ id: 'muse:usage', provider: 'muse', status: 'cached', windows: [{ key: 'weekly', label: 'Weekly usage', used: 100, limit: 500, unit: 'weighted tokens', usedPercent: null }] }] };
  const card = dashboardView(data).providers.find(row => row.id === 'muse');
  assert.equal(card.windows[0].amount, 'Usage unavailable');
  assert.equal(card.windows[0].hasPercent, false);
  assert.equal(allDetailWindows(data, 'muse')[0].amount, '100 / 500 weighted tokens used');
  assert.doesNotMatch(card.source, /Sampled/);
});

test('three Claude Max rows show an explicit unavailable Fable slot; Pro has no slot', () => {
  const accounts = ['plum', 'party', 'me'].map(id => ({ id: `claude:${id}`, provider: 'claude', plan: 'max', windows: [
    { key: 'five_hour', label: 'Five-hour usage', windowMinutes: 300, usedPercent: 0 },
    { key: 'seven_day', label: 'Weekly usage', windowMinutes: 10080, usedPercent: 100 },
    { key: 'seven_day_opus', label: 'Weekly Opus usage', windowMinutes: 10080, usedPercent: 75 },
    { key: 'seven_day_sonnet', label: 'Weekly Sonnet usage', windowMinutes: 10080, usedPercent: 41 },
  ] }));
  accounts.push({ id: 'claude:gmail', provider: 'claude', plan: 'pro', windows: [] });
  const view = dashboardView({ accounts });
  assert.deepEqual(view.claude.map(row => row.showFable), [true, true, true, false]);
  for (const row of view.claude.slice(0, 3)) {
    assert.equal(row.fable.hasPercent, false);
    assert.equal(row.fable.amount, 'Usage unavailable');
    assert.equal(row.fable.reset, '');
    assert.equal(row.fable.label, 'Fable usage');
    assert.equal(row.weekly.amount, '100% used');
  }
  assert.equal(accounts[0].windows.length, 4, 'unknown Fable does not create a raw window');
});

test('reported Fable remains distinct from total weekly quota and displays its actual reset', () => {
  const account = { id: 'claude:party', provider: 'claude', plan: 'Claude Max 20x', windows: [
    { key: 'seven_day_fable', label: 'Fable usage', windowMinutes: 10080, usedPercent: 7.1255, resetAt: '2026-10-08T05:33:43Z' },
    { key: 'seven_day', label: 'Weekly usage', windowMinutes: 10080, usedPercent: 12 },
  ] };
  const view = dashboardView({ accounts: [account] }).claude[0];
  assert.equal(view.showFable, true);
  assert.equal(view.fable.amount, '7.13% used');
  assert.equal(view.fable.percent, 7.1255);
  assert.match(view.fable.reset, /^Resets /);
  assert.equal(view.weekly.amount, '12% used');
  assert.equal(allDetailWindows({ accounts: [account] }, account.id)[0].amount, '7.13% used');
  assert.equal(account.windows[0].usedPercent, 7.1255);
  const zero = dashboardView({ accounts: [{ ...account, windows: [{ key: 'fable', label: 'Fable usage', usedPercent: 0 }] }] }).claude[0];
  assert.equal(zero.fable.amount, '0% used');
  assert.equal(zero.fable.hasPercent, true);
});

test('cached optional windows retain their original sample time beside fresh core and Fable quotas', () => {
  const original = '2026-10-02T04:15:23.456Z';
  const refreshed = '2026-10-02T11:00:00Z';
  const account = { id: 'claude:plum', provider: 'claude', plan: 'max', status: 'ok', fetchedAt: refreshed, sampledAt: refreshed, windows: [
    { key: 'seven_day', label: 'Weekly usage', usedPercent: 12, resetAt: '2026-10-08T12:00:00Z' },
    { key: 'seven_day_fable', label: 'Weekly Fable usage', usedPercent: 0, resetAt: '2026-10-08T12:00:00Z' },
    { key: 'reset_credit_used_grant_1', label: 'Used rate-limit reset grant 1', kind: 'balance', used: 1, limit: 1, remaining: 0, unit: 'resets', expiresAt: '2026-10-22T16:00:00Z', status: 'cached', sampledAt: original },
  ] };
  const data = { accounts: [account] };
  const row = dashboardView(data).claude[0];
  assert.equal(row.status, 'Live');
  assert.equal(row.weekly.meta, '');
  assert.equal(row.fable.meta, '');
  assert.equal(row.fable.amount, '0% used');
  const retained = allDetailWindows(data, account.id)[2];
  assert.equal(retained.amount, '1 / 1 resets used · 0 resets remaining');
  assert.equal(retained.meta, `Cached · ${timeLabel(original, 'Sampled')}`);
  assert.doesNotMatch(retained.meta, new RegExp(timeLabel(refreshed, 'Sampled')));
  assert.match(retained.expiration, /^Expires /);
  assert.equal(retained.reset, '');
  assert.equal(account.windows[2].sampledAt, original);
  assert.equal(account.fetchedAt, refreshed);
});

test('cache provenance neither fabricates dates nor replaces an unknown usage value', () => {
  for (const sampledAt of [undefined, '', 'not-an-ISO-date']) {
    const view = usageView({ status: 'cached', sampledAt });
    assert.equal(view.meta, 'Cached');
    assert.equal(view.amount, 'Usage unavailable');
    assert.equal(view.hasPercent, false);
    assert.equal(view.reset, '');
  }
  const live = usageView({ usedPercent: 8, status: 'ok', sampledAt: '2026-10-02T11:00:00Z' });
  assert.equal(live.meta, '');
  assert.equal(usageView({ remaining: 0, unit: 'credits', enabled: false, status: 'cached' }).meta, 'Disabled · Cached');
});
