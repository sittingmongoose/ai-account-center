import test from 'node:test';
import assert from 'node:assert/strict';

// Analytics history (docs/store.md section 12): the 90D range, answers keyed by range, the
// multi-month picker, weekly buckets for long ranges and gaps that are never drawn as zero.
process.env.TZ = 'UTC';
const { setDisplayTimeZone } = await import('../public/time-format.mjs');
setDisplayTimeZone('UTC');
const U = await import('../public/analytics-usage.mjs');
const P = await import('../public/analytics-page.mjs');
const { usageView, apiRangeFor, apiQueryFor, payloadViewKey, RANGES } = U;

const D = 86_400_000;
const H = 3_600_000;
const now = Date.parse('2026-11-13T12:00:00Z');
const row = (iso, provider, inputTokens, cost) => ({ hour: iso, provider, inputTokens, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: cost });
const state = (overrides = {}) => ({ range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', ...overrides });
function payload({ preset = '90d', from = now - 90 * D, rows = [], history = null, resolution = 'hour', availableFrom = Date.parse('2026-08-20T10:00:00Z'), days } = {}) {
  const tot = rows.reduce((t, r) => ({ ...t, inputTokens: t.inputTokens + r.inputTokens, estimatedCostUsd: t.estimatedCostUsd + r.estimatedCostUsd }), { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: 0 });
  return {
    schemaVersion: 1,
    range: { preset, from: new Date(from).toISOString(), to: new Date(now).toISOString(), bucketMinutes: 1440, availableFrom: new Date(availableFrom).toISOString(), resolution, ...(days ? { days } : {}) },
    activity: {
      status: 'ok', fetchedAt: new Date(now - 60_000).toISOString(), message: '', resolution, totals: tot, byHour: rows,
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
