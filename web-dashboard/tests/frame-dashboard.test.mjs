// The app frame's dashboard controller (public/frame-dashboard.mjs) against bridge.js, the code it replaces in the
// frame. The differential tests drive the real bridge.js, framed exactly as the app frame's hidden iframe ran it, and the
// controller through the same actions, and require the same server calls (method, path and body, in order) and the
// same views, toasts and header line. The rest ports bridge-session.test.mjs's guards: switch approvals, the held
// changes, sign-out, the session rotation, an ended session, open access, Update apps and the Back entries.
// Synthetic data only; controller and data proof, not visual QA.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createFrameDashboard, installFrameDashboard, DASHBOARD_ACTIONS, FRAME_COMMANDS } from '../public/frame-dashboard.mjs';
import { PAGE_ACTIONS } from '../public/frame-seam.mjs';
import { ORIGIN, TOKEN, flush, fixtureServer, respond, loadController, sameOut, sameView, writes, reads, antigravityServer } from './fixtures/frame-dashboard-fixture.mjs';

let loads = 0;

/** bridge.js as the app frame's hidden iframe ran it (`/?content=1` under a frame window), subscribed for its overlays. */
async function loadFramedBridge(server, { width = 1440 } = {}) {
  const base = new URL('../public/', import.meta.url);
  let source = await readFile(new URL('bridge.js', base), 'utf8');
  const first = source.split('\n')[0];
  const declarations = first.match(/\{ ([^}]+) \}/)[1];
  const key = `__frameDashboardWasm${++loads}`;
  source = source.replace(first, `const init = async () => {}; const { ${declarations} } = globalThis.${key};`)
    .replace(/import \{ requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard \} from '[^']+';/, 'const requireWebGL = () => {}; const WEBGL_REQUIRED_MESSAGE = "fixture"; const startSlintDashboard = fn => fn();')
    .replace(/from '(\.\/[^']+)'/g, (_match, relative) => `from '${new URL(relative, base).href}'`)
    .concat(`\n// ${key}\n`);
  const posted = [], calls = [], listeners = new Map(), historyCalls = [], intervals = [];
  const frameWindow = { __aacFrame: true, innerWidth: width, postMessage: (message) => posted.push(message) };
  const originals = new Map();
  const assign = (name, value) => {
    if (!originals.has(name)) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  };
  const restore = () => { for (const [name, descriptor] of originals) descriptor ? Object.defineProperty(globalThis, name, descriptor) : delete globalThis[name]; };
  assign(key, Object.fromEntries(declarations.split(', ').map((name) => [name, () => {}])));
  assign('window', {});
  assign('navigator', { userAgent: 'Mac fixture' });
  assign('location', { pathname: '/', search: '?content=1', protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN, href: '' });
  assign('document', { querySelector: () => ({ hidden: false, textContent: '' }), getElementById: () => null });
  assign('innerWidth', 0); assign('innerHeight', 900); assign('devicePixelRatio', 1);
  assign('parent', frameWindow); assign('top', frameWindow); assign('self', globalThis);
  assign('addEventListener', (name, fn) => listeners.set(name, [...(listeners.get(name) || []), fn])); assign('matchMedia', () => ({ matches: false }));
  assign('history', { pushState: (...args) => historyCalls.push(['push', ...args]), replaceState: () => {}, back: () => historyCalls.push(['back']) });
  const stored = new Map();
  assign('localStorage', { getItem: (k) => (stored.has(k) ? stored.get(k) : null), setItem: (k, v) => stored.set(k, String(v)), removeItem: (k) => stored.delete(k) });
  assign('setInterval', (fn, ms) => { intervals.push({ fn, ms }); return intervals.length; }); assign('clearInterval', () => {});
  assign('requestAnimationFrame', () => 0); assign('cancelAnimationFrame', () => {});
  assign('fetch', async (path, options = {}) => {
    const method = options.method || 'GET';
    const body = options.body ? JSON.parse(options.body) : null;
    calls.push({ method, path, body });
    return respond(await server(method, path, body));
  });
  try {
    await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
  } catch (error) { restore(); throw error; }
  await flush(16);
  const fromFrame = (data) => { for (const fn of listeners.get('message') || []) fn({ origin: ORIGIN, source: frameWindow, data }); };
  // the frame subscribed for its overlays and Analytics (dash.rs before G5)
  fromFrame({ type: 'aac-views', overlays: ['details', 'activation', 'analytics'] });
  posted.length = 0;
  let id = 0;
  return {
    calls, posted, historyCalls, intervals, restore,
    act: async (kind, value = '') => { fromFrame({ type: 'aac-page', kind, value }); await flush(16); },
    command: async (action, value = '') => { fromFrame({ type: 'aac-frame', action, value, id: ++id }); await flush(16); },
    // what the frame received: views, toasts and the header line, in order
    out: () => posted.filter((m) => m.type === 'aac-content' && ['view', 'toast', 'chrome'].includes(m.kind)).map((m) =>
      m.kind === 'view' ? ['view', m.key, m.json] : m.kind === 'toast' ? ['toast', m.toastKind, m.title, m.body] : ['chrome', m.statusLead, m.statusStrong, m.statusMore]),
  };
}

/** One step through both: the server calls and what the frame received, from this step only. */
async function step(bridge, ctl, run) {
  const [bc, bo, cc, co] = [bridge.calls.length, bridge.out().length, ctl.calls.length, ctl.out().length];
  await run(bridge, ctl);
  return {
    bridge: { calls: bridge.calls.slice(bc), out: bridge.out().slice(bo) },
    ctl: { calls: ctl.calls.slice(cc), out: ctl.out().slice(co) },
  };
}

