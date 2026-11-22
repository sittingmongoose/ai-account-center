import test from 'node:test';
import assert from 'node:assert/strict';
import { sparkRuns, focusChart, quotaAccounts } from '../public/analytics-quota.mjs';
process.env.TZ = 'UTC';
const { setDisplayTimeZone } = await import('../public/time-format.mjs');
setDisplayTimeZone('UTC');
const H = 3_600_000, D = 24 * H;
const iso = t => new Date(t).toISOString();

test('passed reset ends historical runs without a current ring or forecast', () => {
  const now = Date.parse('2026-10-01T12:00:00Z'), reset = now - H;
  const historyWindow = { key: 'seven_day', label: 'Weekly', kind: 'rate_limit', usedPercent: null, resetAt: iso(reset), windowMinutes: 10080,
    points: [{ sampledAt: iso(now - 3 * H), status: 'ok', usedPercent: 40, resetAt: iso(reset) }, { sampledAt: iso(now - 2 * H), status: 'ok', usedPercent: 50, resetAt: iso(reset) }] };
  const raw = { id: 'codex:a', provider: 'codex', email: 'fixture@example.test', windows: [historyWindow] };
  const p = { history: { oldestSampleAt: iso(now - 4 * H) }, accounts: [raw] };
  const acc = quotaAccounts(p, { now, dashboard: { accounts: [raw] } })[0], w = acc.windows[0];
  const spark = sparkRuns(acc, w, now - 4 * H, now);
  assert.match(spark.calmLine, /H450$/);
  const focus = focusChart(acc, p, { now, t0: now - 4 * H, accounts: [acc] });
  assert.equal(focus.dots.length, 0);
  assert.ok(focus.stops.some(s => s.rows.some(r => r.value === '50%')));
  assert.ok(focus.stops.at(-1).rows.every(r => r.value === 'no reading'));
  assert.ok(!focus.shapes.some(s => s.layer === 2 && ['series', 'crit'].includes(s.stroke)));
});


test('focus histories and projections are reused per account object within one render', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');
  const make = value => ({ id: 'codex:same', provider: 'codex', email: 'fixture@example.test', windows: [{ key: 'seven_day', label: 'Weekly', kind: 'rate_limit', windowMinutes: 10080, usedPercent: value, resetAt: iso(now + D),
    points: Array.from({ length: 249 }, (_, i) => ({ sampledAt: iso(now - (249 - i) * H), usedPercent: value, status: 'ok', resetAt: iso(now + D) })) }] });
  const raw = [make(40), make(70)];
  const accounts = raw.map(a => quotaAccounts({ accounts: [a] }, { now })[0]);
  const reads = [0, 0];
  accounts.forEach((a, i) => { a.analytics = { get windows() { reads[i]++; return raw[i].windows; } }; });
  const focus = focusChart(accounts[0], { history: { oldestSampleAt: iso(now - 7 * D) } }, { now, compare: true, accounts });
  assert.deepEqual(reads, [2, 2]);
  assert.ok(focus.stops.some(s => s.rows[0].value === '40%' && s.rows[1].value === '70%'));
  assert.ok(focus.lut.every(i => i >= 0 && i < focus.stops.length));
});

test('the final observed cell is taken over by the live reading; a missing live reading draws no zero', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');
  const historyWindow = { key: 'seven_day', label: 'Weekly', kind: 'rate_limit', usedPercent: null, resetAt: iso(now + D), windowMinutes: 10080,
    points: [{ sampledAt: iso(now - 3 * H), status: 'ok', usedPercent: 40, resetAt: iso(now + D) }, { sampledAt: iso(now - 2 * H), status: 'ok', usedPercent: 50, resetAt: iso(now + D) }] };
  const raw = { id: 'codex:takeover', provider: 'codex', email: 'fixture@example.test', windows: [historyWindow] };
  const p = { history: { oldestSampleAt: iso(now - 4 * H) }, accounts: [raw] };
  // The live dashboard reading (55%) is newer than the last history sample (50%).
  const live = { ...raw, windows: [{ ...historyWindow, usedPercent: 55, sampledAt: iso(now - 60e3) }] };
  const acc = quotaAccounts(p, { now, dashboard: { accounts: [live] } })[0];
  const focus = focusChart(acc, p, { now, accounts: [acc] });
  const observed = focus.stops.filter(s => !s.proj);
  const finalCell = observed.at(-1);
  assert.equal(finalCell.time, 'OCT 1, 12:00 PM');
  // The live figure takes over the final cell; the earlier history cells keep their own readings.
  assert.equal(finalCell.rows[0].value, '55%');
  assert.equal(observed[0].rows[0].value, '40%');
  assert.equal(observed[1].rows[0].value, '50%');
  // The sparkline's last run steps to the live value at now; history itself is never altered.
  const spark = sparkRuns(acc, acc.windows[0], now - 4 * H, now);
  assert.ok(spark.calmLine.endsWith('M592 12V11H600'), spark.calmLine);
  // No live reading: the final cell says so, and no zero is drawn anywhere.
  const stale = { ...raw, windows: [{ ...historyWindow, usedPercent: null }] };
  const acc2 = quotaAccounts(p, { now, dashboard: { accounts: [stale] } })[0];
  const focus2 = focusChart(acc2, p, { now, accounts: [acc2] });
  assert.equal(focus2.stops.filter(s => !s.proj).at(-1).rows[0].value, 'no reading');
  assert.ok(!focus2.stops.some(s => s.rows.some(r => r.value === '0%')), JSON.stringify(focus2.stops));
  const spark2 = sparkRuns(acc2, acc2.windows[0], now - 4 * H, now);
  assert.equal(spark2.points, 2);
  assert.ok(!spark2.calmLine.includes('H600'), spark2.calmLine);
});
