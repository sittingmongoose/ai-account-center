import test from 'node:test';
import assert from 'node:assert/strict';

// Local time is the page's time; pin it so day and hour buckets are deterministic.
process.env.TZ = 'UTC';
const { setDisplayTimeZone } = await import('../public/time-format.mjs');
setDisplayTimeZone('UTC');
const U = await import('../public/analytics-usage.mjs');
const { usageView, usageHead, apiRangeFor, activityData, trendPaths, mixGeo, dashedRect } = U;

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
    providers: [{ provider: 'claude', label: 'Claude Code logs', totals: totals(claudeRows), usageEvents: 30, sessionCount: 3, tools: ['claude'] },
      { provider: 'codex', label: 'Codex logs', totals: totals(codexRows), usageEvents: 10, sessionCount: 2, tools: ['codex'] }],
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
  assert.equal(view.sessions.stats.find(s => s.key === 'avg').text, U.money(totals(hours).estimatedCostUsd / 5));
  const p = payload();
  p.activity.providers[1] = { ...p.activity.providers[1], sessionCount: null };
  const missing = usageView(p, state(), { now });
  assert.equal(missing.sessions.stats.find(s => s.key === 'sess').has, false);
  // the average still comes from the rows that have counts and costs: one provider without them
  // leaves the others priced, so it is a plain average over what is there
  assert.equal(missing.sessions.stats.find(s => s.key === 'avg').has, true);
  assert.ok(!/partial/.test(missing.sessions.stats.find(s => s.key === 'avg').label));
});

