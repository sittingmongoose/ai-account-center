// Analytics, quota blocks (version 3): the compact quota history (the former Headroom), each row's focus chart
// and the "Upcoming resets and expiries" agenda, built like the approved concept (c-daylight-atlas/
// app-analytics.js). Pure functions; the focus chart's lines, markers and labels are laid out here in pixels
// (labels never overlap: each is placed in the first free spot or dropped) and drawn by Slint as given.
//
// Truthfulness: quota observations are snapshots per account and window, never added, averaged or merged
// across accounts; a missing reading is a gap, never zero; a reset is never bridged by a line; projections are
// labelled as projections; at most two decimals.
import { visibleUsageWindows } from './visible-usage.mjs';
import { usedPercent, currentUsedPercent, isFable, isMeterWindow, period, windowLabel, planLabel, valueText, hiddenProviders } from './view-model.mjs';
import { H, D, clockTxt, hourTxt, mdTxt, wmdTxt, wdTxt, timeTxt, duration, untilTxt, dayStart, addDays, bucketStart, nextBucket, dashLine, dashPolyline } from './analytics-usage.mjs';

const finite = value => typeof value === 'number' && Number.isFinite(value);
const text = value => typeof value === 'string' ? value : '';
const validDate = value => typeof value === 'string' && Number.isFinite(Date.parse(value));
const pt = value => Math.round(value * 10) / 10;
const clamp = (value, low, high) => Math.max(low, Math.min(high, value));
const nf2 = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const money = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 2 });
const run = (value, strong = false, tone = '') => ({ text: String(value), strong: !!strong, tone });

export const QUOTA_PROVIDERS = [
  ['claude', 'Claude'], ['codex', 'Codex'], ['antigravity', 'Google Antigravity CLI'], ['cursor', 'Cursor'],
  ['muse', 'Muse Code'], ['kimi-code', 'Kimi Code'], ['qwen', 'Qwen Token Plan'], ['zai', 'Z.ai Coding Plan'],
  ['opencode-go', 'OpenCode Go'],
];
const SWITCHABLE = ['codex', 'antigravity'];
export const sev = v => !finite(v) ? 'na' : v > 100 ? 'over' : v >= 95 ? 'crit' : v >= 80 ? 'warn' : 'calm';
const SEV_INT = { na: -1, calm: 0, warn: 1, crit: 2, over: 3 };
const unitAmount = (value, unit) => {
  if (!finite(value)) return '';
  if (unit === 'USD') return money.format(value);
  const u = unit ? (value === 1 ? unit.replace(/s$/, '') : unit) : '';
  return nf2.format(value) + (u ? ` ${u}` : '');
};

// ---------------------------------------------------------------- accounts
const MAIN_WINDOW = { codex: 'seven_day', claude: 'seven_day', antigravity: 'gemini-weekly' };
/** The window a quota-history row summarises: the canonical weekly window, else the first weekly rate limit,
 *  else the first rate limit with a reading, else the first rate limit; never Fable, never an amount. */
export function mainWindow(account) {
  const windows = (Array.isArray(account?.windows) ? account.windows : []).filter(window => window && !['balance', 'extra_usage', 'spend'].includes(window.kind) && !/fable/i.test(`${text(window.key)} ${text(window.label)}`));
  return windows.find(window => window.key === MAIN_WINDOW[account?.provider])
    || windows.find(window => period(window) === 'week' && usedPercent(window) !== null)
    || windows.find(window => usedPercent(window) !== null)
    || windows[0] || null;
}
/**
 * One merged history per account and window key, from every analytics series with that key (a renamed label is
 * the same window): { t, v, reset, active }. Unavailable samples stay null; duplicates keep the real reading.
 */
export function historyOf(analyticsAccount, key) {
  const map = new Map();
  for (const window of Array.isArray(analyticsAccount?.windows) ? analyticsAccount.windows : []) {
    if (window?.key !== key) continue;
    for (const point of Array.isArray(window.points) ? window.points : []) {
      const t = Date.parse(point?.sampledAt);
      if (!finite(t)) continue;
      const ok = ['ok', 'cached'].includes(point.status);
      const v = ok ? usedPercent(point) : null;
      const rec = { t, v, reset: validDate(point.resetAt) ? Date.parse(point.resetAt) : null, active: point.isActive === true };
      const cur = map.get(t);
      if (!cur) map.set(t, rec);
      else { if (cur.v === null && v !== null) { cur.v = v; cur.reset = rec.reset; } cur.active = cur.active || rec.active; }
    }
  }
  return [...map.values()].sort((a, b) => a.t - b.t);
}
/** The quota accounts: the current dashboard readings (when given) joined to their analytics history by id. */
export function quotaAccounts(payload, ctx = {}) {
  const hidden = ctx.hidden instanceof Set ? ctx.hidden : hiddenProviders(ctx.dashboard);
  const history = new Map((Array.isArray(payload?.accounts) ? payload.accounts : []).filter(a => text(a?.id)).map(a => [a.id, a]));
  const current = Array.isArray(ctx.dashboard?.accounts) ? ctx.dashboard.accounts.filter(a => text(a?.id)) : [...history.values()];
  const known = new Set(QUOTA_PROVIDERS.map(([id]) => id));
  const now = Number.isFinite(ctx.now) ? ctx.now : Date.now();
  // Accounts hidden from the dashboard one by one leave Analytics as they leave Home (`accounts[].hidden`,
  // `settings.hiddenAccountIds`). The trays' own switch (`trayHidden`) is never read here.
  const hiddenIds = new Set(Array.isArray(ctx.dashboard?.settings?.hiddenAccountIds) ? ctx.dashboard.settings.hiddenAccountIds : []);
  return current.filter(a => known.has(a.provider) && !hidden.has(a.provider) && a.hidden !== true && !hiddenIds.has(a.id)).map(a => {
    const windows = visibleUsageWindows(a.provider, a.windows).map(w => ({
      key: text(w.key), label: text(w.label), short: windowLabel(a.provider, w), period: period(w), fable: isFable(w),
      // a reading from before a reset that has passed is no current reading (F6)
      used: isMeterWindow(w) && w.unlimited !== true ? currentUsedPercent(a, w, now) : null, meter: isMeterWindow(w),
      resetAt: validDate(w.resetAt) ? Date.parse(w.resetAt) : null, expiresAt: validDate(w.expiresAt) ? Date.parse(w.expiresAt) : null,
      planExpiry: !!w.planExpiry, kind: text(w.kind) || 'rate_limit', unit: text(w.unit) || null, remaining: finite(w.remaining) ? w.remaining : null,
      raw: w,
    }));
    return {
      id: a.id, provider: a.provider, email: text(a.email) || text(a.label) || 'Identity unavailable', label: text(a.label), plan: text(a.plan),
      active: SWITCHABLE.includes(a.provider) && a.isActive === true, windows, analytics: history.get(a.id) || null,
    };
  });
}
const meters = acc => acc.windows.filter(w => w.meter);
const mainOf = acc => {
  const raw = mainWindow({ provider: acc.provider, windows: acc.windows.map(w => w.raw) });
  return raw ? acc.windows.find(w => w.raw === raw) || null : null;
};
const histStart = (payload, now) => Date.parse(payload?.history?.oldestSampleAt || payload?.history?.collectedSince) || now - D;

