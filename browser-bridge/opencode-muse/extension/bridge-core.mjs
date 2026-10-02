export const NATIVE_HOST = 'com.ccs.opencode_usage_bridge';
const WORKSPACE = /^(?:wrk_|org_)[A-Za-z0-9_-]{1,128}$/;
const NAMES = new Set(['auth', '__Host-console_session']);
export const ERROR_MESSAGES = Object.freeze({
  no_browser_cookie: 'No OpenCode console sign-in was found in this browser profile. Open the signed-in console here.',
  needs_sign_in: 'OpenCode rejected this console session. Open the console in this browser and try again.',
  choose_workspace: 'Open the billing page of the OpenCode workspace you want to show, then sync again.',
  workspace_mismatch: 'The open workspace does not belong to this console sign-in.',
  wallet_not_prepaid: 'This workspace does not expose a prepaid Zen wallet.',
  host_unavailable: 'The Mac AI Account Center browser usage helper is not installed or could not start.',
  invalid_request: 'The browser usage request was invalid.',
  invalid_response: 'OpenCode returned no usable workspace wallet.',
  network_error: 'The OpenCode usage request did not finish. Try again later.',
  provider_error: 'OpenCode could not return the wallet right now.',
  local_storage_error: 'The private local usage capsule could not be saved.',
  protocol_error: 'The local usage helper returned an invalid response.',
  busy: 'An OpenCode usage refresh is already running.',
  error: 'OpenCode usage could not be read.',
});
export function safeCode(code) {
  return typeof code === 'string' && Object.hasOwn(ERROR_MESSAGES, code) ? code : 'error';
}
export function workspaceFromURL(raw) {
  if (typeof raw !== 'string') return null;
  try {
    const url = new URL(raw);
    if (url.origin !== 'https://opencode.ai') return null;
    const parts = url.pathname.split('/').filter(Boolean);
    if (!['console', 'workspace'].includes(parts[0]) || !WORKSPACE.test(parts[1] ?? '')) return null;
    return parts[1];
  } catch { return null; }
}
export function filterCookies(values, now = Date.now() / 1000) {
  if (!Array.isArray(values) || values.length > 100) throw new Error('invalid_request');
  const result = [];
  const names = new Set();
  for (const cookie of values) {
    if (!cookie || !NAMES.has(cookie.name) || !['opencode.ai', '.opencode.ai'].includes(cookie.domain)) continue;
    if (cookie.path !== '/' || cookie.secure !== true || typeof cookie.hostOnly !== 'boolean' ||
        typeof cookie.value !== 'string' || !cookie.value || cookie.value.length > 8192 || /[;\u0000-\u0020\u007f]/.test(cookie.value)) {
      throw new Error('invalid_request');
    }
    if (cookie.name === '__Host-console_session' && (cookie.domain !== 'opencode.ai' || !cookie.hostOnly)) throw new Error('invalid_request');
    if (cookie.expirationDate !== undefined && (typeof cookie.expirationDate !== 'number' || !Number.isFinite(cookie.expirationDate))) throw new Error('invalid_request');
    if (cookie.expirationDate !== undefined && cookie.expirationDate <= now) continue;
    if (names.has(cookie.name)) throw new Error('invalid_request');
    names.add(cookie.name);
    const item = {name: cookie.name, value: cookie.value, domain: cookie.domain, path: '/', secure: true, hostOnly: cookie.hostOnly};
    if (cookie.expirationDate !== undefined) item.expirationDate = cookie.expirationDate;
    result.push(item);
  }
  if (!result.length) throw new Error('no_browser_cookie');
  return result;
}
function iso(raw) {
  if (typeof raw !== 'string' || raw.length > 40 || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(raw)) return null;
  const date = new Date(raw);
  return Number.isFinite(date.getTime()) && date.getUTCFullYear() >= 2000 && date.getUTCFullYear() <= 2200 ? date.toISOString() : null;
}
const LABELS = {'zen-balance': 'Zen balance', 'console-fiveHour': '5 hours', 'console-week': 'Weekly', 'console-month': 'Monthly'};
export function projectSample(value) {
  if (!value || typeof value !== 'object' || value.provider !== 'opencode-go' || value.platform !== 'mac' || value.status !== 'ok' ||
      typeof value.id !== 'string' || !/^plan-opencode-go-console-mac-[a-f0-9]{12}$/.test(value.id) ||
      !Array.isArray(value.windows) || value.windows.length > 4 || !iso(value.fetchedAt) || !iso(value.sampledAt)) throw new Error('protocol_error');
  const windows = [];
  const seen = new Set();
  for (const row of value.windows) {
    if (!row || typeof row !== 'object' || !Object.hasOwn(LABELS, row.key) || seen.has(row.key)) throw new Error('protocol_error');
    seen.add(row.key);
    if (row.key === 'zen-balance') {
      if (row.unit !== 'USD' || typeof row.remaining !== 'number' || !Number.isFinite(row.remaining) || row.kind !== 'balance') throw new Error('protocol_error');
      windows.push({key: row.key, label: LABELS[row.key], kind: 'balance', remaining: row.remaining, unit: 'USD', expiresAt: iso(row.expiresAt), resetAt: null});
    } else {
      if (typeof row.usedPercent !== 'number' || !Number.isFinite(row.usedPercent) || row.usedPercent < 0) throw new Error('protocol_error');
      windows.push({key: row.key, label: LABELS[row.key], kind: 'rate_limit', usedPercent: row.usedPercent,
        remainingPercent: Math.max(0, 100 - row.usedPercent), resetAt: iso(row.resetAt)});
    }
  }
  if (!seen.has('zen-balance')) throw new Error('protocol_error');
  return {id: value.id, provider: 'opencode-go', platform: 'mac', status: 'ok', label: 'OpenCode console wallet',
    source: 'Authenticated console workspace on Mac', fetchedAt: iso(value.fetchedAt), sampledAt: iso(value.sampledAt), windows};
}

