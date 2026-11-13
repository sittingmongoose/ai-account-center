// The app frame's Analytics: the frame reads /api/accounts/analytics itself, as Status reads /api/status, and builds
// the page's view with the same builders bridge.js uses (analytics-usage.mjs, analytics-quota.mjs, analytics-data.mjs,
// ANALYTICS_VIEW_VERSION 7; the page rules in analytics-page.mjs). The tab never loads the Slint dashboard or waits on
// its bridge. The page (web-dashboard/leptos/src/analytics/) draws the views and sends its action kinds here.
// Pure apart from the injected fetch, clock, timers and sinks (tests/frame-analytics.test.mjs).
//
// What it sends, through `emit(key, json)`: `analytics` (the view; `analytics-motion` "morph" or "static" just before
// it), `analytics-head` (the header between refreshes), `analytics-loading` ({loading, error}), `analytics-fresh`
// ({shown, updating, stale, asOf, note}: whether the numbers on screen are current and whether newer ones are on
// their way, worded for the page's updating strip) and `chrome` ({statusLead, statusStrong, statusMore}: the frame
// header's line, worded as bridge.js words it). `signedOut(reason)` says a read found the session gone.
//
// The last answers are kept in this browser (`storage`, ANALYTICS_SAVED_KEY, cleared at sign-out), so the next visit
// draws them at once, said to be older ("Updating · showing data from 2 min ago"), while the read for current numbers
// runs. With nothing kept, nothing is sent until the first answer but `analytics-loading`: the page shows its loading
// state, never zeros.
import { analyticsSlintModel } from './analytics-data.mjs';
import { usageView, usageHead, apiRangeFor, trendPaths, hostProgress } from './analytics-usage.mjs';
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
import { rememberSeen, ANALYTICS_SAVED_KEY } from './auth-view.mjs';
import { relTxt } from './analytics-usage.mjs';

/** The header and the chrome line are worded again this often while the page is shown (bridge.js does the same). */
export const HEAD_EVERY_MS = 15_000;
/** The dashboard's refresh interval until its settings say otherwise. */
export const DEFAULT_INTERVAL_SECONDS = 60;
/** A burst of dashboard changes elsewhere in the frame is read once, this long after the last. */
export const SIDE_SETTLE_MS = 250;
/** The kept answers are written at most this often (and when the page leaves the screen). */
export const SAVE_EVERY_MS = 30_000;
/** Kept answers older than this are not drawn. */
export const SAVED_MAX_AGE_MS = 7 * 24 * 3_600_000;
/** What this browser keeps at most, in characters (localStorage holds about 5 million per site). */
export const SAVED_MAX_CHARS = 2_500_000;

/** A kept or served answer timed more than this ahead of this clock is from an earlier read (clocks can differ). */
export const FUTURE_SLACK_MS = 120_000;
const REMOTE_NAMES = { mac: 'Mac', windows: 'Windows', nas1: 'Nas1' };
/** The remote computers a scan is still waiting on, by name, in page order. */
const remoteNames = (list) => (Array.isArray(list) ? Object.keys(REMOTE_NAMES).filter((h) => list.includes(h)).map((h) => REMOTE_NAMES[h]) : []);

