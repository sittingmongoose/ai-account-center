import test from 'node:test';
import assert from 'node:assert/strict';
import {
  FRAME_PAGES, READY_TIMEOUT_MS, isFramePage, parseRect, parseCover, frameClip, frameMessage, effectiveTheme, isAppPath,
  createShellFrames,
} from '../public/shell-frame.mjs';

test('the two framed pages and their embed URLs', () => {
  assert.deepEqual(Object.keys(FRAME_PAGES), ['status', 'scoreboard']);
  assert.equal(FRAME_PAGES.status.src, '/app/status?embed=1');
  assert.equal(FRAME_PAGES.scoreboard.src, '/app/scoreboard?embed=1');
  assert.equal(isFramePage('status'), true);
  assert.equal(isFramePage('scoreboard'), true);
  for (const page of ['home', 'analytics', 'accounts', 'toString', '', null, undefined]) assert.equal(isFramePage(page), false);
});

test('content-rect values become whole-pixel boxes; anything malformed is refused', () => {
  assert.deepEqual(parseRect('0,62,1440,838'), { left: 0, top: 62, width: 1440, height: 838 });
  // fractional edges round as edges, so the box meets the header and the tab bar exactly
  assert.deepEqual(parseRect('0.4,59.5,402,688.25'), { left: 0, top: 60, width: 402, height: 688 });
  assert.deepEqual(parseRect('47,0,780,0'), { left: 47, top: 0, width: 780, height: 0 });
  assert.deepEqual(parseRect('-0.5,10,20,30'), { left: -0, top: 10, width: 20, height: 30 });
  for (const bad of ['', '1,2,3', '1,2,3,4,5', '1,2,-3,4', '1,2,3,-4', 'a,b,c,d', '1,2,3,1e9', '1,2,3,Infinity',
    '1,2,3,NaN', '0x10,2,3,4', '1,,3,4', '200000,0,1,1', null, undefined, 42, {}]) {
    assert.equal(parseRect(bad), null, String(bad));
  }
  assert.equal(parseRect('1,'.repeat(150) + '1'), null);
});

test('content-cover: all, nothing, or holes', () => {
  assert.deepEqual(parseCover('all'), { all: true, holes: [] });
  assert.deepEqual(parseCover(''), { all: false, holes: [] });
  assert.deepEqual(parseCover(undefined), { all: false, holes: [] });
  assert.deepEqual(parseCover('1036,744,412,140'), { all: false, holes: [{ left: 1036, top: 744, width: 412, height: 140 }] });
  assert.deepEqual(parseCover('10,10,5,5;junk;20,20,0,4;30,30,2,2').holes, [
    { left: 10, top: 10, width: 5, height: 5 }, { left: 30, top: 30, width: 2, height: 2 },
  ]);
  assert.equal(parseCover(Array(12).fill('1,1,1,1').join(';')).holes.length, 8);
  assert.deepEqual(parseCover('ALL'), { all: false, holes: [] });
});

test('holes are cut out of the frame in its own coordinates', () => {
  const rect = { left: 0, top: 62, width: 1440, height: 838 };
  assert.equal(frameClip(rect, []), '');
  // a hole outside the frame (over the header) cuts nothing
  assert.equal(frameClip(rect, [{ left: 10, top: 0, width: 50, height: 40 }]), '');
  assert.equal(
    frameClip(rect, [{ left: 1036, top: 744, width: 412, height: 200 }]),
    'path(evenodd, "M0 0H1440V838H0Z M1036 682H1440V838H1036Z")'
  );
  // a hover card hanging off the header is clamped to the frame's top edge
  assert.equal(
    frameClip(rect, [{ left: 900, top: 40, width: 200, height: 60 }]),
    'path(evenodd, "M0 0H1440V838H0Z M900 0H1100V38H900Z")'
  );
  // overlapping holes merge into one box (evenodd would show their overlap again)
  assert.equal(
    frameClip(rect, [{ left: 100, top: 100, width: 100, height: 100 }, { left: 150, top: 150, width: 100, height: 100 }]),
    'path(evenodd, "M0 0H1440V838H0Z M100 38H250V188H100Z")'
  );
  assert.equal(frameClip(null, [{ left: 0, top: 0, width: 1, height: 1 }]), '');
  assert.equal(frameClip({ left: 0, top: 0, width: 0, height: 10 }, [{ left: 0, top: 0, width: 1, height: 1 }]), '');
});

