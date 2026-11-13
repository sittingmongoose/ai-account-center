// The app frame's Analytics controller (public/frame-analytics.mjs) and the page rules it shares with bridge.js
// (public/analytics-page.mjs), against a fake server with made-up data (tests/browser/device/analytics-fixture.mjs).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  createFrameAnalytics,
  installFrameAnalytics,
  HEAD_EVERY_MS,
} from '../public/frame-analytics.mjs';
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
    {
      id: 'codex-1',
      provider: 'codex',
      label: 'codex.one@example.invalid',
      email: 'codex.one@example.invalid',
      status: 'ok',
      windows: [],
    },
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
    return {
      ok: answer.status >= 200 && answer.status < 300,
      status: answer.status,
      json: async () => answer.body ?? null,
    };
  };
  return { fetch, calls };
}

function defaultRoutes(overrides = {}) {
  return (path) => {
    const url = new URL(path, 'http://x');
    if (overrides[url.pathname]) return overrides[url.pathname](url);
    if (url.pathname === '/api/accounts/analytics')
      return {
        status: 200,
        body: analyticsFixture({
          range: url.searchParams.get('range') || '7d',
          now: NOW,
          accounts: dashboard().accounts,
        }),
      };
    if (url.pathname === '/api/accounts/dashboard') return { status: 200, body: dashboard() };
    if (url.pathname === '/api/accounts/preferences')
      return { status: 200, body: { timeZone: 'UTC' } };
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
    setTimeout: (fn, ms) => {
      const id = next++;
      due.set(id, { at: t + ms, fn });
      return id;
    },
    clearTimeout: (id) => due.delete(id),
    pending: () => [...due.values()].map((d) => d.at - t).sort((a, b) => a - b),
    async run(ms) {
      t += ms;
      for (const [id, d] of [...due])
        if (d.at <= t) {
          due.delete(id);
          d.fn();
        }
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
  assert.ok(
    server.calls.some((p) => p.startsWith('/api/accounts/analytics?platform=mac&range=7d'))
  );
  assert.ok(server.calls.some((p) => p === '/api/accounts/dashboard?platform=mac&refresh=false'));
  const order = keys(sent);
  assert.ok(
    order.indexOf('analytics-motion') < order.indexOf('analytics'),
    'the motion comes just before the view'
  );
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
  assert.equal(
    controller.state().intervalSeconds,
    120,
    'the dashboard settings set the refresh interval'
  );
});

test('the display time zone is applied before the view is built', async () => {
  setDisplayTimeZone('America/New_York');
  const { controller } = make(
    defaultRoutes({
      '/api/accounts/preferences': () => ({ status: 200, body: { timeZone: 'Europe/Berlin' } }),
    })
  );
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
  assert.ok(
    server.calls.slice(before).some((p) => p.includes('range=30d')),
    'a 30-day range is read'
  );
  assert.equal(JSON.parse(last(sent, 'analytics')).state.range, '30d');
  // 24h is covered by the 30-day answer: drawn without a read
  const n = server.calls.length;
  controller.act('analytics-range', '24h');
  await settle();
  assert.equal(JSON.parse(last(sent, 'analytics')).state.range, '24h');
  assert.ok(
    !server.calls
      .slice(n)
      .some((p) => p.startsWith('/api/accounts/analytics') && p.includes('range=30d'))
  );
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
  const read = calls.findIndex(
    (p) => p.startsWith('/api/accounts/analytics') && p.includes('refresh=true')
  );
  assert.ok(dash >= 0 && read > dash, JSON.stringify(calls));
});

test('a session that ended stops the page: the frame is told once', async () => {
  const { controller, ended, sent } = make(
    defaultRoutes({
      '/api/accounts/analytics': () => ({
        status: 401,
        body: { code: 'session_revoked', error: 'Signed out.' },
      }),
    })
  );
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
      '/api/accounts/analytics': (url) =>
        fail
          ? { status: 500, body: { error: 'Analytics failed on the server.' } }
          : {
              status: 200,
              body: {
                ...analyticsFixture({
                  range: url.searchParams.get('range') || '7d',
                  now: NOW,
                  accounts: dashboard().accounts,
                }),
                activity: {
                  ...analyticsFixture({ range: '7d', now: NOW, accounts: [] }).activity,
                  refreshing: true,
                },
              },
            },
    })
  );
  controller.show();
  await settle();
  await settle();
  const drawn = last(sent, 'analytics');
  assert.ok(drawn);
  // the server is still reading: the page polls quietly
  assert.ok(
    c.pending().includes(60000) || c.pending().includes(10000),
    JSON.stringify(c.pending())
  );
  fail = true;
  const loadingBefore = sent.filter(([k]) => k === 'analytics-loading').length;
  await c.run(60000);
  await settle();
  // the answer in hand stays drawn (its rolling window follows the clock), never cleared to zeros
  const shown = JSON.parse(last(sent, 'analytics'));
  assert.ok(
    shown.usage.ready && shown.usage.kpis.length === 5 && shown.usage.kpis.every((k) => k.has),
    'the numbers on screen stay'
  );
  assert.equal(
    sent.filter(([k]) => k === 'analytics-loading').length,
    loadingBefore,
    'one failed poll says nothing'
  );
  controller.act('analytics-range', '30d');
  for (let i = 0; i < 4; i++) await settle();
  // numbers on screen: one message, in the strip (never "Updating" beside an error banner); the error is its tip
  assert.deepEqual(JSON.parse(last(sent, 'analytics-loading')), { loading: false, error: '' });
  const state = JSON.parse(last(sent, 'analytics-fresh'));
  assert.equal(state.failed, true);
  assert.equal(state.updating, false);
  assert.equal(state.stale, true);
  assert.match(state.note, /^Couldn't update · data from /);
  assert.match(state.tip, /Analytics failed on the server\./);
});