test('the controller runs only the seam\'s page actions and the header\'s Refresh and Update apps', () => {
  for (const kind of PAGE_ACTIONS) assert.equal(DASHBOARD_ACTIONS.has(kind), !kind.startsWith('analytics-'), kind);
  // sign-in, the frame's navigation, theme and sign-out never go through a page action
  for (const kind of ['login', 'setup', 'navigate', 'theme', 'logout', 'auth-recheck', 'overlay-open', 'content-rect', 'refresh', 'update-apps', 'analytics-range'])
    assert.equal(DASHBOARD_ACTIONS.has(kind), false, kind);
  assert.deepEqual([...FRAME_COMMANDS].sort(), ['refresh', 'update-apps']);
  const c = createFrameDashboard({ fetch: async () => { throw new Error('nothing is read'); } });
  for (const kind of ['login', 'navigate', 'logout', 'analytics-range', 'activate ', '']) assert.equal(c.act(kind, 'x'), false, kind);
  assert.equal(c.command('logout'), false);
  assert.equal(c.command('theme'), false);
});

test('differential: the start reads what bridge.js read and Home gets the same view; nothing before the answer', async () => {
  const a = fixtureServer(), b = fixtureServer();
  const bridge = await loadFramedBridge(a.server);
  const holdDashboard = { release: null };
  const gated = async (method, path, body) => {
    if (path.startsWith('/api/accounts/dashboard?') && !holdDashboard.release) await new Promise((resolve) => { holdDashboard.release = resolve; });
    return b.server(method, path, body);
  };
  const starting = loadController(gated);
  await flush(16);
  try {
    // before the dashboard answers, the page has only its loading state: no dashboard view, no numbers
    assert.ok(holdDashboard.release, 'the dashboard read is in flight');
    const ctlEarly = (await Promise.race([starting, Promise.resolve(null)]));
    assert.equal(ctlEarly, null);
    holdDashboard.release();
    const ctl = await starting;
    const bridgeReads = new Set(reads(bridge.calls));
    const ctlReads = new Set(reads(ctl.calls));
    assert.deepEqual([...ctlReads].sort(), [...bridgeReads].sort(), 'the same reads at start');
    ctl.c.stop();
  } finally { bridge.restore(); }
});

test('differential: Details, a Codex switch with its one-use approval, auto-switch, the interval and Update apps', async () => {
  const a = fixtureServer(), b = fixtureServer();
  const bridge = await loadFramedBridge(a.server);
  const ctl = await loadController(b.server);
  try {
    // Home as both have drawn it: the same view (bridge.js's came before the subscription; read it from a refresh)
    const refreshed = await step(bridge, ctl, async (x, y) => { await x.command('refresh'); await y.command('refresh'); });
    assert.deepEqual(refreshed.ctl.calls, refreshed.bridge.calls, 'Refresh: the same calls');
    sameOut(refreshed.ctl.out, refreshed.bridge.out, 'Refresh: the same views and header line');
    const script = [
      ['details', 'codex:other'],
      ['details', 'codex:one'],
      ['details-closed', ''],
      ['details', 'codex:other'],
      ['activate', 'other'],
      ['activation-confirm', ''],
      ['activation-confirm', ''],
      ['automatic', 'true'],
      ['threshold', '80%'],
      ['refresh-interval', '2m'],
      ['launch', 'nobody:mac'],
      ['remove', 'codex:other'],
      ['activate', 'one'],
    ];
    for (const [kind, value] of script) {
      const r = await step(bridge, ctl, async (x, y) => { await x.act(kind, value); await y.act(kind, value); });
      assert.deepEqual(r.ctl.calls, r.bridge.calls, `${kind} ${value}: the same server calls`);
      sameOut(r.ctl.out, r.bridge.out, `${kind} ${value}: the same views, toasts and header line`);
    }
    // the approval went once, with the token, after the 409; the second confirm sent nothing
    assert.equal(a.s.attempts, 2);
    assert.equal(b.s.attempts, 2);
    const confirmed = writes(ctl.calls).filter((c) => c.path === '/api/codex/profiles/other/activate');
    assert.deepEqual(confirmed.map((c) => c.body), [{}, { confirmationToken: TOKEN }]);
    // the token never reached a view
    assert.equal(JSON.stringify(ctl.emitted).includes(TOKEN), false);
    const update = await step(bridge, ctl, async (x, y) => { await x.command('update-apps'); await y.command('update-apps'); });
    assert.deepEqual(update.ctl.calls, update.bridge.calls, 'Update apps: the same calls (a 409 job adopted)');
    sameOut(update.ctl.out, update.bridge.out, 'Update apps: the same views');
  } finally { ctl.c.stop(); bridge.restore(); }
});

