// Analytics, Usage blocks (version 3): the header, KPI row, usage trends, cost by model, model donut, session
// stats, token breakdown, cache efficiency, weekday x hour heatmap and daily cost by provider, built like the
// approved Daylight Atlas concept (c-daylight-atlas/app-analytics.js). Pure functions of the analytics response
// and the page state; every chart geometry is computed here and drawn by ui/pages/analytics/*.slint.
//
// Truthfulness: activity is the CLI usage logs the server reads (activity.sources says which tools on which
// computers). Only Claude Code and Codex are providers on this page; OMP, Muse Code, zcode, Antigravity and generic JSONL logs
// merge into the model views and the totals and never become a filter, legend, row or header. The Tokens by tool line, the
// trend readout and a model's detail say how much each tool logged and where a model's usage came from; models
// stay the only division, and every model with usage is listed. Cost is an estimated API equivalent, not a bill; a cost that is neither logged nor priced at a listed rate is "not logged", never zero,
// and a total that leaves such a cost out says "partial". Activity covers all accounts and is never attributed
// to one or added to quota numbers; a missing reading is unavailable, never zero; at most two decimals; local
// time throughout.
import { reconcile, modelRates } from './model-rates.mjs';
import { displayFormat, zonedAddDays, zonedDayStart, zonedHour, zonedHourStart, zonedMonthStart, zonedWeekday } from './time-format.mjs';

export const H = 3_600_000;
export const D = 24 * H;
const finite = value => typeof value === 'number' && Number.isFinite(value);
const text = value => typeof value === 'string' ? value : '';
const clamp = (value, low, high) => Math.max(low, Math.min(high, value));
const pt = value => Math.round(value * 10) / 10;

