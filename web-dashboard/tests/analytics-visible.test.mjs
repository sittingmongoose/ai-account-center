import test from 'node:test';
import assert from 'node:assert/strict';

// Every provider's usage is visible: the Tokens by provider summary under the KPI row, the provider picker (every
// provider with usage in the range), every model listed in Cost by model (with a token sort), the donut's "N
// smaller models" and, by cost, its models with no arc open into their models, and the trend readout names each
// provider's tokens. Models stay the only division of the charts.
process.env.TZ = 'UTC';
const { usageView, providersView, providerChoices, activityData, pageRange, tokC, provItemWidth, provItemGap, provNoteWidth } = await import('../public/analytics-usage.mjs');
const { analyticsSlintModel } = await import('../public/analytics-data.mjs');

const now = Date.parse('2026-10-01T12:00:00Z');
const at = h => new Date(now - h * 3_600_000).toISOString().replace('.000', '');
const FALLBACK = { inputPerMillion: 3, outputPerMillion: 15, cacheCreationPerMillion: 3.75, cacheReadPerMillion: 0.3, source: 'fallback' };
const LISTED = { inputPerMillion: 1, outputPerMillion: 5, cacheCreationPerMillion: 1.25, cacheReadPerMillion: 0.1, source: 'builtin' };
const listed = (i, o, w, r) => (i * 1 + o * 5 + w * 1.25 + r * 0.1) / 1e6;
const tok = (inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens) => ({ inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens });
const FIELDS = ['inputTokens', 'outputTokens', 'cacheCreationTokens', 'cacheReadTokens', 'estimatedCostUsd', 'fallbackCostUsd'];
const add = (...rows) => Object.fromEntries(FIELDS.map(k => [k, rows.reduce((s, r) => s + (r[k] || 0), 0)]));
const tokensOf = (...rows) => rows.reduce((s, r) => s + r.inputTokens + r.outputTokens + r.cacheCreationTokens + r.cacheReadTokens, 0);
const priced = (model, provider, tools, i, o, w, r) => ({ model, provider, tools, ...tok(i, o, w, r), estimatedCostUsd: listed(i, o, w, r), fallbackCostUsd: 0, costByType: { input: i / 1e6, output: o * 5 / 1e6, cacheWrite: w * 1.25 / 1e6, cacheRead: r * 0.1 / 1e6 }, costByTypeReconciled: true, rates: LISTED });
// no logged cost and no listed rate: the whole estimate is the server's fallback guess (not logged)
const unlogged = (model, provider, tools, i, o, r) => ({ model, provider, tools, ...tok(i, o, 0, r), estimatedCostUsd: (i * 3 + o * 15 + r * 0.3) / 1e6, fallbackCostUsd: (i * 3 + o * 15 + r * 0.3) / 1e6, costByType: null, costByTypeReconciled: false, rates: FALLBACK });
// a cost the route logged itself (OMP), with no listed rate and nothing at the fallback
const logged = (model, provider, cost, i, o, r) => ({ model, provider, tools: ['omp'], ...tok(i, o, 0, r), estimatedCostUsd: cost, fallbackCostUsd: 0, costByType: null, costByTypeReconciled: false, rates: FALLBACK });

