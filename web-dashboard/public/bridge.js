import init, { start_dashboard, resize_dashboard, set_dashboard, set_chrome, set_auth, set_busy, set_theme_mode, set_system_dark, set_reduced_motion, push_toast, show_details, close_details, set_update_status, show_activation_confirmation, close_activation_confirmation, set_analytics, set_analytics_loading, set_analytics_head, set_analytics_trend_paths, set_current_page, set_refresh_interval, set_accounts, set_signin_strength } from './pkg/ccs_account_dashboard.js';
import { dashboardViewModel, detailsViewModel, chromeView, updateViewModel, intervalLabel, parseIntervalLabel, hiddenProviders, PROVIDER_REGISTRY } from './view-model.mjs';
import { accountsViewModel, transportOf, transportNote } from './accounts-view.mjs';
import { strength, validateSetup, triesLeft, triesLine, limitWindowMinutes, retrySeconds, limitedView, rememberSignIn, forgetSignIn, signedInAt, endedReason, expiredBanner } from './auth-view.mjs';
import { createActivationConfirmation } from './activation-confirmation.mjs';
import { antigravityView, antigravitySettingsPatch, validAntigravityAuto } from './antigravity-data.mjs';
import { createAntigravityConfirmation } from './antigravity-confirmation.mjs';
import { analyticsView, analyticsChoiceId, analyticsSlintModel } from './analytics-data.mjs';
import { usageView, apiRangeFor, trendPaths, mixGeo, parseIsoDay, addDays, RANGES, H, D } from './analytics-usage.mjs';
import { quotaView, agendaView, QUOTA_PROVIDERS } from './analytics-quota.mjs';
import { requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard } from './renderer.mjs';
import { createClaudeOpen, openProgress } from './claude-open.mjs';
import { PAGES, pageFromUrl, pagePath } from './page-route.mjs';

// The browser bridge: network, session, timers and every truthfulness rule stay in JavaScript
// (public/*.mjs); the Slint UI receives version 2 view-model JSON and reports intent through
// window.ccsDashboardAction(kind, value). See web-dashboard/ui/README-ARCHITECTURE.md.
let authenticated = false;
let busy = false;
// `serverData` is the dashboard response as received; `data` is the same with this browser's hidden providers
// added to settings.hiddenProviders, so Home, Details and Analytics honour a "Show on dashboard" choice.
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
let openDetailsId = '';
// Counts Details opens, so a click outside the panel can tell a row click (which opens that row) from any other.
let detailsOpens = 0;
// Sign-in: the session length from GET /api/auth/setup, and what the first-run form may do.
let sessionHours = 24;
let setupInfo = { form: false, codeRequired: false };
let authNonce = 0;
const transport = transportOf(location.protocol, location.hostname);
const origin = typeof location.origin === 'string' ? location.origin : '';
// "Show on dashboard": saved in this browser until the server stores visibility (CONTRACT-registry-lifecycle 4).
const HIDDEN_KEY = 'aac-hidden-providers';
function storedHidden() {
  try {
    const list = JSON.parse(localStorage.getItem(HIDDEN_KEY) || '[]');
    return new Set(Array.isArray(list) ? list.filter(id => PROVIDER_REGISTRY.some(row => row.id === id)) : []);
  } catch { return new Set(); }
}
let localHidden = storedHidden();
function withLocalHidden(next) {
  if (!next) return next;
  const hidden = new Set([...hiddenProviders(next), ...localHidden]);
  return { ...next, settings: { ...(next.settings || {}), hiddenProviders: [...hidden] } };
}
/** URL state: the page routes /, /analytics and /accounts (page-route.mjs); ?view= still works as an alias. */
function pageFromLocation() { return pageFromUrl(location.pathname, location.search); }
let currentPage = pageFromLocation();
const analyticsSelection = { range: '7d', provider: 'all', account: 'all', metricKey: '', activityInterval: 'Daily' };
// The Analytics page state (version 3). Every number and label is computed in analytics-*.mjs from the response.
const analyticsPage = {
  range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost',
  open: new Set(), compare: new Set(), collapsed: new Set(),
  // chart boxes reported by the Slint layout (analytics-layout): the charts are laid out in these pixels
  sizes: { trend: null, daily: null, heat: null, focus: null },
};
let motionReduced = false;
const platform = /Windows/i.test(navigator.userAgent) ? 'windows' : 'mac';
const host = typeof location.host === 'string' ? location.host : '';

// ---------------------------------------------------------------- feedback
function setBusy(value) { busy = value; set_busy(value); }
/** Results and failures appear as toasts on any page, never buried at the bottom. */
function toast(kind, title, body = '', ms = 4800) { push_toast(kind, title, body, ms); }
function failure(message, title = 'That did not work') { toast('err', title, message, 6400); }
let authState = 'loading';
function auth(signedIn, state, extra = {}) {
  authenticated = signedIn;
  authState = state;
  set_auth(signedIn, JSON.stringify({
    state, host, username, transportNote: transportNote(transport), sessionHours, nonce: authNonce,
    setupForm: setupInfo.form, codeRequired: setupInfo.codeRequired, ...extra,
  }));
}

