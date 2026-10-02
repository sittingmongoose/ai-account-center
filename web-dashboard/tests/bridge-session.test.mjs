// The real browser bridge (public/bridge.js) with a synthetic server and Slint exports: signing out, an ended
// session and a pending account switch. This is controller and data proof, not canvas or visual QA.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { TRUSTED_NOTE } from '../public/accounts-view.mjs';

const flush = async (rounds = 4) => { for (let i = 0; i < rounds; i++) await new Promise(resolve => setImmediate(resolve)); };
let loads = 0;

/**
 * Loads a fresh copy of bridge.js. `server(method, path, body)` answers each request with { status, payload }
 * (or a Promise of it). Returns the dispatcher, every sign-in view handed to Slint, every toast and every request.
 */
async function loadBridge(server, { hostname = '192.168.50.179', pathname = '/accounts' } = {}) {
  const base = new URL('../public/', import.meta.url);
  let source = await readFile(new URL('bridge.js', base), 'utf8');
  const first = source.split('\n')[0];
  const declarations = first.match(/\{ ([^}]+) \}/)[1];
  const key = `__bridgeSessionWasm${++loads}`;
  source = source.replace(first, `const init = async () => {}; const { ${declarations} } = globalThis.${key};`)
    .replace(/import \{ requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard \} from '[^']+';/, 'const requireWebGL = () => {}; const WEBGL_REQUIRED_MESSAGE = "fixture"; const startSlintDashboard = fn => fn();')
    .replace(/from '(\.\/[^']+)'/g, (_match, relative) => `from '${new URL(relative, base).href}'`)
    .concat(`\n// ${key}\n`);
  const auths = [], toasts = [], calls = [];
  const originals = new Map();
  const assign = (name, value) => {
    if (!originals.has(name)) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  };
  const restore = () => { for (const [name, descriptor] of originals) descriptor ? Object.defineProperty(globalThis, name, descriptor) : delete globalThis[name]; };
  const stored = new Map();
  assign(key, Object.fromEntries(declarations.split(', ').map(name => [name,
    name === 'set_auth' ? (signedIn, json) => auths.push({ signedIn, ...JSON.parse(json) })
      : name === 'push_toast' ? (kind, title, body) => toasts.push({ kind, title, body })
        : () => {}])));
  assign('window', {});
  assign('navigator', { userAgent: 'Mac fixture' });
  assign('location', { pathname, search: '', protocol: 'http:', hostname, host: `${hostname}:3000`, origin: `http://${hostname}:3000`, href: '' });
  assign('document', { querySelector: () => ({ hidden: false, textContent: '' }) });
  assign('innerWidth', 1920); assign('innerHeight', 1080); assign('devicePixelRatio', 1);
  assign('addEventListener', () => {}); assign('matchMedia', () => ({ matches: true }));
  assign('localStorage', { getItem: k => stored.has(k) ? stored.get(k) : null, setItem: (k, v) => stored.set(k, String(v)), removeItem: k => stored.delete(k) });
  assign('setInterval', () => 1); assign('clearInterval', () => {});
  assign('fetch', async (path, options = {}) => {
    const method = options.method || 'GET';
    const body = options.body ? JSON.parse(options.body) : null;
    calls.push({ method, path, body });
    const answer = await server(method, path, body);
    if (!answer) throw new Error(`Unexpected fixture request: ${method} ${path}`);
    const status = answer.status ?? 200;
    return { ok: status >= 200 && status < 300, status, json: async () => answer.payload, headers: { get: () => null } };
  });
  try {
    await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
  } catch (error) { restore(); throw error; }
  return { action: globalThis.window.ccsDashboardAction, auths, toasts, calls, restore, lastAuth: () => auths.at(-1) };
}

/** A dashboard server with a session, local network trust and the routes the Accounts page reads. */
function fixtureServer(over = {}) {
  const s = { trust: true, signedIn: true, revoked: false, ...over };
  const view = () => ({ trustLocalNetwork: s.trust, trustedNetworks: ['192.168.0.0/16'], connection: { peer: '192.168.50.20', trusted: s.trust }, canTurnOn: false });
  const accounts = [
    { id: 'codex:one', provider: 'codex', email: 'one@example.test', isActive: true, status: 'ok', windows: [], capabilities: { codexProfile: 'one' } },
    { id: 'codex:other', provider: 'codex', email: 'other@example.test', isActive: false, status: 'ok', windows: [], capabilities: { codexProfile: 'other' } },
  ];
  const routes = {
    'GET /api/auth/setup': () => ({ payload: { sessionTimeoutHours: 24, configured: true } }),
    'GET /api/auth/check': () => ({ payload: { authenticated: s.signedIn, authRequired: true, accessMode: 'login', username: s.signedIn ? 'owner' : null, signedOutReason: null, secureTransport: s.trust, trustedLocalNetwork: s.trust, connection: { peer: '192.168.50.20', trusted: s.trust } } }),
    'GET /api/accounts/settings': () => ({ payload: { refreshIntervalSeconds: 60 } }),
    'GET /api/claude/desktop-profiles': () => ({ payload: { profiles: [] } }),
    'GET /api/antigravity/profiles': () => ({ status: 404, payload: { code: 'not_found' } }),
    'GET /api/antigravity/auto-switch': () => ({ status: 404, payload: { code: 'not_found' } }),
    'GET /api/app-updates/status': () => ({ payload: { job: null } }),
    'GET /api/accounts/registry': () => ({ payload: { providers: [], accounts: [], jobs: [], trash: [] } }),
    'GET /api/auth/session': () => ({ payload: { username: 'owner', otherBrowsers: 0, secureTransport: s.trust, managedBy: 'config' } }),
    'GET /api/auth/devices': () => ({ payload: { devices: [] } }),
    'GET /api/auth/network': () => ({ payload: view() }),
    'PUT /api/auth/network': body => { s.trust = body.trustLocalNetwork; return { payload: view() }; },
    'POST /api/auth/logout': () => { s.signedIn = false; return { payload: { success: true } }; },
    ...over.routes,
  };
  const server = async (method, path, body) => {
    if (method === 'GET' && path.startsWith('/api/accounts/dashboard?')) {
      if (s.revoked) return { status: 401, payload: { error: 'Signed out', code: 'session_revoked' } };
      return { payload: { schemaVersion: 1, updatedAt: new Date().toISOString(), accounts, providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: { enabled: false, thresholdPercent: 5 } } };
    }
    const route = routes[`${method} ${path}`];
    return route ? route(body) : null;
  };
  return { s, server };
}

