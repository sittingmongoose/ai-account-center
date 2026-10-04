import init, { start_dashboard, resize_dashboard, set_dashboard, set_chrome, set_auth, set_busy, set_theme_mode, set_system_dark, set_reduced_motion, push_toast, show_details, close_details, set_update_status, show_activation_confirmation, close_activation_confirmation, set_analytics, set_analytics_loading, set_analytics_head, set_analytics_trend_paths, set_current_page, set_refresh_interval, set_accounts, set_signin_strength, set_accounts_strength, set_login_fields, probe_tick } from './pkg/ccs_account_dashboard.js';
import { dashboardViewModel, detailsViewModel, chromeView, updateViewModel, intervalLabel, parseIntervalLabel } from './view-model.mjs';
import { accountsViewModel, transportOf, transportNote } from './accounts-view.mjs';
import { strength, validateSetup, triesLine, limitWindowMinutes, limitedView, rememberSignIn, forgetSignIn, signedInAt, endedReason, expiredBanner, triesFrom, retryFrom, loginFailure, setupFailure, parseLoginValue } from './auth-view.mjs';
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
let analyticsModel = null;
let analyticsGeneration = 0;
// While the log scan runs behind the page (loading, or a cached snapshot with a refresh running),
// the page re-reads the server every few seconds so the new numbers land on their own.
let analyticsPollTimer = 0;
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
function auth(signedIn, state, extra = {}) {
  authenticated = signedIn;
  authState = state;
  const view = {
    state, host, username, transportNote: transportNote(transport, authCheck), sessionHours, nonce: authNonce,
    setupForm: setupInfo.form, codeRequired: setupInfo.codeRequired, ...extra,
  };
  // the e2e harness reads the words it handed Slint next to the pixels it sees (?e2e only)
  if (e2e) globalThis.__aacLastAuth = { signedIn, ...view };
  set_auth(signedIn, JSON.stringify(view));
}