test('a failed read with nothing on screen says so in the banner', async () => {
  const { controller, sent } = make(
    defaultRoutes({
      '/api/accounts/analytics': () => ({
        status: 500,
        body: { error: 'Analytics failed on the server.' },
      }),
    })
  );
  controller.show();
  for (let i = 0; i < 4; i++) await settle();
  assert.deepEqual(JSON.parse(last(sent, 'analytics-loading')), {
    loading: false,
    error: 'Analytics failed on the server.',
  });
  assert.equal(last(sent, 'analytics-fresh'), undefined);
});

test('a server that just started answers loading first: the page reads again after 2 s, not 10 s', async () => {
  let reads = 0;
  const { controller, c, sent } = make(
    defaultRoutes({
      '/api/accounts/analytics': (url) => {
        reads++;
        if (reads === 1)
          return {
            status: 200,
            body: {
              schemaVersion: 1,
              accounts: [],
              range: { preset: '7d', from: new Date(NOW - 7 * D).toISOString() },
              activity: { status: 'loading', refreshing: true },
            },
          };
        return {
          status: 200,
          body: analyticsFixture({
            range: url.searchParams.get('range') || '7d',
            now: NOW,
            accounts: dashboard().accounts,
          }),
        };
      },
    })
  );
  controller.show();
  await settle();
  await settle();
  assert.equal(reads, 1);
  assert.ok(
    !JSON.parse(last(sent, 'analytics')).usage.ready,
    'no numbers yet: the page keeps its loading state'
  );
  assert.equal(c.pending()[0], 2000, JSON.stringify(c.pending()));
  await c.run(2000);
  await settle();
  assert.equal(reads, 2);
  assert.ok(JSON.parse(last(sent, 'analytics')).usage.ready, 'the numbers land on the second read');
});

/** An answer from a server still reading its logs: no totals yet. */
const stillLoading = () => ({
  status: 200,
  body: {
    schemaVersion: 1,
    accounts: [],
    range: { preset: '7d', from: new Date(NOW - 7 * D).toISOString() },
    activity: { status: 'loading', refreshing: true },
  },
});

test('a background poll reads the analytics only, and the header never says Refreshing for it (review L1)', async () => {
  const { controller, server, c, sent } = make(
    defaultRoutes({ '/api/accounts/analytics': stillLoading })
  );
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
  assert.ok(
    polled.every((p) => p.startsWith('/api/accounts/analytics?')),
    `a poll reads the analytics only: ${JSON.stringify(polled)}`
  );
  const lines = sent
    .slice(said)
    .filter(([k]) => k === 'chrome')
    .map(([, j]) => JSON.parse(j).statusLead);
  assert.ok(
    !lines.includes('Refreshing'),
    `the header stays put while polling: ${JSON.stringify(lines)}`
  );
});

