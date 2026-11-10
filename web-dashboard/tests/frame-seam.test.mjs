// The seam between the dashboard (bridge.js in the app frame's hidden iframe) and the frame's native pages
// (public/frame-seam.mjs, leptos/src/dash.rs): which messages are accepted, which action kinds may run, which views
// are forwarded and when, and that the classic app (no frame) is untouched.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { PAGE_ACTIONS, VIEW_KEYS, FRAME_OVERLAYS, FRAME_PAGES, createFrameViews, pageAction, viewsSubscription } from '../public/frame-seam.mjs';
import { MUTATING_ACTIONS } from '../public/accounts-controller.mjs';

const ORIGIN = 'http://192.0.2.10:3000';
const parent = { name: 'frame' };
const where = { origin: ORIGIN, parent };
const event = (data, over = {}) => ({ origin: ORIGIN, source: parent, data, ...over });

test('page actions: only from the parent window and this origin, only allowed kinds, value always a string', () => {
  assert.deepEqual(pageAction(event({ type: 'aac-page', kind: 'remove', value: 'claude:a' }), where), { kind: 'remove', value: 'claude:a' });
  assert.deepEqual(pageAction(event({ type: 'aac-page', kind: 'flow-cancel' }), where), { kind: 'flow-cancel', value: '' });
  assert.deepEqual(pageAction(event({ type: 'aac-page', kind: 'line-cancel', value: 7 }), where), { kind: 'line-cancel', value: '' });
  // another window of the same origin, another origin, no window at all
  assert.equal(pageAction(event({ type: 'aac-page', kind: 'remove', value: 'x' }, { source: {} }), where), null);
  assert.equal(pageAction(event({ type: 'aac-page', kind: 'remove', value: 'x' }, { origin: 'http://evil.test' }), where), null);
  assert.equal(pageAction(event({ type: 'aac-page', kind: 'remove', value: 'x' }), { origin: ORIGIN, parent: null }), null);
  // other message types, a missing kind
  assert.equal(pageAction(event({ type: 'aac-frame', action: 'remove' }), where), null);
  assert.equal(pageAction(event({ type: 'aac-page' }), where), null);
  assert.equal(pageAction(null, where), null);
});

test('the allowlist covers every Accounts & Settings change and never sign-in, the frame\'s commands or the canvas', () => {
  for (const kind of MUTATING_ACTIONS) assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  for (const kind of ['details', 'details-closed', 'launch', 'activate', 'antigravity-activate', 'activation-confirm', 'activation-cancel',
    'flow-submit', 'flow-copy', 'flow-open-url', 'refuse', 'line-cancel', 'pw-toggle', 'pw-typing', 'refresh-interval', 'update-cancel']) {
    assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  }
  for (const kind of ['login', 'setup', 'setup-typing', 'login-overlay', 'login-field-focus', 'login-limit-over', 'probe-rect',
    'content-rect', 'content-cover', 'overlay-open', 'overlay-close', 'navigate', 'navigate-dashboard', 'navigate-analytics',
    'theme', 'refresh', 'update-apps', 'logout', 'auth-recheck', 'analytics-refresh', 'analytics-activity-interval',
    'analytics-metric-key', 'analytics-account', 'analytics-account-id', 'analytics-metric']) {
    assert.equal(PAGE_ACTIONS.has(kind), false, `${kind} is not a page action`);
    assert.equal(pageAction(event({ type: 'aac-page', kind, value: '' }), where), null);
  }
});

test('the Analytics page controls are page actions; its refresh is the frame\'s own Refresh', () => {
  for (const kind of ['analytics-range', 'analytics-custom', 'analytics-provider', 'analytics-split', 'analytics-cache', 'analytics-donut',
    'analytics-cbm-sort', 'analytics-donut-open', 'analytics-heat', 'analytics-focus', 'analytics-compare', 'analytics-group', 'analytics-layout']) {
    assert.deepEqual(pageAction(event({ type: 'aac-page', kind, value: 'x' }), where), { kind, value: 'x' });
  }
  for (const key of ['analytics', 'analytics-motion', 'analytics-head', 'analytics-loading']) assert.ok(VIEW_KEYS.includes(key), key);
  assert.deepEqual([...FRAME_PAGES], ['analytics']);
});

