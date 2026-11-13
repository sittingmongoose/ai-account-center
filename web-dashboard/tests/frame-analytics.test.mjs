// The app frame's Analytics controller (public/frame-analytics.mjs) and the page rules it shares with bridge.js
// (public/analytics-page.mjs), against a fake server with made-up data (tests/browser/device/analytics-fixture.mjs).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createFrameAnalytics, HEAD_EVERY_MS } from '../public/frame-analytics.mjs';
import {
  createAnalyticsPage,
  analyticsCovers,
  analyticsPollDelay,
  analyticsThresholds,
  applyAnalyticsAction,
  applyAnalyticsLayout,
} from '../public/analytics-page.mjs';
import { getDisplayTimeZone, setDisplayTimeZone } from '../public/time-format.mjs';
import { analyticsFixture } from './browser/device/analytics-fixture.mjs';

const NOW = Date.parse('2026-11-12T15:00:00Z');
const H = 3_600_000;
const D = 24 * H;

const dashboard = (extra = {}) => ({
  schemaVersion: 1,
  updatedAt: new Date(NOW - 60_000).toISOString(),
  accounts: [
    { id: 'codex-1', provider: 'codex', label: 'codex.one@example.invalid', email: 'codex.one@example.invalid', status: 'ok', windows: [] },
  ],
  settings: { refreshIntervalSeconds: 120, hiddenProviders: [], hiddenAccountIds: [] },
  codexAutoSwitch: { enabled: true, thresholdPercent: 10 },
  ...extra,
});

/** A fake server: `routes(path)` answers { status, body } (or 'offline', or a promise of one). Records every call. */
function fakeServer(routes) {
  const calls = [];
  const fetch = async (path) => {
    calls.push(path);
    let answer = await routes(path);
    if (answer === 'offline') throw new TypeError('Failed to fetch');
    if (!answer) answer = { status: 404, body: { error: 'Not found' } };
    return { ok: answer.status >= 200 && answer.status < 300, status: answer.status, json: async () => answer.body ?? null };
  };
  return { fetch, calls };
}

function defaultRoutes(overrides = {}) {
  return (path) => {
    const url = new URL(path, 'http://x');
    if (overrides[url.pathname]) return overrides[url.pathname](url);
    if (url.pathname === '/api/accounts/analytics')
      return { status: 200, body: analyticsFixture({ range: url.searchParams.get('range') || '7d', now: NOW, accounts: dashboard().accounts }) };
    if (url.pathname === '/api/accounts/dashboard') return { status: 200, body: dashboard() };
    if (url.pathname === '/api/accounts/preferences') return { status: 200, body: { timeZone: 'UTC' } };
    if (url.pathname === '/api/antigravity/auto-switch') return { status: 404, body: null };
    return null;
  };
}

/** Manual timers: `run(ms)` fires what is due. */
function clock() {
  let t = NOW;
  let next = 1;
  const due = new Map();
  return {
    now: () => t,
    setTimeout: (fn, ms) => { const id = next++; due.set(id, { at: t + ms, fn }); return id; },
    clearTimeout: (id) => due.delete(id),
    pending: () => [...due.values()].map((d) => d.at - t).sort((a, b) => a - b),
    async run(ms) {
      t += ms;
      for (const [id, d] of [...due]) if (d.at <= t) { due.delete(id); d.fn(); }
      await settle();
    },
  };
}
const settle = () => new Promise((r) => setTimeout(r, 0));

function make(routes = defaultRoutes(), opts = {}) {
  const server = fakeServer(routes);
  const c = clock();
  const sent = [];
  const views = [];
  let ended = [];
  const controller = createFrameAnalytics({
    fetch: server.fetch,
    now: c.now,
    setTimeout: c.setTimeout,
    clearTimeout: c.clearTimeout,
    emit: (key, json) => sent.push([key, json]),
    signedOut: (reason) => ended.push(reason ?? null),
    onView: (v) => views.push(v),
    ...opts,
  });
  return { controller, server, c, sent, views, ended: () => ended };
}
const keys = (sent) => sent.map(([k]) => k);
const last = (sent, key) => [...sent].reverse().find(([k]) => k === key)?.[1];