/** Where the recent pace takes a window by its reset (only from readings of the same reset period). */
export function project(acc, w, now) {
  if (!w || !finite(w.used)) return { kind: 'na', rank: 0, text: 'No current reading' };
  if (w.used >= 100) return { kind: 'limit', rank: 5, text: w.resetAt ? `At the limit until ${timeTxt(w.resetAt)}` : 'At the limit, no reset reported' };
  if (!w.resetAt) return { kind: 'noreset', rank: 1, text: 'No reset reported' };
  const h = historyOf(acc.analytics, w.key).filter(p => p.v !== null && p.reset && Math.abs(p.reset - w.resetAt) < 10 * 60e3);
  const span = h.length ? h.at(-1).t - h[0].t : 0;
  if (h.length < 3 || span < 2 * H) return { kind: 'thin', rank: 2, text: 'Too little data to project' };
  const rate = (h.at(-1).v - h[0].v) / (span / H);
  const hoursLeft = (w.resetAt - now) / H;
  if (rate <= 0.005) return { kind: 'flat', rank: 3, text: `About ${Math.round(100 - w.used)}% left at reset`, rate: 0 };
  const toFull = (100 - w.used) / rate;
  if (toFull < hoursLeft) { const before = (hoursLeft - toFull) * H; return { kind: 'runout', rank: 4, text: `Runs out about ${duration(before)} before reset`, rate, at: now + toFull * H, before }; }
  const left = 100 - (w.used + rate * hoursLeft);
  return { kind: 'ok', rank: 3, text: `About ${Math.round(left)}% left at reset`, rate, left };
}

// ---------------------------------------------------------------- sparklines and the shared axis
/** Step runs of a window's history in a 600 x 24 viewbox, coloured by each run's own value; gaps stay gaps. */
export function sparkRuns(acc, w, t0, now) {
  const empty = { calmLine: '', warnLine: '', critLine: '', overLine: '', calmFill: '', warnFill: '', critFill: '', overFill: '', limit: '', points: 0 };
  if (!w) return { ...empty, na: 'No usage window reported' };
  const pts = historyOf(acc.analytics, w.key).filter(p => p.t >= t0);
  if (!pts.length) return { ...empty, na: 'No history yet' };
  const Wv = 600, Hv = 24;
  const x = t => (t - t0) / Math.max(1, now - t0) * Wv;
  const ymax = Math.max(100, ...pts.map(p => p.v || 0), finite(w.used) ? w.used : 0);
  const y = v => Hv - 2 - v / ymax * (Hv - 4);
  const runs = [];
  let prev = null;
  const close = x1 => { const r = runs.at(-1); if (r && r.x1 === undefined) r.x1 = x1; };
  for (const p of pts) {
    if (p.v === null) { if (prev) close(x(p.t)); prev = null; continue; }
    const brk = prev && prev.reset && p.reset && Math.abs(prev.reset - p.reset) > 10 * 60e3 && prev.reset <= p.t;
    if (brk) { close(x(prev.reset)); runs.push({ x0: x(prev.reset), v: p.v, from: null }); }
    else if (!prev) runs.push({ x0: x(p.t), v: p.v, from: null });
    else { close(x(p.t)); runs.push({ x0: x(p.t), v: p.v, from: prev.v }); }
    prev = p;
  }
  const cur = finite(w.used) ? w.used : prev ? prev.v : null;
  // the live reading closes the line at "now" (a refreshed value steps there; history itself is never altered)
  if (prev && finite(cur) && Math.abs(cur - prev.v) > 0.005) { close(Wv - 8); runs.push({ x0: Wv - 8, v: cur, from: prev.v, x1: Wv }); }
  else if (prev) close(Wv);
  const out = { ...empty };
  for (const r of runs) {
    if (r.x1 === undefined) r.x1 = r.x0;
    const s = sev(r.v);
    if (s === 'na') continue;
    const yv = pt(y(r.v)), x0 = pt(r.x0), x1 = pt(r.x1);
    out[`${s}Line`] += `M${x0} ${finite(r.from) ? pt(y(r.from)) : yv}V${yv}H${x1}`;
    if (r.x1 > r.x0) out[`${s}Fill`] += `M${x0} ${Hv}V${yv}H${x1}V${Hv}Z`;
  }
  out.limit = dashLine(0, pt(y(100)), Wv, pt(y(100)), 2, 3);
  out.points = pts.filter(p => p.v !== null).length;
  out.na = '';
  return out;
}
export function quotaAxis(payload, now) {
  const t0 = Math.max(now - 7 * D, histStart(payload, now));
  const span = now - t0;
  const step = [H, 2 * H, 3 * H, 6 * H, 12 * H, D].find(s => span / s <= 8) || D;
  const ticks = [];
  for (let t = nextBucket(bucketStart(t0, step), step); t < now - Math.max(step * 0.3, span * 0.16) && ticks.length < 20; t = nextBucket(t, step)) {
    ticks.push({ pos: (t - t0) / span, label: new Date(t).getHours() === 0 ? wdTxt(t) : hourTxt(t) });
  }
  return { t0, ticks };
}