test('after Turn off, the sign-in page says trust is off: from the PUT answer, and from a fresh check at sign-out', async () => {
  const { s, server } = fixtureServer();
  const b = await loadBridge(server);
  try {
    assert.equal(b.lastAuth().signedIn, true);
    assert.equal(b.lastAuth().transportNote, TRUSTED_NOTE);
    // the review's case: Turn off on the LAN address, then Sign out
    await b.action('network-off', '');
    assert.equal(s.trust, false);
    await b.action('logout', '');
    await flush();
    const page = b.lastAuth();
    assert.deepEqual([page.signedIn, page.state, page.message], [false, 'default', 'Signed out.']);
    assert.notEqual(page.transportNote, TRUSTED_NOTE);
    assert.match(page.transportNote, /local network trust is off/);
  } finally { b.restore(); }
  // trust turned off in another browser: the sign-out reads the check again
  const other = fixtureServer();
  const c = await loadBridge(other.server);
  try {
    other.s.trust = false;
    await c.action('logout', '');
    await flush();
    assert.equal(c.lastAuth().state, 'default');
    assert.match(c.lastAuth().transportNote, /local network trust is off/);
  } finally { c.restore(); }
});

test('an ended session shows the sign-in page with the trust as it is now, not as it was at sign-in', async () => {
  const { s, server } = fixtureServer();
  const b = await loadBridge(server);
  try {
    s.trust = false; s.revoked = true; s.signedIn = false;
    await b.action('refresh', '');
    await flush();
    const page = b.lastAuth();
    assert.deepEqual([page.signedIn, page.state, page.bannerTitle], [false, 'expired', 'Signed out from another browser']);
    assert.match(page.transportNote, /local network trust is off/);
  } finally { b.restore(); }
});

test('Sign out runs while a save is in flight, and a failure is said in the page\'s words', async () => {
  let release = null;
  const { s, server } = fixtureServer({ routes: {
    'PUT /api/accounts/settings': body => new Promise(resolve => { release = () => resolve({ payload: { refreshIntervalSeconds: body.refreshIntervalSeconds } }); }),
  } });
  const b = await loadBridge(server);
  try {
    const saving = b.action('refresh-interval', '120s');
    await flush();
    assert.ok(release, 'the interval save is in flight (busy)');
    await b.action('logout', '');
    assert.ok(b.calls.some(c => c.method === 'POST' && c.path === '/api/auth/logout'), 'Sign out was sent while busy');
    assert.equal(s.signedIn, false);
    assert.equal(b.lastAuth().state, 'default');
    release();
    await saving;
  } finally { b.restore(); }
  // a codeless 500 from logout: never the server's sentence, and this browser stays signed in
  const failing = fixtureServer({ routes: { 'POST /api/auth/logout': () => ({ status: 500, payload: { error: 'Failed to logout' } }) } });
  const c = await loadBridge(failing.server);
  try {
    const before = c.auths.length;
    await c.action('logout', '');
    assert.equal(c.auths.length, before, 'still signed in');
    const toast = c.toasts.at(-1);
    assert.deepEqual([toast.kind, toast.title, toast.body], ['err', 'Not signed out', 'Sign-out failed on the dashboard. Try again.']);
    assert.doesNotMatch(JSON.stringify(c.toasts), /Failed to logout/);
  } finally { c.restore(); }
});

test('a pending account-switch confirmation holds the Accounts & Settings changes and Sign out', async () => {
  const token = 'A'.repeat(43);
  const { server } = fixtureServer({ routes: {
    'POST /api/codex/profiles/other/activate': () => ({ status: 409, payload: { error: 'Confirm', code: 'confirmation_required', confirmation: { token, targetProfile: 'other', expiresAt: new Date(Date.now() + 60_000).toISOString(), processes: [] } } }),
    'POST /api/accounts/codex%3Aother/remove': () => ({ payload: { confirmation: { token: 't', effects: ['Its saved login is deleted from Ubuntu.'] } } }),
  } });
  const b = await loadBridge(server);
  try {
    await b.action('activate', 'other');
    const removes = () => b.calls.filter(c => c.path === '/api/accounts/codex%3Aother/remove').length;
    await b.action('remove', 'codex:other');
    assert.equal(removes(), 0, 'Remove waits for the switch');
    assert.equal(b.toasts.at(-1).title, 'Finish the account switch first');
    await b.action('accounts-show', 'zai:hide');
    await b.action('logout', '');
    assert.equal(b.calls.some(c => c.method !== 'GET' && (c.path === '/api/accounts/visibility' || c.path === '/api/auth/logout')), false);
    // a local action (opening the password form) stays live
    const toastsBefore = b.toasts.length;
    await b.action('pw-toggle', '');
    assert.equal(b.toasts.length, toastsBefore);
    // once the switch is cancelled, Remove asks the server as usual
    await b.action('activation-cancel', '');
    await b.action('remove', 'codex:other');
    assert.equal(removes(), 1);
  } finally { b.restore(); }
});