// ---------------------------------------------------------------- formatting
const nfSig = new Intl.NumberFormat(undefined, { notation: 'compact', maximumSignificantDigits: 3 });
const nfAxis = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
const nf0 = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });
const nf1 = new Intl.NumberFormat(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const nf2v = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const usd = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 2 });
const usd0 = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 0 });
export const tokC = value => finite(value) ? nfSig.format(value) : 'Unavailable';
export const tokX = value => finite(value) ? `${nf0.format(value)} tokens` : 'Unavailable';
export const money = value => finite(value) ? usd.format(value) : 'Unavailable';
const moneyAxis = (value, step) => step < 1 ? usd.format(value) : usd0.format(value);
export const share1 = value => !finite(value) ? 'Unavailable' : value > 0 && value < 0.05 ? '<0.1' : value < 100 && value >= 99.95 ? '>99.9' : nf1.format(value);
export const share2 = value => !finite(value) ? 'Unavailable' : value > 0 && value < 0.005 ? '<0.01' : value < 100 && value > 99.995 ? '>99.99' : nf2v.format(value);
const intText = value => finite(value) ? nf0.format(Math.round(value)) : 'Unavailable';
const dtf = options => displayFormat(options);
const F_HOUR = dtf({ hour: 'numeric' });
const F_CLOCK = dtf({ hour: 'numeric', minute: '2-digit' });
const F_MD = dtf({ month: 'short', day: 'numeric' });
const F_WMD = dtf({ weekday: 'short', month: 'short', day: 'numeric' });
const F_WD = dtf({ weekday: 'short' });
const F_TIME = dtf({ month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
export const hourTxt = t => F_HOUR.format(new Date(t));
export const clockTxt = t => F_CLOCK.format(new Date(t));
export const mdTxt = t => F_MD.format(new Date(t));
export const wmdTxt = t => F_WMD.format(new Date(t));
export const wdTxt = t => F_WD.format(new Date(t));
export const timeTxt = t => F_TIME.format(new Date(t));
export function zoneName(now = Date.now()) {
  try { return dtf({ timeZoneName: 'short' }).formatToParts(new Date(now)).find(part => part.type === 'timeZoneName')?.value || ''; } catch { return ''; }
}
export function duration(ms) {
  ms = Math.max(0, ms);
  const m = Math.floor(ms / 60000), h = Math.floor(m / 60), d = Math.floor(h / 24);
  if (d >= 1) return `${d}d ${h % 24}h`;
  if (h >= 1) return `${h}h ${m % 60}m`;
  if (m >= 1) return `${m}m`;
  return `${Math.max(1, Math.floor(ms / 1000))}s`;
}
export const relTxt = (t, now) => !finite(t) ? 'time unavailable' : now - t < 10_000 ? 'just now' : `${duration(now - t)} ago`;
export const untilTxt = (t, now) => !finite(t) ? '' : t - now <= 0 ? 'reset due' : `in ${duration(t - now)}`;
/** A line that mixes weights travels as runs ({ text, strong, tone }), like the Home view model. */
const run = (value, strong = false, tone = '') => ({ text: String(value), strong: !!strong, tone });

// ---------------------------------------------------------------- display-zone calendar
export const dayStart = t => zonedDayStart(t);
export const addDays = (t, n) => zonedAddDays(t, n);
export function bucketStart(t, step) {
  if (step >= D) return zonedDayStart(t);
  const k = Math.round(step / H);
  const start = zonedDayStart(t);
  return start + Math.floor((t - start) / (k * H)) * (k * H);
}
export function nextBucket(t, step) {
  if (step >= D) return zonedAddDays(t, 1);
  return zonedHourStart(t) + Math.round(step / H) * H;
}
const sameDay = (a, b) => dayStart(a) === dayStart(b);
/** YYYY-MM-DD of a local day (the custom-range action carries whole local days). */
export const isoDay = t => { const d = new Date(t); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`; };
export function parseIsoDay(value) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(value || ''));
  if (!m) return NaN;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return d.getFullYear() === Number(m[1]) && d.getMonth() === Number(m[2]) - 1 && d.getDate() === Number(m[3]) ? d.getTime() : NaN;
}

// ---------------------------------------------------------------- scales and curves
export function niceStep(raw) {
  if (!(raw > 0)) return 1;
  const p = Math.pow(10, Math.floor(Math.log10(raw))), n = raw / p;
  return ([1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10].find(k => n <= k + 1e-9) || 10) * p;
}
/** k divisions with a round step; the trend chart's two axes share k so their gridlines line up. */
export const niceScale = (max, k) => { const step = niceStep((max > 0 ? max : 1) / k); return { step, top: step * k, k }; };
/** Monotone cubic (Fritsch-Carlson) sampling; it never overshoots between nodes. */
export function monoFn(xs, ys) {
  const n = xs.length;
  if (n === 1) return () => ys[0];
  const dx = [], m = [], t = new Array(n);
  for (let i = 0; i < n - 1; i++) { dx[i] = (xs[i + 1] - xs[i]) || 1e-6; m[i] = (ys[i + 1] - ys[i]) / dx[i]; }
  t[0] = m[0]; t[n - 1] = m[n - 2];
  for (let i = 1; i < n - 1; i++) t[i] = m[i - 1] * m[i] <= 0 ? 0 : 3 * (dx[i - 1] + dx[i]) / ((2 * dx[i] + dx[i - 1]) / m[i - 1] + (dx[i] + 2 * dx[i - 1]) / m[i]);
  let j = 0;
  return x => {
    if (x <= xs[0]) return ys[0];
    if (x >= xs[n - 1]) return ys[n - 1];
    if (x < xs[j]) j = 0;
    while (j < n - 2 && x > xs[j + 1]) j++;
    const h = dx[j], s = (x - xs[j]) / h, s2 = s * s, s3 = s2 * s;
    const y = (2 * s3 - 3 * s2 + 1) * ys[j] + (s3 - 2 * s2 + s) * h * t[j] + (-2 * s3 + 3 * s2) * ys[j + 1] + (s3 - s2) * h * t[j + 1];
    return clamp(y, Math.min(ys[j], ys[j + 1]), Math.max(ys[j], ys[j + 1]));
  };
}
export function lineD(x0, x1, ys) { const n = ys.length; let d = ''; for (let i = 0; i < n; i++) d += `${i ? 'L' : 'M'}${pt(x0 + (x1 - x0) * i / Math.max(1, n - 1))} ${pt(ys[i])}`; return d; }
export function bandD(x0, x1, top, bot) {
  const n = top.length; let d = '';
  for (let i = 0; i < n; i++) d += `${i ? 'L' : 'M'}${pt(x0 + (x1 - x0) * i / Math.max(1, n - 1))} ${pt(top[i])}`;
  for (let i = n - 1; i >= 0; i--) d += `L${pt(x0 + (x1 - x0) * i / Math.max(1, n - 1))} ${pt(bot[i])}`;
  return `${d}Z`;
}
/** A dashed line as separate subpaths (Slint paths have no dash array). */
export function dashLine(x0, y0, x1, y1, dash, gap) {
  const len = Math.hypot(x1 - x0, y1 - y0);
  if (!(len > 0)) return '';
  const ux = (x1 - x0) / len, uy = (y1 - y0) / len;
  let d = '';
  for (let s = 0; s < len - 0.01; s += dash + gap) {
    const e = Math.min(len, s + dash);
    d += `M${pt(x0 + ux * s)} ${pt(y0 + uy * s)}L${pt(x0 + ux * e)} ${pt(y0 + uy * e)}`;
  }
  return d;
}
/** A dashed polyline (pattern of dash/gap lengths) along a list of [x, y] points. */
export function dashPolyline(points, pattern) {
  if (!pattern || !pattern.length) return points.map(([x, y], i) => `${i ? 'L' : 'M'}${pt(x)} ${pt(y)}`).join('');
  let d = '', k = 0, left = pattern[0], on = true, pen = false;
  for (let i = 0; i + 1 < points.length; i++) {
    let [x0, y0] = points[i];
    const [x1, y1] = points[i + 1];
    let len = Math.hypot(x1 - x0, y1 - y0);
    const ux = len ? (x1 - x0) / len : 0, uy = len ? (y1 - y0) / len : 0;
    while (len > 1e-6) {
      const stepLen = Math.min(left, len);
      const nx = x0 + ux * stepLen, ny = y0 + uy * stepLen;
      if (on) { if (!pen) { d += `M${pt(x0)} ${pt(y0)}`; pen = true; } d += `L${pt(nx)} ${pt(ny)}`; }
      x0 = nx; y0 = ny; len -= stepLen; left -= stepLen;
      if (left <= 1e-6) { k = (k + 1) % pattern.length; left = pattern[k]; on = !on; pen = false; }
    }
  }
  return d;
}

// ---------------------------------------------------------------- data
export const TYPES = [
  { k: 'in', f: 'inputTokens', label: 'Input', long: 'Input, uncached' },
  { k: 'out', f: 'outputTokens', label: 'Output', long: 'Output' },
  { k: 'cw', f: 'cacheCreationTokens', label: 'Cache write', long: 'Cache write' },
  { k: 'cr', f: 'cacheReadTokens', label: 'Cache read', long: 'Cache read' },
];
/**
 * The dashboard providers, in the dashboard's order, with its labels (the response's provider table overrides a
 * label). Every activity row (activity.providers[] / byHour[] / models[] `provider`) is the provider that served
 * the usage: the server groups each log under its route (Claude Code, Codex, the Muse Code CLI and Antigravity are
 * their own provider; OMP and zcode record a route per call). "other" is a route no provider claims, never a guess.
 */
export const PROVIDER_ORDER = ['claude', 'codex', 'antigravity', 'muse', 'cursor', 'kimi-code', 'qwen', 'zai', 'opencode-go', 'other'];
export const PROVIDER_LABEL = { claude: 'Claude', codex: 'Codex', antigravity: 'Antigravity', muse: 'Muse Code', cursor: 'Cursor', 'kimi-code': 'Kimi Code', qwen: 'Qwen token plan', zai: 'Z.ai coding plan', 'opencode-go': 'OpenCode Go', other: 'Other' };
const validProvider = p => typeof p === 'string' && /^[a-z][a-z0-9-]{0,39}$/.test(p);
const providerRank = p => { const i = PROVIDER_ORDER.indexOf(p); return i < 0 ? PROVIDER_ORDER.length - 1 : i; };
/** Dashboard order; a provider the table does not list sorts before "other", by id. */
export const byProviderOrder = (a, b) => providerRank(a) - providerRank(b) || (a === 'other') - (b === 'other') || a.localeCompare(b);
/** Claude and Codex get their own series in the charts; every other provider shares the neutral third one. */
const ownSeries = p => p === 'claude' || p === 'codex';
/** A provider mark exists for every dashboard provider; "other" has none. */
const markOf = p => p === 'other' ? '' : p;
/** The tools whose logs the server reads. Their names say where a model's usage came from, never a division. */
export const TOOLS = ['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl', 'antigravity'];
export const TOOL_LABEL = { claude: 'Claude Code', codex: 'Codex', omp: 'OMP', muse: 'Muse Code', zcode: 'zcode', jsonl: 'Generic JSONL', antigravity: 'Antigravity' };
const toolNames = tools => andList(TOOLS.filter(t => tools.has(t)).map(t => TOOL_LABEL[t]));
/** Add v to o[k] (per-provider sums over providers that are only known from the data). */
const bump = (o, k, v) => { o[k] = (o[k] || 0) + v; };
export const RANGES = [['24h', '24H'], ['7d', '7D'], ['30d', '30D'], ['month', 'Month'], ['all', 'All']];
const tokens = row => TYPES.every(t => finite(row?.[t.f]) && row[t.f] >= 0);
const TINY = 1e-9;
export const NOT_LOGGED = 'Not logged';
export const FREE = 'Free';
/** A listed all-zero rate is a known price: the model is free, never unknown. */
export const isFreeRate = r => !!r && r.source !== 'fallback' && ['in', 'out', 'cw', 'cr'].every(k => r[k] === 0);
/**
 * The part of a row's estimate that is not logged: tokens with no logged cost and no listed rate, which the
 * server prices only at its unknown-model fallback (fallbackCostUsd). A response from before that field counts
 * a row with an unknown split and no listed rate as wholly not logged, so a guess is never shown as a cost.
 * This holds for every provider: a model with no known rate reads as not logged, whoever served it.
 */
export function notLoggedPart(row, est) {
  if (est === null) return 0;
  if (finite(row?.fallbackCostUsd) && row.fallbackCostUsd >= 0) return Math.min(est, row.fallbackCostUsd);
  if (row && 'costByType' in row && row.costByType === null && (!row.rates || row.rates.source === 'fallback')) return est;
  return 0;
}
const RATE_FIELDS = ['inputPerMillion', 'outputPerMillion', 'cacheCreationPerMillion', 'cacheReadPerMillion'];
/** The server's published rates for a model, when they are a listed rate (the unknown-model fallback is not). */
function listedRate(r) {
  if (!r || typeof r !== 'object' || r.source === 'fallback' || !RATE_FIELDS.every(k => finite(r[k]) && r[k] >= 0)) return null;
  return { in: r.inputPerMillion, out: r.outputPerMillion, cw: r.cacheCreationPerMillion, cr: r.cacheReadPerMillion, source: text(r.source) || 'builtin' };
}
/**
 * A model's per-type cost: the server's own split when it reconciles, else the tokens at its listed rate when they
 * add up to the logged estimate, else token shares of what is logged. A response without published rates uses
 * the page's mirror of the CCS rates (model-rates.mjs), as before; the mirror's fallback is no listed rate.
 */
function modelSplit(row, tok, total, known) {
  const shares = () => Object.fromEntries(TYPES.map(t => [t.k, total > 0 && finite(known) ? tok[t.k] / total * known : 0]));
  if (!('rates' in row)) {
    const r = reconcile(row);
    const rate = r.rate.source === 'fallback' ? null : r.rate;
    return rate && r.reconciled ? { cost: r.cost, mode: 'rates', rate } : { cost: shares(), mode: 'shares', rate };
  }
  const rate = listedRate(row.rates);
  const p = row.costByType;
  if (rate && row.costByTypeReconciled === true && p && ['input', 'output', 'cacheWrite', 'cacheRead'].every(k => finite(p[k]) && p[k] >= 0))
    return { cost: { in: p.input, out: p.output, cw: p.cacheWrite, cr: p.cacheRead }, mode: 'rates', rate };
  if (rate && finite(known)) {
    const cost = Object.fromEntries(TYPES.map(t => [t.k, tok[t.k] * rate[t.k] / 1e6]));
    if (Math.abs(cost.in + cost.out + cost.cw + cost.cr - known) <= Math.max(0.005, known * 1e-6)) return { cost, mode: 'rates', rate };
  }
  return { cost: shares(), mode: 'shares', rate };
}

/** The analytics API range that covers a page range (the backend accepts 24h, 7d and 30d today). */
export function apiRangeFor(state, now = Date.now()) {
  const range = state?.range;
  if (range === '24h' || range === '7d' || range === '30d') return range;
  if (range === 'month') return new Date(now).getDate() <= 1 ? '24h' : dayStartMonth(now) >= now - 7 * D + H ? '7d' : '30d';
  if (range === 'custom' && finite(state.from)) return state.from >= now - D ? '24h' : state.from >= now - 7 * D ? '7d' : '30d';
  return '30d';
}
const dayStartMonth = now => zonedDayStart(zonedMonthStart(now));

/**
 * Validated activity rows. Malformed and duplicate rows are dropped, never counted twice. So is `<synthetic>`,
 * Claude Code's placeholder for messages that never reached a model: it is excluded silently, never listed and
 * never noted. Every row carries the provider that served it. Each row's cost is the part that is logged or
 * priced at a listed rate (`cost`), and `unk` says some of it is not logged. `label(p)` is a provider's name
 * (the response's provider table, else the dashboard's), and `tools(p)` the tools whose logs hold its usage.
 */
function buildActivityRows(payload) {
  const act = payload?.activity || {};
  const available = ['ok', 'cached'].includes(act.status) && tokens(act.totals);
  // a known provider: the dashboard's, one the response's provider table lists, or one its activity reports
  const known = new Set(PROVIDER_ORDER);
  for (const row of [...(Array.isArray(payload?.providers) ? payload.providers : []), ...(Array.isArray(act.providers) ? act.providers : [])])
    if (validProvider(row?.provider)) known.add(row.provider);
  const isKnown = p => validProvider(p) && known.has(p);
  const seen = new Set();
  const hours = [];
  let costMissing = false;
  if (available) {
    for (const row of Array.isArray(act.byHour) ? act.byHour : []) {
      const value = text(row?.hour);
      if (!/^\d{4}-\d{2}-\d{2}T\d{2}:00(?::00)?Z$/.test(value) || !isKnown(row.provider) || !tokens(row)) continue;
      const t = Date.parse(value);
      if (!finite(t) || new Date(t).toISOString().slice(0, 13) !== value.slice(0, 13)) continue;
      const key = `${row.provider}|${t}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const est = finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0 ? row.estimatedCostUsd : null;
      if (est === null) costMissing = true;
      const nl = notLoggedPart(row, est);
      hours.push({ t, p: row.provider, in: row.inputTokens, out: row.outputTokens, cw: row.cacheCreationTokens, cr: row.cacheReadTokens, cost: est === null ? 0 : Math.max(0, est - nl), unk: nl > TINY });
    }
  }
  hours.sort((a, b) => a.t - b.t);
  const models = [];
  const modelKeys = new Set();
  const toolList = v => Array.isArray(v) ? TOOLS.filter(t => v.includes(t)) : [];
  if (available) {
    for (const row of Array.isArray(act.models) ? act.models : []) {
      if (!isKnown(row?.provider) || !text(row.model) || !tokens(row) || text(row.model) === '<synthetic>') continue;
      const key = `${row.provider}|${row.model}`;
      if (modelKeys.has(key)) continue;
      modelKeys.add(key);
      const tok = Object.fromEntries(TYPES.map(t => [t.k, row[t.f]]));
      const total = tok.in + tok.out + tok.cw + tok.cr;
      const est = finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0 ? row.estimatedCostUsd : null;
      // a response without fallbackCostUsd or rates: the mirror's unknown-model fallback is no listed rate either
      const legacy = !('fallbackCostUsd' in row) && !('rates' in row) && !('costByType' in row);
      const nl = legacy ? (est !== null && modelRates(row.model).source === 'fallback' ? est : 0) : notLoggedPart(row, est);
      const logged = est === null ? null : Math.max(0, est - nl);
      const split = modelSplit(row, tok, total, logged);
      models.push({ model: row.model, provider: row.provider, tools: toolList(row.tools), tok, total, logged, hasCost: est !== null, unk: nl > TINY, cost: split.cost, mode: split.mode, rate: split.rate });
    }
  }
  // a listed rate that does not add up to the logged estimate (a model without a listed rate is not "unreconciled")
  const unreconciled = models.filter(m => m.mode === 'shares' && m.rate && (m.total > 0 || (m.logged || 0) > 0));
  const providerRows = (available && Array.isArray(act.providers) ? act.providers : []).filter(p => validProvider(p?.provider));
  const providers = [...new Set([...hours.map(r => r.p), ...models.map(m => m.provider), ...providerRows.map(p => p.provider)])].sort(byProviderOrder);
  // a provider's blended per-type rate over the logged window, used to split a shorter range's logged cost
  const blend = {};
  for (const p of providers) {
    blend[p] = {};
    const list = models.filter(m => m.provider === p);
    for (const t of TYPES) {
      const tk = list.reduce((s, m) => s + m.tok[t.k], 0), c = list.reduce((s, m) => s + m.cost[t.k], 0);
      blend[p][t.k] = tk > 0 ? c / tk : 0;
    }
  }
  const apiFrom = Date.parse(payload?.range?.from);
  const fetched = Date.parse(act.fetchedAt);
  // labels: the response's provider table (the dashboard's), then the activity rows, then the built-in table
  const names = { ...PROVIDER_LABEL };
  for (const row of providerRows) if (text(row.label) && !/ logs$/.test(row.label)) names[row.provider] = row.label;
  for (const row of Array.isArray(payload?.providers) ? payload.providers : []) if (validProvider(row?.provider) && text(row.label)) names[row.provider] = row.label;
  const label = p => names[p] || p;
  const toolsOf = {};
  for (const row of providerRows) toolsOf[row.provider] = toolList(row.tools);
  const sessionSeen = new Set();
  const sessions = providerRows.filter(p => !sessionSeen.has(p.provider) && sessionSeen.add(p.provider)).map(p => {
    const est = finite(p.totals?.estimatedCostUsd) && p.totals.estimatedCostUsd >= 0 ? p.totals.estimatedCostUsd : null;
    const nl = notLoggedPart(p.totals, est);
    return {
      p: p.provider, label: label(p.provider),
      sessions: Number.isInteger(p.sessionCount) && p.sessionCount >= 0 ? p.sessionCount : null,
      events: Number.isInteger(p.usageEvents) && p.usageEvents >= 0 ? p.usageEvents : null,
      cost: est === null ? null : Math.max(0, est - nl), unk: nl > TINY,
    };
  });
  // a session several providers served counts under each of them, and once in this total
  const sessionTotal = Number.isInteger(act.sessions?.total) && act.sessions.total >= 0 ? act.sessions.total : null;
  const sessionSample = Array.isArray(act.sessions?.sample) ? act.sessions.sample.filter(s => s && typeof s === 'object') : [];
  const sessionsTruncated = act.sessions?.truncated === true;
  // usage from a provider other than Claude and Codex (it shares the charts' neutral third series)
  const others = hours.some(r => !ownSeries(r.p)) || models.some(m => !ownSeries(m.provider));
  const refreshingRemote = Array.isArray(act.refreshingRemote) ? act.refreshingRemote.filter(h => h === 'mac' || h === 'windows') : [];
  return { available, status: text(act.status), refreshing: act.refreshing === true, refreshingRemote, message: text(act.message), hours, models, unreconciled, blend, sessions, sessionTotal, sessionSample, sessionsTruncated, costMissing, others, providers, label, tools: p => toolsOf[p] || [], apiPreset: text(payload?.range?.preset), apiFrom, fetched };
}

// Only the two window ends below read the clock, so the validated rows are kept per response: the
// 15-second header tick and every re-render of the same payload reuse them instead of revalidating
// every hour, model and session row again.
const activityRowsCache = new WeakMap();
function activityRows(payload) {
  const keyable = payload !== null && typeof payload === 'object';
  const hit = keyable ? activityRowsCache.get(payload) : null;
  if (hit) return hit;
  const built = buildActivityRows(payload);
  if (keyable) activityRowsCache.set(payload, built);
  return built;
}
/** The validated rows plus the log window they cover, whose ends follow `now`. */
export function activityData(payload, now = Date.now()) {
  const rows = activityRows(payload);
  const { apiFrom, fetched, hours } = rows;
  const win0 = Math.min(finite(apiFrom) ? Math.ceil(apiFrom / H) * H : Infinity, hours.length ? hours[0].t : Infinity, finite(apiFrom) ? Infinity : now - 7 * D);
  const win1 = finite(fetched) ? Math.min(fetched, now) : hours.length ? Math.min(now, hours.at(-1).t + H) : now;
  return { ...rows, win0, win1 };
}

