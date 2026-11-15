import { HOST_IDS, HOST_LABELS } from './computers.mjs';
// Dashboard view model, version 3 (Daylight Atlas; version 3 dropped account switching). Pure functions from the public API DTOs to the
// JSON that src/lib.rs deserializes into the Slint structs in ui/models.slint. Every truthfulness rule
// lives here or in the modules it builds on: a missing reading is unavailable, never zero; nothing is
// added or averaged across accounts or windows; at most two decimals; the product visibility rules of
// visible-usage.mjs; Fable only for Claude Max plans, from the seven_day_fable window.
import { visibleUsageWindows, isWeeklyScoped, scopedModelLabel, clearsText } from './visible-usage.mjs';
import { claudeSignInFacts, claudeSignInGroup, claudeSignInLine, claudeSignInNote, needsFor } from './claude-signins.mjs';
import { isResetWindow, resetRowsOf, resetsMode } from './reset-rows.mjs';
import { cursorPlanLabel, cursorPlanNote } from './cursor-data.mjs';
import {
  antigravityView,
  antigravityColumns,
  antigravityWindowLabel,
  antigravityMissingWindow,
  antigravityNoQuota,
  antigravityPlanFacts,
  antigravityPlanNote,
  antigravityCreditsHint,
} from './antigravity-data.mjs';
import { lazyFormat, zonedParts } from './time-format.mjs';

export const VIEW_MODEL_VERSION = 3;

/** The provider registry, derived client-side until the backend sends one (see CONTRACT notes in W1). */
export const PROVIDER_REGISTRY = [
  {
    id: 'claude',
    label: 'Claude',
    longLabel: 'Claude',
    signIn: 'Desktop profile',
    multi: true,
  },
  {
    id: 'codex',
    label: 'Codex',
    longLabel: 'Codex',
    signIn: 'Device-code sign-in',
    multi: true,
  },
  {
    id: 'antigravity',
    label: 'Antigravity',
    longLabel: 'Google Antigravity CLI',
    signIn: 'Supervised CLI login',
    multi: true,
  },
  {
    id: 'cursor',
    label: 'Cursor',
    longLabel: 'Cursor',
    signIn: 'Desktop app session',
    multi: false,
  },
  {
    id: 'muse',
    label: 'Muse Code',
    longLabel: 'Muse Code',
    signIn: 'Desktop app session',
    multi: false,
  },
  {
    id: 'kimi-code',
    label: 'Kimi Code',
    longLabel: 'Kimi Code',
    signIn: 'API key',
    multi: false,
  },
  {
    id: 'qwen',
    label: 'Qwen Token Plan',
    longLabel: 'Qwen Token Plan',
    signIn: 'API key, web extras by browser extension',
    multi: false,
  },
  {
    id: 'zai',
    label: 'Z.ai Coding Plan',
    longLabel: 'Z.ai Coding Plan',
    signIn: 'API key',
    multi: false,
  },
  {
    id: 'opencode-go',
    label: 'OpenCode Go',
    longLabel: 'OpenCode Go',
    signIn: 'API key, console wallet by browser session',
    multi: true,
  },
];
const CARD_PROVIDERS = ['cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go'];