test('the subscription names only known overlays and pages and comes only from the frame', () => {
  assert.deepEqual(viewsSubscription(event({ type: 'aac-views', overlays: ['details', 'bogus', 'activation'] }), where), ['details', 'activation']);
  assert.deepEqual(viewsSubscription(event({ type: 'aac-views', overlays: ['analytics', 'home'] }), where), ['analytics']);
  assert.deepEqual(viewsSubscription(event({ type: 'aac-views' }), where), []);
  assert.equal(viewsSubscription(event({ type: 'aac-views', overlays: ['details'] }, { source: {} }), where), null);
  assert.equal(viewsSubscription(event({ type: 'aac-page', kind: 'details' }), where), null);
  assert.deepEqual([...FRAME_OVERLAYS], ['details', 'activation']);
});

test('views wait for the subscription, are all replayed on it, and only owned overlays leave Slint', () => {
  const posted = [];
  const views = createFrameViews({ post: (kind, fields) => posted.push({ kind, ...fields }) });
  views.view('dashboard', '{"version":2}');
  views.view('busy', 'false');
  views.view('nonsense', 'x');
  assert.equal(posted.length, 0);
  assert.equal(views.owns('details'), false);
  views.subscribe(['details']);
  assert.deepEqual(posted, [{ kind: 'view', key: 'dashboard', json: '{"version":2}' }, { kind: 'view', key: 'busy', json: 'false' }]);
  assert.ok(views.owns('details'));
  assert.equal(views.owns('activation'), false);
  views.view('busy', 'true');
  assert.deepEqual(posted.at(-1), { kind: 'view', key: 'busy', json: 'true' });
  // a second subscription (the frame mounted a page again) replays the latest of each key
  posted.length = 0;
  views.subscribe([]);
  assert.deepEqual(posted.map(p => [p.key, p.json]), [['dashboard', '{"version":2}'], ['busy', 'true']]);
  assert.equal(views.owns('details'), false);
  assert.ok(VIEW_KEYS.includes('accounts') && VIEW_KEYS.includes('activation'));
});

// ---------------------------------------------------------------- bridge.js itself, in the frame and without it

const flush = async (rounds = 6) => { for (let i = 0; i < rounds; i++) await new Promise(resolve => setImmediate(resolve)); };
let loads = 0;

function fixtureServer() {
  const accounts = [
    { id: 'codex:one', provider: 'codex', email: 'one@example.test', isActive: true, status: 'ok', windows: [], capabilities: { codexProfile: 'one' } },
    { id: 'codex:other', provider: 'codex', email: 'other@example.test', isActive: false, status: 'ok', windows: [], capabilities: { codexProfile: 'other' } },
  ];
  const routes = {
    'GET /api/auth/setup': () => ({ payload: { sessionTimeoutHours: 24, configured: true } }),
    'GET /api/auth/check': () => ({ payload: { authenticated: true, authRequired: true, accessMode: 'login', username: 'owner', signedOutReason: null, secureTransport: true, trustedLocalNetwork: true, connection: { peer: '192.168.10.20', trusted: true } } }),
    'GET /api/accounts/settings': () => ({ payload: { refreshIntervalSeconds: 60 } }),
    'GET /api/accounts/preferences': () => ({ payload: { timeZone: 'UTC' } }),
    'GET /api/claude/desktop-profiles': () => ({ payload: { profiles: [] } }),
    'GET /api/antigravity/profiles': () => ({ status: 404, payload: { code: 'not_found' } }),
    'GET /api/antigravity/auto-switch': () => ({ status: 404, payload: { code: 'not_found' } }),
    'GET /api/app-updates/status': () => ({ payload: { job: null } }),
    'GET /api/accounts/registry': () => ({ payload: { providers: [], accounts: [], jobs: [], trash: [] } }),
    'GET /api/auth/session': () => ({ payload: { username: 'owner', otherBrowsers: 0, secureTransport: true, managedBy: 'config' } }),
    'GET /api/auth/devices': () => ({ payload: { devices: [] } }),
    'GET /api/auth/network': () => ({ payload: { trustLocalNetwork: true, connection: { peer: '192.168.10.20', trusted: true }, canTurnOn: false } }),
    'POST /api/auth/logout': () => ({ payload: { success: true } }),
  };
  return async (method, path) => {
    if (method === 'GET' && path.startsWith('/api/accounts/analytics?')) return { payload: analyticsPayload() };
    if (method === 'GET' && path.startsWith('/api/accounts/dashboard?')) {
      return { payload: { schemaVersion: 1, updatedAt: new Date().toISOString(), accounts, providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: { enabled: false, thresholdPercent: 5 } } };
    }
    const route = routes[`${method} ${path}`];
    return route ? route() : { status: 404, payload: { code: 'not_found' } };
  };
}

