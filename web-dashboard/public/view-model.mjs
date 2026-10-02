// Dashboard view model, version 2 (Daylight Atlas). Pure functions from the public API DTOs to the
// JSON that src/lib.rs deserializes into the Slint structs in ui/models.slint. Every truthfulness rule
// lives here or in the modules it builds on: a missing reading is unavailable, never zero; nothing is
// added or averaged across accounts or windows; at most two decimals; the product visibility rules of
// visible-usage.mjs; Fable only for Claude Max plans, from the seven_day_fable window.
import { visibleUsageWindows } from './visible-usage.mjs';
import { antigravityView } from './antigravity-data.mjs';

export const VIEW_MODEL_VERSION = 2;

/** The provider registry, derived client-side until the backend sends one (see CONTRACT notes in W1). */
export const PROVIDER_REGISTRY = [
  { id: 'claude', label: 'Claude', longLabel: 'Claude', signIn: 'Desktop profile', multi: true, switchable: false },
  { id: 'codex', label: 'Codex', longLabel: 'Codex', signIn: 'Device-code sign-in', multi: true, switchable: true },
  { id: 'antigravity', label: 'Antigravity', longLabel: 'Google Antigravity CLI', signIn: 'Supervised CLI login', multi: true, switchable: true },
  { id: 'cursor', label: 'Cursor', longLabel: 'Cursor', signIn: 'Desktop app session', multi: false, switchable: false },
  { id: 'muse', label: 'Muse Code', longLabel: 'Muse Code', signIn: 'Desktop app session', multi: false, switchable: false },
  { id: 'kimi-code', label: 'Kimi Code', longLabel: 'Kimi Code', signIn: 'API key', multi: false, switchable: false },
  { id: 'qwen', label: 'Qwen Token Plan', longLabel: 'Qwen Token Plan', signIn: 'API key, web extras by browser extension', multi: false, switchable: false },
  { id: 'zai', label: 'Z.ai Coding Plan', longLabel: 'Z.ai Coding Plan', signIn: 'API key', multi: false, switchable: false },
  { id: 'opencode-go', label: 'OpenCode Go', longLabel: 'OpenCode Go', signIn: 'API key, console wallet by browser session', multi: true, switchable: false },
];
const SWITCHABLE = ['codex', 'antigravity'];
const CARD_PROVIDERS = ['cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go'];

// ---------------------------------------------------------------- formatting
const finite = value => typeof value === 'number' && Number.isFinite(value);
const validDate = value => typeof value === 'string' && Number.isFinite(Date.parse(value));
const text = value => typeof value === 'string' ? value : '';
const nf2 = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const nfCompact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 2 });
const money = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 2 });
const dateTime = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
const PLATFORM = { mac: 'Mac', windows: 'Windows', ubuntu: 'Ubuntu', linux: 'Linux' };

export const platformLabel = id => PLATFORM[id] || (id ? id[0].toUpperCase() + id.slice(1) : 'Unknown');
export const planLabel = plan => !plan ? '' : plan.length <= 5 ? plan[0].toUpperCase() + plan.slice(1) : plan;
/** At most two decimals, locale grouping, never rounding a real reading up to a different integer label. */
export const valueText = value => finite(value) ? nf2.format(value) : '';
export function duration(ms) {
  const m = Math.floor(Math.max(0, ms) / 60000), h = Math.floor(m / 60), d = Math.floor(h / 24);
  if (d >= 1) return `${d}d ${h % 24}h`;
  if (h >= 1) return `${h}h ${m % 60}m`;
  return `${Math.max(1, m)}m`;
}
export function relative(value, now) {
  const time = typeof value === 'number' ? value : Date.parse(value);
  if (!Number.isFinite(time)) return '';
  const delta = now - time;
  if (delta < 10_000) return 'just now';
  return `${duration(delta)} ago`;
}
export function resetIn(value, now) {
  if (!validDate(value)) return '';
  const delta = Date.parse(value) - now;
  return delta <= 0 ? 'reset due' : `resets in ${duration(delta)}`;
}
const exact = (value, prefix) => validDate(value) ? `${prefix}${dateTime.format(new Date(value))}` : '';
export function intervalLabel(seconds) {
  if (seconds < 60) return `${seconds} s`;
  return seconds % 60 === 0 ? `${seconds / 60} min` : `${Math.floor(seconds / 60)} min ${seconds % 60} s`;
}
export function parseIntervalLabel(label) {
  const match = /^\s*(?:(\d+)\s*min)?\s*(?:(\d+)\s*s)?\s*$/.exec(String(label || ''));
  if (!match || (!match[1] && !match[2])) return null;
  return Number(match[1] || 0) * 60 + Number(match[2] || 0);
}
const unitAmount = (value, unit) => {
  if (!finite(value)) return '';
  if (unit === 'USD') return money.format(value);
  const u = unit ? (value === 1 ? unit.replace(/s$/, '') : unit) : '';
  return nf2.format(value) + (u ? ` ${u}` : '');
};
const compactAmount = value => Math.abs(value) >= 1e6 ? nfCompact.format(value) : nf2.format(value);
/** A run of text in a line that mixes weights ("**4** accounts · desktop profiles"); tone '' | 'good' | 'warn'. */
export const run = (value, strong = false, tone = '') => ({ text: String(value), strong: !!strong, tone });
const STALE_MS = 30 * 60_000;