// ---------------------------------------------------------------- formatting
const finite = (value) => typeof value === 'number' && Number.isFinite(value);
const validDate = (value) => typeof value === 'string' && Number.isFinite(Date.parse(value));
const text = (value) => (typeof value === 'string' ? value : '');
const nf2 = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const nfCompact = new Intl.NumberFormat(undefined, {
  notation: 'compact',
  maximumFractionDigits: 2,
});
const money = new Intl.NumberFormat(undefined, {
  style: 'currency',
  currency: 'USD',
  maximumFractionDigits: 2,
});
const DATE_TIME = { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' };
const CLOCK_TIME = { hour: 'numeric', minute: '2-digit' };
const DAY_CLOCK = { weekday: 'short', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' };
const dateTime = lazyFormat(DATE_TIME);
const clockTime = lazyFormat(CLOCK_TIME);
const dayClock = lazyFormat(DAY_CLOCK);
/**
 * What this browser's formatters write for fixed values (UTC instants), in order: two decimals, compact, money, the
 * date and time, the clock, the day and clock, the day (claude-signins.mjs), and the collation of four letters. The
 * server builds Home (aac-views::home) only when its en-US formatters write exactly these.
 */
export function homeFormatProbe() {
  const utc = (options) => new Intl.DateTimeFormat(undefined, { ...options, timeZone: 'UTC' });
  const first = Date.UTC(2026, 0, 5, 0, 7);
  const second = Date.UTC(2026, 0, 5, 13, 7);
  return [
    nf2.format(-1234.567),
    nfCompact.format(1234567),
    money.format(-1234.5),
    utc(DATE_TIME).format(first),
    utc(CLOCK_TIME).format(second),
    utc(DAY_CLOCK).format(second),
    utc({ month: 'short', day: 'numeric' }).format(first),
    ['b', 'a', 'B', '_'].sort((a, b) => a.localeCompare(b)).join(''),
  ];
}
const PLATFORM = { ...HOST_LABELS, linux: 'Linux' };

export const platformLabel = (id) =>
  PLATFORM[id] || (id ? id[0].toUpperCase() + id.slice(1) : 'Unknown');
/**
 * The computer an account's readings come from: its display-only `host` (Nas1, a second Ubuntu
 * computer whose sources keep platform 'ubuntu'), else its platform.
 */
export const hostLabel = (account) => platformLabel(account?.host ?? account?.platform);
export const planLabel = (plan, provider) =>
  provider === 'cursor'
    ? cursorPlanLabel(plan)
    : !plan
      ? ''
      : plan.length <= 5
        ? plan[0].toUpperCase() + plan.slice(1)
        : plan;
/** At most two decimals, locale grouping, never rounding a real reading up to a different integer label. */
export const valueText = (value) => (finite(value) ? nf2.format(value) : '');
export function duration(ms) {
  const m = Math.floor(Math.max(0, ms) / 60000),
    h = Math.floor(m / 60),
    d = Math.floor(h / 24);
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
const exact = (value, prefix) =>
  validDate(value) ? `${prefix}${dateTime.format(new Date(value))}` : '';
export function intervalLabel(seconds) {
  if (seconds < 60) return `${seconds} s`;
  return seconds % 60 === 0
    ? `${seconds / 60} min`
    : `${Math.floor(seconds / 60)} min ${seconds % 60} s`;
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
const compactAmount = (value) =>
  Math.abs(value) >= 1e6 ? nfCompact.format(value) : nf2.format(value);
/** A run of text in a line that mixes weights ("**4** accounts · desktop profiles"); tone '' | 'good' | 'warn'. */
export const run = (value, strong = false, tone = '') => ({
  text: String(value),
  strong: !!strong,
  tone,
});
/**
 * The same runs as wrap units for the touch layouts: Slint cannot split strings, so each word
 * becomes its own run (whitespace collapses; the layout adds the spaces back between words).
 * Desktop keeps `runs` untouched.
 */
export const wordsOf = (runs) =>
  (runs || []).flatMap((r) =>
    String(r?.text ?? '')
      .split(/\s+/)
      .filter((word) => word !== '')
      .map((word) => run(word, !!r?.strong, r?.tone || ''))
  );
const STALE_MS = 30 * 60_000;

// ---------------------------------------------------------------- windows
export const usedPercent = (w) =>
  finite(w?.usedPercent) && w.usedPercent >= 0
    ? w.usedPercent
    : finite(w?.remainingPercent) && w.remainingPercent >= 0 && w.remainingPercent <= 100
      ? 100 - w.remainingPercent
      : null;
const description = (w) => `${text(w?.key)} ${text(w?.label)}`;
export const isFable = (w) => w?.key === 'seven_day_fable';
/** Meters are rate limits; packs, balances, credits and spend are amounts, never fake meters. */
export const isMeterWindow = (w) =>
  !['balance', 'extra_usage', 'spend'].includes(w?.kind) && w?.unlimited !== true;
export function period(w) {
  // An explicit null duration degrades honestly: only the window key (which names its
  // cadence) can settle the period, never the free-text label.
  const d = w?.windowMinutes === null ? text(w?.key) : description(w);
  if (w?.windowMinutes === 300 || /five.?hour|5.?hour|\b5h\b|5 hours|rolling/i.test(d)) return '5h';
  if (w?.windowMinutes === 10080 || /seven.?day|weekly|\bweek\b|7.?day/i.test(d)) return 'week';
  if (/month/i.test(d)) return 'month';
  return 'other';
}
export const isMaxPlan = (account) =>
  account?.provider === 'claude' &&
  (account?.planClass != null
    ? account.planClass === 'max'
    : /(?:^|\s)max(?:\s*(?:5|20)x)?(?:$|\s)/i.test(
        String(account.plan || '').replace(/[_-]/g, ' ')
      ));
const PERIOD_LABEL = { '5h': '5-hour', week: 'Weekly', month: 'Monthly' };
export function windowLabel(provider, w) {
  if (!w) return 'Usage';
  if (isFable(w)) return 'Fable';
  if (isWeeklyScoped(w)) return scopedModelLabel(w);
  const p = period(w);
  if (provider === 'antigravity') return antigravityWindowLabel(w);
  if (provider === 'cursor')
    return (
      {
        'plan-reported': 'Included',
        autoPercentUsed: 'Cursor models',
        apiPercentUsed: 'Other models',
      }[w?.key] || text(w?.label) || 'Usage'
    );
  if (provider === 'zai')
    return (
      { 'usage-1': '5h tokens', 'usage-2': 'Weekly tokens', 'usage-3': 'Monthly requests' }[
        w?.key
      ] || text(w?.label) || 'Usage'
    );
  if (
    provider === 'codex' ||
    provider === 'claude' ||
    provider === 'kimi-code' ||
    provider === 'opencode-go' ||
    provider === 'muse' ||
    provider === 'qwen'
  )
    return PERIOD_LABEL[p] || text(w?.label) || 'Usage';
  return text(w?.label) || 'Usage';
}
/**
 * H3: single-line column heads for the stacked Antigravity grid. Only Antigravity
 * has its own shorts; every other provider keeps its full window label.
 */
export function shortWindowLabel(provider, w) {
  if (provider !== 'antigravity') return windowLabel(provider, w);
  // space, not +: crowded table heads wrap between the words, never mid-word
  return (
    {
      'gemini-5h': 'Gemini 5hr',
      'gemini-weekly': 'Gemini wk',
      '3p-5h': 'Claude GPT 5hr',
      '3p-weekly': 'Claude GPT wk',
    }[w?.key] || windowLabel(provider, w)
  );
}
export function fullWindowLabel(provider, w) {
  if (!w) return 'Usage';
  if (isFable(w)) return 'Fable weekly';
  if (provider === 'antigravity' && w?.key === 'google-ai-credits') return antigravityWindowLabel(w);
  if (provider === 'codex')
    return (
      { week: 'Weekly', '5h': '5-hour' }[text(w?.label)] || text(w?.label) || windowLabel(provider, w)
    );
  if (provider === 'claude')
    return { 'Five-hour usage': '5-hour usage' }[text(w?.label)] || text(w?.label) || 'Usage';
  return text(w?.label) || 'Usage';
}
/**
 * Claude only: the computers the server says need a sign-in before Open (`accounts[].signInNeeded`), as
 * "Mac", "Windows" or "Mac and Windows"; '' when none. Each computer's profile signs in on its own, and
 * Open stays allowed: it shows Claude's own sign-in window there.
 */
export function claudeSignInNeeded(account) {
  const list = Array.isArray(account?.signInNeeded) ? account.signInNeeded : [];
  return ['mac', 'windows']
    .filter((p) => list.includes(p))
    .map((p) => (p === 'mac' ? 'Mac' : 'Windows'))
    .join(' and ');
}
export function statusWord(account) {
  return (
    {
      ok: 'Live',
      cached: 'Cached',
      needs_sign_in: 'Sign-in needed',
      error: 'Refresh failed',
      unavailable: 'Unavailable',
    }[account?.status] || 'Unavailable'
  );
}
/**
 * F6: a meter window whose reset time has passed while its reading was sampled before that reset (or at an
 * unknown time) no longer says anything about the new period. Its old percent and fill are hidden and it
 * reads "Reset at 10:15 AM · new reading pending" until a reading sampled after the reset arrives. Amounts,
 * unlimited and switched-off windows are left alone, and nothing is ever shown as 0%.
 */
export function pendingReset(account, w, now = Date.now()) {
  if (!w || !isMeterWindow(w) || w.enabled === false || usedPercent(w) === null) return false;
  if (!validDate(w.resetAt) || Date.parse(w.resetAt) > now) return false;
  const at = validDate(w.sampledAt)
    ? w.sampledAt
    : validDate(account?.sampledAt)
      ? account.sampledAt
      : null;
  return !at || Date.parse(at) < Date.parse(w.resetAt);
}
/** A window's "% used" for comparisons and labels: null while a reset is pending, like any missing reading. */
export const currentUsedPercent = (account, w, now = Date.now()) =>
  pendingReset(account, w, now) ? null : usedPercent(w);
/** "10:15 AM" today, "Thu, Oct 1, 10:15 AM" on another day. */
export function resetClock(value, now = Date.now()) {
  const time = new Date(value);
  const day = zonedParts(time.getTime());
  const today = zonedParts(now);
  return day.year === today.year && day.month === today.month && day.day === today.day
    ? clockTime.format(time)
    : dayClock.format(time);
}
function sampledText(account, w, now) {
  const at = validDate(w?.sampledAt)
    ? w.sampledAt
    : validDate(account?.sampledAt)
      ? account.sampledAt
      : null;
  if (!at) return '';
  return `${dateTime.format(new Date(at))} · ${relative(at, now)}`;
}

/** One window as a MeterView. */
export function meterView(
  account,
  w,
  {
    now = Date.now(),
    naText = 'Unavailable',
    naSub = '',
    label,
  } = {}
) {
  const pending = pendingReset(account, w, now);
  const value = w && !pending ? usedPercent(w) : null;
  const hasValue = value !== null && w?.unlimited !== true;
  const unit = text(w?.unit);
  // A limit of 100 with no unit only restates the meter's own percentage ("99 of 100" under a 99%
  // bar), so cards show no caption for it. Counts with a real limit or a named unit stay.
  const restatesPercent = w?.limit === 100 && !unit;
  const amount =
    !pending && !restatesPercent && finite(w?.used) && finite(w?.limit) && w.limit > 0
      ? `${nfCompact.format(w.used)} of ${compactAmount(w.limit)}${unit ? ` ${unit}` : ''}`
      : '';
  return {
    key: `${account?.id || ''}|${text(w?.key) || 'missing'}`,
    label: label || (w ? windowLabel(account?.provider, w) : 'Usage'),
    fullLabel: w ? fullWindowLabel(account?.provider, w) : label || 'Usage',
    hasValue,
    value: hasValue ? value : 0,
    valueText: hasValue ? valueText(value) : '',
    overText: hasValue && value > 100 ? valueText(value - 100) : '',
    reset: hasValue ? resetIn(w?.resetAt, now) || 'no reset reported' : '',
    resetExact: pending
      ? `Reset at ${resetClock(w.resetAt, now)} · new reading pending`
      : exact(w?.resetAt, 'Resets '),
    resetSoon:
      validDate(w?.resetAt) &&
      Date.parse(w.resetAt) - now < 2 * 3_600_000 &&
      Date.parse(w.resetAt) > now,
    naText: hasValue ? '' : pending ? `Reset at ${resetClock(w.resetAt, now)}` : naText,
    naSub: hasValue ? '' : pending ? 'New reading pending' : naSub,
    amount,
    left: !pending && finite(w?.remaining) && unit ? `${unitAmount(w.remaining, unit)} left` : '',
    sampled: sampledText(account, w, now),
    source: [
      text(account?.source) || 'Unknown source',
      hostLabel(account),
      w?.status === 'cached' ? 'Cached window' : statusWord(account),
    ].join(' · '),
    caption: '',
    captionTip: '',
  };
}
const emptyCell = (account, key) => ({
  key: '',
  label: '',
  fullLabel: '',
  hasValue: false,
  value: 0,
  valueText: '',
  overText: '',
  reset: '',
  resetExact: '',
  resetSoon: false,
  naText: '',
  naSub: '',
  amount: '',
  left: '',
  sampled: '',
  source: '',
  caption: '',
  captionTip: '',
});

/** A Z.ai row that counts all reset packs of one kind (the collector's roll-up). */
const rolledUpPacks = (account, w) =>
  account?.provider === 'zai' && /^reset-packs-(?:5h|weekly)$/.test(text(w?.key));

// Only an amount kept from an earlier reading says so: a whole cached account already says it on its row.
const cacheProvenance = (account, w, now) => {
  if (w?.status !== 'cached') return '';
  const at = validDate(w?.sampledAt) ? w.sampledAt : account?.sampledAt;
  return validDate(at) ? `cached, sampled ${relative(at, now)}` : 'cached';
};
const expiryText = (value, now, prefix = 'Expires ') =>
  exact(value, validDate(value) && Date.parse(value) <= now ? 'Reported expiry passed ' : prefix);

/** Packs, balances, credits and spend as an AmountView. */
export function amountView(account, w, now = Date.now(), { noExpiry = false } = {}) {
  if (!w || typeof w !== 'object') return { key: `${account?.id || ''}|missing`, label: 'Balance', value: 'Unavailable', unit: '', sub: '', icon: 'wallet', spent: false };
  const unit = text(w?.unit);
  const label = text(w?.label)
    .replace(/^Additional credit pack/i, 'Credit pack')
    .replace(/^Listed active credit packs/i, 'Active packs listed')
    .replace(/^Rate-limit resets available/i, 'Rate-limit resets left');
  const expires = noExpiry ? '' : expiryText(w?.expiresAt, now);
  let value = '',
    suffix = '',
    sub = '',
    icon = 'wallet',
    spent = false;
  if (rolledUpPacks(account, w) && finite(w?.limit)) {
    // Every Z.ai reset pack of one kind on one line: how many are left, how many were used and
    // the soonest expiry among those left.
    icon = 'pack';
    value = unitAmount(w?.remaining, null) || 'Unavailable';
    suffix = `of ${unitAmount(w?.limit, unit)} left`;
    spent = w?.remaining === 0;
    sub = [
      finite(w?.used) && w.used > 0 ? `${nf2.format(w.used)} used` : '',
      noExpiry ? '' : expiryText(w?.expiresAt, now, w?.remaining > 1 ? 'Next expires ' : 'Expires '),
    ]
      .filter(Boolean)
      .join(' · ');
  } else if (/pack/i.test(text(w?.label)) && finite(w?.limit)) {
    icon = 'pack';
    value = unitAmount(w?.remaining, null) || 'Unavailable';
    suffix = `of ${unitAmount(w?.limit, unit)} left`;
    spent = w?.remaining === 0;
    sub = [spent ? 'Used up' : '', expires].filter(Boolean).join(' · ');
  } else if (w?.kind === 'spend') {
    value = unitAmount(w?.used, unit) || 'Unavailable';
    suffix = finite(w?.limit) ? `of ${unitAmount(w?.limit, unit)} spent` : 'spent';
    sub = exact(w?.resetAt, 'Resets ');
  } else if (w?.kind === 'extra_usage') {
    icon = 'zap';
    if (w?.enabled === false) {
      value = 'Off';
      sub = finite(w?.limit)
        ? `Limit ${unitAmount(w?.limit, unit)}`
        : finite(w?.used) && unit
          ? `${unitAmount(w?.used, unit)} used`
          : '';
    } else {
      value = finite(w?.used) ? unitAmount(w?.used, unit) : 'Unavailable';
      suffix = finite(w?.used) ? 'used' : '';
      sub = finite(w?.limit) ? `of ${unitAmount(w?.limit, unit)}` : '';
    }
  } else if (unit === 'resets') {
    icon = 'rotate';
    if (finite(w?.used) && finite(w?.limit)) {
      value = nf2.format(w.used);
      suffix = `of ${unitAmount(w?.limit, 'resets')} used`;
    } else value = unitAmount(w?.remaining, 'resets') || 'Unavailable';
    // The summary (value + suffix) stays; refill targets and the next-available time ride in `sub`.
    sub = [
      expires,
      clearsText(w),
      w?.key === 'session_reset_allowance' ? exact(w?.resetAt, 'Next available ') : '',
    ]
      .filter(Boolean)
      .join(' · ');
  } else if (unit === 'packs') {
    icon = 'pack';
    value = unitAmount(w?.remaining, 'packs') || 'Unavailable';
  } else {
    value = finite(w?.remaining)
      ? unitAmount(w?.remaining, unit)
      : finite(w?.used)
        ? `${unitAmount(w?.used, unit)} used`
        : 'Unavailable';
    sub = [w?.enabled === false ? 'Off' : '', expires].filter(Boolean).join(' · ');
  }
  return {
    key: `${account?.id || ''}|${text(w?.key)}`,
    label:
      account?.provider === 'antigravity' && w?.key === 'google-ai-credits'
        ? antigravityWindowLabel(w)
        : label || 'Balance',
    value,
    unit: suffix,
    sub: [
      sub,
      account.provider === 'antigravity' && w.key === 'google-ai-credits'
        ? antigravityCreditsHint(account)
        : '',
      cacheProvenance(account, w, now),
    ]
      .filter(Boolean)
      .join(' · '),
    icon,
    spent,
  };
}

/** Codex credits and banked resets of one account, as runs with the figures in bold ("**62.5K** credits · **1** banked"). */
function codexAmountsRuns(account, now) {
  const windows = visibleUsageWindows('codex', account.windows);
  const credits = windows.find(
    (w) => w.key === 'credits_balance' || (w.kind === 'balance' && w.unit === 'credits')
  );
  // The backend groups resets by expiration. Every group belongs to this account's total.
  const banked = windows.filter((w) => /^banked[_-]?resets/i.test(text(w.key)));
  const bankedTotal =
    banked.length && banked.every((w) => finite(w.remaining) && w.remaining >= 0)
      ? banked.reduce((total, w) => total + w.remaining, 0)
      : null;
  const parts = [];
  if (credits) {
    if (credits.enabled === false) parts.push([run('Credits off')]);
    else if (finite(credits.remaining))
      parts.push([
        run(
          credits.remaining >= 10000
            ? nfCompact.format(credits.remaining)
            : nf2.format(credits.remaining),
          true
        ),
        run(' credits'),
      ]);
  }
  if (bankedTotal !== null) parts.push([run(nf2.format(bankedTotal), true), run(' banked')]);
  const provenance = [
    ...new Set(
      [credits, ...banked]
        .filter(Boolean)
        .map((w) => cacheProvenance(account, w, now))
        .filter(Boolean)
    ),
  ];
  for (const note of provenance) parts.push([run(note)]);
  return parts.flatMap((part, index) => (index ? [run(' · '), ...part] : part));
}
const runsText = (runs) => runs.map((r) => r.text).join('');
const noFoot = () => ({ shown: false, warn: false, runs: [], words: [], when: '' });

const visibleMeters = (account) =>
  visibleUsageWindows(account.provider, account.windows).filter(isMeterWindow);
const visibleAmounts = (account) =>
  visibleUsageWindows(account.provider, account.windows).filter((w) => !isMeterWindow(w));
const shortIdentity = (account) =>
  (text(account.email) || text(account.label) || 'Account').replace(/@.*/, '');

/** Hidden providers when the backend sends them (`settings.hiddenProviders`, see the W1 contract notes). */
export function hiddenProviders(data) {
  const hidden = data?.settings?.hiddenProviders;
  return new Set(
    Array.isArray(hidden)
      ? hidden.filter((id) => PROVIDER_REGISTRY.some((row) => row.id === id))
      : []
  );
}
export function providerRegistry(data) {
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const hidden = hiddenProviders(data);
  return PROVIDER_REGISTRY.map((row) => ({
    ...row,
    visible: !hidden.has(row.id),
    count: accounts.filter((account) => account.provider === row.id).length,
  }));
}

// ---------------------------------------------------------------- sections
/** The row line of a Claude Open in progress (claude-open.mjs), or ''. */
function openLine(openProgress, profileId) {
  const view = profileId && openProgress instanceof Map ? openProgress.get(profileId) : null;
  return view && typeof view.text === 'string' ? view.text : '';
}
function claudeSection(accounts, profiles, platform, now, openProgress, data = null) {
  const anyMax = accounts.some(isMaxPlan);
  const byAccount = accounts.map((account) => visibleMeters(account).filter((w) => w && typeof w === 'object'));
  const hasFive = accounts.some((_, index) =>
    byAccount[index].some((w) => w && !isFable(w) && !isWeeklyScoped(w) && period(w) === '5h')
  );
  const hasWeekly = accounts.some((_, index) =>
    byAccount[index].some((w) => w?.key === 'seven_day')
  );
  // Weekly-scoped models (Opus, Sonnet, ...) in first-seen order, Fable apart: Fable's column
  // follows anyMax and is never shown on Pro rows, like before.
  const scopedKeys = [];
  for (const meters of byAccount)
    for (const w of meters)
      if (w && isWeeklyScoped(w) && !isFable(w) && !scopedKeys.includes(w?.key)) scopedKeys.push(w.key);
  const columns = [
    ...(hasFive ? [{ key: 'five', label: '5-hour' }] : []),
    ...(hasWeekly ? [{ key: 'weekly', label: 'Weekly' }] : []),
    ...scopedKeys.map((key) => {
      const sample = byAccount.flat().find((w) => w?.key === key);
      return { key, label: scopedModelLabel(sample) };
    }),
    ...(anyMax ? [{ key: 'seven_day_fable', label: 'Fable' }] : []),
  ].slice(0, 6);
  const rows = accounts.map((account, index) => {
    const meters = byAccount[index];
    const five = meters.find((w) => w && !isFable(w) && !isWeeklyScoped(w) && period(w) === '5h');
    const weekly = meters.find((w) => w?.key === 'seven_day');
    const fable = meters.find(isFable);
    const launcher = (Array.isArray(profiles) ? profiles : []).find(
      (profile) => profile.id === account.capabilities?.claudeProfileId
    );
    const cells = columns.map((col) => {
      if (col.key === 'five')
        return five ? meterView(account, five, { now, label: '5-hour' }) : emptyCell(account, 'five_hour');
      if (col.key === 'weekly')
        return weekly ? meterView(account, weekly, { now, label: 'Weekly' }) : emptyCell(account, 'seven_day');
      if (col.key === 'seven_day_fable')
        return isMaxPlan(account)
          ? meterView(account, fable, { now, label: 'Fable', naText: 'Not reported yet' })
          : emptyCell(account, 'seven_day_fable');
      const scoped = meters.find((w) => w.key === col.key);
      return scoped ? meterView(account, scoped, { now, label: col.label }) : emptyCell(account, col.key);
    });
    const opening = openLine(openProgress, account.capabilities?.claudeProfileId);
    const signIn = claudeSignInNeeded(account);
    const renewal = claudeSignInLine(data, account.email, now);
    return {
      id: account.id,
      provider: 'claude',
      profile: account.capabilities?.claudeProfileId || '',
      email: text(account.email) || text(account.label) || 'Account identity unavailable',
      plan: text(account.plan),
      // While an Open runs, its progress replaces the meta line ("Copying history 3 of 18", "Opening on Mac").
      meta:
        opening ||
        [
          planLabel(text(account.plan)),
          account.status === 'ok' || account.status === 'cached'
            ? relative(account.sampledAt || account.fetchedAt, now)
            : signIn
              ? ''
              : statusWord(account),
          signIn ? `Sign-in needed on ${signIn}` : '',
          renewal,
        ]
          .filter(Boolean)
          .join(' · '),
      status: signIn ? `Sign-in needed on ${signIn}` : statusWord(account),
      note: text(account.message),
      platform: text(account.platform),
      active: false,
      activeLabel: '',
      setup: false,
      canMac: !!launcher?.mac?.canOpen,
      canWindows:
        launcher?.windows?.canOpen === true ||
        (platform === 'windows' && !!launcher?.windows?.launchUri),
      amountsLine: '',
      amountsRuns: [],
      amountsWords: [],
      cells,
    };
  });
  const needs = needsFor(data);
  const metaRuns = [
    run(rows.length, true),
    run(` ${rows.length === 1 ? 'account' : 'accounts'} · desktop profiles`),
    ...(needs.length
      ? [run(' · '), run(`${needs.length} sign-in ${needs.length === 1 ? 'step needs' : 'steps need'} you`, true, 'warn')]
      : []),
  ];
  return {
    id: 'claude',
    kind: 'claude',
    label: 'Claude',
    longLabel: 'Claude',
    meta: runsText(metaRuns),
    metaRuns,
    metaWords: wordsOf(metaRuns),
    foot: noFoot(),
    activeId: '',
    activeLabel: '',
    empty: 'No Claude accounts are reported yet.',
    columns,
    rows,
  };
}

// `allAccounts` includes the ones hidden from the dashboard too: the live account is named even when its row is
// hidden.
function codexSection(accounts, now, allAccounts = accounts) {
  const hiddenIds = new Set(
    allAccounts.filter((account) => !accounts.includes(account)).map((account) => account.id)
  );
  // Only the canonical keys identify the columns; additional quotas with the same duration stay in Details.
  const hasFive = accounts.some((account) =>
    visibleUsageWindows('codex', account.windows).some(
      (w) => w.key === 'five_hour' && usedPercent(w) !== null
    )
  );
  const columns = [
    ...(hasFive ? [{ key: 'five', label: '5-hour' }] : []),
    { key: 'weekly', label: 'Weekly' },
  ];
  const active = allAccounts.find((account) => account.isActive === true);
  const activeName = active
    ? `${shortIdentity(active)}${hiddenIds.has(active.id) ? ' (hidden)' : ''}`
    : '';
  const rows = accounts.map((account) => {
    const windows = visibleUsageWindows('codex', account.windows);
    const five = windows.find((w) => w.key === 'five_hour');
    const weekly = windows.find((w) => w.key === 'seven_day');
    const cells = [];
    if (hasFive)
      cells.push(
        five && usedPercent(five) !== null
          ? meterView(account, five, { now, label: '5-hour' })
          : emptyCell(account, 'five_hour')
      );
    const weeklyCell = meterView(account, weekly, { now, label: 'Weekly' });
    if (!weekly) weeklyCell.key = `${account.id}|seven_day`;
    cells.push(weeklyCell);
    const amountsRuns = codexAmountsRuns(account, now);
    return {
      id: account.id,
      provider: 'codex',
      profile: account.capabilities?.codexProfile || '',
      email: text(account.email) || text(account.label) || 'Account identity unavailable',
      plan: text(account.plan),
      meta: [
        planLabel(text(account.plan)),
        hostLabel(account),
        account.status === 'ok' || account.status === 'cached'
          ? relative(account.sampledAt || account.fetchedAt, now)
          : statusWord(account),
      ]
        .filter(Boolean)
        .join(' · '),
      status: statusWord(account),
      note: text(account.message),
      platform: hostLabel(account),
      // The live ~/.codex login on Ubuntu: a fact, not a control.
      active: account.isActive === true,
      activeLabel: 'on Ubuntu',
      setup: false,
      canMac: false,
      canWindows: false,
      amountsLine: runsText(amountsRuns),
      amountsRuns,
      amountsWords: wordsOf(amountsRuns),
      cells,
    };
  });
  const metaRuns = [
    run(rows.length, true),
    run(` ${rows.length === 1 ? 'account' : 'accounts'} · `),
    ...(active ? [run(activeName, true, 'good'), run(' active')] : [run('none active')]),
  ];
  return {
    id: 'codex',
    kind: 'accounts',
    label: 'Codex',
    longLabel: 'Codex',
    meta: `${rows.length} ${rows.length === 1 ? 'account' : 'accounts'}${active ? ` · ${activeName} active` : ''}`,
    metaRuns,
    metaWords: wordsOf(metaRuns),
    foot: noFoot(),
    activeId: active?.id || '',
    activeLabel: active
      ? `${text(active.email) || text(active.label)}${hiddenIds.has(active.id) ? ' (hidden)' : ''}`
      : '',
    empty: 'No Codex accounts are reported yet.',
    columns,
    rows,
  };
}

function antigravitySection(accounts, data, inventory, now) {
  const native = antigravityView(data, inventory);
  const bound = new Map(native.antigravityAccounts.map((row) => [row.id, row]));
  // Columns come from the canonical provider buckets; the short single-line heads ride
  // along, fed by each column's representative window (H3).
  const columns = antigravityColumns(accounts).map((col) => ({
    ...col,
    shortLabel: shortWindowLabel(
      'antigravity',
      accounts.flatMap((row) => visibleMeters(row)).find((w) => w.key === col.key) || {
        key: col.key,
        label: col.label,
      }
    ),
  }));
  const rows = accounts.map((account) => {
    const nativeRow = bound.get(account.id);
    // The live Antigravity login on Ubuntu: a fact, not a control.
    const active = nativeRow?.selected === true;
    const noQuota = antigravityNoQuota(account) ? 'No Antigravity quota on this plan' : '';
    const cells = noQuota
      ? columns.map(({ key }) => emptyCell(account, key))
      : columns.map(({ key, label }) => {
          const w = visibleMeters(account).find((row) => row.key === key);
          const cell = meterView(account, w, {
            now,
            label,
            naText: w ? 'Unavailable' : antigravityMissingWindow(account, key),
          });
          if (!w) cell.key = `${account.id}|${key}`;
          return cell;
        });
    return {
      id: account.id,
      provider: 'antigravity',
      profile: nativeRow?.profile || account.capabilities?.antigravityProfileId || '',
      email: text(account.email) || text(account.label) || 'Account identity unavailable',
      plan: text(account.plan),
      meta: [
        text(account.plan),
        hostLabel(account),
        account.status === 'ok' || account.status === 'cached'
          ? relative(account.sampledAt || account.fetchedAt, now)
          : statusWord(account),
      ]
        .filter(Boolean)
        .join(' · '),
      status: nativeRow?.status || statusWord(account),
      note: nativeRow?.note || text(account.message),
      platform: hostLabel(account),
      active,
      activeLabel: active
        ? nativeRow.runtimeVerified
          ? 'on Ubuntu'
          : 'on Ubuntu, running unverified'
        : '',
      setup: false,
      canMac: false,
      canWindows: false,
      amountsLine: noQuota,
      amountsRuns: noQuota ? [run(noQuota)] : [],
      amountsWords: noQuota ? wordsOf([run(noQuota)]) : [],
      cells,
    };
  });
  const activeRow = rows.find((row) => row.active);
  const metaRuns = [
    run('Google Antigravity CLI · '),
    run(rows.length, true),
    run(` ${rows.length === 1 ? 'account' : 'accounts'}`),
  ];
  const problem = native.antigravityServiceProblem || native.antigravityUpdatePaused;
  return {
    id: 'antigravity',
    kind: 'accounts',
    label: 'Antigravity',
    longLabel: 'Google Antigravity CLI',
    meta: `Google Antigravity CLI · ${rows.length} ${rows.length === 1 ? 'account' : 'accounts'}`,
    metaRuns,
    metaWords: wordsOf(metaRuns),
    // A runtime service that cannot start outranks a paused update.
    foot: problem
      ? { shown: true, warn: true, runs: [run(problem)], words: wordsOf([run(problem)]), when: '' }
      : noFoot(),
    activeId: activeRow?.id || '',
    activeLabel: activeRow?.email || '',
    empty: 'No Antigravity accounts are reported yet.',
    columns,
    rows,
  };
}

function providerCards(accounts, hidden, now) {
  const order = new Map(CARD_PROVIDERS.map((id, index) => [id, index]));
  return accounts
    .filter((account) => CARD_PROVIDERS.includes(account.provider) && !hidden.has(account.provider))
    .sort(
      (a, b) =>
        order.get(a.provider) - order.get(b.provider) || text(a.id).localeCompare(text(b.id))
    )
    .map((account) => {
      const provider = PROVIDER_REGISTRY.find((row) => row.id === account.provider);
      const label = provider?.label || text(account.providerLabel);
      // Up to three meters; a window with no reading is drawn as unavailable, never as zero. The rest stay in Details.
      const windows = visibleMeters(account).slice(0, 3);
      const meters = windows.map((w) => meterView(account, w, { now }));
      const planExpiry = visibleMeters(account).find((w) => w.planExpiry && validDate(w.expiresAt));
      // Packs that all expire together say so once instead of on every line.
      const amountWindows = visibleAmounts(account);
      // A rolled-up Z.ai row counts many packs and states its own soonest expiry.
      const packs = amountWindows.filter(
        (w) => /pack/i.test(text(w.label)) && validDate(w.expiresAt) && !rolledUpPacks(account, w)
      );
      const sameExpiry = packs.length > 1 && packs.every((w) => w.expiresAt === packs[0].expiresAt);
      const sampledAt = account.sampledAt || account.fetchedAt;
      const normal = account.status === 'ok' || account.status === 'cached';
      const stale = validDate(sampledAt) && now - Date.parse(sampledAt) > STALE_MS;
      // The plan without the provider's own name ("Muse Code High Usage" reads "High Usage").
      const plan = planLabel(
        text(account.plan).replace(
          new RegExp(`^${label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*`, 'i'),
          ''
        ),
        account.provider
      );
      return {
        id: account.id,
        provider: account.provider,
        accountId: account.id,
        label,
        identity: text(account.email) || text(account.label) || 'Identity unavailable',
        plan,
        status: statusWord(account),
        source: [
          account.status === 'cached'
            ? `Cached · sampled ${relative(sampledAt, now)}`
            : statusWord(account),
          text(account.source),
        ]
          .filter(Boolean)
          .join(' · '),
        flag: !normal ? statusWord(account) : stale ? 'Stale' : '',
        sampled: validDate(sampledAt) ? `sampled ${relative(sampledAt, now)}` : 'never sampled',
        platform: hostLabel(account),
        planNote: [
          cursorPlanNote(account),
          planExpiry ? exact(planExpiry.expiresAt, 'Plan subscription ends ') : '',
        ]
          .filter(Boolean)
          .join(' · '),
        packsNote: sameExpiry
          ? expiryText(packs[0].expiresAt, now, `All ${packs.length} packs expire `)
          : '',
        note: text(account.message) || (meters.length ? '' : 'Usage unavailable'),
        meters,
        amounts: amountWindows.map((w) =>
          amountView(account, w, now, { noExpiry: sameExpiry && packs.includes(w) })
        ),
      };
    });
}

/** Header status line: "Updated 1m ago · cached readings". */
export function chromeView(
  data,
  { now = Date.now(), refreshing = false, intervalSeconds = 60, username = '', host = '' } = {}
) {
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const cached = accounts.some((account) => account.status === 'cached');
  const updated = validDate(data?.updatedAt) ? data.updatedAt : null;
  if (refreshing)
    return {
      statusLead: 'Refreshing',
      statusStrong: Array.isArray(data?.accounts) ? `${accounts.length} accounts` : '',
      statusMore: '',
      statusTip: 'Asking every provider for fresh readings',
      refreshing: true,
      username,
      host,
    };
  if (!updated)
    return {
      statusLead: 'Account data unavailable',
      statusStrong: '',
      statusMore: '',
      statusTip: '',
      refreshing: false,
      username,
      host,
    };
  return {
    statusLead: 'Updated',
    statusStrong: relative(updated, now),
    statusMore: cached ? 'cached readings' : 'live',
    statusTip: `Last refresh ${dateTime.format(new Date(updated))} · ${cached ? 'cached readings' : 'live'} · every ${intervalLabel(intervalSeconds)}`,
    refreshing: false,
    username,
    host,
  };
}

/**
 * The complete Home view model.
 * ctx: { profiles, platform, antigravityInventory, now, refreshing, intervalSeconds, username, host, includeHidden }
 */
export function dashboardViewModel(data, ctx = {}) {
  const now = ctx.now ?? Date.now();
  // Accounts hidden one by one (`settings.hiddenAccountIds`, saved on the server) leave Home like hidden providers.
  const hiddenIds = new Set(
    Array.isArray(data?.settings?.hiddenAccountIds) ? data.settings.hiddenAccountIds : []
  );
  const everyAccount = Array.isArray(data?.accounts) ? data.accounts.filter((a) => a && typeof a === 'object') : [];
  const accounts = ctx.includeHidden
    ? everyAccount
    : everyAccount.filter((account) => !hiddenIds.has(account?.id) && account?.hidden !== true);
  const hidden = ctx.includeHidden ? new Set() : hiddenProviders(data);
  const of = (provider) => accounts.filter((account) => account?.provider === provider);
  const sections = [];
  if (!hidden.has('claude'))
    sections.push(
      claudeSection(of('claude'), ctx.profiles, ctx.platform || 'mac', now, ctx.openProgress, data)
    );
  if (!hidden.has('codex'))
    sections.push(
      codexSection(
        of('codex'),
        now,
        everyAccount.filter((account) => account.provider === 'codex')
      )
    );
  if (!hidden.has('antigravity') && of('antigravity').length)
    sections.push(
      antigravitySection(of('antigravity'), data, ctx.antigravityInventory, now)
    );
  return {
    version: VIEW_MODEL_VERSION,
    sections,
    cards: providerCards(accounts, hidden, now),
    registry: providerRegistry(data),
    chrome: chromeView(data, {
      now,
      refreshing: ctx.refreshing,
      intervalSeconds: ctx.intervalSeconds,
      username: ctx.username,
      host: ctx.host,
    }),
  };
}

/** Details for one account: every visible window, the amounts, and where the reading came from. */
export function detailsViewModel(data, id, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const account =
    accounts.find((row) => row.id === id) || accounts.find((row) => row.provider === id);
  if (!account) return null;
  const home = dashboardViewModel(data, { ...ctx, includeHidden: true });
  const row = home.sections.flatMap((section) => section.rows).find((r) => r.id === account.id);
  const section = home.sections.find((s) => s.rows.some((r) => r.id === account.id));
  const meters = visibleMeters(account).map((w) =>
    meterView(account, w, { now, label: fullWindowLabel(account.provider, w) })
  );
  const provider = PROVIDER_REGISTRY.find((p) => p.id === account.provider);
  const state = row?.active ? `Active ${row.activeLabel}` : statusWord(account);
  const profile =
    account.capabilities?.claudeProfileId ||
    account.capabilities?.codexProfile ||
    account.capabilities?.antigravityProfileId ||
    '';
  const facts = [
    ...antigravityPlanFacts(account),
    { label: 'Status', value: statusWord(account), mono: false },
    {
      label: 'Sampled',
      value: validDate(account.sampledAt)
        ? `${dateTime.format(new Date(account.sampledAt))} · ${relative(account.sampledAt, now)}`
        : 'Never',
      mono: false,
    },
    {
      label: 'Fetched',
      value: validDate(account.fetchedAt) ? dateTime.format(new Date(account.fetchedAt)) : 'Never',
      mono: false,
    },
    { label: 'Source', value: text(account.source) || 'Unknown', mono: false },
    { label: 'Platform', value: hostLabel(account), mono: false },
    ...(profile ? [{ label: 'Profile', value: profile, mono: true }] : []),
    ...(text(account.message)
      ? [{ label: 'Note', value: text(account.message), mono: false }]
      : []),
    ...(account.provider === 'claude' ? claudeSignInFacts(data, account.email, now) : []),
  ];
  const missingFable = isMaxPlan(account) && !visibleMeters(account).some(isFable);
  const opening =
    account.provider === 'claude'
      ? openLine(ctx.openProgress, account.capabilities?.claudeProfileId)
      : '';
  return {
    id: account.id,
    provider: account.provider,
    title: text(account.email) || text(account.label) || 'Account identity unavailable',
    sub: [
      provider?.label || text(account.providerLabel),
      planLabel(text(account.plan), account.provider),
      state,
    ]
      .filter(Boolean)
      .join(' · '),
    subLead: [
      provider?.label || text(account.providerLabel),
      planLabel(text(account.plan), account.provider),
    ]
      .filter(Boolean)
      .join(' · '),
    state,
    active: !!row?.active,
    activeLabel: row?.activeLabel || '',
    platform: hostLabel(account),
    profile: row?.profile || profile,
    canMac: !!row?.canMac,
    canWindows: !!row?.canWindows,
    note: [
      antigravityPlanNote(account),
      cursorPlanNote(account),
      opening,
      account.provider === 'claude' ? claudeSignInNote(data, account.email) : '',
      missingFable
        ? 'Fable usage is not reported yet. It appears here as its own weekly window once the dashboard sends one.'
        : '',
    ]
      .filter(Boolean)
      .join(' · '),
    meters,
    amounts: visibleAmounts(account).map((w) => amountView(account, w, now)),
    facts,
    sectionId: section?.id || '',
    // The fact groups (caps head, one sentence, then a table or label/value pairs) the frame's Details draws.
    resetsMode: resetsMode(account.provider),
    resetRows: resetRowsOf(account, visibleAmounts(account)),
    balances: visibleAmounts(account)
      .filter((w) => !isResetWindow(account, w))
      .map((w) => amountView(account, w, now)),
    reading: readingGroup(account, profile, now),
    signIns: account.provider === 'claude' ? claudeSignInGroup(data, account.email, now) : null,
  };
}

/** One sentence per reading status, when the reading carries no message of its own. */
const READING_WORDS = {
  Live: 'Read from the provider at the last refresh.',
  Cached: 'This is the last successful reading.',
  'Sign-in needed': 'Sign in again so AAC can read this account.',
  'Refresh failed': 'The last refresh failed.',
  Unavailable: 'AAC has no reading for this account.',
};

/**
 * The Details "Reading" fact group: when it was sampled in the head, the status as the answer, then Read from and
 * Profile. Fetched shows only when it differs from Sampled, and the computer is part of Read from.
 */
export function readingGroup(account, profile = '', now = Date.now()) {
  const status = statusWord(account);
  const sampled = validDate(account?.sampledAt) ? Date.parse(account.sampledAt) : null;
  const fetched = validDate(account?.fetchedAt) ? Date.parse(account.fetchedAt) : null;
  const source = text(account?.source);
  const host = hostLabel(account);
  const rows = [
    ...antigravityPlanFacts(account).map(({ label, value }) => ({ label, value, mono: false })),
    { label: 'Read from', value: source ? `${source}, on ${host}` : `On ${host}`, mono: false },
    ...(profile ? [{ label: 'Profile', value: profile, mono: true }] : []),
    ...(fetched !== null && (sampled === null || Math.abs(fetched - sampled) > 90_000)
      ? [{ label: 'Fetched', value: dateTime.format(new Date(fetched)), mono: false }]
      : []),
  ];
  return {
    head: sampled === null ? 'Never sampled' : `${dateTime.format(new Date(sampled))}, ${relative(sampled, now)}`,
    lead: `${status}.`,
    rest: text(account?.message) || READING_WORDS[status] || '',
    rows,
  };
}

/** "Ubuntu", "Ubuntu and Mac", "Ubuntu, Mac, Windows and Nas1". */
const listWords = (words) =>
  words.length < 2 ? words.join('') : `${words.slice(0, -1).join(', ')} and ${words[words.length - 1]}`;

/** The computers Update all runs on, in the order the header names them. */
export const UPDATE_HOST_IDS = HOST_IDS;
/** The apps the update helper checks on every computer (one result row each). */
const UPDATE_APPS_PER_HOST = 12;
/**
 * The result rows a run expects: the job's own `expectedResults`, else one row per app on every
 * computer (twelve apps per registered computer), never a stale fixed count.
 */
export const updateTotal = (job) =>
  finite(job?.expectedResults) && job.expectedResults > 0
    ? job.expectedResults
    : UPDATE_HOST_IDS.length * UPDATE_APPS_PER_HOST;

/**
 * The computers an update job is working on now. Every computer runs at once
 * (job.hosts); a job saved before that names a single activePlatform.
 */
export function updateRunningHosts(job) {
  if (job?.state !== 'running') return [];
  const hosts = job.hosts && typeof job.hosts === 'object' ? job.hosts : null;
  if (hosts) return UPDATE_HOST_IDS.filter((id) => hosts[id]?.state === 'running');
  return job.activePlatform ? [job.activePlatform] : [];
}

/**
 * A Muse row whose model catalog did not refresh: the helper reports
 * restart_failed (binary moved, sessions restarted) or failed (already
 * current, warm failed), never a clean updated/current. The DTO carries words,
 * never a message code, so the words are the signal, the same way unknown and
 * action-required rows are read in accounts-view.mjs.
 */
export function isCatalogPending(row) {
  return (
    (row?.status === 'restart_failed' || row?.status === 'failed') &&
    typeof row?.message === 'string' &&
    row.message.includes('not registered yet')
  );
}

/** Update apps button state from /api/app-updates/status. */
export function updateViewModel(job, { done = false } = {}) {
  const results = Array.isArray(job?.results) ? job.results : [];
  const failed = results.filter(
    (row) => row.status === 'failed' || row.status === 'restart_failed'
  ).length;
  const action = results.filter((row) => row.status === 'action_required').length;
  const held = results.filter((row) => row.status === 'held').length;
  const catalog = results.filter(isCatalogPending).length;
  const catalogWords = catalog ? `${catalog} model${catalog === 1 ? '' : 's'} not yet registered` : '';
  const running = job?.state === 'running';
  const where = listWords(updateRunningHosts(job).map((id) => platformLabel(id)));
  const tip = running
    ? `Updating apps on ${where || 'your computers'} · running apps may restart`
    : job
      ? `Last run ${job.state}: ${results.length} results${failed ? `, ${failed} failed` : ''}${action ? `, ${action} need action` : ''}${held ? `, ${held} held` : ''}${catalogWords ? `, ${catalogWords}` : ''}. Details under Accounts & Settings.`
      : `Update the Claude and Codex apps and CLIs on ${listWords(HOST_IDS.map(id => HOST_LABELS[id]))}`;
  return {
    running,
    done: !running && done,
    count: results.length,
    total: updateTotal(job),
    tip,
    summary: job
      ? `${results.length} results${failed ? ` · ${failed} failed` : ''}${action ? ` · ${action} need action` : ''}${held ? ` · ${held} held` : ''}${catalogWords ? ` · ${catalogWords}` : ''}`
      : '',
  };
}
