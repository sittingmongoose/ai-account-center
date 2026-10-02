import { visibleUsageWindows } from './visible-usage.mjs';

export const PROVIDERS = [
  ['cursor', 'Cursor', '◇'], ['muse', 'Muse Code', 'M'],
  ['antigravity', 'Google Antigravity CLI', 'G'], ['kimi-code', 'Kimi Code', 'K'],
  ['qwen', 'Qwen Token Plan', 'Q'], ['zai', 'Z.ai Coding Plan', 'Z'],
  ['opencode-go', 'OpenCode Go', '∞'],
];
const finite = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const percent = value => finite(value) && value <= 100;
const finiteAmount = value => typeof value === 'number' && Number.isFinite(value);
export function timeLabel(value, prefix) {
  if (typeof value !== 'string' || !value) return '';
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return '';
  return `${prefix} ${date.toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit', ...(prefix === 'Resets' ? { timeZoneName: 'short' } : {}) })}`;
}
function compactResetLabel(value) {
  if (typeof value !== 'string' || !value) return '';
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return '';
  const day = date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  const time = date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit', timeZoneName: 'short' });
  return `Resets ${day}\n${time}`;
}
const number = value => new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(value);
export function usageView(window = {}, percentageOnly = false, includeRemaining = true) {
  const p = finite(window.usedPercent) ? window.usedPercent : percent(window.remainingPercent) ? 100 - window.remainingPercent : null;
  const unit = typeof window.unit === 'string' && window.unit ? ` ${window.unit}` : '';
  const parts = [];
  if (finiteAmount(window.used)) parts.push(`${number(window.used)}${finiteAmount(window.limit) && window.limit > 0 ? ` / ${number(window.limit)}` : ''}${unit} used`);
  else if (finiteAmount(window.limit) && window.limit > 0) parts.push(`${number(window.limit)}${unit} limit`);
  if (includeRemaining && finiteAmount(window.remaining)) parts.push(`${number(window.remaining)}${unit} remaining`);
  const state = [window.unlimited === true ? 'Unlimited' : '', typeof window.enabled === 'boolean' ? window.enabled ? 'Enabled' : 'Disabled' : ''].filter(Boolean).join(' · ');
  const meta = [state, window.status === 'cached' ? 'Cached' : '', window.status === 'cached' ? timeLabel(window.sampledAt, 'Sampled') : ''].filter(Boolean).join(' · ');
  const hasPercent = p !== null && window.unlimited !== true;
  return {
    label: typeof window.label === 'string' ? window.label : 'Usage',
    amount: percentageOnly ? (hasPercent ? `${number(p)}% used` : 'Usage unavailable') : parts.length ? parts.join(' · ') + (hasPercent ? ` · ${number(p)}% used` : '') : hasPercent ? `${number(p)}% used` : state || 'Usage unavailable',
    percent: hasPercent ? p : 0, hasPercent,
    reset: timeLabel(window.resetAt, 'Resets'),
    resetCompact: compactResetLabel(window.resetAt),
    expiration: timeLabel(window.expiresAt, 'Expires'), meta,
  };
}
const fableWindow = window => window?.key === 'seven_day_fable' || /\bfable\b/i.test(window?.label || '');
function primaryWindow(windows, weekly, provider) {
  // Codex additional/model quotas can report the same duration as its core
  // quotas. Only the provider's canonical keys identify the main columns.
  if (provider === 'codex') return windows.find(window => window.key === (weekly ? 'seven_day' : 'five_hour'));
  return windows.find(window => !fableWindow(window) && window.kind !== 'balance' && window.kind !== 'extra_usage' && (weekly
    ? window.windowMinutes === 10080 || /seven.?day|weekly|week|7.?day/i.test(`${window.key} ${window.label}`)
    : window.windowMinutes === 300 || /five.?hour|5.?hour|5h/i.test(`${window.key} ${window.label}`)));
}
export function statusLabel(account) {
  if (account.isActive && account.provider === 'codex') return '✓ Active on Ubuntu';
  return { ok: 'Live', cached: 'Cached', needs_sign_in: 'Sign-in needed', error: 'Refresh failed', unavailable: 'Unavailable' }[account.status] || 'Unavailable';
}
export function accountView(account, inventory = [], clientPlatform = 'mac') {
  const windows = visibleUsageWindows(account.provider, account.windows);
  const launcher = inventory.find(profile => profile.id === account.capabilities?.claudeProfileId);
  const showFable = account.provider === 'claude' && /(?:^|\s)max(?:\s*(?:5|20)x)?(?:$|\s)/i.test(String(account.plan || '').replace(/[_-]/g, ' '));
  return {
    id: account.id, profile: account.capabilities?.codexProfile || account.capabilities?.claudeProfileId || '',
    email: account.email || account.label || 'Account identity unavailable', plan: account.plan || '',
    status: statusLabel(account), active: account.isActive === true,
    canMac: !!launcher?.mac?.canOpen, canWindows: launcher?.windows?.canOpen === true || (clientPlatform === 'windows' && !!launcher?.windows?.launchUri),
    five: usageView(primaryWindow(windows, false, account.provider), true), weekly: usageView(primaryWindow(windows, true, account.provider), true),
    showFable, fable: usageView(showFable ? windows.find(fableWindow) || { label: 'Fable usage' } : { label: 'Fable usage' }, true),
    windows: windows.map(window => usageView(window)), note: account.message || account.source || '',
  };
}
export function dashboardView(data, inventory = [], clientPlatform = 'mac') {
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const auto = data?.codexAutoSwitch;
  const providers = PROVIDERS.map(([id, name, glyph]) => {
    const rows = accounts.filter(account => account.provider === id);
    return {
      id, name, glyph, identity: rows.map(row => row.email || row.label).filter(Boolean).join(' · '),
      plan: rows.map(row => row.plan).filter(Boolean).join(' · '),
      status: rows.length ? rows.map(statusLabel).filter((item, index, all) => all.indexOf(item) === index).join(' · ') : 'Unavailable',
      note: rows.map(row => row.message).filter(Boolean).join(' · ') || 'Usage unavailable',
      source: rows.map(row => [row.source, row.provider === 'muse' && row.status === 'cached'
        ? timeLabel(row.sampledAt, 'Sampled') || timeLabel(row.fetchedAt, 'Fetched') : ''].filter(Boolean).join(' · ')).filter(Boolean).join(' · '),
      // Cards preview three rows; all actual rows are retained for the Details view.
      windows: rows.flatMap(row => visibleUsageWindows(row.provider, row.windows).map(window => usageView(window, row.provider === 'muse',
        !(row.provider === 'qwen' && window.unit === 'credits' && finiteAmount(window.used))))).slice(0, 3),
    };
  });
  return {
    claude: accounts.filter(account => account.provider === 'claude').map(account => accountView(account, inventory, clientPlatform)),
    codex: accounts.filter(account => account.provider === 'codex').map(account => accountView(account, inventory, clientPlatform)),
    providers, activeCodexEmail: accounts.find(account => account.provider === 'codex' && account.isActive === true)?.email || accounts.find(account => account.provider === 'codex' && account.isActive === true)?.label || 'Active account unavailable', autoEnabled: auto?.enabled === true,
    autoAvailable: !!auto && percent(auto.thresholdPercent),
    autoSetting: auto && percent(auto.thresholdPercent) ? `${number(100 - auto.thresholdPercent)}% used · ${auto.pollIntervalSeconds}s` : 'Setting unavailable',
    thresholdUsed: auto && percent(auto.thresholdPercent) ? 100 - auto.thresholdPercent : -1,
    thresholdLabel: auto && percent(auto.thresholdPercent) ? `${number(100 - auto.thresholdPercent)}%` : "—",
    autoMessage: auto?.message || 'Automatic switching status unavailable.',
    updated: timeLabel(data?.updatedAt, 'Updated') || 'Account data unavailable',
  };
}
export function allDetailWindows(data, id) {
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const rows = PROVIDERS.some(provider => provider[0] === id) ? accounts.filter(account => account.provider === id) : accounts.filter(account => account.id === id);
  return rows.flatMap(account => visibleUsageWindows(account.provider, account.windows).map(window => ({ ...usageView(window), label: rows.length > 1 ? `${account.email || account.label || account.platform} · ${window.label}` : window.label })));
}
