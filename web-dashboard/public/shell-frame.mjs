// The Status and Scoreboard tabs: Leptos pages (/app/status, /app/scoreboard) shown in an iframe over the Slint
// canvas, the way the sign-in layer lays real HTML inputs over it (login-bridge.mjs). docs/status-tab.md, "Shell
// iframe", is the contract.
//
// - One iframe per page, created on the first visit and kept while hidden, so returning is instant. It is shown
//   only while its page is current, the user is signed in, no Slint overlay covers the content area and the page
//   has said `aac-ready`. Signing out removes them.
// - Geometry: the shell reports the content area as the `content-rect` action ("x,y,w,h" in CSS px) and what the
//   canvas draws over it as `content-cover` ("all", "" or "x,y,w,h" rects joined by ";"). Rects are cut out of
//   the iframe with clip-path, so toasts and hover cards stay visible and clickable.
// - Messages (same origin, from our own iframes only): the page sends { type: "aac-ready" } and
//   { type: "aac-navigate", page }; the shell sends { type: "aac-visible", visible } when the page becomes or stops
//   being the current tab, and { type: "aac-theme", mode: "light" | "dark" } on load, on
//   ready and whenever the effective theme changes. Anything else is ignored.
// - A page that does not load, lands anywhere but /app/, or never says aac-ready within READY_TIMEOUT_MS is
//   replaced by the shell's own "Status page unavailable" note with Try again; the area is never blank.
import { PAGES } from './page-route.mjs';

export const FRAME_PAGES = Object.freeze({
  status: Object.freeze({ src: '/app/status?embed=1', title: 'Status' }),
  scoreboard: Object.freeze({ src: '/app/scoreboard?embed=1', title: 'Scoreboard' }),
});
/** How long a new iframe may take to say aac-ready before the shell calls it unavailable. */
export const READY_TIMEOUT_MS = 20_000;
const MAX_COORD = 100_000;
const MAX_HOLES = 8;

/** Whether a page is shown in the shell's iframe. */
export function isFramePage(page) {
  return typeof page === 'string' && Object.hasOwn(FRAME_PAGES, page);
}

/**
 * One "x,y,w,h" rect (CSS px, floats allowed) as whole-pixel edges: { left, top, width, height }, or null when it
 * is malformed, not finite, negative in size or absurdly large. A zero-size rect is valid (nothing to show).
 */
export function parseRect(value) {
  if (typeof value !== 'string' || value.length > 200) return null;
  const parts = value.split(',');
  if (parts.length !== 4) return null;
  const nums = parts.map((part) => (/^\s*-?\d+(?:\.\d+)?\s*$/.test(part) ? Number(part) : NaN));
  if (!nums.every((n) => Number.isFinite(n) && Math.abs(n) <= MAX_COORD)) return null;
  const [x, y, w, h] = nums;
  if (w < 0 || h < 0) return null;
  const left = Math.round(x);
  const top = Math.round(y);
  return { left, top, width: Math.max(0, Math.round(x + w) - left), height: Math.max(0, Math.round(y + h) - top) };
}

/** The `content-cover` value: { all } for "all", else the valid rects (at most MAX_HOLES) as holes. */
export function parseCover(value) {
  if (value === 'all') return { all: true, holes: [] };
  if (typeof value !== 'string' || value === '' || value.length > 2000) return { all: false, holes: [] };
  const holes = value.split(';').map(parseRect).filter((rect) => rect && rect.width > 0 && rect.height > 0);
  return { all: false, holes: holes.slice(0, MAX_HOLES) };
}

/**
 * The clip-path that cuts the holes (window coordinates) out of a frame at `rect`: '' when no hole touches it.
 * Holes are clamped to the frame; overlapping holes are rare (a toast and a hover card) and evenodd would show
 * their overlap again, so a hole that overlaps an earlier one is merged into their bounding box.
 */
export function frameClip(rect, holes) {
  if (!rect || rect.width <= 0 || rect.height <= 0 || !Array.isArray(holes)) return '';
  const boxes = [];
  for (const hole of holes) {
    if (!hole) continue;
    let box = {
      l: Math.max(0, hole.left - rect.left),
      t: Math.max(0, hole.top - rect.top),
      r: Math.min(rect.width, hole.left + hole.width - rect.left),
      b: Math.min(rect.height, hole.top + hole.height - rect.top),
    };
    if (box.r <= box.l || box.b <= box.t) continue;
    for (let i = boxes.length - 1; i >= 0; i--) {
      const other = boxes[i];
      if (other.l < box.r && box.l < other.r && other.t < box.b && box.t < other.b) {
        box = { l: Math.min(box.l, other.l), t: Math.min(box.t, other.t), r: Math.max(box.r, other.r), b: Math.max(box.b, other.b) };
        boxes.splice(i, 1);
        i = boxes.length;
      }
    }
    boxes.push(box);
  }
  if (boxes.length === 0) return '';
  const cut = boxes.map((b) => `M${b.l} ${b.t}H${b.r}V${b.b}H${b.l}Z`).join(' ');
  return `path(evenodd, "M0 0H${rect.width}V${rect.height}H0Z ${cut}")`;
}