// ---------------------------------------------------------------- the focus chart (an expanded quota row)
export const FOCUS_PAD = { l: 50, r: 18, t: 28, b: 30 };
// Martian Mono advances 0.65 em: chart labels at 11 px are 7.15 px a character, so label boxes are exact.
export const MONO_ADVANCE = 7.15;
const DASHES = [[], [9, 4], [9, 3, 2, 3]];
function focusWindows(acc) {
  const ms = meters(acc);
  if (acc.provider === 'antigravity') return ms;
  const order = { '5h': 0, week: 1, month: 2, other: 3 };
  return ms.slice().sort((a, b) => order[a.period] - order[b.period] || a.fable - b.fable).slice(0, 3);
}
const tagOf = acc => acc.email.split('@')[0];
/**
 * The focus chart of one account in pixels: shapes (paths with a stroke or fill key), placed labels, dots,
 * legend and the hover stops (nearest stop per 4 px column in `lut`). ctx: { now, width, height, compare,
 * thresholds: { codex, antigravity } (% used or null), accounts (the quota accounts) }.
 */
export function focusChart(acc, payload, ctx) {
  const now = ctx.now;
  const W = Math.max(320, ctx.width || 1200), Hc = Math.max(200, ctx.height || 270);
  const pad = FOCUS_PAD;
  const plotW = W - pad.l - pad.r, plotH = Hc - pad.t - pad.b;
  const obsW = Math.round(plotW * 0.7), gap = 18;
  const obsEnd = pad.l + obsW, fStart = pad.l + obsW + gap;
  const t0 = Math.max(now - 7 * D, histStart(payload, now) - 20 * 60e3), t1 = now;
  const peers = (ctx.accounts || []).filter(a => a.provider === acc.provider);
  const comparable = peers.length > 1;
  const cmp = !!ctx.compare && comparable;
  const prim = mainOf(acc);
  const shadeOf = i => Math.min(2, i);
  let series;
  if (cmp && prim) {
    series = peers.map((a, i) => {
      const w = a.windows.find(x => x.key === prim.key && x.meter) || meters(a).find(x => !x.fable && x.period === prim.period);
      return w ? { acc: a, w, shade: shadeOf(i), dash: i % 3, width: a.id === acc.id ? 2.4 : 1.9, name: tagOf(a) + (a.active ? ' (active)' : ''), tag: tagOf(a) } : null;
    }).filter(Boolean);
  } else {
    series = focusWindows(acc).map((w, i) => ({ acc, w, shade: shadeOf(i), dash: i % 3, width: w === prim ? 2.4 : 1.9, name: w.raw?.label ? fullName(acc.provider, w) : w.short, tag: w.short }));
    series.sort((a, b) => (a.w === prim) - (b.w === prim));
  }
  const base = { id: acc.id, provider: acc.provider, w: W, h: Hc, comparable, compare: cmp, cmpLabel: comparable && prim ? `Compare ${prim.short.toLowerCase()} across ${QUOTA_PROVIDERS.find(p => p[0] === acc.provider)?.[1] || acc.provider} accounts` : '' };
  if (!series.length) return { ...base, empty: 'No usage windows to chart.', shapes: [], labels: [], dots: [], legend: [], stops: [], lut: [], foot: '' };
  const resetMax = Math.max(now + 6 * H, ...series.map(s => s.w.resetAt || 0));
  const tf0 = now, tf1 = Math.min(resetMax, now + 32 * D);
  const xo = t => pad.l + ((t - t0) / (t1 - t0)) * obsW;
  const xf = t => fStart + ((t - tf0) / (tf1 - tf0)) * (plotW - obsW - gap);
  const obsV = series.flatMap(s => historyOf(s.acc.analytics, s.w.key).filter(p => p.t >= t0).map(p => p.v || 0)).concat(series.map(s => s.w.used || 0));
  const ymax = Math.max(100, ...obsV);
  const thr = SWITCHABLE.includes(acc.provider) && finite(ctx.thresholds?.[acc.provider]) ? ctx.thresholds[acc.provider] : null;
  const Y = v => pad.t + plotH - (v / ymax) * plotH;
  const ticks = [0, 25, 50, 75, 100].filter(v => v <= ymax);

  const shapes = [];
  const shape = (d, stroke, extra = {}) => { if (d) shapes.push({ d, stroke, fill: '', sw: 1, op: 1, shade: 0, round: false, layer: 0, ...extra }); };
  const fillShape = (d, fill, extra = {}) => { if (d) shapes.push({ d, stroke: '', fill, sw: 0, op: 1, shade: 0, round: false, layer: 0, ...extra }); };
  const labels = [], dots = [];
  // ---- label placement: placed label boxes, data-line obstacles, regions ----
  const TW = s => s.length * MONO_ADVANCE;
  const placed = [], obst = [];
  const seg = (x0, y0, x1, y1) => obst.push({ x0: Math.min(x0, x1) - 1.5, x1: Math.max(x0, x1) + 1.5, y0: Math.min(y0, y1) - 1.5, y1: Math.max(y0, y1) + 1.5 });
  const diag = (x0, y0, x1, y1) => { const n = Math.max(1, Math.ceil(Math.hypot(x1 - x0, y1 - y0) / 3)); for (let k = 0; k < n; k++) seg(x0 + (x1 - x0) * k / n, y0 + (y1 - y0) * k / n, x0 + (x1 - x0) * (k + 1) / n, y0 + (y1 - y0) * (k + 1) / n); };
  const lbox = (x, y, w, a) => { const x0 = a === 'end' ? x - w : a === 'middle' ? x - w / 2 : x; return { x0: x0 - 3, x1: x0 + w + 3, y0: y - 10, y1: y + 3 }; };
  const hits = (b, list) => list.some(o => b.x0 < o.x1 && b.x1 > o.x0 && b.y0 < o.y1 && b.y1 > o.y0);
  const inside = (b, r) => b.x0 >= r.x0 - 0.5 && b.x1 <= r.x1 + 0.5 && b.y0 >= r.y0 - 0.5 && b.y1 <= r.y1 + 0.5;
  const PLOT = { x0: pad.l + 1, x1: pad.l + plotW - 1, y0: pad.t + 1, y1: pad.t + plotH - 1 };
  const OBS = { ...PLOT, x1: obsEnd - 2 }, FC = { ...PLOT, x0: fStart + 1 };
  const TOP = { x0: 0, x1: W - 2, y0: 0, y1: pad.t - 2 }, BOT = { x0: 0, x1: W - 2, y0: pad.t + plotH + 2, y1: Hc };
  // first candidate clear of labels and data lines, else one clear of labels only; none: the label is dropped
  function put(txt, cands, color, region, extra = {}) {
    const w = TW(txt), plate = region !== TOP && region !== BOT && !extra.noPlate;
    const bs = cands.map(c => ({ c, b: lbox(c.x, c.y, w, c.a) })).filter(o => inside(o.b, region));
    const pick = bs.find(o => !hits(o.b, placed) && !hits(o.b, obst)) || bs.find(o => !hits(o.b, placed));
    if (!pick) return null;
    placed.push(pick.b);
    const x0 = pick.c.a === 'end' ? pick.c.x - w : pick.c.a === 'middle' ? pick.c.x - w / 2 : pick.c.x;
    labels.push({ x: pt(x0), y: pt(pick.c.y), w: pt(w), text: txt, color: color || 'ink-3', shade: extra.shade ?? 0, plate, px: pt(pick.b.x0), py: pt(pick.b.y0), pw: pt(pick.b.x1 - pick.b.x0), ph: pt(pick.b.y1 - pick.b.y0) });
    return pick.b;
  }

  fillShape(`M${pt(fStart)} ${pad.t}H${pt(pad.l + plotW)}V${pt(pad.t + plotH)}H${pt(fStart)}Z`, 'paper-2');
  let grid = '', base0 = '';
  for (const v of ticks) {
    const y = pt(Y(v));
    if (v === 0) base0 += `M${pad.l} ${y}H${pt(pad.l + plotW)}`;
    else if (v === 100) grid += dashLine(pad.l, y, pad.l + plotW, y, 4, 3);
    else grid += `M${pad.l} ${y}H${pt(pad.l + plotW)}`;
    put(`${v}%`, [{ x: pad.l - 8, y: Y(v) + 4, a: 'end' }], 'ink-3', { x0: 0, x1: pad.l, y0: 0, y1: Hc }, { noPlate: true });
  }
  shape(`M${pad.l} ${pt(Y(80))}H${pt(pad.l + plotW)}`, 'warn', { op: 0.45 });
  shape(`M${pad.l} ${pt(Y(95))}H${pt(pad.l + plotW)}`, 'crit', { op: 0.45 });
  const hA = historyOf(acc.analytics, (prim || series[0].w).key);
  let start = null;
  const bands = [];
  hA.forEach((p, i) => {
    if (p.t < t0) return;
    if (p.active && start === null) start = p.t;
    const nextT = i + 1 < hA.length ? hA[i + 1].t : now;
    if (p.active && (i + 1 >= hA.length || !hA[i + 1].active)) { bands.push([start, nextT]); start = null; }
  });
  let bandD = '';
  for (const [a, b] of bands) bandD += `M${pt(xo(a))} ${pad.t}H${pt(xo(a) + Math.max(2, xo(Math.min(b, now)) - xo(a)))}V${pt(pad.t + plotH)}H${pt(xo(a))}Z`;
  fillShape(bandD, 'accent', { op: 0.08 });
  // x axis: hour ticks in the observed part, day ticks in the forecast; a tick label that would touch its neighbour is dropped
  const span = t1 - t0;
  const stepH = span > 5 * D ? 24 : span > 2 * D ? 12 : span > 20 * H ? 4 : span > 8 * H ? 2 : 1;
  const first = new Date(t0); first.setMinutes(0, 0, 0); first.setHours(first.getHours() + 1);
  const bx = obsEnd + gap / 2;
  placed.push({ x0: bx - 7, x1: bx + 7, y0: pad.t + plotH - 5, y1: pad.t + plotH + 8 });
  for (let t = first.getTime(); t < t1; t += H) {
    if (new Date(t).getHours() % stepH) continue;
    const x = xo(t);
    if (x < pad.l + 30 || x > obsEnd - 30) continue;
    grid += `M${pt(x)} ${pad.t}V${pt(pad.t + plotH)}`;
    put(stepH >= 24 ? mdTxt(t) : hourTxt(t), [{ x, y: Hc - 10, a: 'middle' }], 'ink-3', BOT);
  }
  const fd = new Date(now); fd.setHours(24, 0, 0, 0);
  const fSpanD = (tf1 - tf0) / D, fStep = fSpanD > 14 ? 7 : fSpanD > 6 ? 2 : 1;
  let fgrid = '';
  for (let t = fd.getTime(), k = 0; t < tf1; t = addDays(t, 1), k++) {
    if (k % fStep) continue;
    const x = xf(t);
    if (x > pad.l + plotW - 24 || x < fStart + 24) continue;
    fgrid += dashLine(x, pad.t, x, pad.t + plotH, 2, 3);
    put(mdTxt(t), [{ x, y: Hc - 10, a: 'middle' }], 'ink-3', BOT);
  }
  shape(grid + fgrid, 'grid');
  shape(base0, 'rule-strong');
  let marksD = '';
  const fg = [];
  fg.push({ d: `M${pt(bx - 5)} ${pt(pad.t + plotH + 6)}l4 -10M${pt(bx + 1)} ${pt(pad.t + plotH + 6)}l4 -10`, stroke: 'ink-3', sw: 1.25 });
  fg.push({ d: `M${pt(obsEnd)} ${pad.t - 6}V${pt(pad.t + plotH)}`, stroke: 'ink-2', sw: 1 });
  seg(obsEnd, pad.t - 6, obsEnd, pad.t + plotH);
  if (finite(thr)) {
    fg.push({ d: dashLine(pad.l, Y(thr), pad.l + plotW, Y(thr), 6, 4), stroke: 'ink', sw: 1.25, op: 0.7 });
    seg(pad.l, Y(thr), pad.l + plotW, Y(thr));
  }
  const marks = new Set(), ends = [], resetsAt = [], runouts = [];
  const lines = [], legend = [];
  for (const s of series) {
    const h = historyOf(s.acc.analytics, s.w.key).filter(p => p.t >= t0);
    const polys = [];
    let poly = null, prev = null, cx = null, cy = null;
    const hTo = x => { if (cx !== null) seg(cx, cy, x, cy); poly.push([x, cy]); cx = x; };
    const vTo = y => { if (cx !== null) seg(cx, cy, cx, y); poly.push([cx, y]); cy = y; };
    const mTo = (x, y) => { poly = [[x, y]]; polys.push(poly); cx = x; cy = y; };
    for (const p of h) {
      if (p.v === null) { if (prev) hTo(xo(p.t)); prev = null; continue; }
      const brk = prev && prev.reset && p.reset && Math.abs(prev.reset - p.reset) > 10 * 60e3 && prev.reset <= p.t;
      if (brk) {
        const rx = xo(prev.reset);
        hTo(rx);
        const key = Math.round(rx);
        if (!marks.has(key)) { marks.add(key); marksD += dashLine(rx, pad.t, rx, pad.t + plotH, 3, 3); seg(rx, pad.t, rx, pad.t + plotH); resetsAt.push({ x: rx, t: prev.reset }); }
        mTo(rx, Y(p.v)); hTo(xo(p.t));
      } else if (!prev) mTo(xo(p.t), Y(p.v));
      else { hTo(xo(p.t)); vTo(Y(p.v)); }
      prev = p;
    }
    const cur = finite(s.w.used) ? s.w.used : prev ? prev.v : null;
    if (prev && finite(cur)) { hTo(xo(now)); if (cur !== prev.v) vTo(Y(cur)); }
    lines.push({ d: polys.map(pl => dashPolyline(pl, DASHES[s.dash])).join(''), stroke: 'series', shade: s.shade, sw: s.width });
    if (finite(cur)) { dots.push({ x: pt(xo(now)), y: pt(Y(cur)), shade: s.shade, r: 3.5, fill: 'paper' }); obst.push({ x0: xo(now) - 5, x1: xo(now) + 5, y0: Y(cur) - 5, y1: Y(cur) + 5 }); ends.push({ y: Y(cur), s, cur }); }
    const pr = project(s.acc, s.w, now);
    if (s.w.resetAt && finite(cur) && s.w.resetAt <= tf1 + 1) {
      const rxF = xf(s.w.resetAt);
      const key = `f${Math.round(rxF)}`;
      if (!marks.has(key)) { marks.add(key); marksD += dashLine(rxF, pad.t, rxF, pad.t + plotH, 3, 3); seg(rxF, pad.t, rxF, pad.t + plotH); }
      if (pr.kind === 'runout') {
        const xr = xf(pr.at);
        fg.push({ d: dashLine(xf(now), Y(cur), xr, Y(100), 1.5, 4), stroke: 'series', shade: s.shade, sw: 1.75, round: true });
        diag(xf(now), Y(cur), xr, Y(100));
        fg.push({ d: `M${pt(xr - 4)} ${pt(Y(100) - 4)}l8 8m0 -8l-8 8`, stroke: 'crit', sw: 1.75, round: true });
        obst.push({ x0: xr - 6, x1: xr + 6, y0: Y(100) - 6, y1: Y(100) + 6 });
        runouts.push({ x: xr, at: pr.at });
      } else if (pr.kind === 'ok' || pr.kind === 'flat') {
        const end = Math.min(100, cur + (pr.rate || 0) * ((s.w.resetAt - now) / H));
        fg.push({ d: dashLine(xf(now), Y(cur), rxF, Y(end), 1.5, 4), stroke: 'series', shade: s.shade, sw: 1.75, round: true });
        diag(xf(now), Y(cur), rxF, Y(end));
        dots.push({ x: pt(rxF), y: pt(Y(end)), shade: s.shade, r: 3, fill: 'series' });
      } else if (pr.kind === 'limit') {
        fg.push({ d: dashLine(xf(now), Y(cur), rxF, Y(cur), 1.5, 4), stroke: 'crit', sw: 1.75, round: true });
        seg(xf(now), Y(cur), rxF, Y(cur));
      }
    }
    legend.push({ name: s.name, kind: 'series', shade: s.shade, dash: s.dash, width: s.width });
  }
  shape(marksD, 'ink-3');
  for (const l of lines) shapes.push({ fill: '', op: 1, round: false, ...l, layer: 1 });
  for (const f of fg) shapes.push({ fill: '', op: 1, shade: 0, round: false, ...f, layer: 2 });

  // ---- labels, in priority order ----
  put(`NOW ${clockTxt(now).toUpperCase()}`, [{ x: obsEnd - 4, y: pad.t - 10, a: 'end' }], 'ink-2', TOP);
  put('FORECAST TO RESET', [{ x: fStart + 6, y: pad.t - 10 }], 'ink-3', { ...TOP, x0: fStart }) || put('FORECAST', [{ x: fStart + 6, y: pad.t - 10 }], 'ink-3', { ...TOP, x0: fStart });
  if (series.length > 1 && ends.length) {
    const LH = 14;
    ends.sort((a, b) => a.y - b.y);
    const ys = ends.map(e => e.y - 5);
    const lo = PLOT.y1 - 3, hi = PLOT.y0 + 10;
    ys[0] = Math.max(ys[0], hi);
    for (let n = 1; n < ys.length; n++) ys[n] = Math.max(ys[n], ys[n - 1] + LH);
    ys[ys.length - 1] = Math.min(ys.at(-1), lo);
    for (let n = ys.length - 2; n >= 0; n--) ys[n] = Math.min(ys[n], ys[n + 1] - LH);
    ends.forEach((e, n) => {
      const txt = `${e.s.tag} ${valueText(e.cur)}%`;
      put(txt, [{ x: obsEnd - 10, y: ys[n], a: 'end' }, { x: obsEnd - 10, y: ys[n] + LH, a: 'end' }, { x: obsEnd - 10, y: ys[n] - LH, a: 'end' }], 'series-ink', OBS, { shade: e.s.shade });
    });
  }
  if (finite(thr)) {
    const yt = Y(thr), txt = `AUTO-SWITCH ${valueText(thr)}% USED`;
    put(txt, [{ x: obsEnd - 8, y: yt + 14, a: 'end' }, { x: obsEnd - 8, y: yt - 5, a: 'end' }, { x: pad.l + 8, y: yt + 14 }, { x: pad.l + 8, y: yt - 5 }, { x: pad.l + obsW / 2, y: yt + 14, a: 'middle' }], 'ink-2', OBS)
      || put(txt, [{ x: pad.l + plotW - 8, y: yt + 14, a: 'end' }, { x: pad.l + plotW - 8, y: yt - 5, a: 'end' }], 'ink-2', FC);
  }
  for (const r of runouts) {
    const yr = Y(100), txt = `RUNS OUT ${timeTxt(r.at).toUpperCase()}`;
    const c = [];
    for (const dy of [20, 34, 48, 62, 76]) c.push({ x: r.x, y: yr + dy, a: 'middle' }, { x: r.x + 8, y: yr + dy }, { x: FC.x1 - 4, y: yr + dy, a: 'end' }, { x: r.x - 8, y: yr + dy, a: 'end' });
    c.push({ x: r.x, y: yr - 9, a: 'middle' }, { x: FC.x1 - 4, y: yr - 9, a: 'end' });
    put(txt, c, 'crit-text', FC);
  }
  for (const r of resetsAt) {
    const txt = `RESET ${clockTxt(r.t).toUpperCase()}`;
    put(txt, [{ x: r.x + 5, y: PLOT.y1 - 5 }, { x: r.x - 5, y: PLOT.y1 - 5, a: 'end' }, { x: r.x + 5, y: PLOT.y0 + 13 }, { x: r.x - 5, y: PLOT.y0 + 13, a: 'end' }], 'ink-3', OBS);
  }
  for (const [a, b] of bands) {
    const x0 = xo(a), x1 = Math.min(xo(Math.min(b, now)), obsEnd - 2);
    const txt = `ACTIVE${cmp ? ` · ${tagOf(acc).toUpperCase()}` : ''}`;
    put(txt, [{ x: x0 + 6, y: PLOT.y0 + 13 }, { x: x0 + 6, y: PLOT.y1 - 5 }, { x: x0 + 6, y: PLOT.y0 + 27 }], 'accent-text', { ...OBS, x0: Math.max(OBS.x0, x0), x1: Math.min(OBS.x1, x1) });
  }
  legend.push({ name: 'Projection', kind: 'proj', shade: 0, dash: 0, width: 1.75 });
  if (finite(thr)) legend.push({ name: 'Auto-switch', kind: 'thr', shade: 0, dash: 0, width: 1.25 });
  if (bands.length) legend.push({ name: 'Active', kind: 'active', shade: 0, dash: 0, width: 0 });

  // ---- hover stops: every observed sample time and "now", then the forecast an hour at a time ----
  const valueAt = (s, t, inF) => {
    if (!inF) {
      if (t >= now - 60e3) return finite(s.w.used) ? s.w.used : null;
      const h = historyOf(s.acc.analytics, s.w.key).filter(p => p.t <= t);
      return h.length ? h.at(-1).v : null;
    }
    const pr = project(s.acc, s.w, now), cur = s.w.used;
    if (!finite(cur)) return null;
    if (s.w.resetAt && t >= s.w.resetAt) return 'reset';
    if (pr.kind === 'limit') return cur;
    if (finite(pr.rate)) return Math.min(100, cur + pr.rate * ((t - now) / H));
    return 'nodata';
  };
  const times = [...new Set(series.flatMap(s => historyOf(s.acc.analytics, s.w.key).filter(p => p.t >= t0).map(p => p.t)).concat([now]))].sort((a, b) => a - b);
  const stops = [];
  const stopFor = (t, x, inF) => {
    const rows = series.map(s => {
      const v = valueAt(s, t, inF);
      return { name: s.name, value: v === 'reset' ? 'after reset' : v === 'nodata' ? 'too little data' : finite(v) ? `${valueText(v)}%` : 'no reading', shade: s.shade };
    });
    const pv = valueAt(series.at(-1), t, inF);
    return { x: pt(x), time: `${inF ? 'Projected · ' : ''}${timeTxt(t)}`.toUpperCase(), proj: inF, rows, dotY: finite(pv) ? pt(Y(pv)) : -1 };
  };
  for (const t of times) stops.push(stopFor(t, xo(t), false));
  const fW = plotW - obsW - gap;
  let lastHour = null;
  for (let px = 0; px <= fW; px += 6) {
    const t = Math.round(clamp(tf0 + (px / fW) * (tf1 - tf0), tf0, tf1) / H) * H;
    if (t === lastHour) continue;
    lastHour = t;
    stops.push(stopFor(t, xf(t), true));
  }
  // nearest stop per 4 px column; the observed half only picks observed stops, the forecast half forecast ones
  const lut = [];
  const split = pad.l + obsW + gap / 2;
  const obsIdx = stops.map((s, i) => [s, i]).filter(([s]) => !s.proj), fIdx = stops.map((s, i) => [s, i]).filter(([s]) => s.proj);
  for (let c = 0; c * 4 <= W; c++) {
    const mx = c * 4;
    const pool = mx > split && fIdx.length ? fIdx : obsIdx;
    let best = pool[0]?.[1] ?? -1, bd = Infinity;
    for (const [s, i] of pool) { const dd = Math.abs(s.x - mx); if (dd < bd) { bd = dd; best = i; } }
    lut.push(best);
  }
  return {
    ...base, empty: '', shapes, labels, dots, legend, stops, lut,
    plot: { l: pad.l, t: pad.t, w: plotW, h: plotH },
    foot: `Hourly snapshots; a gap is a missing reading, never zero. Dashed verticals mark resets; dotted lines carry the recent pace to the reset.${cmp ? ' Overlay only: accounts are never added together.' : ''}`,
  };
}
function fullName(provider, w) {
  if (w.fable) return 'Fable weekly';
  if (provider === 'codex') return { week: 'Weekly', '5h': '5-hour' }[w.label] || w.label || w.short;
  if (provider === 'claude') return { 'Five-hour usage': '5-hour usage' }[w.label] || w.label || w.short;
  return w.label || w.short;
}