const M = {
  opus: priced('claude-opus-fixture', 'claude', ['claude'], 4e6, 8e5, 6e7, 9e9),
  gpt: priced('gpt-fixture', 'codex', ['codex'], 3e7, 1e6, 0, 3e9),
  deepseek: logged('deepseek-v4.1-flash', 'opencode-go', 16.87, 1.5e8, 1e6, 1.2e9),
  qwen: unlogged('qwen3.8-flash', 'qwen', ['omp'], 1.2e7, 1.2e6, 6.2e8),
  spark: logged('muse-spark-1.3-contributor', 'muse', 0.71, 3e6, 2e5, 2.2e8),
  sparkCli: unlogged('muse-spark-1.3-contributor', 'muse', ['muse'], 8e7, 9e6, 2.4e9),
  glm: unlogged('GLM-5.3-Flash', 'zai', ['zcode'], 4e7, 5e6, 8.2e8),
};
// the server publishes one row per provider and model: Muse Code's two logs are one row
const SPARK = { ...M.spark, ...add(M.spark, M.sparkCli), tools: ['omp', 'muse'], costByType: null, costByTypeReconciled: false, rates: FALLBACK };
const MODELS = [M.opus, M.gpt, M.deepseek, M.qwen, SPARK, M.glm];
const strip = ({ model, provider, tools, rates, costByType, costByTypeReconciled, ...rest }) => rest;
const hour = (h, provider, ...models) => ({ hour: at(h), provider, ...add(...models.map(strip)), costByType: null });
const HOURS = [
  hour(30, 'claude', M.opus),
  hour(29, 'codex', M.gpt),
  hour(5, 'opencode-go', M.deepseek),
  hour(5, 'qwen', M.qwen),
  hour(5, 'muse', M.spark, M.sparkCli),
  hour(3, 'zai', M.glm),
];
const provider = (id, label, tools, rows) => ({ provider: id, label, totals: add(...rows), sessionCount: 2, usageEvents: 9, tools });
const PROVIDERS = [provider('claude', 'Claude', ['claude'], [M.opus]), provider('codex', 'Codex', ['codex'], [M.gpt]), provider('muse', 'Muse Code', ['omp', 'muse'], [SPARK]),
  provider('qwen', 'Qwen token plan', ['omp'], [M.qwen]), provider('zai', 'Z.ai coding plan', ['zcode'], [M.glm]), provider('opencode-go', 'OpenCode Go', ['omp'], [M.deepseek])];