// ---------------------------------------------------------------- windows
export const usedPercent = w => finite(w?.usedPercent) && w.usedPercent >= 0 ? w.usedPercent
  : finite(w?.remainingPercent) && w.remainingPercent >= 0 && w.remainingPercent <= 100 ? 100 - w.remainingPercent : null;
const description = w => `${text(w?.key)} ${text(w?.label)}`;
export const isFable = w => w?.key === 'seven_day_fable' || /\bfable\b/i.test(text(w?.label));
/** Meters are rate limits; packs, balances, credits and spend are amounts, never fake meters. */
export const isMeterWindow = w => !['balance', 'extra_usage', 'spend'].includes(w?.kind) && w?.unlimited !== true;
export function period(w) {
  const d = description(w);
  if (w?.windowMinutes === 300 || /five.?hour|5.?hour|\b5h\b|5 hours|rolling/i.test(d)) return '5h';
  if (w?.windowMinutes === 10080 || /seven.?day|weekly|\bweek\b|7.?day/i.test(d)) return 'week';
  if (/month/i.test(d)) return 'month';
  return 'other';
}
export const isMaxPlan = account => account?.provider === 'claude'
  && /(?:^|\s)max(?:\s*(?:5|20)x)?(?:$|\s)/i.test(String(account.plan || '').replace(/[_-]/g, ' '));
const PERIOD_LABEL = { '5h': '5-hour', week: 'Weekly', month: 'Monthly' };
export function windowLabel(provider, w) {
  if (isFable(w)) return 'Fable';
  const p = period(w);
  if (provider === 'antigravity') return `${/^gemini/i.test(text(w.key)) ? 'Gemini' : 'Claude and GPT'} ${p === '5h' ? '5-hour' : 'weekly'}`;
  if (provider === 'cursor') return { 'plan-reported': 'Included', autoPercentUsed: 'Cursor models', apiPercentUsed: 'Other models' }[w.key] || text(w.label);
  if (provider === 'zai') return { 'usage-1': '5h tokens', 'usage-2': 'Weekly tokens', 'usage-3': 'Monthly requests' }[w.key] || text(w.label);
  if (provider === 'codex' || provider === 'claude' || provider === 'kimi-code' || provider === 'opencode-go' || provider === 'muse' || provider === 'qwen') return PERIOD_LABEL[p] || text(w.label) || 'Usage';
  return text(w.label) || 'Usage';
}
export function fullWindowLabel(provider, w) {
  if (isFable(w)) return 'Fable weekly';
  if (provider === 'codex') return { week: 'Weekly', '5h': '5-hour' }[text(w.label)] || text(w.label) || windowLabel(provider, w);
  if (provider === 'claude') return { 'Five-hour usage': '5-hour usage' }[text(w.label)] || text(w.label) || 'Usage';
  return text(w.label) || 'Usage';
}
function statusWord(account) {
  return { ok: 'Live', cached: 'Cached', needs_sign_in: 'Sign-in needed', error: 'Refresh failed', unavailable: 'Unavailable' }[account?.status] || 'Unavailable';
}
function sampledText(account, w, now) {
  const at = validDate(w?.sampledAt) ? w.sampledAt : validDate(account?.sampledAt) ? account.sampledAt : null;
  if (!at) return '';
  return `${dateTime.format(new Date(at))} · ${relative(at, now)}`;
}

