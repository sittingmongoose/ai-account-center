// Analytics, Usage blocks (version 3): the header, KPI row, usage trends, cost by model, model donut, session
// stats, token breakdown, cache efficiency, weekday x hour heatmap and daily cost by provider, built like the
// approved Daylight Atlas concept (c-daylight-atlas/app-analytics.js). Pure functions of the analytics response
// and the page state; every chart geometry is computed here and drawn by ui/pages/analytics/*.slint.
//
// Truthfulness: activity is Ubuntu Claude Code and Codex logs only; cost is an estimated API equivalent, not a
// bill; activity covers all accounts and is never attributed to one or added to quota numbers; a missing reading
// is unavailable, never zero; at most two decimals; local time throughout.
import { reconcile } from './model-rates.mjs';

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
const dtf = options => new Intl.DateTimeFormat(undefined, options);
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

// ---------------------------------------------------------------- local-time calendar
export const dayStart = t => { const d = new Date(t); d.setHours(0, 0, 0, 0); return d.getTime(); };
export const addDays = (t, n) => { const d = new Date(t); d.setDate(d.getDate() + n); return d.getTime(); };
export function bucketStart(t, step) {
  const d = new Date(t);
  if (step >= D) d.setHours(0, 0, 0, 0);
  else { const k = Math.round(step / H); d.setMinutes(0, 0, 0); d.setHours(Math.floor(d.getHours() / k) * k); }
  return d.getTime();
}
export function nextBucket(t, step) {
  if (step >= D) return addDays(t, 1);
  const d = new Date(t); d.setHours(d.getHours() + Math.round(step / H)); return d.getTime();
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
export const PROVS = [['claude', 'Claude Code'], ['codex', 'Codex']];
const PROV_LABEL = Object.fromEntries(PROVS);
export const RANGES = [['24h', '24H'], ['7d', '7D'], ['30d', '30D'], ['month', 'Month'], ['all', 'All']];
const tokens = row => TYPES.every(t => finite(row?.[t.f]) && row[t.f] >= 0);

/** The analytics API range that covers a page range (the backend accepts 24h, 7d and 30d today). */
export function apiRangeFor(state, now = Date.now()) {
  const range = state?.range;
  if (range === '24h' || range === '7d' || range === '30d') return range;
  if (range === 'month') return new Date(now).getDate() <= 1 ? '24h' : dayStartMonth(now) >= now - 7 * D + H ? '7d' : '30d';
  if (range === 'custom' && finite(state.from)) return state.from >= now - D ? '24h' : state.from >= now - 7 * D ? '7d' : '30d';
  return '30d';
}
const dayStartMonth = now => { const d = new Date(now); d.setDate(1); d.setHours(0, 0, 0, 0); return d.getTime(); };

/** Validated activity rows. Malformed, unknown-provider and duplicate rows are dropped, never counted twice. */
export function activityData(payload, now = Date.now()) {
  const act = payload?.activity || {};
  const available = ['ok', 'cached'].includes(act.status) && tokens(act.totals);
  const seen = new Set();
  const hours = [];
  let costMissing = false;
  if (available) {
    for (const row of Array.isArray(act.byHour) ? act.byHour : []) {
      const value = text(row?.hour);
      if (!/^\d{4}-\d{2}-\d{2}T\d{2}:00(?::00)?Z$/.test(value) || !['claude', 'codex'].includes(row.provider) || !tokens(row)) continue;
      const t = Date.parse(value);
      if (!finite(t) || new Date(t).toISOString().slice(0, 13) !== value.slice(0, 13)) continue;
      const key = `${row.provider}|${t}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const cost = finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0 ? row.estimatedCostUsd : null;
      if (cost === null) costMissing = true;
      hours.push({ t, p: row.provider, in: row.inputTokens, out: row.outputTokens, cw: row.cacheCreationTokens, cr: row.cacheReadTokens, cost: cost ?? 0 });
    }
  }
  hours.sort((a, b) => a.t - b.t);
  const models = [];
  const modelKeys = new Set();
  if (available) {
    for (const row of Array.isArray(act.models) ? act.models : []) {
      if (!['claude', 'codex'].includes(row?.provider) || !text(row.model) || !tokens(row)) continue;
      const key = `${row.provider}|${row.model}`;
      if (modelKeys.has(key)) continue;
      modelKeys.add(key);
      const tok = Object.fromEntries(TYPES.map(t => [t.k, row[t.f]]));
      const total = tok.in + tok.out + tok.cw + tok.cr;
      const r = reconcile(row);
      const logged = r.logged;
      const mode = r.reconciled ? 'rates' : 'shares';
      const cost = mode === 'rates' ? r.cost : Object.fromEntries(TYPES.map(t => [t.k, total > 0 && finite(logged) ? tok[t.k] / total * logged : 0]));
      models.push({ model: row.model, provider: row.provider, tok, total, logged, hasCost: finite(logged), cost, mode, rate: r.rate });
    }
  }
  const unreconciled = models.filter(m => m.mode === 'shares' && (m.total > 0 || (m.logged || 0) > 0));
  // a provider's blended per-type rate over the logged window, used to split a shorter range's logged cost
  const blend = {};
  for (const [p] of PROVS) {
    blend[p] = {};
    const list = models.filter(m => m.provider === p);
    for (const t of TYPES) {
      const tk = list.reduce((s, m) => s + m.tok[t.k], 0), c = list.reduce((s, m) => s + m.cost[t.k], 0);
      blend[p][t.k] = tk > 0 ? c / tk : 0;
    }
  }
  const apiFrom = Date.parse(payload?.range?.from);
  const fetched = Date.parse(act.fetchedAt);
  const win0 = Math.min(finite(apiFrom) ? Math.ceil(apiFrom / H) * H : Infinity, hours.length ? hours[0].t : Infinity, finite(apiFrom) ? Infinity : now - 7 * D);
  const win1 = finite(fetched) ? Math.min(fetched, now) : hours.length ? Math.min(now, hours.at(-1).t + H) : now;
  const sessions = (available && Array.isArray(act.providers) ? act.providers : []).filter(p => ['claude', 'codex'].includes(p?.provider)).map(p => ({
    p: p.provider, label: PROV_LABEL[p.provider],
    sessions: Number.isInteger(p.sessionCount) && p.sessionCount >= 0 ? p.sessionCount : null,
    events: Number.isInteger(p.usageEvents) && p.usageEvents >= 0 ? p.usageEvents : null,
    cost: finite(p.totals?.estimatedCostUsd) && p.totals.estimatedCostUsd >= 0 ? p.totals.estimatedCostUsd : null,
  }));
  return { available, status: text(act.status), message: text(act.message), hours, models, unreconciled, blend, win0, win1, sessions, costMissing, apiPreset: text(payload?.range?.preset), apiFrom };
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
const provOK = (state, p) => !state?.prov || state.prov === 'all' || state.prov === p;
const provName = state => !state?.prov || state.prov === 'all' ? 'Claude Code and Codex' : PROV_LABEL[state.prov] || 'Claude Code and Codex';
const rowsIn = (A, R, state) => A.hours.filter(r => provOK(state, r.p) && r.t + H > R.a2 && r.t < R.b);
export function sumRows(rows) {
  const o = { in: 0, out: 0, cw: 0, cr: 0, cost: 0, pc: { claude: 0, codex: 0 }, ptok: { claude: 0, codex: 0 } };
  for (const r of rows) { o.in += r.in; o.out += r.out; o.cw += r.cw; o.cr += r.cr; o.cost += r.cost; o.pc[r.p] += r.cost; o.ptok[r.p] += r.in + r.out + r.cw + r.cr; }
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
  for (const [p] of PROVS) {
    const pr = rows.filter(r => r.p === p);
    if (!pr.length) continue;
    const logged = pr.reduce((s, r) => s + r.cost, 0);
    const w = {}; let ws = 0;
    for (const t of TYPES) { w[t.k] = pr.reduce((s, r) => s + r[t.k], 0) * A.blend[p][t.k]; ws += w[t.k]; }
    const k = ws > 0 ? logged / ws : 0;
    if (Math.abs(k - 1) > 1e-6) exact = false;
    for (const t of TYPES) out[t.k] += w[t.k] * k;
  }
  return { cost: out, exact: exact && !A.unreconciled.length };
}
export function buckets(A, R, state) {
  const list = [], map = new Map();
  if (R.end <= R.a2) return list;
  for (let t = bucketStart(R.a2, R.step), guard = 0; t < R.end && guard < 800; t = nextBucket(t, R.step), guard++) {
    const t1 = nextBucket(t, R.step);
    const lo = Math.max(t, R.a2), hi = Math.min(t1, R.end);
    const b = { t, t1, lo, hi, mid: (lo + hi) / 2, in: 0, out: 0, cw: 0, cr: 0, cost: 0, pc: { claude: 0, codex: 0 }, partial: t < R.a2 || t1 > R.end };
    list.push(b); map.set(t, b);
  }
  for (const r of rowsIn(A, R, state)) {
    const b = map.get(bucketStart(r.t, R.step));
    if (!b) continue;
    for (const t of TYPES) b[t.k] += r[t.k];
    b.cost += r.cost; b.pc[r.p] += r.cost;
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
      .map(([key, label, swatch]) => ({ key, label, swatch, num: 0, has: false, fmt: key === 'tok' || key === 'cache' ? 'tok' : 'money', text: 'Unavailable', tip: '', sub: [run('Local CLI activity unavailable')], apport: false }));
  }
  const days = (R.end - R.a2) / D;
  const cacheCost = C.cost.cw + C.cost.cr;
  const costOk = !A.costMissing;
  const perDay = !costOk ? [] : days >= 0.75 ? [run(money(K.cost / Math.max(1, days)), true), run(' a day · ')] : days > 0 ? [run(money(K.cost / (days * 24)), true), run(' an hour · ')] : [];
  const apport = !C.exact;
  const card = (key, label, swatch, num, fmt, tip, sub) => ({ key, label, swatch, num: finite(num) ? num : 0, has: finite(num), fmt, text: finite(num) ? fmt === 'money' ? money(num) : tokC(num) : 'Unavailable', tip, sub, apport: apport && (key === 'in' || key === 'out') });
  return [
    card('tok', 'Total tokens', '', K.tok, 'tok', tokX(K.tok), [run(`${tokC(K.in)} in · ${tokC(K.out)} out · ${tokC(K.cw + K.cr)} cache`)]),
    card('cost', 'Total cost', 'cost', costOk ? K.cost : null, 'money', 'An estimate at API prices, not a bill', [...perDay, run('API-equivalent estimate')]),
    card('cache', 'Cache tokens', 'cache', K.cw + K.cr, 'tok', tokX(K.cw + K.cr), costOk ? [run(money(cacheCost), true), run(` cache cost · ${share1(K.cost ? cacheCost / K.cost * 100 : 0)}% of cost`)] : [run('Cache cost unavailable')]),
    card('in', 'Input cost', 'in', costOk ? C.cost.in : null, 'money', '', [run(`${tokC(K.in)} uncached input tokens`)]),
    card('out', 'Output cost', 'out', costOk ? C.cost.out : null, 'money', '', [run(`${tokC(K.out)} output tokens, with reasoning`)]),
  ];
}
export const APPORT_TIP = "Model detail is logged only for the whole log window. For a shorter range, each provider's logged cost is split by its token mix at its models' rates, so the parts still add up to the logged total.";

// ---------------------------------------------------------------- usage trends
function xTicks(R, pw) {
  const span = R.b - R.a2;
  const cands = [H, 2 * H, 3 * H, 6 * H, 12 * H, D, 2 * D, 7 * D].filter(s => s >= Math.min(R.step, D));
  const step = cands.find(s => pw / (span / s) >= 74) || 7 * D;
  const out = [];
  let t = step >= D ? addDays(dayStart(R.a2), 1) : nextBucket(bucketStart(R.a2, step), step);
  if (step >= D && dayStart(R.a2) === R.a2) t = R.a2;
  for (let i = 0; t < R.b && i < 400; i++, t = step >= D ? addDays(t, Math.round(step / D)) : nextBucket(t, step)) {
    const midnight = new Date(t).getHours() === 0;
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
  const bucketsOut = B.map(b => {
    const top = inc.reduce((s, t) => s + b[t.k], 0), all = b.in + b.out + b.cw + b.cr;
    const when = R.step >= D ? `${wmdTxt(b.t)}${b.partial ? `, ${clockTxt(b.lo)} to ${clockTxt(b.hi)}` : ''}`
      : `${wmdTxt(b.t)} · ${hourTxt(b.lo)} to ${b.hi === b.t1 ? hourTxt(b.hi) : clockTxt(b.hi)}`;
    const foot = [state.prov === 'all' && costOk ? PROVS.map(([p, l]) => `${l} ${money(b.pc[p])}`).join(' · ') : '', b.t1 > A.win1 ? `Logs read ${clockTxt(A.win1)}; later activity is not in yet` : ''].filter(Boolean);
    return {
      x: pt(X(b.mid)), yTok: pt(Y(top)), yCost: pt(costOk ? YC(b.cost) : ph), time: when.toUpperCase(),
      vin: tokC(b.in), vout: tokC(b.out), vcw: tokC(b.cw), vcr: tokC(b.cr), all: tokC(all), cost: costOk ? money(b.cost) : 'Unavailable',
      tin: tokX(b.in), tout: tokX(b.out), tcw: tokX(b.cw), tcr: tokX(b.cr), tall: tokX(all), crDim: !state.cache,
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
  legend.push({ key: 'cost', label: 'Estimated cost', note: costOk ? 'right axis, USD' : 'unavailable for this range' });
  return {
    w: W, h: Hc, pw: pt(pw), ph: pt(ph),
    geo: { x0, x1, lv, cost },
    sub: `${stepWord(R.step)} buckets · ${provName(state)}${R.clipped ? ` · logs start ${mdTxt(R.a2)}` : ''}`,
    split: !!state.split, cache: !!state.cache, costShown: costOk,
    empty, emptyText: !A.available ? (A.message || 'Local CLI activity is unavailable.') : empty ? `No ${provName(state)} activity was logged in this range.` : '',
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
const modelsShown = (A, state) => A.models.filter(m => provOK(state, m.provider) && (m.total > 0 || (m.logged || 0) > 0)).sort((a, b) => (b.logged || 0) - (a.logged || 0));
const hiddenModels = (A, state) => A.models.filter(m => provOK(state, m.provider) && !(m.total > 0 || (m.logged || 0) > 0));
/** One colour per model in its provider's family: full tone for the largest, lighter steps after it. */
function modelShades(A) {
  const out = {}, n = { claude: 0, codex: 0 };
  const mixes = [1, 0.66, 0.42, 0.24];
  A.models.slice().sort((a, b) => (b.logged || 0) - (a.logged || 0)).forEach(m => { n[m.provider] += 1; out[`${m.provider}|${m.model}`] = mixes[Math.min(3, n[m.provider] - 1)]; });
  return out;
}
function ioStatus(r) {
  if (r >= 200) return 'Extended thinking or large context loading. Expected for reasoning models.';
  if (r >= 50) return 'More input than output. Typical for analysis tasks.';
  if (r >= 5) return 'Balanced input to output ratio for typical coding tasks.';
  return 'More output than input. A generation-heavy workload.';
}
const ioTxt = v => v >= 10 ? nf0.format(v) : nf2v.format(v);
const RATE_SOURCE = { builtin: 'CCS pricing table', 'models-dev': 'models.dev rates, as CCS resolves them', fallback: 'CCS fallback rate for models it does not list' };
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
    const rateTxt = m.rate ? `${money(m.rate.in)} in, ${money(m.rate.out)} out, ${money(m.rate.cw)} cache write, ${money(m.rate.cr)} cache read per million tokens; ${RATE_SOURCE[m.rate.source] || 'CCS rate'}${m.rate.source === 'fallback' ? ', an estimate with a fallback rate' : ''}.` : 'No rate listed; the split shows token shares.';
    return {
      key: `${m.provider}|${m.model}`, name: m.model, provider: m.provider, idx: i,
      w: Math.max(0.4, (m.logged || 0) / max * 100),
      fin: frac('in'), fout: frac('out'), fcw: frac('cw'), fcr: frac('cr'),
      tipIn: `Input: ${money(m.cost.in)} for ${tokX(m.tok.in)}`, tipOut: `Output: ${money(m.cost.out)} for ${tokX(m.tok.out)}`,
      tipCw: `Cache write: ${money(m.cost.cw)} for ${tokX(m.tok.cw)}`, tipCr: `Cache read: ${money(m.cost.cr)} for ${tokX(m.tok.cr)}`,
      tok: tokC(m.total), tokTip: tokX(m.total), cost: m.hasCost ? money(m.logged) : 'Unavailable', share: m.hasCost ? share1((m.logged || 0) / total * 100) : 'Unavailable',
      // the model detail popover
      sub: `${PROV_LABEL[m.provider]} logs · ${windowText}`,
      usage: share1(totTok ? m.total / totTok * 100 : 0),
      types: TYPES.map(t => ({ key: t.k, label: t.long, tok: m.tok[t.k] ? tokC(m.tok[t.k]) : 'None', tip: tokX(m.tok[t.k]), cost: money(m.cost[t.k]), w: m.cost[t.k] > 0 ? Math.max(1.5, m.cost[t.k] / maxType * 100) : 0, none: !m.tok[t.k] })),
      io: io === null ? 'Unavailable' : `${ioTxt(io)} to 1`,
      ioNote: io === null ? 'No output was logged, so there is no ratio.' : ioStatus(io),
      ioQuiet: io === null ? '' : `Uncached input divided by output, as in the original. Counting cache reads and writes as input, it is ${ioTxt(ioCtx)} to 1: ${share2((m.tok.cr + m.tok.cw) / Math.max(1, m.tok.in + m.tok.cr + m.tok.cw) * 100)}% of this model's input side arrived through the cache.`,
      rate: `${rateTxt}${m.mode === 'rates' ? ' The four parts add up to the logged estimate.' : ' These rates do not reconcile with the logged estimate, so the split shows token shares.'}`,
    };
  });
}
function donutView(A, state, shades) {
  const list = modelsShown(A, state);
  const val = m => state.donut === 'cost' ? (m.logged || 0) : m.total;
  const total = list.reduce((s, m) => s + val(m), 0);
  const segs = [], small = [];
  list.slice().sort((a, b) => val(b) - val(a)).forEach(m => { if (total && val(m) / total < 0.01) small.push(m); else segs.push(m); });
  const idxOf = m => list.indexOf(m);
  const seg = m => ({ key: `${m.provider}|${m.model}`, name: m.model, provider: m.provider, mix: shades[`${m.provider}|${m.model}`] ?? 1, other: false, v: val(m), model: `${m.provider}|${m.model}`, tip: '', idx: idxOf(m) });
  const out = segs.map(seg);
  if (small.length === 1) out.push(seg(small[0]));
  else if (small.length) out.push({ key: '_other', name: `${small.length} smaller models`, provider: '', mix: 1, other: true, v: small.reduce((s, m) => s + val(m), 0), model: '', tip: small.map(m => m.model).join(', '), idx: -1 });
  let a = 0;
  const cost = state.donut === 'cost';
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
  return {
    mode: cost ? 'cost' : 'tokens', segs: rows, lut,
    centre: cost ? money(total) : tokC(total), centreLabel: `${cost ? 'estimated cost' : 'tokens'}, ${rows.length > 1 ? 'all models' : '1 model'}`,
    unit: cost ? '' : ' tokens', empty: !rows.length,
  };
}

