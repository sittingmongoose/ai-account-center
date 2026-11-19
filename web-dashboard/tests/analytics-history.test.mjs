import test from 'node:test';
import assert from 'node:assert/strict';

// Analytics history (docs/store.md section 12): the 90D range, answers keyed by range, the
// multi-month picker, weekly buckets for long ranges and gaps that are never drawn as zero.
process.env.TZ = 'UTC';
const { setDisplayTimeZone, zonedWeekday } = await import('../public/time-format.mjs');
setDisplayTimeZone('UTC');
const U = await import('../public/analytics-usage.mjs');
const P = await import('../public/analytics-page.mjs');
const { usageView, apiRangeFor, apiQueryFor, payloadViewKey, RANGES } = U;

const D = 86_400_000;
const H = 3_600_000;
const now = Date.parse('2026-11-13T12:00:00Z');
const row = (iso, provider, inputTokens, cost) => ({ hour: iso, provider, inputTokens, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: cost });
const state = (overrides = {}) => ({ range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', ...overrides });
function payload({ preset = '90d', from = now - 90 * D, rows = [], history = null, resolution = 'hour', availableFrom = Date.parse('2026-08-20T10:00:00Z'), days, now: at = now, fetchedAt = at - 60_000 } = {}) {
  const tot = rows.reduce((t, r) => ({ ...t, inputTokens: t.inputTokens + r.inputTokens, estimatedCostUsd: t.estimatedCostUsd + r.estimatedCostUsd }), { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: 0 });
  return {
    schemaVersion: 1,
    range: { preset, from: new Date(from).toISOString(), to: new Date(at).toISOString(), bucketMinutes: 1440, availableFrom: new Date(availableFrom).toISOString(), resolution, ...(days ? { days } : {}) },
    activity: {
      status: 'ok', fetchedAt: new Date(fetchedAt).toISOString(), message: '', resolution, totals: tot, byHour: rows,
      providers: [{ provider: 'codex', label: 'Codex', totals: tot, usageEvents: rows.length, sessionCount: 1, tools: ['codex'] }],
      models: [{ model: 'gpt-6', provider: 'codex', ...tot }],
      ...(history ? { history } : {}),
    },
  };
}
const history = (computers) => ({ computers, sources: [], backfill: { running: false, sources: [] } });

test('a 90D range button sits between 30D and Month, and asks for its own answer', () => {
  assert.deepEqual(RANGES.map(([id]) => id), ['24h', '7d', '30d', '90d', 'month', 'all']);
  assert.equal(RANGES.find(([id]) => id === '90d')[1], '90D');
  assert.equal(apiRangeFor(state({ range: '90d' }), now), '90d');
  assert.equal(P.applyAnalyticsAction(state({ range: '7d' }), 'analytics-range', '90d').range, true);
  const view = usageView(payload(), state({ range: '90d' }), { now });
  assert.equal(view.range.a, now - 90 * D);
  assert.match(view.trend.sub, /^Daily buckets/);
});

test('a custom range longer than the last 30 days is its own answer, keyed by its days', () => {
  const page = state({ range: 'custom', from: Date.parse('2026-08-25T00:00:00Z'), to: Date.parse('2026-10-11T00:00:00Z') });
  const key = apiRangeFor(page, now);
  assert.equal(key, 'custom:2026-08-25:2026-10-10');
  assert.deepEqual(apiQueryFor(key), { range: 'custom', from: '2026-08-25', to: '2026-10-10' });
  assert.deepEqual(apiQueryFor('90d'), { range: '90d' });
  const answer = payload({ preset: 'custom', from: page.from, days: { from: '2026-08-25', to: '2026-10-10' } });
  assert.equal(payloadViewKey(answer), key);
  assert.equal(P.analyticsCovers(answer, page, now), true);
  assert.equal(P.analyticsCovers(payload({ preset: '90d' }), page, now), false);
  // a short custom range is still cut from a preset
  assert.equal(apiRangeFor(state({ range: 'custom', from: now - 3 * D, to: now }), now), '7d');
});

test('the picker spans months back to the oldest stored day, and refuses a day before it', () => {
  const stored = payload({ history: history([{ host: 'ubuntu', oldestAt: '2026-08-20T10:00:00Z', lastReadAt: new Date(now).toISOString(), gaps: [] }]) });
  const view = usageView(stored, state({ range: 'custom', from: Date.parse('2026-09-25T00:00:00Z'), to: Date.parse('2026-10-06T00:00:00Z') }), { now });
  const cal = view.calendar;
  assert.deepEqual(cal.months.map(m => m.label), ['August 2026', 'September 2026', 'October 2026', 'November 2026']);
  const day = d => cal.cells.find(c => c.day === d);
  assert.equal(day('2026-08-19').off, true, 'before the oldest stored day');
  assert.equal(day('2026-08-20').off, false, 'the oldest stored day');
  assert.equal(day('2026-11-13').today, true);
  assert.equal(day('2026-11-14').off, true, 'after today');
  assert.equal(cal.firstN, day('2026-08-20').n);
  // a range that starts in September and ends in October
  assert.equal(cal.lo, day('2026-09-25').n);
  assert.equal(cal.hi, day('2026-10-05').n);
  assert.notEqual(cal.cells[cal.lo].m, cal.cells[cal.hi].m);
  // Monday-first weeks: August 2026 starts on a Saturday, so five blank cells come first
  assert.equal(cal.cells.filter(c => c.m === 0 && c.pad).length, 5);
  // quick picks stop at the oldest stored day
  assert.deepEqual(cal.quick.map(q => q.label), ['Today', '7 days', '30 days', '90 days']);
  assert.equal(cal.quick.find(q => q.label === '90 days').lo, cal.firstN);
  assert.match(cal.note, /back to Aug 20, 2026, across months/);
  const oldest = Date.parse('2026-08-20T10:00:00Z');
  const page = state();
  assert.equal(P.applyAnalyticsAction(page, 'analytics-custom', '2026-08-20,2026-10-05', { now, oldest }).range, true);
  assert.deepEqual(P.applyAnalyticsAction(state(), 'analytics-custom', '2026-08-19,2026-10-05', { now, oldest }), {});
  // without the history store the picker keeps its 30 days
  const plain = usageView({ ...payload(), activity: { ...payload().activity } }, state(), { now }).calendar;
  assert.equal(plain.months.length <= 2, true);
});

test('ranges longer than 120 days are weekly; day rows cannot draw the hour map', () => {
  const rows = [row('2026-05-04T04:00:00Z', 'codex', 100, 1), row('2026-05-06T04:00:00Z', 'codex', 50, 0.5), row('2026-11-10T05:00:00Z', 'codex', 7, 0.07)];
  const p = payload({ preset: 'all', from: Date.parse('2026-05-01T00:00:00Z'), rows, resolution: 'day', availableFrom: Date.parse('2026-05-01T00:00:00Z') });
  const view = usageView(p, state({ range: 'all' }), { now });
  assert.match(view.trend.sub, /^Weekly buckets/);
  assert.equal(view.daily.title, 'Weekly cost by provider');
  // the week of Monday May 4 holds both May rows
  const may = view.daily.bars.find(b => /WEEK OF MON, MAY 4/.test(b.time));
  assert.ok(may, view.daily.bars.map(b => b.time).join(' | '));
  assert.equal(may.total, '$1.50');
  assert.match(view.heat.sub, /covers ranges up to 92 days/);
});

test('a time no computer was read for is a gap, never a zero; a partly read one names who is missing', () => {
  const day = Date.parse('2026-11-01T00:00:00Z');
  const rows = [row('2026-11-01T09:00:00Z', 'codex', 100, 1), row('2026-11-05T09:00:00Z', 'codex', 50, 0.5), row('2026-11-06T09:00:00Z', 'codex', 9, 0.09)];
  const p = payload({
    preset: 'custom', from: day, rows, days: { from: '2026-11-01', to: '2026-11-07' },
    history: history([
      { host: 'ubuntu', oldestAt: '2026-10-01T00:00:00Z', lastReadAt: new Date(now).toISOString(), gaps: [['2026-11-02T00:00:00Z', '2026-11-05T00:00:00Z']] },
      { host: 'mac', oldestAt: '2026-10-01T00:00:00Z', lastReadAt: new Date(now).toISOString(), gaps: [['2026-11-02T00:00:00Z', '2026-11-07T00:00:00Z']] },
    ]),
  });
  const view = usageView(p, state({ range: 'custom', from: day, to: day + 7 * D }), { now });
  const bar = d => view.daily.bars.find(b => b.time.startsWith(d));
  for (const d of ['MON, NOV 2', 'TUE, NOV 3', 'WED, NOV 4']) {
    assert.equal(bar(d).gap, true, d);
    assert.equal(bar(d).total, 'Not read');
    assert.equal(bar(d).h, 0);
  }
  assert.equal(bar('SUN, NOV 1').gap, false);
  // Mac was still not read on Nov 5 and 6: the bars stand, and say so
  assert.equal(bar('THU, NOV 5').gap, false);
  assert.match(bar('THU, NOV 5').foot, /Not fully read: Mac/);
  // the trend passes over the gap (no zero point) and marks it
  assert.equal(view.trend.gaps.length, 1);
  // a 7-day range has 4-hour buckets: three unread days are eighteen of them
  const gapBuckets = view.trend.buckets.filter(b => b.all === 'Not read');
  assert.equal(gapBuckets.length, 18);
  assert.ok(gapBuckets.every(b => b.yTok === -1 && b.yCost === -1));
  // the readout reads like any bucket: every series says "Not read", and no $0.00 split is made up
  for (const b of gapBuckets) {
    assert.equal(b.gap, true);
    assert.deepEqual([b.vin, b.vout, b.vcw, b.vcr, b.all, b.cost], Array(6).fill('Not read'));
    assert.doesNotMatch(`${b.foot} ${b.foot2}`, /\$0\.00/);
  }
  // the scope says what is kept and since when
  assert.match(view.scope.find(l => l.icon === 'database').text, /since Oct 1, 2026\. Hours a computer could not be read show as gaps, never as zero/);
});

test('multi-week buckets align to whole weeks and step by their own width', () => {
  // the fortnight buckets "All" grows to past 800 weeks: every bucket is a Monday, buckets never overlap,
  // and a row always maps back to the bucket that holds it
  const fortnight = 14 * D;
  const starts = new Set();
  for (let m = Date.parse('2001-01-01T00:00:00Z'); m <= Date.parse('2026-11-30T00:00:00Z'); m += 7 * D) {
    const s = U.bucketStart(m, fortnight);
    assert.equal(zonedWeekday(s), 0, `${new Date(s).toISOString()} starts on a Monday`);
    assert.equal(U.bucketStart(s, fortnight), s, 'an aligned Monday is its own bucket start');
    assert.equal(U.nextBucket(s, fortnight) - s, fortnight, 'the next bucket is a fortnight away');
    assert.ok(s <= m && m < U.nextBucket(s, fortnight), 'the Monday falls inside its bucket');
    starts.add(s);
  }
  // a fortnight grid keeps every other Monday: 1353 Mondays, 677 distinct bucket starts
  assert.equal(starts.size, 677);
});

test('"All" over more than 800 weeks grows the bucket instead of silently cutting the range off', () => {
  // one bogus 2001 record puts the store's oldest hour 25 years back: about 1329 weeks, past the 800 buckets
  const from = Date.parse('2001-06-04T00:00:00Z');
  const rows = [row('2001-06-04T04:00:00Z', 'codex', 111, 1.11), row('2026-11-10T05:00:00Z', 'codex', 7, 0.07)];
  const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from });
  const view = usageView(p, state({ range: 'all' }), { now });
  // the week doubles until the buckets cover the whole range, and the labels say the wider bucket
  assert.equal(view.range.step, 14 * D);
  assert.match(view.trend.sub, /^2-week buckets/);
  assert.equal(view.daily.title, '2-week cost by provider');
  assert.ok(view.trend.buckets.length <= 800);
  // the last bucket is the one holding the latest record: the buckets reach the end of the range
  assert.match(view.trend.buckets.at(-1).time, /^2 WEEKS FROM MON, NOV 2, 2026/);
  const A = U.activityData(p, now), R = U.pageRange(state({ range: 'all' }), A, now);
  const B = U.buckets(A, R, state({ range: 'all' }));
  assert.equal(B.length, view.trend.buckets.length);
  for (const b of B) assert.equal(U.bucketStart(b.t, R.step), b.t);
  // no data is dropped silently: the bogus 2001 record and the latest one are both in the chart
  assert.equal(B.reduce((s, b) => s + b.in, 0), 111 + 7);
  assert.ok(view.daily.bars.some(b => b.total === '$1.11'));
  assert.ok(view.daily.bars.some(b => b.total === '$0.07'));
});