test('the refresh interval and Refresh say Refreshing in the header while they read the dashboard', async () => {
  const { controller, c, sent } = make();
  controller.show();
  await settle();
  await settle();
  const leads = (from) =>
    sent
      .slice(from)
      .filter(([k]) => k === 'chrome')
      .map(([, j]) => JSON.parse(j).statusLead);
  let from = sent.length;
  await c.run(120_000);
  for (let i = 0; i < 4; i++) await settle();
  // (the 15 s header ticks before it say Updated too)
  let l = leads(from);
  assert.ok(
    l.includes('Refreshing') && l.at(-1) === 'Updated',
    `the refresh interval: ${JSON.stringify(l)}`
  );
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
    '/api/accounts/dashboard': () => ({
      status: 200,
      body: dashboard({
        settings: {
          refreshIntervalSeconds: 120,
          hiddenProviders: [],
          hiddenAccountIds: state.hidden,
        },
      }),
    }),
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
  assert.ok(
    views.every(([, j]) => !rowIds(j).includes('codex-1')),
    'no view drawn on return shows the hidden account'
  );
  assert.ok(
    !server.calls.slice(calls).some((p) => p.startsWith('/api/accounts/analytics')),
    'the young answer is not read again'
  );
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
  assert.equal(
    read.filter((p) => p.startsWith('/api/accounts/dashboard')).length,
    1,
    `one dashboard read for a burst: ${JSON.stringify(read)}`
  );
  assert.ok(
    !read.some((p) => p.startsWith('/api/accounts/analytics')),
    'no analytics read while the tab is away'
  );
  assert.ok(
    !rowIds(last(sent, 'analytics')).includes('codex-1'),
    'the hidden account is gone before the tab is shown'
  );
  assert.deepEqual(c.pending(), [], 'no timer left while the tab is away');
});

