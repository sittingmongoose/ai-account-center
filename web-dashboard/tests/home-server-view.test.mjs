// D7: Home drawn from the server's view (GET /api/views/home, aac-views::home). The page asks for it only once a
// dashboard answer offers it (`server.views`), draws it once for the answer it came with, and keeps today's path in
// every other case: switch off, a view the server declined, an Open in progress, a route that is gone or failed.
// Synthetic data only; controller proof, not visual QA (the browser kit's Home checks run with the switch on and off).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fixtureServer, loadController, flush, sameView } from './fixtures/frame-dashboard-fixture.mjs';
import { dashboardViewModel } from '../public/view-model.mjs';
import { setDisplayTimeZone, getDisplayTimeZone } from '../public/time-format.mjs';

const MARK = 'drawn from the server view';
const markedView = () => ({
  version: 3,
  sections: [],
  cards: [],
  registry: [],
  chrome: { statusLead: MARK, statusStrong: '', statusMore: '', statusTip: '', refreshing: false, username: '', host: '' },
});

/**
 * The fixture server, offering Home from `/api/views/home` when `offer` is on. `home(reply)` shapes each answer of
 * that route: the reply GET /api/views/home would give ({schemaVersion, dashboard, profiles, antigravity, view}).
 */
function offering({ offer = true, home = (reply) => ({ payload: reply }), over = {} } = {}) {
  const { s, server: base } = fixtureServer(over);
  const asked = [];
  const dashboard = async (method, path) => {
    const answer = await base(method, path);
    if (offer && answer.payload?.schemaVersion === 1)
      answer.payload.server = { version: '0.0.0-test', commit: null, views: ['home'] };
    return answer;
  };
  const server = async (method, path, body) => {
    if (method === 'GET' && path.startsWith('/api/accounts/dashboard?')) return dashboard(method, path);
    if (method === 'GET' && path.startsWith('/api/views/home?')) {
      asked.push(path);
      // the switch off: the route is not offered, and the server answers as for any unknown endpoint
      if (!offer) return { status: 404, payload: { error: 'API endpoint was not found.' } };
      const answer = await dashboard('GET', path.replace('/api/views/home?', '/api/accounts/dashboard?'));
      if (answer.status && answer.status !== 200) return answer;
      const profiles = (await base('GET', '/api/claude/desktop-profiles')).payload.profiles;
      return home({ schemaVersion: 1, dashboard: answer.payload, profiles, antigravity: null, view: markedView() });
    }
    return base(method, path, body);
  };
  return { s, server, asked };
}

const drawn = (ctl) => JSON.parse(ctl.last('dashboard'));
const homeReads = (ctl) => ctl.calls.filter((c) => c.path.startsWith('/api/views/home?')).length;
const threeReads = (ctl) => ctl.calls.filter((c) => c.path.startsWith('/api/accounts/dashboard?')).length;

test('switch off: the dashboard offers nothing and the page never asks for the server view', async () => {
  const { server } = offering({ offer: false });
  const ctl = await loadController(server);
  try {
    await ctl.command('refresh');
    await ctl.command('refresh');
    assert.equal(homeReads(ctl), 0);
    assert.equal(threeReads(ctl), 3);
    assert.notEqual(drawn(ctl).chrome.statusLead, MARK);
  } finally { ctl.c.stop(); }
});