function analyticsPayload() {
  const now = Date.now(), H = 3600_000;
  const at = ms => new Date(Math.floor(ms / H) * H).toISOString();
  const row = (ms, provider, tokens) => ({ hour: at(ms), provider, inputTokens: tokens, outputTokens: tokens / 10, cacheCreationTokens: 0, cacheReadTokens: tokens * 4, estimatedCostUsd: tokens / 1e6 });
  const byHour = [row(now - 30 * H, 'codex', 2e6), row(now - 5 * H, 'codex', 1e6)];
  const totals = { inputTokens: 3e6, outputTokens: 3e5, cacheCreationTokens: 0, cacheReadTokens: 1.2e7, estimatedCostUsd: 3 };
  return {
    schemaVersion: 1, updatedAt: new Date(now).toISOString(),
    range: { preset: '7d', from: new Date(now - 7 * 24 * H).toISOString(), to: new Date(now).toISOString(), bucketMinutes: 60 },
    accounts: [],
    activity: { status: 'ok', scope: 'ubuntu-local-cli', fetchedAt: new Date(now).toISOString(), message: '', totals, byHour,
      providers: [{ provider: 'codex', label: 'Codex logs', totals, usageEvents: 4, sessionCount: 2, tools: ['codex'] }],
      models: [{ model: 'gpt-5', provider: 'codex', ...totals }] },
  };
}

/** Loads a fresh bridge.js; `framed` puts it in the app frame's iframe (`/?content=1` under a frame window). */
async function loadBridge({ framed }) {
  const base = new URL('../public/', import.meta.url);
  let source = await readFile(new URL('bridge.js', base), 'utf8');
  const first = source.split('\n')[0];
  const declarations = first.match(/\{ ([^}]+) \}/)[1];
  const key = `__frameSeamWasm${++loads}`;
  source = source.replace(first, `const init = async () => {}; const { ${declarations} } = globalThis.${key};`)
    .replace(/import \{ requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard \} from '[^']+';/, 'const requireWebGL = () => {}; const WEBGL_REQUIRED_MESSAGE = "fixture"; const startSlintDashboard = fn => fn();')
    .replace(/from '(\.\/[^']+)'/g, (_match, relative) => `from '${new URL(relative, base).href}'`)
    .concat(`\n// ${key}\n`);
  const slint = { details: [], closed: 0, accounts: 0, analytics: 0, analyticsLoading: 0, trend: 0 };
  const posted = [], calls = [], listeners = new Map(), historyCalls = [];
  const frameWindow = { __aacFrame: true, innerWidth: 1440, postMessage: (message, target) => posted.push({ message, target }) };
  const originals = new Map();
  const assign = (name, value) => {
    if (!originals.has(name)) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  };
  const restore = () => { for (const [name, descriptor] of originals) descriptor ? Object.defineProperty(globalThis, name, descriptor) : delete globalThis[name]; };
  assign(key, Object.fromEntries(declarations.split(', ').map(name => [name,
    name === 'show_details' ? json => slint.details.push(JSON.parse(json))
      : name === 'close_details' ? () => { slint.closed++; }
        : name === 'set_accounts' ? () => { slint.accounts++; }
          : name === 'set_analytics' ? () => { slint.analytics++; }
            : name === 'set_analytics_loading' ? () => { slint.analyticsLoading++; }
              : name === 'set_analytics_trend_paths' ? () => { slint.trend++; }
                : () => {}])));
  assign('window', {});
  assign('navigator', { userAgent: 'Mac fixture' });
  assign('location', { pathname: framed ? '/' : '/accounts', search: framed ? '?content=1' : '', protocol: 'http:', hostname: '192.0.2.10', host: '192.0.2.10:3000', origin: ORIGIN, href: '' });
  assign('document', { querySelector: () => ({ hidden: false, textContent: '' }), getElementById: () => null });
  assign('innerWidth', framed ? 0 : 1440); assign('innerHeight', 900); assign('devicePixelRatio', 1);
  if (framed) { assign('parent', frameWindow); assign('top', frameWindow); assign('self', globalThis); }
  assign('addEventListener', (name, fn) => listeners.set(name, [...(listeners.get(name) || []), fn])); assign('matchMedia', () => ({ matches: false }));
  assign('history', { pushState: (...args) => historyCalls.push(['push', ...args]), replaceState: () => {}, back: () => historyCalls.push(['back']) });
  const stored = new Map();
  assign('localStorage', { getItem: k => stored.has(k) ? stored.get(k) : null, setItem: (k, v) => stored.set(k, String(v)), removeItem: k => stored.delete(k) });
  assign('setInterval', () => 0); assign('clearInterval', () => {});
  assign('requestAnimationFrame', () => 0); assign('cancelAnimationFrame', () => {});
  const server = fixtureServer();
  assign('fetch', async (path, options = {}) => {
    const method = options.method || 'GET';
    calls.push({ method, path });
    const answer = await server(method, path);
    const status = answer.status ?? 200;
    return { ok: status >= 200 && status < 300, status, json: async () => answer.payload, headers: { get: () => null } };
  });
  try {
    await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
  } catch (error) { restore(); throw error; }
  await flush(12);
  const fromFrame = (data, over = {}) => { for (const fn of listeners.get('message') || []) fn({ origin: ORIGIN, source: frameWindow, data, ...over }); };
  const views = () => posted.map(p => p.message).filter(m => m.type === 'aac-content' && m.kind === 'view');
  return { action: globalThis.window.ccsDashboardAction, posted, views, calls, slint, historyCalls, fromFrame, restore, event: name => { for (const fn of listeners.get(name) || []) fn(); } };
}