// ---------------------------------------------------------------- session stats
function sessionsView(A, state) {
  const rows = A.sessions.filter(s => provOK(state, s.p));
  const sum = k => rows.length && rows.every(r => finite(r[k])) ? rows.reduce((s, r) => s + r[k], 0) : null;
  const sessions = sum('sessions'), events = sum('events'), cost = sum('cost');
  const avg = finite(sessions) && sessions > 0 && finite(cost) ? cost / sessions : null;
  const evs = finite(sessions) && sessions > 0 && finite(events) ? events / sessions : null;
  return {
    stats: [
      { key: 'sess', label: 'Sessions', num: sessions ?? 0, has: finite(sessions), fmt: 'int', text: intText(sessions) },
      { key: 'avg', label: 'Average estimated cost per session', num: avg ?? 0, has: finite(avg), fmt: 'money', text: money(avg) },
      { key: 'evs', label: 'Usage events per session', num: evs ?? 0, has: finite(evs), fmt: 'int', text: intText(evs) },
    ],
    rows: rows.map(r => ({
      provider: r.p, label: r.label,
      sessions: finite(r.sessions) ? nf0.format(r.sessions) : 'Unavailable',
      per: finite(r.sessions) && r.sessions > 0 && finite(r.cost) ? money(r.cost / r.sessions) : 'Unavailable',
      events: finite(r.sessions) && r.sessions > 0 && finite(r.events) ? nf0.format(Math.round(r.events / r.sessions)) : 'Unavailable',
      eventsTip: finite(r.events) ? `${nf0.format(r.events)} usage events` : '',
    })),
  };
}