/** One request: { status, payload } on success; a refusal throws with its status, payload and headers. */
async function send(path, options = {}) {
  let response;
  try {
    response = await fetch(path, { credentials: 'same-origin', ...options, headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
  } catch (cause) {
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
function render() {
  if (!data) return;
  set_dashboard(JSON.stringify(dashboardViewModel(data, context({ refreshing: false }))));
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
    if (e2e) globalThis.__aacLastAccounts = vm;
    set_accounts(JSON.stringify(vm));
  } catch (error) { console.error('Accounts & Settings could not be drawn.', error); }
}
function renderChrome(isRefreshing = false) {
  if (data || isRefreshing) set_chrome(JSON.stringify(chromeView(data, { refreshing: isRefreshing, intervalSeconds: refreshIntervalSeconds, username, host })));
}
function renderDetails(id) {
  const view = data ? detailsViewModel(data, id, context({ refreshing: false })) : null;
  if (!view) { openDetailsId = ''; close_details(); return; }
  openDetailsId = id;
  show_details(JSON.stringify(view));
}
function pendingActivation() { return activationConfirmation.hasPending() || antigravityConfirmation.hasPending(); }
/** The slide-over's left edge: SlideOver.panel-w in ui/components/slide-over.slint is clamp(30%, 420, 580) px. */
function detailsPanelLeft() { return innerWidth - Math.min(580, Math.max(420, innerWidth * 0.3)); }
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
  prompt: confirmation => show_activation_confirmation(JSON.stringify({ ...confirmation, expiresAt: `Review valid until ${new Date(confirmation.expiresAt).toLocaleString()}` })),
  close: close_activation_confirmation,
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
  prompt: confirmation => show_activation_confirmation(JSON.stringify({ ...confirmation, expiresAt: `Review valid until ${new Date(confirmation.expiresAt).toLocaleString()}` })),
  close: close_activation_confirmation,
  busy: inProgress => setBusy(inProgress),
  success: async result => {
    // Selection/running proof comes from the next inventory, never an optimistic UI guess.
    toast('ok', 'Antigravity switched on Ubuntu', `${result?.email || 'The selected login'} was activated. Checking the native identity now.`);
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
/** mode 'morph' animates the usage trend from the shape on screen to the new one (range, filter and toggles). */
function renderAnalytics(mode = 'static') {
  if (!analyticsPayload) return;
  const now = Date.now();
  analyticsModel = analyticsView(analyticsPayload, { catalog: data?.accounts || [], metricKey: analyticsSelection.metricKey, activityInterval: analyticsSelection.activityInterval }, now);
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
  const slintView = analyticsSlintModel(analyticsModel, { usage, quota, agenda, state: analyticsPage, paths: trendPaths(morph ? from : next) });
  // e2e only (?e2e): the last Analytics view handed to Slint, so a harness can read its words
  if (e2e) globalThis.__aacLastAnalytics = slintView;
  set_analytics(JSON.stringify(slintView));
  clearTimeout(analyticsPollTimer);
  const act = analyticsPayload?.activity;
  if (currentPage === 'analytics' && (act?.status === 'loading' || act?.refreshing === true))
    analyticsPollTimer = setTimeout(() => { if (currentPage === 'analytics') void refreshAnalytics(); }, 5000);
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
  try { set_analytics_head(JSON.stringify(usageHead(analyticsPayload, analyticsPage, { now: Date.now() }))); } catch {}
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
async function refreshAnalytics(force = false, mode = 'static') {
  if (!authenticated) return;
  const generation = ++analyticsGeneration;
  set_analytics_loading(true, '');
  // the backend accepts 24h, 7d and 30d: Month, All and custom ranges read the covering window and are cut here
  const query = new URLSearchParams({ platform, range: apiRangeFor(analyticsPage, Date.now()), provider: 'all', account: 'all' });
  if (force) query.set('refresh', 'true');
  try {
    const result = await request(`/api/accounts/analytics?${query}`);
    if (generation !== analyticsGeneration || !authenticated) return;
    if (result?.schemaVersion !== 1 || !Array.isArray(result.accounts)) throw new Error('Unsupported analytics response.');
    analyticsPayload = result; renderAnalytics(mode); set_analytics_loading(false, '');
  } catch (error) { if (generation === analyticsGeneration) { renderAnalytics(); set_analytics_loading(false, error?.message || 'Unable to load account analytics.'); } }
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
    analyticsSelection.metricKey = analyticsModel?.choices?.metrics?.some(row => row.id === value) ? value : '';
    renderAnalytics(); return;
  }
  if (action === 'analytics-account-id' || action === 'analytics-account' || action === 'analytics-metric') {
    const kind = action === 'analytics-account-id' ? 'account' : action.slice('analytics-'.length);
    const id = action === 'analytics-account-id' ? (analyticsModel?.choices?.accounts?.some(row => row.id === value) ? value : null) : analyticsChoiceId(analyticsModel, kind, value);
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
  usageTimer = setInterval(() => { if (authenticated && !busy && !pendingActivation()) { void refresh(); if (currentPage === 'analytics') void refreshAnalytics(); } }, seconds * 1000);
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
      const skippedN = count('skipped'), unknownN = count('unknown');
      toast(failed ? 'err' : 'ok', failed ? 'App update finished with failures' : 'Apps updated', `${count('updated')} updated, ${count('current')} already current, ${count('not_installed')} not installed${failed ? `, ${failed} failed` : ''}${skippedN ? `, ${skippedN} skipped` : ''}${unknownN ? `, ${unknownN} unknown` : ''}. Details under Accounts & Settings.`, 7000);
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
  currentPage = page;
  set_current_page(page);
  const url = pagePath(page);
  try { globalThis.history?.[replace ? 'replaceState' : 'pushState']?.(null, '', url); } catch {}
  if (page === 'analytics' && authenticated) void refreshAnalytics();
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
  analyticsGeneration++; analyticsPayload = null; analyticsModel = null; clearTimeout(analyticsPollTimer); data = null; serverData = null; profiles = []; antigravityInventory = null; antigravityAuto = null; refreshGeneration++; openDetailsId = '';
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
async function afterSignIn() {
  try { await accounts.migrateLocalHidden(); } catch {}
  try { authCheck = await request('/api/auth/check'); } catch {}
  if (currentPage === 'accounts') await accounts.loadAll();
}
async function checkSession(early = null) {
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
      await afterSignIn();
      if (currentPage === 'analytics') await refreshAnalytics();
    } else if (status.accessMode === 'setup') showSetup();
    else if (status.signedOutReason === 'revoked') sessionEnded('revoked');
    else {
      const reason = endedReason(signedInAt(globalThis.localStorage), sessionHours);
      if (reason) sessionEnded(reason); else auth(false, 'default');
    }
  } catch { auth(false, 'default', { message: 'Unable to connect to AI Account Center. Try refreshing this page.' }); }
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
}
let loginBridge = null;
async function signIn(value) {
  if (busy) return;
  const { username: user, password, remember } = parseLoginValue(value);
  if (!user || !password) { authNonce++; auth(false, 'default', { message: 'Enter your username and password.' }); return; }
  // the hidden form holds the same values, so managers offer to save them
  try { loginBridge?.mirror({ username: user, password, remember }); } catch {}
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
    if (action === 'details') { detailsOpens++; renderDetails(value); return; }
    if (action === 'details-closed') { openDetailsId = ''; return; }
    if (action === 'login') { await signIn(value); return; }
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

// ---------------------------------------------------------------- theme and motion
const THEMES = { auto: 0, light: 1, dark: 2 };
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
  set_theme_mode(THEMES[value]);
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
  // password managers fill the hidden HTML form; its values land in the Slint fields, and its
  // submit runs the same login as Sign in
  try {
    loginBridge = installLoginBridge({
      document,
      onFilled: filled => { if (!authenticated && !busy) set_login_fields(filled.username, filled.password, filled.remember); },
      onSubmit: filled => { if (!authenticated && !busy) void signIn(`${filled.username}\n${filled.password}\n${filled.remember ? '1' : '0'}`); },
    });
  } catch {}
  set_current_page(currentPage);
  const resize = () => resize_dashboard(innerWidth, innerHeight);
  addEventListener('resize', resize); resize();
  closeDetailsOnOutsideClicks(document.querySelector('#canvas'));
  addEventListener('popstate', () => {
    currentPage = pageFromLocation(); set_current_page(currentPage);
    if (currentPage === 'analytics' && authenticated) void refreshAnalytics();
    if (currentPage === 'accounts' && authenticated) { renderAccounts(); void accounts.loadAll(); }
  });
  set_theme_mode(THEMES[storedTheme()]);
  // Auto follows the browser: the scheme is pushed now and on every change.
  watchMedia('(prefers-color-scheme: dark)', dark => set_system_dark(dark));
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