test('differential: Accounts & Settings, its reads on entry and a held Remove while a switch waits', async () => {
  const a = fixtureServer(), b = fixtureServer();
  const bridge = await loadFramedBridge(a.server);
  const ctl = await loadController(b.server);
  try {
    const entered = await step(bridge, ctl, async (x, y) => { await x.command('navigate', 'accounts'); y.c.page('accounts'); await flush(16); });
    assert.deepEqual(reads(entered.ctl.calls).sort(), reads(entered.bridge.calls).sort(), 'entering Accounts reads the same blocks');
    const views = (o, key) => o.filter((x) => x[0] === 'view' && x[1] === key).map((x) => x[2]);
    sameView(views(entered.ctl.out, 'accounts').at(-1), views(entered.bridge.out, 'accounts').at(-1), 'the same Accounts view');
    for (const [kind, value] of [['activate', 'other'], ['remove', 'codex:other'], ['accounts-show', 'zai:hide'], ['pw-toggle', ''], ['activation-cancel', ''], ['remove', 'codex:other'], ['line-cancel', 'codex:other']]) {
      const r = await step(bridge, ctl, async (x, y) => { await x.act(kind, value); await y.act(kind, value); });
      assert.deepEqual(r.ctl.calls, r.bridge.calls, `${kind} ${value}: the same server calls`);
      sameOut(r.ctl.out, r.bridge.out, `${kind} ${value}: the same views and toasts`);
    }
    // Remove waited for the switch, then asked once it was cancelled
    assert.equal(ctl.calls.filter((c) => c.path === '/api/accounts/codex%3Aother/remove').length, 1);
  } finally { ctl.c.stop(); bridge.restore(); }
});

test('differential: Antigravity, its separate approval, the held Codex switch and the confirmed auto settings', async () => {
  const a = antigravityServer(), b = antigravityServer();
  const bridge = await loadFramedBridge(a.server);
  const ctl = await loadController(b.server);
  try {
    const script = [
      ['antigravity-automatic', 'true'],
      ['antigravity-pool', 'Gemini models (reported group)'],
      ['antigravity-automatic', 'true'],
      ['antigravity-threshold', '85%'],
      ['antigravity-cooldown', '600'],
      ['antigravity-activate', 'party'],
      ['activate', 'other'],
      ['antigravity-automatic', 'false'],
      ['activation-cancel', ''],
      ['antigravity-activate', 'party'],
      ['activation-confirm', ''],
      ['activation-confirm', ''],
    ];
    for (const [kind, value] of script) {
      const r = await step(bridge, ctl, async (x, y) => { await x.act(kind, value); await y.act(kind, value); });
      assert.deepEqual(r.ctl.calls, r.bridge.calls, `${kind} ${value}: the same server calls`);
      sameOut(r.ctl.out, r.bridge.out, `${kind} ${value}`);
    }
    // one confirmation, with its own token, and never a Codex write while the Antigravity approval waited
    assert.deepEqual(b.confirms, [{ hostId: 'ubuntu', confirmationToken: b.token }]);
    assert.equal(writes(ctl.calls).some((c) => c.path.startsWith('/api/codex')), false);
    assert.deepEqual(writes(ctl.calls).filter((c) => c.method === 'PUT').map((c) => c.body), [{ requestedPoolId: 'reported@gemini' }, { enabled: true }, { thresholdUsedPercent: 85 }, { cooldownSeconds: 600 }]);
    assert.equal(JSON.stringify(ctl.emitted).includes(b.token), false, 'the token never reaches a view');
  } finally { ctl.c.stop(); bridge.restore(); }
});

test('a pending switch holds Accounts changes and Sign out; a local action stays live; cancel releases them', async () => {
  const { s, server } = fixtureServer();
  const ctl = await loadController(server);
  try {
    await ctl.act('activate', 'other');
    assert.ok(ctl.last('activation'), 'the approval is on screen');
    await ctl.act('remove', 'codex:other');
    assert.equal(ctl.toasts().at(-1).title, 'Finish the account switch first');
    const held = await ctl.c.signOut();
    assert.deepEqual(held, { ok: false, held: true });
    assert.equal(s.signedIn, true, 'Sign out waited');
    assert.equal(ctl.calls.some((c) => c.method !== 'GET' && (c.path === '/api/auth/logout' || c.path.endsWith('/remove'))), false);
    const before = ctl.toasts().length;
    await ctl.act('pw-toggle', '');
    assert.equal(ctl.toasts().length, before, 'opening the password form is not held');
    await ctl.act('activation-cancel', '');
    assert.equal(ctl.toasts().at(-1).title, 'Switch canceled');
    assert.equal(ctl.last('activation'), '');
    const out = await ctl.c.signOut();
    assert.deepEqual(out, { ok: true });
    assert.equal(s.signedIn, false);
    assert.deepEqual(JSON.parse(ctl.last('signed-out')), { state: 'default', reason: '' });
  } finally { ctl.c.stop(); }
});

test('Sign out runs while a save is in flight; a failure is said in the page\'s words and keeps the session', async () => {
  let release = null;
  const { s, server } = fixtureServer({ routes: {
    'PUT /api/accounts/settings': (body) => new Promise((resolve) => { release = () => resolve({ payload: { refreshIntervalSeconds: body.refreshIntervalSeconds } }); }),
  } });
  const ctl = await loadController(server);
  try {
    assert.equal(ctl.c.act('refresh-interval', '120s'), true);
    await flush();
    assert.ok(release, 'the interval save is in flight (busy)');
    assert.deepEqual(await ctl.c.signOut(), { ok: true });
    assert.equal(s.signedIn, false);
    release();
    await flush();
  } finally { ctl.c.stop(); }
  const failing = fixtureServer({ routes: { 'POST /api/auth/logout': () => ({ status: 500, payload: { error: 'Failed to logout' } }) } });
  const c = await loadController(failing.server);
  try {
    assert.deepEqual(await c.c.signOut(), { ok: false });
    const toast = c.toasts().at(-1);
    assert.deepEqual([toast.kind, toast.title, toast.body], ['err', 'Not signed out', 'Sign-out failed on the dashboard. Try again.']);
    assert.doesNotMatch(JSON.stringify(c.toasts()), /Failed to logout/);
    assert.equal(c.last('signed-out'), undefined, 'still signed in');
    assert.equal(c.c.state().authenticated, true);
  } finally { c.c.stop(); }
});

