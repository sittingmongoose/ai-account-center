import test from 'node:test';
import assert from 'node:assert/strict';

// Local time is the page's time; pin it so day and hour buckets are deterministic.
process.env.TZ = 'UTC';
const U = await import('../public/analytics-usage.mjs');
const { usageView, apiRangeFor, activityData, trendPaths, mixGeo, dashedRect } = U;

const now = Date.parse('2026-10-01T12:00:00Z');
const hour = (iso, provider, inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens, estimatedCostUsd) =>
  ({ hour: iso, provider, inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens, estimatedCostUsd });
// Claude Haiku 4.5 is priced 1 / 5 / 1.25 / 0.1 and GPT-5 1.25 / 10 / 0 / 0.125 per million (model-pricing.ts).
const haiku = (i, o, w, r) => i * 1 / 1e6 + o * 5 / 1e6 + w * 1.25 / 1e6 + r * 0.1 / 1e6;
const gpt5 = (i, o, w, r) => i * 1.25 / 1e6 + o * 10 / 1e6 + w * 0 + r * 0.125 / 1e6;
const hours = [
  hour('2026-09-30T10:00:00Z', 'claude', 1e6, 2e5, 1e6, 4e7, haiku(1e6, 2e5, 1e6, 4e7)),
  hour('2026-09-30T11:00:00Z', 'codex', 2e6, 1e5, 0, 1e7, gpt5(2e6, 1e5, 0, 1e7)),
  hour('2026-10-01T09:00:00Z', 'claude', 5e5, 1e5, 0, 1e7, haiku(5e5, 1e5, 0, 1e7)),
];
const sum = (rows, k) => rows.reduce((s, r) => s + r[k], 0);
const claudeRows = hours.filter(r => r.provider === 'claude'), codexRows = hours.filter(r => r.provider === 'codex');
const modelRow = (model, provider, rows) => ({ model, provider, inputTokens: sum(rows, 'inputTokens'), outputTokens: sum(rows, 'outputTokens'), cacheCreationTokens: sum(rows, 'cacheCreationTokens'), cacheReadTokens: sum(rows, 'cacheReadTokens'), estimatedCostUsd: sum(rows, 'estimatedCostUsd') });
const totals = rows => ({ inputTokens: sum(rows, 'inputTokens'), outputTokens: sum(rows, 'outputTokens'), cacheCreationTokens: sum(rows, 'cacheCreationTokens'), cacheReadTokens: sum(rows, 'cacheReadTokens'), estimatedCostUsd: sum(rows, 'estimatedCostUsd') });
const payload = (overrides = {}, activity = {}) => ({
  schemaVersion: 1,
  range: { preset: '7d', from: '2026-09-24T12:00:00Z', to: '2026-10-01T12:00:00Z', bucketMinutes: 60 },
  activity: {
    status: 'ok', scope: 'ubuntu-local-cli', fetchedAt: '2026-10-01T11:30:00Z', message: '',
    totals: totals(hours), byHour: hours,
    providers: [{ provider: 'claude', label: 'Claude Code logs', totals: totals(claudeRows), usageEvents: 30, sessionCount: 3 },
      { provider: 'codex', label: 'Codex logs', totals: totals(codexRows), usageEvents: 10, sessionCount: 2 }],
    models: [modelRow('claude-haiku-4-5', 'claude', claudeRows), modelRow('gpt-5', 'codex', codexRows)],
    ...activity,
  },
  ...overrides,
});
const state = (overrides = {}) => ({ range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', ...overrides });
const kpi = (view, key) => view.kpis.find(k => k.key === key);

test('the KPI row totals the logged activity and splits cost by type at each model rate, adding up exactly', () => {
  const view = usageView(payload(), state(), { now });
  const all = totals(hours);
  assert.equal(kpi(view, 'tok').num, all.inputTokens + all.outputTokens + all.cacheCreationTokens + all.cacheReadTokens);
  assert.equal(kpi(view, 'cost').num, all.estimatedCostUsd);
  // whole log window, every model reconciled: the split is exact, so no apportioning note
  assert.equal(view.kpis.some(k => k.apport), false);
  const expectedInput = sum(claudeRows, 'inputTokens') * 1 / 1e6 + sum(codexRows, 'inputTokens') * 1.25 / 1e6;
  assert.ok(Math.abs(kpi(view, 'in').num - expectedInput) < 1e-9);
  const parts = view.tokens.rows.map(r => Number(r.cost.replace(/[$,]/g, '')));
  assert.ok(Math.abs(parts.reduce((a, b) => a + b, 0) - all.estimatedCostUsd) < 0.02);
  // at most two decimals in every label; compact token values with the exact count on hover
  assert.match(kpi(view, 'cost').text, /^\$\d[\d,]*\.\d{2}$/);
  assert.match(kpi(view, 'tok').tip, /tokens$/);
});

test('a model whose rates do not reconcile shows token shares, says so, and never rescales its parts', () => {
  const p = payload();
  p.activity.models[0] = { ...p.activity.models[0], estimatedCostUsd: p.activity.models[0].estimatedCostUsd * 1.5 };
  const view = usageView(p, state(), { now });
  assert.match(view.cbm.foot, /claude-haiku-4-5: rates do not reconcile/);
  assert.match(view.scope.find(l => l.icon === 'layers').text, /1 model do not reconcile/);
  assert.equal(view.kpis.find(k => k.key === 'in').apport, true);
  const row = view.cbm.rows.find(r => r.name === 'claude-haiku-4-5');
  assert.match(row.rate, /do not reconcile/);
  // token shares: each part is the type's token share of the logged estimate
  const m = p.activity.models[0], tot = m.inputTokens + m.outputTokens + m.cacheCreationTokens + m.cacheReadTokens;
  assert.ok(Math.abs(row.fcr - m.cacheReadTokens / tot) < 1e-9);
});

test('a missing cost estimate leaves every cost unavailable, never zero', () => {
  const p = payload();
  p.activity.byHour = [...hours.slice(0, 2), { ...hours[2], estimatedCostUsd: null }];
  const view = usageView(p, state(), { now });
  for (const key of ['cost', 'in', 'out']) { assert.equal(kpi(view, key).has, false); assert.equal(kpi(view, key).text, 'Unavailable'); }
  assert.equal(view.trend.costShown, false);
  assert.equal(view.cache.hasSave, false);
  assert.equal(view.cache.saveText, 'Unavailable');
  assert.ok(view.tokens.rows.every(r => r.cost === 'Unavailable'));
  assert.ok(view.daily.bars.every(b => b.h === 0));
  assert.equal(view.daily.emptyText, 'Cost is unavailable for this range.');
});

test('unavailable local activity shows unavailable blocks, not zeros', () => {
  const view = usageView(payload({}, { status: 'unavailable', totals: null, message: 'Local CLI logs are unavailable.' }), state(), { now });
  assert.equal(view.available, false);
  assert.ok(view.kpis.every(k => !k.has && k.text === 'Unavailable'));
  assert.equal(view.trend.empty, true);
  assert.equal(view.trend.emptyText, 'Local CLI logs are unavailable.');
  assert.ok(view.heat.cells.every(c => c.state === 0));
  assert.deepEqual(view.daily.bars, []);
  assert.ok(view.sessions.stats.every(s => !s.has));
  assert.deepEqual(view.cbm.rows, []);
});

test('malformed, unknown-provider and duplicate hourly rows are never counted', () => {
  const p = payload();
  // cliproxy is no provider the response or the dashboard knows; 'Bad id' is malformed
  p.activity.byHour = [...hours, hours[0], { ...hours[1], provider: 'cliproxy' }, { ...hours[1], provider: 'Bad id' }, { ...hours[1], hour: '2026-09-30T11:30:00Z' }, { ...hours[1], hour: '2026-02-30T11:00:00Z' }, { ...hours[1], inputTokens: -1 }];
  assert.equal(activityData(p, now).hours.length, hours.length);
  const view = usageView(p, state(), { now });
  const all = totals(hours);
  assert.equal(kpi(view, 'tok').num, all.inputTokens + all.outputTokens + all.cacheCreationTokens + all.cacheReadTokens);
});

test('ranges: 24H is hourly, 7D four-hourly, 30D daily, Month from the 1st, custom whole local days', () => {
  assert.match(usageView(payload(), state({ range: '24h' }), { now }).trend.sub, /^Hourly buckets/);
  assert.match(usageView(payload(), state(), { now }).trend.sub, /^4-hour buckets/);
  const d30 = usageView(payload(), state({ range: '30d' }), { now });
  assert.match(d30.trend.sub, /^Daily buckets.*logs start/);
  const month = usageView(payload(), state({ range: 'month' }), { now });
  assert.equal(month.range.a, Date.parse('2026-10-01T00:00:00Z'));
  assert.equal(kpi(month, 'tok').num, hours[2].inputTokens + hours[2].outputTokens + hours[2].cacheCreationTokens + hours[2].cacheReadTokens);
  const custom = usageView(payload(), state({ range: 'custom', from: Date.parse('2026-09-30T00:00:00Z'), to: Date.parse('2026-10-01T00:00:00Z') }), { now });
  assert.equal(custom.head.custom, true);
  assert.equal(kpi(custom, 'cost').num, hours[0].estimatedCostUsd + hours[1].estimatedCostUsd);
  // per-model rows cover the fetched window: a different range says so in one quiet line
  assert.match(custom.cbm.note, /Per-model data cover the logs read for .*, not the custom range/);
  assert.equal(usageView(payload(), state(), { now }).cbm.note, '');
  assert.equal(apiRangeFor(state({ range: 'month' }), now), '24h');
  assert.equal(apiRangeFor(state({ range: 'month' }), Date.parse('2026-10-20T12:00:00Z')), '30d');
  assert.equal(apiRangeFor(state({ range: 'all' }), now), '30d');
  assert.equal(apiRangeFor(state({ range: 'custom', from: now - 3 * 86400e3 }), now), '7d');
});

test('the provider filter narrows every usage block to that CLI', () => {
  const view = usageView(payload(), state({ prov: 'codex' }), { now });
  const c = totals(codexRows);
  assert.equal(kpi(view, 'cost').num, c.estimatedCostUsd);
  assert.deepEqual(view.cbm.rows.map(r => r.name), ['gpt-5']);
  assert.deepEqual(view.sessions.rows.map(r => r.provider), ['codex']);
  assert.equal(view.sessions.stats.find(s => s.key === 'sess').num, 2);
  assert.equal(view.daily.showClaude, false);
});

test('the heatmap keeps hours outside the logs empty and hours with nothing logged at zero', () => {
  const view = usageView(payload(), state({ range: '24h' }), { now });
  const cells = view.heat.cells;
  assert.equal(cells.length, 7 * 24);
  // Thursday Oct 1 09:00 UTC had activity; Thursday 03:00 was read and empty; Monday is outside a 24-hour range
  const at = (weekday, h) => cells[weekday * 24 + h];
  assert.equal(at(3, 9).state, 2);
  assert.equal(at(3, 3).state, 1);
  assert.equal(at(0, 9).state, 0);
  assert.match(at(0, 9).tip, /outside the logs/);
});

test('session stats derive per-session figures and stay unavailable without counts', () => {
  const view = usageView(payload(), state(), { now });
  assert.equal(view.sessions.stats.find(s => s.key === 'sess').text, '5');
  assert.equal(view.sessions.stats.find(s => s.key === 'evs').text, '8');
  const p = payload();
  p.activity.providers[1] = { ...p.activity.providers[1], sessionCount: null };
  const missing = usageView(p, state(), { now });
  assert.equal(missing.sessions.stats.find(s => s.key === 'sess').has, false);
  assert.equal(missing.sessions.rows.find(r => r.provider === 'codex').sessions, 'Unavailable');
});

test('the trend has round ticks, an unread tail, a crosshair lookup and morphable paths', () => {
  const view = usageView(payload(), state(), { now, sizes: { trend: { w: 1200, h: 380 } } });
  const t = view.trend;
  assert.equal(t.pw, 1200 - 62 - 70);
  assert.ok(t.yTicks.length >= 4);
  assert.ok(t.yTicks.every(y => /^(0|[\d.]+[KMB]?)$/.test(y.left)));
  assert.ok(t.tailX > 0 && t.tailLabel.startsWith('LOGS READ'));
  assert.ok(t.lut.length > 0 && t.lut.every(i => i >= 0 && i < t.buckets.length));
  const paths = trendPaths(t.geo);
  assert.match(paths.total, /^M[\d.]+ [\d.]+L/);
  const mid = mixGeo(t.geo, t.geo, 0.5);
  assert.deepEqual(trendPaths(mid), paths);
});

test('the donut groups models under one percent and every degree maps to its segment', () => {
  const view = usageView(payload(), state(), { now });
  const segs = view.donut.segs;
  assert.ok(Math.abs(segs.at(-1).a1 - Math.PI * 2) < 1e-9);
  assert.equal(view.donut.lut.length, 360);
  assert.ok(view.donut.lut.every(i => i >= 0 && i < segs.length));
  assert.ok(segs.every(s => s.idx < view.cbm.rows.length));
  assert.match(dashedRect(40, 20), /^M/);
});

test("Claude Code's <synthetic> placeholder is excluded silently: never listed, never noted", () => {
  const synthetic = { model: '<synthetic>', provider: 'claude', inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: 0 };
  const p = payload({}, { models: [modelRow('claude-haiku-4-5', 'claude', claudeRows), modelRow('gpt-5', 'codex', codexRows), synthetic] });
  assert.ok(activityData(p, now).models.every(m => m.model !== '<synthetic>'));
  const view = usageView(p, state(), { now });
  assert.ok(view.cbm.rows.every(r => r.name !== '<synthetic>'));
  assert.equal(view.cbm.foot, '');
  assert.ok(!JSON.stringify(view).includes('<synthetic>'));
  // a zero-token model that is not the placeholder is still disclosed as left out
  const q = payload({}, { models: [modelRow('claude-haiku-4-5', 'claude', claudeRows),
    { model: 'gpt-5', provider: 'codex', inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: 0 }] });
  assert.match(usageView(q, state(), { now }).cbm.foot, /gpt-5 is left out/);
});