/** One window as a MeterView. `opts.notch` is a "% used" threshold or null. */
export function meterView(account, w, { now = Date.now(), notch = null, notchFaint = false, naText = 'Unavailable', naSub = '', label } = {}) {
  const value = w ? usedPercent(w) : null;
  const hasValue = value !== null && w?.unlimited !== true;
  const unit = text(w?.unit);
  const amount = finite(w?.used) && finite(w?.limit) && w.limit > 0
    ? `${nfCompact.format(w.used)} of ${compactAmount(w.limit)}${unit ? ` ${unit}` : ''}` : '';
  return {
    key: `${account?.id || ''}|${text(w?.key) || 'missing'}`,
    label: label || (w ? windowLabel(account?.provider, w) : 'Usage'),
    fullLabel: w ? fullWindowLabel(account?.provider, w) : label || 'Usage',
    hasValue,
    value: hasValue ? value : 0,
    valueText: hasValue ? valueText(value) : '',
    overText: hasValue && value > 100 ? valueText(value - 100) : '',
    reset: hasValue ? resetIn(w?.resetAt, now) || 'no reset reported' : '',
    resetExact: exact(w?.resetAt, 'Resets '),
    resetSoon: validDate(w?.resetAt) && Date.parse(w.resetAt) - now < 2 * 3_600_000 && Date.parse(w.resetAt) > now,
    naText: hasValue ? '' : naText,
    naSub: hasValue ? '' : naSub,
    amount,
    left: finite(w?.remaining) && unit ? `${unitAmount(w.remaining, unit)} left` : '',
    sampled: sampledText(account, w, now),
    source: [text(account?.source) || 'Unknown source', platformLabel(account?.platform), w?.status === 'cached' ? 'Cached window' : statusWord(account)].join(' · '),
    caption: '',
    captionTip: '',
    notch: hasValue && finite(notch) ? notch : null,
    notchFaint: !!notchFaint,
    notchOff: false,
  };
}
const emptyCell = (account, key) => ({ key: '', label: '', fullLabel: '', hasValue: false, value: 0, valueText: '', overText: '', reset: '', resetExact: '', resetSoon: false, naText: '', naSub: '', amount: '', left: '', sampled: '', source: '', caption: '', captionTip: '', notch: null, notchFaint: false, notchOff: false });

/** Packs, balances, credits and spend as an AmountView. */
export function amountView(account, w, now = Date.now(), { noExpiry = false } = {}) {
  const unit = text(w.unit);
  const label = text(w.label).replace(/^Additional credit pack/i, 'Credit pack').replace(/^Listed active credit packs/i, 'Active packs listed')
    .replace(/^Rate-limit resets available/i, 'Rate-limit resets left');
  const expires = noExpiry ? '' : exact(w.expiresAt, 'Expires ');
  let value = '', suffix = '', sub = '', icon = 'wallet', spent = false;
  if (/pack/i.test(text(w.label)) && finite(w.limit)) {
    icon = 'pack';
    value = unitAmount(w.remaining, null) || 'Unavailable';
    suffix = `of ${unitAmount(w.limit, unit)} left`;
    spent = w.remaining === 0;
    sub = [spent ? 'Used up' : '', expires].filter(Boolean).join(' · ');
  } else if (w.kind === 'spend') {
    value = unitAmount(w.used, unit) || 'Unavailable';
    suffix = finite(w.limit) ? `of ${unitAmount(w.limit, unit)} spent` : 'spent';
    sub = exact(w.resetAt, 'Resets ');
  } else if (w.kind === 'extra_usage') {
    icon = 'zap';
    if (w.enabled === false) {
      value = 'Off';
      sub = finite(w.limit) ? `Limit ${unitAmount(w.limit, unit)}` : finite(w.used) && unit ? `${unitAmount(w.used, unit)} used` : '';
    } else {
      value = finite(w.used) ? unitAmount(w.used, unit) : 'Unavailable';
      suffix = finite(w.used) ? 'used' : '';
      sub = finite(w.limit) ? `of ${unitAmount(w.limit, unit)}` : '';
    }
  } else if (unit === 'resets') {
    icon = 'rotate';
    if (finite(w.used) && finite(w.limit)) { value = nf2.format(w.used); suffix = `of ${unitAmount(w.limit, 'resets')} used`; }
    else value = unitAmount(w.remaining, 'resets') || 'Unavailable';
    sub = expires;
  } else if (unit === 'packs') {
    icon = 'pack';
    value = unitAmount(w.remaining, 'packs') || 'Unavailable';
  } else {
    value = finite(w.remaining) ? unitAmount(w.remaining, unit) : finite(w.used) ? `${unitAmount(w.used, unit)} used` : 'Unavailable';
    sub = [w.enabled === false ? 'Off' : '', expires].filter(Boolean).join(' · ');
  }
  return { key: `${account.id}|${text(w.key)}`, label: label || 'Balance', value, unit: suffix, sub, icon, spent };
}