test('shown, it reads its own data and draws a version 7 view; a loading state first, never zeros', async () => {
  const { controller, server, sent, views } = make();
  controller.show();
  // before any answer: only the loading state; no view, and the header says nothing until there is a reading
  assert.deepEqual(sent, [['analytics-loading', JSON.stringify({ loading: true, error: '' })]]);
  await settle();
  await settle();
  assert.ok(server.calls.some((p) => p.startsWith('/api/accounts/analytics?platform=mac&range=7d')));
  assert.ok(server.calls.some((p) => p === '/api/accounts/dashboard?platform=mac&refresh=false'));
  const order = keys(sent);
  assert.ok(order.indexOf('analytics-motion') < order.indexOf('analytics'), 'the motion comes just before the view');
  const view = JSON.parse(last(sent, 'analytics'));
  assert.equal(view.version, 7);
  assert.ok(view.usage.ready && view.usage.kpis.length === 5);
  assert.equal(last(sent, 'analytics-motion'), 'static');
  assert.deepEqual(JSON.parse(last(sent, 'analytics-loading')), { loading: false, error: '' });
  assert.equal(views.length, 1);
  // the chrome line is the dashboard's, as bridge.js words it
  const chrome = JSON.parse(last(sent, 'chrome'));
  assert.equal(chrome.statusLead, 'Updated');
  assert.equal(chrome.statusMore, 'live');
  assert.equal(controller.state().intervalSeconds, 120, 'the dashboard settings set the refresh interval');
});

test('the display time zone is applied before the view is built', async () => {
  setDisplayTimeZone('America/New_York');
  const { controller } = make(defaultRoutes({ '/api/accounts/preferences': () => ({ status: 200, body: { timeZone: 'Europe/Berlin' } }) }));
  controller.show();
  await settle();
  await settle();
  assert.equal(getDisplayTimeZone(), 'Europe/Berlin');
  setDisplayTimeZone('UTC');
});

test('the page actions change the state; a range the answer does not cover is read; refused ones do nothing', async () => {
  const { controller, server, sent } = make();
  controller.show();
  await settle();
  await settle();
  const before = server.calls.length;
  assert.equal(controller.act('analytics-range', '30d'), true);
  await settle();
  await settle();
  assert.equal(controller.state().range, '30d');
  assert.ok(server.calls.slice(before).some((p) => p.includes('range=30d')), 'a 30-day range is read');
  assert.equal(JSON.parse(last(sent, 'analytics')).state.range, '30d');
  // 24h is covered by the 30-day answer: drawn without a read
  const n = server.calls.length;
  controller.act('analytics-range', '24h');
  await settle();
  assert.equal(JSON.parse(last(sent, 'analytics')).state.range, '24h');
  assert.ok(!server.calls.slice(n).some((p) => p.startsWith('/api/accounts/analytics') && p.includes('range=30d')));
  // refused values change nothing
  const m = server.calls.length;
  assert.equal(controller.act('analytics-range', 'forever'), true);
  assert.equal(controller.act('analytics-provider', 'Not An Id'), true);
  assert.equal(controller.act('analytics-focus', 'no-such-account'), true);
  await settle();
  assert.equal(server.calls.length, m);
  assert.equal(controller.state().prov, 'all');
  // a kind that isn't the page's
  assert.equal(controller.act('remove', 'codex:one'), false);
  // a known account opens its focus chart
  controller.act('analytics-provider', 'codex');
  assert.equal(JSON.parse(last(sent, 'analytics')).state.prov, 'codex');
});

test('a layout report lays the charts out again once, after a short wait', async () => {
  const { controller, c, views } = make();
  controller.show();
  await settle();
  await settle();
  const drawn = views.length;
  controller.act('analytics-layout', 'trend,800,300');
  controller.act('analytics-layout', 'trend,800,300');
  assert.equal(views.length, drawn);
  await c.run(30);
  assert.equal(views.length, drawn + 1);
});

test('Refresh asks every provider first, then reads the analytics afresh', async () => {
  const { controller, server } = make();
  controller.show();
  await settle();
  await settle();
  const before = server.calls.length;
  controller.act('analytics-refresh', '');
  for (let i = 0; i < 4; i++) await settle();
  const calls = server.calls.slice(before);
  const dash = calls.findIndex((p) => p === '/api/accounts/dashboard?platform=mac&refresh=true');
  const read = calls.findIndex((p) => p.startsWith('/api/accounts/analytics') && p.includes('refresh=true'));
  assert.ok(dash >= 0 && read > dash, JSON.stringify(calls));
});

