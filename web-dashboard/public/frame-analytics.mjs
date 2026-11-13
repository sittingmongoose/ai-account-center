// The app frame's Analytics: the frame reads /api/accounts/analytics itself, as Status reads /api/status, and builds
// the page's view with the same builders bridge.js uses (analytics-usage.mjs, analytics-quota.mjs, analytics-data.mjs,
// ANALYTICS_VIEW_VERSION 7; the page rules in analytics-page.mjs). The tab never loads the Slint dashboard or waits on
// its bridge. The page (web-dashboard/leptos/src/analytics/) draws the views and sends its action kinds here.
// Pure apart from the injected fetch, clock, timers and sinks (tests/frame-analytics.test.mjs).
//
// What it sends, through `emit(key, json)`: `analytics` (the view; `analytics-motion` "morph" or "static" just before
// it), `analytics-head` (the header between refreshes), `analytics-loading` ({loading, error}) and `chrome`
// ({statusLead, statusStrong, statusMore}: the frame header's line, worded as bridge.js words it). `signedOut(reason)`
// says a read found the session gone. Until the first answer nothing is sent but `analytics-loading`: the page shows
// its loading state, never zeros.
import { analyticsSlintModel } from './analytics-data.mjs';
import { usageView, usageHead, apiRangeFor, trendPaths } from './analytics-usage.mjs';
import { quotaView, agendaView } from './analytics-quota.mjs';
import {
  createAnalyticsPage,
  analyticsThresholds,
  analyticsCovers,
  analyticsPollDelay,
  applyAnalyticsAction,
  applyAnalyticsLayout,
} from './analytics-page.mjs';
import { chromeView } from './view-model.mjs';
import { validAntigravityAuto } from './antigravity-data.mjs';
import { setDisplayTimeZone } from './time-format.mjs';
import { rememberSeen } from './auth-view.mjs';

/** The header and the chrome line are worded again this often while the page is shown (bridge.js does the same). */
export const HEAD_EVERY_MS = 15_000;
/** The dashboard's refresh interval until its settings say otherwise. */
export const DEFAULT_INTERVAL_SECONDS = 60;
/** A burst of dashboard changes elsewhere in the frame is read once, this long after the last. */
export const SIDE_SETTLE_MS = 250;

/**
 * The frame's Analytics controller. `fetch`, `now()`, `setTimeout`/`clearTimeout`, `visible()` (the document is on
 * screen), `reducedMotion()` and `storage` (localStorage or null) are injected; `platform` is 'mac' or 'windows' as
 * bridge.js picks it.
 */
