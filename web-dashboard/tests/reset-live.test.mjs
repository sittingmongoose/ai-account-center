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
    win.setTimeout = (fn, ms) => { const id = Symbol(); win.timers.set(id, fn); win.delays.push(ms); return id; };
    win.clearTimeout = (id) => win.timers.delete(id);
    win.sockets = [];
    win.WebSocket = class {
      constructor(url) { this.url = url; win.sockets.push(this); }
      close() { this.closed = true; }
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
test('focus, visibility, reconnect and online invalidate before another server read', () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  for (const type of ['focus', 'online', 'aac-shown']) win.dispatchEvent(new Event(type));
  win.document.dispatchEvent(new Event('visibilitychange'));
  assert.equal(fences, 4);
  win.document.visibilityState = 'hidden'; win.dispatchEvent(new Event('focus'));
  assert.equal(fences, 4);
  win.sockets[0].onopen(); win.sockets[0].onclose(); assert.equal(fences, 6);
  assert.equal(win.timers.size, 1);
  for (const fn of win.timers.values()) fn();
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

test('a signed-out close stops the reconnect: the page\'s signed-out handling takes over', () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  win.sockets[0].onopen();
  win.sockets[0].onclose({ code: 4001, reason: 'Signed out', wasClean: true });
  assert.equal(fences, 2);
  assert.equal(win.timers.size, 0);
  for (const type of ['focus', 'online']) win.dispatchEvent(new Event(type));
  assert.equal(win.sockets.length, 1);
  stop();
});
test('an upgrade refused before any live session stops the reconnect', () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  assert.equal(win.timers.size, 0);
  win.dispatchEvent(new Event('online'));
  assert.equal(win.sockets.length, 1);
  stop();
});
test('a live drop backs off instead of retrying every second', () => {
  const { browser } = browserPair(); const win = browser();
  const bus = createResetHints(win); const stop = bus.subscribe(() => {});
  win.sockets[0].onopen();
  const drop = () => {
    win.sockets.at(-1).onclose({ code: 1006, wasClean: false });
    assert.equal(win.timers.size, 1);
    for (const [id, fn] of win.timers) { win.timers.delete(id); fn(); }
  };
  for (let i = 0; i < 7; i++) drop();
  assert.deepEqual(win.delays, [1000, 2000, 4000, 8000, 16000, 30000, 30000]);
  assert.equal(win.sockets.length, 8);
  win.sockets.at(-1).onopen();
  win.sockets.at(-1).onclose({ code: 1006, wasClean: false });
  assert.equal(win.delays.at(-1), 1000);
  stop();
});
test('a dropped connection on a live session still reconnects', () => {
  const { browser } = browserPair(); const win = browser(); let fences = 0;
  const bus = createResetHints(win); const stop = bus.subscribe(() => fences++);
  win.sockets[0].onopen();
  win.sockets[0].onmessage({ data: JSON.stringify({ type: 'reset-claims-changed', id: 'live-1' }) });
  assert.equal(fences, 2);
  win.sockets[0].onclose({ code: 1006, wasClean: false });
  assert.equal(win.timers.size, 1);
  for (const [id, fn] of win.timers) { win.timers.delete(id); fn(); }
  assert.equal(win.sockets.length, 2);
  // The first retry misses (the server is still down): a live session keeps trying, with back-off.
  win.sockets[1].onclose({ code: 1006, wasClean: false });
  assert.equal(win.delays.at(-1), 2000);
  for (const [id, fn] of win.timers) { win.timers.delete(id); fn(); }
  assert.equal(win.sockets.length, 3);
  win.sockets[2].onopen();
  win.sockets[2].onmessage({ data: JSON.stringify({ type: 'reset-claims-changed', id: 'live-2' }) });
  assert.equal(fences, 6);
  stop();
});