/** Codex credits and banked resets of one account, as runs with the figures in bold ("**62.5K** credits · **1** banked"). */
function codexAmountsRuns(account) {
  const windows = visibleUsageWindows('codex', account.windows);
  const credits = windows.find(w => w.key === 'credits_balance' || (w.kind === 'balance' && w.unit === 'credits'));
  const banked = windows.find(w => /^banked[_-]?resets/i.test(text(w.key)));
  const parts = [];
  if (credits) {
    if (credits.enabled === false) parts.push([run('Credits off')]);
    else if (finite(credits.remaining)) parts.push([run(credits.remaining >= 10000 ? nfCompact.format(credits.remaining) : nf2.format(credits.remaining), true), run(' credits')]);
  }
  if (banked && finite(banked.remaining)) parts.push([run(nf2.format(banked.remaining), true), run(' banked')]);
  return parts.flatMap((part, index) => index ? [run(' · '), ...part] : part);
}
const runsText = runs => runs.map(r => r.text).join('');
/** The larger of an account's 5-hour and weekly use: the Codex switch point compares this with the threshold. */
function codexPeak(account) {
  const values = visibleUsageWindows('codex', account?.windows).filter(w => (w.key === 'five_hour' || w.key === 'seven_day') && usedPercent(w) !== null).map(usedPercent);
  return values.length ? Math.max(...values) : null;
}
/** The inline question before activating an account that is already past the switch point. */
function confirmRuns(peak, threshold, autoOn) {
  return [run(`${valueText(peak)}% used`, true, 'warn'), run(`, above the ${valueText(threshold)}% switch point.${autoOn ? ' Auto-switch would move off it again on its next check.' : ''} Activate anyway?`)];
}
/** Codex footer: what auto-switch is doing about the active account, in the concept's words. */
function codexFoot(accounts, auto, known, threshold, now) {
  const short = account => shortIdentity(account);
  const active = accounts.find(account => account.isActive === true);
  const checked = validDate(auto?.lastCheckedAt) ? relative(auto.lastCheckedAt, now) : 'never';
  const when = `Checked ${checked}${Number.isInteger(auto?.pollIntervalSeconds) ? ` · every ${intervalLabel(auto.pollIntervalSeconds)}` : ''}`;
  const foot = (runs, warn = false) => ({ shown: true, warn, runs, when: known ? when : '' });
  if (!accounts.length) return { shown: false, warn: false, runs: [], when: '' };
  if (!known) return foot([run(text(auto?.message) || 'Automatic switching status unavailable')]);
  if (!active) return foot([run('No Codex account is active')], true);
  const peak = codexPeak(active);
  if (peak !== null && peak >= threshold) {
    if (auto.enabled !== true) return foot([run(short(active), true), run(` is above ${valueText(threshold)}% used; auto-switch is off, so it stays active until you switch`)], true);
    const next = accounts.filter(account => account !== active).map(account => ({ account, peak: codexPeak(account) }))
      .filter(row => row.peak !== null && row.peak < threshold).sort((a, b) => a.peak - b.peak)[0]?.account;
    return foot(next
      ? [run(short(active), true), run(` is above the ${valueText(threshold)}% switch point; auto-switch moves to `), run(short(next), true), run(' on the next check')]
      : [run(short(active), true), run(` is above the ${valueText(threshold)}% switch point and no other account is below it`)], true);
  }
  if (auto.enabled !== true) return foot([run('Auto-switch is off; the active account changes only when you press Activate')]);
  if (text(auto.message)) return foot([run(text(auto.message))], ['error', 'no_candidate', 'no_fresh_quota'].includes(auto.outcome));
  return foot([run(short(active), true), run(` is below the ${valueText(threshold)}% switch point${peak !== null ? ` at ${valueText(peak)}% used` : ''}`)]);
}
const noFoot = () => ({ shown: false, warn: false, runs: [], when: '' });

const visibleMeters = account => visibleUsageWindows(account.provider, account.windows).filter(isMeterWindow);
const visibleAmounts = account => visibleUsageWindows(account.provider, account.windows).filter(w => !isMeterWindow(w));
const shortIdentity = account => (text(account.email) || text(account.label) || 'Account').replace(/@.*/, '');

/** Hidden providers when the backend sends them (`settings.hiddenProviders`, see the W1 contract notes). */
export function hiddenProviders(data) {
  const hidden = data?.settings?.hiddenProviders;
  return new Set(Array.isArray(hidden) ? hidden.filter(id => PROVIDER_REGISTRY.some(row => row.id === id)) : []);
}
export function providerRegistry(data) {
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const hidden = hiddenProviders(data);
  return PROVIDER_REGISTRY.map(row => ({ ...row, visible: !hidden.has(row.id), count: accounts.filter(account => account.provider === row.id).length }));
}