/** The page range in local time: [a2, b) clipped to the logs that were read; step is the bucket size. */
export function pageRange(state, A, now) {
  const range = state?.range || '7d';
  let a, b = now;
  if (range === '24h') a = now - D;
  else if (range === '7d') a = now - 7 * D;
  else if (range === '30d') a = now - 30 * D;
  else if (range === 'month') a = dayStartMonth(now);
  else if (range === 'custom' && finite(state.from) && finite(state.to)) { a = state.from; b = Math.min(now, state.to); }
  else a = A.win0;
  const a2 = Math.max(a, A.win0);
  const span = b - a2;
  let step;
  if (range === '24h') step = H;
  else if (range === '7d') step = 4 * H;
  else if (range === '30d' || range === 'all') step = D;
  else step = span <= 2 * D + H ? H : span <= 10 * D ? 4 * H : D;
  const end = Math.min(b, A.win1);
  return { range, a, b, a2, end, step, now, clipped: a < A.win0 - H, unread: A.win1 < b ? b - A.win1 : 0 };
}
export const stepWord = s => s >= D ? 'Daily' : s === H ? 'Hourly' : `${Math.round(s / H)}-hour`;
/** The provider filter: a picked provider narrows every block to the usage it served. */
const provOK = (state, p) => !state?.prov || state.prov === 'all' || state.prov === p;
const allPicked = state => !state?.prov || state.prov === 'all';
/** What a block covers, for its subtitle: the picked provider, or every provider under All. */
const provName = (state, A) => allPicked(state) ? 'all providers' : A?.label ? A.label(state.prov) : PROVIDER_LABEL[state.prov] || state.prov;
/** The same for a sentence ("No ... activity was logged"); all providers need no name. */
const provWords = (state, A) => allPicked(state) ? '' : `${provName(state, A)} `;
const rowsIn = (A, R, state) => A.hours.filter(r => provOK(state, r.p) && r.t + H > R.a2 && r.t < R.b);
export function sumRows(rows) {
  const o = { in: 0, out: 0, cw: 0, cr: 0, cost: 0, pc: {}, ptok: {}, partial: false };
  for (const r of rows) { o.in += r.in; o.out += r.out; o.cw += r.cw; o.cr += r.cr; o.cost += r.cost; bump(o.pc, r.p, r.cost); bump(o.ptok, r.p, r.in + r.out + r.cw + r.cr); if (r.unk) o.partial = true; }
  o.tok = o.in + o.out + o.cw + o.cr;
  return o;
}
/**
 * Per-type costs for a set of hourly rows: each provider's logged cost split by its token mix at its models'
 * blended rates, so the parts always add up to the logged total. For the whole logged window with every model
 * reconciled this equals the sum of each model's tokens at its rates (exact).
 */
export function typeCosts(A, rows) {
  const out = { in: 0, out: 0, cw: 0, cr: 0 };
  let exact = true;
  const present = new Set(rows.map(r => r.p));
  for (const p of present) {
    const pr = rows.filter(r => r.p === p);
    const logged = pr.reduce((s, r) => s + r.cost, 0);
    const w = {}; let ws = 0;
    for (const t of TYPES) { w[t.k] = pr.reduce((s, r) => s + r[t.k], 0) * (A.blend[p]?.[t.k] || 0); ws += w[t.k]; }
    const k = ws > 0 ? logged / ws : 0;
    if (Math.abs(k - 1) > 1e-6) exact = false;
    for (const t of TYPES) out[t.k] += w[t.k] * k;
  }
  // exact only when every model of these providers splits at its rates and every cost is logged
  const inexact = A.models.some(m => present.has(m.provider) && (m.mode !== 'rates' || m.unk) && (m.total > 0 || (m.logged || 0) > 0));
  return { cost: out, exact: exact && !inexact && !rows.some(r => r.unk) };
}
export function buckets(A, R, state) {
  const list = [], map = new Map();
  if (R.end <= R.a2) return list;
  for (let t = bucketStart(R.a2, R.step), guard = 0; t < R.end && guard < 800; t = nextBucket(t, R.step), guard++) {
    const t1 = nextBucket(t, R.step);
    const lo = Math.max(t, R.a2), hi = Math.min(t1, R.end);
    const b = { t, t1, lo, hi, mid: (lo + hi) / 2, in: 0, out: 0, cw: 0, cr: 0, cost: 0, pc: {}, ptok: {}, punk: {}, unk: false, others: false, partial: t < R.a2 || t1 > R.end };
    list.push(b); map.set(t, b);
  }
  for (const r of rowsIn(A, R, state)) {
    const b = map.get(bucketStart(r.t, R.step));
    if (!b) continue;
    for (const t of TYPES) b[t.k] += r[t.k];
    b.cost += r.cost; bump(b.pc, r.p, r.cost); bump(b.ptok, r.p, r.in + r.out + r.cw + r.cr);
    // unk: some cost in the bucket is not logged (punk: per provider); others: a provider other than Claude and Codex
    if (r.unk) { b.unk = true; b.punk[r.p] = true; }
    if (!ownSeries(r.p)) b.others = true;
  }
  return list;
}
const shortClock = t => new Date(t).getMinutes() ? clockTxt(t) : hourTxt(t);
export function dateLabel(R) {
  const a = R.a2, b = Math.max(R.a2, R.b - 1);
  return sameDay(a, b) ? `${mdTxt(a)}, ${shortClock(a)} to ${shortClock(R.b)}` : `${mdTxt(a)} to ${mdTxt(b)}`;
}

// ---------------------------------------------------------------- sizes (reported by the Slint layout)
const TREND_PAD = { l: 62, r: 70, t: 30, b: 34 };
const DAILY_PAD = { l: 58, r: 14, t: 18, b: 30 };
export const DEFAULT_SIZES = { trend: { w: 1320, h: 380 }, daily: { w: 1320, h: 240 }, heat: { w: 1320 } };

// ---------------------------------------------------------------- KPI row
function kpis(A, R, state, rows, K, C) {
  if (!A.available) {
    return [['tok', 'Total tokens', ''], ['cost', 'Total cost', 'cost'], ['cache', 'Cache tokens', 'cache'], ['in', 'Input cost', 'in'], ['out', 'Output cost', 'out']]
      .map(([key, label, swatch]) => ({ key, label, swatch, num: 0, has: false, fmt: key === 'tok' || key === 'cache' ? 'tok' : 'money', text: 'Unavailable', tip: '', sub: [run('CLI usage logs unavailable')], apport: false }));
  }
  const days = (R.end - R.a2) / D;
  const cacheCost = C.cost.cw + C.cost.cr;
  const costOk = !A.costMissing;
  // partial: some cost in the range is not logged, so the totals cover only what is
  const partial = costOk && K.partial;
  const perDay = !costOk || partial ? [] : days >= 0.75 ? [run(money(K.cost / Math.max(1, days)), true), run(' a day · ')] : days > 0 ? [run(money(K.cost / (days * 24)), true), run(' an hour · ')] : [];
  const apport = !C.exact;
  // a cost that is all not logged reads "Not logged", never $0.00
  const costCard = (num, sub) => partial && !(K.cost > TINY) ? { num: null, text: NOT_LOGGED, sub } : { num, sub };
  const card = (key, label, swatch, num, fmt, tip, sub, na = 'Unavailable') => ({ key, label, swatch, num: finite(num) ? num : 0, has: finite(num), fmt, text: finite(num) ? fmt === 'money' ? money(num) : tokC(num) : na, tip, sub, apport: apport && (key === 'in' || key === 'out') });
  const total = costCard(costOk ? K.cost : null, partial ? [run('Partial', true), run(' · some cost is not logged')] : [...perDay, run('API-equivalent estimate')]);
  const input = costCard(costOk ? C.cost.in : null, partial ? [run('Partial', true), run(` · ${tokC(K.in)} uncached input tokens`)] : [run(`${tokC(K.in)} uncached input tokens`)]);
  const output = costCard(costOk ? C.cost.out : null, partial ? [run('Partial', true), run(` · ${tokC(K.out)} output tokens`)] : [run(`${tokC(K.out)} output tokens, with reasoning`)]);
  return [
    card('tok', 'Total tokens', '', K.tok, 'tok', tokX(K.tok), [run(`${tokC(K.in)} in · ${tokC(K.out)} out · ${tokC(K.cw + K.cr)} cache`)]),
    card('cost', 'Total cost', 'cost', total.num, 'money', partial ? 'An estimate at API prices, not a bill. Partial: it leaves out the cost that is not logged (no logged cost and no listed rate).' : 'An estimate at API prices, not a bill', total.sub, total.text),
    card('cache', 'Cache tokens', 'cache', K.cw + K.cr, 'tok', tokX(K.cw + K.cr), !costOk ? [run('Cache cost unavailable')] : partial ? [run(K.cost > TINY ? money(cacheCost) : NOT_LOGGED, true), run(' cache cost · partial')] : [run(money(cacheCost), true), run(` cache cost · ${share1(K.cost ? cacheCost / K.cost * 100 : 0)}% of cost`)]),
    card('in', 'Input cost', 'in', input.num, 'money', '', input.sub, input.text),
    card('out', 'Output cost', 'out', output.num, 'money', '', output.sub, output.text),
  ];
}
export const APPORT_TIP = "Model detail is logged only for the whole log window. For a shorter range, each log's cost is split by its token mix at its models' rates, so the parts still add up to the logged total. Cost that is not logged is left out.";