const payload = ({ models = MODELS, hours = HOURS, providers = PROVIDERS, ...activity } = {}) => ({
  schemaVersion: 1,
  range: { preset: '7d', from: '2026-09-24T12:00:00Z', to: '2026-10-01T12:00:00Z', bucketMinutes: 60 },
  providers: [{ provider: 'qwen', label: 'Qwen Token Plan' }, { provider: 'zai', label: 'Z.ai Coding Plan' }],
  activity: {
    status: 'ok', scope: 'multi-host-cli', fetchedAt: '2026-10-01T11:30:00Z', message: '',
    totals: add(...models), byHour: hours, providers, models, sessions: { total: 9, sample: [], truncated: true }, sources: [],
    ...activity,
  },
});
const state = (o = {}) => ({ range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', ...o });
const fmt = n => new Intl.NumberFormat().format(n);
const exact = { claude: tokensOf(M.opus), codex: tokensOf(M.gpt), muse: tokensOf(SPARK), qwen: tokensOf(M.qwen), zai: tokensOf(M.glm), 'opencode-go': tokensOf(M.deepseek) };

test('Tokens by provider: each provider\'s tokens in range, largest first, adding up to Total tokens', () => {
  const view = usageView(payload(), state(), { now });
  assert.equal(view.providers.shown, true);
  assert.equal(view.providers.label, 'TOKENS BY PROVIDER');
  assert.equal(view.providers.note, 'in this range');
  const order = Object.entries(exact).sort((a, b) => b[1] - a[1]).map(([k]) => k);
  assert.deepEqual(view.providers.items.map(i => i.key), order);
  for (const it of view.providers.items) {
    assert.equal(it.value, tokC(exact[it.key]), it.key);
    assert.equal(it.mark, it.key);
    assert.ok(it.tip.startsWith(`${it.label}: ${fmt(exact[it.key])} tokens, `), it.tip);
  }
  assert.equal(view.providers.items.find(i => i.key === 'zai').label, 'Z.ai Coding Plan');
  assert.equal(Object.values(exact).reduce((s, n) => s + n, 0), view.kpis.find(k => k.key === 'tok').num);
  assert.match(view.providers.items.find(i => i.key === 'opencode-go').tip, /\$16\.87 estimated cost\. From OMP logs\.$/);
  // it follows the range (24H: Claude and Codex are out) and the filter
  assert.deepEqual(usageView(payload(), state({ range: '24h' }), { now }).providers.items.map(i => i.key).includes('claude'), false);
  assert.deepEqual(usageView(payload(), state({ prov: 'zai' }), { now }).providers.items.map(i => i.key), ['zai']);
  assert.equal(usageView(payload({ status: 'unavailable' }), state(), { now }).providers.shown, false);
});

test('Tokens by provider packs into lines that fit the page, Other always last', () => {
  const A = activityData(payload(), now);
  const rows = A.hours;
  const wide = providersView(A, rows, state(), 2400);
  assert.equal(wide.lines.length, 1);
  assert.deepEqual([wide.lines[0].first, wide.lines[0].last], [true, true]);
  const narrow = providersView(A, rows, state(), 700);
  assert.ok(narrow.lines.length > 1);
  assert.deepEqual(narrow.lines.flatMap(l => l.items.map(i => i.key)), wide.items.map(i => i.key));
  assert.deepEqual(narrow.lines.map(l => [l.first, l.last]), narrow.lines.map((_, i) => [i === 0, i === narrow.lines.length - 1]));
  const withOther = payload({ models: [...MODELS, unlogged('qwen3.8-27b', 'other', ['omp'], 9e8, 1e8, 9e9)], hours: [...HOURS, hour(4, 'other', unlogged('qwen3.8-27b', 'other', ['omp'], 9e8, 1e8, 9e9))], providers: [...PROVIDERS, provider('other', 'Other', ['omp'], [])] });
  const keys = usageView(withOther, state(), { now }).providers.items.map(i => i.key);
  assert.equal(keys.at(-1), 'other');
  assert.equal(usageView(withOther, state(), { now }).providers.items.at(-1).mark, '');
});

test('Tokens by provider: phone widths leave room for the trailing note on the last line', () => {
  const A = activityData(payload(), now);
  const rows = A.hours;
  // Below-D content widths: 320/360 P, 669 F, 1024 T (window minus gutters). The Slint lines use
  // the full width minus 2 px side padding, and the note ends the last line after its gap.
  for (const cw of [296, 336, 629, 976]) {
    const v = providersView(A, rows, state(), cw);
    assert.ok(v.lines.length > 0);
    for (const [i, l] of v.lines.entries()) {
      const width = l.items.reduce((sum, it, k) => sum + (k ? provItemGap : 0) + provItemWidth(it), 0);
      const tail = l.last ? provItemGap + provNoteWidth : 0;
      assert.ok(width + tail <= cw - 4, `content ${cw}: line ${i} needs ${width + tail}`);
    }
  }
});

test('the provider picker lists every provider with usage in the range, in the dashboard\'s order', () => {
  const view = usageView(payload(), state(), { now });
  assert.deepEqual(view.picker.items.map(i => i.value), ['all', 'claude', 'codex', 'muse', 'qwen', 'zai', 'opencode-go']);
  assert.deepEqual(view.picker.items.map(i => i.label), ['All providers', 'Claude', 'Codex', 'Muse Code', 'Qwen Token Plan', 'Z.ai Coding Plan', 'OpenCode Go']);
  assert.deepEqual(view.picker.items.map(i => i.mark), ['', 'claude', 'codex', 'muse', 'qwen', 'zai', 'opencode-go']);
  assert.equal(view.picker.items[0].tokens, tokC(Object.values(exact).reduce((s, n) => s + n, 0)));
  assert.equal(view.picker.items.find(i => i.value === 'zai').tokens, tokC(exact.zai));
  assert.deepEqual([view.picker.label, view.picker.mark], ['All providers', '']);
  // 24H: only the providers with usage in it; a pick with none in range stays, marked off, so it can say so
  const day = usageView(payload(), state({ range: '24h', prov: 'claude' }), { now });
  assert.deepEqual(day.picker.items.map(i => [i.value, i.off]), [['all', false], ['claude', true], ['muse', false], ['qwen', false], ['zai', false], ['opencode-go', false]]);
  assert.deepEqual([day.picker.label, day.picker.mark], ['Claude', 'claude']);
  assert.match(day.trend.emptyText, /^No Claude activity was logged in this range\.$/);
  // the picker's value narrows every view; the page state echoes it
  const zai = usageView(payload(), state({ prov: 'zai' }), { now });
  assert.equal(zai.kpis.find(k => k.key === 'tok').num, exact.zai);
  assert.deepEqual(zai.cbm.rows.map(r => r.name), ['GLM-5.3-Flash']);
  assert.ok(zai.donut.segs.every(s => s.name === 'GLM-5.3-Flash'));
  // its cost is not logged, so the cost heatmap has no value to draw; its tokens heatmap does
  assert.ok(zai.heat.cells.every(c => c.state !== 2));
  assert.ok(usageView(payload(), state({ prov: 'zai', heat: 'tokens' }), { now }).heat.cells.some(c => c.state === 2));
  const A = activityData(payload(), now);
  assert.equal(providerChoices(A, pageRange(state(), A, now), state()).items.length, 7);
});

test('Cost by model lists every model with usage, past thirty, and a not-logged one shows tokens and "Not logged"', () => {
  const many = Array.from({ length: 34 }, (_, k) => priced(`claude-extra-${String(k).padStart(2, '0')}`, 'claude', ['claude'], 1e5 * (k + 1), 1e4, 0, 1e6));
  const models = [...MODELS, ...many];
  const view = usageView(payload({ models }), state(), { now });
  const names = new Set(models.map(m => m.model));
  assert.equal(view.cbm.rows.length, names.size);
  assert.deepEqual(new Set(view.cbm.rows.map(r => r.name)), names);
  assert.match(view.cbm.sub, new RegExp(` · ${names.size} models by cost · select one for detail$`));
  for (const name of ['qwen3.8-flash', 'GLM-5.3-Flash']) {
    const row = view.cbm.rows.find(r => r.name === name);
    assert.equal(row.cost, 'Not logged');
    assert.equal(row.costNa, true);
    assert.equal(row.tok, tokC(tokensOf(models.find(m => m.model === name))));
  }
  assert.deepEqual(view.cbm.rows.slice(-2).map(r => r.name), ['GLM-5.3-Flash', 'qwen3.8-flash']);
});

test('Cost by model sorts by cost or by tokens; the donut\'s model indexes follow the same order', () => {
  const byCost = usageView(payload(), state(), { now });
  const byTokens = usageView(payload(), state({ cbmSort: 'tokens', donutOpen: ['_other'] }), { now });
  assert.equal(byCost.cbm.sort, 'cost');
  assert.equal(byTokens.cbm.sort, 'tokens');
  assert.match(byTokens.cbm.sub, / · 6 models by tokens · /);
  assert.deepEqual(byTokens.cbm.rows.map(r => r.name), ['claude-opus-fixture', 'gpt-fixture', 'muse-spark-1.3-contributor', 'deepseek-v4.1-flash', 'GLM-5.3-Flash', 'qwen3.8-flash']);
  assert.deepEqual(byCost.cbm.rows.slice(-2).map(r => r.cost), ['Not logged', 'Not logged']);
  for (const view of [byCost, byTokens])
    for (const l of view.donut.legend) if (l.seg.model) assert.equal(view.cbm.rows[l.seg.idx].name, l.seg.name);
  const slint = analyticsSlintModel({}, { usage: byTokens, quota: {}, agenda: {}, state: state({ cbmSort: 'tokens' }) });
  assert.equal(slint.state.cbmSort, 'tokens');
  assert.equal(analyticsSlintModel({}, { usage: byCost, quota: {}, agenda: {}, state: state({ cbmSort: 'bogus' }) }).state.cbmSort, 'cost');
});

test('the donut\'s "N smaller models" opens into its models, so no model is only inside a group', () => {
  const small = { models: [...MODELS, logged('kimi-for-coding', 'kimi-code', 0.01, 19_438, 1, 0), priced('claude-haiku-fixture', 'claude', ['claude'], 4e5, 2e4, 0, 3e6), priced('gpt-mini-fixture', 'codex', ['codex'], 9e5, 1e4, 0, 1e7)] };
  const closed = usageView(payload(small), state(), { now });
  const group = closed.donut.legend.find(l => l.kind === 'group');
  assert.equal(group.seg.key, '_other');
  assert.equal(group.open, false);
  assert.ok(!closed.donut.legend.some(l => l.kind === 'child'));
  assert.equal(Number(/^(\d+) smaller models$/.exec(group.seg.name)[1]), 3);
  const open = usageView(payload(small), state({ donutOpen: new Set(['_other']) }), { now });
  const g = open.donut.legend.findIndex(l => l.kind === 'group');
  const children = open.donut.legend.slice(g + 1).filter(l => l.kind === 'child');
  assert.equal(children.length, 3);
  for (const c of children) {
    assert.equal(c.arc, open.donut.legend[g].arc);
    assert.match(c.seg.share, /^(<0\.1|\d+(\.\d)?)$/);
    assert.equal(open.cbm.rows[c.seg.idx].name, c.seg.name);
  }
  const named = new Set(open.donut.legend.filter(l => l.kind !== 'group').map(l => l.seg.name));
  for (const row of open.cbm.rows) assert.ok(named.has(row.name), row.name);
  assert.deepEqual(open.donut.segs, closed.donut.segs);
});

test('by cost, the models with no arc (not logged, or $0.00) are a legend group of their own', () => {
  const view = usageView(payload(), state({ donut: 'cost' }), { now });
  assert.ok(!view.donut.segs.some(s => ['qwen3.8-flash', 'GLM-5.3-Flash'].includes(s.name)));
  const group = view.donut.legend.find(l => l.seg.key === '_undrawn');
  assert.equal(group.seg.name, '2 models not drawn');
  assert.match(group.seg.tip, /their cost is not logged/);
  assert.deepEqual([group.seg.value, group.seg.share, group.arc, group.open], ['Not logged', '', -1, false]);
  const open = usageView(payload(), state({ donut: 'cost', donutOpen: ['_undrawn'] }), { now });
  const kids = open.donut.legend.filter(l => l.kind === 'child' && l.arc === -1);
  assert.deepEqual(kids.map(k => [k.seg.name, k.seg.value]), [['GLM-5.3-Flash', 'Not logged'], ['qwen3.8-flash', 'Not logged']]);
  assert.equal(kids[0].seg.valueTip, `${fmt(tokensOf(M.glm))} tokens`);
  assert.ok(!usageView(payload(), state({ donutOpen: ['_undrawn'] }), { now }).donut.legend.some(l => l.seg.key === '_undrawn'));
});

test('the trend readout names each provider\'s tokens in a bucket while other providers are in range', () => {
  const view = usageView(payload(), state({ range: '24h' }), { now });
  const b = view.trend.buckets.find(x => x.byProvider !== '');
  assert.equal(b.byProvider, `Muse Code ${tokC(exact.muse)} · Qwen Token Plan ${tokC(exact.qwen)} · OpenCode Go ${tokC(exact['opencode-go'])}`);
  assert.equal(b.tall, `${fmt(exact.muse + exact.qwen + exact['opencode-go'])} tokens`);
  assert.ok(usageView(payload(), state({ range: '24h', prov: 'qwen' }), { now }).trend.buckets.every(x => x.byProvider === ''));
  const only = usageView(payload({ models: [M.opus, M.gpt], hours: HOURS.slice(0, 2), providers: PROVIDERS.slice(0, 2) }), state(), { now });
  assert.ok(only.trend.buckets.every(x => x.byProvider === ''));
  assert.ok(only.trend.buckets.some(x => /^Claude \$/.test(x.foot)));
});
