import test from 'node:test';
import assert from 'node:assert/strict';
import { usageView, dashboardView, allDetailWindows } from '../public/accounts-data.mjs';
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
  const data = { accounts: [{ id: 'codex:lex', provider: 'codex', email: 'lexxmariah@gmail.com', plan: 'Plus', isActive: true, capabilities: { codexProfile: 'lexxmariah' }, windows: [{ key: 'five_hour', label: '5-hour', usedPercent: 5 }, { key: 'seven_day', label: 'Weekly', usedPercent: 9 }] }] };
  const view = dashboardView(data);
  assert.equal(view.activeCodexEmail, 'lexxmariah@gmail.com');
  assert.equal(view.codex[0].active, true);
  assert.equal(view.codex[0].status, '✓ Active on Ubuntu');
  assert.equal(view.codex[0].five.amount, '5% used');
  assert.equal(view.codex[0].weekly.amount, '9% used');
});