test('Sign out other browsers waits for requests in flight and holds new ones until the new session is set', async () => {
  let finishRefresh = null;
  const order = [];
  const { server: base } = fixtureServer();
  const server = async (method, path, body) => {
    if (method === 'GET' && path.startsWith('/api/accounts/dashboard?') && finishRefresh === null && order.includes('armed')) {
      order.push('refresh-started');
      await new Promise((resolve) => { finishRefresh = resolve; });
      order.push('refresh-finished');
    }
    if (method === 'POST' && path === '/api/auth/sessions/revoke-others') { order.push('revoke'); return { payload: { signedOutBrowsers: 1 } }; }
    if (method === 'GET' && path === '/api/auth/session' && order.includes('revoke')) order.push('session-after');
    return base(method, path, body);
  };
  const ctl = await loadController(server, { page: 'accounts' });
  try {
    order.push('armed');
    assert.equal(ctl.c.command('refresh'), true);
    await flush();
    assert.equal(ctl.c.act('others-out', ''), true);
    await flush();
    assert.ok(!order.includes('revoke'), 'the rotation waits for the refresh in flight');
    finishRefresh();
    await flush(24);
    assert.ok(order.indexOf('revoke') > order.indexOf('refresh-finished'));
    assert.ok(order.indexOf('session-after') > order.indexOf('revoke'), 'reads after it wait for the new session');
    assert.equal(ctl.last('signed-out'), undefined);
  } finally { ctl.c.stop(); }
});

test('a session that ends under the page says why once, and forgets what it read', async () => {
  for (const [revoked, reason] of [[true, 'revoked'], [false, 'ended']]) {
    const { s, server } = fixtureServer();
    const ctl = await loadController(server);
    try {
      ctl.stored.set('aac-signin-at', String(Date.now() - 60_000));
      if (revoked) s.revoked = true; else s.signedIn = false;
      await ctl.command('refresh');
      const ends = ctl.emitted.filter((e) => e.key === 'signed-out');
      assert.equal(ends.length, 1);
      const end = JSON.parse(ends[0].json);
      assert.equal(end.state, 'expired');
      if (revoked) assert.equal(end.reason, reason);
      else assert.ok(['ended', 'expired'].includes(end.reason));
      assert.equal(ctl.c.state().authenticated, false);
      assert.equal(ctl.c.state().hasData, false);
      // nothing runs after it
      const calls = ctl.calls.length;
      await ctl.act('activate', 'other');
      assert.equal(ctl.calls.length, calls);
      assert.equal(ctl.toasts().some((t) => t.title === 'Refresh failed'), false, 'no failure toast over the sign-in');
    } finally { ctl.c.stop(); }
  }
});

test('open access keeps Home usable and Accounts explicitly restricted without ending a session', async () => {
  const server = async (method, path) => {
    if (path === '/api/auth/check') return { payload: { authenticated: false, authRequired: false, accessMode: 'open' } };
    return { status: 401, payload: { error: 'Authentication required', code: 'auth_required' } };
  };
  const ctl = await loadController(server, { page: 'accounts' });
  try {
    const home = JSON.parse(ctl.last('dashboard'));
    assert.equal(home.chrome.statusLead, 'Dashboard sign-in is off');
    assert.deepEqual([home.sections, home.cards], [[], []]);
    assert.match(JSON.parse(ctl.last('accounts')).accessMessage, /Accounts & Settings needs dashboard sign-in/);
    assert.equal(ctl.emitted.some((e) => e.key === 'signed-out'), false);
    assert.equal(ctl.calls.some((c) => c.path.includes('/api/accounts/preferences') || c.path.includes('/api/accounts/dashboard')), false);
  } finally { ctl.c.stop(); }
});

test('Update apps: a validated 409 job is adopted and its finish reported once; a malformed one is refused', async () => {
  const { server } = fixtureServer();
  const id = '11111111-1111-4111-8111-111111111111';
  let job = null;
  const ctl = await loadController((method, path, body) => (path === '/api/app-updates/status' ? { payload: { job } } : server(method, path, body)));
  try {
    await ctl.command('update-apps');
    job = { id, state: 'failed', results: [{ status: 'unknown' }, { status: 'held' }] };
    ctl.intervals.find((t) => t.ms === 3_000).fn();
    await flush(16);
    const finishes = () => ctl.toasts().filter((t) => /App (updates|checks) finished/.test(t.title));
    assert.equal(finishes().length, 1);
    assert.equal(finishes()[0].kind, 'err');
    assert.match(finishes()[0].body, /0 updated, 0 failed, 1 unknown, 1 held/);
    ctl.c.focused();
    ctl.intervals.find((t) => t.ms === 30_000).fn();
    await flush(16);
    assert.equal(finishes().length, 1);
  } finally { ctl.c.stop(); }
  const bad = fixtureServer({ routes: { 'POST /api/app-updates/start': () => ({ status: 409, payload: { error: 'Already running', job: { id: 'invalid', state: 'running', results: [] } } }) } });
  const c = await loadController(bad.server);
  try {
    await c.command('update-apps');
    assert.ok(c.toasts().some((t) => t.kind === 'err'));
  } finally { c.c.stop(); }
});