// ---------------------------------------------------------------- the quota history
/**
 * The compact quota history: one row per account, grouped by provider in the registry order, rows by identity
 * (active state never selects or reorders a row). ctx: { now, dashboard, hidden, open: Set, compare: Set,
 * collapsed: Set, focusWidth, thresholds }.
 */
export function quotaView(payload, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const accounts = quotaAccounts(payload, ctx);
  const axis = quotaAxis(payload, now);
  const open = ctx.open instanceof Set ? ctx.open : new Set(), compare = ctx.compare instanceof Set ? ctx.compare : new Set(), collapsed = ctx.collapsed instanceof Set ? ctx.collapsed : new Set();
  const groups = [];
  const focus = [];
  for (const [provider, label] of QUOTA_PROVIDERS) {
    const list = accounts.filter(a => a.provider === provider).sort((a, b) => a.email.localeCompare(b.email) || a.id.localeCompare(b.id));
    if (!list.length) continue;
    const wins = list.map(mainOf).filter(Boolean).map(w => w.short);
    const common = wins.slice().sort((a, b) => wins.filter(x => x === b).length - wins.filter(x => x === a).length)[0] || '';
    const rows = list.map(acc => {
      const w = mainOf(acc);
      const v = w ? w.used : null;
      const nextR = meters(acc).filter(x => x.resetAt && x.resetAt > now).sort((a, b) => a.resetAt - b.resetAt)[0];
      const spark = sparkRuns(acc, w, axis.t0, now);
      if (open.has(acc.id)) focus.push(focusChart(acc, payload, { now, width: ctx.focusWidth, height: ctx.focusHeight, compare: compare.has(acc.id), thresholds: ctx.thresholds, accounts }));
      return {
        id: acc.id, provider, label: acc.email, active: acc.active, plan: planLabel(acc.plan),
        win: w && w.short !== common ? w.short : '',
        hasValue: finite(v), value: finite(v) ? v : 0, valueText: finite(v) ? valueText(v) : '', sev: SEV_INT[sev(v)],
        next: nextR ? untilTxt(nextR.resetAt, now) : '', nextTip: nextR ? `${nextR.short} resets ${timeTxt(nextR.resetAt)}` : '',
        open: open.has(acc.id), ...spark,
      };
    });
    groups.push({ provider, label, count: rows.length, meta: `${rows.length} account${rows.length === 1 ? '' : 's'}${common ? ` · ${common.toLowerCase()} window` : ''}`, collapsed: collapsed.has(provider), rows });
  }
  return { sub: `Each account's main window, observed since ${timeTxt(histStart(payload, now))} · kept up to 30 days · select a row for its focus chart`, ticks: axis.ticks, groups, focus };
}