test('a daylight-saving change cannot hide a week from the bucket count', () => {
  setDisplayTimeZone('America/New_York');
  try {
    // 800 full New York weeks plus 30 minutes of week 801: the elapsed time is under 800 fixed 168-hour
    // weeks, so a millisecond count misses that the 801st local bucket exists and the range truncates
    const at = Date.parse('2026-05-04T04:30:00Z');
    const from = Date.parse('2011-01-03T05:00:00Z');
    const rows = [row('2011-01-03T05:00:00Z', 'codex', 10, 1), row('2026-05-04T04:00:00Z', 'codex', 37, 3.7)];
    const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from, now: at });
    const view = usageView(p, state({ range: 'all' }), { now: at });
    assert.equal(view.range.step, 14 * D, 'the 801st local week widens the bucket');
    assert.match(view.trend.sub, /^2-week buckets/);
    const A = U.activityData(p, at), R = U.pageRange(state({ range: 'all' }), A, at);
    assert.equal(U.buckets(A, R, state({ range: 'all' })).reduce((s, b) => s + b.in, 0), 47);
    assert.ok(view.daily.bars.some(b => b.total === '$3.70'), 'the newest record is in the daily bars');
    // the reverse offset change widens nothing: exactly 800 local weeks stay weekly
    const at2 = Date.parse('2026-11-02T05:00:00Z');
    const from2 = Date.parse('2011-07-04T04:00:00Z');
    const q = payload({ preset: 'all', from: from2, rows: [row('2011-07-04T04:00:00Z', 'codex', 5, 0.5)], resolution: 'day', availableFrom: from2, now: at2 });
    const view2 = usageView(q, state({ range: 'all' }), { now: at2 });
    assert.equal(view2.range.step, 7 * D);
    assert.match(view2.trend.sub, /^Weekly buckets/);
    assert.equal(view2.trend.buckets.length, 800);
  } finally {
    setDisplayTimeZone('UTC');
  }
});