// ---------------------------------------------------------------- sections
function claudeSection(accounts, profiles, platform, now) {
  const anyMax = accounts.some(isMaxPlan);
  const columns = [{ key: 'five', label: '5-hour' }, { key: 'weekly', label: 'Weekly' }, ...(anyMax ? [{ key: 'fable', label: 'Fable' }] : [])];
  const rows = accounts.map(account => {
    const meters = visibleMeters(account);
    const five = meters.find(w => !isFable(w) && period(w) === '5h');
    const weekly = meters.find(w => !isFable(w) && period(w) === 'week');
    const fable = meters.find(isFable);
    const launcher = (Array.isArray(profiles) ? profiles : []).find(profile => profile.id === account.capabilities?.claudeProfileId);
    const cells = [
      meterView(account, five, { now, label: '5-hour' }),
      meterView(account, weekly, { now, label: 'Weekly' }),
    ];
    if (anyMax) cells.push(isMaxPlan(account)
      ? meterView(account, fable, { now, label: 'Fable', naText: 'Not reported yet' })
      : emptyCell(account, 'fable'));
    if (!five) cells[0].key = `${account.id}|five_hour`;
    if (!weekly) cells[1].key = `${account.id}|seven_day`;
    if (anyMax && isMaxPlan(account) && !fable) cells[2].key = `${account.id}|seven_day_fable`;
    return {
      id: account.id, provider: 'claude', profile: account.capabilities?.claudeProfileId || '',
      email: text(account.email) || text(account.label) || 'Account identity unavailable', plan: text(account.plan),
      meta: [planLabel(text(account.plan)), account.status === 'ok' || account.status === 'cached' ? relative(account.sampledAt || account.fetchedAt, now) : statusWord(account)].filter(Boolean).join(' · '),
      status: statusWord(account), note: text(account.message), platform: text(account.platform),
      active: false, activeLabel: '', setup: false, canActivate: false, activateKind: '', activateHint: '',
      canMac: !!launcher?.mac?.canOpen,
      canWindows: launcher?.windows?.canOpen === true || (platform === 'windows' && !!launcher?.windows?.launchUri),
      amountsLine: '', amountsRuns: [], confirm: false, confirmRuns: [], cells,
    };
  });
  return {
    id: 'claude', kind: 'claude', label: 'Claude', longLabel: 'Claude', switchable: false, canSwitch: false,
    meta: `${rows.length} ${rows.length === 1 ? 'account' : 'accounts'} · desktop profiles`,
    metaRuns: [run(rows.length, true), run(` ${rows.length === 1 ? 'account' : 'accounts'} · desktop profiles`)],
    foot: noFoot(),
    activeId: '', activeLabel: '', empty: 'No Claude accounts are reported yet.',
    auto: { known: false, shown: false, enabled: false, available: false, canEnable: false, thresholdUsed: null, thresholdLabel: '', min: 50, max: 99, pool: '', offRuns: [], setting: '', message: '', example: false },
    columns, rows,
  };
}

function codexSection(accounts, data, now) {
  const auto = data?.codexAutoSwitch;
  const known = !!auto && finite(auto.thresholdPercent) && auto.thresholdPercent >= 0 && auto.thresholdPercent <= 100;
  const threshold = known ? 100 - auto.thresholdPercent : null;
  const notch = known && auto.enabled === true ? threshold : null;
  // Only the canonical keys identify the columns; additional quotas with the same duration stay in Details.
  const hasFive = accounts.some(account => visibleUsageWindows('codex', account.windows).some(w => w.key === 'five_hour' && usedPercent(w) !== null));
  const columns = [...(hasFive ? [{ key: 'five', label: '5-hour' }] : []), { key: 'weekly', label: 'Weekly' }];
  const active = accounts.find(account => account.isActive === true);
  const rows = accounts.map(account => {
    const windows = visibleUsageWindows('codex', account.windows);
    const five = windows.find(w => w.key === 'five_hour');
    const weekly = windows.find(w => w.key === 'seven_day');
    const faint = account.isActive !== true;
    const cells = [];
    if (hasFive) cells.push(five && usedPercent(five) !== null ? meterView(account, five, { now, notch, notchFaint: faint, label: '5-hour' }) : emptyCell(account, 'five_hour'));
    const weeklyCell = meterView(account, weekly, { now, notch, notchFaint: faint, label: 'Weekly' });
    if (!weekly) weeklyCell.key = `${account.id}|seven_day`;
    cells.push(weeklyCell);
    const profile = account.capabilities?.codexProfile || '';
    const amountsRuns = codexAmountsRuns(account);
    const peak = codexPeak(account);
    const above = known && account.isActive !== true && peak !== null && peak >= threshold;
    return {
      id: account.id, provider: 'codex', profile,
      email: text(account.email) || text(account.label) || 'Account identity unavailable', plan: text(account.plan),
      meta: [planLabel(text(account.plan)), platformLabel(account.platform), account.status === 'ok' || account.status === 'cached' ? relative(account.sampledAt || account.fetchedAt, now) : statusWord(account)].filter(Boolean).join(' · '),
      status: statusWord(account), note: text(account.message), platform: platformLabel(account.platform),
      active: account.isActive === true, activeLabel: `on ${platformLabel(account.platform)}`, setup: false,
      canActivate: account.isActive !== true && !!profile, activateKind: 'activate',
      activateHint: profile ? `Make ${text(account.email) || 'this account'} the active Codex account` : 'This account has no Codex profile to activate',
      canMac: false, canWindows: false, amountsLine: runsText(amountsRuns), amountsRuns,
      confirm: above, confirmRuns: above ? confirmRuns(peak, threshold, auto?.enabled === true) : [], cells,
    };
  });
  return {
    id: 'codex', kind: 'switchable', label: 'Codex', longLabel: 'Codex', switchable: true, canSwitch: rows.length > 1,
    meta: `${rows.length} ${rows.length === 1 ? 'account' : 'accounts'}${active ? ` · ${shortIdentity(active)} active` : ''}`,
    metaRuns: [run(rows.length, true), run(` ${rows.length === 1 ? 'account' : 'accounts'} · `), ...(active ? [run(shortIdentity(active), true, 'good'), run(' active')] : [run('none active')])],
    foot: codexFoot(accounts, auto, known, threshold, now),
    activeId: active?.id || '', activeLabel: active ? text(active.email) || text(active.label) : '',
    empty: 'No Codex accounts are reported yet.',
    auto: {
      known, shown: true, enabled: auto?.enabled === true, available: known && auto?.activationInProgress !== true, canEnable: known,
      thresholdUsed: threshold, thresholdLabel: known ? `${valueText(threshold)}%` : '—',
      min: known ? Math.min(50, threshold) : 50, max: 99, pool: '', offRuns: [],
      setting: known ? `${valueText(threshold)}% used · checks every ${auto.pollIntervalSeconds}s` : 'Setting unavailable',
      message: text(auto?.message) || 'Automatic switching status unavailable.', example: false,
    },
    columns, rows,
  };
}

