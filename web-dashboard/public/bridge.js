import init, { start_dashboard, resize_dashboard, set_dashboard, set_chrome, set_auth, set_busy, set_theme_mode, set_system_dark, set_reduced_motion, set_safe_area, set_input_profile, set_keyboard, set_online, pop_overlay, push_toast, show_details, close_details, set_update_status, show_activation_confirmation, close_activation_confirmation, set_analytics, set_analytics_loading, set_analytics_head, set_analytics_trend_paths, set_current_page, set_refresh_interval, set_accounts, set_signin_strength, set_accounts_strength, set_login_fields, set_login_setup, set_login_pointer, probe_tick } from './pkg/ccs_account_dashboard.js';
import { dashboardViewModel, detailsViewModel, chromeView, updateViewModel, intervalLabel, parseIntervalLabel } from './view-model.mjs';
import { accountsViewModel, transportOf, transportNote } from './accounts-view.mjs';
import { strength, validateSetup, triesLine, limitWindowMinutes, limitedView, rememberSignIn, forgetSignIn, signedInAt, endedReason, expiredBanner, triesFrom, retryFrom, loginFailure, setupFailure, parseLoginValue, offlineView } from './auth-view.mjs';
import { createAccountsController, MUTATING_ACTIONS } from './accounts-controller.mjs';
import { copyText, signOutFailureText } from './account-actions.mjs';
import { createActivationConfirmation } from './activation-confirmation.mjs';
import { antigravityView, antigravitySettingsPatch, validAntigravityAuto } from './antigravity-data.mjs';
import { createAntigravityConfirmation } from './antigravity-confirmation.mjs';
import { analyticsView, analyticsChoiceId, analyticsSlintModel } from './analytics-data.mjs';
import { usageView, usageHead, apiRangeFor, trendPaths, mixGeo, parseIsoDay, addDays, RANGES, H, D } from './analytics-usage.mjs';
import { quotaView, agendaView, QUOTA_PROVIDERS } from './analytics-quota.mjs';
import { requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard } from './renderer.mjs';
import { premultiplySvgTextureUploads } from './renderer.mjs';
import { createClaudeOpen, openProgress } from './claude-open.mjs';
import { PAGES, pageFromUrl, pagePath } from './page-route.mjs';
import { installLoginBridge } from './login-bridge.mjs';
import { setDisplayTimeZone } from './time-format.mjs';
import { readSafeArea, keyboardHeight, isAppleMobile, installRow, themeColor, themeScreen } from './device.mjs';

// The browser bridge: network, session, timers and every truthfulness rule stay in JavaScript
// (public/*.mjs); the Slint UI receives version 2 view-model JSON and reports intent through
// window.ccsDashboardAction(kind, value). See web-dashboard/ui/README-ARCHITECTURE.md.
let authenticated = false;
let busy = false;
// `serverData` is the dashboard response as received; `data` is the copy the pages draw from. "Show on
// dashboard" is saved on the server (settings.hiddenProviders), so both are the same response.
let serverData = null;
let data = null;
let profiles = [];
let antigravityInventory = null;
let antigravityAuto = null;
let updateJob = null;
let updateDoneTimer = null;
let refreshing = null;
let refreshGeneration = 0;
let username = '';
let refreshIntervalSeconds = 60;
let refreshSettingsKnown = false;
let usageTimer = null;
let analyticsPayload = null;
// The legacy choice lists are the only consumer of the full analyticsView pass, so that pass is built
// on demand (at most once a minute per payload) instead of on every Analytics render.
let analyticsChoices = null;
let analyticsChoicesPayload = null;
let analyticsChoicesAt = 0;
let analyticsChoicesCatalog = null;
let analyticsGeneration = 0;
// While the log scan runs behind the page (loading, or a cached snapshot with a refresh running),
// the page re-reads the server every few seconds so the new numbers land on their own.
let analyticsPollTimer = 0;
// Consecutive background-poll failures; the first retries silently, the second surfaces.
let pollFails = 0;
let openDetailsId = '';
// Counts Details opens, so a click outside the panel can tell a row click (which opens that row) from any other.
let detailsOpens = 0;
// Sign-in: the session length from GET /api/auth/setup, and what the first-run form may do.
let sessionHours = 24;
let setupInfo = { form: false, codeRequired: false };
let authNonce = 0;
// GET /api/auth/check as last read: the trusted local network state and this connection, for the sign-in page's
// network note and the Dashboard sign-in block.
let authCheck = null;
const transport = transportOf(location.protocol, location.hostname);
const origin = typeof location.origin === 'string' ? location.origin : '';
// End-to-end probes (?e2e only): the harness asks the canvas where each probed control is.
const e2e = /[?&]e2e\b/.test(location.search);
const probeRects = new Map();
if (e2e) {
  globalThis.__aacProbe = probeRects;
  globalThis.__aacProbeNow = () => { probeRects.clear(); try { probe_tick(); } catch {} };
}
/** URL state: the page routes /, /analytics and /accounts (page-route.mjs); ?view= still works as an alias. */
function pageFromLocation() { return pageFromUrl(location.pathname, location.search); }
let currentPage = pageFromLocation();
const analyticsSelection = { range: '7d', provider: 'all', account: 'all', metricKey: '', activityInterval: 'Daily' };
// The Analytics page state (version 3). Every number and label is computed in analytics-*.mjs from the response.
const analyticsPage = {
  range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', cbmSort: 'cost',
  // donutOpen: the donut legend groups that are open (_other: the models under 1%; _undrawn: by cost, no arc)
  open: new Set(), compare: new Set(), collapsed: new Set(), donutOpen: new Set(),
  // chart boxes reported by the Slint layout (analytics-layout): the charts are laid out in these pixels
  sizes: { trend: null, daily: null, heat: null, focus: null },
};
let motionReduced = false;
const platform = /Windows/i.test(navigator.userAgent) ? 'windows' : 'mac';
const host = typeof location.host === 'string' ? location.host : '';

// ---------------------------------------------------------------- feedback
function setBusy(value) { busy = value; set_busy(value); }
/** Results and failures appear as toasts on any page, never buried at the bottom. */
function toast(kind, title, body = '', ms = 4800) {
  if (e2e) (globalThis.__aacToasts ||= []).push({ kind, title, body });
  push_toast(kind, title, body, ms);
}
function failure(message, title = 'That did not work') { toast('err', title, message, 6400); }
let authState = 'loading';
// The offline screen (DESIGN-MOBILE.md 4.6): at launch, when /api cannot be reached, the sign-in page
// shows the offline card and retries every 15 s while it is visible, on the `online` event and when the
// app comes back to the foreground. No readings are cached on the device, so nothing else is shown.
const OFFLINE_RETRY_MS = 15_000;
let offlineTimer = 0;
let sessionChecking = false;
function goOffline() {
  const words = offlineView(host, Date.now());
  auth(false, 'offline', {
    // the desktop look keeps today's sentence in the sign-in card's message slot
    message: 'Unable to connect to AI Account Center. Try refreshing this page.',
    offlineTitle: words.title, offlineStrong: words.strong, offlineBody: words.body, offlineMeta: words.meta,
  });
}
function stopOfflineRetry() {
  if (offlineTimer) { clearInterval(offlineTimer); offlineTimer = 0; }
}
function retryOffline() {
  if (authenticated || busy || authState !== 'offline' || sessionChecking) return;
  if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
  void checkSession();
}
function auth(signedIn, state, extra = {}) {
  authenticated = signedIn;
  if (state !== 'offline') stopOfflineRetry();
  else if (!offlineTimer) offlineTimer = setInterval(retryOffline, OFFLINE_RETRY_MS);
  authState = state;
  const view = {
    state, host, username, transportNote: transportNote(transport, authCheck), sessionHours, nonce: authNonce,
    setupForm: setupInfo.form, codeRequired: setupInfo.codeRequired, ...extra,
  };
  // the e2e harness reads the words it handed Slint next to the pixels it sees (?e2e only)
  if (e2e) globalThis.__aacLastAuth = { signedIn, ...view };
  // Every sign-in page state clears Slint's data-ready flag (set_auth in src/lib.rs), and only a dashboard push
  // sets it again. So the first pushes after any sign-in (a sign-out, or a session that ended in this tab, such
  // as a server restart) must reach Slint even when their JSON matches the last one sent, or Home never reveals.
  if (!signedIn) pushedJson.clear();
  // The login form's HTML password goes with the Slint one: once signed in (lib.rs set_auth clears Slint's), and when
  // sign-in pauses (signin.slint forgets the typed password in the limited state).
  if (signedIn || state === 'limited') { try { loginBridge?.clearPassword(); } catch {} }
  set_auth(signedIn, JSON.stringify(view));
  syncThemeColor();
}