// ---------------------------------------------------------------- usage trends
function xTicks(R, pw) {
  const span = R.b - R.a2;
  const cands = [H, 2 * H, 3 * H, 6 * H, 12 * H, D, 2 * D, 7 * D].filter(s => s >= Math.min(R.step, D));
  const step = cands.find(s => pw / (span / s) >= 74) || 7 * D;
  const out = [];
  let t = step >= D ? addDays(dayStart(R.a2), 1) : nextBucket(bucketStart(R.a2, step), step);
  if (step >= D && dayStart(R.a2) === R.a2) t = R.a2;
  for (let i = 0; t < R.b && i < 400; i++, t = step >= D ? addDays(t, Math.round(step / D)) : nextBucket(t, step)) {
    const midnight = zonedHour(t) === 0;
    out.push({ t, major: midnight, label: step >= D ? mdTxt(t) : midnight ? `${wdTxt(t)} ${new Date(t).getDate()}` : hourTxt(t) });
  }
  return out;
}
/** 45-degree hatch lines (6 px apart) inside a rectangle, for the logs not read yet. */
function hatch(x0, y0, x1, y1) {
  let d = '';
  const gap = 6 * Math.SQRT2;
  for (let c = x0 + y0; c < x1 + y1; c += gap) {
    // the line x + y = c inside the box
    const ax = clamp(c - y1, x0, x1), ay = c - ax;
    const bx = clamp(c - y0, x0, x1), by = c - bx;
    if (ay < y0 - 0.01 || ay > y1 + 0.01 || by < y0 - 0.01 || by > y1 + 0.01) continue;
    d += `M${pt(ax)} ${pt(ay)}L${pt(bx)} ${pt(by)}`;
  }
  return d;
}
export const TREND_SAMPLES = 360;
/** The trend chart: paths in plot-local pixels (the plot is the box less TREND_PAD), axes, ticks and readouts. */
export function trendView(A, R, state, size) {
  const W = Math.max(200, size?.w || DEFAULT_SIZES.trend.w), Hc = Math.max(160, size?.h || DEFAULT_SIZES.trend.h);
  const pad = TREND_PAD;
  const pw = Math.max(40, W - pad.l - pad.r), ph = Math.max(40, Hc - pad.t - pad.b);
  const B = A.available ? buckets(A, R, state) : [];
  const X = t => R.b === R.a2 ? 0 : (t - R.a2) / (R.b - R.a2) * pw;
  const inc = TYPES.filter(t => t.k !== 'cr' || state.cache);
  const maxTok = Math.max(0, ...B.map(b => inc.reduce((s, t) => s + b[t.k], 0)));
  const costOk = !A.costMissing;
  const maxCost = costOk ? Math.max(0, ...B.map(b => b.cost)) : 0;
  let ts, cs, best = Infinity;
  for (const k of Hc < 300 ? [3, 4] : [4, 5, 6]) {
    const a = niceScale(maxTok * 1.04, k), c = niceScale(maxCost * 1.04, k);
    const waste = Math.max(maxTok > 0 ? a.top / maxTok : 1, maxCost > 0 ? c.top / maxCost : 1);
    if (waste < best - 1e-9) { best = waste; ts = a; cs = c; }
  }
  const Y = v => ph - v / ts.top * ph, YC = v => ph - v / cs.top * ph;
  let xs = B.map(b => X(b.mid)), Bn = B;
  if (B.length === 1) { Bn = [B[0], B[0]]; xs = [X(B[0].lo), X(B[0].hi)]; }
  const x0 = xs.length ? xs[0] : 0, x1 = xs.length ? xs.at(-1) : pw;
  const fns = Object.fromEntries(TYPES.map(t => [t.k, xs.length ? monoFn(xs, Bn.map(b => b[t.k])) : () => 0]));
  const fc = xs.length ? monoFn(xs, Bn.map(b => b.cost)) : () => 0;
  const lv = [[], [], [], [], []], cost = [];
  for (let i = 0; i < TREND_SAMPLES; i++) {
    const x = x0 + (x1 - x0) * i / (TREND_SAMPLES - 1);
    let acc = 0;
    lv[0].push(Y(0));
    TYPES.forEach((t, j) => { if (t.k !== 'cr' || state.cache) acc += fns[t.k](x); lv[j + 1].push(Y(acc)); });
    cost.push(YC(fc(x)));
  }
  const empty = !B.length || (maxTok === 0 && maxCost === 0);
  // axes: horizontal gridlines and both tick columns share k divisions
  const yTicks = [];
  for (let i = 0; i <= ts.k; i++) yTicks.push({ y: pt(Y(ts.step * i)), left: i ? nfAxis.format(ts.step * i) : '0', right: costOk ? moneyAxis(cs.step * i, cs.step) : '', base: i === 0 });
  const xt = xTicks(R, pw).map(tk => ({ x: pt(X(tk.t)), label: tk.label, major: tk.major })).filter(tk => tk.x >= 18 && tk.x <= pw - 18);
  let minor = '';
  for (const tk of xt) if (!tk.major) minor += dashLine(tk.x, 0, tk.x, ph, 2, 4);
  const tail = A.win1 < R.b && A.win1 > R.a2 ? pt(X(A.win1)) : -1;
  // the readout's Claude / Codex cost split adds up to the bucket only while no other provider is in the range;
  // with other providers in range it says instead how many tokens each provider served in the bucket
  const showSplit = allPicked(state) && costOk && !B.some(b => b.others);
  const showProviders = allPicked(state) && B.some(b => b.others);
  const partial = costOk && B.some(b => b.unk);
  const bucketsOut = B.map(b => {
    const top = inc.reduce((s, t) => s + b[t.k], 0), all = b.in + b.out + b.cw + b.cr;
    const when = R.step >= D ? `${wmdTxt(b.t)}${b.partial ? `, ${clockTxt(b.lo)} to ${clockTxt(b.hi)}` : ''}`
      : `${wmdTxt(b.t)} · ${hourTxt(b.lo)} to ${b.hi === b.t1 ? hourTxt(b.hi) : clockTxt(b.hi)}`;
    const split = showSplit ? ['claude', 'codex'].map(p => `${A.label(p)} ${money(b.pc[p] || 0)}`).join(' · ') : '';
    const costFoot = costOk && b.unk ? split ? `${split} · partial` : 'Partial: some cost here is not logged' : split;
    const foot = [costFoot, b.t1 > A.win1 ? `Logs read ${clockTxt(A.win1)}; later activity is not in yet` : ''].filter(Boolean);
    return {
      // yCost -1: no logged cost in this bucket, so the crosshair draws no cost marker (never a $0 point)
      x: pt(X(b.mid)), yTok: pt(Y(top)), yCost: !costOk ? pt(ph) : b.unk && !(b.cost > TINY) ? -1 : pt(YC(b.cost)), time: when.toUpperCase(),
      vin: tokC(b.in), vout: tokC(b.out), vcw: tokC(b.cw), vcr: tokC(b.cr), all: tokC(all), cost: !costOk ? 'Unavailable' : b.unk && !(b.cost > TINY) ? NOT_LOGGED : money(b.cost),
      tin: tokX(b.in), tout: tokX(b.out), tcw: tokX(b.cw), tcr: tokX(b.cr), tall: tokX(all), crDim: !state.cache,
      byProvider: showProviders ? Object.keys(b.ptok).filter(p => b.ptok[p] > 0).sort(byProviderOrder).map(p => `${A.label(p)} ${tokC(b.ptok[p])}`).join(' · ') : '',
      foot: foot[0] || '', foot2: foot[1] || '',
    };
  });
  // nearest bucket per 2 px column of the plot, so the crosshair needs no search in Slint
  const lut = [];
  if (bucketsOut.length) {
    let j = 0;
    for (let c = 0; c <= Math.ceil(pw / 2); c++) {
      const mx = c * 2;
      while (j + 1 < bucketsOut.length && Math.abs(bucketsOut[j + 1].x - mx) <= Math.abs(bucketsOut[j].x - mx)) j++;
      lut.push(j);
    }
  }
  const legend = [];
  if (state.split) TYPES.forEach(t => { if (t.k !== 'cr' || state.cache) legend.push({ key: t.k, label: t.label, note: '' }); });
  else legend.push({ key: 'tok', label: state.cache ? 'All tokens' : 'Tokens without cache reads', note: 'left axis' });
  legend.push({ key: 'cost', label: 'Estimated cost', note: !costOk ? 'unavailable for this range' : partial ? 'right axis, USD, partial' : 'right axis, USD' });
  return {
    w: W, h: Hc, pw: pt(pw), ph: pt(ph),
    geo: { x0, x1, lv, cost },
    sub: `${stepWord(R.step)} buckets · ${provName(state, A)}${R.clipped ? ` · logs start ${mdTxt(R.a2)}` : ''}`,
    split: !!state.split, cache: !!state.cache, costShown: costOk,
    empty, emptyText: !A.available ? (A.message || 'CLI usage logs are unavailable.') : empty ? `No ${provWords(state, A)}activity was logged in this range.` : '',
    yTicks, xTicks: xt, minor,
    tailX: tail, tailLabel: tail >= 0 ? `LOGS READ ${clockTxt(A.win1).toUpperCase()}` : '', hatch: tail >= 0 ? hatch(tail, 0, pw, ph) : '',
    clipNote: R.clipped ? `LOGS START ${wmdTxt(R.a2).toUpperCase()}, ${hourTxt(R.a2).toUpperCase()}` : '',
    buckets: bucketsOut, lut, legend,
  };
}
/** The trend's eleven path strings for a geometry (also every frame of the range-change morph). */
export function trendPaths(geo) {
  const { x0, x1, lv, cost } = geo;
  const out = { total: bandD(x0, x1, lv[4], lv[0]), totalLine: lineD(x0, x1, lv[4]), cost: lineD(x0, x1, cost) };
  for (let j = 0; j < 4; j++) { out[`band${j}`] = bandD(x0, x1, lv[j + 1], lv[j]); out[`line${j}`] = lineD(x0, x1, lv[j + 1]); }
  return out;
}
export function mixGeo(from, to, k) {
  const mix = (a, b) => a + (b - a) * k;
  return { x0: mix(from.x0, to.x0), x1: mix(from.x1, to.x1), lv: to.lv.map((row, j) => row.map((v, i) => mix(from.lv[j][i], v))), cost: to.cost.map((v, i) => mix(from.cost[i], v)) };
}

// ---------------------------------------------------------------- models: cost by model + donut
/**
 * The page's model rows: every provider's rows of one model merged into one, because models are the division.
 * `provider` is the provider that served all of the model's usage here (its mark; Claude and Codex also have their
 * colour family); otherwise, or for "other", it is '' and the model has no mark. A muse-spark model is Muse's own
 * model however it was routed, so it keeps Muse's mark whenever Muse served any of it. `providers` and `tools` say
 * who served it and which logs hold it. `logged` is the cost that is logged or priced at a listed rate; `unk` says
 * some cost is not logged; `na` says an estimate is missing.
 */
export function pageModels(A, state) {
  const groups = new Map();
  for (const m of A.models) {
    if (!provOK(state, m.provider)) continue;
    const g = groups.get(m.model) || { model: m.model, parts: [] };
    g.parts.push(m);
    groups.set(m.model, g);
  }
  return [...groups.values()].map(({ model, parts }) => {
    const providers = new Set(parts.map(m => m.provider));
    const only = providers.size === 1 ? [...providers][0] : /^muse-spark([-_]|$)/i.test(model) && providers.has('muse') ? 'muse' : '';
    const tools = new Set(parts.flatMap(m => m.tools));
    const tok = Object.fromEntries(TYPES.map(t => [t.k, parts.reduce((s, m) => s + m.tok[t.k], 0)]));
    const total = tok.in + tok.out + tok.cw + tok.cr;
    const logged = parts.reduce((s, m) => s + (m.logged ?? 0), 0);
    const unk = parts.some(m => m.unk), na = parts.some(m => m.logged === null);
    const exact = !unk && !na && parts.every(m => m.mode === 'rates');
    const cost = exact ? Object.fromEntries(TYPES.map(t => [t.k, parts.reduce((s, m) => s + m.cost[t.k], 0)]))
      : Object.fromEntries(TYPES.map(t => [t.k, total > 0 ? tok[t.k] / total * logged : 0]));
    const r0 = parts[0].rate;
    const rate = r0 && parts.every(m => m.rate && m.rate.source === r0.source && TYPES.every(t => m.rate[t.k] === r0[t.k])) ? r0 : null;
    return {
      model, key: model, provider: markOf(only), providers: [...providers].sort(byProviderOrder), tools, tok, total, logged, unk, na, cost, rate,
      mode: exact ? 'rates' : 'shares', unreconciled: parts.some(m => A.unreconciled.includes(m)),
      // nothing known: every part is not logged or missing
      none: (unk || na) && !(logged > TINY),
    };
  });
}
const active = m => m.total > 0 || m.logged > 0 || m.unk;
const byCost = (a, b) => (b.logged || 0) - (a.logged || 0) || b.total - a.total || a.model.localeCompare(b.model);
const byTokens = (a, b) => b.total - a.total || (b.logged || 0) - (a.logged || 0) || a.model.localeCompare(b.model);
/** The order of Cost by model (and of the model indexes the donut's popovers use): by cost, or by tokens. */
export const cbmSortOf = state => state?.cbmSort === 'tokens' ? 'tokens' : 'cost';
const modelsShown = (A, state) => pageModels(A, state).filter(active).sort(cbmSortOf(state) === 'tokens' ? byTokens : byCost);
const hiddenModels = (A, state) => pageModels(A, state).filter(m => !active(m));
/** The cost of a model row: logged (or listed) cost; "Free" at a listed zero rate, "Not logged" when none of it is, never $0.00. */
const modelCost = m => m.none ? (m.unk ? NOT_LOGGED : 'Unavailable') : isFreeRate(m.rate) && !(m.logged > TINY) ? FREE : money(m.logged);
/**
 * One colour per model in its family (its provider's hue, or the neutral family of models that are no one
 * provider's alone): full tone for the largest, lighter steps after it. The neutral family can hold many models,
 * so it spreads them over an even ramp and interleaves it, so neighbours by size never share a tone.
 */
