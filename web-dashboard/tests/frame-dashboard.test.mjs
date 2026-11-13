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

const ORIGIN = 'http://192.0.2.10:3000';
const TOKEN = 'A'.repeat(43);
// one clock for both servers of a differential test: their answers must match to the second
const EXPIRES = new Date(Date.now() + 300_000).toISOString();
const flush = async (rounds = 8) => { for (let i = 0; i < rounds; i++) await new Promise((resolve) => setImmediate(resolve)); };
let loads = 0;

/** A dashboard server with a session and the routes Home and Accounts read and change. Fresh state per call. */
function fixtureServer(over = {}) {
  const s = { signedIn: true, revoked: false, attempts: 0, auto: { enabled: false, thresholdPercent: 5 }, interval: 60, ...over };
  const updated = new Date(Date.now() - 30_000).toISOString();
  const accounts = [
    { id: 'codex:one', provider: 'codex', email: 'one@example.test', isActive: true, status: 'ok', windows: [], capabilities: { codexProfile: 'one' } },
    { id: 'codex:other', provider: 'codex', email: 'other@example.test', isActive: false, status: 'ok', windows: [], capabilities: { codexProfile: 'other' } },
  ];
  const job = { id: '11111111-1111-4111-8111-111111111111', state: 'running', startedAt: updated, results: [] };
  if (over.accounts) accounts.push(...over.accounts(updated));
  const routes = {
    'GET /api/auth/setup': () => ({ payload: { sessionTimeoutHours: 24, configured: true } }),
    'GET /api/auth/check': () => ({ payload: { authenticated: s.signedIn, authRequired: true, accessMode: 'login', username: s.signedIn ? 'owner' : null, signedOutReason: null, secureTransport: true, trustedLocalNetwork: true, connection: { peer: '192.168.10.20', trusted: true } } }),
    'GET /api/accounts/settings': () => ({ payload: { refreshIntervalSeconds: s.interval } }),
    'PUT /api/accounts/settings': (body) => { s.interval = body.refreshIntervalSeconds; return { payload: { refreshIntervalSeconds: s.interval } }; },
    'GET /api/accounts/preferences': () => ({ payload: { timeZone: 'UTC' } }),
    'GET /api/claude/desktop-profiles': () => ({ payload: { profiles: [] } }),
    'GET /api/antigravity/profiles': () => ({ status: 404, payload: { code: 'not_found' } }),
    'GET /api/antigravity/auto-switch': () => ({ status: 404, payload: { code: 'not_found' } }),
    'GET /api/app-updates/status': () => ({ payload: { job: null } }),
    'POST /api/app-updates/start': () => ({ status: 409, payload: { error: 'Already running', job } }),
    'GET /api/accounts/registry': () => ({ payload: { providers: [], accounts: [], jobs: [], trash: [] } }),
    'GET /api/auth/session': () => ({ payload: { username: 'owner', otherBrowsers: 0, secureTransport: true, managedBy: 'config' } }),
    'GET /api/auth/devices': () => ({ payload: { devices: [] } }),
    'GET /api/auth/network': () => ({ payload: { trustLocalNetwork: true, connection: { peer: '192.168.10.20', trusted: true }, canTurnOn: false } }),
    'POST /api/auth/logout': () => { s.signedIn = false; return { payload: { success: true } }; },
    'PUT /api/codex/profiles/auto-switch': (body) => { s.auto = { ...s.auto, ...body }; return { payload: s.auto }; },
    'POST /api/codex/profiles/other/activate': (body) => {
      s.attempts++;
      if (body?.confirmationToken) return { payload: { name: 'other', email: 'other@example.test' } };
      return { status: 409, payload: { code: 'confirmation_required', error: 'Confirm the switch', confirmation: { token: TOKEN, targetProfile: 'other', expiresAt: EXPIRES, processes: [{ label: 'codex', pid: 42, role: 'cli' }], warning: 'Active Codex work will stop.' } } };
    },
    'POST /api/accounts/codex%3Aother/remove': () => ({ payload: { confirmation: { token: 't'.repeat(43), effects: ['Its saved login is deleted from Ubuntu.'] } } }),
    ...over.routes,
  };
  const server = async (method, path, body) => {
    if (method === 'GET' && path.startsWith('/api/accounts/dashboard?')) {
      if (s.revoked) return { status: 401, payload: { error: 'Signed out', code: 'session_revoked' } };
      if (!s.signedIn) return { status: 401, payload: { error: 'Authentication required', code: 'auth_required' } };
      return { payload: { schemaVersion: 1, updatedAt: updated, accounts: structuredClone(accounts), providers: [], settings: { refreshIntervalSeconds: s.interval, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: s.auto } };
    }
    const route = routes[`${method} ${path}`];
    return route ? route(body) : { status: 404, payload: { code: 'not_found' } };
  };
  return { s, server };
}

function respond(answer) {
  const status = answer.status ?? 200;
  return { ok: status >= 200 && status < 300, status, json: async () => answer.payload, headers: { get: () => null } };
}

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

/** The controller, started as the frame starts it, with the same server. */
async function loadController(server, { width = 1440, page = 'home', check = undefined } = {}) {
  const calls = [], emitted = [], intervals = [], historyCalls = [];
  const stored = new Map();
  let depth = 0;
  const c = createFrameDashboard({
    fetch: async (path, options = {}) => {
      const method = options.method || 'GET';
      const body = options.body ? JSON.parse(options.body) : null;
      calls.push({ method, path, body });
      return respond(await server(method, path, body));
    },
    storage: { getItem: (k) => (stored.has(k) ? stored.get(k) : null), setItem: (k, v) => stored.set(k, String(v)), removeItem: (k) => stored.delete(k) },
    location: { protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN },
    setInterval: (fn, ms) => { intervals.push({ fn, ms }); return intervals.length; },
    clearInterval: () => {},
    emit: (key, json) => emitted.push({ key, json }),
    overlay: {
      depth: () => depth,
      push() { if (width < 1280) { depth++; historyCalls.push(['push']); } },
      consume() { if (width < 1280 && depth > 0) { depth--; historyCalls.push(['back']); } },
    },
  });
  c.page(page);
  const sessionCheck = check === undefined ? (await server('GET', '/api/auth/check')).payload : check;
  await c.start({ check: sessionCheck, username: 'owner' });
  await flush(16);
  return {
    c, calls, emitted, intervals, historyCalls, stored,
    act: async (kind, value = '') => { assert.equal(c.act(kind, value), true, `${kind} is a page action`); await flush(16); },
    command: async (action) => { assert.equal(c.command(action), true); await flush(16); },
    toasts: () => emitted.filter((e) => e.key === 'toast').map((e) => JSON.parse(e.json)),
    last: (key) => emitted.findLast((e) => e.key === key)?.json,
    out: () => emitted.filter((e) => e.key !== 'signed-out').map((e) => {
      if (e.key === 'toast') { const t = JSON.parse(e.json); return ['toast', t.kind, t.title, t.body]; }
      if (e.key === 'chrome') { const t = JSON.parse(e.json); return ['chrome', t.statusLead, t.statusStrong, t.statusMore]; }
      return ['view', e.key, e.json];
    }),
  };
}

/** The paths where two JSON texts differ (a readable failure for a large view). */
function jsonDiff(a, b, path = '', out = []) {
  if (JSON.stringify(a) === JSON.stringify(b)) return out;
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object') { out.push(`${path || '.'}: ${JSON.stringify(a)?.slice(0, 160)} != ${JSON.stringify(b)?.slice(0, 160)}`); return out; }
  for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) jsonDiff(a[k], b[k], `${path}.${k}`, out);
  return out;
}
function sameView(actual, expected, message) {
  if (actual === expected) return;
  let parsed = null;
  try { parsed = [JSON.parse(actual), JSON.parse(expected)]; } catch {}
  assert.fail(`${message}: ${parsed ? jsonDiff(...parsed).slice(0, 12).join('; ') : `${actual} != ${expected}`}`);
}
/** Two lists of [kind, key, json] (views, toasts, header lines), compared entry by entry. */
function sameOut(actual, expected, message) {
  assert.deepEqual(actual.map((x) => x.slice(0, 2)), expected.map((x) => x.slice(0, 2)), `${message}: the same kinds, in order`);
  actual.forEach((x, i) => (x[0] === 'view' ? sameView(x[2], expected[i][2], `${message} (${x[1]})`) : assert.deepEqual(x, expected[i], message)));
}
const writes = (calls) => calls.filter((c) => c.method !== 'GET');
const reads = (calls) => calls.map((c) => `${c.method} ${c.path}`);
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

