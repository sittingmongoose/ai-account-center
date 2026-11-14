// The app frame's sign-in session (public/frame-session.mjs): the same server calls and words as bridge.js's sign-in,
// against a fake server. No real account data; test passwords only.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createFrameSession, OFFLINE_RETRY_MS } from '../public/frame-session.mjs';

function headers(map = {}) {
  const lower = Object.fromEntries(Object.entries(map).map(([k, v]) => [k.toLowerCase(), String(v)]));
  return { get: (name) => lower[String(name).toLowerCase()] ?? null, has: (name) => String(name).toLowerCase() in lower };
}

/** A fake server: `routes[path]` answers { status, body, headers } or a function of the request body. */
function fakeServer(routes) {
  const calls = [];
  const fetch = async (path, options = {}) => {
    calls.push({ path, method: options.method || 'GET', body: options.body ? JSON.parse(options.body) : null });
    let answer = routes[path];
    if (typeof answer === 'function') answer = answer(options.body ? JSON.parse(options.body) : null);
    if (answer === 'offline') throw new TypeError('Failed to fetch');
    if (!answer) answer = { status: 404, body: { error: 'Not found' } };
    return {
      ok: answer.status >= 200 && answer.status < 300,
      status: answer.status,
      headers: headers(answer.headers),
      json: async () => answer.body ?? null,
    };
  };
  return { fetch, calls };
}

function memoryStorage(initial = {}) {
  const data = new Map(Object.entries(initial));
  return {
    getItem: (k) => (data.has(k) ? data.get(k) : null),
    setItem: (k, v) => data.set(k, String(v)),
    removeItem: (k) => data.delete(k),
    dump: () => Object.fromEntries(data),
  };
}

const NOW = Date.UTC(2026, 10, 10, 12, 0, 0);
const SETUP = { status: 200, body: { configured: true, sessionTimeoutHours: 720 } };
const LOCATION = { protocol: 'http:', hostname: 'aac.lan', host: 'aac.lan:3000' };

function session(routes, extra = {}) {
  const server = fakeServer({ '/api/auth/setup': SETUP, ...routes });
  const timers = [];
  const s = createFrameSession({
    fetch: server.fetch,
    storage: extra.storage ?? memoryStorage(),
    location: LOCATION,
    now: () => NOW,
    setInterval: (fn, ms) => { timers.push({ fn, ms }); return timers.length; },
    clearInterval: () => {},
    visible: () => true,
  });
  const views = [];
  s.onChange((v) => views.push(v));
  return { s, server, views, timers, last: () => views[views.length - 1] };
}

test('a signed-in browser goes straight to the frame', async () => {
  const { s, last } = session({ '/api/auth/check': { status: 200, body: { authenticated: true, username: 'labadmin' } } });
  await s.start();
  assert.equal(last().signedIn, true);
  assert.equal(last().username, 'labadmin');
  assert.equal(last().host, 'aac.lan:3000');
  assert.equal(last().fresh, undefined, 'a session the browser already had is not a fresh sign-in');
});

test('the right password signs in with the same call as the dashboard, and remembers the sign-in', async () => {
  const storage = memoryStorage();
  const { s, server, last } = session(
    {
      '/api/auth/check': { status: 200, body: { authenticated: false } },
      '/api/auth/login': { status: 200, body: { username: 'labadmin' } },
    },
    { storage }
  );
  await s.start();
  assert.equal(last().state, 'default');
  await s.signIn({ username: 'labadmin', password: 'Lab-Correct-Horse-Battery-9', remember: true });
  assert.equal(last().signedIn, true);
  assert.equal(last().fresh, true, 'a sign-in through the form is fresh: the frame may reopen the last tab');
  const login = server.calls.find((c) => c.path === '/api/auth/login');
  assert.deepEqual(login.body, { username: 'labadmin', password: 'Lab-Correct-Horse-Battery-9', rememberMe: true });
  assert.ok(Object.keys(storage.dump()).length > 0, 'the sign-in time is kept, as the dashboard keeps it');
});

test('an empty form says what is missing without calling the server', async () => {
  const { s, server, last } = session({ '/api/auth/check': { status: 200, body: { authenticated: false } } });
  await s.start();
  await s.signIn({ username: 'labadmin', password: '' });
  assert.equal(last().message, 'Enter your username and password.');
  assert.equal(server.calls.some((c) => c.path === '/api/auth/login'), false);
});

test('a wrong password says so, with the tries left', async () => {
  const { s, last } = session({
    '/api/auth/check': { status: 200, body: { authenticated: false } },
    '/api/auth/login': { status: 401, body: { error: 'Invalid credentials', code: 'wrong_password' }, headers: { 'RateLimit-Remaining': '3', 'RateLimit-Policy': '5;w=900' } },
  });
  await s.start();
  await s.signIn({ username: 'labadmin', password: 'not-it' });
  assert.equal(last().state, 'wrong');
  assert.equal(last().signedIn, false);
  assert.equal(last().message, "Username or password isn't right.");
  assert.match(last().messageSub || '', /3/);
});

test('too many tries pauses sign-in with a countdown, and it reopens when the countdown ends', async () => {
  const { s, last } = session({
    '/api/auth/check': { status: 200, body: { authenticated: false } },
    '/api/auth/login': { status: 429, body: { error: 'Too many attempts' }, headers: { 'Retry-After': '120' } },
  });
  await s.start();
  await s.signIn({ username: 'labadmin', password: 'not-it' });
  assert.equal(last().state, 'limited');
  assert.equal(last().retrySeconds, 120);
  assert.ok(last().bannerTitle, 'the limited banner has a title');
  s.limitOver();
  assert.equal(last().state, 'default');
  assert.equal(last().message, 'Sign-in is open again.');
  assert.equal(last().notice, true);
});

