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

/** The header and the chrome line are worded again this often while the page is shown (bridge.js does the same). */
export const HEAD_EVERY_MS = 15_000;
/** The dashboard's refresh interval until its settings say otherwise. */
export const DEFAULT_INTERVAL_SECONDS = 60;

/**
 * The frame's Analytics controller. `fetch`, `now()`, `setTimeout`/`clearTimeout`, `visible()` (the document is on
 * screen) and `reducedMotion()` are injected; `platform` is 'mac' or 'windows' as bridge.js picks it.
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
  const timers = { poll: 0, layout: 0, tick: 0, interval: 0 };
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

  /** One GET, as bridge.js's sendNow: the payload, or a thrown error with status and payload (`network` offline). */
  async function read(path) {
    let response;
    try {
      response = await fetchImpl(path, { credentials: 'same-origin' });
      online = true;
    } catch (cause) {
      online = false;
      throw Object.assign(new Error('The dashboard did not answer.'), { network: true, status: 0, payload: null, cause });
    }
    const body = response.status === 204 ? null : await response.json().catch(() => null);
    if (!response.ok) {
      // a 401 about the session ends it (signed out elsewhere, a password change); the frame's sign-in says why
      const code = body?.code;
      if (response.status === 401 && !stopped && !ended && (!code || code === 'auth_required' || code === 'session_revoked')) {
        ended = true;
        signedOut(code === 'session_revoked' ? 'revoked' : undefined);
      }
      throw Object.assign(new Error(body?.error || `Request failed (${response.status}).`), { status: response.status, payload: body });
    }
    return body;
  }

  /**
   * What the page reads beside the analytics: the dashboard (hidden accounts and providers, the switch thresholds, the
   * refresh interval and the chrome line), Antigravity's automatic switching (its threshold) and the display time zone.
   * A failed read keeps what was read before.
   */
  async function readSide(force) {
    reading = true;
    renderChrome();
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
   * Refresh does, then reads the analytics afresh. `silent`: a background poll; one failed read keeps the page's words.
   */
  async function refresh(force = false, mode = 'static', { silent = false } = {}) {
    if (stopped) return;
    const quiet = silent && !force;
    const gen = ++generation;
    if (!quiet) loading(true, '');
    const query = new URLSearchParams({ platform, range: apiRangeFor(page, now()), provider: 'all', account: 'all' });
    if (force) query.set('refresh', 'true');
    let result;
    if (force) {
      await readSide(true);
      result = await read(`/api/accounts/analytics?${query}`).then((r) => ({ r }), (error) => ({ error }));
    } else {
      [result] = await Promise.all([
        read(`/api/accounts/analytics?${query}`).then((r) => ({ r }), (error) => ({ error })),
        readSide(false),
      ]);
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
   * as it is (the interval reads the next one); otherwise it is read now. The dashboard beside it is read again either
   * way (it is cheap): an account hidden or shown on another tab meanwhile is followed at once.
   */
  function enter() {
    const age = now() - Date.parse(payload?.updatedAt ?? '');
    if (payload && Number.isFinite(age) && age >= 0 && age < intervalSeconds * 1000 && analyticsCovers(payload, page, now())) {
      render();
      renderChrome();
      const gen = generation;
      void readSide(false).then(() => {
        if (gen === generation && shown) render();
      });
      return;
    }
    void refresh();
  }

  async function changeRange() {
    const covered = !!payload && analyticsCovers(payload, page, now());
    if (covered) render('morph');
    if (!payload || payload.range?.preset !== apiRangeFor(page, now())) await refresh(false, covered ? 'static' : 'morph');
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
      if (shown && visible()) void refresh(false, 'morph');
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

/**
 * Starts the frame's Analytics: `window.aacFrameAnalytics` holds the calls (a previous one is stopped), and every view
 * is sent as an `aac-analytics` event whose detail is `{key, json}`; `signed-out` (json: the reason) says the session
 * ended. With `?e2e`, `window.__aacLastAnalytics` keeps the last view (as bridge.js keeps it for the kit).
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
  win.aacFrameAnalytics = controller;
  return controller;
}