function antigravitySection(accounts, data, inventory, autoStatus, now) {
  const native = antigravityView(data, inventory, autoStatus, now);
  const bound = new Map(native.antigravityAccounts.map(row => [row.id, row]));
  const status = native.antigravityAutoKnown ? autoStatus : null;
  // Columns: every reported Antigravity rate-limit window, Gemini before Claude and GPT, 5-hour before weekly.
  const keys = [];
  for (const account of accounts) for (const w of visibleMeters(account)) if (usedPercent(w) !== null && !keys.some(k => k.key === w.key)) keys.push({ key: w.key, w });
  const order = w => (/^gemini/i.test(text(w.key)) ? 0 : 2) + (period(w) === '5h' ? 0 : 1);
  keys.sort((a, b) => order(a.w) - order(b.w) || a.key.localeCompare(b.key));
  const columns = keys.map(({ key, w }) => ({ key, label: windowLabel('antigravity', w) }));
  const rows = accounts.map(account => {
    const nativeRow = bound.get(account.id);
    const active = nativeRow?.selected === true;
    const notchFor = w => status?.enabled === true && status.requestedPoolId && w.poolId === status.requestedPoolId ? status.thresholdUsedPercent : null;
    // The switch point reads the windows of the pool the policy watches.
    const poolUse = status?.requestedPoolId ? visibleMeters(account).filter(w => w.poolId === status.requestedPoolId && usedPercent(w) !== null).map(usedPercent) : [];
    const peak = poolUse.length ? Math.max(...poolUse) : null;
    const above = !!status && !active && peak !== null && peak >= status.thresholdUsedPercent;
    const cells = keys.map(({ key }) => {
      const w = visibleMeters(account).find(row => row.key === key);
      const cell = meterView(account, w, { now, notch: w ? notchFor(w) : null, notchFaint: !active });
      if (!w) cell.key = `${account.id}|${key}`;
      return cell;
    });
    return {
      id: account.id, provider: 'antigravity', profile: nativeRow?.profile || account.capabilities?.antigravityProfileId || '',
      email: text(account.email) || text(account.label) || 'Account identity unavailable', plan: text(account.plan),
      meta: [text(account.plan), platformLabel(account.platform), account.status === 'ok' || account.status === 'cached' ? relative(account.sampledAt || account.fetchedAt, now) : statusWord(account)].filter(Boolean).join(' · '),
      status: nativeRow?.status || statusWord(account), note: nativeRow?.note || text(account.message), platform: platformLabel(account.platform),
      active, activeLabel: active ? (nativeRow.runtimeVerified ? 'on Ubuntu' : 'on Ubuntu, running unverified') : '',
      setup: false, canActivate: nativeRow?.canActivate === true, activateKind: 'antigravity-activate',
      activateHint: nativeRow?.canActivate ? `Activate ${text(account.email)} on Ubuntu; running programs are listed for review first` : 'Ubuntu activation needs a verified login and runtime',
      canMac: false, canWindows: false, amountsLine: '', amountsRuns: [],
      confirm: above, confirmRuns: above ? confirmRuns(peak, status.thresholdUsedPercent, status.enabled === true) : [], cells,
    };
  });
  const activeRow = rows.find(row => row.active);
  const policyShown = rows.filter(row => !row.setup).length > 1;
  const pool = native.antigravityPoolLabel && native.antigravityPoolLabel !== 'Choose quota pool' ? native.antigravityPoolLabel : '';
  return {
    id: 'antigravity', kind: 'switchable', label: 'Antigravity', longLabel: 'Google Antigravity CLI', switchable: true, canSwitch: policyShown,
    meta: `Google Antigravity CLI · ${rows.length} ${rows.length === 1 ? 'account' : 'accounts'}`,
    metaRuns: [run('Google Antigravity CLI · '), run(rows.length, true), run(` ${rows.length === 1 ? 'account' : 'accounts'}`)],
    foot: noFoot(),
    activeId: activeRow?.id || '', activeLabel: activeRow?.email || '', empty: 'No Antigravity accounts are reported yet.',
    auto: {
      known: native.antigravityAutoKnown, shown: policyShown, enabled: native.antigravityAutoEnabled, available: native.antigravityAutoAvailable,
      canEnable: native.antigravityCanEnable, thresholdUsed: status ? status.thresholdUsedPercent : null,
      thresholdLabel: native.antigravityThresholdLabel,
      min: status ? Math.min(50, status.thresholdUsedPercent) : 50, max: 99, pool,
      // Switching moves between Antigravity accounts, so the policy starts once a second account is signed in.
      offRuns: policyShown ? [] : [run('Auto-switch '), run('off', true), run(' · needs a second account')],
      setting: native.antigravityAutoSetting,
      message: rows.length < 2 ? 'Automatic switching needs a second Antigravity account.' : native.antigravityAutoMessage,
      example: false,
    },
    columns, rows,
  };
}