test('a skipped calendar date cannot drop a widened bucket (Pacific/Apia)', () => {
  setDisplayTimeZone('Pacific/Apia');
  try {
    // Apia skipped local Friday Dec 30 2011: stepping local dates lands the fortnight after Dec 19 on
    // Tuesday Jan 3, while the rows align to Monday Jan 2 — counting, enumeration and row lookup must
    // share the civil week index so every row lands in exactly one bucket
    const at = Date.parse('2026-11-16T11:00:00Z');
    const from = Date.parse('2007-01-01T11:00:00Z');
    const rows = [row('2012-01-01T11:00:00Z', 'codex', 100, 100), row('2012-01-09T10:00:00Z', 'codex', 25, 25), row('2026-11-15T10:00:00Z', 'codex', 7, 7)];
    const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from, now: at, fetchedAt: at });
    const view = usageView(p, state({ range: 'all' }), { now: at });
    assert.equal(view.range.step, 14 * D);
    assert.match(view.trend.sub, /^2-week buckets/);
    const A = U.activityData(p, at), R = U.pageRange(state({ range: 'all' }), A, at);
    const B = U.buckets(A, R, state({ range: 'all' }));
    assert.equal(B.length, 520);
    // every bucket starts on a Monday, even across the skipped date
    for (const b of B) assert.equal(zonedWeekday(b.t), 0, `${new Date(b.t).toISOString()} starts on a Monday`);
    // every row lands in exactly one bucket, and the totals match on both charts
    for (const r of rows) assert.equal(B.filter(b => b.lo <= Date.parse(r.hour) && Date.parse(r.hour) < b.hi).length, 1, r.hour);
    assert.equal(B.reduce((s, b) => s + b.in, 0), 132);
    assert.equal(B.reduce((s, b) => s + b.cost, 0), 132);
    // the fortnight the two 2012 rows align to is the Monday Jan 2 bucket, not a Tuesday drift
    const jan2 = B.find(b => b.t === Date.parse('2012-01-01T10:00:00Z'));
    assert.ok(jan2, 'the Monday Jan 2 2012 fortnight is enumerated');
    assert.equal(jan2.in, 125);
    assert.equal(jan2.cost, 125);
    assert.ok(view.daily.bars.some(b => b.total === '$125.00'));
    assert.ok(view.daily.bars.some(b => b.total === '$7.00'));
    assert.equal(view.trend.buckets.length, B.length);
  } finally {
    setDisplayTimeZone('UTC');
  }
});

