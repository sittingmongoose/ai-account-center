import test from 'node:test';
import assert from 'node:assert/strict';
import { visibleUsageWindows, clearsText } from '../public/visible-usage.mjs';
import { dashboardViewModel, windowLabel, fullWindowLabel, amountView } from '../public/view-model.mjs';
import { usageView, accountView } from '../public/accounts-data.mjs';
import { mainWindow, bindingWeekly } from '../public/analytics-quota.mjs';

test('dashboardViewModel guards per-item: one null window does not take down the whole dashboard', () => {
  const account = {
    id: 'claude:1',
    provider: 'claude',
    plan: 'max',
    sampledAt: '2026-11-10T10:00:00Z',
    windows: [
      null,
      { key: 'seven_day', label: 'Weekly', windowMinutes: 10080, usedPercent: 20, resetAt: '2026-11-10T12:00:00Z' },
    ],
  };
  const account2 = {
    id: 'claude:2',
    provider: 'claude',
    plan: 'pro',
    windows: [{ key: 'seven_day', label: 'Weekly', windowMinutes: 10080, usedPercent: 10 }],
  };
  const vm = dashboardViewModel({ accounts: [account, account2] }, { now: Date.parse('2026-11-10T10:00:00Z') });
  assert.ok(vm);
  assert.equal(vm.sections.find(s => s.id === 'claude').rows.length, 2);
  // the good account's weekly is present
  const row1 = vm.sections.find(s => s.id === 'claude').rows.find(r => r.id === 'claude:1');
  assert.ok(row1.cells.some(c => c.key === 'claude:1|seven_day' && c.hasValue));
});

test('windowLabel and fullWindowLabel return Usage for null windows', () => {
  assert.equal(windowLabel('claude', null), 'Usage');
  assert.equal(windowLabel('claude', undefined), 'Usage');
  assert.equal(fullWindowLabel('claude', null), 'Usage');
  assert.equal(windowLabel('cursor', null), 'Usage');
  assert.equal(windowLabel('zai', null), 'Usage');
});

test('amountView returns Unavailable for null window', () => {
  const view = amountView({ id: 'x', provider: 'claude' }, null, Date.now());
  assert.equal(view.value, 'Unavailable');
  assert.equal(view.label, 'Balance');
});

test('clearsText ignores non-string clears entries', () => {
  const r = clearsText({ clears: [null, 123, {}, 'five_hour', 'seven_day'] });
  assert.match(r, /session/);
  assert.match(r, /weekly/);
  assert.doesNotMatch(r, /123/);
  assert.doesNotMatch(r, /\[object Object\]/);
  // only strings are used
  assert.equal(clearsText({ clears: [123, null] }), '');
  assert.equal(clearsText({ clears: 'five_hour' }), '');
});

test('visibleUsageWindows filters null and primitive windows', () => {
  const out = visibleUsageWindows('claude', [null, undefined, 123, 'x', { key: 'seven_day', label: 'Weekly' }]);
  assert.equal(out.length, 1);
  assert.equal(out[0].key, 'seven_day');
  const qwen = visibleUsageWindows('qwen', [null, { key: 'monthly', label: 'Monthly' }]);
  assert.equal(qwen.length, 1);
});

test('accounts-data usageView handles null window', () => {
  const view = usageView(null);
  assert.equal(view.amount, 'Usage unavailable');
  assert.equal(view.hasPercent, false);
  const view2 = usageView(undefined);
  assert.equal(view2.amount, 'Usage unavailable');
});

test('analytics-quota mainWindow handles null windows and windowMinutes:null', () => {
  const acct = { provider: 'claude', windows: [
    null,
    { key: 'seven_day_opus', label: 'Weekly Opus usage', windowMinutes: null, usedPercent: 50 },
    { key: 'seven_day', label: 'Weekly usage', windowMinutes: 10080, usedPercent: 20 },
  ] };
  const main = mainWindow(acct);
  assert.equal(main?.key, 'seven_day');
  const binding = bindingWeekly(acct);
  // binding considers both weeklies, picks higher used (50 opus)
  assert.equal(binding?.key, 'seven_day_opus');
  // null windows alone
  assert.equal(mainWindow({ provider: 'claude', windows: [null, 42] }), null);
});

test('claudeSection with windowMinutes:null scoped weeklies renders without throwing', () => {
  const mk = (k, l, used) => ({ key: k, label: l, windowMinutes: null, usedPercent: used, remainingPercent: 100 - used, resetAt: '2026-11-10T12:00:00Z' });
  const acct = { id: 'claude:scoped', provider: 'claude', plan: 'pro', status: 'ok', sampledAt: '2026-11-10T10:00:00Z', windows: [
    mk('seven_day_opus', 'Weekly Opus usage', 44),
    mk('seven_day_sonnet', 'Weekly Sonnet usage', 22),
    mk('seven_day_fable_code', 'Weekly Fable (Code) usage', 90),
    { key: 'seven_day_fable', label: 'Weekly Fable usage', windowMinutes: 10080, usedPercent: 7 },
    { key: 'session_reset_allowance', label: 'Weekly session reset', kind: 'balance', unit: 'resets', remaining: 1, limit: 2, resetAt: '2026-11-13T12:00:00Z' },
    { key: 'reset_credit_session_grant_1', label: 'Session reset grant 1', kind: 'balance', unit: 'resets', used: 1, limit: 2, remaining: 1, clears: ['five_hour', 'seven_day'] },
  ] };
  const vm = dashboardViewModel({ accounts: [acct] }, { now: Date.parse('2026-11-10T10:00:00Z') });
  const sec = vm.sections.find(s => s.id === 'claude');
  assert.ok(sec.columns.some(c => c.key === 'seven_day_opus'));
  assert.ok(sec.columns.some(c => c.key === 'seven_day_fable_code'));
  assert.ok(sec.rows[0].cells.length >= 2);
});

test('accountView never borrows a scoped weekly for the aggregate weekly', () => {
  const acct = { id: 'claude:scoped', provider: 'claude', plan: 'pro', windows: [
    { key: 'seven_day_opus', label: 'Weekly Opus usage', windowMinutes: 10080, usedPercent: 75 },
    { key: 'seven_day', label: 'Weekly usage', windowMinutes: 10080, usedPercent: 24 },
  ] };
  const view = accountView(acct, [], 'mac');
  assert.equal(view.weekly.amount, '24% used');
  assert.equal(view.five.hasPercent, false);
});