test('only the two page messages are understood', () => {
  assert.deepEqual(frameMessage({ type: 'aac-ready' }), { type: 'aac-ready' });
  assert.deepEqual(frameMessage({ type: 'aac-ready', extra: 1 }), { type: 'aac-ready' });
  for (const page of ['home', 'analytics', 'accounts', 'status', 'scoreboard']) {
    assert.deepEqual(frameMessage({ type: 'aac-navigate', page }), { type: 'aac-navigate', page });
  }
  for (const bad of [null, undefined, 'aac-ready', ['aac-ready'], {}, { type: 'aac-theme', mode: 'dark' },
    { type: 'aac-navigate' }, { type: 'aac-navigate', page: 'settings' }, { type: 'aac-navigate', page: '/accounts' },
    { type: 'AAC-READY' }, { type: 'aac-navigate', page: { toString: () => 'home' } }]) {
    assert.equal(frameMessage(bad), null, JSON.stringify(bad));
  }
});

test('the effective theme follows the mode, Auto follows the system', () => {
  assert.equal(effectiveTheme('dark', false), 'dark');
  assert.equal(effectiveTheme('light', true), 'light');
  assert.equal(effectiveTheme('auto', true), 'dark');
  assert.equal(effectiveTheme('auto', false), 'light');
  assert.equal(effectiveTheme(undefined, true), 'dark');
});

test('only the Leptos pages count as a loaded frame', () => {
  assert.equal(isAppPath('/app/status'), true);
  assert.equal(isAppPath('/app/scoreboard/'), true);
  for (const bad of ['/', '/status', '/app/', '/app/other', 'about:blank', null]) assert.equal(isAppPath(bad), false);
});

// ---------------------------------------------------------------- the controller against a fake DOM

function fakeDom() {
  const ORIGIN = 'https://aac.test';
  const listeners = new Map();
  const win = {
    addEventListener: (type, fn) => listeners.set(type, fn),
    setTimeout: () => 0,
    clearTimeout: () => {},
  };
  const posted = [];
  const body = { children: [], appendChild(el) { this.children.push(el); el.parent = this; } };
  function element(tag) {
    const handlers = {};
    const el = {
      tag, style: {}, attrs: {}, children: [], textContent: '', id: '', className: '', type: '',
      setAttribute(k, v) { this.attrs[k] = v; },
      addEventListener(type, fn) { handlers[type] = fn; },
      fire(type) { handlers[type]?.(); },
      append(...kids) { this.children.push(...kids); },
      remove() { body.children = body.children.filter((c) => c !== this); this.removed = true; },
      querySelector(sel) {
        if (sel === 'button') return this.children.find((c) => c.tag === 'button');
        if (sel === '.aac-frame-note-text') return this.children.find((c) => c.className === 'aac-frame-note-text');
        return null;
      },
    };
    if (tag === 'iframe') {
      el.contentWindow = { location: { pathname: '/app/status' }, postMessage: (msg, origin) => posted.push({ el, msg, origin }) };
    }
    return el;
  }
  const doc = { body, createElement: element };
  const timers = { pending: new Map(), next: 1 };
  const fakeTimers = {
    setTimeout(fn, ms) { const id = timers.next++; timers.pending.set(id, { fn, ms }); return id; },
    clearTimeout(id) { timers.pending.delete(id); },
    fireAll() { const all = [...timers.pending.values()]; timers.pending.clear(); for (const t of all) t.fn(); },
  };
  const iframes = () => body.children.filter((c) => c.tag === 'iframe');
  const message = (data, source, origin = ORIGIN) => listeners.get('message')({ data, source, origin });
  return { ORIGIN, win, doc, posted, iframes, message, fakeTimers };
}

function setup() {
  const dom = fakeDom();
  const navigations = [];
  const frames = createShellFrames({
    doc: dom.doc, win: dom.win, origin: dom.ORIGIN, timers: dom.fakeTimers, onNavigate: (page) => navigations.push(page),
  });
  return { ...dom, frames, navigations };
}

