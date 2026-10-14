import test from 'node:test';
import assert from 'node:assert/strict';
import { dashboardViewModel, detailsViewModel, planLabel } from '../public/view-model.mjs';
import { accountsViewModel } from '../public/accounts-view.mjs';
import { dashboardView } from '../public/accounts-data.mjs';
import { analyticsView } from '../public/analytics-data.mjs';
import { quotaView, quotaAccounts, focusChart, project } from '../public/analytics-quota.mjs';

const now = Date.parse('2026-10-14T12:00:00Z');
const H = 3_600_000;
const iso = (value) => new Date(value).toISOString();
const pool = (key = 'grok-shared-weekly', extra = {}) => ({
  key,
  label: 'Grok and Composer',
  kind: 'rate_limit',
  windowMinutes: 10080,
  usedPercent: 25,
  remainingPercent: 75,
  used: null,
  limit: null,
  remaining: null,
  resetAt: iso(now + 48 * H),
  ...extra,
});
const account = (plan = 'premium', windows = [pool()], extra = {}) => ({
  id: 'cursor:one',
  provider: 'cursor',
  providerLabel: 'Cursor',
  email: 'one@example.test',
  plan,
  status: 'ok',
  source: 'Cursor desktop',
  sampledAt: iso(now),
  platform: 'windows',
  windows,
  ...extra,
});
const data = (accounts) => ({ accounts, settings: {}, updatedAt: iso(now) });
const point = (t, usedPercent) => ({
  sampledAt: iso(t),
  usedPercent,
  resetAt: iso(now + 48 * H),
  status: 'ok',
});

test('Cursor plan codes identify X subscriptions and legacy tiers across all account views', () => {
  for (const [code, label] of [
    ['plus', 'X Plus'],
    ['premium', 'X Premium'],
    ['super', 'X Super'],
    ['unified_ultra', 'X Ultra'],
    ['free', 'Free'],
    ['pro', 'Pro'],
    ['pro_plus', 'Pro+'],
    ['ultra', 'Cursor Ultra'],
    ['team', 'Team'],
    ['enterprise', 'Enterprise'],
  ]) {
    const a = account(code);
    const dashboard = data([a]);
    assert.equal(planLabel(code, 'cursor'), label);
    assert.equal(dashboardViewModel(dashboard, { now }).cards[0].plan, label);
    assert.equal(detailsViewModel(dashboard, a.id, { now }).subLead, `Cursor · ${label}`);
    const management = accountsViewModel(dashboard, { now });
    assert.equal(
      [...management.colA, ...management.colB].find((p) => p.id === 'cursor').rows[0].meta,
      label
    );
    assert.equal(dashboardView(dashboard).providers.find((p) => p.id === 'cursor').plan, label);
    assert.equal(quotaView({ accounts: [] }, { dashboard, now }).groups[0].rows[0].plan, label);
    const analytics = analyticsView({ accounts: [a] }, {}, now);
    assert.equal(analytics.accounts[0].plan, label);
    assert.equal(analytics.quotaHistory[0].rows[0].sub, `${label} · windows`);
  }
  assert.equal(planLabel('plus', 'codex'), 'Plus', 'other provider labels stay unchanged');
  assert.equal(
    planLabel('unified_ultra'),
    'unified_ultra',
    'a plan string alone does not select Cursor semantics'
  );
  assert.equal(
    planLabel('future-plan', 'cursor'),
    'future-plan',
    'unknown plans retain their reported name'
  );
});

test('X plan cards and details explain the shared pool while preserving only reported quota facts', () => {
  for (const code of ['plus', 'premium', 'super', 'unified_ultra']) {
    const a = account(code);
    const dashboard = data([a]);
    const card = dashboardViewModel(dashboard, { now }).cards[0];
    assert.match(card.planNote, /^Weekly Grok/);
    assert.match(card.planNote, /shared across Cursor, Grok and X/);
    assert.equal(card.planNote.includes('separately purchased top-ups'), code !== 'plus');
    assert.equal(card.planNote.includes('Composer'), code !== 'plus');
    assert.equal(detailsViewModel(dashboard, a.id, { now }).note, card.planNote);
    assert.equal(card.meters[0].amount, '', 'no tier-based quota denominator');
    assert.equal(card.meters[0].value, 25);
  }
  for (const code of ['pro', 'pro_plus', 'ultra', 'free', 'unknown']) {
    assert.equal(dashboardViewModel(data([account(code)]), { now }).cards[0].planNote, '');
  }
  for (const window of [
    pool('grok-shared-monthly', { windowMinutes: null }),
    pool('grok-shared-usage', {
      windowMinutes: null,
      resetAt: null,
      usedPercent: null,
      remainingPercent: null,
    }),
  ]) {
    const a = account('premium', [window]);
    const card = dashboardViewModel(data([a]), { now }).cards[0];
    assert.doesNotMatch(card.planNote, /Weekly/);
    assert.equal(card.meters[0].hasValue, window.usedPercent !== null);
    assert.equal(card.meters[0].amount, '');
    if (window.resetAt === null) assert.equal(card.meters[0].resetExact, '');
  }
});