test('the average session cost uses priced rows and says partial when some rows lack costs', () => {
  const p = payload();
  // codex costs unlogged: the fallback swallows the estimate
  const cx = p.activity.providers[1].totals;
  p.activity.providers[1] = { ...p.activity.providers[1], totals: { ...cx, fallbackCostUsd: cx.estimatedCostUsd } };
  const view = usageView(p, state(), { now });
  const avg = view.sessions.stats.find(s => s.key === 'avg');
  const cl = p.activity.providers[0];
  assert.equal(avg.has, true);
  assert.equal(avg.num, cl.totals.estimatedCostUsd / cl.sessionCount);
  assert.match(avg.label, /partial/);
  // no priced row at all: still Not logged, with no partial claim
  const q = payload();
  q.activity.providers = q.activity.providers.map(pr => ({ ...pr, totals: { ...pr.totals, fallbackCostUsd: pr.totals.estimatedCostUsd } }));
  const none = usageView(q, state(), { now }).sessions.stats.find(s => s.key === 'avg');
  assert.equal(none.has, false);
  assert.equal(none.text, 'Not logged');
  assert.ok(!/partial/.test(none.label));
  // every row priced: a plain average with no partial claim
  const plain = usageView(payload(), state(), { now }).sessions.stats.find(s => s.key === 'avg');
  assert.equal(plain.has, true);
  assert.ok(!/partial/.test(plain.label));
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

test('the session table lists the sample most recent first: five in Session stats, twenty more in Recent sessions', () => {
  // the server's AccountAnalyticsSessionRow shape: provider, lastActivity, string models, token totals
  const row = (key, provider, lastMs, models, tok, est, fallback = 0) => ({
    key, provider, lastActivity: new Date(lastMs).toISOString(), models, target: provider,
    inputTokens: tok[0], outputTokens: tok[1], cacheCreationTokens: tok[2], cacheReadTokens: tok[3],
    estimatedCostUsd: est, fallbackCostUsd: fallback,
  });
  const sample = [
    row('a1', 'codex', now - 5 * 60e3, ['gpt-5'], [9000, 3000, 0, 0], 0.5),
    row('b2', 'claude', now - 2 * 3600e3, ['mystery'], [2000, 1000, 0, 0], 0.03, 0.03),
    row('c3', 'claude', now - 26 * 3600e3, ['claude-haiku-4-5', 'claude-opus-5-5'], [500, 300, 0, 0], 0.02),
  ];
  const p = payload({}, { sessions: { total: 5, sample, truncated: false } });
  const view = usageView(p, state(), { now });
  assert.deepEqual(view.sessions.recent.map(r => r.tool), ['codex', 'claude', 'claude']);
  assert.equal(view.sessions.recent[0].models, 'gpt-5');
  assert.equal(view.sessions.recent[0].cost, '$0.50');
  assert.equal(view.sessions.recent[1].cost, 'Not logged');
  assert.equal(view.sessions.recent[2].models, 'claude-haiku-4-5, claude-opus-5-5');
  assert.match(view.sessions.recent[0].tip, /gpt-5 · 12,000 tokens/);
  assert.ok(!JSON.stringify([view.sessions.recent, view.sessions.recentMore]).includes('/home/'), 'no paths leak into the list');
  assert.equal(view.sessions.foot, '');
  // fewer sessions than the Session stats table holds: the continuation stays empty and says where it comes from
  assert.deepEqual(view.sessions.recentMore, []);
  assert.equal(view.sessions.moreSub, 'Continued from Session stats');
  // rows in any other shape are dropped, never guessed
  const bad = usageView(payload({}, { sessions: { total: 5, sample: [{ key: 'x', tool: 'codex', last: now, tokens: 5, models: [{ model: 'gpt-5' }] }], truncated: false } }), state(), { now });
  assert.deepEqual(bad.sessions.recent, []);
  assert.deepEqual(bad.sessions.recentMore, []);
  // no sessions at all: the continuation has nothing to continue, so the sub says nothing
  assert.equal(bad.sessions.moreSub, '');
  // a picked provider narrows the list to its sessions
  const codex = usageView(p, state({ prov: 'codex' }), { now });
  assert.deepEqual(codex.sessions.recent.map(r => r.tool), ['codex']);
  // a full sample fills both boxes as one continued list: five here, the next twenty there, a foot for the range
  const minute = (i, extra = {}) => ({ ...sample[0], key: `k${i}`, lastActivity: new Date(now - i * 60e3).toISOString(), ...extra });
  const many = usageView(payload({}, { sessions: { total: 1848, sample: Array.from({ length: 50 }, (_, i) => minute(i)), truncated: true } }), state(), { now });
  assert.deepEqual([many.sessions.recent.length, many.sessions.recentMore.length], [5, 20]);
  assert.equal(many.sessions.recent[4].when, '4m ago');
  assert.equal(many.sessions.recentMore[0].when, '5m ago');
  assert.equal(many.sessions.recentMore[19].when, '24m ago');
  assert.equal(many.sessions.foot, 'Most recent 25 of 1,848 sessions in this range');
  assert.equal(many.sessions.moreSub, 'Sessions 6 to 25, continued from Session stats');
  // a sample that ends mid-continuation: the sub names the actual last session and no foot is needed
  const mid = usageView(payload({}, { sessions: { total: 12, sample: Array.from({ length: 12 }, (_, i) => minute(i)), truncated: false } }), state(), { now });
  assert.equal(mid.sessions.recentMore.length, 7);
  assert.equal(mid.sessions.moreSub, 'Sessions 6 to 12, continued from Session stats');
  assert.equal(mid.sessions.foot, '');
  // exactly one session in the continuation: the sub names it in the singular
  const six = usageView(payload({}, { sessions: { total: 6, sample: Array.from({ length: 6 }, (_, i) => minute(i)), truncated: false } }), state(), { now });
  assert.equal(six.sessions.moreSub, 'Session 6, continued from Session stats');
  // exactly the table's 25 of a 26-session sample: the foot counts against the sample
  const edge = usageView(payload({}, { sessions: { total: 26, sample: Array.from({ length: 26 }, (_, i) => minute(i)), truncated: false } }), state(), { now });
  assert.equal(edge.sessions.foot, 'Most recent 25 of 26 sessions in this range');
});

test('Session stats lists one row per provider with sessions, in dashboard order', () => {
  const view = usageView(payload(), state(), { now });
  // labels are the dashboard's (a response label ending in " logs" never wins); sessions and per-session
  // events are exact, the per-session cost is the provider's logged cost over its sessions
  assert.deepEqual(view.sessions.rows.map(r => [r.provider, r.label, r.sessions, r.events]),
    [['claude', 'Claude', '3', '10'], ['codex', 'Codex', '2', '5']]);
  assert.equal(view.sessions.rows[0].per, U.money((haiku(1e6, 2e5, 1e6, 4e7) + haiku(5e5, 1e5, 0, 1e7)) / 3));
  assert.equal(view.sessions.rows[1].per, U.money(gpt5(2e6, 1e5, 0, 1e7) / 2));
  assert.equal(view.sessions.rows[0].eventsTip, '30 usage events');
  // a picked provider narrows the rows to itself
  const codex = usageView(payload(), state({ prov: 'codex' }), { now });
  assert.deepEqual(codex.sessions.rows.map(r => r.label), ['Codex']);
  // a provider whose cost is all unlogged reads Not logged per session, never $0.00
  const q = payload();
  q.activity.providers.push({ provider: 'qwen', label: 'Qwen token plan', totals: { ...totals(codexRows), estimatedCostUsd: 5, fallbackCostUsd: 5 }, usageEvents: 6, sessionCount: 1, tools: ['omp'] });
  const unk = usageView(q, state(), { now });
  assert.deepEqual(unk.sessions.rows.map(r => [r.label, r.sessions]), [['Claude', '3'], ['Codex', '2'], ['Qwen token plan', '1']]);
  assert.equal(unk.sessions.rows[2].per, 'Not logged');
  // a provider with no session count reads Unavailable; one with none at all is left out
  const r = payload();
  r.activity.providers.push({ provider: 'zai', label: 'Z.ai coding plan', totals: totals(codexRows), usageEvents: 4, sessionCount: null, tools: ['zcode'] });
  r.activity.providers.push({ provider: 'other', label: 'Other', totals: totals(codexRows), usageEvents: 0, sessionCount: 0, tools: ['omp'] });
  const mixed = usageView(r, state(), { now });
  assert.deepEqual(mixed.sessions.rows.map(x => [x.label, x.sessions, x.per, x.events]).slice(2),
    [['Z.ai coding plan', 'Unavailable', 'Unavailable', 'Unavailable']]);
});

test('a muse-spark model served through several routes keeps Muse mark', () => {
  const part = (model, provider, cost) => ({ model, provider, inputTokens: 1000, outputTokens: 500, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: cost });
  const p = payload();
  p.activity.models.push(
    part('muse-spark-1.3-contributor', 'muse', 0.1),
    part('muse-spark-1.3-contributor', 'opencode-go', 0.2),
    part('shared-model', 'muse', 0.05),
    part('shared-model', 'opencode-go', 0.05),
  );
  const view = usageView(p, state(), { now });
  assert.equal(view.cbm.rows.find(r => r.name === 'muse-spark-1.3-contributor').provider, 'muse');
  // a model no one provider owns alone still has no mark
  assert.equal(view.cbm.rows.find(r => r.name === 'shared-model').provider, '');
  // every single-provider model row carries its provider's mark
  for (const row of view.cbm.rows.filter(r => r.name !== 'shared-model')) assert.ok(row.provider, `${row.name} has a mark`);
});

test('usageHead is the analytics header alone and always equals the full view head', () => {
  const refreshing = payload({}, { status: 'cached', refreshing: true });
  const cases = [
    [payload(), state()],
    [payload(), state({ range: '24h' })],
    [payload(), state({ range: 'custom', from: Date.parse('2026-09-28T00:00:00Z'), to: Date.parse('2026-10-01T00:00:00Z') })],
    [payload(), state({ prov: 'codex' })],
    [refreshing, state()],
    [payload({ activity: { status: 'unavailable', message: 'No logs.' } }), state()],
  ];
  for (const [p, st] of cases) {
    assert.deepEqual(usageHead(p, st, { now }), usageView(p, st, { now }).head);
  }
  // the tick reads "logs read …" and the refreshing state, never the blocks
  const head = usageHead(refreshing, state(), { now });
  assert.equal(head.refreshing, true);
  assert.match(head.readTip, /scan is refreshing/);
  assert.ok(!('kpis' in head) && !('trend' in head));
});

test('the header names the remote hosts a refresh waits on', () => {
  const both = payload({}, { status: 'cached', refreshing: true, refreshingRemote: ['windows', 'mac'] });
  assert.equal(usageHead(both, state(), { now }).refreshNote, '· Refreshing Mac and Windows…');
  const one = payload({}, { status: 'cached', refreshing: true, refreshingRemote: ['windows'] });
  assert.equal(usageHead(one, state(), { now }).refreshNote, '· Refreshing Windows…');
  // settled or host-less answers carry no note; the page falls back to its plain line
  assert.equal(usageHead(payload(), state(), { now }).refreshNote, '');
  const junk = payload({}, { refreshingRemote: ['mac', 'mac', 'mars', null] });
  assert.equal(usageHead(junk, state(), { now }).refreshNote, '· Refreshing Mac…');
  assert.equal(usageView(both, state(), { now }).head.refreshNote, '· Refreshing Mac and Windows…');
});