// ---------------------------------------------------------------- upcoming resets and expiries
const PERIOD_ORDER = { '5h': 0, week: 1, month: 2, other: 3 };
function resetName(a, w, first) {
  if (w.kind === 'spend') return first ? 'Spend' : 'spend';
  if (w.fable) return 'Fable';
  if (['antigravity', 'cursor', 'zai'].includes(a.provider)) return w.short || w.label;
  const p = { '5h': '5-hour', week: 'Weekly', month: 'Monthly' }[w.period];
  if (!p) return w.label || 'Usage';
  return first || p === '5-hour' ? p : p.toLowerCase();
}
function resetWhat(a, wins) {
  if (wins.length === 1) {
    const w = wins[0];
    if (w.fable) return 'Weekly Fable limit resets';
    if (w.kind === 'spend') return 'Spend limit resets';
    if (['antigravity', 'cursor', 'zai'].includes(a.provider)) return `${w.short} limit resets`;
    return ({ '5h': '5-hour limit resets', week: 'Weekly limit resets', month: 'Monthly limit resets' }[w.period]) || `${w.label} resets`;
  }
  const names = wins.map((w, i) => resetName(a, w, i === 0));
  return `${names.slice(0, -1).join(', ')} and ${names.at(-1)} limits reset`;
}
export function agendaEvents(accounts, now) {
  const ev = [];
  for (const a of accounts) {
    // windows of one account that reset at the same minute share one line ("Weekly and Fable limits reset")
    const resets = new Map();
    const addReset = w => { const k = Math.round(w.resetAt / 60e3); if (!resets.has(k)) resets.set(k, []); resets.get(k).push(w); };
    for (const w of meters(a)) {
      if (w.resetAt && w.resetAt > now) addReset(w);
      if (w.expiresAt && w.expiresAt > now) ev.push({ t: w.expiresAt, a, what: w.planExpiry ? 'Plan subscription ends' : `${w.label} expires`, kind: 'exp', amt: '' });
    }
    for (const w of a.windows.filter(x => !x.meter)) {
      const lab = w.label;
      if (w.expiresAt && w.expiresAt > now && !(finite(w.remaining) && w.remaining === 0)) ev.push({ t: w.expiresAt, a, what: /pack/i.test(lab) ? 'Credit pack expires' : /bank|reset/i.test(lab) ? 'Banked reset expires' : `${lab} expires`, kind: 'exp', amt: finite(w.remaining) ? `${unitAmount(w.remaining, w.unit)} left` : '' });
      if (w.kind === 'spend' && w.resetAt && w.resetAt > now) addReset(w);
    }
    for (const ws of resets.values()) {
      ws.sort((x, y) => (x.kind === 'spend') - (y.kind === 'spend') || x.fable - y.fable || (PERIOD_ORDER[x.period] ?? 3) - (PERIOD_ORDER[y.period] ?? 3));
      const vals = ws.filter(w => w.kind !== 'spend' && finite(w.used)).map(w => ({ name: resetName(a, w, true), v: w.used }));
      ev.push({ t: Math.min(...ws.map(w => w.resetAt)), a, what: resetWhat(a, ws), vals, kind: 'reset' });
    }
  }
  return ev.sort((x, y) => x.t - y.t || x.a.email.localeCompare(y.a.email));
}
/** "now 0% used", "now 0% used each", "now 1.45%, 1.47% and 0.44% used", or "20,000 credits left". */
function nowRuns(e) {
  if (e.kind === 'exp') return e.amt ? [run(e.amt)] : [];
  const vals = e.vals || [];
  if (!vals.length) return [];
  const v = x => run(`${valueText(x.v)}%`, true, sev(x.v));
  if (vals.length === 1) return [run('now '), v(vals[0]), run(' used')];
  if (vals.every(x => x.v === vals[0].v)) return [run('now '), v(vals[0]), run(' used each')];
  const parts = vals.flatMap((x, i) => i === 0 ? [v(x)] : i === vals.length - 1 ? [run(' and '), v(x)] : [run(', '), v(x)]);
  return [run('now '), ...parts, run(' used')];
}
/**
 * The agenda, by local day, split into two balanced columns of whole days for the wide layout (stacked, the
 * second column continues the first under one header).
 */