test('an iframe is made on the first visit, shown once ready, kept while hidden and removed at sign-out', () => {
  const t = setup();
  t.frames.setRect('0,62,1440,838');
  t.frames.setPage('status');
  assert.equal(t.iframes().length, 0, 'nothing before sign-in');
  t.frames.setSignedIn(true);
  assert.equal(t.iframes().length, 1);
  const [status] = t.iframes();
  assert.equal(status.attrs.src, '/app/status?embed=1');
  assert.equal(status.attrs.title, 'Status');
  assert.equal(status.attrs.sandbox, undefined);
  assert.equal(status.attrs.allow, undefined);
  assert.equal(status.style.visibility, 'hidden', 'hidden until the page says it is ready');
  assert.deepEqual([status.style.left, status.style.top, status.style.width, status.style.height], ['0px', '62px', '1440px', '838px']);
  assert.equal(t.frames.snapshot().note, 'Loading Status…');
  // load: the theme goes in at once
  status.fire('load');
  assert.deepEqual(t.posted.at(-1).msg, { type: 'aac-theme', mode: 'light' });
  assert.equal(t.posted.at(-1).origin, t.ORIGIN);
  const sent = () => t.posted.filter((p) => p.el === status).map((p) => p.msg);
  const before = sent().length;
  t.message({ type: 'aac-ready' }, status.contentWindow);
  assert.equal(status.style.visibility, 'visible');
  assert.equal(t.frames.snapshot().note, null);
  // ready: the theme again, then whether it is the current tab
  assert.deepEqual(sent().slice(before), [{ type: 'aac-theme', mode: 'light' }, { type: 'aac-visible', visible: true }]);
  assert.ok(t.posted.every((p) => p.origin === t.ORIGIN));
  // away and back: the same iframe, hidden then shown again, told each time and only on a change
  t.frames.setPage('home');
  assert.equal(status.style.visibility, 'hidden');
  assert.deepEqual(sent().at(-1), { type: 'aac-visible', visible: false });
  const away = sent().length;
  t.frames.setRect('0,62,1440,800');
  assert.equal(sent().length, away, 'a resize while away says nothing new');
  t.frames.setPage('scoreboard');
  assert.equal(t.iframes().length, 2);
  assert.equal(sent().length, away, 'another tab opening says nothing new to this one');
  t.frames.setPage('status');
  assert.equal(t.iframes()[0], status);
  assert.equal(status.style.visibility, 'visible');
  assert.deepEqual(sent().slice(away), [{ type: 'aac-visible', visible: true }]);
  // a reload of the page (a second ready) is told again
  t.message({ type: 'aac-ready' }, status.contentWindow);
  assert.deepEqual(sent().slice(-2), [{ type: 'aac-theme', mode: 'light' }, { type: 'aac-visible', visible: true }]);
  // sign-out removes both
  t.frames.setSignedIn(false);
  assert.equal(t.iframes().length, 0);
  assert.equal(status.removed, true);
});

test('a ready page that reloads hides until ready again, and one sent outside /app fails', () => {
  const t = setup();
  t.frames.setSignedIn(true);
  t.frames.setRect('0,62,1440,838');
  t.frames.setPage('status');
  const [frame] = t.iframes();
  frame.fire('load');
  t.message({ type: 'aac-ready' }, frame.contentWindow);
  assert.equal(frame.style.visibility, 'visible');
  // a reload inside /app: hidden and loading until it says ready, then told it is the current tab again
  frame.fire('load');
  assert.equal(frame.style.visibility, 'hidden');
  assert.equal(t.frames.snapshot().frames[0].state, 'loading');
  assert.equal(t.frames.snapshot().note, 'Loading Status…');
  t.message({ type: 'aac-ready' }, frame.contentWindow);
  assert.equal(frame.style.visibility, 'visible');
  assert.deepEqual(t.posted.at(-1).msg, { type: 'aac-visible', visible: true });
  // the same page later lands on the shell (a sign-in redirect): unavailable, not left on screen
  frame.contentWindow.location.pathname = '/';
  frame.fire('load');
  assert.equal(t.frames.snapshot().frames[0].state, 'failed');
  assert.equal(frame.style.visibility, 'hidden');
  assert.equal(t.frames.snapshot().note, 'Status page unavailable');
  // a reload that never says ready times out
  t.frames.retry();
  const [again] = t.iframes();
  again.fire('load');
  t.message({ type: 'aac-ready' }, again.contentWindow);
  again.fire('load');
  t.fakeTimers.fireAll();
  assert.equal(t.frames.snapshot().frames[0].state, 'failed');
});