test('a Monday clock change cannot drop a widened bucket (Atlantic/Madeira 1920)', () => {
  setDisplayTimeZone('Atlantic/Madeira');
  try {
    // Madeira's Monday 1920-03-01 began at 01:00 local (01:00 UTC) after a Sunday jump from UTC−01.
    // Resolving the next UTC midnight used to answer Tuesday Mar 2, the read's exclusive end, so the bucket
    // was never enumerated: both charts lost its 125 tokens while the headline totals kept them
    const at = Date.parse('2026-11-18T00:00:00Z');
    const from = Date.parse('1903-12-28T04:00:00Z');
    const rows = [row('1920-02-29T22:00:00Z', 'codex', 7, 7), row('1920-03-01T01:00:00Z', 'codex', 25, 25), row('1920-03-01T23:00:00Z', 'codex', 100, 100)];
    const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from, now: at, fetchedAt: Date.parse('1920-03-02T00:00:00Z') });
    const view = usageView(p, state({ range: 'all' }), { now: at });
    assert.equal(view.range.step, 14 * D);
    assert.match(view.trend.sub, /^2-week buckets/);
    const A = U.activityData(p, at), R = U.pageRange(state({ range: 'all' }), A, at);
    const B = U.buckets(A, R, state({ range: 'all' }));
    assert.equal(B.length, 423);
    // every bucket starts on its Monday, in order, and the next one begins where the last one ends
    for (const b of B) assert.equal(zonedWeekday(b.t), 0, `${new Date(b.t).toISOString()} starts on a Monday`);
    for (let i = 1; i < B.length; i++) assert.equal(B[i].t, B[i - 1].t1, 'buckets are contiguous');
    // the Monday after the jump resolves to the earliest instant of Mar 1, never to Tuesday Mar 2
    const feb16 = B.find(b => b.t === Date.parse('1920-02-16T01:00:00Z'));
    const mar1 = B.find(b => b.t === Date.parse('1920-03-01T01:00:00Z'));
    assert.ok(feb16 && mar1, 'the fortnights around the jump are enumerated');
    assert.equal(feb16.t1, mar1.t);
    // every row lands in exactly one bucket, by its interval and by its civil week, and the totals match
    for (const r of rows) {
      const t = Date.parse(r.hour);
      assert.equal(B.filter(b => b.lo <= t && t < b.hi).length, 1, r.hour);
      assert.equal(B.filter(b => b.t === U.bucketStart(t, R.step)).length, 1, r.hour);
    }
    assert.equal(mar1.in, 125);
    assert.equal(mar1.cost, 125);
    // the fortnight ends at the read's exclusive end: nothing after Mar 2 00:00 belongs to it
    assert.equal(mar1.hi, Date.parse('1920-03-02T00:00:00Z'));
    assert.equal(B.filter(b => b.lo <= mar1.hi && mar1.hi < b.hi).length, 0);
    assert.equal(B.reduce((s, b) => s + b.in, 0), 132);
    assert.equal(B.reduce((s, b) => s + b.cost, 0), 132);
    assert.ok(view.daily.bars.some(b => b.total === '$125.00'));
    assert.ok(view.daily.bars.some(b => b.total === '$7.00'));
    assert.equal(view.trend.buckets.length, B.length);
    assert.equal(view.daily.bars.length, B.length);
  } finally {
    setDisplayTimeZone('UTC');
  }
});

