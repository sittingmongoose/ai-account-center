import init, { start_dashboard, resize_dashboard, set_dashboard, set_chrome, set_auth, set_busy, set_theme_mode, set_system_dark, set_reduced_motion, push_toast, show_details, close_details, set_update_status, show_activation_confirmation, close_activation_confirmation, set_analytics, set_analytics_loading, set_current_page, set_refresh_interval } from './pkg/ccs_account_dashboard.js';
import { dashboardViewModel, detailsViewModel, chromeView, updateViewModel, intervalLabel, parseIntervalLabel } from './view-model.mjs';
import { createActivationConfirmation } from './activation-confirmation.mjs';
import { antigravityView, antigravitySettingsPatch, validAntigravityAuto } from './antigravity-data.mjs';
import { createAntigravityConfirmation } from './antigravity-confirmation.mjs';
import { analyticsView, analyticsChoiceId, analyticsSlintModel } from './analytics-data.mjs';
import { requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard } from './renderer.mjs';

// The browser bridge: network, session, timers and every truthfulness rule stay in JavaScript
// (public/*.mjs); the Slint UI receives version 2 view-model JSON and reports intent through
// window.ccsDashboardAction(kind, value). See web-dashboard/ui/README-ARCHITECTURE.md.
let authenticated = false;
let busy = false;
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
const PAGES = ['home', 'analytics', 'accounts'];
/** URL state: ?view=home|analytics|accounts. The legacy /analytics path and ?view=analytics still work. */
function pageFromLocation() {
  const view = new URLSearchParams(location.search).get('view');
  if (location.pathname === '/analytics' || view === 'analytics') return 'analytics';
  return PAGES.includes(view) ? view : 'home';
}
let currentPage = pageFromLocation();
const analyticsSelection = { range: '7d', provider: 'all', account: 'all', metricKey: '', activityInterval: 'Daily' };
const platform = /Windows/i.test(navigator.userAgent) ? 'windows' : 'mac';
const host = typeof location.host === 'string' ? location.host : '';

// ---------------------------------------------------------------- feedback
function setBusy(value) { busy = value; set_busy(value); }
/** Results and failures appear as toasts on any page, never buried at the bottom. */
function toast(kind, title, body = '', ms = 4800) { push_toast(kind, title, body, ms); }
function failure(message, title = 'That did not work') { toast('err', title, message, 6400); }
function auth(signedIn, state, extra = {}) { authenticated = signedIn; set_auth(signedIn, JSON.stringify({ state, host, username, ...extra })); }