test('the timers bridge.js kept: the interval read waits for a switch, the approval expires, Accounts reads on entry', async () => {
  const { server } = fixtureServer();
  const ctl = await loadController(server);
  try {
    const dashboardReads = () => ctl.calls.filter((c) => c.path.startsWith('/api/accounts/dashboard?')).length;
    await ctl.act('activate', 'other');
    const before = dashboardReads();
    for (const t of ctl.intervals.filter((x) => x.ms === 60_000)) t.fn();
    await flush(16);
    assert.equal(dashboardReads(), before, 'no interval read while the approval waits');
    for (const t of ctl.intervals.filter((x) => x.ms === 1_000)) t.fn();
    await ctl.act('activation-cancel', '');
    for (const t of ctl.intervals.filter((x) => x.ms === 60_000)) t.fn();
    await flush(16);
    assert.ok(dashboardReads() > before, 'the interval reads again once it is answered');
    const updates = () => ctl.calls.filter((c) => c.path === '/api/app-updates/status').length;
    const registry = () => ctl.calls.filter((c) => c.path === '/api/accounts/registry').length;
    const [u, r] = [updates(), registry()];
    ctl.c.page('accounts');
    await flush(16);
    assert.equal(updates(), u + 1);
    assert.ok(registry() > r);
    assert.ok(JSON.parse(ctl.last('accounts')).version === 3);
    // the frame keeps the tab this device showed last (frame.rs show), for every tab: the controller leaves it alone
    assert.equal(ctl.stored.get('aac-last-page'), undefined);
  } finally { ctl.c.stop(); }
});

test('below 1280 px Details and the approval take one Back entry; Back closes the top one; desktop takes none', async () => {
  for (const width of [1920, 900]) {
    for (const end of ['cancel', 'back', 'success']) {
      const { s, server } = fixtureServer();
      const listeners = new Map();
      const historyCalls = [];
      let state = null;
      const win = {
        location: { protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN, search: '', href: '' },
        navigator: { userAgent: 'Mac fixture' },
        innerWidth: width,
        localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
        fetch: async (path, options = {}) => respond(await server(options.method || 'GET', path, options.body ? JSON.parse(options.body) : null)),
        CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init.detail; } },
        dispatchEvent: (event) => { for (const fn of listeners.get(event.type) || []) fn(event); },
        addEventListener: (name, fn) => listeners.set(name, [...(listeners.get(name) || []), fn]),
        removeEventListener: (name, fn) => listeners.set(name, (listeners.get(name) || []).filter((f) => f !== fn)),
        history: {
          get state() { return state; },
          pushState: (s2) => { state = s2; historyCalls.push('push'); },
          back: () => { state = null; historyCalls.push('back'); for (const fn of listeners.get('popstate') || []) fn(); },
        },
        matchMedia: () => ({ matches: false }),
      };
      const views = [];
      win.addEventListener('aac-dash', (e) => views.push(e.detail));
      const ctl = installFrameDashboard(win);
      try {
        await ctl.start({ check: (await server('GET', '/api/auth/check')).payload });
        await flush(16);
        const pushes = () => historyCalls.filter((c) => c === 'push').length;
        const backs = () => historyCalls.filter((c) => c === 'back').length;
        const last = (key) => views.findLast((v) => v.key === key)?.json;
        ctl.act('details', 'codex:other'); ctl.act('details', 'codex:one'); ctl.act('details', 'codex:other');
        await flush(16);
        assert.equal(pushes(), width < 1280 ? 1 : 0, 'retargeting Details does not stack entries');
        ctl.act('activate', 'other');
        await flush(16);
        assert.equal(last('details'), '', 'the approval replaces Details');
        assert.ok(last('activation'));
        assert.equal(pushes(), width < 1280 ? 1 : 0, 'the approval inherits the Details entry');
        if (end === 'back' && width < 1280) {
          // a Back press: the browser pops the entry, then popstate
          state = null;
          for (const fn of listeners.get('popstate') || []) fn();
          await flush(16);
          assert.equal(last('activation'), '', 'Back cancels the approval');
          assert.equal(backs(), 0, 'Back already took the entry');
        } else if (end === 'success') {
          ctl.act('activation-confirm', '');
          await flush(24);
          assert.equal(s.attempts, 2);
          assert.equal(backs(), width < 1280 ? 1 : 0);
        } else {
          ctl.act('activation-cancel', '');
          await flush(16);
          assert.equal(backs(), width < 1280 ? 1 : 0);
        }
        assert.equal(last('details'), '', 'Details stays closed after the approval ends');
        ctl.act('details', 'codex:other');
        await flush(16);
        assert.equal(pushes(), width < 1280 ? 2 : 0, 'opening Details again gets a new entry');
        // a Back press over Details closes it
        if (width < 1280) {
          state = null;
          for (const fn of listeners.get('popstate') || []) fn();
          await flush(16);
          assert.equal(last('details'), '');
        }
      } finally { ctl.stop(); }
    }
  }
});