/** One request: { status, payload } on success; a refusal throws with its status, payload and headers. */
async function send(path, options = {}) {
  const response = await fetch(path, { credentials: 'same-origin', ...options, headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    if (response.status === 401 && path !== '/api/auth/login' && authenticated) sessionEnded();
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
  try {
    set_accounts(JSON.stringify(accountsViewModel(serverData, context({
      refreshing: false, serverHidden: hiddenProviders(serverData), localHidden,
      refreshSeconds: refreshIntervalSeconds, refreshKnown: refreshSettingsKnown, updateJob,
      origin, transport, sessionHours, signedInAt: signedInAt(globalThis.localStorage),
    }))));
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

async function refresh(force = false) {
  if (!authenticated) return;
  if (refreshing && !force) return refreshing;
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
      data = withLocalHidden(next);
      if (Number.isInteger(next.settings?.refreshIntervalSeconds)) applyRefreshInterval(next.settings.refreshIntervalSeconds);
      profiles = Array.isArray(inventory?.profiles) ? inventory.profiles : profiles;
      antigravityInventory = antigravityProfiles;
      antigravityAuto = validAntigravityAuto(antigravityStatus) ? antigravityStatus : validAntigravityAuto(next.antigravityAutoSwitch) ? next.antigravityAutoSwitch : null;
      render();
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
  const quota = quotaView(analyticsPayload, ctx);
  const agenda = agendaView(analyticsPayload, ctx);
  const next = usage.trend.geo, size = `${usage.trend.pw}x${usage.trend.ph}`;
  const from = shownGeo;
  const morph = mode === 'morph' && !motionReduced && from && shownSize === size && from.lv[0].length === next.lv[0].length;
  cancelAnimationFrame(trendRaf);
  set_analytics(JSON.stringify(analyticsSlintModel(analyticsModel, { usage, quota, agenda, state: analyticsPage, paths: trendPaths(morph ? from : next) })));
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
  try { set_analytics_head(JSON.stringify(usageView(analyticsPayload, analyticsPage, { sizes: analyticsPage.sizes }).head)); } catch {}
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
  if (action === 'analytics-provider' && ['all', 'claude', 'codex'].includes(value)) {
    if (analyticsPage.prov === value) return;
    analyticsPage.prov = value; renderAnalytics('morph'); return;
  }
  if (action === 'analytics-split') { analyticsPage.split = value === 'true'; renderAnalytics('morph'); return; }
  if (action === 'analytics-cache') { analyticsPage.cache = value === 'true'; renderAnalytics('morph'); return; }
  if (action === 'analytics-donut') { if (['tokens', 'cost'].includes(value)) { analyticsPage.donut = value; renderAnalytics(); } return; }
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
  try { const result = await request('/api/accounts/settings'); applyRefreshInterval(result?.refreshIntervalSeconds); } catch {}
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
      toast(failed ? 'err' : 'ok', failed ? 'App update finished with failures' : 'Apps updated', `${count('updated')} updated, ${count('current')} already current, ${count('not_installed')} not installed${failed ? `, ${failed} failed` : ''}. Details under Accounts & Settings.`, 7000);
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
  if (page === 'accounts' && authenticated) renderAccounts();
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
/** The server no longer knows this browser's session: say whether it ran out or ended early. */
function sessionEnded(reason = endedReason(signedInAt(globalThis.localStorage), sessionHours) || 'ended') {
  forgetSignIn(globalThis.localStorage);
  claudeOpen.reset();
  const banner = expiredBanner(reason, sessionHours);
  auth(false, 'expired', { bannerTitle: banner.title, bannerBody: banner.body });
}
async function enterDashboard() {
  await loadSettings(); await refresh(true); await updateStatus();
  if (currentPage === 'analytics') await refreshAnalytics();
}
async function checkSession() {
  try {
    await loadAuthSetup();
    const status = await request('/api/auth/check');
    username = typeof status?.username === 'string' ? status.username : '';
    if (status.authenticated === true || status.authRequired === false) {
      auth(true, 'default');
      await loadSettings(); await refresh(); await updateStatus();
      if (currentPage === 'analytics') await refreshAnalytics();
    } else if (status.accessMode === 'setup') showSetup();
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
async function signIn(value) {
  if (busy) return;
  const separator = value.indexOf('\n');
  const user = value.slice(0, separator).trim(), password = value.slice(separator + 1);
  if (!user || !password) { authNonce++; auth(false, 'default', { message: 'Enter your username and password.' }); return; }
  auth(false, 'connecting'); setBusy(true);
  try {
    const result = await mutation('/api/auth/login', { username: user, password });
    await signedIn(typeof result?.username === 'string' ? result.username : user);
  } catch (error) {
    setBusy(false);
    authNonce++;
    const window = limitWindowMinutes(error.headers);
    if (error.status === 429) {
      const limit = limitedView(retrySeconds(error.headers));
      auth(false, 'limited', { bannerTitle: limit.title, bannerBody: limit.body, bannerStrong: limit.until, retrySeconds: limit.seconds, limitSeconds: window * 60 });
    } else if (error.status === 401) {
      auth(false, 'wrong', { message: "Username or password isn't right.", messageSub: triesLine(triesLeft(error.headers), window) });
    } else if (error.status === 400 && /not configured/i.test(error.message)) showSetup();
    else auth(false, 'default', { message: error.message || 'Sign-in failed.' });
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
    if (code === 'setup_code_required' || code === 'setup_code_invalid') { showSetup({ field: 'code', message: "That setup code isn't right. It has 8 letters and digits." }); return; }
    if (code === 'secure_transport_required') { showSetup({ message: 'Setting up from another computer needs HTTPS or an encrypted tunnel. Use the dashboard host itself, or run ai-account-center dashboard auth setup there.' }); return; }
    showSetup({ message: error.message || 'The sign-in could not be created.' });
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
    if (action === 'accounts-show') {
      if (!authenticated) return;
      const [id, mode] = String(value).split(':');
      const entry = PROVIDER_REGISTRY.find(row => row.id === id);
      if (!entry || !['show', 'hide'].includes(mode)) return;
      if (mode === 'hide') localHidden.add(id); else localHidden.delete(id);
      try { localStorage.setItem(HIDDEN_KEY, JSON.stringify([...localHidden])); } catch {}
      data = withLocalHidden(serverData);
      render();
      toast('info', mode === 'hide' ? `${entry.label} hidden from Home` : `${entry.label} shown on Home`,
        'Saved in this browser. The trays and other browsers follow once the server stores it.');
      return;
    }
    if (!authenticated || busy || pendingActivation()) return;
    if (action === 'logout') {
      await mutation('/api/auth/logout', {});
      forgetSignIn(globalThis.localStorage);
      analyticsGeneration++; analyticsPayload = null; analyticsModel = null; data = null; serverData = null; profiles = []; antigravityInventory = null; antigravityAuto = null; refreshGeneration++; openDetailsId = '';
      claudeOpen.reset();
      auth(false, 'default', { notice: true, message: 'Signed out.' }); return;
    }
    if (action === 'refresh') { await refresh(true); if (currentPage === 'analytics') await refreshAnalytics(true); return; }
    if (action === 'launch') {
      const [id, target] = value.split(':');
      if (!['platyr', 'gmail', 'party', 'me'].includes(id) || !['mac', 'windows'].includes(target)) throw new Error('Unknown Claude profile.');
      const launcher = profiles.find(row => row.id === id)?.[target];
      if (target === 'windows' && launcher?.canOpen !== true) {
        if (platform === 'windows' && /^ccs-claude:\/\/launch\/(platyr|gmail|party|me)$/.test(launcher?.launchUri || '')) { location.href = launcher.launchUri; return; }
        throw new Error('Remote Windows launcher is unavailable.');
      }
      // An Open of this profile that is already running (here, in a tray or another browser) is followed, never sent again.
      if (claudeOpen.running(id) || openProgress(null, profiles).has(id)) { toast('info', 'Claude is already opening', 'Its progress shows on the row.'); return; }
      let outcome;
      setBusy(true);
      try { outcome = await claudeOpen.start(id, target); }
      finally { setBusy(false); }
      if (outcome === 'opened') toast('info', `Opening Claude on ${target === 'mac' ? 'Mac' : 'Windows'}`, 'It opens in its own desktop profile.');
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
  await init();
  startSlintDashboard(() => start_dashboard(innerWidth, innerHeight, devicePixelRatio));
  set_current_page(currentPage);
  const resize = () => resize_dashboard(innerWidth, innerHeight);
  addEventListener('resize', resize); resize();
  closeDetailsOnOutsideClicks(document.querySelector('#canvas'));
  addEventListener('popstate', () => {
    currentPage = pageFromLocation(); set_current_page(currentPage);
    if (currentPage === 'analytics' && authenticated) void refreshAnalytics();
    if (currentPage === 'accounts' && authenticated) renderAccounts();
  });
  set_theme_mode(THEMES[storedTheme()]);
  // Auto follows the browser: the scheme is pushed now and on every change.
  watchMedia('(prefers-color-scheme: dark)', dark => set_system_dark(dark));
  // Headless captures settle instantly (the existing screenshot guard); ?motion keeps motion on.
  const headless = /HeadlessChrome/.test(navigator.userAgent) && !/[?&]motion\b/.test(location.search);
  watchMedia('(prefers-reduced-motion: reduce)', reduced => { motionReduced = reduced || headless; set_reduced_motion(motionReduced); });
  document.querySelector('#loading').hidden = true;
  auth(false, 'loading');
  await checkSession();
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