async function request(path, options = {}) {
  const response = await fetch(path, { credentials: 'same-origin', ...options, headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    if (response.status === 401 && path !== '/api/auth/login' && authenticated) auth(false, 'expired', { message: 'Your session ended. Sign in again.' });
    const error = new Error(payload?.error || `Request failed (${response.status}).`);
    error.status = response.status;
    error.payload = payload;
    error.retryAfter = response.headers?.get?.('Retry-After') || response.headers?.get?.('RateLimit-Reset') || '';
    throw error;
  }
  return payload;
}
const mutation = (path, body, method = 'POST') => request(path, { method, body: JSON.stringify(body) });

// ---------------------------------------------------------------- rendering
function context(extra = {}) {
  return { profiles, platform, antigravityInventory, antigravityAuto, refreshing: !!refreshing && extra.refreshing !== false, intervalSeconds: refreshIntervalSeconds, username, host, ...extra };
}
function antigravityModel() { return antigravityView(data, antigravityInventory, antigravityAuto); }
function render() {
  if (!data) return;
  set_dashboard(JSON.stringify(dashboardViewModel(data, context({ refreshing: false }))));
  if (openDetailsId) renderDetails(openDetailsId);
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
      data = next;
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
function renderAnalytics() {
  if (!analyticsPayload) return;
  analyticsModel = analyticsView(analyticsPayload, { catalog: data?.accounts || [], metricKey: analyticsSelection.metricKey, activityInterval: analyticsSelection.activityInterval });
  set_analytics(JSON.stringify(analyticsSlintModel(analyticsModel)));
}
async function refreshAnalytics(force = false) {
  if (!authenticated) return;
  const generation = ++analyticsGeneration;
  set_analytics_loading(true, '');
  const query = new URLSearchParams({ platform, range: analyticsSelection.range, provider: analyticsSelection.provider, account: analyticsSelection.account });
  if (force) query.set('refresh', 'true');
  try {
    const result = await request(`/api/accounts/analytics?${query}`);
    if (generation !== analyticsGeneration || !authenticated) return;
    if (result?.schemaVersion !== 1 || !Array.isArray(result.accounts)) throw new Error('Unsupported analytics response.');
    analyticsPayload = result; renderAnalytics(); set_analytics_loading(false, '');
  } catch (error) { if (generation === analyticsGeneration) { renderAnalytics(); set_analytics_loading(false, error?.message || 'Unable to load account analytics.'); } }
}
async function analyticsAction(action, value) {
  if (action === 'analytics-activity-interval') {
    if (!['Daily', 'Hourly'].includes(value)) return;
    analyticsSelection.activityInterval = value; renderAnalytics(); return;
  }
  if (action === 'analytics-metric-key') {
    // A quota-history row's focus chart: the exact history key, or nothing.
    analyticsSelection.metricKey = analyticsModel?.choices?.metrics?.some(row => row.id === value) ? value : '';
    renderAnalytics(); return;
  }
  if (action === 'analytics-account-id') {
    if (!analyticsModel?.choices?.accounts?.some(row => row.id === value)) return;
    analyticsSelection.account = value; analyticsSelection.metricKey = '';
    await refreshAnalytics(); return;
  }
  if (action === 'analytics-range') {
    const label = String(value).toLowerCase();
    const range = label.includes('24') ? '24h' : label.includes('30') ? '30d' : label.includes('7') ? '7d' : null;
    if (!range) return;
    analyticsSelection.range = range;
  } else if (action === 'analytics-provider' || action === 'analytics-account' || action === 'analytics-metric') {
    const kind = action.slice('analytics-'.length);
    const id = analyticsChoiceId(analyticsModel, kind, value);
    if (id === null) return;
    if (kind === 'metric') { analyticsSelection.metricKey = id; renderAnalytics(); return; }
    analyticsSelection[kind] = id;
    if (kind === 'provider') analyticsSelection.account = 'all';
    analyticsSelection.metricKey = '';
  }
  if (action === 'analytics-refresh') await refresh(true);
  await refreshAnalytics(action === 'analytics-refresh');
}

// ---------------------------------------------------------------- settings, updates, session
function applyRefreshInterval(seconds, confirmed = true) {
  if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
  refreshIntervalSeconds = seconds;
  refreshSettingsKnown = confirmed;
  set_refresh_interval(seconds, confirmed);
  if (usageTimer) clearInterval(usageTimer);
  usageTimer = setInterval(() => { if (authenticated && !busy && !pendingActivation()) { void refresh(); if (currentPage === 'analytics') void refreshAnalytics(); } }, seconds * 1000);
}
async function loadSettings() {
  try { const result = await request('/api/accounts/settings'); applyRefreshInterval(result?.refreshIntervalSeconds); } catch {}
}
function renderUpdate(done = false) { set_update_status(JSON.stringify(updateViewModel(updateJob, { done }))); }
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
  const url = page === 'home' ? location.pathname === '/analytics' ? '/' : location.pathname : `${location.pathname === '/analytics' ? '/' : location.pathname}?view=${page}`;
  try { globalThis.history?.[replace ? 'replaceState' : 'pushState']?.(null, '', url); } catch {}
  if (page === 'analytics' && authenticated) void refreshAnalytics();
}
async function checkSession() {
  try {
    const status = await request('/api/auth/check');
    username = typeof status?.username === 'string' ? status.username : '';
    if (status.authenticated === true || status.authRequired === false) {
      auth(true, 'default');
      await loadSettings(); await refresh(); await updateStatus();
      if (currentPage === 'analytics') await refreshAnalytics();
    } else auth(false, status.accessMode === 'setup' ? 'setup' : 'default');
  } catch { auth(false, 'default', { message: 'Unable to connect to AI Account Center. Try refreshing this page.' }); }
}
async function signIn(value) {
  if (busy) return;
  const separator = value.indexOf('\n');
  const user = value.slice(0, separator), password = value.slice(separator + 1);
  if (!user || !password) { auth(false, 'wrong', { message: 'Enter the username and the password.' }); return; }
  auth(false, 'connecting'); setBusy(true);
  try {
    const result = await mutation('/api/auth/login', { username: user, password });
    username = typeof result?.username === 'string' ? result.username : user;
    setBusy(false);
    // Success: the sign-in layer shows "Signed in", then cross-fades into the dashboard load-in.
    auth(false, 'success');
    await new Promise(resolve => setTimeout(resolve, 450));
    auth(true, 'default');
    await loadSettings(); await refresh(true); await updateStatus();
    if (currentPage === 'analytics') await refreshAnalytics();
  } catch (error) {
    setBusy(false);
    if (error.status === 429) auth(false, 'limited', { retry: retryLabel(error.retryAfter) });
    else if (error.status === 401) auth(false, 'wrong', { message: "That username and password don't match." });
    else if (error.status === 400 && /not configured/i.test(error.message)) auth(false, 'setup');
    else auth(false, 'default', { message: error.message || 'Sign-in failed.' });
  }
}
function retryLabel(value) {
  const seconds = Number(value);
  if (!Number.isFinite(seconds) || seconds <= 0) return '';
  return seconds >= 60 ? `${Math.ceil(seconds / 60)} minutes` : `${Math.ceil(seconds)} seconds`;
}

// ---------------------------------------------------------------- actions from the UI
window.ccsDashboardAction = async (action, value) => {
  try {
    if (action === 'activation-cancel') { const agy = antigravityConfirmation.hasPending(); if ((agy ? antigravityConfirmation : activationConfirmation).cancel()) toast('info', 'Switch canceled', `Running ${agy ? 'Antigravity' : 'Codex'} programs were left unchanged.`); return; }
    if (action === 'activation-confirm') { if (authenticated) await (antigravityConfirmation.hasPending() ? antigravityConfirmation : activationConfirmation).confirm(); return; }
    if (action === 'navigate') { navigate(value); return; }
    if (action === 'navigate-dashboard') { navigate('home'); return; }
    if (action === 'navigate-analytics') { navigate('analytics'); return; }
    if (action.startsWith('analytics-')) { if (authenticated) await analyticsAction(action, value); return; }
    if (action === 'theme') { saveTheme(value); return; }
    if (action === 'details') { renderDetails(value); return; }
    if (action === 'details-closed') { openDetailsId = ''; return; }
    if (action === 'login') { await signIn(value); return; }
    if (!authenticated || busy || pendingActivation()) return;
    if (action === 'logout') {
      await mutation('/api/auth/logout', {});
      analyticsGeneration++; analyticsPayload = null; analyticsModel = null; data = null; profiles = []; antigravityInventory = null; antigravityAuto = null; refreshGeneration++; openDetailsId = '';
      auth(false, 'default', { message: 'Signed out.' }); return;
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
      setBusy(true);
      try { await mutation(`/api/claude/desktop-profiles/${encodeURIComponent(id)}/open`, { platform: target }); }
      finally { setBusy(false); }
      toast('info', `Opening Claude on ${target === 'mac' ? 'Mac' : 'Windows'}`, 'It opens in its own desktop profile.'); return;
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
    if (['antigravity-automatic', 'antigravity-threshold', 'antigravity-pool'].includes(action)) {
      const patch = antigravitySettingsPatch(antigravityModel(), action, value);
      if (!patch) { render(); failure('Antigravity controls need verified Ubuntu support, two accounts and a fresh reported quota pool.', 'Antigravity'); return; }
      refreshGeneration++;
      setBusy(true);
      try {
        const status = await mutation('/api/antigravity/auto-switch', patch, 'PUT');
        if (!validAntigravityAuto(status)) throw new Error('Automatic switching settings could not be confirmed. Refresh before trying again.');
        antigravityAuto = status; render();
      } finally { setBusy(false); }
      return;
    }
    if (action === 'automatic') {
      refreshGeneration++;
      setBusy(true);
      try { const status = await mutation('/api/codex/profiles/auto-switch', { enabled: value === 'true' }, 'PUT'); if (data) data.codexAutoSwitch = status; render(); }
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
  addEventListener('popstate', () => { currentPage = pageFromLocation(); set_current_page(currentPage); if (currentPage === 'analytics' && authenticated) void refreshAnalytics(); });
  set_theme_mode(THEMES[storedTheme()]);
  // Auto follows the browser: the scheme is pushed now and on every change.
  watchMedia('(prefers-color-scheme: dark)', dark => set_system_dark(dark));
  // Headless captures settle instantly (the existing screenshot guard); ?motion keeps motion on.
  const headless = /HeadlessChrome/.test(navigator.userAgent) && !/[?&]motion\b/.test(location.search);
  watchMedia('(prefers-reduced-motion: reduce)', reduced => set_reduced_motion(reduced || headless));
  document.querySelector('#loading').hidden = true;
  await checkSession();
  applyRefreshInterval(refreshIntervalSeconds, refreshSettingsKnown);
  setInterval(() => { activationConfirmation.expire(); antigravityConfirmation.expire(); }, 1_000);
  setInterval(() => { if (authenticated && updateJob?.state === 'running') void updateStatus(); }, 3_000);
  // "Updated 1m ago" and the reset countdowns stay current between refreshes.
  setInterval(() => { if (authenticated && !refreshing) renderChrome(false); }, 15_000);
  setInterval(() => { if (authenticated && !refreshing && !busy) render(); }, 60_000);
} catch (error) {
  const status = document.querySelector('#loading');
  status.hidden = false; status.textContent = error?.code === 'webgl_required' ? WEBGL_REQUIRED_MESSAGE : 'Unable to start the Slint dashboard. Reload to try again.';
  console.error('Slint dashboard failed to start.', error);
}
