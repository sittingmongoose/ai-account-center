// The app frame's dashboard controller (public/frame-dashboard.mjs): the guards bridge.js kept, ported with it (its
// bridge-session.test.mjs): switch approvals, the held changes, sign-out, the session rotation, an ended session, open
// access, Update apps, the timers and the Back entries. What bridge.js itself did on the same scripts is replayed in
// frame-dashboard-golden.test.mjs. Synthetic data only; controller and data proof, not visual QA.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createFrameDashboard, installFrameDashboard, DASHBOARD_ACTIONS, FRAME_COMMANDS } from '../public/frame-dashboard.mjs';
import { PAGE_ACTIONS } from '../public/frame-seam.mjs';
import { ORIGIN, flush, fixtureServer, respond, loadController } from './fixtures/frame-dashboard-fixture.mjs';

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

test('the start draws nothing before the dashboard answers: only the page\'s loading state, no numbers', async () => {
  const { server } = fixtureServer();
  const holdDashboard = { release: null };
  const gated = async (method, path, body) => {
    if (path.startsWith('/api/accounts/dashboard?') && !holdDashboard.release) await new Promise((resolve) => { holdDashboard.release = resolve; });
    return server(method, path, body);
  };
  const starting = loadController(gated);
  await flush(16);
  assert.ok(holdDashboard.release, 'the dashboard read is in flight');
  assert.equal(await Promise.race([starting, Promise.resolve(null)]), null, 'the start waits for it');
  holdDashboard.release();
  const ctl = await starting;
  try {
    assert.ok(ctl.last('dashboard'), 'Home is drawn once it answers');
  } finally { ctl.c.stop(); }
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

test('the frame asks for the module of the tab its address opens at once, classic addresses included', async () => {
  const html = await readFile(new URL('../leptos/index.html', import.meta.url), 'utf8');
  const source = html.match(/const startPage = \(\(path, search\) => \{[\s\S]*?\n {10}\}\)/)?.[0];
  assert.ok(source, 'index.html names the start tab');
  // eslint-disable-next-line no-new-func
  const startPage = new Function(`${source.replace('const startPage = ', 'return ')};`)();
  // lib.rs the_classic_addresses_open_the_same_tabs, case for case
  for (const [path, search, tab] of [
    ['/', '', 'home'],
    ['/login', '', 'home'],
    ['/analytics', '', 'analytics'],
    ['/analytics/', '', 'analytics'],
    ['/accounts', '', 'accounts'],
    ['/accounts/claude', '', 'accounts'],
    ['/accounts/opencode-go', '', 'accounts'],
    ['/accounts/Claude', '', 'home'],
    ['/accounts/-x', '', 'home'],
    ['/status', '', 'status'],
    ['/scoreboard', '', 'scoreboard'],
    ['/nowhere', '', 'home'],
    ['/', '?view=analytics', 'analytics'],
    ['/', '?e2e&view=accounts', 'accounts'],
    ['/analytics', '?view=status', 'status'],
    ['/', '?view=bogus', 'home'],
    ['/', '?classic=1', 'home'],
    ['/', '?content=1', 'home'],
    ['/app/scoreboard', '?view=home', 'scoreboard'],
    ['/app/home', '', 'home'],
    ['/app/', '', 'status'],
  ])
    assert.equal(startPage(path, search), tab, `${path}${search}`);
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

// Ported from bridge-session.test.mjs when bridge.js was retired (Leptos finish, step 9).
test('the Accounts About row receives the current server version from the dashboard DTO', async () => {
  const { server } = fixtureServer();
  const ctl = await loadController(async (method, path, body) => {
    const answer = await server(method, path, body);
    if (path.startsWith('/api/accounts/dashboard?')) answer.payload.server = { version: '8.10.test' };
    return answer;
  }, { page: 'accounts' });
  try { assert.equal(JSON.parse(ctl.last('accounts')).about.version, 'Version 8.10.test'); }
  finally { ctl.c.stop(); }
});

test('an older status read cannot erase a job adopted from a 409', async () => {
  const { server } = fixtureServer();
  const id = '44444444-4444-4444-8444-444444444444';
  let hold = false;
  let release = null;
  let job = null;
  const ctl = await loadController(async (method, path, body) => {
    if (path === '/api/app-updates/start') return { status: 409, payload: { job: { id, state: 'running', startedAt: new Date().toISOString(), results: [] } } };
    if (path === '/api/app-updates/status') return hold ? new Promise((resolve) => { release = resolve; }) : { payload: { job } };
    return server(method, path, body);
  });
  try {
    hold = true;
    ctl.c.focused();
    await flush(16);
    await ctl.command('update-apps');
    hold = false;
    release({ payload: { job: null } });
    await flush(16);
    job = { id, state: 'completed', results: [{ status: 'current' }] };
    ctl.intervals.find((t) => t.ms === 3_000).fn();
    await flush(16);
    assert.equal(ctl.toasts().filter((t) => t.title === 'App checks finished').length, 1);
  } finally { ctl.c.stop(); }
});