/** What an iframe sent, validated: { type: 'aac-ready' }, { type: 'aac-navigate', page } or null. */
export function frameMessage(data) {
  if (!data || typeof data !== 'object' || Array.isArray(data)) return null;
  if (data.type === 'aac-ready') return { type: 'aac-ready' };
  if (data.type === 'aac-navigate' && typeof data.page === 'string' && PAGES.includes(data.page)) {
    return { type: 'aac-navigate', page: data.page };
  }
  return null;
}

/** The effective theme the pages draw in: the stored mode, Auto following the system. */
export function effectiveTheme(mode, systemDark) {
  return mode === 'dark' || (mode !== 'light' && systemDark === true) ? 'dark' : 'light';
}

/** Whether the iframe's document is one of the Leptos pages (a redirect to the shell or anywhere else is not). */
export function isAppPath(pathname) {
  return typeof pathname === 'string' && /^\/app\/(status|scoreboard)\/?$/.test(pathname);
}

const NOTE_INK = { light: '#63717F', dark: '#8794A2' };
const NOTE_RULE = { light: 'rgba(21,32,43,.18)', dark: 'rgba(255,255,255,.18)' };

/**
 * The iframes and the unavailable note. `doc` and `win` are the document and window (injected for tests);
 * `onNavigate(page)` follows an `aac-navigate` from a page; `timers` defaults to the window's.
 */