test('a session that ended stops the page: the frame is told once', async () => {
  const { controller, ended, sent } = make(defaultRoutes({ '/api/accounts/analytics': () => ({ status: 401, body: { code: 'session_revoked', error: 'Signed out.' } }) }));
  controller.show();
  await settle();
  await settle();
  assert.deepEqual(ended(), ['revoked']);
  assert.ok(!keys(sent).includes('analytics'), 'nothing drawn');
});

test('a failed read says so and keeps what was on screen; one failed poll stays quiet', async () => {
  let fail = false;
  const { controller, sent, c } = make(
    defaultRoutes({
      '/api/accounts/analytics': (url) => fail ? { status: 500, body: { error: 'Analytics failed on the server.' } } : {
        status: 200,
        body: { ...analyticsFixture({ range: url.searchParams.get('range') || '7d', now: NOW, accounts: dashboard().accounts }), activity: { ...analyticsFixture({ range: '7d', now: NOW, accounts: [] }).activity, refreshing: true } },
      },
    })
  );
  controller.show();
  await settle();
  await settle();
  const drawn = last(sent, 'analytics');
  assert.ok(drawn);
  // the server is still reading: the page polls quietly
  assert.ok(c.pending().includes(60000) || c.pending().includes(10000), JSON.stringify(c.pending()));
  fail = true;
  const loadingBefore = sent.filter(([k]) => k === 'analytics-loading').length;
  await c.run(60000);
  await settle();
  // the answer in hand stays drawn (its rolling window follows the clock), never cleared to zeros
  const shown = JSON.parse(last(sent, 'analytics'));
  assert.ok(shown.usage.ready && shown.usage.kpis.length === 5 && shown.usage.kpis.every((k) => k.has), 'the numbers on screen stay');
  assert.equal(sent.filter(([k]) => k === 'analytics-loading').length, loadingBefore, 'one failed poll says nothing');
  controller.act('analytics-range', '30d');
  for (let i = 0; i < 4; i++) await settle();
  assert.deepEqual(JSON.parse(last(sent, 'analytics-loading')), { loading: false, error: 'Analytics failed on the server.' });
});

test('a server that just started answers loading first: the page reads again after 2 s, not 10 s', async () => {
  let reads = 0;
  const { controller, c, sent } = make(
    defaultRoutes({
      '/api/accounts/analytics': (url) => {
        reads++;
        if (reads === 1) return { status: 200, body: { schemaVersion: 1, accounts: [], range: { preset: '7d', from: new Date(NOW - 7 * D).toISOString() }, activity: { status: 'loading', refreshing: true } } };
        return { status: 200, body: analyticsFixture({ range: url.searchParams.get('range') || '7d', now: NOW, accounts: dashboard().accounts }) };
      },
    })
  );
  controller.show();
  await settle();
  await settle();
  assert.equal(reads, 1);
  assert.ok(!JSON.parse(last(sent, 'analytics')).usage.ready, 'no numbers yet: the page keeps its loading state');
  assert.equal(c.pending()[0], 2000, JSON.stringify(c.pending()));
  await c.run(2000);
  await settle();
  assert.equal(reads, 2);
  assert.ok(JSON.parse(last(sent, 'analytics')).usage.ready, 'the numbers land on the second read');
});

/** An answer from a server still reading its logs: no totals yet. */
const stillLoading = () => ({ status: 200, body: { schemaVersion: 1, accounts: [], range: { preset: '7d', from: new Date(NOW - 7 * D).toISOString() }, activity: { status: 'loading', refreshing: true } } });

test('a background poll reads the analytics only, and the header never says Refreshing for it (review L1)', async () => {
  const { controller, server, c, sent } = make(defaultRoutes({ '/api/accounts/analytics': stillLoading }));
  controller.show();
  await settle();
  await settle();
  const calls = server.calls.length;
  const said = sent.length;
  for (let i = 0; i < 5; i++) {
    await c.run(2000);
    await settle();
    await settle();
  }
  const polled = server.calls.slice(calls);
  assert.ok(polled.length >= 4, `the page kept polling: ${JSON.stringify(polled)}`);
  assert.ok(polled.every((p) => p.startsWith('/api/accounts/analytics?')), `a poll reads the analytics only: ${JSON.stringify(polled)}`);
  const lines = sent.slice(said).filter(([k]) => k === 'chrome').map(([, j]) => JSON.parse(j).statusLead);
  assert.ok(!lines.includes('Refreshing'), `the header stays put while polling: ${JSON.stringify(lines)}`);
});