test('closing Details after a tab change never takes the tab\'s own history entry', async () => {
  const listeners = new Map();
  const historyCalls = [];
  let state = null;
  const { server } = fixtureServer();
  const win = {
    location: { protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN, search: '', href: '' },
    navigator: { userAgent: 'Mac fixture' }, innerWidth: 900,
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
    fetch: async (path, options = {}) => respond(await server(options.method || 'GET', path, options.body ? JSON.parse(options.body) : null)),
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init.detail; } },
    dispatchEvent: () => {},
    addEventListener: (name, fn) => listeners.set(name, [...(listeners.get(name) || []), fn]),
    removeEventListener: () => {},
    history: { get state() { return state; }, pushState: (s2) => { state = s2; historyCalls.push('push'); }, back: () => { historyCalls.push('back'); } },
    matchMedia: () => ({ matches: false }),
  };
  const ctl = installFrameDashboard(win);
  try {
    await ctl.start({ check: (await server('GET', '/api/auth/check')).payload });
    await flush(16);
    ctl.act('details', 'codex:other');
    await flush();
    // the frame pushed a tab's entry over it (frame.rs push_history, state null)
    state = null;
    ctl.act('details-closed', '');
    await flush();
    assert.deepEqual(historyCalls, ['push'], 'no Back that would undo the tab change');
  } finally { ctl.stop(); }
});

test('stop: nothing more is read or sent', async () => {
  const { server } = fixtureServer();
  const ctl = await loadController(server);
  const [calls, out] = [ctl.calls.length, ctl.emitted.length];
  ctl.c.stop();
  assert.equal(ctl.c.act('details', 'codex:one'), false);
  assert.equal(ctl.c.command('refresh'), false);
  for (const t of ctl.intervals) t.fn();
  await flush(16);
  assert.equal(ctl.calls.length, calls);
  assert.equal(ctl.emitted.length, out);
});

test('the frame loads a page module once, not early while offline, and asks again under a new address after a failure', async () => {
  const html = await readFile(new URL('../leptos/index.html', import.meta.url), 'utf8');
  const source = html.match(/const loader = \(path, early\) => \{[\s\S]*?\n {10}\};/)?.[0];
  assert.ok(source, 'index.html defines the loader');
  const make = (online, answers) => {
    const asked = [];
    // eslint-disable-next-line no-new-func
    const loader = new Function('__import', 'navigator', `${source.replaceAll('import(', '__import(')}; return loader;`)(
      (url) => { asked.push(url); return answers.shift()(url); },
      { onLine: online }
    );
    return { loader, asked };
  };
  const ok = (url) => Promise.resolve({ url });
  const fail = () => Promise.reject(new Error('offline'));
  // online and on the page's own address: asked for at once, then reused
  const a = make(true, [ok]);
  const loadA = a.loader('/frame-dashboard.mjs', true);
  assert.deepEqual(a.asked, ['/frame-dashboard.mjs']);
  assert.equal((await loadA()).url, '/frame-dashboard.mjs');
  await loadA();
  assert.equal(a.asked.length, 1, 'one load');
  // offline at start: nothing is asked for early; a failed load is asked again under a new address
  const b = make(false, [fail, ok]);
  const loadB = b.loader('/frame-dashboard.mjs', true);
  assert.deepEqual(b.asked, []);
  await assert.rejects(loadB());
  await flush();
  assert.equal((await loadB()).url, '/frame-dashboard.mjs?retry=2');
  assert.deepEqual(b.asked, ['/frame-dashboard.mjs', '/frame-dashboard.mjs?retry=2']);
  // another page's address: not early, loaded on first use
  const c = make(true, [ok]);
  c.loader('/frame-dashboard.mjs', false);
  assert.deepEqual(c.asked, []);
});

test('each install numbers its controller and stops the one before, so a page drives only its own', async () => {
  const listeners = new Map();
  const win = {
    location: { protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN, search: '', href: '' },
    navigator: { userAgent: 'Mac fixture' }, innerWidth: 1440,
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
    fetch: async () => respond({ status: 404, payload: {} }),
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init.detail; } },
    dispatchEvent: () => {},
    addEventListener: (name, fn) => listeners.set(name, [...(listeners.get(name) || []), fn]),
    removeEventListener: (name, fn) => listeners.set(name, (listeners.get(name) || []).filter((f) => f !== fn)),
    history: { state: null, pushState: () => {}, back: () => {} },
    matchMedia: () => ({ matches: false }),
  };
  const first = installFrameDashboard(win);
  const second = installFrameDashboard(win);
  assert.ok(Number.isInteger(first.id) && first.id >= 1);
  assert.equal(second.id, first.id + 1);
  assert.equal(win.aacFrameDashboard, second);
  assert.equal(first.state().stopped, true, 'the earlier controller was stopped');
  assert.equal(first.act('details', 'codex:one'), false);
  assert.equal((listeners.get('popstate') || []).length, 1, 'only the current one listens');
  second.stop();
  assert.equal((listeners.get('popstate') || []).length, 0);
});

// ---------------------------------------------------------------- post-landing fix (Astra max M1)
// A stopped controller (signed out, its page gone) stays in the top window, unlike the old iframe, which went with its
// work. Nothing it queued, awaited or scheduled may reach the network after stop() (the browser would send it with the
// next session's cookie), nor touch the next session's views or the shared state.

test('M1: a queued write is never sent after stop, even when the reply it waited for lands in the next session', async () => {
  let release;
  let session = 1;
  const patches = [];
  const { server: base } = fixtureServer();
  const server = async (method, path, body) => {
    if (method === 'PATCH' && path === '/api/accounts/visibility') {
      patches.push({ session, body });
      if (patches.length === 1) await new Promise((resolve) => { release = resolve; });
      return { payload: { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: ['claude'] } };
    }
    return base(method, path, body);
  };
  const ctl = await loadController(server, { page: 'accounts' });
  // Show in tray off for Claude (its reply is held), then Show on dashboard off for Codex: queued behind it
  await ctl.act('accounts-tray', 'claude:hide');
  await ctl.act('accounts-show', 'codex:hide');
  assert.equal(patches.length, 1, 'the second save waits for the first');
  assert.deepEqual(await ctl.c.signOut(), { ok: true });
  ctl.c.stop();
  // a new session starts; then the old reply arrives
  session = 2;
  const out = ctl.emitted.length;
  release();
  await flush(24);
  assert.deepEqual(patches.map((p) => p.session), [1], 'the queued Codex save is dropped, never sent');
  assert.equal(ctl.emitted.length, out, 'and the old controller draws and says nothing');
});