test('a successful read records when the session was last seen; a refused one does not (review L3)', async () => {
  const store = new Map();
  const storage = {
    setItem: (k, v) => store.set(k, String(v)),
    getItem: (k) => store.get(k) ?? null,
    removeItem: (k) => store.delete(k),
  };
  const { controller } = make(defaultRoutes(), { storage });
  controller.show();
  await settle();
  await settle();
  assert.equal(store.get('aac-session-seen-at'), String(NOW), 'seen at the read');
  store.clear();
  // a session the server no longer knows: every read is refused
  const refused = make(() => ({ status: 401, body: { code: 'auth_required' } }), { storage });
  refused.controller.show();
  await settle();
  await settle();
  assert.equal(
    store.get('aac-session-seen-at'),
    undefined,
    'a session the server refused is not marked seen'
  );
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
  assert.ok(
    !again.some((p) => p.startsWith('/api/accounts/analytics')),
    'a young answer that covers the range is drawn as it is'
  );
  assert.ok(
    again.includes('/api/accounts/dashboard?platform=mac&refresh=false'),
    'the dashboard beside it is read again'
  );
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

test('each installed controller has its own id, and a new one stops the one before (review L4)', () => {
  const listeners = new Set();
  const win = {
    location: { search: '' },
    navigator: { userAgent: 'test' },
    matchMedia: () => ({ matches: false }),
    fetch: async () => ({ ok: false, status: 503, json: async () => null }),
    localStorage: null,
    CustomEvent: class {
      constructor(type, init) {
        this.type = type;
        this.detail = init?.detail;
      }
    },
    dispatchEvent: () => true,
    document: {
      visibilityState: 'visible',
      addEventListener: (_, f) => listeners.add(f),
      removeEventListener: (_, f) => listeners.delete(f),
    },
  };
  const first = installFrameAnalytics(win);
  const second = installFrameAnalytics(win);
  assert.ok(Number.isInteger(first.id) && first.id >= 1, `an id from 1: ${first.id}`);
  assert.ok(second.id > first.id, 'a later install has a later id');
  assert.equal(win.aacFrameAnalytics, second);
  assert.equal(first.state().stopped, true, 'the one before is stopped');
  assert.equal(second.state().stopped, false);
  assert.equal(listeners.size, 1, 'only the current one follows the window');
  second.stop();
  assert.equal(listeners.size, 0);
});

test('analytics-page: covers, poll delay, thresholds and layout', () => {
  const page = createAnalyticsPage();
  const payload = { range: { from: new Date(NOW - 7 * D).toISOString(), preset: '7d' } };
  assert.equal(analyticsCovers(payload, page, NOW), true);
  page.range = '30d';
  assert.equal(analyticsCovers(payload, page, NOW), false);
  page.range = 'all';
  assert.equal(
    analyticsCovers({ range: { from: payload.range.from, preset: '30d' } }, page, NOW),
    true
  );
  assert.equal(analyticsCovers({}, page, NOW), false);

  assert.equal(analyticsPollDelay(null), 0);
  assert.equal(
    analyticsPollDelay({ activity: { status: 'loading' } }),
    10000,
    'bridge.js passes no wait: calm'
  );
  assert.equal(
    analyticsPollDelay({ activity: { status: 'loading' } }, 0),
    2000,
    'a page waiting for its first numbers reads again soon'
  );
  assert.equal(analyticsPollDelay({ activity: { status: 'loading' } }, 12_000), 5000);
  assert.equal(analyticsPollDelay({ activity: { status: 'loading' } }, 40_000), 10000);
  assert.equal(
    analyticsPollDelay({ activity: { status: 'loading', totals: {} } }, 0),
    60000,
    'numbers in hand: calm'
  );
  assert.equal(
    analyticsPollDelay({ activity: { status: 'cached', sources: [{ state: 'scanning' }] } }),
    15000
  );
  assert.equal(analyticsPollDelay({ activity: { status: 'loading', totals: {} } }), 60000);
  assert.equal(analyticsPollDelay({ activity: { status: 'ready', refreshing: false } }), 0);

  assert.deepEqual(analyticsThresholds(dashboard(), null), { codex: 90, antigravity: null });
  const ag = { enabled: true, thresholdUsedPercent: 80 };
  const two = dashboard({
    accounts: [
      { id: 'a', provider: 'antigravity' },
      { id: 'b', provider: 'antigravity' },
    ],
  });
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
  assert.deepEqual(
    applyAnalyticsAction(p, 'analytics-range', '7d'),
    {},
    'the same range changes nothing'
  );
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-range', '30d'), { range: true });
  const custom = applyAnalyticsAction(p, 'analytics-custom', '2026-11-09,2026-11-11', { now: NOW });
  assert.deepEqual(custom, { range: true });
  assert.equal(p.range, 'custom');
  assert.ok(Number.isFinite(p.from) && p.to > p.from);
  assert.deepEqual(
    applyAnalyticsAction(p, 'analytics-custom', '2025-01-01,2025-01-02', { now: NOW }),
    {},
    'older than 31 days is refused'
  );
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-provider', 'codex'), { render: 'morph' });
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-donut', 'cost'), { render: 'static' });
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-donut', 'colour'), {});
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-focus', 'a', { known: (id) => id === 'a' }), {
    render: 'static',
  });
  assert.ok(p.open.has('a'));
  applyAnalyticsAction(p, 'analytics-focus', 'a', { known: () => true });
  assert.ok(!p.open.has('a'), 'a second focus closes it');
  assert.deepEqual(applyAnalyticsAction(p, 'analytics-group', 'nope'), {});
});

// Post-landing fix (Astra/Sol max M1, the same class): the frame's controllers stay in the window after sign-out, so a
// read queued behind a side read must not go out with the next session's cookie once the controller has stopped.
test('a refresh still waiting on its side reads at stop sends no analytics read afterwards', async () => {
  let hold = false;
  let release;
  const routes = defaultRoutes({
    '/api/accounts/dashboard': async () => {
      if (hold) {
        hold = false;
        await new Promise((resolve) => {
          release = resolve;
        });
      }
      return { status: 200, body: dashboard() };
    },
  });
  const { controller, server } = make(routes);
  controller.show();
  for (let i = 0; i < 6; i++) await settle();
  hold = true;
  assert.equal(controller.act('analytics-refresh', ''), true);
  for (let i = 0; i < 4; i++) await settle();
  assert.ok(release, 'the forced refresh waits on its dashboard read');
  controller.stop();
  const before = server.calls.length;
  release();
  for (let i = 0; i < 8; i++) await settle();
  assert.deepEqual(
    server.calls.slice(before),
    [],
    'no GET /api/accounts/analytics?refresh=true with the next session'
  );
});

// ---------------------------------------------------------------- kept answers and the updating strip