test('Cursor top-up spend remains an amount and never becomes an included quota graph', () => {
  const spend = {
    key: 'third-party-spend',
    label: 'Other model top-ups',
    kind: 'spend',
    unit: 'USD',
    used: 7.25,
    limit: null,
    usedPercent: null,
  };
  const a = account('premium', [pool(), spend]);
  const dashboard = data([a]);
  const card = dashboardViewModel(dashboard, { now }).cards[0];
  assert.equal(card.meters.length, 1);
  assert.equal(card.amounts.length, 1);
  assert.equal(card.amounts[0].value, '$7.25');
  assert.equal(card.amounts[0].unit, 'spent');
  const accounts = quotaAccounts({ accounts: [] }, { dashboard, now });
  const chart = focusChart(accounts[0], {}, { now, accounts });
  assert.deepEqual(
    chart.legend.filter((row) => row.kind === 'series').map((row) => row.name),
    ['Grok and Composer']
  );
});

test('switching Cursor plans replaces the current pool without joining legacy history or forecasts', () => {
  const legacy = pool('autoPercentUsed', {
    label: 'Cursor models',
    windowMinutes: null,
    usedPercent: 98,
  });
  const other = pool('apiPercentUsed', {
    label: 'Other models',
    windowMinutes: null,
    usedPercent: 84,
  });
  const current = pool();
  const history = {
    history: { oldestSampleAt: iso(now - 12 * H) },
    accounts: [
      account('premium', [
        {
          ...legacy,
          points: [point(now - 6 * H, 70), point(now - 4 * H, 85), point(now - 2 * H, 98)],
        },
        { ...other, points: [point(now - 4 * H, 84)] },
        { ...current, points: [point(now - H, 25)] },
      ]),
    ],
  };
  const before = data([account('ultra', [legacy, other])]);
  const after = data([account('premium', [current])]);
  assert.deepEqual(
    dashboardViewModel(before, { now }).cards[0].meters.map((w) => w.label),
    ['Cursor models', 'Other models']
  );
  assert.deepEqual(
    dashboardViewModel(after, { now }).cards[0].meters.map((w) => w.label),
    ['Grok and Composer']
  );
  const accounts = quotaAccounts(history, { dashboard: after, now });
  assert.deepEqual(
    accounts[0].windows.map((w) => w.key),
    ['grok-shared-weekly']
  );
  const chart = focusChart(accounts[0], history, { now, accounts });
  assert.deepEqual(
    chart.legend.filter((row) => row.kind === 'series').map((row) => row.name),
    ['Grok and Composer']
  );
  assert.equal(
    chart.stops.filter((stop) => !stop.proj).length,
    2,
    'only the new pool observation and now'
  );
  assert.ok(chart.stops.filter((stop) => !stop.proj).every((stop) => stop.rows[0].value === '25%'));
  assert.equal(
    project(accounts[0], accounts[0].windows[0], now).kind,
    'thin',
    'legacy samples cannot supply a forecast'
  );
  const row = quotaView(history, { dashboard: after, now }).groups[0].rows[0];
  assert.equal(row.plan, 'X Premium');
  assert.equal(row.points, 1);
  const returned = quotaView(history, { dashboard: before, now }).groups[0].rows[0];
  assert.equal(returned.plan, 'Cursor Ultra');
  assert.equal(returned.points, 3, 'old history is retained for its original pool');
});

test('Cursor comparisons require the same pool key even when different pools report the same duration', () => {
  const selected = account('premium');
  const incompatible = account('ultra', [pool('autoPercentUsed')], {
    id: 'cursor:legacy',
    email: 'legacy@example.test',
  });
  const matching = account('super', [pool()], {
    id: 'cursor:matching',
    email: 'matching@example.test',
  });
  const make = (rows) => {
    const accounts = quotaAccounts({ accounts: [] }, { now, dashboard: data(rows) });
    return focusChart(accounts[0], {}, { now, accounts, compare: true });
  };
  const incompatibleOnly = make([selected, incompatible]);
  assert.equal(incompatibleOnly.comparable, false);
  assert.equal(incompatibleOnly.compare, false);
  const chart = make([selected, incompatible, matching]);
  assert.equal(chart.comparable, true);
  assert.equal(chart.compare, true);
  assert.deepEqual(
    chart.legend.filter((row) => row.kind === 'series').map((row) => row.name),
    ['one', 'matching']
  );
});