test('the refresh interval and Refresh say Refreshing in the header while they read the dashboard', async () => {
  const { controller, c, sent } = make();
  controller.show();
  await settle();
  await settle();
  const leads = (from) => sent.slice(from).filter(([k]) => k === 'chrome').map(([, j]) => JSON.parse(j).statusLead);
  let from = sent.length;
  await c.run(120_000);
  for (let i = 0; i < 4; i++) await settle();
  // (the 15 s header ticks before it say Updated too)
  let l = leads(from);
  assert.ok(l.includes('Refreshing') && l.at(-1) === 'Updated', `the refresh interval: ${JSON.stringify(l)}`);
  from = sent.length;
  controller.act('analytics-refresh', '');
  for (let i = 0; i < 4; i++) await settle();
  l = leads(from);
  assert.ok(l[0] === 'Refreshing' && l.at(-1) === 'Updated', `Refresh: ${JSON.stringify(l)}`);
});

const rowIds = (json) => JSON.parse(json).quota.groups.flatMap((g) => g.rows.map((r) => r.id));
/** A dashboard whose hidden accounts the test changes, as Accounts & Settings would. */
function hiding() {
  const state = { hidden: [] };
  const routes = defaultRoutes({
    '/api/accounts/dashboard': () => ({ status: 200, body: dashboard({ settings: { refreshIntervalSeconds: 120, hiddenProviders: [], hiddenAccountIds: state.hidden } }) }),
  });
  return { state, routes };
}

test('back on the tab, an account hidden meanwhile is never drawn again (review L2)', async () => {
  const { state, routes } = hiding();
  const { controller, sent, c, server } = make(routes);
  controller.show();
  await settle();
  await settle();
  assert.ok(rowIds(last(sent, 'analytics')).includes('codex-1'), 'the account is drawn first');
  controller.hide();
  state.hidden = ['codex-1'];
  await c.run(30_000);
  const from = sent.length;
  const calls = server.calls.length;
  controller.show();
  for (let i = 0; i < 4; i++) await settle();
  const views = sent.slice(from).filter(([k]) => k === 'analytics');
  assert.ok(views.length >= 1, 'the page is drawn again on its return');
  assert.ok(views.every(([, j]) => !rowIds(j).includes('codex-1')), 'no view drawn on return shows the hidden account');
  assert.ok(!server.calls.slice(calls).some((p) => p.startsWith('/api/accounts/analytics')), 'the young answer is not read again');
});

test('an account hidden on another tab leaves the Analytics page before it comes back (review L2)', async () => {
  const { state, routes } = hiding();
  const { controller, sent, c, server } = make(routes);
  controller.show();
  await settle();
  await settle();
  controller.hide();
  state.hidden = ['codex-1'];
  const calls = server.calls.length;
  // the dashboard's own view changed (bridge.js after a hide): the frame tells the Analytics controller
  controller.dashboardChanged();
  controller.dashboardChanged();
  await c.run(1000);
  for (let i = 0; i < 4; i++) await settle();
  const read = server.calls.slice(calls);
  assert.equal(read.filter((p) => p.startsWith('/api/accounts/dashboard')).length, 1, `one dashboard read for a burst: ${JSON.stringify(read)}`);
  assert.ok(!read.some((p) => p.startsWith('/api/accounts/analytics')), 'no analytics read while the tab is away');
  assert.ok(!rowIds(last(sent, 'analytics')).includes('codex-1'), 'the hidden account is gone before the tab is shown');
  assert.deepEqual(c.pending(), [], 'no timer left while the tab is away');
});

test('hidden, nothing is read and no timer runs; shown again with a young answer, it draws without a read', async () => {
  const { controller, server, c } = make();
  controller.show();
  await settle();
  await settle();
  controller.hide();
  assert.deepEqual(c.pending(), []);
  const n = server.calls.length;
  await c.run(HEAD_EVERY_MS * 4);
  assert.equal(server.calls.length, n);
  controller.show();
  await settle();
  const again = server.calls.slice(n);
  assert.ok(!again.some((p) => p.startsWith('/api/accounts/analytics')), 'a young answer that covers the range is drawn as it is');
  assert.ok(again.includes('/api/accounts/dashboard?platform=mac&refresh=false'), 'the dashboard beside it is read again');
  await c.run(120_000);
  await settle();
  assert.ok(server.calls.length > n, 'the refresh interval reads again');
});