test('first run shows the setup form and creates the sign-in with the same call', async () => {
  const { s, server, last } = session({
    '/api/auth/setup': (body) => (body ? { status: 200, body: { username: 'labadmin' } } : { status: 200, body: { setupCodeRequired: false } }),
    '/api/auth/check': { status: 200, body: { authenticated: false, accessMode: 'setup' } },
  });
  await s.start();
  assert.equal(last().state, 'setup');
  assert.equal(last().setupForm, true);
  s.typing('short', '');
  assert.equal(last().strength.word, 'Too short');
  await s.createSignIn({ username: 'labadmin', password: 'Lab-Correct-Horse-Battery-9', confirm: 'nope' });
  assert.equal(last().state, 'setup');
  assert.ok(last().message, 'a mismatch is named');
  await s.createSignIn({ username: 'labadmin', password: 'Lab-Correct-Horse-Battery-9', confirm: 'Lab-Correct-Horse-Battery-9' });
  assert.equal(last().signedIn, true);
  const posted = server.calls.find((c) => c.path === '/api/auth/setup' && c.method === 'POST');
  assert.deepEqual(posted.body, { username: 'labadmin', password: 'Lab-Correct-Horse-Battery-9' });
});

test('an old server without the setup form shows the command, and Check again asks again', async () => {
  const { s, server, last } = session({
    '/api/auth/setup': { status: 200, body: {} },
    '/api/auth/check': { status: 200, body: { authenticated: false, accessMode: 'setup' } },
  });
  await s.start();
  assert.equal(last().state, 'setup');
  assert.equal(last().setupForm, false);
  assert.match(last().bannerBody, /ai-account-center dashboard auth setup/);
  const before = server.calls.length;
  await s.recheck();
  assert.ok(server.calls.length > before);
});

test('a revoked session says it was revoked', async () => {
  const { s, last } = session({ '/api/auth/check': { status: 200, body: { authenticated: false, signedOutReason: 'revoked' } } });
  await s.start();
  assert.equal(last().state, 'expired');
  assert.ok(last().bannerTitle);
});

test('a session the server no longer knows says it ended, from the time it was last seen', async () => {
  const storage = memoryStorage();
  const { s, last } = session({ '/api/auth/check': { status: 200, body: { authenticated: true, username: 'labadmin' } } }, { storage });
  await s.start();
  assert.equal(last().signedIn, true);
  s.ended();
  assert.equal(last().signedIn, false);
  assert.equal(last().state, 'expired');
  assert.ok(last().bannerTitle && last().bannerBody);
});

test('signing out makes the same call and shows the signed-out page', async () => {
  const { s, server, last } = session({
    '/api/auth/check': { status: 200, body: { authenticated: true, username: 'labadmin' } },
    '/api/auth/logout': { status: 200, body: { ok: true } },
  });
  await s.start();
  const result = await s.signOut();
  assert.equal(result.ok, true);
  assert.ok(server.calls.some((c) => c.path === '/api/auth/logout' && c.method === 'POST'));
  assert.equal(last().signedIn, false);
  assert.equal(last().message, 'Signed out.');
});

test('a failed sign-out keeps the browser signed in and says why', async () => {
  const { s, last } = session({
    '/api/auth/check': { status: 200, body: { authenticated: true, username: 'labadmin' } },
    '/api/auth/logout': { status: 500, body: { error: 'Boom' } },
  });
  await s.start();
  const result = await s.signOut();
  assert.equal(result.ok, false);
  assert.ok(result.failure?.title);
  assert.equal(last().signedIn, true);
});

test('no answer at launch is the offline page, retried while it is shown', async () => {
  const { s, timers, last } = session({ '/api/auth/check': 'offline', '/api/auth/setup': 'offline' });
  await s.start();
  assert.equal(last().state, 'offline');
  assert.ok(last().offlineTitle);
  assert.equal(timers.length, 1);
  assert.equal(timers[0].ms, OFFLINE_RETRY_MS);
});

test('the dashboard signing out inside the frame shows the same signed-out page', async () => {
  const { s, last } = session({ '/api/auth/check': { status: 200, body: { authenticated: true, username: 'labadmin' } } });
  await s.start();
  s.signedOutElsewhere();
  assert.equal(last().signedIn, false);
  assert.equal(last().message, 'Signed out.');
});

test("sign out's fallback: a session the server already ended only updates this page, with no second logout", async () => {
  let authenticated = true;
  const { s, server, last } = session({
    '/api/auth/check': () => ({ status: 200, body: { authenticated, username: 'labadmin' } }),
    '/api/auth/logout': { status: 200, body: { ok: true } },
  });
  await s.start();
  authenticated = false; // the dashboard's own logout reached the server; its answer is still on the way
  const result = await s.finishSignOut();
  assert.equal(result.ok, true);
  assert.equal(server.calls.filter((c) => c.path === '/api/auth/logout').length, 0);
  assert.equal(last().signedIn, false);
  assert.equal(last().message, 'Signed out.');
});

test("sign out's fallback: a session still signed in gets exactly one logout", async () => {
  const { s, server, last } = session({
    '/api/auth/check': { status: 200, body: { authenticated: true, username: 'labadmin' } },
    '/api/auth/logout': { status: 200, body: { ok: true } },
  });
  await s.start();
  const result = await s.finishSignOut();
  assert.equal(result.ok, true);
  assert.equal(server.calls.filter((c) => c.path === '/api/auth/logout').length, 1);
  assert.equal(last().signedIn, false);
});
