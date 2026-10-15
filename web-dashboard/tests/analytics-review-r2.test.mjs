import test from 'node:test';
import assert from 'node:assert/strict';
import * as T from '../public/time-format.mjs';
import * as U from '../public/analytics-usage.mjs';
import { quotaAxis } from '../public/analytics-quota.mjs';

process.env.TZ = 'Asia/Tokyo';
const H = U.H, D = U.D;
const iso = t => new Date(t).toISOString().replace('.000Z', 'Z');
const state = extra => ({ range: '7d', prov: 'all', cache: true, heat: 'cost', donut: 'cost', ...extra });
const row = (t, cost = 1, extra = {}) => ({ hour: iso(t), provider: 'claude', inputTokens: 1000, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: cost, ...extra });
const payload = (now, rows, extra = {}) => ({ range: { preset: '7d', from: iso(now - 7 * D) }, activity: {
  status: 'ok', fetchedAt: iso(now), byHour: rows, totals: { inputTokens: 1, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 }, models: [], ...extra,
} });

test('NY spring and fall bucket grids retain all 168 hourly rows with a different process zone', () => {
  T.setDisplayTimeZone('America/New_York');
  for (const date of ['2026-03-11T15:00:00Z', '2026-11-04T15:00:00Z']) {
    const now = Date.parse(date), rows = Array.from({ length: 168 }, (_, i) => row(now - (168 - i) * H));
    const p = payload(now, rows), A = U.activityData(p, now), R = U.pageRange(state(), A, now);
    const B = U.buckets(A, R, state());
    assert.equal(B.reduce((sum, b) => sum + b.in, 0), 168000);
    assert.equal(U.usageView(p, state(), { now }).kpis[0].num, 168000);
    for (const b of B) assert.equal(U.bucketStart(b.t, R.step), b.t);
    for (const r of rows) assert.ok(B.some(b => b.t === U.bucketStart(Date.parse(r.hour), R.step)));
    const axis = quotaAxis({ history: { oldestSampleAt: iso(now - 7 * D) } }, now);
    assert.ok(axis.ticks.length >= 2);
    for (const tick of axis.ticks) {
      assert.ok(Number.isFinite(tick.pos));
      const t = Math.round(axis.t0 + tick.pos * (now - axis.t0));
      assert.equal(T.zonedHour(t), 0);
      assert.equal(t, T.zonedDayStart(t));
    }
    const view = U.usageView(p, state(), { now, sizes: { trend: { w: 350, h: 220 } } });
    assert.ok(view.trend.xTicks.length >= 2);
    for (const tick of view.trend.xTicks) assert.ok(tick.x >= 18 && tick.x <= view.trend.pw - 18);
  }
});

test('display-zone midnights, daily buckets, calendar and month boundaries agree across DST', () => {
  T.setDisplayTimeZone('America/New_York');
  for (const [date, midnight, hours] of [['2026-03-08', '2026-03-08T05:00:00Z', 23], ['2026-11-01', '2026-11-01T04:00:00Z', 25]]) {
    const start = U.parseIsoDay(date), end = U.addDays(start, 1), now = end + 10 * H;
    assert.equal(start, Date.parse(midnight));
    assert.equal(T.zonedDayStart(start + 12 * H), start);
    assert.equal(end - start, hours * H);
    assert.equal(U.isoDay(start), date);
    const rows = Array.from({ length: hours }, (_, i) => row(start + i * H));
    const p = payload(now, rows), A = U.activityData(p, now);
    const R = U.pageRange(state({ range: 'custom', from: start, to: end }), A, now);
    const daily = U.buckets(A, { ...R, step: D }, state());
    assert.equal(daily.length, 1);
    assert.equal(daily[0].in, hours * 1000);
    assert.equal(daily[0].t1, end);
    const cal = U.calendarView(A, R, now);
    const day = cal.cells.find(c => c.day === date);
    assert.equal(day.label, String(T.zonedParts(start).day));
    assert.match(day.long, /Sun/);
  }
  const boundary = Date.parse('2026-11-01T02:00:00Z');
  assert.equal(U.isoDay(boundary), '2026-10-31');
  assert.equal(T.zonedMonthStart(boundary), Date.parse('2026-10-01T04:00:00Z'));
  assert.equal(U.apiRangeFor({ range: 'month' }, boundary), '30d');
  assert.equal(U.parseIsoDay('2026-02-30'), NaN);
});