function providerCards(accounts, hidden, now) {
  const order = new Map(CARD_PROVIDERS.map((id, index) => [id, index]));
  return accounts
    .filter(account => CARD_PROVIDERS.includes(account.provider) && !hidden.has(account.provider))
    .sort((a, b) => order.get(a.provider) - order.get(b.provider) || text(a.id).localeCompare(text(b.id)))
    .map(account => {
      const provider = PROVIDER_REGISTRY.find(row => row.id === account.provider);
      const label = provider?.label || text(account.providerLabel);
      // Up to three meters; a window with no reading is drawn as unavailable, never as zero. The rest stay in Details.
      const windows = visibleMeters(account).slice(0, 3);
      const meters = windows.map(w => meterView(account, w, { now }));
      const planExpiry = visibleMeters(account).find(w => w.planExpiry && validDate(w.expiresAt));
      // Packs that all expire together say so once instead of on every line.
      const amountWindows = visibleAmounts(account);
      const packs = amountWindows.filter(w => /pack/i.test(text(w.label)) && validDate(w.expiresAt));
      const sameExpiry = packs.length > 1 && packs.every(w => w.expiresAt === packs[0].expiresAt);
      const sampledAt = account.sampledAt || account.fetchedAt;
      const normal = account.status === 'ok' || account.status === 'cached';
      const stale = validDate(sampledAt) && now - Date.parse(sampledAt) > STALE_MS;
      // The plan without the provider's own name ("Muse Code High Usage" reads "High Usage").
      const plan = planLabel(text(account.plan).replace(new RegExp(`^${label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*`, 'i'), ''));
      return {
        id: account.id, provider: account.provider, accountId: account.id, label,
        identity: text(account.email) || text(account.label) || 'Identity unavailable', plan,
        status: statusWord(account),
        source: [account.status === 'cached' ? `Cached · sampled ${relative(sampledAt, now)}` : statusWord(account), text(account.source)].filter(Boolean).join(' · '),
        flag: !normal ? statusWord(account) : stale ? 'Stale' : '',
        sampled: validDate(sampledAt) ? `sampled ${relative(sampledAt, now)}` : 'never sampled',
        platform: platformLabel(account.platform),
        planNote: planExpiry ? exact(planExpiry.expiresAt, 'Plan subscription ends ') : '',
        packsNote: sameExpiry ? exact(packs[0].expiresAt, `All ${packs.length} packs expire `) : '',
        note: text(account.message) || (meters.length ? '' : 'Usage unavailable'),
        meters,
        amounts: amountWindows.map(w => amountView(account, w, now, { noExpiry: sameExpiry && packs.includes(w) })),
      };
    });
}

/** Header status line: "Updated 1m ago · cached readings". */
export function chromeView(data, { now = Date.now(), refreshing = false, intervalSeconds = 60, username = '', host = '' } = {}) {
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const cached = accounts.some(account => account.status === 'cached');
  const updated = validDate(data?.updatedAt) ? data.updatedAt : null;
  if (refreshing) return { statusLead: 'Refreshing', statusStrong: `${accounts.length} accounts`, statusMore: '', statusTip: 'Asking every provider for fresh readings', refreshing: true, username, host };
  if (!updated) return { statusLead: 'Account data unavailable', statusStrong: '', statusMore: '', statusTip: '', refreshing: false, username, host };
  return {
    statusLead: 'Updated', statusStrong: relative(updated, now), statusMore: cached ? 'cached readings' : 'live',
    statusTip: `Last refresh ${dateTime.format(new Date(updated))} · ${cached ? 'cached readings' : 'live'} · every ${intervalLabel(intervalSeconds)}`,
    refreshing: false, username, host,
  };
}

/**
 * The complete Home view model.
 * ctx: { profiles, platform, antigravityInventory, antigravityAuto, now, refreshing, intervalSeconds, username, host }
 */