test('switch on: after the first answer offers it, a refresh is one read and Home is the server view, once', async () => {
  const { server } = offering();
  const ctl = await loadController(server);
  try {
    // the first read cannot know yet: the three answers, the page's own view
    assert.equal(homeReads(ctl), 0);
    assert.notEqual(drawn(ctl).chrome.statusLead, MARK);
    // the start's last 60 s timer is the redraw tick (each refresh then sets a new 60 s usage read after it)
    const tick = ctl.intervals.findLast((i) => i.ms === 60_000);
    const before = ctl.calls.length;
    await ctl.command('refresh');
    const after = ctl.calls.slice(before).map((c) => c.path);
    assert.deepEqual(after.filter((p) => /dashboard|desktop-profiles\b(?!\/)|antigravity\/profiles|views\/home/.test(p)), ['/api/views/home?platform=mac&refresh=true']);
    assert.equal(drawn(ctl).chrome.statusLead, MARK, 'the server view is drawn as it came');
    // the page context travels as one URI-encoded JSON header; the server reads only these keys
    const sent = ctl.headers.findLast((h) => h.path.startsWith('/api/views/home?')).headers['X-AAC-Home-Context'];
    assert.match(sent, /^[\x21-\x7e]+$/, 'ASCII on the wire');
    const context = JSON.parse(decodeURIComponent(sent));
    assert.deepEqual(Object.keys(context), ['now', 'timeZone', 'platform', 'intervalSeconds', 'username', 'host', 'formatProbe']);
    assert.equal(context.timeZone, 'UTC');
    assert.equal(context.platform, 'mac');
    assert.equal(context.intervalSeconds, 60);
    assert.equal(context.username, 'owner');
    assert.equal(context.formatProbe.length, 8);
    // between refreshes the clock tick draws the page's own view of the same answer
    tick.fn();
    await flush();
    assert.notEqual(drawn(ctl).chrome.statusLead, MARK);
    assert.equal(drawn(ctl).chrome.statusLead, 'Updated');
  } finally { ctl.c.stop(); }
});

test('a declined view (view null) is the page\'s own view of the same answer', async () => {
  const { server } = offering({ home: (reply) => ({ payload: { ...reply, view: null } }) });
  const ctl = await loadController(server);
  try {
    await ctl.command('refresh');
    assert.equal(homeReads(ctl), 1);
    const state = ctl.c.state?.() ?? {};
    assert.ok(state, 'controller state');
    const own = drawn(ctl);
    assert.notEqual(own.chrome.statusLead, MARK);
    assert.equal(own.version, 3);
    assert.ok(own.sections.some((s) => s.id === 'codex' && s.rows.length === 2), 'the Codex rows of the fixture');
  } finally { ctl.c.stop(); }
});

test('the server view and the page view of one answer are the same JSON when the server proves it (the spec\'s rule)', async () => {
  // The page passes the server's JSON through unchanged: what aac-views proves equal is what is drawn.
  let replyView = null;
  const { server } = offering({
    home: (reply) => {
      replyView = dashboardViewModel(reply.dashboard, { now: Date.now(), profiles: reply.profiles, platform: 'mac', antigravityInventory: null, intervalSeconds: 60, username: 'owner', host: '192.0.2.10:3000' });
      return { payload: { ...reply, view: replyView } };
    },
  });
  const ctl = await loadController(server);
  try {
    await ctl.command('refresh');
    sameView(ctl.last('dashboard'), JSON.stringify(replyView), 'drawn as sent');
  } finally { ctl.c.stop(); }
});

for (const [label, answer] of [
  ['gone after a restart with the switch off (404)', { status: 404, payload: { code: 'not_found' } }],
  ['failed this time (500)', { status: 500, payload: { error: 'Account usage could not be read safely.' } }],
  ['refused (403)', { status: 403, payload: { code: 'device_scope' } }],
  ['an old server\'s 400', { status: 400, payload: { error: 'The page context for Home could not be read.' } }],
]) {
  test(`a server view that is ${label}: the same refresh reads the three answers, and asks again once offered`, async () => {
    let failing = true;
    const { server } = offering({ home: (reply) => (failing ? answer : { payload: reply }) });
    const ctl = await loadController(server);
    try {
      const toastsBefore = ctl.toasts().length;
      await ctl.command('refresh');
      assert.equal(homeReads(ctl), 1);
      assert.equal(threeReads(ctl), 2, 'the start, then the fallback of this refresh');
      assert.notEqual(drawn(ctl).chrome.statusLead, MARK);
      assert.equal(ctl.toasts().length, toastsBefore, 'no failure is reported: the readings arrived');
      failing = false;
      await ctl.command('refresh');
      assert.equal(homeReads(ctl), 2, 'the fallback answer offered it again');
      assert.equal(drawn(ctl).chrome.statusLead, MARK);
    } finally { ctl.c.stop(); }
  });
}