export function createFrameAnalytics({
  fetch: fetchImpl,
  now = () => Date.now(),
  setTimeout: later = (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: cancel = (id) => globalThis.clearTimeout(id),
  platform = 'mac',
  visible = () => true,
  reducedMotion = () => false,
  emit = () => {},
  signedOut = () => {},
  onView = null,
  storage = null,
}) {
  const page = createAnalyticsPage();
  let payload = null;
  let dashboard = null;
  let antigravityAuto = null;
  let intervalSeconds = DEFAULT_INTERVAL_SECONDS;
  let shown = false;
  let stopped = false;
  let online = true;
  let reading = false;
  let ended = false;
  let generation = 0;
  let pollFails = 0;
  let lastGeo = null;
  let lastSize = '';
  // when the page first got an answer without numbers (a server that just started): its first polls come quickly
  let waitingSince = null;
  const timers = { poll: 0, layout: 0, tick: 0, interval: 0, side: 0 };
  const pushed = new Map();

  const clear = (name) => {
    if (timers[name]) cancel(timers[name]);
    timers[name] = 0;
  };
  const push = (key, json) => {
    if (stopped || pushed.get(key) === json) return false;
    pushed.set(key, json);
    emit(key, json);
    return true;
  };

  // The controller's lifetime (G5 post-landing fix, as frame-dashboard.mjs): it stays in the window after its page has
  // gone, so once stopped it sends no read (the browser would send it with the next session's cookie), aborts what is in
  // flight and acts on no late answer.
  const lifetime = typeof AbortController === 'function' ? new AbortController() : null;
  const dropped = () => Object.assign(new Error('This page has signed out.'), { dropped: true, status: 0, payload: null });

  /** One GET, as bridge.js's sendNow: the payload, or a thrown error with status and payload (`network` offline). */
  async function read(path) {
    if (stopped) throw dropped();
    let response;
    try {
      response = await fetchImpl(path, { credentials: 'same-origin', ...(lifetime ? { signal: lifetime.signal } : {}) });
    } catch (cause) {
      if (stopped) throw dropped();
      online = false;
      throw Object.assign(new Error('The dashboard did not answer.'), { network: true, status: 0, payload: null, cause });
    }
    if (stopped) throw dropped();
    online = true;
    const body = response.status === 204 ? null : await response.json().catch(() => null);
    if (stopped) throw dropped();
    if (!response.ok) {
      // a 401 about the session ends it (signed out elsewhere, a password change); the frame's sign-in says why
      const code = body?.code;
      if (response.status === 401 && !stopped && !ended && (!code || code === 'auth_required' || code === 'session_revoked')) {
        ended = true;
        signedOut(code === 'session_revoked' ? 'revoked' : undefined);
      }
      throw Object.assign(new Error(body?.error || `Request failed (${response.status}).`), { status: response.status, payload: body });
    }
    // the server accepted the session just now (bridge.js does the same): a later sign-out within its lifetime ended
    // early, and the frame's sign-in words it so (auth-view.mjs endedReason)
    rememberSeen(storage, now());
    return body;
  }

  /**
   * What the page reads beside the analytics: the dashboard (hidden accounts and providers, the switch thresholds, the
   * refresh interval and the chrome line), Antigravity's automatic switching (its threshold) and the display time zone.
   * A failed read keeps what was read before. `announce`: a real refresh (the refresh interval, Refresh), which the
   * header says ("Refreshing", as bridge.js says it); the first read and a return to the tab say nothing.
   */
  async function readSide(force, announce = false) {
    if (announce) {
      reading = true;
      renderChrome();
    }
    const [next, auto, prefs] = await Promise.all([
      read(`/api/accounts/dashboard?platform=${platform}&refresh=${force}`).catch(() => null),
      read('/api/antigravity/auto-switch').catch(() => null),
      read('/api/accounts/preferences').catch(() => null),
    ]);
    reading = false;
    if (stopped) return;
    if (next?.schemaVersion === 1 && Array.isArray(next.accounts)) {
      dashboard = next;
      const seconds = next.settings?.refreshIntervalSeconds;
      if (Number.isInteger(seconds) && seconds >= 30 && seconds <= 3600) intervalSeconds = seconds;
      antigravityAuto = validAntigravityAuto(auto) ? auto : validAntigravityAuto(next.antigravityAutoSwitch) ? next.antigravityAutoSwitch : null;
    }
    if (prefs && typeof prefs.timeZone === 'string') {
      try {
        setDisplayTimeZone(prefs.timeZone);
      } catch {}
    }
    renderChrome();
  }

  function renderChrome() {
    if (!dashboard && !reading) return;
    const chrome = chromeView(dashboard, { now: now(), refreshing: reading, intervalSeconds });
    // the connection dropped: the readings stay on screen and the status line says so
    if (!online && !reading && chrome.statusLead === 'Updated') chrome.statusLead = 'Offline';
    push('chrome', JSON.stringify({ statusLead: chrome.statusLead, statusStrong: chrome.statusStrong, statusMore: chrome.statusMore }));
  }

  function loading(isLoading, error) {
    push('analytics-loading', JSON.stringify({ loading: isLoading, error }));
  }

  /** mode 'morph' lets the page glide the usage trend from the shape on screen to the new one. */
  function render(mode = 'static') {
    if (!payload || stopped) return;
    const t = now();
    const usage = usageView(payload, page, { now: t, sizes: page.sizes });
    const ctx = {
      now: t,
      dashboard,
      open: page.open,
      compare: page.compare,
      collapsed: page.collapsed,
      focusWidth: page.sizes.focus?.w,
      focusHeight: page.sizes.focus?.h,
      thresholds: analyticsThresholds(dashboard, antigravityAuto),
    };
    // quota and agenda times read at minute precision, so they run on a minute clock (bridge.js renderAnalytics)
    const slow = { ...ctx, now: Math.floor(t / 60000) * 60000 };
    const quota = quotaView(payload, slow);
    const agenda = agendaView(payload, slow);
    const next = usage.trend.geo;
    const size = `${usage.trend.pw}x${usage.trend.ph}`;
    const morph = mode === 'morph' && !reducedMotion() && !!lastGeo && lastSize === size && lastGeo.lv[0].length === next.lv[0].length;
    const view = analyticsSlintModel(null, { usage, quota, agenda, state: page, paths: trendPaths(next) });
    if (onView) onView(view);
    const json = JSON.stringify(view);
    if (pushed.get('analytics') !== json) {
      push('analytics-motion', morph ? 'morph' : 'static');
      push('analytics', json);
    }
    lastGeo = next;
    lastSize = size;
    schedulePoll();
  }

  function renderHead() {
    if (!payload || stopped) return;
    try {
      push('analytics-head', JSON.stringify(usageHead(payload, page, { now: now() })));
    } catch {}
  }

  /** While the server is still reading, the page reads again (analytics-page.mjs analyticsPollDelay). */
  function schedulePoll() {
    clear('poll');
    if (payload?.activity?.totals) waitingSince = null;
    else if (waitingSince === null) waitingSince = now();
    const ms = analyticsPollDelay(payload, waitingSince === null ? Infinity : now() - waitingSince);
    if (!shown || ms <= 0) return;
    timers.poll = later(() => {
      timers.poll = 0;
      if (shown) void refresh(false, 'morph', { silent: true });
    }, ms);
  }

  /**
   * Reads the analytics (and beside it the dashboard). A refresh (`force`) asks every provider first, as bridge.js's
   * Refresh does, then reads the analytics afresh. `silent`: a background poll, which reads the analytics only (as
   * bridge.js's poll does) and keeps the header as it is; one failed poll keeps the page's words. `side: false` reads
   * the analytics only (a range change). `announce`: the header says "Refreshing" (the refresh interval, Refresh).
   */
  async function refresh(force = false, mode = 'static', { silent = false, side = !silent, announce = force } = {}) {
    if (stopped) return;
    const quiet = silent && !force;
    const gen = ++generation;
    if (!quiet) loading(true, '');
    const query = new URLSearchParams({ platform, range: apiRangeFor(page, now()), provider: 'all', account: 'all' });
    if (force) query.set('refresh', 'true');
    const analytics = () => read(`/api/accounts/analytics?${query}`).then((r) => ({ r }), (error) => ({ error }));
    let result;
    if (force) {
      await readSide(true, true);
      result = await analytics();
    } else if (side) {
      [result] = await Promise.all([analytics(), readSide(false, announce)]);
    } else {
      result = await analytics();
    }
    if (gen !== generation || stopped) return;
    armInterval();
    if (result.r && result.r.schemaVersion === 1 && Array.isArray(result.r.accounts)) {
      payload = result.r;
      render(mode);
      pollFails = 0;
      loading(false, '');
      return;
    }
    const error = result.error || new Error('Unsupported analytics response.');
    render();
    if (quiet && ++pollFails < 2) return;
    pollFails = 0;
    loading(false, error?.message || 'Unable to load account analytics.');
  }

  /**
   * The page comes on screen. A response that still covers the range and is younger than the refresh interval is drawn
   * as it is (the interval reads the next one); otherwise it is read now. Either way the dashboard beside it is read
   * again first (it is cheap), and the page is drawn once that read lands, so an account hidden or shown on another
   * tab meanwhile is never drawn the old way. Until then the page keeps the view it had.
   */
  function enter() {
    const age = now() - Date.parse(payload?.updatedAt ?? '');
    if (payload && Number.isFinite(age) && age >= 0 && age < intervalSeconds * 1000 && analyticsCovers(payload, page, now())) {
      const gen = generation;
      void readSide(false).then(() => {
        if (gen !== generation || !shown || stopped) return;
        render();
        renderChrome();
      });
      return;
    }
    void refresh();
  }

  /**
   * The dashboard changed elsewhere in the frame (bridge.js sent Home or Accounts a new view: an account hidden, shown
   * or removed). While this tab is away, the dashboard is read again (once for a burst) and the page is drawn with it,
   * so it is current before it comes back on screen. Nothing else is read.
   */
  function dashboardChanged() {
    if (stopped || shown || !payload) return;
    clear('side');
    timers.side = later(() => {
      timers.side = 0;
      const gen = generation;
      void readSide(false).then(() => {
        if (gen === generation && !shown && !stopped) render();
      });
    }, SIDE_SETTLE_MS);
  }

  async function changeRange() {
    const covered = !!payload && analyticsCovers(payload, page, now());
    if (covered) render('morph');
    if (!payload || payload.range?.preset !== apiRangeFor(page, now())) await refresh(false, covered ? 'static' : 'morph', { side: false });
  }

  function armTick() {
    clear('tick');
    if (!shown) return;
    timers.tick = later(() => {
      timers.tick = 0;
      renderChrome();
      renderHead();
      armTick();
    }, HEAD_EVERY_MS);
  }

  /** The dashboard's refresh interval: while the page is shown and the window is on screen, read again. */
  function armInterval() {
    clear('interval');
    if (!shown) return;
    timers.interval = later(() => {
      timers.interval = 0;
      if (shown && visible()) void refresh(false, 'morph', { announce: true });
      else armInterval();
    }, intervalSeconds * 1000);
  }

  return {
    /** The Analytics tab is on screen. */
    show() {
      if (stopped || shown) return;
      shown = true;
      enter();
      armTick();
      armInterval();
    },
    /** Another tab is on screen: no reads or timers until it comes back. */
    hide() {
      shown = false;
      for (const name of Object.keys(timers)) clear(name);
    },
    /** The window came back on screen (an app resumed): read now if what is shown is old. */
    resumed() {
      if (shown && !stopped) enter();
    },
    dashboardChanged,
    /** One of the page's action kinds; false for one that isn't the page's. */
    act(kind, value) {
      if (stopped) return false;
      const text = typeof value === 'string' ? value : '';
      if (kind === 'analytics-layout') {
        if (applyAnalyticsLayout(page, text)) {
          clear('layout');
          timers.layout = later(() => {
            timers.layout = 0;
            render('static');
          }, 30);
        }
        return true;
      }
      if (kind === 'analytics-refresh') {
        void refresh(true);
        return true;
      }
      const known = (id) => !!payload?.accounts?.some((a) => a?.id === id) || !!dashboard?.accounts?.some((a) => a?.id === id);
      const step = applyAnalyticsAction(page, kind, text, { known, now: now() });
      if (!step) return false;
      if (step.range) void changeRange();
      else if (step.render) render(step.render);
      return true;
    },
    /** Signed out (the page is gone): nothing more is read or sent. */
    stop() {
      stopped = true;
      lifetime?.abort();
      generation++;
      shown = false;
      for (const name of Object.keys(timers)) clear(name);
    },
    /** For tests and the kit: the page state and what is held. */
    state() {
      return { range: page.range, prov: page.prov, shown, stopped, hasPayload: !!payload, intervalSeconds };
    },
  };
}

let installs = 0;

/**
 * Starts the frame's Analytics: `window.aacFrameAnalytics` holds the calls (a previous one is stopped), and every view
 * is sent as an `aac-analytics` event whose detail is `{key, json}`; `signed-out` (json: the reason) says the session
 * ended. Each controller has its own `id` (from 1): the page that installed it drives and stops only that one
 * (leptos/src/analytics/feed.rs). With `?e2e`, `window.__aacLastAnalytics` keeps the last view (as bridge.js keeps it
 * for the kit).
 */
export function installFrameAnalytics(win = globalThis) {
  win.aacFrameAnalytics?.stop?.();
  const search = String(win.location?.search || '');
  const agent = String(win.navigator?.userAgent || '');
  const e2e = /[?&]e2e\b/.test(search);
  const headless = /HeadlessChrome/.test(agent) && !/[?&]motion\b/.test(search);
  let motion = null;
  try {
    motion = win.matchMedia('(prefers-reduced-motion: reduce)');
  } catch {}
  const send = (key, json) => {
    try {
      win.dispatchEvent(new win.CustomEvent('aac-analytics', { detail: { key, json } }));
    } catch {}
  };
  const controller = createFrameAnalytics({
    fetch: (...args) => win.fetch(...args),
    storage: (() => {
      try {
        return win.localStorage;
      } catch {
        return null;
      }
    })(),
    platform: /Windows/i.test(agent) ? 'windows' : 'mac',
    visible: () => win.document?.visibilityState !== 'hidden',
    reducedMotion: () => headless || !!motion?.matches,
    emit: send,
    signedOut: (reason) => send('signed-out', reason || ''),
    onView: e2e
      ? (view) => {
          win.__aacLastAnalytics = view;
        }
      : null,
  });
  const onVisible = () => {
    if (win.document?.visibilityState === 'visible') controller.resumed();
  };
  win.document?.addEventListener?.('visibilitychange', onVisible);
  const stop = controller.stop;
  controller.stop = () => {
    win.document?.removeEventListener?.('visibilitychange', onVisible);
    stop();
  };
  controller.id = ++installs;
  win.aacFrameAnalytics = controller;
  return controller;
}