export const MUSE_ERROR_MESSAGES = Object.freeze({
  no_browser_cookie: 'No Muse web sign-in was found. Open dev.meta.ai in this browser and sync again.',
  needs_sign_in: 'The Muse web session expired. Open dev.meta.ai in this browser and sync again.',
  account_mismatch: 'The Muse web account differs from the signed-in Muse CLI account.',
  identity_unavailable: 'Muse is not returning the signed-in web account email right now. Usage will refresh automatically when it is available.',
  plan_mismatch: 'The selected web team has a different subscription from the Muse CLI account.',
  choose_team: 'Select the team that owns your Muse coding subscription, then sync again.',
  team_mismatch: 'The selected Muse team is not available to this account.',
  inactive_subscription: 'This account has no active Muse Code subscription.',
  host_unavailable: 'The Mac browser usage helper could not start.',
  invalid_request: 'The Muse browser usage request was invalid.',
  invalid_response: 'Muse returned no usable rolling or weekly quota.',
  network_error: 'The Muse usage request did not finish. Try again later.',
  provider_error: 'Muse could not return its usage right now.',
  rate_limited: 'Muse is limiting requests. Usage refreshes automatically after a cooldown.',
  local_storage_error: 'The private Muse web session could not be read or saved.',
  busy: 'A Muse usage refresh is already running.',
  error: 'Muse usage could not be read.',
});
export function safeMuseCode(code) {
  return typeof code === 'string' && Object.hasOwn(MUSE_ERROR_MESSAGES, code) ? code : 'error';
}
export function filterMuseCookies(values, now = Date.now() / 1000) {
  if (!Array.isArray(values) || values.length > 100) throw new Error('invalid_request');
  const result = [], names = new Set();
  for (const row of values) {
    if (!row || !['llama_dev_sess', 'llm_sess'].includes(row.name) || !['dev.meta.ai', '.dev.meta.ai'].includes(row.domain)) continue;
    if (row.path !== '/' || row.secure !== true || typeof row.hostOnly !== 'boolean' ||
        typeof row.value !== 'string' || !row.value || row.value.length > 8192 || /[;\u0000-\u0020\u007f]|^(?:dca:|sk-)/.test(row.value) ||
        row.hostOnly && row.domain !== 'dev.meta.ai') throw new Error('invalid_request');
    if (row.expirationDate !== undefined && (typeof row.expirationDate !== 'number' || !Number.isFinite(row.expirationDate))) throw new Error('invalid_request');
    if (row.expirationDate !== undefined && row.expirationDate <= now) continue;
    if (names.has(row.name)) throw new Error('invalid_request');
    names.add(row.name);
    const cookie = {name: row.name, value: row.value, domain: row.domain, path: '/', secure: true, hostOnly: row.hostOnly};
    if (row.expirationDate !== undefined) cookie.expirationDate = row.expirationDate;
    result.push(cookie);
  }
  if (!result.length) throw new Error('no_browser_cookie');
  return result;
}
export function projectMuseTeams(values) {
  if (!Array.isArray(values) || values.length > 100) return [];
  const seen = new Set();
  return values.map(row => {
    if (!row || typeof row.id !== 'string' || !/^[0-9]{1,32}$/.test(row.id) || seen.has(row.id) ||
        typeof row.name !== 'string' || !row.name || row.name.length > 160 || /[\u0000-\u001f\u007f]|dca:|bearer\s|sk-|eyJ/i.test(row.name)) throw new Error('protocol_error');
    seen.add(row.id); return {id: row.id, name: row.name};
  });
}
// Meta's own /usage card names with our cadence, as desktop_usage.muse_window_label.
export function museWindowLabel(key, minutes) {
  if (key === 'weekly') return 'Weekly limit';
  if (!minutes || minutes > 525600) return 'Current usage';
  return minutes % 60 === 0 ? `Current usage (${minutes / 60}-hour)` : `Current usage (${minutes}-minute)`;
}
export function projectMuseSample(value) {
  if (!value || value.provider !== 'muse' || value.platform !== 'mac' || !['ok', 'cached'].includes(value.status) ||
      !iso(value.fetchedAt) || !iso(value.sampledAt) || !Array.isArray(value.windows) || !value.windows.length || value.windows.length > 2 ||
      typeof value.email !== 'string' || value.email.length > 254 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value.email) ||
      typeof value.plan !== 'string' || !value.plan || value.plan.length > 160 || /[\u0000-\u001f\u007f]|dca:|bearer\s|sk-|eyJ/i.test(value.plan)) throw new Error('protocol_error');
  const nonnegative = raw => typeof raw === 'number' && Number.isFinite(raw) && raw >= 0 ? raw : null;
  const seen = new Set();
  const windows = value.windows.map(row => {
    if (!row || !['window', 'weekly'].includes(row.key) || seen.has(row.key)) throw new Error('protocol_error');
    seen.add(row.key);
    const usedPercent = nonnegative(row.usedPercent), resetAt = iso(row.resetAt), used = nonnegative(row.used), limit = nonnegative(row.limit);
    if (usedPercent === null && resetAt === null && used === null) throw new Error('protocol_error');
    const minutes = nonnegative(row.windowMinutes);
    return {key: row.key, label: museWindowLabel(row.key, minutes),
      usedPercent, remainingPercent: usedPercent === null ? null : Math.max(0, 100 - usedPercent), resetAt,
      windowMinutes: minutes && minutes <= 525600 ? minutes : null, used, limit, unit: 'weighted tokens', kind: 'rate_limit'};
  });
  return {id: 'native:muse:mac', provider: 'muse', providerLabel: 'Muse Code', label: 'Muse Code', email: value.email, plan: value.plan,
    platform: 'mac', source: 'Authenticated Meta web quota on Mac', status: value.status,
    message: value.status === 'cached' ? 'Showing the last successful Muse usage reading. Usage refreshes automatically.' : null,
    fetchedAt: iso(value.fetchedAt), sampledAt: iso(value.sampledAt), isActive: false, windows,
    capabilities: {codexProfile: null, claudeProfileId: null, claudePlatforms: []}};
}