test('the rect follows the shell and covers hide or cut the frame', () => {
  const t = setup();
  t.frames.setSignedIn(true);
  t.frames.setRect('0,60,402,688');
  t.frames.setPage('scoreboard');
  const [frame] = t.iframes();
  frame.contentWindow.location.pathname = '/app/scoreboard';
  frame.fire('load');
  t.message({ type: 'aac-ready' }, frame.contentWindow);
  assert.equal(frame.style.visibility, 'visible');
  // a malformed rect is ignored, a rotation moves the frame
  t.frames.setRect('nope');
  assert.equal(frame.style.height, '688px');
  t.frames.setRect('47,48,780,354');
  assert.deepEqual([frame.style.left, frame.style.top, frame.style.width, frame.style.height], ['47px', '48px', '780px', '354px']);
  // a toast cuts a hole; a menu hides the frame so the click outside reaches the canvas
  t.frames.setCover('600,300,227,80');
  assert.equal(frame.style.clipPath, 'path(evenodd, "M0 0H780V354H0Z M553 252H780V332H553Z")');
  t.frames.setCover('all');
  assert.equal(frame.style.visibility, 'hidden');
  assert.equal(t.frames.snapshot().note, null, 'the note steps aside for an overlay too');
  t.frames.setCover('');
  assert.equal(frame.style.visibility, 'visible');
  assert.equal(frame.style.clipPath, 'none');
  // an empty content area (keyboard over everything) shows nothing
  t.frames.setRect('0,48,402,0');
  assert.equal(frame.style.visibility, 'hidden');
});

test('a page that never says ready, or lands outside /app, is replaced by the unavailable note', () => {
  const t = setup();
  t.frames.setSignedIn(true);
  t.frames.setRect('0,62,1440,838');
  t.frames.setPage('status');
  let [frame] = t.iframes();
  // a redirect to the shell (or any other page) fails at once
  frame.contentWindow.location.pathname = '/';
  frame.fire('load');
  assert.equal(t.frames.snapshot().frames[0].state, 'failed');
  assert.equal(t.frames.snapshot().note, 'Status page unavailable');
  assert.equal(frame.style.visibility, 'hidden');
  // a late ready from the failed page does not show it
  t.message({ type: 'aac-ready' }, frame.contentWindow);
  assert.equal(frame.style.visibility, 'hidden');
  // Try again makes a new iframe
  t.frames.retry();
  assert.equal(t.iframes().length, 1);
  assert.notEqual(t.iframes()[0], frame);
  assert.equal(t.frames.snapshot().note, 'Loading Status…');
  // no ready within the timeout: unavailable
  t.fakeTimers.fireAll();
  assert.equal(t.frames.snapshot().note, 'Status page unavailable');
  // opening the page again retries too
  frame = t.iframes()[0];
  t.frames.setPage('home');
  assert.equal(t.frames.snapshot().note, null);
  t.frames.setPage('status');
  assert.notEqual(t.iframes()[0], frame);
  // an error event fails it
  t.iframes()[0].fire('error');
  assert.equal(t.frames.snapshot().note, 'Status page unavailable');
  assert.ok(READY_TIMEOUT_MS >= 10_000);
});

test('messages: same origin, our own iframes, the two known types only', () => {
  const t = setup();
  t.frames.setSignedIn(true);
  t.frames.setRect('0,62,1440,838');
  t.frames.setPage('status');
  const [frame] = t.iframes();
  const stranger = { postMessage() {} };
  t.message({ type: 'aac-ready' }, frame.contentWindow, 'https://evil.test');
  t.message({ type: 'aac-ready' }, stranger);
  t.message({ type: 'aac-ready' }, null);
  assert.equal(frame.style.visibility, 'hidden', 'a foreign origin or window cannot mark it ready');
  t.message({ type: 'aac-navigate', page: 'analytics' }, stranger);
  t.message({ type: 'aac-navigate', page: 'analytics' }, frame.contentWindow, 'https://evil.test');
  t.message({ type: 'aac-navigate', page: 'nowhere' }, frame.contentWindow);
  assert.deepEqual(t.navigations, []);
  t.message({ type: 'aac-navigate', page: 'analytics' }, frame.contentWindow);
  assert.deepEqual(t.navigations, ['analytics']);
  // the theme follows the shell
  const before = t.posted.length;
  t.frames.setTheme('dark');
  assert.deepEqual(t.posted.at(-1).msg, { type: 'aac-theme', mode: 'dark' });
  t.frames.setTheme('dark');
  assert.equal(t.posted.length, before + 1, 'an unchanged theme is not sent again');
  t.frames.setTheme('sepia');
  assert.deepEqual(t.posted.at(-1).msg, { type: 'aac-theme', mode: 'light' });
});