test('in the frame: nothing is forwarded before the subscription, then the latest views and every change follow', async () => {
  const b = await loadBridge({ framed: true });
  try {
    assert.ok(b.posted.some(p => p.message.kind === 'ready'), 'the dashboard says it is ready');
    assert.ok(b.posted.every(p => p.target === ORIGIN), 'messages go to this origin only');
    assert.equal(b.views().length, 0, 'no views before the frame subscribes');
    b.fromFrame({ type: 'aac-views', overlays: ['details'] });
    const keys = new Set(b.views().map(v => v.key));
    for (const key of ['dashboard', 'refresh-interval', 'update']) assert.ok(keys.has(key), `${key} replayed`);
    for (const key of keys) assert.ok(VIEW_KEYS.includes(key));
    const dashboard = JSON.parse(b.views().findLast(v => v.key === 'dashboard').json);
    assert.equal(dashboard.version, 2);
    // the Accounts page's view goes once the dashboard is on that page (the frame sends navigate)
    await b.action('navigate', 'accounts');
    await flush(12);
    const accounts = b.views().findLast(v => v.key === 'accounts');
    assert.ok(accounts, 'the accounts view is forwarded');
    assert.equal(JSON.parse(accounts.json).version, 3);
  } finally { b.restore(); }
});

test('in the frame: an owned Details is drawn by the frame, not Slint; Back and details-closed close it', async () => {
  const b = await loadBridge({ framed: true });
  try {
    b.fromFrame({ type: 'aac-views', overlays: ['details'] });
    b.fromFrame({ type: 'aac-page', kind: 'details', value: 'codex:other' });
    await flush();
    const opened = b.views().findLast(v => v.key === 'details');
    assert.equal(JSON.parse(opened.json).id, 'codex:other');
    assert.equal(b.slint.details.length, 0, 'Slint does not draw an owned Details');
    // the frame is 1440 wide (the hidden iframe is 0): no overlay history entry on desktop
    assert.equal(b.historyCalls.filter(c => c[0] === 'push').length, 0);
    b.fromFrame({ type: 'aac-page', kind: 'details-closed', value: '' });
    await flush();
    assert.equal(b.views().findLast(v => v.key === 'details').json, '');
    assert.equal(b.slint.closed, 0);
  } finally { b.restore(); }
});

test('in the frame: refused kinds, other windows and other origins run nothing', async () => {
  const b = await loadBridge({ framed: true });
  try {
    b.fromFrame({ type: 'aac-views', overlays: ['details'] });
    const before = b.calls.length;
    b.fromFrame({ type: 'aac-page', kind: 'logout', value: '' });
    b.fromFrame({ type: 'aac-page', kind: 'login', value: 'owner\nsecret\n1' });
    b.fromFrame({ type: 'aac-page', kind: 'details', value: 'codex:one' }, { source: {} });
    b.fromFrame({ type: 'aac-page', kind: 'details', value: 'codex:one' }, { origin: 'http://evil.test' });
    await flush();
    assert.equal(b.calls.slice(before).filter(c => c.method !== 'GET').length, 0, 'no change was sent');
    assert.equal(b.views().filter(v => v.key === 'details').length, 0, 'Details did not open');
  } finally { b.restore(); }
});