export function modelShades(list) {
  const out = {}, n = {};
  const mixes = [1, 0.66, 0.42, 0.24];
  const sorted = list.slice().sort(byCost);
  // each provider's models share its hue; a model that is no one provider's alone draws in the neutral family
  const family = m => m.provider;
  const neutral = sorted.filter(m => family(m) === '').length;
  const half = Math.ceil(neutral / 2);
  sorted.forEach(m => {
    const f = family(m);
    const i = n[f] = (n[f] || 0) + 1;
    if (f !== '' || neutral <= mixes.length) { out[m.key] = mixes[Math.min(3, i - 1)]; return; }
    const k = (i - 1) % 2 === 0 ? (i - 1) / 2 : half + (i - 2) / 2;
    out[m.key] = Math.round((1 - 0.72 * k / (neutral - 1)) * 1000) / 1000;
  });
  return out;
}
function ioStatus(r) {
  if (r >= 200) return 'Extended thinking or large context loading. Expected for reasoning models.';
  if (r >= 50) return 'More input than output. Typical for analysis tasks.';
  if (r >= 5) return 'Balanced input to output ratio for typical coding tasks.';
  return 'More output than input. A generation-heavy workload.';
}
const ioTxt = v => v >= 10 ? nf0.format(v) : nf2v.format(v);
const RATE_SOURCE = { builtin: 'CCS pricing table', 'models-dev': 'models.dev rates, as CCS resolves them', fallback: 'CCS fallback rate for models it does not list, an estimate with a fallback rate' };
function rateText(m) {
  if (m.rate && isFreeRate(m.rate)) return 'Free: this model has no per-token charge at its listed rate, so its cost is $0. It is a known price, never unknown.';
  if (m.rate) {
    const base = `${money(m.rate.in)} in, ${money(m.rate.out)} out, ${money(m.rate.cw)} cache write, ${money(m.rate.cr)} cache read per million tokens; ${RATE_SOURCE[m.rate.source] || 'CCS rate'}.`;
    return `${base}${m.mode === 'rates' ? ' The four parts add up to the logged estimate.' : ' These rates do not reconcile with the logged estimate, so the split shows token shares.'}`;
  }
  if (m.none && m.unk) return 'No cost is logged for this model and CCS lists no rate for it, so its cost is not logged. It is never counted as $0.';
  if (m.unk) return 'Part of this cost was logged with the usage; the rest has no logged cost and no listed rate, so it is not logged. The split shows token shares of the logged part.';
  return 'CCS lists no rate for this model; the cost was logged with the usage. The split shows token shares.';
}
function modelRows(A, state, windowText) {
  const list = modelsShown(A, state);
  const max = Math.max(0, ...list.map(m => m.logged || 0)) || 1;
  const total = list.reduce((s, m) => s + (m.logged || 0), 0) || 1;
  const totTok = list.reduce((s, m) => s + m.total, 0);
  return list.map((m, i) => {
    const sum = m.cost.in + m.cost.out + m.cost.cw + m.cost.cr;
    const frac = k => sum > 0 ? m.cost[k] / sum : 0;
    const io = m.tok.out > 0 ? m.tok.in / m.tok.out : null;
    const ioCtx = m.tok.out > 0 ? (m.tok.in + m.tok.cr + m.tok.cw) / m.tok.out : null;
    const maxType = Math.max(...TYPES.map(t => m.cost[t.k]), 0) || 1;
    const partial = (m.unk || m.na) && !m.none;
    const free = !m.none && isFreeRate(m.rate) && !(m.logged > TINY);
    const typeCost = k => m.none ? m.unk ? NOT_LOGGED : 'Unavailable' : free ? FREE : money(m.cost[k]);
    // who served the usage, and which logs hold it ("Qwen token plan · OMP logs")
    const tools = m.tools.size ? ` · ${toolNames(m.tools)} logs` : '';
    const from = `${andList(m.providers.map(p => A.label(p)))}${tools}`;
    return {
      key: m.key, name: m.model, provider: m.provider, idx: i,
      w: m.none ? 0 : Math.max(0.4, (m.logged || 0) / max * 100),
      fin: frac('in'), fout: frac('out'), fcw: frac('cw'), fcr: frac('cr'),
      tipIn: `Input: ${typeCost('in')} for ${tokX(m.tok.in)}`, tipOut: `Output: ${typeCost('out')} for ${tokX(m.tok.out)}`,
      tipCw: `Cache write: ${typeCost('cw')} for ${tokX(m.tok.cw)}`, tipCr: `Cache read: ${typeCost('cr')} for ${tokX(m.tok.cr)}`,
      tok: tokC(m.total), tokTip: `${tokX(m.total)} · ${from}`, cost: modelCost(m), costNa: m.none, partial,
      share: m.none ? '' : share1((m.logged || 0) / total * 100),
      // the model detail popover
      sub: `${from} · ${windowText}`,
      usage: share1(totTok ? m.total / totTok * 100 : 0),
      types: TYPES.map(t => ({ key: t.k, label: t.long, tok: m.tok[t.k] ? tokC(m.tok[t.k]) : 'None', tip: tokX(m.tok[t.k]), cost: m.tok[t.k] ? typeCost(t.k) : money(0), w: m.cost[t.k] > 0 ? Math.max(1.5, m.cost[t.k] / maxType * 100) : 0, none: !m.tok[t.k] })),
      io: io === null ? 'Unavailable' : `${ioTxt(io)} to 1`,
      ioNote: io === null ? 'No output was logged, so there is no ratio.' : ioStatus(io),
      ioQuiet: io === null ? '' : `Uncached input divided by output, as in the original. Counting cache reads and writes as input, it is ${ioTxt(ioCtx)} to 1: ${share2((m.tok.cr + m.tok.cw) / Math.max(1, m.tok.in + m.tok.cr + m.tok.cw) * 100)}% of this model's input side arrived through the cache.`,
      rate: `${rateText(m)}${partial ? ' The cost shown is partial.' : ''}`,
    };
  });
}
function donutView(A, state, shades) {
  const cost = state.donut === 'cost';
  // by cost, a model whose cost is not logged has no share to draw; it stays in the token view
  const list = modelsShown(A, state);
  const drawn = cost ? list.filter(m => m.logged > TINY) : list;
  const val = m => cost ? (m.logged || 0) : m.total;
  const total = drawn.reduce((s, m) => s + val(m), 0);
  const segs = [], small = [];
  drawn.slice().sort((a, b) => val(b) - val(a)).forEach(m => { if (total && val(m) / total < 0.01) small.push(m); else segs.push(m); });
  const idxOf = m => list.indexOf(m);
  const seg = m => ({ key: m.key, name: m.model, provider: m.provider, mix: shades[m.key] ?? 1, other: false, v: val(m), model: m.key, tip: '', idx: idxOf(m) });
  const out = segs.map(seg);
  if (small.length === 1) out.push(seg(small[0]));
  else if (small.length) out.push({ key: '_other', name: `${small.length} smaller models`, provider: '', mix: 1, other: true, v: small.reduce((s, m) => s + val(m), 0), model: '', tip: small.map(m => m.model).join(', '), idx: -1 });
  let a = 0;
  const rows = out.map(s => {
    const span = total ? s.v / total * Math.PI * 2 : 0;
    const row = { ...s, a0: a, a1: a + span, share: share1(total ? s.v / total * 100 : 0), shareNum: total ? s.v / total * 100 : 0, value: cost ? money(s.v) : tokC(s.v), valueTip: cost ? '' : tokX(s.v), label: total && s.v / total >= 0.05 };
    a += span;
    delete row.v;
    return row;
  });
  // the segment under each degree, clockwise from the top (hover in Slint needs no search)
  const lut = Array.from({ length: 360 }, (_, deg) => {
    const a = (deg + 0.5) / 180 * Math.PI;
    return rows.findIndex(r => a >= r.a0 && a < r.a1);
  });
  const partial = cost && list.some(m => m.unk || m.na);
  const allNone = cost && !drawn.length && list.some(m => m.unk);
  // every model drawn has no cost because each is free at a listed zero rate: the donut has no arcs, but the
  // legend below still lists every model, so this is a free range, never an empty one
  const allFree = cost && !drawn.length && !allNone && list.length > 0 && list.every(m => !m.unk && !m.na);
  // The legend: one row per arc. A group row opens into its models, so no model is only inside a group: "N
  // smaller models" (the arc of the models under 1%) and, by cost, the models that have no cost to draw (not
  // logged, or $0.00), which have no arc. A child row opens the model's detail; `arc` is the arc it lights.
  const open = new Set(state.donutOpen instanceof Set ? state.donutOpen : Array.isArray(state.donutOpen) ? state.donutOpen : []);
  const leg = (seg, kind, arc, isOpen = false) => ({ seg, kind, open: isOpen, arc });
  const child = (m, arc, valueText, shareText) => leg({ key: `${m.key}:child`, name: m.model, provider: m.provider, mix: shades[m.key] ?? 1, other: false, model: m.key, tip: '', a0: 0, a1: 0, share: shareText, shareNum: 0, value: valueText, valueTip: tokX(m.total), label: false, idx: idxOf(m) }, 'child', arc);
  const legend = [];
  rows.forEach((r, i) => {
    if (!r.other) { legend.push(leg(r, 'seg', i)); return; }
    legend.push(leg(r, 'group', i, open.has(r.key)));
    if (open.has(r.key)) for (const m of small) legend.push(child(m, i, cost ? money(val(m)) : tokC(val(m)), share1(total ? val(m) / total * 100 : 0)));
  });
  const undrawn = cost ? list.filter(m => !(m.logged > TINY)) : [];
  if (undrawn.length) {
    const allUnk = undrawn.every(m => m.unk || m.na);
    const n = undrawn.length;
    const name = `${n} model${n === 1 ? '' : 's'} not drawn`;
    const why = allUnk ? 'their cost is not logged' : 'their cost is $0.00 or not logged';
    legend.push(leg({ key: '_undrawn', name, provider: '', mix: 1, other: true, model: '', tip: `No arc: a share of cost needs a logged or listed cost above $0.00, and ${why}. Open to list them; each one's tip has its tokens.`, a0: 0, a1: 0, share: '', shareNum: 0, value: allUnk ? NOT_LOGGED : '', valueTip: tokX(undrawn.reduce((s, m) => s + m.total, 0)), label: false, idx: -1 }, 'group', -1, open.has('_undrawn')));
    if (open.has('_undrawn')) for (const m of undrawn.slice().sort(byTokens)) legend.push(child(m, -1, modelCost(m), ''));
  }
  return {
    mode: cost ? 'cost' : 'tokens', segs: rows, lut, legend,
    centre: cost ? allNone ? NOT_LOGGED : allFree ? FREE : money(total) : tokC(total),
    centreLabel: cost ? allFree ? 'estimated cost, free models' : `estimated cost, ${partial ? 'partial' : rows.length > 1 ? 'all models' : '1 model'}` : `tokens, ${rows.length > 1 ? 'all models' : '1 model'}`,
    unit: cost ? '' : ' tokens', empty: !rows.length && !allNone && !allFree,
    emptyText: allNone ? 'No model in these logs has a logged cost.' : 'No model activity in the logs.',
  };
}

// ---------------------------------------------------------------- session stats
// The session table lists at most this many sessions: the first five at the bottom of the Session stats
// box, the rest in the Recent sessions box, which continues the same list.
const SESS_TOP = 5, SESS_SHOWN = 25;
/**
 * Session stats over the providers in the filter: the summary numbers, one row per provider with
 * sessions, plus one session table continued across two boxes. The stats are the columns' sums and
 * means, so they always line up with their columns; a session that several providers served counts under
 * each of them, in the rows and in the Sessions number alike. Session stats shows the summary, the
 * provider rows, and the SESS_TOP most recent sessions as a compact table at the bottom; Recent sessions
 * continues the table with the next sessions up to SESS_SHOWN in total, same columns and styling, so the
 * two boxes read as one list. The average cost comes from the rows that have costs, and says partial when
 * some rows do not; only with no priced row at all is it not logged. Every native tool names a session on
 * every host it runs on, so sessions cover Ubuntu, Mac and Windows alike; a generic JSONL log names none,
 * and the note says so. The table lists the sample most recent first, without paths. The sample rows are
 * the server's AccountAnalyticsSessionRow shape (provider, lastActivity, string models, token totals);
 * anything else is dropped, never guessed.
 */