/** A localStorage stand-in. */
function memoryStorage() {
  const items = new Map();
  return {
    items,
    getItem: (k) => (items.has(k) ? items.get(k) : null),
    setItem: (k, v) => items.set(k, String(v)),
    removeItem: (k) => items.delete(k),
  };
}
const fresh = (sent) => JSON.parse(last(sent, 'analytics-fresh') || 'null');

test('a second visit draws the kept answer before any network answer, said to be older, then the current one', async () => {
  const storage = memoryStorage();
  const first = make(defaultRoutes(), { storage });
  first.controller.show();
  await settle();
  await settle();
  assert.ok(storage.items.has('aac.analytics.saved.v1'), 'the first answer is kept');
  assert.equal(fresh(first.sent).stale, false);
  first.controller.stop();

  // the next visit: the server is slow to answer
  let answer;
  const held = new Promise((resolve) => {
    answer = resolve;
  });
  const second = make(defaultRoutes({ '/api/accounts/analytics': () => held }), { storage });
  second.c.run(0);
  await second.c.run(120_000);
  second.controller.show();
  // drawn at once, before any read has answered: the kept numbers, never the loading state's placeholders
  const view = JSON.parse(last(second.sent, 'analytics'));
  assert.ok(view.usage.ready && view.usage.kpis.length === 5);
  const shown = fresh(second.sent);
  assert.equal(shown.stale, true);
  assert.equal(shown.updating, true);
  assert.equal(shown.note, 'Updating · data from 2m ago');
  assert.match(shown.tip, /The numbers shown are from 2m ago; newer ones are on their way\./);
  // the server answers: the strip goes away and the numbers are current
  answer({
    status: 200,
    body: analyticsFixture({ range: '7d', now: second.c.now(), accounts: dashboard().accounts }),
  });
  await settle();
  await settle();
  await settle();
  const after = fresh(second.sent);
  assert.equal(after.stale, false);
  assert.equal(after.shown, after.updating, 'shown only while newer numbers are on their way');
});

test('an answer the server kept from before the read is said to be older and read again within seconds', async () => {
  const kept = (url) => ({
    status: 200,
    body: {
      ...analyticsFixture({
        range: url.searchParams.get('range') || '7d',
        now: NOW - 3 * 60_000,
        accounts: dashboard().accounts,
      }),
      asOf: new Date(NOW - 3 * 60_000).toISOString(),
      stale: true,
      refreshing: true,
    },
  });
  const { controller, sent, c, server } = make(defaultRoutes({ '/api/accounts/analytics': kept }));
  controller.show();
  await settle();
  await settle();
  const state = fresh(sent);
  assert.equal(state.stale, true);
  assert.equal(state.updating, true);
  assert.equal(state.note, 'Updating · data from 3m ago');
  // the next read comes in 2 s, not at the refresh interval
  const reads = () => server.calls.filter((p) => p.startsWith('/api/accounts/analytics')).length;
  const before = reads();
  await c.run(2_000);
  assert.equal(reads(), before + 1);
});

test('kept answers: written at most every 30 s and when the page leaves, never across platforms, too old or after sign-out', async () => {
  const { forgetSignIn } = await import('../public/auth-view.mjs');
  const storage = memoryStorage();
  let writes = 0;
  const counted = {
    ...storage,
    setItem: (k, v) => {
      if (k === 'aac.analytics.saved.v1') writes++;
      storage.setItem(k, v);
    },
  };
  const { controller, c } = make(defaultRoutes(), { storage: counted });
  controller.show();
  await settle();
  await settle();
  assert.equal(writes, 1);
  controller.act('analytics-range', '30d');
  await settle();
  await settle();
  assert.equal(writes, 1, 'a second answer within 30 s waits');
  controller.hide();
  assert.equal(writes, 2, 'leaving the page writes it');
  const record = JSON.parse(storage.getItem('aac.analytics.saved.v1'));
  assert.deepEqual(Object.keys(record.views).sort(), ['30d', '7d']);
  assert.equal(record.platform, 'mac');
  assert.equal(
    record.dashboard.schemaVersion,
    1,
    'the account list it was drawn with is kept beside it'
  );
  controller.stop();
  void c;

  // another platform's kept answer is not drawn
  const windows = make(() => new Promise(() => {}), { storage, platform: 'windows' });
  windows.controller.show();
  assert.equal(last(windows.sent, 'analytics'), undefined);
  windows.controller.stop();

  // one older than a week is not drawn
  const old = make(() => new Promise(() => {}), { storage });
  await old.c.run(8 * D);
  old.controller.show();
  assert.equal(last(old.sent, 'analytics'), undefined);
  old.controller.stop();

  // signed out: the kept answers leave this browser
  forgetSignIn(storage);
  assert.equal(storage.getItem('aac.analytics.saved.v1'), null);
});