test('a skipped calendar date keeps a weekly bucket whole (Pacific/Apia 2011)', () => {
  setDisplayTimeZone('Pacific/Apia');
  try {
    // Apia skipped local Friday Dec 30 2011. A weekly grid that steps local dates lands the week of
    // Monday Dec 26 on a Tuesday; keying by civil week number keeps the week whole, so rows on both
    // sides of the gap stay in exactly one bucket whatever the transition did to its instants
    const at = Date.parse('2012-02-01T12:00:00Z');
    const from = Date.parse('2011-08-01T12:00:00Z');
    const rows = [row('2011-12-28T10:00:00Z', 'codex', 10, 10), row('2011-12-30T10:00:00Z', 'codex', 20, 20), row('2012-01-01T10:00:00Z', 'codex', 30, 30)];
    const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from, now: at, fetchedAt: at });
    const view = usageView(p, state({ range: 'all' }), { now: at });
    assert.equal(view.range.step, 7 * D);
    assert.match(view.trend.sub, /^Weekly buckets/);
    const A = U.activityData(p, at), R = U.pageRange(state({ range: 'all' }), A, at);
    const B = U.buckets(A, R, state({ range: 'all' }));
    assert.equal(B.length, 27);
    for (const b of B) assert.equal(zonedWeekday(b.t), 0, `${new Date(b.t).toISOString()} starts on a Monday`);
    for (let i = 1; i < B.length; i++) assert.equal(B[i].t, B[i - 1].t1, 'buckets are contiguous');
    // the skipped Friday stays inside the week of Monday Dec 26; Monday Jan 2 starts the next week
    const dec26 = B.find(b => b.t === U.bucketStart(Date.parse('2011-12-28T10:00:00Z'), R.step));
    const jan2 = B.find(b => b.t === U.bucketStart(Date.parse('2012-01-01T10:00:00Z'), R.step));
    assert.ok(dec26 && jan2, 'the weeks on both sides of the skipped date are enumerated');
    assert.equal(dec26.t1, jan2.t);
    assert.equal(dec26.in, 30);
    assert.equal(jan2.in, 30);
    for (const r of rows) {
      const t = Date.parse(r.hour);
      assert.equal(B.filter(b => b.lo <= t && t < b.hi).length, 1, r.hour);
      assert.equal(B.filter(b => b.t === U.bucketStart(t, R.step)).length, 1, r.hour);
    }
    assert.equal(B.reduce((s, b) => s + b.in, 0), 60);
    assert.equal(B.reduce((s, b) => s + b.cost, 0), 60);
    assert.ok(view.daily.bars.some(b => b.total === '$30.00'));
    assert.equal(view.trend.buckets.length, B.length);
    assert.equal(view.daily.bars.length, B.length);
  } finally {
    setDisplayTimeZone('UTC');
  }
});