export function agendaView(payload, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const ev = agendaEvents(quotaAccounts(payload, ctx), now);
  const today = dayStart(now), tomorrow = addDays(today, 1);
  const groups = [];
  for (const e of ev) { const d = dayStart(e.t); let g = groups.at(-1); if (!g || g.d !== d) { g = { d, items: [] }; groups.push(g); } g.items.push(e); }
  const wt = g => 1.4 + g.items.length;
  const total = groups.reduce((s, g) => s + wt(g), 0);
  let cut = groups.length, best = Infinity, acc = 0;
  groups.forEach((g, k) => { acc += wt(g); const m = Math.max(acc, total - acc); if (k < groups.length - 1 && m < best) { best = m; cut = k + 1; } });
  let i = 0;
  const rowsOf = gs => gs.flatMap(g => {
    const n = Math.round((g.d - today) / D);
    const head = g.d === today ? { day: 'Today', hint: wmdTxt(g.d) } : g.d === tomorrow ? { day: 'Tomorrow', hint: wmdTxt(g.d) } : { day: wmdTxt(g.d), hint: `in ${n} days` };
    return [{ kind: 'day', ...head, time: '', rel: '', provider: '', who: '', what: '', now: [], tip: '', i: i++ },
      ...g.items.map(e => ({ kind: 'line', day: '', hint: '', time: clockTxt(e.t), rel: g.d === today ? untilTxt(e.t, now) : '', provider: e.a.provider, who: e.a.email, what: e.what, now: nowRuns(e), tip: `${timeTxt(e.t)} · ${untilTxt(e.t, now)}`, i: i++ }))];
  });
  return { empty: !ev.length, a: rowsOf(groups.slice(0, cut)), b: rowsOf(groups.slice(cut)) };
}