test('offline with a kept answer: it stays on screen and the strip says it is not current', async () => {
  const storage = memoryStorage();
  const first = make(defaultRoutes(), { storage });
  first.controller.show();
  await settle();
  await settle();
  first.controller.stop();
  const second = make(() => 'offline', { storage });
  second.controller.show();
  await settle();
  await settle();
  await settle();
  const state = fresh(second.sent);
  assert.equal(state.stale, true);
  assert.equal(state.updating, false);
  assert.match(state.note, /^Offline · data from /);
  assert.ok(JSON.parse(last(second.sent, 'analytics')).usage.ready, 'the kept numbers stay');
});

test('current numbers while the server reads new log lines: the strip says so, the numbers are not dimmed', async () => {
  const reading = (url) => {
    const body = analyticsFixture({
      range: url.searchParams.get('range') || '7d',
      now: NOW,
      accounts: dashboard().accounts,
    });
    body.activity = { ...body.activity, refreshing: true, refreshingRemote: ['mac'] };
    return { status: 200, body: { ...body, asOf: body.updatedAt, stale: false, refreshing: true } };
  };
  const { controller, sent } = make(defaultRoutes({ '/api/accounts/analytics': reading }));
  controller.show();
  await settle();
  await settle();
  const state = fresh(sent);
  assert.equal(state.stale, false);
  assert.equal(state.updating, true);
  // the scan's progress in short (the fixture's Windows cells and Mac still scanning); the full words are the tip
  assert.match(state.note, /^Updating · \d+ of \d+ tools$/);
  assert.match(state.tip, /^Updating: new log lines are being read\./);
});

test('the server could not compute newer numbers: the strip says so once, reads again in 15 s, and a recovery clears it', async () => {
  let recovered = false;
  const answer = (url) => {
    const range = url.searchParams.get('range') || '7d';
    if (recovered) {
      const body = analyticsFixture({ range, now: NOW + 15_000, accounts: dashboard().accounts });
      body.activity = {
        ...body.activity,
        refreshing: false,
        sources: (body.activity.sources || []).map((r) => ({
          ...r,
          state: r.state === 'scanning' ? 'ok' : r.state,
        })),
      };
      return {
        status: 200,
        body: { ...body, asOf: body.updatedAt, stale: false, refreshing: false },
      };
    }
    const body = analyticsFixture({
      range,
      now: NOW - 25 * 60_000,
      accounts: dashboard().accounts,
    });
    body.activity = {
      ...body.activity,
      refreshing: false,
      sources: (body.activity.sources || []).map((r) => ({
        ...r,
        state: r.state === 'scanning' ? 'ok' : r.state,
      })),
    };
    return {
      status: 200,
      body: {
        ...body,
        asOf: body.updatedAt,
        stale: true,
        refreshing: false,
        refreshError: 'The newest numbers could not be computed; the last ones are shown.',
      },
    };
  };
  const { controller, sent, c, server } = make(
    defaultRoutes({ '/api/accounts/analytics': answer })
  );
  controller.show();
  await settle();
  await settle();
  const state = fresh(sent);
  assert.equal(state.failed, true);
  assert.equal(state.updating, false);
  assert.equal(state.note, "Couldn't update · data from 25m ago");
  assert.match(state.tip, /could not be computed/);
  // a calm read is armed in 15 s (never the 2/5/10 s steps, never only the refresh interval)
  assert.ok(c.pending().includes(15_000), JSON.stringify(c.pending()));
  assert.ok(!c.pending().some((ms) => ms > 0 && ms < 15_000), JSON.stringify(c.pending()));
  assert.equal(
    analyticsPollDelay({
      stale: true,
      refreshing: false,
      refreshError: 'x',
      activity: { status: 'ok' },
    }),
    15_000
  );
  // a failure that lasts: the next read is again 15 s on
  const reads = () => server.calls.filter((p) => p.startsWith('/api/accounts/analytics')).length;
  const before = reads();
  await c.run(15_000);
  assert.equal(reads(), before + 1);
  assert.equal(fresh(sent).failed, true);
  // the server recovers: the next short poll brings the newer numbers and the strip goes
  recovered = true;
  await c.run(15_000);
  await settle();
  assert.equal(reads(), before + 2);
  const after = fresh(sent);
  assert.equal(after.failed, false);
  assert.equal(after.shown, false);
});