test('a Monday clock change cannot drop a weekly bucket (America/Danmarkshavn 1996)', () => {
  setDisplayTimeZone('America/Danmarkshavn');
  try {
    // Danmarkshavn moved from UTC−03 to UTC+00 on Monday 1996-01-01, whose first instant is 03:00 UTC.
    // A weekly range ending at Jan 2 00:00 lost the whole Monday bucket to the same next-day fallback
    const at = Date.parse('2026-11-18T00:00:00Z');
    const from = Date.parse('1995-08-01T00:00:00Z');
    const to = Date.parse('1996-01-02T00:00:00Z');
    const rows = [row('1995-12-31T22:00:00Z', 'codex', 7, 7), row('1996-01-01T04:00:00Z', 'codex', 25, 25), row('1996-01-01T20:00:00Z', 'codex', 100, 100)];
    const p = payload({ preset: 'custom', from, rows, resolution: 'day', days: { from: '1995-08-01', to: '1996-01-01' } });
    p.range.to = new Date(to).toISOString();
    const view = usageView(p, state({ range: 'custom', from, to }), { now: at });
    assert.equal(view.range.step, 7 * D);
    assert.match(view.trend.sub, /^Weekly buckets/);
    const A = U.activityData(p, at), R = U.pageRange(state({ range: 'custom', from, to }), A, at);
    assert.equal(R.end, to);
    const B = U.buckets(A, R, state({ range: 'custom', from, to }));
    assert.equal(B.length, 23);
    for (let i = 1; i < B.length; i++) assert.equal(B[i].t, B[i - 1].t1, 'buckets are contiguous');
    // Monday Jan 1 begins at its first instant, 03:00 UTC, and ends at the read's exclusive end
    const jan1 = B.at(-1);
    assert.equal(jan1.t, Date.parse('1996-01-01T03:00:00Z'));
    assert.equal(zonedWeekday(jan1.t), 0);
    assert.equal(jan1.hi, to);
    assert.equal(jan1.in, 125);
    assert.equal(jan1.cost, 125);
    for (const r of rows) {
      const t = Date.parse(r.hour);
      assert.equal(B.filter(b => b.lo <= t && t < b.hi).length, 1, r.hour);
      assert.equal(B.filter(b => b.t === U.bucketStart(t, R.step)).length, 1, r.hour);
    }
    assert.equal(B.reduce((s, b) => s + b.in, 0), 132);
    assert.equal(B.reduce((s, b) => s + b.cost, 0), 132);
    assert.ok(view.daily.bars.some(b => b.total === '$125.00'));
    assert.equal(view.daily.bars.length, B.length);
  } finally {
    setDisplayTimeZone('UTC');
  }
});