function sessionsView(A, state, now) {
  const all = A.sessions.filter(s => provOK(state, s.p));
  const sum = k => all.length && all.every(r => finite(r[k])) ? all.reduce((s, r) => s + r[k], 0) : null;
  const sessions = sum('sessions');
  const events = sum('events');
  const withSessions = all.filter(r => finite(r.sessions) && r.sessions > 0);
  const priced = withSessions.filter(r => !r.unk && finite(r.cost));
  const pricedSessions = priced.reduce((s, r) => s + r.sessions, 0);
  const pricedCost = priced.reduce((s, r) => s + r.cost, 0);
  const avg = pricedSessions > 0 ? pricedCost / pricedSessions : null;
  const avgPartial = finite(avg) && priced.length < withSessions.length;
  const evs = finite(sessions) && sessions > 0 && finite(events) ? events / sessions : null;
  // A saved generic JSONL source maps no session field, so its tokens are counted while it adds no sessions.
  const sessionless = all.some(r => A.tools(r.p).includes('jsonl'));
  // the sample covers the providers in the filter; without one, show every session
  const sample = [];
  for (const s of A.sessionSample) {
    if (!validProvider(s?.provider) || !provOK(state, s.provider)) continue;
    const last = Date.parse(text(s.lastActivity));
    if (!finite(last)) continue;
    sample.push({ s, last });
  }
  sample.sort((a, b) => b.last - a.last);
  const listed = sample.slice(0, SESS_SHOWN).map(({ s, last }) => {
    const names = (Array.isArray(s.models) ? s.models : []).map(text).filter(Boolean);
    const tok = TYPES.every(t => finite(s[t.f]) && s[t.f] >= 0) ? TYPES.reduce((n, t) => n + s[t.f], 0) : null;
    const est = finite(s.estimatedCostUsd) && s.estimatedCostUsd >= 0 ? s.estimatedCostUsd : null;
    const nl = notLoggedPart(s, est);
    const cost = est === null ? null : Math.max(0, est - nl);
    const unk = nl > TINY;
    return {
      tool: markOf(s.provider),
      models: (names.slice(0, 2).join(', ') + (names.length > 2 ? ` +${names.length - 2} more` : '')) || '—',
      tokens: tokC(tok),
      cost: unk && !(cost > TINY) ? NOT_LOGGED : money(cost),
      when: relTxt(last, now),
      tip: `${names.join(', ') || 'no models logged'} · ${tokX(tok)} · last activity ${clockTxt(last)}`,
    };
  });
  const recentFoot = sample.length > listed.length
    ? A.sessionsTruncated && finite(A.sessionTotal) && A.sessionTotal > sample.length
      ? `Most recent ${listed.length} of ${nf0.format(A.sessionTotal)} sessions in this range`
      : `Most recent ${listed.length} of ${sample.length} sessions in this range`
    : '';
  return {
    note: sessionless ? 'A generic JSONL usage log records no session, so its tokens are counted but it adds no sessions.' : '',
    recent: listed.slice(0, SESS_TOP),
    recentMore: listed.slice(SESS_TOP),
    moreSub: listed.length === 0 ? ''
      : listed.length === SESS_TOP + 1 ? `Session ${SESS_TOP + 1}, continued from Session stats`
        : listed.length > SESS_TOP ? `Sessions ${SESS_TOP + 1} to ${listed.length}, continued from Session stats`
          : 'Continued from Session stats',
    recentFoot,
    stats: [
      { key: 'sess', label: 'Sessions', num: sessions ?? 0, has: finite(sessions), fmt: 'int', text: intText(sessions) },
      { key: 'avg', label: 'Average estimated cost per session' + (avgPartial ? ' · partial' : ''), num: avg ?? 0, has: finite(avg), fmt: 'money', text: !finite(avg) && finite(sessions) && sessions > 0 ? NOT_LOGGED : money(avg) },
      { key: 'evs', label: 'Usage events per session', num: evs ?? 0, has: finite(evs), fmt: 'int', text: intText(evs) },
    ],
    rows: all.filter(r => !finite(r.sessions) || r.sessions > 0).sort((a, b) => byProviderOrder(a.p, b.p)).map(r => ({
      provider: markOf(r.p), label: r.label,
      sessions: finite(r.sessions) ? nf0.format(r.sessions) : 'Unavailable',
      per: finite(r.sessions) && r.sessions > 0 && r.unk ? NOT_LOGGED : finite(r.sessions) && r.sessions > 0 && finite(r.cost) ? money(r.cost / r.sessions) : 'Unavailable',
      events: finite(r.sessions) && r.sessions > 0 && finite(r.events) ? nf0.format(Math.round(r.events / r.sessions)) : 'Unavailable',
      eventsTip: finite(r.events) ? `${nf0.format(r.events)} usage events` : '',
    })),
  };
}

// ---------------------------------------------------------------- token breakdown and cache efficiency
function tokensView(K, C, costOk, available = true) {
  if (!available) return TYPES.map(t => ({ key: t.k, label: t.label, wt: 0, wc: 0, tok: 'Unavailable', tokTip: '', tokShare: '', cost: 'Unavailable', costShare: '' }));
  // shares of the logged cost; with none of it logged there is no cost to show, never $0.00
  const none = K.partial && !(K.cost > TINY);
  return TYPES.map(t => {
    const ts = K.tok ? K[t.k] / K.tok * 100 : 0, cs = costOk && K.cost ? C.cost[t.k] / K.cost * 100 : 0;
    return { key: t.k, label: t.label, wt: ts > 0 ? Math.max(0.6, ts) : 0, wc: cs > 0 ? Math.max(0.6, cs) : 0, tok: tokC(K[t.k]), tokTip: tokX(K[t.k]), tokShare: share1(ts), cost: !costOk ? 'Unavailable' : none ? NOT_LOGGED : money(C.cost[t.k]), costShare: costOk && !none ? share1(cs) : '' };
  });
}
function cacheView(A, rows, K, C, costOk) {
  const hit = K.cr + K.cw > 0 ? K.cr / (K.cr + K.cw) * 100 : null;
  // savings = cache-read tokens x (input rate - cache-read rate), per provider at its listed models' blended rates
  let save = 0;
  for (const p of new Set(rows.map(r => r.p))) {
    const crTok = rows.filter(r => r.p === p).reduce((s, r) => s + r.cr, 0);
    const list = A.models.filter(m => m.provider === p && m.rate && m.tok.cr > 0);
    const crAll = list.reduce((s, m) => s + m.tok.cr, 0);
    const perTok = crAll > 0 ? list.reduce((s, m) => s + m.tok.cr * Math.max(0, m.rate.in - m.rate.cr) / 1e6, 0) / crAll : 0;
    save += crTok * perTok;
  }
  const ccost = C.cost.cw + C.cost.cr;
  const tot = K.cr + K.cw;
  const sh = v => tot > 0 ? share2(v / tot * 100) : 'Unavailable';
  // with none of the cost logged, the cache cost is not logged either (never $0.00)
  const none = costOk && K.partial && !(K.cost > TINY);
  const c = v => !costOk ? 'Unavailable' : none ? NOT_LOGGED : money(v);
  return {
    hit: hit ?? 0, hasHit: finite(hit), hitText: finite(hit) ? share2(hit) : 'Unavailable',
    save: costOk && !none ? save : 0, hasSave: costOk && !none, saveText: c(save),
    ccost: costOk && !none ? ccost : 0, hasCcost: costOk && !none, ccostText: c(ccost),
    ccostShare: costOk && !none ? share1(K.cost ? ccost / K.cost * 100 : 0) : '',
    reads: { tok: tokC(K.cr), tip: tokX(K.cr), share: sh(K.cr), cost: c(C.cost.cr), w: K.cr },
    writes: { tok: tokC(K.cw), tip: tokX(K.cw), share: sh(K.cw), cost: c(C.cost.cw), w: K.cw, note: tot && K.cw === 0 ? 'none logged' : '' },
    readFrac: tot > 0 ? K.cr / tot : 0,
  };
}

// ---------------------------------------------------------------- weekday x hour heatmap
const WD = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
/** A dashed rounded rectangle (3 on, 3 off) for the heatmap's hours outside the logs. */
export function dashedRect(w, h, r = 4) {
  const x0 = 0.5, y0 = 0.5, x1 = Math.max(1, w - 0.5), y1 = Math.max(1, h - 0.5);
  const rr = Math.min(r, (x1 - x0) / 2, (y1 - y0) / 2);
  const pts = [];
  const arc = (cx, cy, a0) => { for (let k = 0; k <= 4; k++) { const a = (a0 + k * 22.5) * Math.PI / 180; pts.push([cx + rr * Math.cos(a), cy + rr * Math.sin(a)]); } };
  pts.push([x0 + rr, y0]); pts.push([x1 - rr, y0]); arc(x1 - rr, y0 + rr, -90);
  pts.push([x1, y1 - rr]); arc(x1 - rr, y1 - rr, 0);
  pts.push([x0 + rr, y1]); arc(x0 + rr, y1 - rr, 90);
  pts.push([x0, y0 + rr]); arc(x0 + rr, y0 + rr, 180);
  return dashPolyline(pts, [3, 3]);
}
// The dash depends only on the cell box, so only a resize of the heatmap rebuilds the ~200-point string.
let dashRectKey = '';
let dashRectPath = '';
function heatDash(width, height) {
  const key = `${width}x${height}`;
  if (key !== dashRectKey) {
    dashRectKey = key;
    dashRectPath = dashedRect(width, height);
  }
  return dashRectPath;
}
function heatView(A, R, state, width, height) {
  const cells = Array.from({ length: 7 }, () => Array.from({ length: 24 }, () => ({ v: null, n: 0, tok: 0, cost: 0, unk: false })));
  // hours inside the read window count (zero when nothing ran); hours outside it stay empty, never zero;
  // without readable activity every hour stays empty
  for (let t = Math.ceil(R.a2 / H) * H; A.available && t < R.end; t += H) { const c = cells[zonedWeekday(t)][zonedHour(t)]; if (c.v === null) c.v = 0; c.n++; }
  for (const r of rowsIn(A, R, state)) { const c = cells[zonedWeekday(r.t)][zonedHour(r.t)]; if (c.v === null) c.v = 0; c.tok += r.in + r.out + r.cw; c.cost += r.cost; if (r.unk) c.unk = true; }
  const byCost = state.heat !== 'tokens' && !A.costMissing;
  const partial = byCost && cells.some(row => row.some(c => c.unk));
  for (const row of cells) for (const c of row) if (c.v !== null) c.v = byCost ? c.cost : c.tok;
  const max = Math.max(0, ...cells.flat().filter(c => c.v !== null).map(c => c.v));
  const valTxt = v => byCost ? money(v) : `${tokC(v)} tokens`;
  const hourLab = h => hourTxt(new Date(2026, 0, 1, h).getTime());
  const wide = (width || DEFAULT_SIZES.heat.w) > 760;
  const out = [];
  cells.forEach((row, i) => row.forEach((c, h) => {
    const name = `${WD[i]} ${hourLab(h)}`;
    if (c.v === null) { out.push({ state: 0, k: 0, tip: `${name} · outside the logs in this range` }); return; }
    const what = c.v ? `${valTxt(c.v)}${byCost && c.unk ? ', partial' : ''}` : byCost && c.unk ? 'cost not logged' : 'nothing logged';
    out.push({ state: c.v === 0 ? 1 : 2, k: max > 0 ? Math.sqrt(c.v / max) : 0, tip: `${name} · ${what}${c.n > 1 ? ` · ${c.n} hours in range` : ''}` });
  }));
  return {
    mode: byCost ? 'cost' : 'tokens',
    sub: `${byCost ? partial ? 'Estimated cost, partial,' : 'Estimated cost' : 'Tokens without cache reads'} per hour, local time · ${dateLabel(R)}`,
    cells: out, days: WD,
    hours: Array.from({ length: 24 }, (_, h) => h % (wide ? 3 : 6) === 0 ? hourLab(h) : ''),
    busiest: max ? valTxt(max) : 'none',
    dash: heatDash(Math.max(4, ((width || DEFAULT_SIZES.heat.w) - 38 - 72) / 24), height || 24),
  };
}

// ---------------------------------------------------------------- daily cost by provider
/**
 * Daily (or hourly) cost by provider, stacked: Claude, Codex, then every other provider together in the neutral
 * colour (the legend stays three entries wide), so each bar adds up to the logged cost of every provider; the
 * readout lists each provider's cost. A picked provider is one series in its own hue. Cost that is not logged is
 * left out and the bar says so.
 */