test('a 401 from the server view ends the session as any read does; nothing is read again behind it', async () => {
  const { s, server } = offering();
  const ctl = await loadController(server);
  try {
    s.signedIn = false;
    const before = threeReads(ctl);
    await ctl.command('refresh');
    assert.equal(homeReads(ctl), 1);
    assert.equal(threeReads(ctl), before, 'no fallback reads after a sign-out');
    assert.ok(ctl.last('signed-out'), 'the page is signed out');
  } finally { ctl.c.stop(); }
});

test('a Claude Open in progress: its row line is the page\'s, so the page draws its own view', async () => {
  const { server } = offering({
    over: {
      accounts: (updated) => [
        { id: 'claude:a', provider: 'claude', email: 'a@example.test', status: 'ok', sampledAt: updated, windows: [], capabilities: { claudeProfileId: 'a', claudePlatforms: ['mac'] } },
      ],
      routes: {
        'GET /api/claude/desktop-profiles': () => ({ payload: { profiles: [{ id: 'a', name: 'A', openOperation: { id: 'op-1', state: 'copying', platform: 'mac', confirmedCount: 2, totalCount: 9 } }] } }),
      },
    },
  });
  const ctl = await loadController(server);
  try {
    await ctl.command('refresh');
    assert.equal(homeReads(ctl), 1);
    assert.notEqual(drawn(ctl).chrome.statusLead, MARK);
  } finally { ctl.c.stop(); }
});

test('a remembered offer: the first read after a reload waits for the saved zone, then asks for the server view', async () => {
  const { server } = offering();
  const stored = new Map();
  const first = await loadController(server, { stored });
  first.c.stop();
  assert.equal(stored.get('aac.homeServerView'), '1', 'the offer is remembered');
  const ctl = await loadController(server, { stored });
  try {
    const order = ctl.calls.map((c) => c.path.split('?')[0]);
    assert.equal(threeReads(ctl), 0, 'no dashboard read on the first refresh');
    assert.ok(order.indexOf('/api/accounts/preferences') < order.indexOf('/api/views/home'), 'the zone is read first');
    const context = JSON.parse(decodeURIComponent(ctl.headers.find((h) => h.path.startsWith('/api/views/home?')).headers['X-AAC-Home-Context']));
    assert.equal(context.timeZone, 'UTC', 'the saved zone, not the default');
    assert.equal(drawn(ctl).chrome.statusLead, MARK, 'the first draw is the server view');
  } finally { ctl.c.stop(); }
});

test('a remembered offer the server no longer makes (switch off): one fallback, then nothing is stored or asked', async () => {
  const { server } = offering({ offer: false });
  const stored = new Map([['aac.homeServerView', '1']]);
  const ctl = await loadController(server, { stored });
  try {
    assert.equal(homeReads(ctl), 1);
    assert.equal(threeReads(ctl), 1);
    assert.equal(stored.has('aac.homeServerView'), false);
    assert.equal(ctl.toasts().filter((t) => t.kind === 'err').length, 0);
    await ctl.command('refresh');
    assert.equal(homeReads(ctl), 1, 'not asked again');
  } finally { ctl.c.stop(); }
});

test('a view built in a zone the page left while it was read is not drawn: the page draws its own', async () => {
  let release = null;
  const { server } = offering({
    home: async (reply) => {
      await new Promise((resolve) => { release = resolve; });
      return { payload: reply };
    },
  });
  const zone = getDisplayTimeZone();
  const ctl = await loadController(server);
  try {
    assert.ok(ctl.c.command('refresh'));
    await flush();
    assert.ok(release, 'the server view is in flight');
    setDisplayTimeZone('Asia/Kolkata');
    release();
    await flush(16);
    assert.equal(homeReads(ctl), 1);
    assert.notEqual(drawn(ctl).chrome.statusLead, MARK);
  } finally {
    ctl.c.stop();
    setDisplayTimeZone(zone);
  }
});