test('module formatters follow Settings changes; heatmap hours remain nominal local labels', () => {
  const now = Date.parse('2026-10-01T00:30:00Z');
  T.setDisplayTimeZone('UTC');
  assert.equal(U.mdTxt(now), 'Oct 1');
  T.setDisplayTimeZone('America/New_York');
  assert.equal(U.mdTxt(now), 'Sep 30');
  assert.equal(U.hourTxt(now), '8 PM');
  const view = U.usageView(payload(now, []), state(), { now, sizes: { heat: { w: 850, h: 22, cellW: 24 } } });
  assert.equal(view.heat.hours[0], '12 AM');
  assert.equal(view.heat.hours[12], '12 PM');
  assert.equal(view.heat.dash, U.dashedRect(24, 22));
});

test('missing fetched timestamps and unavailable activity never claim freshness or measured cache zeros', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');
  const view = U.usageView(payload(now, [], { fetchedAt: null, status: 'unavailable', totals: null }), state({ prov: 'claude' }), { now });
  assert.equal(view.head.read, 'not read yet');
  assert.match(view.head.readTip, /not read yet/i);
  assert.match(view.calendar.note, /not read yet/i);
  assert.match(view.scope.find(r => r.icon === 'clock').text, /not read yet/i);
  assert.equal(view.picker.items.find(r => r.value === 'claude').tokens, 'Unavailable');
  assert.equal(view.cache.reads.tok, 'Unavailable');
  assert.equal(view.cache.writes.tok, 'Unavailable');
  assert.equal(view.cache.hasHit, false);
  assert.equal(view.cache.reads.w, 0);
  assert.ok(Number.isFinite(view.range.end));
  const measured = U.usageView(payload(now, []), state(), { now });
  assert.equal(measured.cache.reads.tok, '0');
});

test('unknown-only costs break trend paths and differ from empty and free cells', () => {
  T.setDisplayTimeZone('UTC');
  const now = Date.parse('2026-10-01T12:00:00Z');
  const rows = [row(now - 3 * H), row(now - 2 * H, 2, { fallbackCostUsd: 2 }), row(now - H)];
  const view = U.usageView(payload(now, rows), state({ range: '24h' }), { now });
  const path = U.trendPaths(view.trend.geo).cost;
  assert.equal((path.match(/M/g) || []).length, 2);
  assert.ok(view.trend.geo.cost.includes(null));
  assert.ok(U.mixGeo(view.trend.geo, view.trend.geo, 0.5).cost.includes(null));
  const unknown = view.heat.cells.find(c => /cost not logged/.test(c.tip));
  assert.equal(unknown.state, 3);
  assert.ok(view.heat.cells.some(c => c.state === 1));
  assert.ok(view.daily.bars.some(b => b.total === 'Not logged'));
  const free = U.usageView(payload(now, [row(now - H, 0)]), state({ range: '24h' }), { now });
  assert.equal(free.heat.cells.find(c => /Thu 11 AM/.test(c.tip)).state, 1);
  assert.ok(!free.trend.geo.cost.includes(null));
  const missing = U.usageView(payload(now, [row(now - H, null)]), state(), { now });
  assert.ok(missing.daily.bars.every(b => b.total === 'Unavailable'));
});

test('cost ticks keep fractional dollar steps and never use sub-cent divisions', () => {
  T.setDisplayTimeZone('UTC');
  const now = Date.parse('2026-10-01T12:00:00Z');
  for (const cost of [9, 0.001]) {
    const view = U.usageView(payload(now, [row(now - H, cost)]), state({ range: '24h' }), { now });
    for (const ticks of [view.trend.yTicks.map(t => t.right), view.daily.yTicks.map(t => t.label)]) {
      const amounts = ticks.map(t => Number(t.replace(/[$,]/g, '')));
      assert.equal(new Set(amounts).size, amounts.length);
      assert.ok(amounts[1] >= 0.01);
      for (const v of amounts) assert.ok(Math.abs(v * 100 - Math.round(v * 100)) < 1e-8);
      if (cost === 9) assert.ok(amounts.some(v => !Number.isInteger(v)));
    }
  }
});


test('all-free cost donuts explain why they have no arcs', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');
  const model = { provider: 'claude', model: 'free-fixture', inputTokens: 1000, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: 0, fallbackCostUsd: 0,
    rates: { source: 'models-dev', inputPerMillion: 0, outputPerMillion: 0, cacheCreationPerMillion: 0, cacheReadPerMillion: 0 } };
  const view = U.usageView(payload(now, [row(now - H, 0)], { models: [model] }), state(), { now });
  assert.equal(view.donut.segs.length, 0);
  assert.equal(view.donut.empty, false);
  assert.match(view.donut.emptyText, /free/);
  assert.ok(view.donut.legend.length > 0);
});

