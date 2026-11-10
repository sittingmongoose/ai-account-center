// Frame recheck 3 (M3) regression: Sign out's fallback check is answered only after the frame signed out and signed in again;
// finishSignOut then ends the NEW session. Real frame-session.mjs; a fake server with cookie-bound sessions.
import assert from 'node:assert/strict';
import { test } from 'node:test';
const mod = await import(process.env.FRAME_SESSION ?? new URL('../public/frame-session.mjs', import.meta.url).href);
const { createFrameSession } = mod;

function server() {
  let next = 1; const live = new Set(); let cookie = 0; const calls = [];
  let holdCheck = null;
  const reply = (status, body) => ({ ok: status < 300, status, headers: { get: () => null }, json: async () => body });
  const fetch = async (path, options = {}) => {
    const sent = cookie; // a browser attaches the cookie when it sends the request
    calls.push({ path, cookie: sent });
    if (path === '/api/auth/setup') return reply(200, { configured: true, sessionTimeoutHours: 720 });
    if (path === '/api/auth/check') {
      const body = { authRequired: true, authenticated: live.has(sent), username: 'labadmin' };
      if (holdCheck) { const h = holdCheck; holdCheck = null; await h; }
      return reply(200, body);
    }
    if (path === '/api/auth/login') { const id = next++; live.add(id); cookie = id; return reply(200, { username: 'labadmin' }); }
    if (path === '/api/auth/logout') { live.delete(sent); cookie = 0; return reply(200, { success: true }); }
    return reply(404, {});
  };
  return { fetch, calls, live, login: () => { const id = next++; live.add(id); cookie = id; }, endCurrent: () => { live.delete(cookie); cookie = 0; }, hold: (p) => { holdCheck = p; }, get cookie() { return cookie; } };
}

test('fallback check answered after a fresh sign-in must not end the new session', async () => {
  const srv = server();
  srv.login(); // session 1
  const s = createFrameSession({ fetch: srv.fetch, storage: null, location: { protocol: 'http:', hostname: 'aac.lan', host: 'aac.lan' }, setInterval: () => 0, clearInterval: () => {} });
  await s.start();
  assert.equal(s.view().signedIn, true);
  // t=10 s: the frame's fallback fires (epoch still matches), its /api/auth/check is slow.
  let release; srv.hold(new Promise((r) => { release = r; }));
  const fallback = s.finishSignOut();
  await new Promise((r) => setTimeout(r, 5));
  // The dashboard's own logout now lands: session 1 ends, the dashboard tells the frame (signedOutElsewhere).
  srv.endCurrent();
  s.signedOutElsewhere();
  assert.equal(s.view().signedIn, false);
  // The person signs in again in the frame: session 2.
  await s.signIn({ username: 'labadmin', password: 'test-password-only', remember: true });
  assert.equal(s.view().signedIn, true);
  const second = srv.cookie;
  // The slow check answers (it was read for session 1, still signed in when it was read).
  release();
  await fallback;
  console.log('calls', JSON.stringify(srv.calls));
  console.log('session 2 alive:', srv.live.has(second), 'frame signedIn:', s.view().signedIn);
  assert.equal(srv.live.has(second), true, 'the later session was ended by the earlier Sign out');
  assert.equal(s.view().signedIn, true);
});