test('M1: a reply that lands after stop starts nothing: no sign-in polling, no request, no timer', async () => {
  let hold = false;
  let release;
  const calls = [];
  const timers = [];
  const { server: base } = fixtureServer();
  const job = { id: 'job_aaaaaaaaaaaaaaaa', provider: 'codex', kind: 'device-code', mode: 'add', state: 'waiting', profileName: 'pending', verification: { userCode: 'ABCD-1234', url: 'https://example.test' } };
  const c = createFrameDashboard({
    fetch: async (path, options = {}) => {
      calls.push(path);
      if (hold && path === '/api/accounts/registry') {
        hold = false;
        await new Promise((resolve) => { release = resolve; });
        return respond({ payload: { providers: [], accounts: [], trash: [], jobs: [job] } });
      }
      if (path.includes('/api/accounts/signin-jobs/')) return respond({ payload: job });
      return respond(await base(options.method || 'GET', path, options.body ? JSON.parse(options.body) : null));
    },
    setTimeout: (fn, ms) => { timers.push({ fn, ms }); return timers.length; },
    clearTimeout: () => {},
    setInterval: () => 1,
    clearInterval: () => {},
  });
  await c.start({ check: (await base('GET', '/api/auth/check')).payload });
  await flush();
  hold = true;
  c.page('accounts');
  await flush();
  c.stop();
  const before = calls.length;
  const scheduled = timers.length;
  release();
  await flush(16);
  for (const timer of timers.splice(0)) timer.fn();
  await flush(16);
  assert.equal(calls.length, before, 'no request after stop');
  assert.equal(timers.length, 0, `no timer scheduled after stop (had ${scheduled} before)`);
});

test('M1: a switch request queued behind a session rotation is never sent once the controller stops', async () => {
  let finishRevoke;
  const { s, server: base } = fixtureServer();
  const server = async (method, path, body) => {
    if (method === 'POST' && path === '/api/auth/sessions/revoke-others') {
      await new Promise((resolve) => { finishRevoke = resolve; });
      return { payload: { signedOutBrowsers: 1 } };
    }
    return base(method, path, body);
  };
  const ctl = await loadController(server, { page: 'accounts' });
  // "Sign out other browsers" rotates the session: requests made meanwhile wait for the new cookie (bridge.js send).
  // While a switch approval waits, the rotation itself is held, so the switch's first ask is what can queue behind it.
  assert.equal(ctl.c.act('others-out', ''), true);
  await flush();
  assert.ok(finishRevoke, 'the rotation is in flight');
  assert.equal(ctl.c.act('activate', 'other'), true);
  await flush();
  assert.equal(s.attempts, 0, 'the switch request waits behind the rotation');
  // the session ends (here: signed out elsewhere) and the frame stops the controller
  ctl.c.stop();
  const [calls, out] = [ctl.calls.length, ctl.emitted.length];
  finishRevoke();
  await flush(24);
  assert.equal(s.attempts, 0, 'the switch request is never sent with the next session');
  assert.equal(ctl.calls.length, calls, 'nothing else is sent either');
  assert.equal(ctl.emitted.length, out, 'and nothing is drawn');
});

test('M1: a switch confirmation in flight at stop is aborted and its answer acts on nothing', async () => {
  let answer;
  const signals = [];
  const { s, server: base } = fixtureServer();
  const server = async (method, path, body) => {
    if (method === 'POST' && path === '/api/codex/profiles/other/activate' && body?.confirmationToken) {
      s.attempts++;
      await new Promise((resolve) => { answer = resolve; });
      return { payload: { name: 'other', email: 'other@example.test' } };
    }
    return base(method, path, body);
  };
  const calls = [];
  const emitted = [];
  const c = createFrameDashboard({
    fetch: async (path, options = {}) => {
      const method = options.method || 'GET';
      const body = options.body ? JSON.parse(options.body) : null;
      calls.push({ method, path, body });
      signals.push(options.signal);
      return respond(await server(method, path, body));
    },
    setInterval: () => 1,
    clearInterval: () => {},
    emit: (key, json) => emitted.push({ key, json }),
  });
  await c.start({ check: (await base('GET', '/api/auth/check')).payload });
  await flush(16);
  assert.equal(c.act('activate', 'other'), true);
  await flush(16);
  assert.equal(c.act('activation-confirm', ''), true);
  await flush(16);
  assert.equal(s.attempts, 2, 'the confirmation is on its way');
  const confirmSignal = signals.at(-1);
  assert.ok(confirmSignal && confirmSignal.aborted === false, 'every request carries the controller\'s abort signal');
  c.stop();
  assert.equal(confirmSignal.aborted, true, 'stop aborts what is in flight');
  const [n, out] = [calls.length, emitted.length];
  // the server's answer still arrives (a fetch that ignores the abort): the stopped controller acts on none of it
  answer();
  await flush(24);
  assert.equal(calls.length, n, 'no refresh or other read follows with the next session');
  assert.equal(emitted.length, out, 'no "Codex account switched" toast, no view');
});