/** One request: { status, payload } on success; a refusal throws with its status, payload and headers. */
async function send(path, options = {}) {
  let response;
  try {
    response = await fetch(path, { credentials: 'same-origin', ...options, headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
    setOnline(true);
  } catch (cause) {
    setOnline(false);
    throw Object.assign(new Error('The dashboard did not answer.'), { network: true, status: 0, payload: null, cause });
  }
  const payload = response.status === 204 ? null : await response.json().catch(() => null);
  if (!response.ok) {
    // signed out from another browser (sign out other browsers, a password change, sign out all devices)
    // A 401 that is about the session ends it here; a refused password (wrong_password) is only an answer.
    const code = payload?.code;
    if (response.status === 401 && path !== '/api/auth/login' && authenticated && (!code || code === 'auth_required' || code === 'session_revoked')) sessionEnded(code === 'session_revoked' ? 'revoked' : undefined);
    const error = new Error(payload?.error || `Request failed (${response.status}).`);
    error.status = response.status;
    error.payload = payload;
    error.headers = response.headers;
    error.retryAfter = response.headers?.get?.('Retry-After') || response.headers?.get?.('RateLimit-Reset') || '';
    throw error;
  }
  return { status: response.status, payload };
}
async function request(path, options = {}) { return (await send(path, options)).payload; }
const mutation = (path, body, method = 'POST') => request(path, { method, body: JSON.stringify(body) });
/** One request built by account-actions.mjs `requests` ({ method, path, body }). */
const call = req => send(req.path, { method: req.method, ...(req.body !== undefined ? { body: JSON.stringify(req.body) } : {}) });

// Accounts & Settings: every sign-in and sign-out control (accounts-controller.mjs).
const accounts = createAccountsController({
  call,
  changed: () => renderAccounts(),
  toast: (kind, title, body, ms) => toast(kind, title, body, ms),
  refresh: () => refresh(true),
  data: () => serverData,
  // A save's answer is newer than any dashboard read already in flight: drop that read (it may hold the lists from
  // before the save) and start a fresh one, so a switch never flips back and the next switch never sends a stale list.
  setData: next => {
    const inFlight = !!refreshing;
    refreshGeneration++;
    serverData = next; data = next; render();
    if (inFlight) void refresh(false, true);
  },
  strength: view => { try { set_accounts_strength(JSON.stringify(view)); } catch {} },
  copy: value => copyText(value),
  open: url => { try { globalThis.open?.(url, '_blank', 'noopener'); } catch {} },
  storage: (() => { try { return globalThis.localStorage; } catch { return null; } })(),
  // Turn off (or on): the saved check follows at once, then is read again (secureTransport moves with it)
  networkChanged: async net => {
    if (authCheck && net && typeof net.trustLocalNetwork === 'boolean') {
      authCheck = { ...authCheck, trustedLocalNetwork: net.trustLocalNetwork, ...(net.connection && typeof net.connection === 'object' ? { connection: net.connection } : {}) };
    }
    await refreshAuthCheck();
  },
});

// Claude "Open" progress (claude-open.mjs): one POST that asks for the 202 answer, then read-only polling of the
// profile list. A finished Open ends in a toast; the row line clears a few seconds later.
const claudeOpen = createClaudeOpen({
  post: async (id, target) => {
    const { status, payload } = await send(`/api/claude/desktop-profiles/${encodeURIComponent(id)}/open`, {
      method: 'POST', body: JSON.stringify({ platform: target }), headers: { Prefer: 'respond-async' },
    });
    return { status, body: payload };
  },
  list: async () => {
    const payload = await request('/api/claude/desktop-profiles');
    if (Array.isArray(payload?.profiles)) profiles = payload.profiles;
    return payload;
  },
  changed: () => { if (authenticated) render(); },
  finished: (id, view) => {
    if (!authenticated) return;
    const where = view.platform === 'windows' ? 'Windows' : 'Mac';
    if (view.state === 'opened') toast('ok', `Claude opened on ${where}`, 'It opened in its own desktop profile.');
    else if (view.state === 'failed' || view.state === 'blocked_uncertain') failure(view.text, `Claude did not open on ${where}`);
    else toast('info', `Opening Claude on ${where}`, view.text, 7000);
  },
});

// ---------------------------------------------------------------- rendering
function context(extra = {}) {
  return { profiles, platform, antigravityInventory, antigravityAuto, refreshing: !!refreshing && extra.refreshing !== false, intervalSeconds: refreshIntervalSeconds, username, host, openProgress: openProgress(claudeOpen.views(), profiles), ...extra };
}
function antigravityModel() { return antigravityView(data, antigravityInventory, antigravityAuto); }
// Slint keeps every model and property between pushes, so a JSON identical to the one already on
// screen cannot change anything: the clock ticks rebuild the same header three times out of four,
// and a poll that returns the same payload re-renders the same page.
const pushedJson = new Map();
function pushModel(key, json, send) {
  if (pushedJson.get(key) === json) return;
  pushedJson.set(key, json);
  send(json);
}
function render() {
  if (!data) return;
  pushModel('dashboard', JSON.stringify(dashboardViewModel(data, context({ refreshing: false }))), set_dashboard);
  if (openDetailsId) renderDetails(openDetailsId);
  renderAccounts();
  // the quota history and the agenda read the current readings too
  if (currentPage === 'analytics' && analyticsPayload) renderAnalytics();
}
/** Accounts & Settings (version 1, accounts-view.mjs): drawn while the page is open. */
function renderAccounts() {
  if (!serverData || currentPage !== 'accounts') return;
  const st = accounts.state;
  try {
    const vm = accountsViewModel(serverData, context({
      refreshing: false,
      refreshSeconds: refreshIntervalSeconds, refreshKnown: refreshSettingsKnown, updateJob,
      origin, transport, sessionHours, signedInAt: signedInAt(globalThis.localStorage),
      registry: st.registry, flows: st.flows, lines: st.lines, busyAct: st.busyAct, visPending: st.visPending, check: authCheck, signin: st.signin, prefs: st.prefs,
    }));
    // Phase 6 feeds the Settings "Home screen app" row state; accounts-view.mjs does not compute it.
    vm.install = currentInstallRow();
    if (e2e) globalThis.__aacLastAccounts = vm;
    pushModel('accounts', JSON.stringify(vm), set_accounts);
  } catch (error) { console.error('Accounts & Settings could not be drawn.', error); }
}
function renderChrome(isRefreshing = false) {
  if (data || isRefreshing) {
    const chrome = chromeView(data, { refreshing: isRefreshing, intervalSeconds: refreshIntervalSeconds, username, host });
    // 4.6: the connection dropped; the readings stay on screen and the status line says so
    if (!onlineState && !isRefreshing && chrome.statusLead === 'Updated') chrome.statusLead = 'Offline';
    pushModel('chrome', JSON.stringify(chrome), set_chrome);
  }
}
function renderDetails(id) {
  const view = data ? detailsViewModel(data, id, context({ refreshing: false })) : null;
  if (!view) { openDetailsId = ''; close_details(); return; }
  openDetailsId = id;
  show_details(JSON.stringify(view));
}
function pendingActivation() { return activationConfirmation.hasPending() || antigravityConfirmation.hasPending(); }

// ---------------------------------------------------------------- Back button and history (3.9)
// Every overlay (Details, a sheet, a popover, the activation dialog) pushes one entry; a popstate
// closes the top-most overlay through pop_overlay(). Closing an overlay any other way consumes its
// entry with history.back(); the flag marks our own consumption so the popstate handler ignores it.
// Page switches push one entry from Home and replace otherwise, so Back from Analytics or Accounts
// lands on Home and Back from Home leaves the app.
let overlayDepth = 0;
let consumingOverlayEntry = false;
function belowD() { return innerWidth < 1280; }
function pushOverlayEntry() {
  if (!belowD()) return;
  overlayDepth++;
  try { history.pushState({ aac: 'overlay' }, ''); } catch {}
}
function consumeOverlayEntry() {
  if (!belowD() || overlayDepth <= 0) return;
  overlayDepth--;
  consumingOverlayEntry = true;
  try { history.back(); } catch { consumingOverlayEntry = false; }
}
/**
 * The slide-over's left edge on D: SlideOver.panel-w in ui/components/slide-over.slint is
 * clamp(30%, 420, 580) px. Below D the sheet and side panel close through their own scrims, so
 * the outside-click probe is disabled there (a left edge of 0 never matches).
 */
function detailsPanelLeft() { return innerWidth >= 1280 ? innerWidth - Math.min(580, Math.max(420, innerWidth * 0.3)) : 0; }
/**
 * Details closes on a click anywhere outside its panel (ROUND2). Slint's own background areas only see clicks
 * that nothing else takes, so a header button, a nested row action or an Analytics card would leave it open.
 * A click on another row opens that row instead: its `details` action arrives while Slint handles the click,
 * before the timeout below runs. A pending switch confirmation keeps Details as it is.
 */
function closeDetailsOnOutsideClicks(canvas) {
  if (typeof canvas?.addEventListener !== 'function') return;
  let pressed = null;
  canvas.addEventListener('pointerdown', event => {
    pressed = openDetailsId && event.button === 0 && event.clientX < detailsPanelLeft() && !pendingActivation() ? detailsOpens : null;
  }, true);
  canvas.addEventListener('pointerup', () => {
    const opens = pressed; pressed = null;
    if (opens === null) return;
    setTimeout(() => {
      if (openDetailsId && detailsOpens === opens && !pendingActivation()) { openDetailsId = ''; close_details(); }
    }, 0);
  }, true);
}

const activationConfirmation = createActivationConfirmation({
  activate: (target, body) => mutation(`/api/codex/profiles/${encodeURIComponent(target)}/activate`, body),
  prompt: confirmation => {
    pushOverlayEntry();
    show_activation_confirmation(JSON.stringify({ ...confirmation, expiresAt: `Review valid until ${new Date(confirmation.expiresAt).toLocaleString()}` }));
  },
  close: () => {
    consumeOverlayEntry();
    close_activation_confirmation();
  },
  busy: inProgress => setBusy(inProgress),
  success: async result => {
    if (data && typeof result?.name === 'string') {
      for (const account of data.accounts) if (account.provider === 'codex') account.isActive = account.capabilities?.codexProfile === result.name;
      render();
    }
    toast('ok', 'Codex account switched', `${result?.email || result?.name || 'The selected account'} is now active on Ubuntu.`);
    await refresh(true);
  },
  error: message => failure(message, 'Codex switch failed'),
});

const antigravityConfirmation = createAntigravityConfirmation({
  activate: (target, body) => mutation(`/api/antigravity/profiles/${encodeURIComponent(target)}/activate`, body),
  confirm: (target, body) => mutation(`/api/antigravity/profiles/${encodeURIComponent(target)}/confirm`, body),
  recover: () => mutation('/api/antigravity/recover', { hostId: 'ubuntu' }),
  prompt: confirmation => {
    pushOverlayEntry();
    show_activation_confirmation(JSON.stringify({ ...confirmation, expiresAt: `Review valid until ${new Date(confirmation.expiresAt).toLocaleString()}` }));
  },
  close: () => {
    consumeOverlayEntry();
    close_activation_confirmation();
  },
  busy: inProgress => setBusy(inProgress),
  success: async result => {
    // Selection/running proof comes from the next inventory, never an optimistic UI guess.
    if (['completed', 'aborted', 'restored-previous', 'no-recovery-pending'].includes(result?.status)) {
      const who = result?.email || 'The live login';
      const what = result?.status === 'completed' ? `${who} was activated.`
        : result?.status === 'aborted' ? `The stuck switch was undone; ${who} is still active.`
        : result?.status === 'restored-previous' ? `${who} was restored from its saved login.`
        : 'Nothing was stuck.';
      toast('ok', 'Antigravity recovery finished on Ubuntu', `${what} Checking the native identity now.`);
    } else {
      toast('ok', 'Antigravity switched on Ubuntu', `${result?.email || 'The selected login'} was activated. Checking the native identity now.`);
    }
    antigravityInventory = null; render();
    await refresh(true);
  },
  error: message => failure(message, 'Antigravity switch failed'),
});

async function refresh(force = false, restart = false) {
  if (!authenticated) return;
  if (refreshing && !force && !restart) return refreshing;
  const generation = ++refreshGeneration;
  renderChrome(true);
  const operation = (async () => {
    try {
      const [next, inventory, antigravityProfiles, antigravityStatus] = await Promise.all([
        request(`/api/accounts/dashboard?platform=${platform}&refresh=${force}`),
        request('/api/claude/desktop-profiles').catch(() => ({ profiles })),
        request('/api/antigravity/profiles').catch(() => null),
        request('/api/antigravity/auto-switch').catch(() => null),
      ]);
      if (generation !== refreshGeneration || !authenticated) return;
      if (next?.schemaVersion !== 1 || !Array.isArray(next.accounts)) throw new Error('Unsupported account dashboard response.');
      serverData = next;
      data = next;
      if (Number.isInteger(next.settings?.refreshIntervalSeconds)) applyRefreshInterval(next.settings.refreshIntervalSeconds);
      profiles = Array.isArray(inventory?.profiles) ? inventory.profiles : profiles;
      antigravityInventory = antigravityProfiles;
      antigravityAuto = validAntigravityAuto(antigravityStatus) ? antigravityStatus : validAntigravityAuto(next.antigravityAutoSwitch) ? next.antigravityAutoSwitch : null;
      render();
      // the page's actions and refusals follow the accounts: read them again with each refresh while it is open
      if (currentPage === 'accounts') void accounts.loadRegistry();
    } catch (error) {
      if (generation === refreshGeneration && authenticated) failure(`${error.message} The last received readings stay visible.`, 'Refresh failed');
    }
  })();
  refreshing = operation;
  await operation;
  if (refreshing === operation) { refreshing = null; renderChrome(false); }
}

// ---------------------------------------------------------------- analytics
// The usage-trend morph: the geometry on screen and the one being animated to (paths rebuilt every frame).
let shownGeo = null, shownSize = '', trendRaf = 0, layoutTimer = 0;
const easeInOut = t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
function analyticsContext(now) {
  const auto = data?.codexAutoSwitch;
  const codex = auto?.enabled === true && Number.isFinite(auto?.thresholdPercent) ? 100 - auto.thresholdPercent : null;
  const agAccounts = (data?.accounts || []).filter(account => account.provider === 'antigravity').length;
  const antigravity = antigravityAuto?.enabled === true && agAccounts > 1 && Number.isFinite(antigravityAuto?.thresholdUsedPercent) ? antigravityAuto.thresholdUsedPercent : null;
  return { now, dashboard: data, open: analyticsPage.open, compare: analyticsPage.compare, collapsed: analyticsPage.collapsed, focusWidth: analyticsPage.sizes.focus?.w, focusHeight: analyticsPage.sizes.focus?.h, thresholds: { codex, antigravity } };
}
/** The choice lists behind the legacy analytics metric and account actions; Rust never reads them. */
function choiceLists() {
  if (!analyticsPayload) {
    analyticsChoices = null;
    analyticsChoicesPayload = null;
    analyticsChoicesCatalog = null;
    return null;
  }
  const catalog = data?.accounts || [];
  const now = Date.now();
  // The account choices merge the dashboard catalog, so a registry refresh that lands
  // while the analytics payload stays young must not validate against stale choices.
  if (
    analyticsChoices &&
    analyticsChoicesPayload === analyticsPayload &&
    analyticsChoicesCatalog === catalog &&
    now - analyticsChoicesAt < 60_000
  )
    return analyticsChoices;
  const view = analyticsView(
    analyticsPayload,
    { catalog, metricKey: analyticsSelection.metricKey, activityInterval: analyticsSelection.activityInterval },
    now
  );
  analyticsChoicesPayload = analyticsPayload;
  analyticsChoicesCatalog = catalog;
  analyticsChoicesAt = now;
  analyticsChoices = view?.choices ?? null;
  return analyticsChoices;
}
/** mode 'morph' animates the usage trend from the shape on screen to the new one (range, filter and toggles). */
function renderAnalytics(mode = 'static') {
  if (!analyticsPayload) return;
  const now = Date.now();
  const usage = usageView(analyticsPayload, analyticsPage, { now, sizes: analyticsPage.sizes });
  const ctx = analyticsContext(now);
  // quota and agenda times read at minute precision ("in 3h 25m"), so they run on a minute clock: identical
  // re-renders within the minute build identical models, and the agenda updates only when its data changes
  const slow = { ...ctx, now: Math.floor(now / 60000) * 60000 };
  const quota = quotaView(analyticsPayload, slow);
  const agenda = agendaView(analyticsPayload, slow);
  const next = usage.trend.geo, size = `${usage.trend.pw}x${usage.trend.ph}`;
  const from = shownGeo;
  const morph = mode === 'morph' && !motionReduced && from && shownSize === size && from.lv[0].length === next.lv[0].length;
  cancelAnimationFrame(trendRaf);
  // Only state/usage/quota/agenda reach Slint; the legacy head/KPI/quota-group block is not built.
  const slintView = analyticsSlintModel(null, { usage, quota, agenda, state: analyticsPage, paths: trendPaths(morph ? from : next) });
  // e2e only (?e2e): the last Analytics view handed to Slint, so a harness can read its words
  if (e2e) globalThis.__aacLastAnalytics = slintView;
  pushModel('analytics', JSON.stringify(slintView), set_analytics);
  clearTimeout(analyticsPollTimer);
  const act = analyticsPayload?.activity;
  // While the server is reading (a cold load or a refresh running) the page re-reads it every
  // 10 s - a live analytics projection costs real server CPU, so the poll stays calm; while
  // cells are merely waiting for the next scheduled scan, a 15 s poll watches them settle.
  // Poll answers render as morphs, so converging numbers glide in place.
  const busyNow = act?.status === 'loading' || act?.refreshing === true;
  const converging = Array.isArray(act?.sources) && act.sources.some(r => r?.state === 'scanning');
  if (currentPage === 'analytics' && (busyNow || converging))
    analyticsPollTimer = setTimeout(() => { if (currentPage === 'analytics') void refreshAnalytics(false, 'morph', { silent: true }); }, busyNow ? 10000 : 15000);
  shownSize = size;
  if (!morph) { shownGeo = next; return; }
  const t0 = performance.now();
  const frame = time => {
    const k = easeInOut(Math.min(1, (time - t0) / 520));
    shownGeo = k >= 1 ? next : mixGeo(from, next, k);
    try { set_analytics_trend_paths(JSON.stringify(trendPaths(shownGeo))); } catch { return; }
    if (k < 1) trendRaf = requestAnimationFrame(frame);
  };
  trendRaf = requestAnimationFrame(frame);
}
function renderAnalyticsHead() {
  if (!analyticsPayload || currentPage !== 'analytics') return;
  // The header alone (usageHead), not the whole usage view: this runs every 15 s between refreshes.
  try { pushModel('analyticsHead', JSON.stringify(usageHead(analyticsPayload, analyticsPage, { now: Date.now() })), set_analytics_head); } catch {}
}
/** True when the response in hand already holds the hours of the page range (no fetch is needed to draw it). */
function analyticsCovers(payload, now = Date.now()) {
  const from = Date.parse(payload?.range?.from);
  if (!Number.isFinite(from)) return false;
  const { range } = analyticsPage;
  if (range === 'all') return payload.range?.preset === '30d';
  const start = range === '24h' ? now - D : range === '7d' ? now - 7 * D : range === '30d' ? now - 30 * D
    : range === 'month' ? new Date(new Date(now).getFullYear(), new Date(now).getMonth(), 1).getTime()
    : Number.isFinite(analyticsPage.from) ? analyticsPage.from : now - 30 * D;
  return from <= start + H;
}
/**
 * opts.silent: a background poll. It keeps the view calm - no loading flag, and a single failed
 * fetch does not replace the page's words; it simply tries again on the next tick. A second
 * consecutive failure surfaces like any other error.
 */
async function refreshAnalytics(force = false, mode = 'static', opts = {}) {
  if (!authenticated) return;
  const silent = opts.silent === true && !force;
  const generation = ++analyticsGeneration;
  if (!silent) set_analytics_loading(true, '');
  // the backend accepts 24h, 7d and 30d: Month, All and custom ranges read the covering window and are cut here
  const query = new URLSearchParams({ platform, range: apiRangeFor(analyticsPage, Date.now()), provider: 'all', account: 'all' });
  if (force) query.set('refresh', 'true');
  try {
    const result = await request(`/api/accounts/analytics?${query}`);
    if (generation !== analyticsGeneration || !authenticated) return;
    if (result?.schemaVersion !== 1 || !Array.isArray(result.accounts)) throw new Error('Unsupported analytics response.');
    analyticsPayload = result; renderAnalytics(mode);
    pollFails = 0; set_analytics_loading(false, '');
  } catch (error) {
    if (generation !== analyticsGeneration) return;
    renderAnalytics();
    if (silent && ++pollFails < 2) return;
    pollFails = 0;
    set_analytics_loading(false, error?.message || 'Unable to load account analytics.');
  }
}
/**
 * Entering the Analytics page. A payload that still covers the page range and is younger than the refresh
 * interval is drawn as it is: the interval timer fetches the next one, exactly as it does while the page
 * stays open, so the round-trip only ever buys a reading the timer is about to replace.
 */
function enterAnalytics() {
  const age = Date.now() - Date.parse(analyticsPayload?.updatedAt ?? '');
  if (analyticsPayload && Number.isFinite(age) && age >= 0 && age < refreshIntervalSeconds * 1000 && analyticsCovers(analyticsPayload)) {
    renderAnalytics();
    return;
  }
  void refreshAnalytics();
}
async function changeAnalyticsRange() {
  const covered = analyticsPayload && analyticsCovers(analyticsPayload);
  if (covered) renderAnalytics('morph');
  if (!analyticsPayload || analyticsPayload.range?.preset !== apiRangeFor(analyticsPage, Date.now())) await refreshAnalytics(false, covered ? 'static' : 'morph');
}
const toggleIn = (set, value) => { if (set.has(value)) set.delete(value); else set.add(value); };
async function analyticsAction(action, value) {
  if (action === 'analytics-layout') {
    const [kind, w, h] = String(value).split(',');
    const width = Number(w), height = Number(h);
    if (!(kind in analyticsPage.sizes) || !Number.isFinite(width) || !Number.isFinite(height) || width <= 0) return;
    const prior = analyticsPage.sizes[kind];
    if (prior && Math.abs(prior.w - width) < 1 && Math.abs(prior.h - height) < 1) return;
    analyticsPage.sizes[kind] = { w: width, h: height };
    // a layout report arrives while Slint lays out: draw again once it has finished
    clearTimeout(layoutTimer);
    layoutTimer = setTimeout(() => renderAnalytics('static'), 30);
    return;
  }
  if (action === 'analytics-range') {
    if (!RANGES.some(([id]) => id === value) || (analyticsPage.range === value)) return;
    analyticsPage.range = value; analyticsPage.from = null; analyticsPage.to = null;
    await changeAnalyticsRange(); return;
  }
  if (action === 'analytics-custom') {
    const [first, last] = String(value).split(',').map(parseIsoDay);
    const now = Date.now();
    if (!Number.isFinite(first) || !Number.isFinite(last) || last < first || first < now - 31 * D || first > now) return;
    analyticsPage.range = 'custom'; analyticsPage.from = first; analyticsPage.to = addDays(last, 1);
    await changeAnalyticsRange(); return;
  }
  // the picker offers All and the providers that served usage (dashboard ids, or "other")
  if (action === 'analytics-provider' && (value === 'all' || /^[a-z][a-z0-9-]{0,39}$/.test(value))) {
    if (analyticsPage.prov === value) return;
    analyticsPage.prov = value; renderAnalytics('morph'); return;
  }
  if (action === 'analytics-split') { analyticsPage.split = value === 'true'; renderAnalytics('morph'); return; }
  if (action === 'analytics-cache') { analyticsPage.cache = value === 'true'; renderAnalytics('morph'); return; }
  if (action === 'analytics-donut') { if (['tokens', 'cost'].includes(value)) { analyticsPage.donut = value; renderAnalytics(); } return; }
  if (action === 'analytics-cbm-sort') { if (['cost', 'tokens'].includes(value)) { analyticsPage.cbmSort = value; renderAnalytics(); } return; }
  if (action === 'analytics-donut-open') { if (['_other', '_undrawn'].includes(value)) { toggleIn(analyticsPage.donutOpen, value); renderAnalytics(); } return; }
  if (action === 'analytics-heat') { if (['cost', 'tokens'].includes(value)) { analyticsPage.heat = value; renderAnalytics(); } return; }
  if (action === 'analytics-focus' || action === 'analytics-compare') {
    if (!analyticsPayload?.accounts?.some(account => account?.id === value) && !data?.accounts?.some(account => account?.id === value)) return;
    toggleIn(action === 'analytics-focus' ? analyticsPage.open : analyticsPage.compare, value); renderAnalytics(); return;
  }
  if (action === 'analytics-group') {
    if (!QUOTA_PROVIDERS.some(([id]) => id === value)) return;
    toggleIn(analyticsPage.collapsed, value); renderAnalytics(); return;
  }
  // the earlier analytics kinds keep working (they drive the quota-history data, not the page controls)
  if (action === 'analytics-activity-interval') {
    if (!['Daily', 'Hourly'].includes(value)) return;
    analyticsSelection.activityInterval = value; renderAnalytics(); return;
  }
  if (action === 'analytics-metric-key') {
    analyticsSelection.metricKey = choiceLists()?.metrics?.some(row => row.id === value) ? value : '';
    renderAnalytics(); return;
  }
  if (action === 'analytics-account-id' || action === 'analytics-account' || action === 'analytics-metric') {
    const kind = action === 'analytics-account-id' ? 'account' : action.slice('analytics-'.length);
    const id = action === 'analytics-account-id' ? (choiceLists()?.accounts?.some(row => row.id === value) ? value : null) : analyticsChoiceId({ choices: choiceLists() }, kind, value);
    if (id === null) return;
    if (kind === 'metric') analyticsSelection.metricKey = id; else analyticsSelection.account = id;
    renderAnalytics(); return;
  }
  if (action === 'analytics-refresh') { await refresh(true); await refreshAnalytics(true); return; }
  await refreshAnalytics();
}

// ---------------------------------------------------------------- settings, updates, session
function applyRefreshInterval(seconds, confirmed = true) {
  if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
  refreshIntervalSeconds = seconds;
  refreshSettingsKnown = confirmed;
  set_refresh_interval(seconds, confirmed);
  renderAccounts();
  if (usageTimer) clearInterval(usageTimer);
  usageTimer = setInterval(() => { if (authenticated && !busy && !pendingActivation()) { void refresh(); if (currentPage === 'analytics') void refreshAnalytics(false, 'morph'); } }, seconds * 1000);
}
async function loadSettings() {
  // The two independent reads fly together; each still applies (or fails) on its own, in order.
  const [settings, prefs] = await Promise.all([
    request('/api/accounts/settings').catch(() => null),
    request('/api/accounts/preferences').catch(() => null),
  ]);
  try { applyRefreshInterval(settings?.refreshIntervalSeconds); } catch {}
  try { if (prefs && typeof prefs.timeZone === 'string') setDisplayTimeZone(prefs.timeZone); } catch {}
}
function renderUpdate(done = false) { set_update_status(JSON.stringify(updateViewModel(updateJob, { done }))); renderAccounts(); }
async function updateStatus() {
  if (!authenticated) return;
  const wasRunning = updateJob?.state === 'running';
  try {
    const result = await request('/api/app-updates/status');
    updateJob = result?.job || null;
    if (wasRunning && updateJob && updateJob.state !== 'running') {
      // On finish the button morphs to a check for 2 s and the result appears in a toast.
      const results = Array.isArray(updateJob.results) ? updateJob.results : [];
      const count = status => results.filter(row => row.status === status).length;
      const failed = count('failed') + count('restart_failed');
      const skippedN = count('skipped'), unknownN = count('unknown'), actionN = count('action_required');
      toast(failed ? 'err' : 'ok', failed ? 'App update finished with failures' : 'Apps updated', `${count('updated')} updated, ${count('current')} already current, ${count('not_installed')} not installed${failed ? `, ${failed} failed` : ''}${actionN ? `, ${actionN} need action` : ''}${skippedN ? `, ${skippedN} skipped` : ''}${unknownN ? `, ${unknownN} unknown` : ''}. Details under Accounts & Settings.`, 7000);
      renderUpdate(true);
      clearTimeout(updateDoneTimer);
      updateDoneTimer = setTimeout(() => renderUpdate(false), 2000);
      return;
    }
    renderUpdate(false);
  } catch { renderUpdate(false); }
}
function navigate(page, { replace = false } = {}) {
  if (!PAGES.includes(page)) page = 'home';
  const wasHome = currentPage === 'home';
  currentPage = page;
  set_current_page(page);
  // phase 5 (4.5.4 Success): the last page id, so a fresh sign-in in the installed app can reopen it
  try { globalThis.localStorage?.setItem('aac-last-page', page); } catch {}
  const url = pagePath(page);
  // Back/history (3.9): Home to another page pushes one entry; between non-Home pages, and back
  // to Home, it replaces, so Back from Analytics or Accounts goes Home and Back from Home leaves.
  const method = replace || !wasHome || page === 'home' ? 'replaceState' : 'pushState';
  try { globalThis.history?.[method]?.(null, '', url); } catch {}
  if (page === 'analytics' && authenticated) enterAnalytics();
  if (page === 'accounts' && authenticated) { renderAccounts(); void accounts.loadAll(); }
}
async function loadAuthSetup() {
  try {
    const setup = await request('/api/auth/setup');
    if (Number.isInteger(setup?.sessionTimeoutHours) && setup.sessionTimeoutHours > 0) sessionHours = setup.sessionTimeoutHours;
    // The first-run form needs POST /api/auth/setup; the server announces that route with `setupCodeRequired`
    // (CONTRACT-auth-devices.md section 4). Until then the setup state shows the command that sets sign-in up.
    setupInfo = { form: typeof setup?.setupCodeRequired === 'boolean', codeRequired: setup?.setupCodeRequired === true };
  } catch {}
}
function showSetup(extra = {}) {
  if (setupInfo.form && authState !== 'setup') set_signin_strength(JSON.stringify({ ...strength(''), matches: false }));
  auth(false, 'setup', setupInfo.form ? extra : {
    bannerTitle: 'Set up sign-in on the server',
    bannerBody: 'Run ai-account-center dashboard auth setup on the dashboard host, then check again here.',
    ...extra,
  });
}
/** GET /api/auth/check again; the copy in hand stays when it cannot be read. */
async function refreshAuthCheck() {
  try { const status = await request('/api/auth/check'); if (status && typeof status === 'object') authCheck = status; } catch {}
}
/**
 * The sign-in page after a sign-out or an ended session. It is drawn at once from the check in hand, then the
 * check is read again: local network trust may have been turned off since sign-in (here or in another browser),
 * and the note under the form must say how the password travels now.
 */
function showSignedOut(state, extra) {
  auth(false, state, extra);
  const shown = transportNote(transport, authCheck);
  void refreshAuthCheck().then(() => {
    if (!authenticated && authState === state && transportNote(transport, authCheck) !== shown) auth(false, state, extra);
  });
}
/** The server no longer knows this browser's session: say whether it ran out, ended early or was revoked. */
function sessionEnded(reason = endedReason(signedInAt(globalThis.localStorage), sessionHours) || 'ended') {
  forgetSignIn(globalThis.localStorage);
  claudeOpen.reset();
  accounts.reset();
  const banner = expiredBanner(reason, sessionHours);
  showSignedOut('expired', { bannerTitle: banner.title, bannerBody: banner.body });
}
/**
 * Sign out. It is never held by a running save or Claude Open (`busy`), only by a switch confirmation that is
 * waiting on screen. A failure is said in the page's own words, and this browser then stays signed in.
 */
async function signOut() {
  if (pendingActivation()) { toast('info', 'Finish the account switch first', 'Confirm or cancel the switch, then sign out.'); return; }
  if (accounts.state.signin.busy === 'logout') return;
  accounts.state.signin.busy = 'logout'; renderAccounts();
  try { await mutation('/api/auth/logout', {}); }
  catch (error) {
    accounts.state.signin.busy = ''; renderAccounts();
    // a session another browser already signed out is signed out here too
    if (error?.payload?.code !== 'session_revoked') {
      if (authenticated) { const t = signOutFailureText(error); failure(t.body, t.title); }
      return;
    }
  }
  forgetSignIn(globalThis.localStorage);
  analyticsGeneration++; analyticsPayload = null; analyticsChoices = null; analyticsChoicesPayload = null; analyticsChoicesCatalog = null; clearTimeout(analyticsPollTimer); data = null; serverData = null; profiles = []; antigravityInventory = null; antigravityAuto = null; refreshGeneration++; openDetailsId = ''; pushedJson.clear();
  pollFails = 0;
  claudeOpen.reset();
  accounts.reset();
  showSignedOut('default', { notice: true, message: 'Signed out.' });
}
async function enterDashboard() {
  await loadSettings(); await refresh(true); await updateStatus();
  await afterSignIn();
  if (currentPage === 'analytics') await refreshAnalytics();
}
/** Once signed in: move a browser-only "Show on dashboard" choice to the server, and read the settings block. */
async function afterSignIn({ recheck = true } = {}) {
  try { await accounts.migrateLocalHidden(); } catch {}
  // A sign-in needs the session's own answer; a page load has just read the same endpoint.
  if (recheck) { try { authCheck = await request('/api/auth/check'); } catch {} }
  if (currentPage === 'accounts') await accounts.loadAll();
}
async function checkSession(early = null) {
  if (sessionChecking) return;
  sessionChecking = true;
  try {
    if (early) await early.setup; else await loadAuthSetup();
    const status = early ? await early.check : await request('/api/auth/check');
    authCheck = status && typeof status === 'object' ? status : null;
    username = typeof status?.username === 'string' ? status.username : '';
    if (status.authenticated === true || status.authRequired === false) {
      auth(true, 'default');
      await loadSettings();
      // The dashboard and the update status render different state; their reads overlap.
      await Promise.all([refresh(), updateStatus()]);
      await afterSignIn({ recheck: false });
      if (currentPage === 'analytics') await refreshAnalytics();
    } else if (status.accessMode === 'setup') showSetup();
    else if (status.signedOutReason === 'revoked') sessionEnded('revoked');
    else {
      const reason = endedReason(signedInAt(globalThis.localStorage), sessionHours);
      if (reason) sessionEnded(reason); else auth(false, 'default');
    }
  } catch (error) {
    if (error?.network) goOffline();
    else auth(false, 'default', { message: 'Unable to connect to AI Account Center. Try refreshing this page.' });
  } finally { sessionChecking = false; }
}
async function signedIn(name) {
  username = name;
  rememberSignIn(globalThis.localStorage);
  setBusy(false);
  // Success: the button turns into a check, then the page cross-fades into the dashboard load-in.
  auth(false, 'success');
  await new Promise(resolve => setTimeout(resolve, motionReduced ? 120 : 650));
  auth(true, 'default');
  await enterDashboard();
  // 4.5.4: the last page opens (a page id in localStorage, no data). A URL that names a page wins.
  let last = '';
  try { last = globalThis.localStorage?.getItem('aac-last-page') || ''; } catch {}
  if (last && last !== currentPage && PAGES.includes(last) && pageFromLocation() === 'home') navigate(last);
}
let loginBridge = null;
/** The Slint sign-in value ("user\npassword\n1|0", auth-view.mjs parseLoginValue) for what the HTML form holds. */
const loginValue = filled => `${filled.username}\n${filled.password}\n${filled.remember ? '1' : '0'}`;
async function signIn(value) {
  if (busy) return;
  // While the login form's HTML inputs are on screen they hold what was typed or filled, so the Slint Sign in button
  // signs in with them too, even when a manager set the values without input events.
  const shown = loginBridge?.shown() === true;
  const { username: user, password, remember } = shown ? parseLoginValue(loginValue(loginBridge.read())) : parseLoginValue(value);
  if (!user || !password) { authNonce++; auth(false, 'default', { message: 'Enter your username and password.' }); return; }
  // the HTML form holds the same values, so managers offer to save them
  if (!shown) { try { loginBridge?.mirror({ username: user, password, remember }); } catch {} }
  auth(false, 'connecting'); setBusy(true);
  try {
    const result = await mutation('/api/auth/login', { username: user, password, rememberMe: remember });
    await signedIn(typeof result?.username === 'string' ? result.username : user);
  } catch (error) {
    setBusy(false);
    authNonce++;
    const window = limitWindowMinutes(error.headers);
    if (error.status === 429) {
      const limit = limitedView(retryFrom(error));
      auth(false, 'limited', { bannerTitle: limit.title, bannerBody: limit.body, bannerStrong: limit.until, retrySeconds: limit.seconds, limitSeconds: window * 60 });
    } else if (error.status === 401) {
      auth(false, 'wrong', { message: "Username or password isn't right.", messageSub: triesLine(triesFrom(error), window) });
    } else if (error.status === 400 && /not configured/i.test(error.payload?.error || '')) showSetup();
    else auth(false, 'default', { message: loginFailure(error) });
  }
}
/** The first-run form (only when the server offers POST /api/auth/setup). */
async function createSignIn(value) {
  if (busy || !setupInfo.form) return;
  const [user = '', password = '', confirm = '', code = ''] = String(value).split('\n');
  const bad = validateSetup({ username: user, password, confirm, code }, { codeRequired: setupInfo.codeRequired });
  if (bad) { authNonce++; showSetup({ field: bad[0], message: bad[1] }); return; }
  setBusy(true); showSetup();
  try {
    const result = await mutation('/api/auth/setup', { username: user.trim(), password, ...(setupInfo.codeRequired ? { setupCode: code.trim() } : {}) });
    await signedIn(typeof result?.username === 'string' ? result.username : user.trim());
  } catch (error) {
    setBusy(false);
    authNonce++;
    const code = error.payload?.code;
    if (code === 'already_configured') { await checkSession(); return; }
    const [field, message] = setupFailure(error, authCheck);
    showSetup({ field, message });
  }
}

// ---------------------------------------------------------------- actions from the UI
window.ccsDashboardAction = async (action, value) => {
  try {
    if (action === 'activation-cancel') { const agy = antigravityConfirmation.hasPending(); if ((agy ? antigravityConfirmation : activationConfirmation).cancel()) toast('info', 'Switch canceled', `Running ${agy ? 'Antigravity' : 'Codex'} programs were left unchanged.`); return; }
    if (action === 'activation-confirm') { if (authenticated) await (antigravityConfirmation.hasPending() ? antigravityConfirmation : activationConfirmation).confirm(); return; }
    if (action === 'navigate') { navigate(value); return; }
    if (action === 'navigate-dashboard') { navigate('home'); return; }
    if (action === 'navigate-analytics') { navigate('analytics'); return; }
    // The former Accounts and Settings dialogs are now one page.
    if (action === 'accounts' || action === 'settings') { navigate('accounts'); return; }
    if (action.startsWith('analytics-')) { if (authenticated) await analyticsAction(action, value); return; }
    if (action === 'theme') { saveTheme(value); return; }
    if (action === 'install-app') { await promptInstall(); return; }
    // One entry per Details session: a second row tapped while it is open swaps in place (D) or
    // re-targets the sheet (below D) without stacking history entries.
    if (action === 'details') { detailsOpens++; if (openDetailsId !== value) pushOverlayEntry(); renderDetails(value); return; }
    if (action === 'details-closed') { openDetailsId = ''; consumeOverlayEntry(); return; }
    // Sheets and popovers the Slint shell opened or closed (header menus, selects): the same
    // history contract as Details (3.9). Closing after a Back pop consumes nothing (depth 0).
    if (action === 'overlay-open') { pushOverlayEntry(); return; }
    if (action === 'overlay-close') { consumeOverlayEntry(); return; }
    if (action === 'login') { await signIn(value); return; }
    // the sign-in layer moved, showed or hid the login form's fields: the HTML inputs follow (login-bridge.mjs)
    if (action === 'login-overlay') {
      const overlay = JSON.parse(value);
      if (e2e) globalThis.__aacLoginOverlay = overlay;
      loginBridge?.place(overlay);
      return;
    }
    if (action === 'login-field-focus') { loginBridge?.focus(value); return; }
    if (action === 'setup') { await createSignIn(value); return; }
    if (action === 'setup-typing') {
      const [pass = '', confirm = ''] = String(value).split('\n');
      set_signin_strength(JSON.stringify({ ...strength(pass), matches: !!confirm && confirm === pass }));
      return;
    }
    if (action === 'auth-recheck') { if (!authenticated && !busy) await checkSession(); return; }
    if (action === 'login-limit-over') { if (!authenticated && !busy) auth(false, 'default', { notice: true, message: 'Sign-in is open again.' }); return; }
    if (action === 'probe-rect') {
      if (!e2e) return;
      const [id, x, y, w, h] = String(value).split('|');
      if (id) probeRects.set(id, { x: Number(x), y: Number(y), w: Number(w), h: Number(h) });
      return;
    }
    if (!authenticated) return;
    if (action === 'logout') { await signOut(); return; }
    // Accounts & Settings: sign-in and sign-out controls, show and hide, the password, trays and the network.
    // A change waits while an account-switch confirmation is pending, as every action here did before.
    if (MUTATING_ACTIONS.has(action) && pendingActivation()) { toast('info', 'Finish the account switch first', 'Confirm or cancel the switch, then try again.'); return; }
    if (await accounts.handle(action, value)) return;
    if (busy || pendingActivation()) return;
    if (action === 'refresh') { await refresh(true); if (currentPage === 'analytics') await refreshAnalytics(true); return; }
    if (action === 'launch') {
      const [id, target] = value.split(':');
      // the profile ids come from the server's inventory (GET /api/claude/desktop-profiles), never a fixed list
      const profile = profiles.find(row => row?.id === id);
      if (!profile || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(id) || !['mac', 'windows'].includes(target)) throw new Error('Unknown Claude profile.');
      const launcher = profile[target];
      if (target === 'windows' && launcher?.canOpen !== true) {
        if (platform === 'windows' && launcher?.launchUri === `ccs-claude://launch/${id}`) { location.href = launcher.launchUri; return; }
        throw new Error('Remote Windows launcher is unavailable.');
      }
      // An Open of this profile that is already running (here, in a tray or another browser) is followed, never sent again.
      if (claudeOpen.running(id) || openProgress(null, profiles).has(id)) { toast('info', 'Claude is already opening', 'Its progress shows on the row.'); return; }
      let outcome;
      setBusy(true);
      try { outcome = await claudeOpen.start(id, target); }
      finally { setBusy(false); }
      if (outcome === 'opened' && authenticated) toast('info', `Opening Claude on ${target === 'mac' ? 'Mac' : 'Windows'}`, 'It opens in its own desktop profile.');
      return;
    }
    if (action === 'activate') {
      refreshGeneration++;
      const account = data?.accounts?.find(row => row.capabilities?.codexProfile === value);
      if (!account || account.isActive) return;
      await activationConfirmation.begin(value); return;
    }
    if (action === 'antigravity-activate') {
      const account = antigravityModel().antigravityAccounts.find(row => row.profile === value);
      if (!account?.canActivate) { render(); failure('Ubuntu activation is unavailable until this login and the native runtime are verified.', 'Antigravity'); return; }
      refreshGeneration++;
      await antigravityConfirmation.begin(value); return;
    }
    if (['antigravity-automatic', 'antigravity-threshold', 'antigravity-pool', 'antigravity-cooldown'].includes(action)) {
      const model = antigravityModel();
      const seconds = Number(value);
      // the cooldown is part of the same existing PUT /api/antigravity/auto-switch (60 to 3600 seconds)
      const patch = action === 'antigravity-cooldown'
        ? (model.antigravitySettingsAvailable && Number.isInteger(seconds) && seconds >= 60 && seconds <= 3600 ? { cooldownSeconds: seconds } : null)
        : antigravitySettingsPatch(model, action, value);
      if (!patch) { render(); failure('Antigravity controls need verified Ubuntu support, two accounts and a fresh reported quota pool.', 'Antigravity'); return; }
      refreshGeneration++;
      setBusy(true);
      try {
        const status = await mutation('/api/antigravity/auto-switch', patch, 'PUT');
        if (!validAntigravityAuto(status)) throw new Error('Automatic switching settings could not be confirmed. Refresh before trying again.');
        antigravityAuto = status; render();
        if (action === 'antigravity-automatic') toast('info', status.enabled ? `Antigravity auto-switch on at ${status.thresholdUsedPercent}% used` : 'Antigravity auto-switch off',
          status.enabled ? 'The active login switches when its watched pool reaches the threshold.' : 'Logins only switch when you press Activate.');
      } finally { setBusy(false); }
      return;
    }
    if (action === 'automatic') {
      refreshGeneration++;
      setBusy(true);
      try {
        const status = await mutation('/api/codex/profiles/auto-switch', { enabled: value === 'true' }, 'PUT'); if (data) data.codexAutoSwitch = status; render();
        // The toast repeats what the server confirmed, not what was asked for.
        const on = status?.enabled === true, used = Number.isInteger(status?.thresholdPercent) ? 100 - status.thresholdPercent : null;
        toast('info', on ? `Codex auto-switch on${used !== null ? ` at ${used}% used` : ''}` : 'Codex auto-switch off',
          on ? 'The active account switches when its weekly or 5-hour window reaches the threshold.' : 'Accounts only switch when you press Activate.');
      }
      finally { setBusy(false); }
      return;
    }
    if (action === 'threshold') {
      refreshGeneration++;
      const used = Number(String(value).replace('%', ''));
      if (!Number.isInteger(used) || used < 1 || used > 99) return;
      setBusy(true);
      try { const status = await mutation('/api/codex/profiles/auto-switch', { thresholdPercent: 100 - used }, 'PUT'); if (data) data.codexAutoSwitch = status; render(); }
      finally { setBusy(false); }
      return;
    }
    if (action === 'refresh-interval') {
      const seconds = parseIntervalLabel(value) ?? Number(String(value).replace(/s$/, ''));
      if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
      setBusy(true);
      try { const result = await mutation('/api/accounts/settings', { refreshIntervalSeconds: seconds }, 'PUT'); applyRefreshInterval(result.refreshIntervalSeconds); toast('ok', 'Refresh interval saved', `Usage refreshes every ${intervalLabel(result.refreshIntervalSeconds)}.`); }
      finally { setBusy(false); }
      return;
    }
    if (action === 'update-apps') {
      const result = await mutation('/api/app-updates/start', {}); updateJob = result.job;
      renderUpdate(false); return;
    }
    if (action === 'update-cancel') {
      const result = await mutation('/api/app-updates/cancel', {}); updateJob = result.job;
      renderUpdate(false); return;
    }
  } catch (error) {
    if (data && (action === 'threshold' || action === 'automatic' || action.startsWith('antigravity-'))) render();
    if (action === 'refresh-interval') set_refresh_interval(refreshIntervalSeconds, refreshSettingsKnown);
    if (authenticated) failure(error?.message || 'Unable to complete the action.');
  }
};

// ---------------------------------------------------------------- device environment
// Safe-area insets (a probe element with env() paddings), the input profile (coarse pointer,
// hover, standalone display), the keyboard height covering the canvas and reachability.
// Pushed into the Device global; the tiers derive from them (DESIGN-MOBILE.md 1.1).
let safeProbe = null;
let keyboardFocus = false;
let onlineState = typeof navigator !== 'undefined' && navigator.onLine === false ? false : true;
function setOnline(value) {
  const next = value !== false;
  if (next === onlineState) return;
  onlineState = next;
  try { set_online(next); } catch {}
  // 4.6: everything returns to normal on the next successful API call - the status line first
  if (next && authenticated && data) renderChrome(false);
}
function pushSafeArea() {
  const insets = readSafeArea(safeProbe, (element) => getComputedStyle(element));
  try { set_safe_area(insets.top, insets.right, insets.bottom, insets.left); } catch {}
}
function pushInputProfile() {
  let coarse = false, hover = true, standalone = false;
  try {
    if (typeof matchMedia === 'function') {
      coarse = matchMedia('(pointer: coarse)').matches === true;
      hover = matchMedia('(hover: hover)').matches !== false;
      standalone = matchMedia('(display-mode: standalone)').matches === true;
    }
    if (typeof navigator !== 'undefined' && navigator.standalone === true) standalone = true;
  } catch {}
  try { set_input_profile(coarse, hover, standalone); } catch {}
  // Standalone hides the Settings "Home screen app" row (installRow); a display-mode
  // change (installed while open) re-renders it.
  installStandalone = standalone;
  renderAccounts();
}
function pushKeyboard() {
  let height = 0;
  try {
    const viewport = typeof visualViewport !== 'undefined' ? visualViewport : null;
    height = keyboardHeight(
      innerHeight,
      viewport ? { height: viewport.height, offsetTop: viewport.offsetTop ?? 0 } : null,
      keyboardFocus
    );
    // The document is fixed (index.html), so iOS must not pan it while a field has focus.
    if (keyboardFocus && viewport && viewport.offsetTop > 0) scrollTo(0, 0);
  } catch {}
  try { set_keyboard(height); } catch {}
}
function setupDevice() {
  try {
    safeProbe = document.createElement('div');
    safeProbe.setAttribute('aria-hidden', 'true');
    safeProbe.style.cssText = 'position:fixed;left:0;top:0;width:0;height:0;overflow:hidden;pointer-events:none;visibility:hidden;padding-top:env(safe-area-inset-top);padding-right:env(safe-area-inset-right);padding-bottom:env(safe-area-inset-bottom);padding-left:env(safe-area-inset-left);';
    document.body.appendChild(safeProbe);
  } catch { safeProbe = null; }
  pushSafeArea();
  pushInputProfile();
  pushKeyboard();
  try { set_online(onlineState); } catch {}
  addEventListener('resize', () => { pushSafeArea(); pushKeyboard(); });
  addEventListener('orientationchange', () => setTimeout(() => { pushSafeArea(); pushKeyboard(); }, 60));
  try {
    visualViewport?.addEventListener?.('resize', () => { pushSafeArea(); pushKeyboard(); });
    visualViewport?.addEventListener?.('scroll', pushKeyboard);
  } catch {}
  for (const query of ['(pointer: coarse)', '(hover: hover)', '(display-mode: standalone)']) {
    try { matchMedia(query)?.addEventListener?.('change', pushInputProfile); } catch {}
  }
  // The login form's real inputs and Slint's hidden text input both live in this document.
  try {
    document.addEventListener('focusin', (event) => {
      const target = event?.target;
      keyboardFocus = !!target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable === true);
      pushKeyboard();
    });
    document.addEventListener('focusout', () => { keyboardFocus = false; pushKeyboard(); });
  } catch {}
  try {
    addEventListener('online', () => { setOnline(true); retryOffline(); });
    addEventListener('offline', () => setOnline(false));
  } catch {}
  try {
    // 4.6: the offline card retries when the app comes back to the foreground
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') retryOffline(); });
  } catch {}
}

// ---------------------------------------------------------------- installable app (PWA)
let deferredInstallPrompt = null;
let installStandalone = false;
/** The Settings "Home screen app" row state (device.mjs installRow): 'chromium'|'ios'|'hidden'. */
function currentInstallRow() {
  let appleMobile = false;
  try {
    appleMobile = isAppleMobile(navigator?.userAgent, { touchPoints: navigator?.maxTouchPoints ?? 0 });
  } catch {}
  return installRow({ standalone: installStandalone, deferredPrompt: deferredInstallPrompt !== null, appleMobile });
}
/** The `install-app` action: show the captured install prompt once, then drop it. */
async function promptInstall() {
  const prompt = deferredInstallPrompt;
  deferredInstallPrompt = null;
  renderAccounts();
  if (!prompt) return;
  try { await prompt.prompt(); } catch {}
}
function setupInstall() {
  // The service worker caches the app shell for offline and fast start (sw.js); outside a
  // secure context, or when /sw.js is missing, registration fails and the dashboard works without it.
  try {
    if (typeof navigator !== 'undefined' && 'serviceWorker' in navigator) {
      navigator.serviceWorker.register('/sw.js').catch(() => {});
    }
  } catch {}
  try {
    // Chromium offers the install prompt; capturing it feeds the row's Install button (6.6).
    addEventListener('beforeinstallprompt', (event) => {
      event.preventDefault();
      deferredInstallPrompt = event;
      renderAccounts();
    });
    addEventListener('appinstalled', () => {
      deferredInstallPrompt = null;
      renderAccounts();
    });
  } catch {}
}

// ---------------------------------------------------------------- theme and motion
const THEMES = { auto: 0, light: 1, dark: 2 };
let themeModeName = 'auto';
let themeSystemDark = false;
/** The theme-color meta follows the resolved app theme and the current screen (device.mjs). */
function syncThemeColor() {
  try {
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', themeColor({ mode: themeModeName, systemDark: themeSystemDark, screen: themeScreen(authenticated) }));
  } catch {}
}
function storedTheme() {
  let theme = null;
  try {
    theme = localStorage.getItem('aac-theme');
    if (!theme) {
      // Older builds stored only light or dark.
      const legacy = localStorage.getItem('ccs-slint-theme');
      theme = legacy === 'light' || legacy === 'dark' ? legacy : null;
    }
  } catch {}
  return theme in THEMES ? theme : 'auto';
}
function saveTheme(value) {
  if (!(value in THEMES)) return;
  try { localStorage.setItem('aac-theme', value); } catch {}
  themeModeName = value;
  set_theme_mode(THEMES[value]);
  syncThemeColor();
}
function watchMedia(query, apply) {
  const media = typeof matchMedia === 'function' ? matchMedia(query) : null;
  apply(media?.matches === true);
  media?.addEventListener?.('change', event => apply(event.matches === true));
}

// ---------------------------------------------------------------- boot
try {
  requireWebGL();
  // Before Slint creates its first image: SVG marks must upload premultiplied (renderer.mjs).
  premultiplySvgTextureUploads();
  // The session answers while the wasm downloads and compiles; checkSession awaits both below.
  const earlySetup = loadAuthSetup();
  const earlyCheck = request('/api/auth/check');
  earlyCheck.catch(() => {});
  await init();
  startSlintDashboard(() => start_dashboard(innerWidth, innerHeight, devicePixelRatio));
  // The login form's real HTML inputs lie over the Slint fields (login-bridge.mjs): what is typed or filled lands
  // in the Slint fields too, their focus and hover reach the Slint boxes, and a submit runs the same login as Sign in.
  try {
    loginBridge = installLoginBridge({
      document,
      canvas: document.querySelector('#canvas'),
      onFilled: filled => {
        if (authenticated || busy) return;
        set_login_fields(filled.username, filled.password, filled.remember);
        // the first-run form is covered too: its confirmation and code mirror into Slint (set_login_setup)
        if (authState === 'setup' && setupInfo.form) {
          try { set_login_setup(filled.confirm ?? '', filled.code ?? ''); } catch {}
          set_signin_strength(JSON.stringify({ ...strength(filled.password), matches: !!filled.confirm && filled.confirm === filled.password }));
        }
      },
      onSubmit: filled => {
        if (authenticated || busy) return;
        if (authState === 'setup' && setupInfo.form) void createSignIn(`${filled.username}\n${filled.password}\n${filled.confirm ?? ''}\n${filled.code ?? ''}`);
        else void signIn(loginValue(filled));
      },
      onPointer: ({ focus, hover }) => { try { set_login_pointer(focus, hover); } catch {} },
    });
  } catch {}
  set_current_page(currentPage);
  const resize = () => resize_dashboard(innerWidth, innerHeight);
  addEventListener('resize', resize); resize();
  closeDetailsOnOutsideClicks(document.querySelector('#canvas'));
  addEventListener('popstate', () => {
    // Our own history.back() consuming a closed overlay's entry: nothing to answer.
    if (consumingOverlayEntry) { consumingOverlayEntry = false; return; }
    // A Back press over an open overlay closes the top-most one (dialog, Details, sheet, popover).
    if (overlayDepth > 0) {
      overlayDepth--;
      try { pop_overlay(); } catch {}
      return;
    }
    currentPage = pageFromLocation(); set_current_page(currentPage);
    if (currentPage === 'analytics' && authenticated) enterAnalytics();
    if (currentPage === 'accounts' && authenticated) { renderAccounts(); void accounts.loadAll(); }
  });
  themeModeName = storedTheme();
  set_theme_mode(THEMES[themeModeName]);
  // Auto follows the browser: the scheme is pushed now and on every change.
  watchMedia('(prefers-color-scheme: dark)', dark => { themeSystemDark = dark; set_system_dark(dark); syncThemeColor(); });
  setupDevice();
  setupInstall();
  syncThemeColor();
  // Headless captures settle instantly (the existing screenshot guard); ?motion keeps motion on.
  const headless = /HeadlessChrome/.test(navigator.userAgent) && !/[?&]motion\b/.test(location.search);
  watchMedia('(prefers-reduced-motion: reduce)', reduced => { motionReduced = reduced || headless; set_reduced_motion(motionReduced); });
  document.querySelector('#loading').hidden = true;
  auth(false, 'loading');
  await checkSession({ setup: earlySetup, check: earlyCheck });
  applyRefreshInterval(refreshIntervalSeconds, refreshSettingsKnown);
  setInterval(() => { activationConfirmation.expire(); antigravityConfirmation.expire(); }, 1_000);
  setInterval(() => { if (authenticated && updateJob?.state === 'running') void updateStatus(); }, 3_000);
  // "Updated 1m ago" and the reset countdowns stay current between refreshes.
  setInterval(() => { if (authenticated && !refreshing) { renderChrome(false); renderAnalyticsHead(); } }, 15_000);
  setInterval(() => { if (authenticated && !refreshing && !busy) render(); }, 60_000);
} catch (error) {
  const status = document.querySelector('#loading');
  status.hidden = false; status.textContent = error?.code === 'webgl_required' ? WEBGL_REQUIRED_MESSAGE : 'Unable to start the Slint dashboard. Reload to try again.';
  console.error('Slint dashboard failed to start.', error);
}