// ---------------------------------------------------------------- token breakdown and cache efficiency
function tokensView(K, C, costOk, available = true) {
  if (!available) return TYPES.map(t => ({ key: t.k, label: t.label, wt: 0, wc: 0, tok: 'Unavailable', tokTip: '', tokShare: '', cost: 'Unavailable', costShare: '' }));
  return TYPES.map(t => {
    const ts = K.tok ? K[t.k] / K.tok * 100 : 0, cs = costOk && K.cost ? C.cost[t.k] / K.cost * 100 : 0;
    return { key: t.k, label: t.label, wt: ts > 0 ? Math.max(0.6, ts) : 0, wc: cs > 0 ? Math.max(0.6, cs) : 0, tok: tokC(K[t.k]), tokTip: tokX(K[t.k]), tokShare: share1(ts), cost: costOk ? money(C.cost[t.k]) : 'Unavailable', costShare: costOk ? share1(cs) : '' };
  });
}
function cacheView(A, rows, K, C, costOk) {
  const hit = K.cr + K.cw > 0 ? K.cr / (K.cr + K.cw) * 100 : null;
  // savings = cache-read tokens x (input rate - cache-read rate), per provider at its models' blended rates
  let save = 0;
  for (const [p] of PROVS) {
    const crTok = rows.filter(r => r.p === p).reduce((s, r) => s + r.cr, 0);
    const list = A.models.filter(m => m.provider === p && m.rate && m.tok.cr > 0);
    const crAll = list.reduce((s, m) => s + m.tok.cr, 0);
    const perTok = crAll > 0 ? list.reduce((s, m) => s + m.tok.cr * Math.max(0, m.rate.in - m.rate.cr) / 1e6, 0) / crAll : 0;
    save += crTok * perTok;
  }
  const ccost = C.cost.cw + C.cost.cr;
  const tot = K.cr + K.cw;
  const sh = v => tot > 0 ? share2(v / tot * 100) : 'Unavailable';
  return {
    hit: hit ?? 0, hasHit: finite(hit), hitText: finite(hit) ? share2(hit) : 'Unavailable',
    save: costOk ? save : 0, hasSave: costOk, saveText: costOk ? money(save) : 'Unavailable',
    ccost: costOk ? ccost : 0, hasCcost: costOk, ccostText: costOk ? money(ccost) : 'Unavailable',
    ccostShare: costOk ? share1(K.cost ? ccost / K.cost * 100 : 0) : '',
    reads: { tok: tokC(K.cr), tip: tokX(K.cr), share: sh(K.cr), cost: costOk ? money(C.cost.cr) : 'Unavailable', w: K.cr },
    writes: { tok: tokC(K.cw), tip: tokX(K.cw), share: sh(K.cw), cost: costOk ? money(C.cost.cw) : 'Unavailable', w: K.cw, note: tot && K.cw === 0 ? 'none logged' : '' },
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
function heatView(A, R, state, width, height) {
  const cells = Array.from({ length: 7 }, () => Array.from({ length: 24 }, () => ({ v: null, n: 0, tok: 0, cost: 0 })));
  // hours inside the read window count (zero when nothing ran); hours outside it stay empty, never zero;
  // without readable activity every hour stays empty
  for (let t = Math.ceil(R.a2 / H) * H; A.available && t < R.end; t += H) { const d = new Date(t), c = cells[(d.getDay() + 6) % 7][d.getHours()]; if (c.v === null) c.v = 0; c.n++; }
  for (const r of rowsIn(A, R, state)) { const d = new Date(r.t), c = cells[(d.getDay() + 6) % 7][d.getHours()]; if (c.v === null) c.v = 0; c.tok += r.in + r.out + r.cw; c.cost += r.cost; }
  const byCost = state.heat !== 'tokens' && !A.costMissing;
  for (const row of cells) for (const c of row) if (c.v !== null) c.v = byCost ? c.cost : c.tok;
  const max = Math.max(0, ...cells.flat().filter(c => c.v !== null).map(c => c.v));
  const valTxt = v => byCost ? money(v) : `${tokC(v)} tokens`;
  const hourLab = h => hourTxt(new Date(2026, 0, 1, h).getTime());
  const wide = (width || DEFAULT_SIZES.heat.w) > 760;
  const out = [];
  cells.forEach((row, i) => row.forEach((c, h) => {
    const name = `${WD[i]} ${hourLab(h)}`;
    if (c.v === null) { out.push({ state: 0, k: 0, tip: `${name} · outside the logs in this range` }); return; }
    out.push({ state: c.v === 0 ? 1 : 2, k: max > 0 ? Math.sqrt(c.v / max) : 0, tip: `${name} · ${c.v ? valTxt(c.v) : 'nothing logged'}${c.n > 1 ? ` · ${c.n} hours in range` : ''}` });
  }));
  return {
    mode: byCost ? 'cost' : 'tokens',
    sub: `${byCost ? 'Estimated cost' : 'Tokens without cache reads'} per hour, local time · ${dateLabel(R)}`,
    cells: out, days: WD,
    hours: Array.from({ length: 24 }, (_, h) => h % (wide ? 3 : 6) === 0 ? hourLab(h) : ''),
    busiest: max ? valTxt(max) : 'none',
    dash: dashedRect(Math.max(4, ((width || DEFAULT_SIZES.heat.w) - 38 - 72) / 24), height || 24),
  };
}

// ---------------------------------------------------------------- daily cost by provider
function dailyView(A, R, state, size) {
  const step = R.end - R.a2 <= 2 * D + H ? H : D;
  const B = A.available ? buckets(A, { ...R, step }, state) : [];
  const W = Math.max(200, size?.w || DEFAULT_SIZES.daily.w), Hc = Math.max(120, size?.h || DEFAULT_SIZES.daily.h);
  const pad = DAILY_PAD;
  const pw = W - pad.l - pad.r;
  const costOk = !A.costMissing;
  const val = (b, p) => provOK(state, p) ? b.pc[p] : 0;
  const max = costOk ? Math.max(0, ...B.map(b => val(b, 'claude') + val(b, 'codex'))) : 0;
  const sc = [3, 4, 5].map(k => niceScale(max * 1.04, k)).sort((a, b) => a.top - b.top || a.k - b.k)[0];
  const n = Math.max(1, B.length), slot = pw / n;
  const every = Math.max(1, Math.ceil(64 / slot));
  const yTicks = [];
  for (let i = 0; i <= sc.k; i++) yTicks.push({ y: i / sc.k, label: costOk ? moneyAxis(sc.step * i, sc.step) : '', base: i === 0 });
  const bars = B.map((b, i) => {
    const cl = val(b, 'claude'), cx = val(b, 'codex'), tot = cl + cx;
    return {
      h: costOk && sc.top > 0 ? tot / sc.top : 0, cl: tot > 0 ? cl / tot : 0,
      label: i % every ? '' : step === H ? (new Date(b.t).getHours() === 0 ? wdTxt(b.t) : hourTxt(b.t)) : mdTxt(b.t),
      time: `${step === H ? `${wmdTxt(b.t)} · ${hourTxt(b.t)}` : wmdTxt(b.t)}${b.partial ? ` · ${clockTxt(b.lo)} to ${clockTxt(b.hi)}` : ''}`.toUpperCase(),
      claude: provOK(state, 'claude') ? money(cl) : '', codex: provOK(state, 'codex') ? money(cx) : '',
      total: state.prov === 'all' || !state.prov ? money(tot) : '',
      foot: b.t1 > A.win1 ? `So far: logs read ${clockTxt(A.win1)}` : b.t < R.a2 ? `Logs start ${clockTxt(R.a2)}` : '',
    };
  });
  const empty = !B.length || max === 0;
  return {
    title: step === H ? 'Hourly cost by provider' : 'Daily cost by provider',
    sub: `Estimated, USD · ${dateLabel(R)}`,
    yTicks, bars, empty, emptyText: !A.available ? (A.message || 'Local CLI activity is unavailable.') : empty ? (costOk ? `No ${provName(state)} cost was logged in this range.` : 'Cost is unavailable for this range.') : '',
    showClaude: provOK(state, 'claude'), showCodex: provOK(state, 'codex'),
  };
}

// ---------------------------------------------------------------- custom range picker (whole local days)
export function calendarView(A, R, now, apiAll) {
  // the backend keeps up to 30 days of logs; the 30-day window bounds the picker until it reports availableFrom
  const first = dayStart(Math.max(apiAll ?? now - 30 * D, now - 30 * D)), last = dayStart(now);
  const mon = t => addDays(t, -((new Date(t).getDay() + 6) % 7));
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

// ---------------------------------------------------------------- the whole Usage part of the page
/**
 * state: { range: 24h|7d|30d|month|all|custom, from, to (local-day ms, custom only), prov: all|claude|codex,
 * split, cache, donut: tokens|cost, heat: cost|tokens }. opts: { now, sizes }.
 */
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
  const shades = modelShades(A);
  const hid = hiddenModels(A, state);
  const foot = [
    hid.length ? `${hid.length === 1 ? hid[0].model : `${hid.length} entries`} ${hid.length === 1 ? 'is' : 'are'} left out: no tokens or cost logged.` : '',
    A.unreconciled.length ? `${A.unreconciled.map(m => m.model).join(', ')}: rates do not reconcile with the logged cost, so the split shows token shares.` : '',
  ].filter(Boolean).join(' ');
  const statusNote = A.available ? '' : A.message || 'Local CLI activity is unavailable.';
  return {
    available: A.available, statusNote, cached: A.status === 'cached',
    head: {
      scope: `Ubuntu Claude Code and Codex logs · local time${zone ? ` (${zone})` : ''} · read `,
      read: relTxt(A.win1, now),
      readTip: `Logs last read ${timeTxt(A.win1)}.${A.status === 'cached' ? ' The log scan is refreshing; earlier records are shown until it completes.' : ''}`,
      date: dateLabel(R), custom: state.range === 'custom', range: state.range, prov: state.prov || 'all',
    },
    scope: [
      { icon: 'terminal', text: 'Only Ubuntu Claude Code and Codex logs. Other providers and other machines are not in these numbers.' },
      { icon: 'wallet', text: 'Cost is an estimated API equivalent at the per-token rates CCS prices each model at. It is not a bill or a subscription charge.' },
      { icon: 'users', text: 'Activity covers all accounts together; it cannot be attributed to one account.' },
      { icon: 'layers', text: A.unreconciled.length ? `${A.unreconciled.length} model${A.unreconciled.length === 1 ? '' : 's'} do not reconcile with these rates; their split uses token shares.` : "Per-type costs use each model's rate as CCS prices it and match every model's logged estimate to the cent." },
      { icon: 'clock', text: `Logs read ${timeTxt(A.win1)}. Hours are shown in local time${zone ? ` (${zone})` : ''}.${A.status === 'cached' ? ' The scan is refreshing; earlier records are shown until it completes.' : ''}` },
    ],
    kpis: kpis(A, R, state, rows, K, C),
    apportTip: APPORT_TIP,
    trend: trendView(A, R, state, opts.sizes?.trend),
    cbm: { sub: `${windowText} · select a model for detail`, note: wholeNote('models'), rows: modelRows(A, state, windowText), foot, empty: A.available ? `No model activity for ${provName(state)} in the logs.` : statusNote },
    donut: { sub: `Share of the logs read for ${windowText}`, note: wholeNote('models'), ...donutView(A, state, shades) },
    sessions: { sub: `Logs read for ${windowText}`, note: wholeNote('sessions'), ...sessionsView(A, state) },
    tokens: { sub: `${dateLabel(R)} · ${provName(state)}`, rows: tokensView(K, C, costOk, A.available) },
    cache: { sub: `${dateLabel(R)} · ${provName(state)}`, ...cacheView(A, rows, K, C, costOk && A.available) },
    heat: heatView(A, R, state, opts.sizes?.heat?.w, opts.sizes?.heat?.h),
    daily: dailyView(A, R, state, opts.sizes?.daily),
    calendar: calendarView(A, R, now),
    range: R,
  };
}