export function createShellFrames({ doc, win, origin, onNavigate, timers = null, readyTimeoutMs = READY_TIMEOUT_MS }) {
  const setT = timers?.setTimeout ?? ((fn, ms) => win.setTimeout(fn, ms));
  const clearT = timers?.clearTimeout ?? ((id) => win.clearTimeout(id));
  const frames = new Map(); // page -> { iframe, state: 'loading' | 'ready' | 'failed', timer }
  let page = 'home';
  let signedIn = false;
  let rect = null;
  let cover = { all: false, holes: [] };
  let theme = 'light';
  let note = null;

  function postTheme(entry) {
    try { entry.iframe.contentWindow?.postMessage({ type: 'aac-theme', mode: theme }, origin); } catch {}
  }
  // Whether the page is the current tab. A hidden iframe's document still reports itself visible, so the shell says
  // so: a page that isn't on screen pauses its reads (and the server's collectors idle). A menu covering the frame
  // for a moment doesn't count as hiding it.
  function postActive(entry, active) {
    if (entry.active === active) return;
    entry.active = active;
    try { entry.iframe.contentWindow?.postMessage({ type: 'aac-visible', visible: active }, origin); } catch {}
  }
  function fail(name) {
    const entry = frames.get(name);
    if (!entry || entry.state === 'ready') return;
    clearT(entry.timer);
    entry.state = 'failed';
    update();
  }
  function create(name) {
    const spec = FRAME_PAGES[name];
    const iframe = doc.createElement('iframe');
    iframe.className = 'aac-frame';
    iframe.setAttribute('title', spec.title);
    iframe.setAttribute('src', spec.src);
    // same origin, no sandbox (the page fetches /api with the session cookie), nothing extra allowed
    iframe.style.cssText = 'position:fixed;left:0;top:0;width:0;height:0;border:0;margin:0;padding:0;z-index:1;'
      + 'background:transparent;visibility:hidden;';
    const entry = { iframe, state: 'loading', timer: 0 };
    entry.timer = setT(() => fail(name), readyTimeoutMs);
    iframe.addEventListener('load', () => {
      if (frames.get(name) !== entry) return;
      let path = null;
      try { path = iframe.contentWindow?.location?.pathname ?? null; } catch { path = null; }
      if (!isAppPath(path)) { fail(name); return; }
      postTheme(entry);
    });
    iframe.addEventListener('error', () => { if (frames.get(name) === entry) fail(name); });
    frames.set(name, entry);
    doc.body.appendChild(iframe);
    return entry;
  }
  function drop(name) {
    const entry = frames.get(name);
    if (!entry) return;
    clearT(entry.timer);
    frames.delete(name);
    try { entry.iframe.remove(); } catch {}
  }
  function ensureNote() {
    if (note) return note;
    note = doc.createElement('div');
    note.id = 'aac-frame-note';
    note.setAttribute('role', 'status');
    note.style.cssText = 'position:fixed;display:none;z-index:1;box-sizing:border-box;flex-direction:column;'
      + 'align-items:center;justify-content:center;gap:12px;padding:16px;text-align:center;'
      + 'font:500 15px/1.4 system-ui,sans-serif;';
    const text = doc.createElement('span');
    text.className = 'aac-frame-note-text';
    const button = doc.createElement('button');
    button.type = 'button';
    button.textContent = 'Try again';
    button.style.cssText = 'font:500 14px system-ui,sans-serif;padding:8px 16px;min-height:44px;border-radius:10px;'
      + 'background:transparent;cursor:pointer;';
    button.addEventListener('click', () => retry());
    note.append(text, button);
    doc.body.appendChild(note);
    return note;
  }
  function placeNote(show, entry) {
    if (!show && !note) return;
    const el = ensureNote();
    if (!show) { el.style.display = 'none'; return; }
    const failed = entry?.state === 'failed';
    const label = FRAME_PAGES[page].title;
    el.querySelector('.aac-frame-note-text').textContent = failed ? `${label} page unavailable` : `Loading ${label}…`;
    const button = el.querySelector('button');
    button.style.display = failed ? '' : 'none';
    button.style.color = NOTE_INK[theme];
    button.style.border = `1px solid ${NOTE_RULE[theme]}`;
    Object.assign(el.style, {
      display: 'flex', left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px`,
      color: NOTE_INK[theme], clipPath: frameClip(rect, cover.holes) || 'none',
    });
  }
  function update() {
    const area = signedIn && isFramePage(page) && !cover.all && rect && rect.width > 0 && rect.height > 0;
    if (signedIn && isFramePage(page) && !frames.has(page)) create(page);
    for (const [name, entry] of frames) {
      const shown = area && name === page && entry.state === 'ready';
      const style = entry.iframe.style;
      if (rect) {
        style.left = `${rect.left}px`;
        style.top = `${rect.top}px`;
        style.width = `${rect.width}px`;
        style.height = `${rect.height}px`;
      }
      style.clipPath = shown ? frameClip(rect, cover.holes) || 'none' : 'none';
      style.visibility = shown ? 'visible' : 'hidden';
      if (entry.state === 'ready') postActive(entry, signedIn && name === page);
    }
    const current = frames.get(page);
    placeNote(!!area && !!current && current.state !== 'ready', current);
  }
  function retry() {
    if (!signedIn || !isFramePage(page)) return;
    const entry = frames.get(page);
    if (entry && entry.state !== 'failed') return;
    drop(page);
    update();
  }
  function onMessage(event) {
    if (!event || event.origin !== origin) return;
    let name = null;
    for (const [key, entry] of frames) {
      try { if (entry.iframe.contentWindow === event.source && event.source) name = key; } catch {}
    }
    if (!name) return;
    const message = frameMessage(event.data);
    if (!message) return;
    const entry = frames.get(name);
    if (message.type === 'aac-ready') {
      // a page already called unavailable stays so until Try again makes a new one
      if (entry.state === 'failed') return;
      clearT(entry.timer);
      entry.state = 'ready';
      entry.active = undefined; // a reloaded page starts unaware; tell it again
      postTheme(entry);
      update();
      return;
    }
    if (message.type === 'aac-navigate' && signedIn) onNavigate?.(message.page);
  }
  win.addEventListener('message', onMessage);

  return {
    setRect(value) { const next = parseRect(value); if (next) { rect = next; update(); } },
    setCover(value) { cover = parseCover(value); update(); },
    setPage(next) {
      page = typeof next === 'string' ? next : 'home';
      // a page that failed gets a fresh try each time it is opened again
      if (signedIn && frames.get(page)?.state === 'failed') drop(page);
      update();
    },
    setSignedIn(value) {
      signedIn = value === true;
      if (!signedIn) for (const name of [...frames.keys()]) drop(name);
      update();
    },
    setTheme(mode) {
      const next = mode === 'dark' ? 'dark' : 'light';
      if (next === theme) return;
      theme = next;
      for (const entry of frames.values()) postTheme(entry);
      update();
    },
    retry,
    /** For tests and the ?e2e harness: each frame's state and box. */
    snapshot() {
      return {
        page, signedIn, rect, cover, theme,
        frames: [...frames].map(([name, entry]) => ({ name, state: entry.state, visibility: entry.iframe.style.visibility })),
        note: note && note.style.display !== 'none' ? note.querySelector('.aac-frame-note-text').textContent : null,
      };
    },
  };
}