export function dashboardViewModel(data, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const hidden = hiddenProviders(data);
  const of = provider => accounts.filter(account => account.provider === provider);
  const sections = [];
  if (!hidden.has('claude')) sections.push(claudeSection(of('claude'), ctx.profiles, ctx.platform || 'mac', now));
  if (!hidden.has('codex')) sections.push(codexSection(of('codex'), data, now));
  if (!hidden.has('antigravity') && of('antigravity').length) sections.push(antigravitySection(of('antigravity'), data, ctx.antigravityInventory, ctx.antigravityAuto, now));
  return {
    version: VIEW_MODEL_VERSION,
    sections,
    cards: providerCards(accounts, hidden, now),
    registry: providerRegistry(data),
    chrome: chromeView(data, { now, refreshing: ctx.refreshing, intervalSeconds: ctx.intervalSeconds, username: ctx.username, host: ctx.host }),
  };
}

/** Details for one account: every visible window, the amounts, and where the reading came from. */
export function detailsViewModel(data, id, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const account = accounts.find(row => row.id === id) || accounts.find(row => row.provider === id);
  if (!account) return null;
  const home = dashboardViewModel(data, ctx);
  const row = home.sections.flatMap(section => section.rows).find(r => r.id === account.id);
  const section = home.sections.find(s => s.rows.some(r => r.id === account.id));
  const switchable = SWITCHABLE.includes(account.provider);
  const notches = new Map((row?.cells || []).filter(cell => cell.key).map(cell => [cell.key, cell]));
  const meters = visibleMeters(account).map(w => {
    const view = meterView(account, w, { now, label: fullWindowLabel(account.provider, w) });
    const fromRow = notches.get(view.key);
    if (fromRow) { view.notch = fromRow.notch; view.notchFaint = fromRow.notchFaint; }
    return view;
  });
  const provider = PROVIDER_REGISTRY.find(p => p.id === account.provider);
  const state = row?.active ? `Active ${row.activeLabel}` : statusWord(account);
  const profile = account.capabilities?.claudeProfileId || account.capabilities?.codexProfile || account.capabilities?.antigravityProfileId || '';
  const facts = [
    { label: 'Status', value: statusWord(account), mono: false },
    { label: 'Sampled', value: validDate(account.sampledAt) ? `${dateTime.format(new Date(account.sampledAt))} · ${relative(account.sampledAt, now)}` : 'Never', mono: false },
    { label: 'Fetched', value: validDate(account.fetchedAt) ? dateTime.format(new Date(account.fetchedAt)) : 'Never', mono: false },
    { label: 'Source', value: text(account.source) || 'Unknown', mono: false },
    { label: 'Platform', value: platformLabel(account.platform), mono: false },
    ...(profile ? [{ label: 'Profile', value: profile, mono: true }] : []),
    ...(text(account.message) ? [{ label: 'Note', value: text(account.message), mono: false }] : []),
  ];
  const missingFable = isMaxPlan(account) && !visibleMeters(account).some(isFable);
  return {
    id: account.id, provider: account.provider,
    title: text(account.email) || text(account.label) || 'Account identity unavailable',
    sub: [provider?.label || text(account.providerLabel), planLabel(text(account.plan)), state].filter(Boolean).join(' · '),
    subLead: [provider?.label || text(account.providerLabel), planLabel(text(account.plan))].filter(Boolean).join(' · '),
    state, active: !!row?.active, switchable, canActivate: !!row?.canActivate, activateKind: row?.activateKind || '',
    // the row's own action slot and inline confirmation, so Details reads and switches the same way
    canSwitch: !!section?.canSwitch, activeLabel: row?.activeLabel || '', activateHint: row?.activateHint || '',
    platform: platformLabel(account.platform), confirm: !!row?.confirm, confirmRuns: row?.confirmRuns || [],
    profile: row?.profile || profile, canMac: !!row?.canMac, canWindows: !!row?.canWindows,
    note: missingFable ? 'Fable usage is not reported yet. It appears here as its own weekly window once the dashboard sends one.' : '',
    meters, amounts: visibleAmounts(account).map(w => amountView(account, w, now)), facts,
    sectionId: section?.id || '',
  };
}

/** Update apps button state from /api/app-updates/status. */
export function updateViewModel(job, { done = false } = {}) {
  const results = Array.isArray(job?.results) ? job.results : [];
  const failed = results.filter(row => row.status === 'failed' || row.status === 'restart_failed').length;
  const running = job?.state === 'running';
  const tip = running ? `Updating apps on ${platformLabel(job.activePlatform || '') || 'the next computer'} · running apps may restart`
    : job ? `Last run ${job.state}: ${results.length} results${failed ? `, ${failed} failed` : ''}. Details under Accounts & Settings.`
      : 'Update the Claude and Codex apps and CLIs on Mac, Windows and Ubuntu';
  return { running, done: !running && done, count: results.length, total: 21, tip, summary: job ? `${results.length} results${failed ? ` · ${failed} failed` : ''}` : '' };
}