test('stopped (signed out), it sends nothing more', async () => {
  const { controller, sent, c } = make();
  controller.show();
  controller.stop();
  const n = sent.length;
  await settle();
  await settle();
  assert.equal(sent.length, n);
  assert.ok(!keys(sent).includes('analytics'));
  assert.deepEqual(c.pending(), []);
  assert.equal(controller.act('analytics-range', '30d'), false);
});

test('analytics-page: covers, poll delay, thresholds and layout', () => {
  const page = createAnalyticsPage();
  const payload = { range: { from: new Date(NOW - 7 * D).toISOString(), preset: '7d' } };
  assert.equal(analyticsCovers(payload, page, NOW), true);
  page.range = '30d';
  assert.equal(analyticsCovers(payload, page, NOW), false);
  page.range = 'all';
  assert.equal(analyticsCovers({ range: { from: payload.range.from, preset: '30d' } }, page, NOW), true);
  assert.equal(analyticsCovers({}, page, NOW), false);

  assert.equal(analyticsPollDelay(null), 0);
  assert.equal(analyticsPollDelay({ activity: { status: 'loading' } }), 10000, 'bridge.js passes no wait: calm');
  assert.equal(analyticsPollDelay({ activity: { status: 'loading' } }, 0), 2000, 'a page waiting for its first numbers reads again soon');
  assert.equal(analyticsPollDelay({ activity: { status: 'loading' } }, 12_000), 5000);
  assert.equal(analyticsPollDelay({ activity: { status: 'loading' } }, 40_000), 10000);
  assert.equal(analyticsPollDelay({ activity: { status: 'loading', totals: {} } }, 0), 60000, 'numbers in hand: calm');
  assert.equal(analyticsPollDelay({ activity: { status: 'cached', sources: [{ state: 'scanning' }] } }), 15000);
  assert.equal(analyticsPollDelay({ activity: { status: 'loading', totals: {} } }), 60000);
  assert.equal(analyticsPollDelay({ activity: { status: 'ready', refreshing: false } }), 0);

  assert.deepEqual(analyticsThresholds(dashboard(), null), { codex: 90, antigravity: null });
  const ag = { enabled: true, thresholdUsedPercent: 80 };
  const two = dashboard({ accounts: [{ id: 'a', provider: 'antigravity' }, { id: 'b', provider: 'antigravity' }] });
  assert.deepEqual(analyticsThresholds(two, ag), { codex: 90, antigravity: 80 });
  assert.deepEqual(analyticsThresholds(null, ag), { codex: null, antigravity: null });

  const p = createAnalyticsPage();
  assert.equal(applyAnalyticsLayout(p, 'trend,800,300'), true);
  assert.equal(applyAnalyticsLayout(p, 'trend,800.4,300.2'), false, 'under a pixel is no change');
  assert.equal(applyAnalyticsLayout(p, 'heat-cell,12'), true);
  assert.equal(applyAnalyticsLayout(p, 'nonsense,1,1'), false);
  assert.equal(applyAnalyticsLayout(p, 'daily,-1,10'), false);
});

test('analytics-page: actions', () => {
  const p = createAnalyticsPage();
  assert.equal(applyAnalyticsAction(p, 'remove', 'x'), null);
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-range', '7d'), {}, 'the same range changes nothing');
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-range', '30d'), { range: true });
  const custom = applyAnalyticsAction(p, 'analytics-custom', '2026-11-09,2026-11-11', { now: NOW });
  assert.deepEqual(custom, { range: true });
  assert.equal(p.range, 'custom');
  assert.ok(Number.isFinite(p.from) && p.to > p.from);
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-custom', '2025-01-01,2025-01-02', { now: NOW }), {}, 'older than 31 days is refused');
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-provider', 'codex'), { render: 'morph' });
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-donut', 'cost'), { render: 'static' });
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-donut', 'colour'), {});
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-focus', 'a', { known: (id) => id === 'a' }), { render: 'static' });
  assert.ok(p.open.has('a'));
  applyAnalyticsAction(p, 'analytics-focus', 'a', { known: () => true });
  assert.ok(!p.open.has('a'), 'a second focus closes it');
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-group', 'nope'), {});
});