test('a clipped multi-week cost bar says the dates it covers, not just clocks', () => {
  // All over [2011-01-05, 2026-05-07): the first fortnight is clipped to Jan 5-17 and the last to May 4-7;
  // both readouts must carry the covered dates, while a same-day slice keeps the clock-only label
  const at = Date.parse('2026-05-07T00:00:00Z');
  const from = Date.parse('2011-01-05T00:00:00Z');
  const rows = [row('2011-01-05T00:00:00Z', 'codex', 10, 1)];
  const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from, now: at, fetchedAt: at });
  const view = usageView(p, state({ range: 'all' }), { now: at });
  assert.equal(view.range.step, 14 * D);
  assert.equal(view.daily.bars[0].time, '2 WEEKS FROM MON, JAN 3, 2011 · JAN 5, 2011 TO JAN 17, 2011');
  assert.equal(view.daily.bars.at(-1).time, '2 WEEKS FROM MON, MAY 4, 2026 · MAY 4 TO MAY 7');
  // the read stops at noon on May 4: the last bar is a same-day slice and keeps the clock-only label
  const q = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from, now: at, fetchedAt: Date.parse('2026-05-04T12:00:00Z') });
  const view2 = usageView(q, state({ range: 'all' }), { now: at });
  assert.equal(view2.daily.bars.at(-1).time, '2 WEEKS FROM MON, MAY 4, 2026 · 12:00 AM TO 12:00 PM');
});

test('a backfill in progress says so', () => {
  const p = payload({ history: { computers: [], sources: [], backfill: { running: true, sources: [{ host: 'ubuntu', tool: 'codex', state: 'running', filesRead: 40, filesTotal: 100 }] } } });
  const view = usageView(p, state({ range: '90d' }), { now });
  assert.match(view.scope.find(l => l.icon === 'download').text, /Importing older usage from the logs: Ubuntu Codex 40%/);
  assert.match(view.calendar.note, /Older usage is still being imported/);
});

test('trend axis labels never overlap: 90D and All over years, at phone, folded-Duo and desktop widths', () => {
  const { xTicks, axisLabelsFit } = U;
  const WEEK = 7 * D;
  const spans = [
    ['24h', { a2: now - D, b: now, step: H }],
    ['7d', { a2: now - 7 * D, b: now, step: 4 * H }],
    ['30d', { a2: now - 30 * D, b: now, step: D }],
    ['90d', { a2: now - 90 * D, b: now, step: D }],
    ['all, two years', { a2: now - 730 * D, b: now, step: WEEK }],
    ['all, fifteen years', { a2: now - 15 * 365 * D, b: now, step: WEEK }],
  ];
  for (const [name, R] of spans) {
    for (let pw = 120; pw <= 1400; pw += 23) {
      const ticks = xTicks(R, pw);
      assert.ok(axisLabelsFit(ticks), `${name} at ${pw} px: ${ticks.map(t => `${t.label}@${Math.round(t.x)}`).join(' ')}`);
      if (pw >= 180) assert.ok(ticks.length >= 2, `${name} at ${pw} px has ${ticks.length} labels`);
    }
  }
  // the plot on an iPhone (402 px) and a folded Duo (466 px) is about 230 to 270 px: month starts at 90D
  for (const pw of [230, 270]) {
    assert.deepEqual(xTicks(spans[3][1], pw).map(t => t.label), ['Sep', 'Oct', 'Nov'], `${pw} px`);
    const years = xTicks(spans[4][1], pw).map(t => t.label);
    assert.ok(years.includes('2026') && years.length >= 3, years.join(' '));
  }
  // desktop keeps its weekly dates at 90D
  assert.match(xTicks(spans[3][1], 1188)[0].label, /^Aug \d+$/);
});