test('the classic app: no frame messages, Details drawn by Slint as before', async () => {
  const b = await loadBridge({ framed: false });
  try {
    b.fromFrame({ type: 'aac-views', overlays: ['details'] });
    b.fromFrame({ type: 'aac-page', kind: 'details', value: 'codex:other' });
    await flush();
    assert.equal(b.posted.length, 0, 'nothing is posted without the frame');
    assert.equal(b.slint.details.length, 0, 'the frame messages are not listened to');
    await b.action('details', 'codex:other');
    assert.equal(b.slint.details.at(-1).id, 'codex:other');
  } finally { b.restore(); }
});

test('in the frame: an owned Analytics gets its views (motion before each), Slint gets none, layout comes from the frame', async () => {
  const b = await loadBridge({ framed: true });
  try {
    b.fromFrame({ type: 'aac-views', overlays: ['details', 'analytics'] });
    await b.action('navigate', 'analytics');
    await flush(12);
    assert.ok(b.calls.some(c => c.path.startsWith('/api/accounts/analytics?')), 'Analytics is read');
    const keys = b.views().map(v => v.key);
    const at = keys.lastIndexOf('analytics');
    assert.ok(at > 0, 'the analytics view is forwarded');
    assert.equal(keys[at - 1], 'analytics-motion', 'its motion comes just before it');
    const view = JSON.parse(b.views()[at].json);
    assert.equal(view.version, 7);
    assert.equal(typeof view.usage.trend.paths, 'object', 'the trend paths travel with the view');
    assert.deepEqual(JSON.parse(b.views().findLast(v => v.key === 'analytics-loading').json), { loading: false, error: '' });
    assert.equal(b.slint.analytics + b.slint.analyticsLoading + b.slint.trend, 0, 'nothing is drawn in Slint');
    // the hidden canvas's own box reports are ignored; the frame's are taken and the charts laid out again in them
    const before = b.views().filter(v => v.key === 'analytics').length;
    await b.action('analytics-layout', 'trend,333,111');
    await new Promise(resolve => setTimeout(resolve, 60));
    assert.equal(b.views().filter(v => v.key === 'analytics').length, before, 'a canvas report changes nothing');
    b.fromFrame({ type: 'aac-page', kind: 'analytics-layout', value: 'trend,640,220' });
    await new Promise(resolve => setTimeout(resolve, 60));
    const laid = JSON.parse(b.views().findLast(v => v.key === 'analytics').json);
    assert.equal(laid.usage.trend.pw > 0 && laid.usage.trend.pw <= 640, true, `trend laid out in the frame's width (${laid.usage.trend.pw})`);
    // a control from the page runs as the Slint page's would
    b.fromFrame({ type: 'aac-page', kind: 'analytics-split', value: 'true' });
    await flush();
    assert.equal(JSON.parse(b.views().findLast(v => v.key === 'analytics').json).state.split, true);
    assert.equal(b.views().findLast(v => v.key === 'analytics-motion').json, 'morph', 'a toggle glides from the shape on screen');
    assert.equal(b.slint.trend, 0, 'and no frame of it runs in the hidden canvas');
  } finally { b.restore(); }
});

test('in the frame without an owned Analytics: Slint draws it exactly as before and nothing is forwarded', async () => {
  const b = await loadBridge({ framed: true });
  try {
    b.fromFrame({ type: 'aac-views', overlays: ['details'] });
    await b.action('navigate', 'analytics');
    await flush(12);
    assert.ok(b.slint.analytics > 0, 'Slint draws Analytics');
    assert.ok(b.slint.analyticsLoading > 0);
    assert.equal(b.views().filter(v => v.key.startsWith('analytics')).length, 0, 'no analytics view is forwarded');
    // its canvas reports still lay the charts out
    const before = b.slint.analytics;
    await b.action('analytics-layout', 'trend,640,220');
    await new Promise(resolve => setTimeout(resolve, 60));
    assert.ok(b.slint.analytics > before, 'a canvas report draws again');
    // and a page action of the kind is refused without ownership (no layout from the frame)
    const now = b.slint.analytics;
    b.fromFrame({ type: 'aac-page', kind: 'analytics-layout', value: 'trend,500,200' });
    await new Promise(resolve => setTimeout(resolve, 60));
    assert.equal(b.slint.analytics, now);
  } finally { b.restore(); }
});
