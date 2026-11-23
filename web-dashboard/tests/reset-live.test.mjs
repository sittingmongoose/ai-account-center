import test from 'node:test';
import assert from 'node:assert/strict';
import { createResetHints } from '../public/reset-live.mjs';

function browserPair({ channel = true } = {}) {
  const windows = [], channels = new Set();
  function browser() {
    const win = new EventTarget();
    win.document = new EventTarget();
    win.document.visibilityState = 'visible';
    win.location = { protocol: 'https:', host: 'synthetic.invalid' };
    win.timers = new Map();
    win.delays = [];
    win.cleared = [];
    win.setTimeout = (fn, ms) => { const id = Symbol(); win.timers.set(id, fn); win.delays.push(ms); return id; };
    win.clearTimeout = (id) => { win.cleared.push(id); win.timers.delete(id); };
    win.sockets = [];
    win.WebSocket = class {
      constructor(url) { this.url = url; win.sockets.push(this); }
      close() { this.closed = true; }
    };
    // GET /api/auth/check always answers 200, signed in or out; win.auth is the answer it gives.
    win.checks = 0;
    win.auth = { authenticated: true, authRequired: true, wsAllowed: true };
    win.fetch = async (url, options) => {
      win.checks++;
      win.lastFetch = { url, options };
      return { ok: true, json: async () => win.auth };
    };
    win.BroadcastChannel = class {
      constructor(name) { if (!channel) throw Error('unsupported'); this.name = name; channels.add(this); }
      postMessage(data) { for (const peer of channels) if (peer !== this && peer.name === this.name) peer.onmessage?.({ data }); }
      close() { channels.delete(this); }
    };
    win.localStorage = { setItem(key, value) {
      for (const peer of windows) if (peer !== win) {
        const event = new Event('storage'); event.key = key; event.newValue = value; peer.dispatchEvent(event);
      }
    } };
    windows.push(win);
    return win;
  }
  return { browser };
}

/** Expire every pending reconnect timer (a snapshot: a fired timer may schedule the next one). */
async function fire(win) {
  for (const [id, fn] of [...win.timers]) { win.timers.delete(id); await fn(); }
}

test('server hints reach an already-open second device over the authenticated dashboard wss path', () => {
  const { browser } = browserPair();
  const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  assert.equal(win.sockets[0].url, 'wss://synthetic.invalid/ws');
  win.sockets[0].onmessage({ data: JSON.stringify({ type: 'accounts-changed' }) });
  assert.equal(fences, 0);
  win.sockets[0].onmessage({ data: '{invalid' });
  win.sockets[0].onmessage({ data: JSON.stringify({ type: 'reset-claims-changed' }) });
  assert.equal(fences, 1);
  stop(); assert.ok(win.sockets[0].closed);
});
for (const channel of [true, false]) {
  test(`same-browser prepare invalidates an already-open peer, storage fallback=${!channel}`, () => {
    const { browser } = browserPair({ channel });
    const a = browser(), b = browser();
    const first = createResetHints(a), second = createResetHints(b);
    let own = 0, peer = 0;
    const stopA = first.subscribe(() => own++), stopB = second.subscribe(() => peer++);
    first.publish();
    assert.equal(own, 0);
    assert.ok(peer >= 1);
    stopB(); const old = peer;
    first.publish(); assert.equal(peer, old);
    stopA();
  });
}
test('focus, visibility, reconnect and online invalidate before another server read', async () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  for (const type of ['focus', 'online', 'aac-shown']) win.dispatchEvent(new Event(type));
  win.document.dispatchEvent(new Event('visibilitychange'));
  assert.equal(fences, 4);
  win.document.visibilityState = 'hidden'; win.dispatchEvent(new Event('focus'));
  assert.equal(fences, 4);
  win.sockets[0].onopen(); win.sockets[0].onclose(); assert.equal(fences, 6);
  assert.equal(win.timers.size, 1);
  await fire(win);
  assert.equal(win.sockets.length, 2);
  stop();
  assert.equal(win.timers.size, 0);
  win.document.visibilityState = 'visible'; win.dispatchEvent(new Event('focus'));
  assert.equal(fences, 6);
});
test('all listener owners must detach before the shared transport is disposed', () => {
  const { browser } = browserPair(); const win = browser(); const bus = createResetHints(win);
  const a = bus.subscribe(() => {}), b = bus.subscribe(() => {});
  assert.equal(win.sockets.length, 1); a(); assert.ok(!win.sockets[0].closed);
  b(); assert.ok(win.sockets[0].closed);
});