const validPayload = (p) => !!p && typeof p === 'object' && p.schemaVersion === 1 && Array.isArray(p.accounts) && !!p.activity && typeof p.activity === 'object' && typeof p.updatedAt === 'string';
/** When an answer's numbers were computed. */
const asOfOf = (p) => { const t = Date.parse(p?.asOf ?? p?.updatedAt ?? ''); return Number.isFinite(t) ? t : null; };

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
  // when the page first got an answer without numbers (a server that just started), or one the server kept from before
  // the read: its next polls come quickly
  let waitingSince = null;
  // the numbers on screen came from this browser's kept answers; no answer from the server yet
  let restored = false;
  // an analytics read is out
  let fetching = false;
  // the last analytics read failed (not offline): what it said; null after a good one
  let readError = null;
  let lastSaved = -Infinity;
  const timers = { poll: 0, layout: 0, tick: 0, interval: 0, side: 0, save: 0 };
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

  /**
   * Whether the numbers on screen are current, for the page's updating strip: `note` is one short line ("Updating · 3
   * of 11 tools · data from 2m ago"), `tip` the whole sentence. `stale`: the numbers predate this visit's newest answer
   * (kept in this browser, kept by the server while it computes the next one, or timed ahead of this clock);
   * `updating`: newer ones are on their way; `failed`: the last read, or the server's last computation, failed, so
   * nothing newer is on its way and the strip says so instead of "Updating" (one message, never both).
   */
  function renderFresh() {
    if (!payload || stopped) return;
    const t = now();
    const asOf = asOfOf(payload);
    // a time ahead of this clock (a clock stepped back on either side): from an earlier read, never "a moment ago"
    const ahead = asOf !== null && asOf - t > FUTURE_SLACK_MS;
    const ago = asOf === null || ahead ? 'an earlier read' : t - asOf < 10_000 ? 'a moment ago' : relTxt(asOf, t);
    const failure = readError ?? (typeof payload.refreshError === 'string' && payload.refreshError ? payload.refreshError : null);
    const stale = restored || payload.stale === true || ahead || failure !== null || !online;
    // a kept answer's own scan flags say nothing about now; only the read under way does
    const act = restored ? {} : payload.activity || {};
    const scanning = restored ? [] : hostProgress(payload).filter((p) => p.scanning > 0);
    const collecting = !restored && (payload.refreshing === true || act.refreshing === true || act.status === 'loading' || scanning.length > 0);
    const updating = online && failure === null && ((restored && fetching) || (payload.stale === true && payload.refreshing !== false) || collecting);
    const remote = restored ? [] : remoteNames(act.refreshingRemote);
    const tools = scanning.length
      ? `${scanning.reduce((n, p) => n + p.done, 0)} of ${scanning.reduce((n, p) => n + p.total, 0)} tools`
      : remote.length ? remote.join(', ') : '';
    const from = `data from ${ago}`;
    const shownFrom = ahead || asOf === null ? 'from an earlier read' : `from ${ago}`;
    let note = '';
    let tip = '';
    if (failure !== null) {
      note = `Couldn't update · ${from}`;
      tip = `Couldn't update: ${failure} The numbers shown are ${shownFrom}.`;
    } else if (!online) {
      note = `Offline · ${from}`;
      tip = `The dashboard did not answer. The numbers shown are ${shownFrom}.`;
    } else if (updating) {
      note = ['Updating', tools, stale ? from : ''].filter(Boolean).join(' · ');
      const scan = restored ? '' : (() => { try { return usageHead(payload, page, { now: t }).updateNote; } catch { return ''; } })();
      tip = [stale ? `Updating. The numbers shown are ${shownFrom}; newer ones are on their way.` : 'Updating: new log lines are being read.', scan].filter(Boolean).join(' ');
    } else if (stale) {
      note = `Data from ${ago} · not updated`;
      tip = `The numbers shown are ${shownFrom} and are not being updated.`;
    }
    push('analytics-fresh', JSON.stringify({ shown: note !== '', updating, stale, failed: failure !== null, asOf, note, tip }));
  }

  /** The kept answer for the page's range, drawn at once on a visit with nothing read yet. */
  function restore() {
    if (payload || !storage) return false;
    let kept = null;
    try {
      kept = JSON.parse(storage.getItem(ANALYTICS_SAVED_KEY) || 'null');
    } catch {
      return false;
    }
    if (!kept || kept.v !== 1 || kept.platform !== platform || !kept.views || typeof kept.views !== 'object') return false;
    const candidate = kept.views[apiRangeFor(page, now())];
    const at = asOfOf(candidate);
    if (!validPayload(candidate) || at === null || now() - at > SAVED_MAX_AGE_MS || at - now() > 60_000) return false;
    payload = candidate;
    restored = true;
    // the account list and thresholds it was drawn with (hidden accounts stay hidden) until the dashboard is read
    if (!dashboard && kept.dashboard?.schemaVersion === 1 && Array.isArray(kept.dashboard.accounts)) {
      dashboard = kept.dashboard;
      antigravityAuto = validAntigravityAuto(kept.auto) ? kept.auto : null;
    }
    return true;
  }

  /** Keeps the newest answer (at most every SAVE_EVERY_MS; `force` when the page leaves the screen). */
  function save(force = false) {
    if (!storage || !payload || restored || stopped || ended) return;
    const wait = lastSaved + SAVE_EVERY_MS - now();
    if (!force && wait > 0) {
      if (!timers.save) timers.save = later(() => { timers.save = 0; save(true); }, wait);
      return;
    }
    clear('save');
    const preset = String(payload.range?.preset || '');
    if (!preset) return;
    let views = { [preset]: payload };
    try {
      const kept = JSON.parse(storage.getItem(ANALYTICS_SAVED_KEY) || 'null');
      // one other range is kept beside it (the one a person switches back to)
      if (kept?.v === 1 && kept.platform === platform && kept.views && typeof kept.views === 'object') {
        const other = Object.entries(kept.views).find(([key, value]) => key !== preset && validPayload(value));
        if (other) views = { [other[0]]: other[1], [preset]: payload };
      }
    } catch {}
    const extra = dashboard ? { dashboard, auto: antigravityAuto } : {};
    let json = JSON.stringify({ v: 1, platform, views, ...extra });
    if (json.length > SAVED_MAX_CHARS) json = JSON.stringify({ v: 1, platform, views: { [preset]: payload }, ...extra });
    if (json.length > SAVED_MAX_CHARS) return;
    try {
      storage.setItem(ANALYTICS_SAVED_KEY, json);
      lastSaved = now();
    } catch {}
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
    renderFresh();
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
    if (payload?.activity?.totals && payload?.stale !== true) waitingSince = null;
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
    const analytics = () => {
      fetching = true;
      renderFresh();
      return read(`/api/accounts/analytics?${query}`).then((r) => ({ r }), (error) => ({ error })).finally(() => { fetching = false; });
    };
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
      restored = false;
      readError = null;
      render(mode);
      save();
      pollFails = 0;
      loading(false, '');
      return;
    }
    const error = result.error || new Error('Unsupported analytics response.');
    render();
    if (quiet && ++pollFails < 2) {
      renderFresh();
      return;
    }
    pollFails = 0;
    const message = error?.message || 'Unable to load account analytics.';
    if (!error?.network) readError = message;
    renderFresh();
    // numbers on screen: the strip says it could not update (one message); with nothing on screen, the banner does
    loading(false, payload ? '' : message);
  }

  /**
   * The page comes on screen. A response that still covers the range and is younger than the refresh interval is drawn
   * as it is (the interval reads the next one); otherwise it is read now. Either way the dashboard beside it is read
   * again first (it is cheap), and the page is drawn once that read lands, so an account hidden or shown on another
   * tab meanwhile is never drawn the old way. Until then the page keeps the view it had.
   */
  function enter() {
    // a first visit with answers kept in this browser draws them at once, said to be older, while it reads
    if (!payload && restore()) {
      render();
      void refresh(false, 'morph');
      return;
    }
    const age = now() - Date.parse(payload?.updatedAt ?? '');
    if (payload && !restored && payload.stale !== true && Number.isFinite(age) && age >= 0 && age < intervalSeconds * 1000 && analyticsCovers(payload, page, now())) {
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
      renderFresh();
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
    /** Another tab is on screen: no reads or timers until it comes back (the newest answer is kept first). */
    hide() {
      shown = false;
      if (timers.save) save(true);
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
