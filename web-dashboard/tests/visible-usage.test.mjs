import test from 'node:test';
import assert from 'node:assert/strict';
import { visibleUsageWindows } from '../public/visible-usage.mjs';
import { dashboardView, allDetailWindows } from '../public/accounts-data.mjs';

test('Codex Pro has no invented five-hour cell and real Plus zero-percent remains usable', () => {
  const pro = { id: 'codex:gmail', provider: 'codex', plan: 'pro', windows: [
    { key: 'seven_day', label: 'week', usedPercent: 99 },
    { key: 'extra_additional_1', label: 'Chat pass · weekly', usedPercent: 0 },
    { key: 'extra_additional_3', label: 'Chat pass · 5 hours', usedPercent: 0, windowMinutes: 300 },
  ] };
  const plus = { id: 'codex:lexxmariah', provider: 'codex', plan: 'plus', windows: [{ key: 'five_hour', label: '5h', windowMinutes: 300, usedPercent: 0 }] };
  const data = { accounts: [pro, plus] };
  const view = dashboardView(data);
  assert.equal(view.codex[0].five.hasPercent, false);
  assert.equal(view.codex[0].weekly.amount, '99% used');
  assert.equal(view.codex[1].five.hasPercent, true);
  assert.equal(view.codex[1].five.amount, '0% used');
  assert.deepEqual(allDetailWindows(data, pro.id).map(row => row.label), ['week']);
  assert.equal(pro.windows.length, 3, 'raw response remains intact');
  assert.equal(visibleUsageWindows('claude', pro.windows).length, 3, 'choice is scoped to Codex');
});

test('Qwen subscription expiry is attached to monthly usage without a metadata-only row', () => {
  const windows = [
    { key: 'monthly', label: 'Monthly', used: 45109.8292496092, limit: 180000, usedPercent: 25.06101624978289, remaining: 134890.1707503908, unit: 'credits', resetAt: '2026-10-20T16:00:00Z' },
    { key: 'subscription', label: 'Plan subscription', expiresAt: '2026-10-20T16:00:00Z' },
    { key: 'addon-pack', label: 'Credit pack', used: 0, limit: 20000, remaining: 20000, unit: 'credits', expiresAt: '2026-10-08T16:00:00Z' },
  ];
  const data = { accounts: [{ id: 'qwen:usage', provider: 'qwen', windows }] };
  const card = dashboardView(data).providers.find(row => row.id === 'qwen');
  assert.equal(card.windows.length, 2);
  assert.equal(card.windows[0].amount, '45,109.83 / 180,000 credits used · 25.06% used');
  assert.doesNotMatch(card.windows[0].amount, /remaining|Resets/);
  assert.match(card.windows[0].reset, /^Resets /);
  assert.match(card.windows[0].expiration, /^Expires /);
  assert.match(allDetailWindows(data, 'qwen')[0].amount, /134,890.17 credits remaining/);
  assert.equal(windows[0].expiresAt, undefined, 'raw monthly window is not mutated');
  assert.equal(windows.length, 3);
  assert.equal(visibleUsageWindows('qwen', [{ key: 'monthly' }, { key: 'plan_subscription', expiresAt: '2026-11-01T00:00:00Z' }])[0].expiresAt, '2026-11-01T00:00:00Z');
});

test('empty Zai reset-pack summaries disappear, while positive counts and real records remain', () => {
  const windows = [
    { key: 'reset-packs-5h', label: 'Available 5-hour reset packs', remaining: 0 },
    { key: 'reset-packs-weekly', label: 'Available weekly reset packs', remaining: null },
    { key: 'reset-pack-positive', label: 'Available reset packs', remaining: 2 },
    { key: 'pack-spent', label: 'Individual pack', remaining: 0, expiresAt: '2026-11-01T00:00:00Z' },
    { key: 'pack-adjustment', label: 'Individual pack adjustment', remaining: -2.5 },
    { key: 'usage-4', label: 'Search', used: 0, unit: 'requests' },
  ];
  const visible = visibleUsageWindows('zai', windows);
  assert.deepEqual(visible.map(row => row.key), ['reset-pack-positive', 'pack-spent', 'pack-adjustment', 'usage-4']);
  assert.equal(visibleUsageWindows('qwen', windows).length, windows.length);
  assert.equal(windows.length, 6);
});
