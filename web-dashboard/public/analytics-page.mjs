// The Analytics page's state rules, shared by bridge.js (the classic app) and frame-analytics.mjs (the app frame, which
// fetches and builds the page itself): the page state, which ranges a response covers, how often a page still being
// read polls, the switch thresholds the quota charts mark, and what each page action changes. Pure; the view itself is
// built by analytics-usage.mjs, analytics-quota.mjs and analytics-data.mjs (tests/analytics-page.test.mjs).
import { RANGES, parseIsoDay, addDays, H, D } from './analytics-usage.mjs';
import { QUOTA_PROVIDERS } from './analytics-quota.mjs';
import { zonedMonthStart } from './time-format.mjs';

/** The page state (version 3): every number and label is computed from the response with it. */
export function createAnalyticsPage() {
  return {
    range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', cbmSort: 'cost',
    // donutOpen: the donut legend groups that are open (_other: the models under 1%; _undrawn: by cost, no arc)
    open: new Set(), compare: new Set(), collapsed: new Set(), donutOpen: new Set(),
    // chart boxes reported by the page's layout (analytics-layout): the charts are laid out in these pixels
    sizes: { trend: null, daily: null, heat: null, focus: null },
  };
}

/** The switch thresholds the quota charts mark: Codex auto-switch, and Antigravity's when it has accounts to switch. */
export function analyticsThresholds(dashboard, antigravityAuto) {
  const auto = dashboard?.codexAutoSwitch;
  const codex = auto?.enabled === true && Number.isFinite(auto?.thresholdPercent) ? 100 - auto.thresholdPercent : null;
  const agAccounts = (dashboard?.accounts || []).filter(account => account.provider === 'antigravity').length;
  const antigravity = antigravityAuto?.enabled === true && agAccounts > 1 && Number.isFinite(antigravityAuto?.thresholdUsedPercent) ? antigravityAuto.thresholdUsedPercent : null;
  return { codex, antigravity };
}

/** True when the response in hand already holds the hours of the page range (no fetch is needed to draw it). */
export function analyticsCovers(payload, page, now = Date.now()) {
  const from = Date.parse(payload?.range?.from);
  if (!Number.isFinite(from)) return false;
  const { range } = page;
  if (range === 'all') return payload.range?.preset === '30d';
  const start = range === '24h' ? now - D : range === '7d' ? now - 7 * D : range === '30d' ? now - 30 * D
    : range === 'month' ? zonedMonthStart(now)
    : Number.isFinite(page.from) ? page.from : now - 30 * D;
  return from <= start + H;
}

/**
 * How long until the page reads again while the server is still reading (0: it is settled). Once totals exist, every
 * minute; while cells only wait for the next scheduled scan, every 15 s. While a read runs with no totals yet (a server
 * that just started answers "loading" first) every 10 s - a live analytics projection costs real server CPU, so the
 * poll stays calm - except at the start: `waitedMs` (how long the page has waited for its first numbers; bridge.js
 * passes none) reads again after 2 s for the first 10 s and 5 s until 30 s, since the first totals usually land within
 * seconds of that first answer. An answer the server kept from before the read (`stale`, while it computes the next)
 * is read again on the same steps.
 */
export function analyticsPollDelay(payload, waitedMs = Infinity) {
  // an answer the server kept from before this read (`stale`): it is computing the next one, which lands in seconds
  // (one sent with refreshError has nothing newer on its way: the usual pace below)
  if (payload?.stale === true && !payload.refreshError) return waitedMs < 10_000 ? 2000 : waitedMs < 30_000 ? 5000 : 10000;
  const act = payload?.activity;
  const busyNow = act?.status === 'loading' || act?.refreshing === true;
  const converging = Array.isArray(act?.sources) && act.sources.some(r => r?.state === 'scanning');
  if (!busyNow && !converging) return 0;
  if (act?.totals) return 60000;
  if (!busyNow) return 15000;
  return waitedMs < 10_000 ? 2000 : waitedMs < 30_000 ? 5000 : 10000;
}

const toggleIn = (set, value) => { if (set.has(value)) set.delete(value); else set.add(value); };

/**
 * One of the page's own actions applied to `page`. Returns null for a kind that isn't one of them, `{}` for one refused
 * or changing nothing, `{ range: true }` when the range changed (draw what the response covers, read when it doesn't),
 * or `{ render: 'morph' | 'static' }`. `known(id)` says whether an account id is on the page.
 */
export function applyAnalyticsAction(page, action, value, { known = () => false, now = Date.now() } = {}) {
  switch (action) {
    case 'analytics-range':
      if (!RANGES.some(([id]) => id === value) || page.range === value) return {};
      page.range = value; page.from = null; page.to = null;
      return { range: true };
    case 'analytics-custom': {
      const [first, last] = String(value).split(',').map(parseIsoDay);
      if (!Number.isFinite(first) || !Number.isFinite(last) || last < first || first < now - 31 * D || first > now) return {};
      page.range = 'custom'; page.from = first; page.to = addDays(last, 1);
      return { range: true };
    }
    // the picker offers All and the providers that served usage (dashboard ids, or "other")
    case 'analytics-provider':
      if (!(value === 'all' || /^[a-z][a-z0-9-]{0,39}$/.test(value)) || page.prov === value) return {};
      page.prov = value;
      return { render: 'morph' };
    case 'analytics-split': page.split = value === 'true'; return { render: 'morph' };
    case 'analytics-cache': page.cache = value === 'true'; return { render: 'morph' };
    case 'analytics-donut':
      if (!['tokens', 'cost'].includes(value)) return {};
      page.donut = value; return { render: 'static' };
    case 'analytics-cbm-sort':
      if (!['cost', 'tokens'].includes(value)) return {};
      page.cbmSort = value; return { render: 'static' };
    case 'analytics-donut-open':
      if (!['_other', '_undrawn'].includes(value)) return {};
      toggleIn(page.donutOpen, value); return { render: 'static' };
    case 'analytics-heat':
      if (!['cost', 'tokens'].includes(value)) return {};
      page.heat = value; return { render: 'static' };
    case 'analytics-focus':
    case 'analytics-compare':
      if (!known(value)) return {};
      toggleIn(action === 'analytics-focus' ? page.open : page.compare, value); return { render: 'static' };
    case 'analytics-group':
      if (!QUOTA_PROVIDERS.some(([id]) => id === value)) return {};
      toggleIn(page.collapsed, value); return { render: 'static' };
    default:
      return null;
  }
}

/** A chart box's size ("kind,w,h" or "heat-cell,w"): true when it changed (the charts are laid out again). */
export function applyAnalyticsLayout(page, value) {
  const [kind, w, h] = String(value).split(',');
  const width = Number(w), height = Number(h);
  if (kind === 'heat-cell') {
    const prior = page.sizes.heat;
    if (!Number.isFinite(width) || width <= 0 || Math.abs((prior?.cellW || 0) - width) < 0.1) return false;
    page.sizes.heat = { ...prior, cellW: width };
    return true;
  }
  if (!(kind in page.sizes) || !Number.isFinite(width) || !Number.isFinite(height) || width <= 0) return false;
  const prior = page.sizes[kind];
  if (prior && Math.abs(prior.w - width) < 1 && Math.abs(prior.h - height) < 1) return false;
  page.sizes[kind] = { ...prior, w: width, h: height };
  return true;
}
