// The fixture the dashboard controller's tests share (tests/frame-dashboard.test.mjs and
// tests/frame-dashboard-golden.test.mjs): a synthetic dashboard server with a session and the routes Home and Accounts
// read and change, an Antigravity server with two verified Ubuntu logins, and the controller started as the frame
// starts it. Synthetic data only.
import assert from 'node:assert/strict';
import { createFrameDashboard } from '../../public/frame-dashboard.mjs';

export const ORIGIN = 'http://192.0.2.10:3000';
export const flush = async (rounds = 8) => { for (let i = 0; i < rounds; i++) await new Promise((resolve) => setImmediate(resolve)); };

/** A dashboard server with a session and the routes Home and Accounts read and change. Fresh state per call. */
export function fixtureServer(over = {}) {
  const s = { signedIn: true, revoked: false, interval: 60, ...over };
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
    'GET /api/app-updates/status': () => ({ payload: { job: null } }),
    'POST /api/app-updates/start': () => ({ status: 409, payload: { error: 'Already running', job } }),
    'GET /api/accounts/registry': () => ({ payload: { providers: [], accounts: [], jobs: [], trash: [] } }),
    'GET /api/auth/session': () => ({ payload: { username: 'owner', otherBrowsers: 0, secureTransport: true, managedBy: 'config' } }),
    'GET /api/auth/devices': () => ({ payload: { devices: [] } }),
    'GET /api/auth/network': () => ({ payload: { trustLocalNetwork: true, connection: { peer: '192.168.10.20', trusted: true }, canTurnOn: false } }),
    'POST /api/auth/logout': () => { s.signedIn = false; return { payload: { success: true } }; },
    'POST /api/accounts/codex%3Aother/remove': () => ({ payload: { confirmation: { token: 't'.repeat(43), effects: ['Its saved login is deleted from Ubuntu.'] } } }),
    ...over.routes,
  };
  const server = async (method, path, body) => {
    if (method === 'GET' && path.startsWith('/api/accounts/dashboard?')) {
      if (s.revoked) return { status: 401, payload: { error: 'Signed out', code: 'session_revoked' } };
      if (!s.signedIn) return { status: 401, payload: { error: 'Authentication required', code: 'auth_required' } };
      return { payload: { schemaVersion: 1, updatedAt: updated, accounts: structuredClone(accounts), providers: [], settings: { refreshIntervalSeconds: s.interval, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] } } };
    }
    const route = routes[`${method} ${path}`];
    return route ? route(body) : { status: 404, payload: { code: 'not_found' } };
  };
  return { s, server };
}

export function respond(answer) {
  const status = answer.status ?? 200;
  return { ok: status >= 200 && status < 300, status, json: async () => answer.payload, headers: { get: () => null } };
}

/** The controller, started as the frame starts it, with the same server. */
export async function loadController(server, { width = 1440, page = 'home', check = undefined, stored = new Map() } = {}) {
  const calls = [], headers = [], emitted = [], intervals = [], historyCalls = [];
  let depth = 0;
  const c = createFrameDashboard({
    fetch: async (path, options = {}) => {
      const method = options.method || 'GET';
      const body = options.body ? JSON.parse(options.body) : null;
      calls.push({ method, path, body });
      headers.push({ path, headers: options.headers ?? {} });
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
    c, calls, headers, emitted, intervals, historyCalls, stored,
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
export function jsonDiff(a, b, path = '', out = []) {
  if (JSON.stringify(a) === JSON.stringify(b)) return out;
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object') { out.push(`${path || '.'}: ${JSON.stringify(a)?.slice(0, 160)} != ${JSON.stringify(b)?.slice(0, 160)}`); return out; }
  for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) jsonDiff(a[k], b[k], `${path}.${k}`, out);
  return out;
}
export function sameView(actual, expected, message) {
  if (actual === expected) return;
  let parsed = null;
  try { parsed = [JSON.parse(actual), JSON.parse(expected)]; } catch {}
  assert.fail(`${message}: ${parsed ? jsonDiff(...parsed).slice(0, 12).join('; ') : `${actual} != ${expected}`}`);
}
/** Two lists of [kind, key, json] (views, toasts, header lines), compared entry by entry. */
export function sameOut(actual, expected, message) {
  assert.deepEqual(actual.map((x) => x.slice(0, 2)), expected.map((x) => x.slice(0, 2)), `${message}: the same kinds, in order`);
  actual.forEach((x, i) => (x[0] === 'view' ? sameView(x[2], expected[i][2], `${message} (${x[1]})`) : assert.deepEqual(x, expected[i], message)));
}
export const writes = (calls) => calls.filter((c) => c.method !== 'GET');
export const reads = (calls) => calls.map((c) => `${c.method} ${c.path}`);