test('history notes: output that counts reasoning twice, and records dated after their read', () => {
  const p = payload({
    preset: 'all', from: Date.parse('2026-01-29T00:00:00Z'), resolution: 'day', rows: [row('2026-09-30T00:00:00Z', 'codex', 100, 1)],
    history: {
      computers: [{ host: 'ubuntu', oldestAt: '2026-01-29T00:00:00Z', lastReadAt: new Date(now).toISOString(), gaps: [] }],
      sources: [{ host: 'ubuntu', tool: 'codex', futureDated: 3 }, { host: 'mac', tool: 'claude', futureDated: null }],
      backfill: { running: false, sources: [] },
      quality: [{ tool: 'codex', flags: ['outputIncludesReasoningTwice'], from: '2026-01-29T10:00:00Z', to: '2026-10-01T22:00:00Z' }],
    },
  });
  const view = usageView(p, state({ range: 'all' }), { now });
  const lines = view.scope.map(l => l.text);
  assert.ok(lines.includes('Codex output before Oct 2 counts reasoning twice (old cache). Input and cached input are exact; those hours are left out of Cost by model.'), lines.join('\n'));
  assert.match(view.cbm.note, /Codex output before Oct 2 counts reasoning twice \(old cache\); those hours are left out here\./);
  assert.ok(lines.some(l => /^3 usage records are dated after they were read and not counted: Ubuntu Codex 3\./.test(l)), lines.join('\n'));
  // nothing flagged in range: no note
  const clean = usageView(payload({ history: history([]) }), state({ range: '90d' }), { now });
  assert.ok(!clean.scope.some(l => /reasoning twice|dated after/.test(l.text)));
  assert.equal(U.doubleReasoningNote({ history: null }), '');
});

test('range labels carry the year when the range crosses a year or is not in this one', () => {
  const { dateLabel } = U;
  const t = s => Date.parse(s);
  const R = (a, b, nowAt = b) => ({ a2: t(a), b: t(b), now: t(nowAt) });
  // this year: no year
  assert.equal(dateLabel(R('2026-11-06T12:00:00Z', '2026-11-13T12:00:00Z')), 'Nov 6 to Nov 13');
  // the year edge: Dec 31 to Jan 1, read on Jan 1
  assert.equal(dateLabel(R('2026-12-31T00:00:00Z', '2027-01-01T12:00:00Z')), 'Dec 31, 2026 to Jan 1, 2027');
  // a week that ends on Dec 31, read on Dec 31: one year, this one
  assert.equal(dateLabel(R('2026-12-24T23:00:00Z', '2026-12-31T23:00:00Z')), 'Dec 24 to Dec 31');
  // the same week read on Jan 1: last year's dates
  assert.equal(dateLabel(R('2026-12-24T23:00:00Z', '2026-12-31T23:00:00Z', '2027-01-01T01:00:00Z')), 'Dec 24, 2026 to Dec 31, 2026');
  // one day: the year only when it is not this one
  assert.match(dateLabel(R('2027-01-01T00:00:00Z', '2027-01-01T10:00:00Z')), /^Jan 1, \d/);
  assert.match(dateLabel(R('2026-12-31T00:00:00Z', '2026-12-31T10:00:00Z', '2027-01-01T10:00:00Z')), /^Dec 31, 2026, /);

  // All over two years: the date button, Cost by model and the weekly readout say which years
  const from = Date.parse('2024-11-12T00:00:00Z');
  const rows = [row('2024-11-12T00:00:00Z', 'codex', 100, 1), row('2025-06-02T00:00:00Z', 'codex', 50, 0.5), row('2026-11-10T00:00:00Z', 'codex', 7, 0.07)];
  const p = payload({ preset: 'all', from, rows, resolution: 'day', availableFrom: from });
  const view = usageView(p, state({ range: 'all' }), { now });
  assert.equal(view.head.date, 'Nov 12, 2024 to Nov 13, 2026');
  assert.match(view.cbm.sub, /^Nov 12, 2024 to Nov 13, 2026 · /);
  assert.ok(view.trend.buckets.some(b => /^WEEK OF MON, NOV 11, 2024$/.test(b.time)), view.trend.buckets.slice(0, 2).map(b => b.time).join(' | '));
  assert.ok(view.daily.bars.some(b => /^WEEK OF MON, JUN 2, 2025/.test(b.time)));
  // a range inside this year keeps its short labels
  assert.equal(usageView(payload({ preset: '90d' }), state({ range: '90d' }), { now }).head.date, 'Aug 15 to Nov 13');
});