function dailyView(A, R, state, size) {
  const step = R.end - R.a2 <= 2 * D + H ? H : D;
  const B = A.available ? buckets(A, { ...R, step }, state) : [];
  const W = Math.max(200, size?.w || DEFAULT_SIZES.daily.w), Hc = Math.max(120, size?.h || DEFAULT_SIZES.daily.h);
  const pad = DAILY_PAD;
  const pw = W - pad.l - pad.r;
  const costOk = !A.costMissing;
  const picked = allPicked(state) ? '' : state.prov;
  const rest = b => Object.entries(b.pc).reduce((s, [p, v]) => ownSeries(p) ? s : s + v, 0);
  const max = costOk ? Math.max(0, ...B.map(b => b.cost)) : 0;
  const sc = [3, 4, 5].map(k => niceScale(max * 1.04, k)).sort((a, b) => a.top - b.top || a.k - b.k)[0];
  const n = Math.max(1, B.length), slot = pw / n;
  const every = Math.max(1, Math.ceil(64 / slot));
  const yTicks = [];
  for (let i = 0; i <= sc.k; i++) yTicks.push({ y: i / sc.k, label: costOk ? moneyAxis(sc.step * i, sc.step) : '', base: i === 0 });
  const showClaude = picked ? picked === 'claude' : A.providers.includes('claude');
  const showCodex = picked ? picked === 'codex' : A.providers.includes('codex');
  const showOther = picked ? !ownSeries(picked) : A.others;
  const otherLabel = picked && !ownSeries(picked) ? A.label(picked) : 'Other providers';
  const bars = B.map((b, i) => {
    const cl = b.pc.claude || 0, cx = b.pc.codex || 0, ot = rest(b), tot = cl + cx + ot;
    const range = b.t1 > A.win1 ? `So far: logs read ${clockTxt(A.win1)}` : b.t < R.a2 ? `Logs start ${clockTxt(R.a2)}` : '';
    // the readout: every provider that served usage in the bucket, with its logged cost ("Not logged" when none)
    const served = Object.keys(b.ptok).filter(p => b.ptok[p] > 0 || (b.pc[p] || 0) > TINY).sort(byProviderOrder);
    return {
      h: costOk && sc.top > 0 ? tot / sc.top : 0, cl: tot > 0 ? cl / tot : 0, cx: tot > 0 ? cx / tot : 0,
      label: i % every ? '' : step === H ? (zonedHour(b.t) === 0 ? wdTxt(b.t) : hourTxt(b.t)) : mdTxt(b.t),
      time: `${step === H ? `${wmdTxt(b.t)} · ${hourTxt(b.t)}` : wmdTxt(b.t)}${b.partial ? ` · ${clockTxt(b.lo)} to ${clockTxt(b.hi)}` : ''}`.toUpperCase(),
      rows: costOk ? served.map(p => ({ p: ownSeries(p) ? p : '', label: A.label(p), value: (b.pc[p] || 0) > TINY || !b.punk[p] ? money(b.pc[p] || 0) : NOT_LOGGED })) : [],
      total: picked ? '' : money(tot),
      foot: costOk && b.unk ? [range, 'Partial: some cost is not logged'].filter(Boolean).join(' · ') : range,
    };
  });
  const empty = !B.length || max === 0;
  const unlogged = B.some(b => b.unk);
  const who = picked ? A.label(picked) : '';
  return {
    title: step === H ? 'Hourly cost by provider' : 'Daily cost by provider',
    sub: `${picked ? `${who} · estimated` : 'Estimated'}, USD · ${dateLabel(R)}`,
    yTicks, bars, empty,
    emptyText: !A.available ? (A.message || 'CLI usage logs are unavailable.') : !empty ? '' : !costOk ? 'Cost is unavailable for this range.'
      : unlogged ? `No ${who ? `${who} ` : ''}cost is logged in this range; the usage is in tokens above.` : `No ${who ? `${who} ` : ''}cost was logged in this range.`,
    showClaude, showCodex, showOther, claudeLabel: A.label('claude'), codexLabel: A.label('codex'), otherLabel,
    // the third series' hue: the picked provider's own, else '' (the neutral colour of every other provider)
    otherHue: picked && !ownSeries(picked) ? markOf(picked) : '',
  };
}

// ---------------------------------------------------------------- custom range picker (whole local days)
export function calendarView(A, R, now, apiAll) {
  // the backend keeps up to 30 days of logs; the 30-day window bounds the picker until it reports availableFrom
  const first = dayStart(Math.max(apiAll ?? now - 30 * D, now - 30 * D)), last = dayStart(now);
  const mon = t => zonedAddDays(t, -zonedWeekday(t));
  const g0 = mon(first), g1 = addDays(mon(last), 6);
  const cells = [];
  for (let t = g0, i = 0; t <= g1 && i < 60; t = addDays(t, 1), i++) {
    const d = new Date(t);
    cells.push({ day: isoDay(t), n: i, label: String(d.getDate()), month: d.getDate() === 1 ? F_MD.format(d).replace(/\s*\d+$/, '') : '', off: t < first || t > last, today: t === last, long: wmdTxt(t) });
  }
  const lo = cells.find(c => c.day === isoDay(R.a2))?.n ?? -1;
  const hi = cells.find(c => c.day === isoDay(Math.max(R.a2, R.b - 1)))?.n ?? lo;
  return { cells, lo, hi, firstN: cells.find(c => !c.off)?.n ?? 0, lastN: cells.findLast(c => !c.off)?.n ?? 0, note: `Logs read ${clockTxt(A.win1)} today. Whole local days within the last 30 days.` };
}

// ---------------------------------------------------------------- included usage (activity.sources)
const SOURCE_TOOLS = [['claude', 'Claude Code'], ['codex', 'Codex'], ['omp', 'OMP'], ['muse', 'Muse Code'], ['zcode', 'zcode'], ['jsonl', 'Generic JSONL'], ['antigravity', 'Antigravity'], ['cursor', 'Cursor']];
const SOURCE_HOSTS = [['ubuntu', 'Ubuntu'], ['mac', 'Mac'], ['windows', 'Windows']];
const SOURCE_STATES = ['ok', 'cached', 'unavailable', 'not_installed', 'scanning', 'no_usage'];
const andList = items => items.length <= 1 ? items.join('') : `${items.slice(0, -1).join(', ')} and ${items.at(-1)}`;
const noLocalLog = r => r.state === 'unavailable' && /no local usage log/i.test(text(r.detail));
const capFirst = v => text(v).replace(/^./, c => c.toUpperCase());
/**
 * The "Included usage" disclosure: one sentence naming the tools and computers whose logs were read, a plain note
 * for the tools that keep no local usage log, and a tool x computer grid of each source's state and last scan.
 * Tools with no local log on any host (Cursor; Antigravity from a server older than its reader) get the note only,
 * never a grid row of "No usage log" cells. It describes where the numbers come from; it never divides them (models stay the only division).
 * States: ok, no_usage, cached (with its age), scanning (a scan is working on the tool or has not reached it
 * yet), not_installed, unavailable (a real failure, with its reason in the tip).
 * tone: ok | cached | unavailable | scanning | quiet.
 */
export function includedView(payload, now = Date.now()) {
  const seen = new Set();
  const list = (Array.isArray(payload?.activity?.sources) ? payload.activity.sources : []).filter(r => SOURCE_TOOLS.some(([t]) => t === r?.tool)
    && SOURCE_HOSTS.some(([h]) => h === r?.host) && SOURCE_STATES.includes(r?.state) && !seen.has(`${r.tool}|${r.host}`) && seen.add(`${r.tool}|${r.host}`));
  if (!list.length) return { shown: false, label: '', line: '', hosts: [], rows: [], foot: '' };
  const read = r => r.state === 'ok' || r.state === 'cached' || r.state === 'no_usage';
  const tools = SOURCE_TOOLS.filter(([t]) => list.some(r => r.tool === t));
  const included = tools.filter(([t]) => list.some(r => r.tool === t && read(r))).map(([, l]) => l);
  const hosts = SOURCE_HOSTS.filter(([h]) => list.some(r => r.host === h && read(r))).map(([, l]) => l);
  const nolog = tools.filter(([t]) => list.filter(r => r.tool === t).every(noLocalLog));
  // Cursor (and Antigravity, from a server older than its reader) keep no local log; their quota still shows on Home.
  const quietOnly = nolog.length > 0 && nolog.every(([t]) => t === 'antigravity' || t === 'cursor');
  const nologNames = nolog.map(([, l]) => l);
  const nologLine = !nolog.length ? '' : quietOnly
    ? nolog.length === 1
      ? `${nologNames[0]} doesn't keep a local usage log; its quota readings still show on Home.`
      : `${andList(nologNames)} don't keep local usage logs; their quota readings still show on Home.`
    : `${andList(nologNames)} keep${nolog.length === 1 ? 's' : ''} no local usage log of ${nolog.length === 1 ? 'its' : 'their'} own; usage another tool routes through ${nolog.length === 1 ? 'it' : 'them'} still counts under ${nolog.length === 1 ? 'it' : 'them'}.`;
  const line = [
    included.length ? `Includes ${andList(included)}${hosts.length ? ` on ${andList(hosts)}` : ''}.` : 'No usage log could be read yet.',
    nologLine,
  ].filter(Boolean).join(' ');
  const when = r => Date.parse(text(r.lastScanAt));
  const events = r => Number.isInteger(r.rowCount) && r.rowCount >= 0 ? `${nf0.format(r.rowCount)} usage events kept` : '';
  const cell = (r, id) => {
    if (!r) return { id, text: 'Not read', tone: 'quiet', tip: 'This computer is not read for this tool.' };
    const t = when(r), last = finite(t) ? `Last scan ${timeTxt(t)}.` : 'Never scanned.';
    const why = text(r.detail) ? `${capFirst(r.detail)}. ` : '';
    if (r.state === 'scanning') return { id, text: 'Scanning…', tone: 'scanning', tip: `${why}${last}` };
    if (r.state === 'ok') return { id, text: finite(t) ? `Read ${relTxt(t, now)}` : 'Read', tone: 'ok', tip: [events(r), last].filter(Boolean).join(' · ') };
    if (r.state === 'cached') return { id, text: finite(t) ? `Cached, ${relTxt(t, now)}` : 'Cached', tone: 'cached', tip: `${why}Earlier records are shown until a scan finishes. ${last}` };
    if (r.state === 'no_usage') return { id, text: 'No usage in range', tone: 'quiet', tip: `${why}${last}` };
    if (noLocalLog(r)) return { id, text: 'No usage log', tone: 'quiet', tip: 'This tool keeps no local usage log to read.' };
    if (r.state === 'not_installed') return { id, text: 'Not installed', tone: 'quiet', tip: why || 'No usage logs for this tool were found on this computer.' };
    return { id, text: 'Unavailable', tone: 'unavailable', tip: `${why}${last}` };
  };
  const quiet = new Set(nolog.map(([t]) => t));
  const rows = tools.filter(([t]) => !quiet.has(t)).map(([t, label]) => ({ tool: label, cells: SOURCE_HOSTS.map(([h]) => cell(list.find(r => r.tool === t && r.host === h), h)) }));
  const scanning = list.filter(r => r.state === 'scanning').length;
  const cached = list.filter(r => r.state === 'cached').length, down = list.filter(r => r.state === 'unavailable' && !noLocalLog(r)).length;
  const flags = [scanning ? `${scanning} scanning` : '', cached ? `${cached} cached` : '', down ? `${down} unavailable` : ''].filter(Boolean).join(', ');
  return {
    shown: true, label: `Included usage${flags ? ` · ${flags}` : ''}`, line,
    hosts: SOURCE_HOSTS.map(([, l]) => l.toUpperCase()), rows,
    foot: 'Usage from every computer is merged, grouped by the provider that served it (the route each log records; a route no provider claims is under Other) and divided by model. A tool a scan has not finished says Scanning until one does; a source that cannot be read keeps its last scan (cached) or shows as unavailable with the reason. A missing reading is never counted as zero.',
  };
}

// ---------------------------------------------------------------- loading and convergence progress
/** The six tools every computer is scanned for; the progress counts only these. */
const PROGRESS_TOOLS = ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'];
/**
 * Per-host scan progress for the loading screen and the header pill, derived from the same sources
 * grid the disclosure shows: how many of the six tools have settled (read, no usage in range, not
 * installed, cached or failed) and how many a scan is still working on. A host with no grid row yet
 * counts as fully scanning only while a collection is actually running, so a settled page never
 * claims progress it does not have.
 */
export function hostProgress(payload) {
  const act = payload?.activity || {};
  const list = Array.isArray(act.sources) ? act.sources : [];
  const pending = new Set(Array.isArray(act.refreshingRemote) ? act.refreshingRemote.filter(h => h === 'mac' || h === 'windows') : []);
  const running = act.status === 'loading' || act.refreshing === true;
  return SOURCE_HOSTS.map(([h, name]) => {
    const cells = list.filter(r => r?.host === h && PROGRESS_TOOLS.includes(r?.tool));
    const total = PROGRESS_TOOLS.length;
    const scanning = cells.length ? cells.filter(r => r.state === 'scanning').length : running ? total : 0;
    const done = total - scanning;
    const live = h === 'ubuntu' ? act.refreshing === true : pending.has(h) || (running && !cells.length);
    // A host whose every tool failed to read is not "read": the loading screen says unavailable,
    // and the page shows the real reasons (the grid cells and their tips) as soon as it arms.
    const failed = cells.length > 0 && cells.every(r => r.state === 'unavailable' && !noLocalLog(r));
    const detail = scanning === 0
      ? failed ? 'unavailable' : cells.length || !running ? 'read' : 'queued'
      : live ? `${done} of ${total} tools · scanning now`
        : `${done} of ${total} tools · continues shortly`;
    return { key: h, name, done, total, scanning, detail };
  });
}