test('after a failed read, another range says "Updating" while its read runs, not the old failure', async () => {
  let failing = true;
  let release;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  const routes = defaultRoutes({
    '/api/accounts/analytics': (url) => {
      const range = url.searchParams.get('range') || '7d';
      if (range === '30d')
        return held.then(() => ({
          status: 200,
          body: analyticsFixture({ range, now: NOW, accounts: dashboard().accounts }),
        }));
      if (failing && url.searchParams.get('refresh') === 'true')
        return { status: 500, body: { error: 'Analytics failed on the server.' } };
      const body = analyticsFixture({ range, now: NOW, accounts: dashboard().accounts });
      body.activity = {
        ...body.activity,
        refreshing: false,
        sources: (body.activity.sources || []).map((r) => ({
          ...r,
          state: r.state === 'scanning' ? 'ok' : r.state,
        })),
      };
      return { status: 200, body };
    },
  });
  const { controller, sent } = make(routes);
  controller.show();
  await settle();
  await settle();
  // Refresh fails: one message, the failure
  controller.act('analytics-refresh', '');
  for (let i = 0; i < 6; i++) await settle();
  assert.equal(fresh(sent).failed, true);
  // another range: its read is held; meanwhile the strip says it is updating
  controller.act('analytics-range', '30d');
  for (let i = 0; i < 4; i++) await settle();
  const during = fresh(sent);
  assert.equal(during.failed, false);
  assert.equal(during.updating, true);
  assert.match(during.note, /^Updating/);
  release();
  for (let i = 0; i < 6; i++) await settle();
  const after = fresh(sent);
  assert.equal(after.failed, false);
  assert.equal(after.updating, false);
  failing = false;
});

test('an answer timed ahead of this clock is from an earlier read, never "a moment ago"', async () => {
  const ahead = (url) => {
    const body = analyticsFixture({
      range: url.searchParams.get('range') || '7d',
      now: NOW + 60 * 60_000,
      accounts: dashboard().accounts,
    });
    return { status: 200, body: { ...body, asOf: body.updatedAt, stale: true, refreshing: true } };
  };
  const { controller, sent } = make(defaultRoutes({ '/api/accounts/analytics': ahead }));
  controller.show();
  await settle();
  await settle();
  const state = fresh(sent);
  assert.equal(state.stale, true);
  assert.match(state.note, /^Updating · (\d+ of \d+ tools · )?data from an earlier read$/);
  assert.doesNotMatch(state.note + state.tip, /moment/);
});

test('a kept answer saved mid-scan: while it is the one shown, only the read under way says "Updating"', async () => {
  const storage = memoryStorage();
  const midScan = (url) => {
    const body = analyticsFixture({
      range: url.searchParams.get('range') || '7d',
      now: NOW,
      accounts: dashboard().accounts,
    });
    body.activity = { ...body.activity, refreshing: true, refreshingRemote: ['mac', 'windows'] };
    return { status: 200, body };
  };
  const first = make(defaultRoutes({ '/api/accounts/analytics': midScan }), { storage });
  first.controller.show();
  await settle();
  await settle();
  first.controller.stop();
  // the next visit's read fails over HTTP: the strip says it could not update, not "Updating"
  const second = make(
    defaultRoutes({
      '/api/accounts/analytics': () => ({ status: 502, body: { error: 'Bad gateway.' } }),
    }),
    { storage }
  );
  second.controller.show();
  for (let i = 0; i < 4; i++) await settle();
  const state = fresh(second.sent);
  assert.equal(state.updating, false);
  assert.equal(state.failed, true);
  assert.match(state.note, /^Couldn't update · data from /);
  assert.deepEqual(JSON.parse(last(second.sent, 'analytics-loading')), {
    loading: false,
    error: '',
  });
});