test('M1 (Sol): a logout queued behind a session rotation never signs out the next session', async () => {
  // Sol max M1's sequence: a held dashboard read; Sign out other browsers waits for it (it rotates the session); Sign
  // out waits behind that rotation; the session ends elsewhere and the frame stops the controller; a new session signs
  // in; then the old read is answered.
  let holdRead = false;
  let releaseRead;
  let session = 1;
  const sent = [];
  const { s, server: base } = fixtureServer();
  const server = async (method, path, body) => {
    if (method !== 'GET') sent.push({ session, method, path });
    if (method === 'POST' && path === '/api/auth/logout') { s.signedIn = false; return { payload: { success: true } }; }
    if (method === 'POST' && path === '/api/auth/sessions/revoke-others') return { payload: { signedOutBrowsers: 1 } };
    if (holdRead && method === 'GET' && path.startsWith('/api/accounts/dashboard?')) {
      holdRead = false;
      await new Promise((resolve) => { releaseRead = resolve; });
    }
    return base(method, path, body);
  };
  const ctl = await loadController(server, { page: 'accounts' });
  holdRead = true;
  assert.equal(ctl.c.command('refresh'), true);
  await flush();
  assert.ok(releaseRead, 'a dashboard read is held');
  assert.equal(ctl.c.act('others-out', ''), true);
  await flush();
  const signingOut = ctl.c.signOut();
  await flush();
  assert.deepEqual(sent, [], 'the rotation waits for the read, and the logout for the rotation');
  // the session ends elsewhere: the frame stops the controller; then a new session signs in
  ctl.c.stop();
  session = 2;
  s.signedIn = true;
  releaseRead();
  await flush(32);
  assert.deepEqual(sent, [], 'neither the rotation nor the logout is ever sent with the new session');
  assert.equal(s.signedIn, true, 'the new session stays signed in');
  assert.deepEqual(await signingOut, { ok: false }, 'the old Sign out ends without effect');
});

test('M1 (Sol): a Claude Add that already started sends no follow-up Open after stop', async () => {
  let releaseAdd;
  const sent = [];
  const { server: base } = fixtureServer();
  const server = async (method, path, body) => {
    if (method !== 'GET') sent.push(`${method} ${path}`);
    if (method === 'POST' && path === '/api/accounts/add') {
      await new Promise((resolve) => { releaseAdd = resolve; });
      return { payload: { success: true } };
    }
    if (method === 'POST' && /\/api\/claude\/desktop-profiles\/[^/]+\/open$/.test(path)) return { status: 202, payload: { operation: { state: 'opening' } } };
    return base(method, path, body);
  };
  const ctl = await loadController(server, { page: 'accounts' });
  await ctl.act('add', 'claude');
  await ctl.act('flow-submit', 'claude\nfixture-two');
  await ctl.act('flow-submit', 'claude\nfixture.two@example.test');
  await ctl.act('claude-host', 'claude:mac');
  assert.deepEqual(sent, ['POST /api/accounts/add'], 'the profile is being created');
  ctl.c.stop();
  const out = ctl.emitted.length;
  releaseAdd();
  await flush(32);
  assert.deepEqual(sent, ['POST /api/accounts/add'], 'its Open never follows once the page has gone');
  assert.equal(ctl.emitted.length, out, 'and nothing is drawn or said');
});

test('Settings "Install app": the page-captured Chromium prompt shows the row, runs once, and the row follows (bridge.js promptInstall)', async () => {
  const listeners = new Map();
  const { server } = fixtureServer();
  let prompted = 0;
  const win = {
    location: { protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN, search: '?e2e', href: '' },
    navigator: { userAgent: 'Linux Chromium fixture', maxTouchPoints: 0 },
    innerWidth: 1440,
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
    fetch: async (path, options = {}) => respond(await server(options.method || 'GET', path, options.body ? JSON.parse(options.body) : null)),
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init.detail; } },
    dispatchEvent: (event) => { for (const fn of listeners.get(event.type) || []) fn(event); },
    addEventListener: (name, fn) => listeners.set(name, [...(listeners.get(name) || []), fn]),
    removeEventListener: (name, fn) => listeners.set(name, (listeners.get(name) || []).filter((f) => f !== fn)),
    history: { state: null, pushState() {}, back() {} },
    matchMedia: () => ({ matches: false }),
  };
  const ctl = installFrameDashboard(win);
  try {
    await ctl.start({ check: (await server('GET', '/api/auth/check')).payload });
    ctl.page('accounts');
    await flush(24);
    assert.equal(win.__aacLastAccounts?.install, 'hidden', 'no prompt captured yet: no install row');
    // leptos/index.html keeps the prompt and says so
    win.__aacInstallPrompt = { prompt: async () => { prompted++; } };
    win.dispatchEvent({ type: 'aac-install' });
    await flush(8);
    assert.equal(win.__aacLastAccounts?.install, 'chromium', 'the captured prompt offers Install app');
    ctl.act('install-app', '');
    await flush(8);
    assert.equal(prompted, 1, 'the prompt is shown');
    assert.equal(win.__aacInstallPrompt, null, 'and used up');
    assert.equal(win.__aacLastAccounts?.install, 'hidden', 'the row follows');
    ctl.act('install-app', '');
    await flush(8);
    assert.equal(prompted, 1, 'a second press shows nothing');
  } finally { ctl.stop(); }
  assert.equal((listeners.get('aac-install') || []).length, 0, 'stop removes the listener');
});