// ---------------------------------------------------------------- tokens by provider, and the provider picker
/** Rough text widths (px) of the summary line's 13 px sans labels and tabular values, for packing its lines. */
const LINE = { label: 160, gap: 26, mark: 21, char: 7.1, digit: 7.7, note: 92 };
const itemWidth = it => (it.mark ? LINE.mark : 0) + it.label.length * LINE.char + 5 + it.value.length * LINE.digit;
/**
 * "Tokens by provider": how many tokens each provider served in the range, for example "Claude 59.5B · Codex
 * 22.9B · Muse Code 2.73B · Z.ai coding plan 1.59B · Other 511M", largest first and Other last. It sums the same
 * hourly rows as Total tokens, so the parts add up to it, and it follows the range and the filter. It is a summary,
 * never a section: the charts stay divided by model. The items are packed into lines that fit `width`.
 */
export function providersView(A, rows, state, width) {
  const none = { shown: false, label: '', note: '', items: [], lines: [] };
  if (!A.available) return none;
  const by = {};
  for (const r of rows) {
    const o = by[r.p] || (by[r.p] = { in: 0, out: 0, cache: 0, cost: 0, unk: false });
    o.in += r.in; o.out += r.out; o.cache += r.cw + r.cr; o.cost += r.cost; if (r.unk) o.unk = true;
  }
  const tokOf = p => by[p].in + by[p].out + by[p].cache;
  const all = Object.keys(by).reduce((s, p) => s + tokOf(p), 0);
  const list = Object.keys(by).filter(p => tokOf(p) > 0).sort((a, b) => (a === 'other') - (b === 'other') || tokOf(b) - tokOf(a) || byProviderOrder(a, b));
  const items = list.map(p => {
    const o = by[p], tok = tokOf(p), label = A.label(p);
    const cost = A.costMissing ? 'estimated cost unavailable' : o.unk ? o.cost > TINY ? `${money(o.cost)} estimated cost, partial: some is not logged` : 'estimated cost not logged' : `${money(o.cost)} estimated cost`;
    const tools = A.tools(p);
    const from = tools.length ? ` From ${andList(tools.map(t => TOOL_LABEL[t]))} logs.` : '';
    const what = p === 'other' ? ' Routes no dashboard provider claims.' : '';
    return { key: p, label, mark: markOf(p), value: tokC(tok), quiet: false,
      tip: `${label}: ${tokX(tok)}, ${share1(all ? tok / all * 100 : 0)}% of the tokens in this range · ${tokC(o.in)} in, ${tokC(o.out)} out, ${tokC(o.cache)} cache · ${cost}.${from}${what}` };
  });
  if (!items.length) return none;
  // pack into lines: the first after the caption, the rest under it; the note ends the last line
  const room = Math.max(320, (width || DEFAULT_SIZES.trend.w) - LINE.label - 8);
  const lines = [];
  let line = [], used = 0;
  for (const it of items) {
    const w = itemWidth(it) + (line.length ? LINE.gap : 0);
    if (line.length && used + w > room) { lines.push(line); line = []; used = 0; }
    used += line.length ? w : itemWidth(it);
    line.push(it);
  }
  lines.push(line);
  return { shown: true, label: 'TOKENS BY PROVIDER', note: 'in this range', items, lines: lines.map((l, i) => ({ items: l, first: i === 0, last: i === lines.length - 1 })) };
}
/**
 * The top-right provider picker: All, then every provider that served usage in the range, in the dashboard's
 * order with its label and mark, Other last. A provider with no usage in the range is not offered, except the one
 * already picked (it stays, so its empty state can say so). Each choice carries its tokens in the range.
 */
export function providerChoices(A, R, state) {
  const tok = {};
  let total = 0;
  for (const r of A.available ? rowsIn(A, R, { prov: 'all' }) : []) { const n = r.in + r.out + r.cw + r.cr; bump(tok, r.p, n); total += n; }
  const list = Object.keys(tok).filter(p => tok[p] > 0);
  if (!allPicked(state) && !list.includes(state.prov)) list.push(state.prov);
  const items = [{ value: 'all', label: 'All providers', mark: '', tokens: A.available ? tokC(total) : '', off: false },
    ...list.sort(byProviderOrder).map(p => ({ value: p, label: A.label(p), mark: markOf(p), tokens: tok[p] > 0 ? tokC(tok[p]) : '0', off: !(tok[p] > 0) }))];
  const cur = allPicked(state) ? items[0] : items.find(it => it.value === state.prov);
  return { items, label: cur.label, mark: cur.mark };
}

// ---------------------------------------------------------------- the whole Usage part of the page
/**
 * state: { range: 24h|7d|30d|month|all|custom, from, to (local-day ms, custom only), prov: all|claude|codex,
 * split, cache, donut: tokens|cost, heat: cost|tokens, cbmSort: cost|tokens, donutOpen: the open legend groups
 * (_other, _undrawn) }. opts: { now, sizes }.
 */
function headOf(A, R, state, now, zone, progress) {
  const remote = [...new Set(Array.isArray(A.refreshingRemote) ? A.refreshingRemote : [])]
    .filter(h => h === 'mac' || h === 'windows')
    .sort((a, b) => (a === 'mac' ? 0 : 1) - (b === 'mac' ? 0 : 1))
    .map(h => (h === 'mac' ? 'Mac' : 'Windows'));
  const counts = progress.filter(p => p.scanning > 0).map(p => `${p.name} ${p.done} of ${p.total} tools`).join(', ');
  // One calm indicator for every background update: a scan running now, or cells
  // still converging between scans. Nothing else on the page moves for it.
  const updating = A.refreshing || counts !== '';
  const updateNote = !updating ? ''
    : A.refreshing ? remote.length ? `Updating · refreshing ${remote.join(' and ')}…` : 'Updating…'
      : `Updating · ${counts} · continues shortly`;
  return {
    scope: `CLI usage logs · local time${zone ? ` (${zone})` : ''} · read `,
    read: relTxt(A.win1, now),
    updating,
    updateNote,
    readTip: `Logs last read ${timeTxt(A.win1)}.${A.status === 'cached' ? ' Records read so far are shown; a bounded scan continues in the background.' : ''}`,
    date: dateLabel(R), custom: state.range === 'custom', range: state.range, prov: state.prov || 'all',
  };
}
/**
 * The analytics header alone: the 15-second "logs read …" tick needs activityData and pageRange
 * only, not the trend, models, donut, heat and calendar blocks. Always equals usageView(...).head.
 */
export function usageHead(payload, state, opts = {}) {
  const now = opts.now ?? Date.now();
  const A = activityData(payload, now);
  const R = pageRange(state, A, now);
  return headOf(A, R, state, now, zoneName(now), hostProgress(payload));
}
export function usageView(payload, state, opts = {}) {
  const now = opts.now ?? Date.now();
  const A = activityData(payload, now);
  const R = pageRange(state, A, now);
  const rows = rowsIn(A, R, state), K = sumRows(rows), C = typeCosts(A, rows);
  const costOk = !A.costMissing;
  const zone = zoneName(now);
  const windowText = `${mdTxt(A.win0)} to ${mdTxt(Math.min(A.win1, now))}`;
  // per-model rows and session counts cover the fetched API window; they cannot follow a different range
  const apiMatches = ['24h', '7d', '30d', 'all'].includes(state.range) && (state.range === 'all' ? A.apiPreset === '30d' : A.apiPreset === state.range);
  const rangeName = state.range === 'custom' ? 'custom' : RANGES.find(r => r[0] === state.range)?.[1] || '';
  const wholeNote = kind => apiMatches || !A.available ? '' : `${kind === 'sessions' ? 'Session counts' : 'Per-model data'} cover the logs read for ${windowText}, not the ${rangeName} range.`;
  const shown = modelsShown(A, state);
  const shades = modelShades(shown);
  const hid = hiddenModels(A, state);
  const unrec = shown.filter(m => m.unreconciled);
  const notLogged = shown.filter(m => m.unk);
  // models with some logged cost but no listed rate (a model with no logged cost has no split to show)
  const noRate = shown.filter(m => m.mode !== 'rates' && !m.unreconciled && !m.none).length;
  const foot = [
    hid.length ? `${hid.length === 1 ? hid[0].model : `${hid.length} entries`} ${hid.length === 1 ? 'is' : 'are'} left out: no tokens or cost logged.` : '',
    unrec.length ? `${unrec.map(m => m.model).join(', ')}: rates do not reconcile with the logged cost, so the split shows token shares.` : '',
    notLogged.length ? `${notLogged.length <= 3 ? andList(notLogged.map(m => m.model)) : `${notLogged.length} models`} ${notLogged.length === 1 ? 'has' : 'have'} cost with no logged amount and no listed rate: it shows as not logged and is left out of the totals.` : '',
  ].filter(Boolean).join(' ');
  const statusNote = A.available ? '' : A.message || 'CLI usage logs are unavailable.';
  const sessions = sessionsView(A, state, now);
  const progress = hostProgress(payload);
  const converging = progress.some(p => p.scanning > 0);
  // The page stays behind the loading screen until the first useful view is ready: numbers to
  // show, or a settled failure the header can explain. A cold answer that is still converging
  // never draws a page of "Unavailable" cards that later pops into numbers.
  const ready = A.available || !(A.status === 'loading' || A.refreshing || converging);
  return {
    available: A.available, statusNote, cached: A.status === 'cached',
    ready,
    loading: { hosts: progress.map(p => ({ name: p.name, detail: p.detail })) },
    head: headOf(A, R, state, now, zone, progress),
    scope: [
      { icon: 'terminal', text: 'Usage from the CLI logs listed under Included usage, grouped by the provider that served it: Claude Code, Codex, Muse Code and Antigravity are their own provider, OMP and zcode record the route of every call, and generic JSONL logs count under the provider their model names, else Other. A route no provider claims is under Other. Tokens by provider, under the totals, says how much each served; pick one to see its usage by model.' },
      { icon: 'wallet', text: 'Cost is an estimated API equivalent at the rates CCS prices each model at, or the cost the log recorded; it is not a bill. Cost with neither shows as not logged, and totals without it say partial.' },
      { icon: 'users', text: 'Activity covers all accounts together; it cannot be attributed to one account.' },
      { icon: 'layers', text: unrec.length || noRate ? [
        unrec.length ? `${unrec.length} model${unrec.length === 1 ? '' : 's'} do not reconcile with these rates; their split uses token shares.` : '',
        noRate ? `${noRate} model${noRate === 1 ? ' has' : 's have'} no listed rate; ${noRate === 1 ? 'its' : 'their'} split uses token shares of the logged cost.` : '',
      ].filter(Boolean).join(' ') : "Per-type costs use each model's rate as CCS prices it and match every model's logged estimate to the cent." },
      { icon: 'clock', text: `Logs read ${timeTxt(A.win1)}. Hours are shown in local time${zone ? ` (${zone})` : ''}.${A.status === 'cached' ? ' Records read so far are shown; a bounded scan continues in the background.' : ''}` },
    ],
    kpis: kpis(A, R, state, rows, K, C),
    providers: providersView(A, rows, state, opts.sizes?.trend?.w),
    picker: providerChoices(A, R, state),
    apportTip: APPORT_TIP,
    trend: trendView(A, R, state, opts.sizes?.trend),
    cbm: { sub: `${windowText} · ${shown.length} model${shown.length === 1 ? '' : 's'} by ${cbmSortOf(state)} · select one for detail`, note: wholeNote('models'), sort: cbmSortOf(state), rows: modelRows(A, state, windowText), foot, empty: A.available ? `No model activity ${provWords(state, A) ? `for ${provName(state, A)} ` : ''}in the logs.` : statusNote },
    donut: { sub: `Share of the logs read for ${windowText}`, note: wholeNote('models'), ...donutView(A, state, shades) },
    sessions: { sub: `Logs read for ${windowText}`, note: [wholeNote('sessions'), sessions.note].filter(Boolean).join(' '), foot: sessions.recentFoot, moreSub: sessions.moreSub, stats: sessions.stats, rows: sessions.rows, recent: sessions.recent, recentMore: sessions.recentMore },
    tokens: { sub: `${dateLabel(R)} · ${provName(state, A)}${costOk && K.partial ? ' · cost partial' : ''}`, rows: tokensView(K, C, costOk, A.available) },
    cache: { sub: `${dateLabel(R)} · ${provName(state, A)}${costOk && K.partial ? ' · cost partial' : ''}`, ...cacheView(A, rows, K, C, costOk && A.available) },
    included: includedView(payload, now),
    heat: heatView(A, R, state, opts.sizes?.heat?.w, opts.sizes?.heat?.h),
    daily: dailyView(A, R, state, opts.sizes?.daily),
    calendar: calendarView(A, R, now),
    range: R,
  };
}
