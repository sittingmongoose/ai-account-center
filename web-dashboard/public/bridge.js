import init, { start_dashboard, resize_dashboard, set_dashboard, set_session, set_message, set_theme, show_details, set_update_status, show_activation_confirmation, close_activation_confirmation, set_analytics, set_analytics_loading, set_current_page, set_refresh_interval } from './pkg/ccs_account_dashboard.js';
import { dashboardView, allDetailWindows, PROVIDERS } from './accounts-data.mjs';
import { createActivationConfirmation } from './activation-confirmation.mjs';
import { analyticsView, analyticsChoiceId } from './analytics-data.mjs';
import { requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard } from './renderer.mjs';

let authenticated = false;
let busy = false;
let noticeMessage = '';
let data = null;
let profiles = [];
let updateJob = null;
let refreshing = null;
let refreshGeneration = 0;
let currentPage = location.pathname === '/analytics' || new URLSearchParams(location.search).get('view') === 'analytics' ? 'analytics' : 'dashboard';
let refreshIntervalSeconds = 60;
let refreshSettingsKnown = false;
let usageTimer = null;
let analyticsPayload = null;
let analyticsModel = null;
let analyticsGeneration = 0;
const analyticsSelection = { range: '7d', provider: 'all', account: 'all', metricKey: '', activityInterval: 'Daily' };
const platform = /Windows/i.test(navigator.userAgent) ? 'windows' : 'mac';
function notice(message = '', inProgress = false) { noticeMessage = message; busy = inProgress; set_message(message, inProgress); }
async function request(path, options = {}) {
  const response = await fetch(path, { credentials: 'same-origin', ...options, headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    if (response.status === 401 && path !== '/api/auth/login') { authenticated = false; set_session(false, false, false, 'Your AI Account Center session expired. Sign in again.'); }
    const error = new Error(payload?.error || `Request failed (${response.status}).`);
    error.status = response.status;
    error.payload = payload;
    throw error;
  }
  return payload;
}
const mutation = (path, body, method = 'POST') => request(path, { method, body: JSON.stringify(body) });
function render() { set_dashboard(JSON.stringify(dashboardView(data, profiles, platform))); }
const activationConfirmation = createActivationConfirmation({
  activate: (target, body) => mutation(`/api/codex/profiles/${encodeURIComponent(target)}/activate`, body),
  prompt: confirmation => show_activation_confirmation(JSON.stringify({ ...confirmation, expiresAt: `Review valid until ${new Date(confirmation.expiresAt).toLocaleString()}` })),
  close: close_activation_confirmation,
  busy: inProgress => { busy = inProgress; set_message(noticeMessage, inProgress); },
  success: async result => {
    if (data && typeof result?.name === 'string') {
      for (const account of data.accounts) if (account.provider === 'codex') account.isActive = account.capabilities?.codexProfile === result.name;
      render();
    }
    notice(`Codex account activated: ${result?.email || result?.name || 'selected account'}.`);
    await refresh(true);
  },
  error: message => notice(message),
});

async function refresh(force = false) {
  if (!authenticated) return;
  if (refreshing && !force) return refreshing;
  const generation = ++refreshGeneration;
  const operation = (async () => {
    try {
      const [next, inventory] = await Promise.all([
        request(`/api/accounts/dashboard?platform=${platform}&refresh=${force}`),
        request('/api/claude/desktop-profiles').catch(() => ({ profiles })),
      ]);
      if (generation !== refreshGeneration || !authenticated) return;
      if (next?.schemaVersion !== 1 || !Array.isArray(next.accounts)) throw new Error('Unsupported account dashboard response.');
      data = next; if (Number.isInteger(next.settings?.refreshIntervalSeconds)) applyRefreshInterval(next.settings.refreshIntervalSeconds); profiles = Array.isArray(inventory?.profiles) ? inventory.profiles : profiles;
      render(); notice();
    } catch (error) { if (generation === refreshGeneration && authenticated) notice(`${error.message} Last received samples remain visible.`); }
  })();
  refreshing = operation;
  await operation;
  if (refreshing === operation) refreshing = null;
}
function renderAnalytics() {
  if (!analyticsPayload) return;
  analyticsModel = analyticsView(analyticsPayload, { catalog: data?.accounts || [], metricKey: analyticsSelection.metricKey, activityInterval: analyticsSelection.activityInterval });
  set_analytics(JSON.stringify(analyticsModel));
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
  if (action === 'analytics-account-id') {
    if (!analyticsModel?.choices?.accounts?.some(row => row.id === value)) return;
    analyticsSelection.account = value; analyticsSelection.metricKey = '';
    await refreshAnalytics(); return;
  }
  if (action === 'analytics-range') {
    const label = value.toLowerCase();
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
function applyRefreshInterval(seconds, confirmed = true) {
  if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
  refreshIntervalSeconds = seconds;
  refreshSettingsKnown = confirmed;
  set_refresh_interval(seconds, confirmed);
  if (usageTimer) clearInterval(usageTimer);
  usageTimer = setInterval(() => { if (authenticated && !busy && !activationConfirmation.hasPending()) { void refresh(); if (currentPage === 'analytics') void refreshAnalytics(); } }, seconds * 1000);
}
async function loadSettings() {
  try { const result = await request('/api/accounts/settings'); applyRefreshInterval(result?.refreshIntervalSeconds); } catch {}
}
async function checkSession() {
  try {
    const status = await request('/api/auth/check');
    authenticated = status.authenticated === true || status.authRequired === false;
    set_session(authenticated, false, false, status.accessMode === 'setup' ? 'Dashboard authentication must be configured on the server.' : '');
    if (authenticated) { await loadSettings(); await refresh(); await updateStatus(); if (currentPage === 'analytics') await refreshAnalytics(); }
  } catch { authenticated = false; set_session(false, false, false, 'Unable to connect to AI Account Center. Try refreshing this page.'); }
}
function details(id) {
  const account = data?.accounts?.find(row => row.id === id);
  const provider = PROVIDERS.find(row => row[0] === id);
  const related = provider ? data?.accounts?.filter(row => row.provider === id) || [] : account ? [account] : [];
  const note = related.map(row => [row.email || row.label, row.plan, row.source, row.message].filter(Boolean).join(' · ')).join('\n') || 'No usable account data was returned.';
  show_details(account?.email || account?.label || provider?.[1] || 'Account usage', note, JSON.stringify(allDetailWindows(data, id)));
}
function updateDescription(job) {
  if (!job) return '';
  const counts = job.results?.length || 0;
  const failures = (job.results || []).filter(row => row.status === 'failed' || row.status === 'restart_failed').length;
  return job.state === 'running' ? `Updating apps on ${job.activePlatform || 'the next computer'} · ${counts}/21 results. Updated running apps may restart.` : `App update ${job.state} · ${counts} results${failures ? ` · ${failures} failed` : ''}. See Settings for details.`;
}
async function updateStatus() {
  if (!authenticated) return;
  try { const result = await request('/api/app-updates/status'); updateJob = result?.job || null; set_update_status(updateDescription(updateJob), updateJob?.state === 'running'); }
  catch { set_update_status('App update status unavailable.', false); }
}
window.ccsDashboardAction = async (action, value) => {
  try {
    if (action === 'activation-cancel') { if (activationConfirmation.cancel()) notice('Account switch canceled. Running Codex programs were left unchanged.'); return; }
    if (action === 'activation-confirm') { if (authenticated) await activationConfirmation.confirm(); return; }
    if (action === 'navigate-dashboard') { currentPage = 'dashboard'; return; }
    if (action === 'navigate-analytics') { currentPage = 'analytics'; if (authenticated) await refreshAnalytics(); return; }
    if (action.startsWith('analytics-')) { if (authenticated) await analyticsAction(action, value); return; }
    if (action === 'theme') { try { localStorage.setItem('ccs-slint-theme', value); } catch {} return; }
    if (action === 'details') { details(value); return; }
    if (action === 'accounts') { const rows = (data?.accounts || []).map(row => ({ label: `${row.providerLabel} · ${row.email || row.label}`, amount: [row.plan, row.status].filter(Boolean).join(' · '), percent: 0, hasPercent: false, reset: '', expiration: '', meta: row.source || '' })); show_details('Accounts', `${rows.length} reported accounts. Select a card’s Details button for every quota and balance.`, JSON.stringify(rows)); return; }
    if (action === 'settings') {
      const auto = data?.codexAutoSwitch;
      const rows = (updateJob?.results || []).map(row => ({ label: `${row.platform} · ${row.appLabel}`, amount: `${row.status}${row.version ? ` · ${row.version}` : ''}`, percent: 0, hasPercent: false, reset: '', expiration: '', meta: row.message || '' }));
      show_details('Settings', `Codex auto-switch ${auto?.enabled ? 'enabled' : 'disabled'} · threshold ${auto ? 100 - auto.thresholdPercent : 'unknown'}% used · ${auto?.pollIntervalSeconds || 'unknown'}s. Waits until idle. Claude stays manual.
${updateJob ? updateDescription(updateJob) : 'No app update has run.'}`, JSON.stringify(rows)); return;
    }
    if (action === 'login') {
      if (busy) return;
      const separator = value.indexOf('\n');
      const username = value.slice(0, separator), password = value.slice(separator + 1);
      set_session(false, false, true, 'Signing in…'); busy = true;
      await mutation('/api/auth/login', { username, password });
      value = ''; authenticated = true; busy = false; set_session(true, false, false, ''); await refresh(true); return;
    }
    if (!authenticated || busy || activationConfirmation.hasPending()) return;
    if (action === 'logout') { await mutation('/api/auth/logout', {}); analyticsGeneration++; analyticsPayload = null; analyticsModel = null; authenticated = false; data = null; profiles = []; refreshGeneration++; render(); set_session(false, false, false, ''); return; }
    if (action === 'refresh') { notice('Refreshing account usage…', true); await refresh(true); await refreshAnalytics(true); return; }
    if (action === 'launch') {
      const [id, target] = value.split(':');
      if (!['platyr', 'gmail', 'party', 'me'].includes(id) || !['mac', 'windows'].includes(target)) throw new Error('Unknown Claude profile.');
      const launcher = profiles.find(row => row.id === id)?.[target];
      if (target === 'windows' && launcher?.canOpen !== true) {
        if (platform === 'windows' && /^ccs-claude:\/\/launch\/(platyr|gmail|party|me)$/.test(launcher?.launchUri || '')) { location.href = launcher.launchUri; return; }
        throw new Error('Remote Windows launcher is unavailable.');
      }
      notice(`Opening Claude on ${target === 'mac' ? 'Mac' : 'Windows'}…`, true);
      await mutation(`/api/claude/desktop-profiles/${encodeURIComponent(id)}/open`, { platform: target });
      notice(`Claude ${id} opened on ${target === 'mac' ? 'Mac' : 'Windows'}.`); return;
    }
    if (action === 'activate') {
      refreshGeneration++;
      const account = data?.accounts?.find(row => row.capabilities?.codexProfile === value);
      if (!account || account.isActive) return;
      notice('Checking running Codex programs before activation…', true);
      await activationConfirmation.begin(value); return;
    }
    if (action === 'automatic') {
      refreshGeneration++;
      notice('Saving automatic switching…', true);
      const status = await mutation('/api/codex/profiles/auto-switch', { enabled: value === 'true' }, 'PUT');
      if (data) data.codexAutoSwitch = status; render(); notice(); return;
    }
    if (action === 'threshold') {
      refreshGeneration++;
      const used = Number(value.replace("%", ""));
      if (!Number.isInteger(used) || used < 1 || used > 99) return;
      notice('Saving switch threshold…', true);
      const status = await mutation('/api/codex/profiles/auto-switch', { thresholdPercent: 100 - used }, 'PUT');
      if (data) data.codexAutoSwitch = status; render(); notice(); return;
    }
    if (action === 'refresh-interval') {
      const seconds = Number(value.replace(/s$/, ''));
      if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
      notice('Saving usage refresh interval…', true);
      const result = await mutation('/api/accounts/settings', { refreshIntervalSeconds: seconds }, 'PUT');
      applyRefreshInterval(result.refreshIntervalSeconds); notice(); return;
    }
    if (action === 'update-apps') {
      notice('Starting app updates…', true);
      const result = await mutation('/api/app-updates/start', {}); updateJob = result.job;
      set_update_status(updateDescription(updateJob), true); notice(); return;
    }
  } catch (error) { if (data && (action === 'threshold' || action === 'automatic')) render(); if (action === 'refresh-interval') set_refresh_interval(refreshIntervalSeconds, refreshSettingsKnown); notice(error?.message || 'Unable to complete the action.'); if (!authenticated) set_session(false, false, false, error?.message || 'Sign-in failed.'); }
};

try {
  requireWebGL();
  await init();
  startSlintDashboard(() => start_dashboard(innerWidth, innerHeight, devicePixelRatio));
  set_current_page(currentPage);
  const resize = () => resize_dashboard(innerWidth, innerHeight);
  addEventListener('resize', resize); resize();
  let theme; try { theme = localStorage.getItem('ccs-slint-theme'); } catch {}
  set_theme(theme ? theme === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches);
  document.querySelector('#loading').hidden = true;
  await checkSession();
  applyRefreshInterval(refreshIntervalSeconds, refreshSettingsKnown);
  setInterval(() => activationConfirmation.expire(), 1_000);
  setInterval(() => { if (authenticated && updateJob?.state === 'running') void updateStatus(); }, 3_000);
} catch (error) {
  const status = document.querySelector('#loading');
  status.hidden = false; status.textContent = error?.code === 'webgl_required' ? WEBGL_REQUIRED_MESSAGE : 'Unable to start the Slint dashboard. Reload to try again.';
  console.error('Slint dashboard failed to start.', error);
}