test('a server hint relays once to browser peers even when their own sockets have missed it', () => {
  const { browser } = browserPair(); const a = browser(), b = browser();
  let first=0, second=0;
  const stopA=createResetHints(a).subscribe(() => first++);
  const stopB=createResetHints(b).subscribe(() => second++);
  const data=JSON.stringify({type:'reset-claims-changed',id:'server-event-1'});
  a.sockets[0].onmessage({data});
  assert.equal(first,1); assert.equal(second,1); // BC and storage duplicates share one id.
  b.sockets[0].onmessage({data});
  assert.equal(first,1); assert.equal(second,1); // A delayed WS copy cannot re-fence a recovered view.
  stopA(); stopB();
});

test('a signed-out close backs off, checks the session once, then stops', async () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  win.sockets[0].onopen();
  win.auth = { authenticated: false, authRequired: true };
  win.sockets[0].onclose({ code: 4001, reason: 'Signed out', wasClean: true });
  assert.equal(fences, 2);
  assert.equal(win.timers.size, 1);
  assert.equal(win.delays.at(-1), 1000);
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.lastFetch.url, '/api/auth/check');
  assert.equal(win.lastFetch.options.credentials, 'same-origin');
  assert.equal(win.sockets.length, 1);
  assert.equal(win.timers.size, 0);
  for (const type of ['focus', 'online']) win.dispatchEvent(new Event(type));
  assert.equal(win.sockets.length, 1);
  stop();
});
test('an upgrade refused before any live session checks the session, then stops', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.auth = { authenticated: false, authRequired: true };
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  assert.equal(win.timers.size, 1);
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.sockets.length, 1);
  assert.equal(win.timers.size, 0);
  stop();
});
test('a transient first failure on a signed-in browser reconnects', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  assert.equal(win.timers.size, 1);
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.sockets.length, 2);
  win.sockets[1].onopen();
  stop();
});
test('session rotation: a 4001 close on a still-signed-in browser reconnects after the check', async () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 4001, reason: 'Signed out', wasClean: true });
  assert.equal(win.timers.size, 1);
  assert.equal(win.delays.at(-1), 1000); // the rotation's Set-Cookie has landed by then
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.sockets.length, 2);
  win.sockets[1].onopen();
  win.sockets[1].onmessage({ data: JSON.stringify({ type: 'reset-claims-changed', id: 'rotated-1' }) });
  assert.equal(fences, 4);
  stop();
});
test('a signed-out session after a live drop stops after one check, with no socket attempt', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  win.auth = { authenticated: false, authRequired: true };
  assert.equal(win.timers.size, 1);
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.sockets.length, 1);
  assert.equal(win.timers.size, 0);
  win.dispatchEvent(new Event('online'));
  assert.equal(win.sockets.length, 1);
  stop();
});
test('an open dashboard (authRequired false) reconnects without a session', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  win.auth = { authenticated: false, authRequired: false, wsAllowed: true };
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.sockets.length, 2);
  stop();
});
test('open mode from another device stops retrying when the server says the socket may not open', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  // Auth is off, but this peer is not on the dashboard computer: the upgrade refuses /ws.
  win.auth = { authenticated: false, authRequired: false, wsAllowed: false };
  assert.equal(win.timers.size, 1);
  await fire(win);
  assert.equal(win.checks, 1);
  assert.equal(win.sockets.length, 1); // no new socket attempt
  assert.equal(win.timers.size, 0); // stopped, not a retry loop
  win.dispatchEvent(new Event('online'));
  assert.equal(win.sockets.length, 1);
  stop();
});
test('a live drop backs off instead of retrying every second', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  const drop = async () => {
    win.sockets.at(-1).onclose({ code: 1006, wasClean: false });
    assert.equal(win.timers.size, 1);
    await fire(win);
  };
  for (let i = 0; i < 7; i++) await drop();
  assert.deepEqual(win.delays, [1000, 2000, 4000, 8000, 16000, 30000, 30000]);
  assert.equal(win.sockets.length, 8);
  assert.equal(win.checks, 7);
  win.sockets.at(-1).onopen();
  win.sockets.at(-1).onclose({ code: 1006, wasClean: false });
  assert.equal(win.delays.at(-1), 1000);
  stop();
});
test('a dropped connection on a live session still reconnects', async () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  win.sockets[0].onopen();
  win.sockets[0].onmessage({ data: JSON.stringify({ type: 'reset-claims-changed', id: 'live-1' }) });
  assert.equal(fences, 2);
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  assert.equal(win.timers.size, 1);
  await fire(win);
  assert.equal(win.sockets.length, 2);
  // The first retry misses (the server is still down): a live session keeps trying, with back-off.
  win.sockets[1].onclose({ code: 1006, wasClean: false });
  assert.equal(win.delays.at(-1), 2000);
  await fire(win);
  assert.equal(win.sockets.length, 3);
  win.sockets[2].onopen();
  win.sockets[2].onmessage({ data: JSON.stringify({ type: 'reset-claims-changed', id: 'live-2' }) });
  assert.equal(fences, 6);
  stop();
});
test('a failed session check keeps backing off instead of stopping or opening', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  const goodFetch = win.fetch;
  win.fetch = async () => { win.checks++; throw Error('network down'); };
  await fire(win);
  assert.equal(win.sockets.length, 1);
  assert.equal(win.timers.size, 1);
  assert.equal(win.delays.at(-1), 2000);
  win.fetch = async () => { win.checks++; return { ok: false, status: 503 }; };
  await fire(win);
  assert.equal(win.sockets.length, 1);
  assert.equal(win.delays.at(-1), 4000);
  win.fetch = goodFetch;
  await fire(win);
  assert.equal(win.sockets.length, 2);
  stop();
});
test('stop() during an in-flight session check cannot open a socket', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  win.fetch = async () => { win.checks++; await gate; return { ok: true, json: async () => win.auth }; };
  const fired = fire(win);
  stop();
  release();
  await fired;
  assert.equal(win.sockets.length, 1);
});
test('a stale in-flight check cannot open a socket into a later subscribe cycle', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win);
  const stopA = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  win.fetch = async () => { win.checks++; await gate; return { ok: true, json: async () => win.auth }; };
  const fired = fire(win);
  stopA();
  const stopB = bus.subscribe(() => {});
  assert.equal(win.sockets.length, 2);
  release();
  await fired;
  assert.equal(win.sockets.length, 2);
  stopB();
});
test('a constructor failure after the last listener detaches schedules nothing', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win);
  let detach = false, stop;
  stop = bus.subscribe(() => { if (detach) stop(); });
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  win.WebSocket = class { constructor() { throw Error('blocked'); } };
  detach = true;
  await fire(win); // the check passes, the constructor throws, notify detaches the last listener
  assert.equal(win.timers.size, 0);
  assert.equal(win.sockets.length, 1);
});
test('reconnect clears the previous retry timer', async () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  const first = [...win.timers.keys()][0];
  win.fetch = async () => { win.checks++; throw Error('network down'); };
  await fire(win);
  assert.ok(win.cleared.includes(first));
  assert.equal(win.timers.size, 1);
  stop();
});