/** Two verified Antigravity logins on Ubuntu with a reported quota pool (antigravity-bridge.test.mjs's fixture). */
function antigravityServer() {
  const now = Date.now();
  const token = 'FixtureTokenForAntigravity12345';
  const profiles = ['gmail', 'party'].map((id, index) => ({ id, email: `${id}@example.com`, plan: 'Pro', hostId: 'ubuntu', available: true, selected: index === 0, runtimeVerified: index === 0, verifiedAt: new Date(now).toISOString() }));
  let auto = { enabled: false, thresholdUsedPercent: 95, pollIntervalSeconds: 60, maxQuotaAgeSeconds: 300, cooldownSeconds: 300, selectedHostIds: ['ubuntu'], requestedPoolId: null, outcome: 'setup_required', message: 'Fixture setup', activationInProgress: false };
  const confirms = [];
  const built = fixtureServer({
    accounts: () => profiles.map((p) => ({
      id: `antigravity:profile:${p.id}`, provider: 'antigravity', providerLabel: 'Antigravity', email: p.email, platform: 'ubuntu', status: 'ok', sampledAt: new Date(now).toISOString(), source: 'Fixture native consumer', isActive: p.selected,
      capabilities: { antigravityProfileId: p.id, antigravityHostIds: ['ubuntu'], antigravityCanActivate: true },
      windows: [300, 10080].map((minutes) => ({ key: `gemini-${minutes}`, label: `Gemini ${minutes}`, kind: 'rate_limit', windowMinutes: minutes, usedPercent: 0, resetAt: new Date(now + minutes * 60000).toISOString(), poolId: 'reported@gemini', poolIdSource: 'provider-bucket-membership', poolLabel: 'Gemini models' })),
    })),
    routes: {
      'GET /api/antigravity/profiles': () => ({ payload: { schemaVersion: 1, hostId: 'ubuntu', activationSupported: true, profiles: structuredClone(profiles) } }),
      'GET /api/antigravity/auto-switch': () => ({ payload: auto }),
      'PUT /api/antigravity/auto-switch': (body) => { auto = { ...auto, ...body }; return { payload: auto }; },
      'POST /api/antigravity/profiles/party/activate': () => ({ status: 409, payload: { status: 'confirmation-required', profileId: 'party', hostId: 'ubuntu', email: 'party@example.com', confirmation: { token, profileId: 'party', hostId: 'ubuntu', email: 'party@example.com', expiresAt: EXPIRES, processes: [{ pid: 456, role: 'cli', label: 'Antigravity CLI' }] } } }),
      'POST /api/antigravity/profiles/party/confirm': (body) => {
        confirms.push(body);
        profiles[0].selected = false; profiles[0].runtimeVerified = false; profiles[1].selected = true; profiles[1].runtimeVerified = true;
        return { payload: { status: 'active', profileId: 'party', hostId: 'ubuntu', email: 'party@example.com' } };
      },
    },
  });
  return { ...built, token, confirms };
}

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
    assert.equal(ctl.stored.get('aac-last-page'), 'accounts');
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
