import test from 'node:test';
import assert from 'node:assert/strict';

// Analytics with every tool's usage grouped under the dashboard provider that served it (the route each log
// records): OMP and zcode are never providers, a route no provider claims is "other". Cost that is not logged
// reads "Not logged", never $0.00, and totals that leave it out say "partial". Local time is pinned so buckets are
// deterministic.
process.env.TZ = 'UTC';
const { setDisplayTimeZone } = await import('../public/time-format.mjs');
setDisplayTimeZone('UTC');
const { usageView, activityData, includedView, notLoggedPart, modelShades, tokC } = await import('../public/analytics-usage.mjs');
const { modelRates } = await import('../public/model-rates.mjs');

const now = Date.parse('2026-10-01T12:00:00Z');
const at = h => new Date(now - h * 3_600_000).toISOString();
const FALLBACK = { inputPerMillion: 3, outputPerMillion: 15, cacheCreationPerMillion: 3.75, cacheReadPerMillion: 0.3, source: 'fallback' };
const GLM = { inputPerMillion: 0.5, outputPerMillion: 2, cacheCreationPerMillion: 0, cacheReadPerMillion: 0.05, source: 'models-dev' };
// Claude Haiku 4.5 is priced 1 / 5 / 1.25 / 0.1 and GPT-5 1.25 / 10 / 0 / 0.125 per million (model-pricing.ts).
const haiku = (i, o, w, r) => i * 1 / 1e6 + o * 5 / 1e6 + w * 1.25 / 1e6 + r * 0.1 / 1e6;
const gpt5 = (i, o, w, r) => i * 1.25 / 1e6 + o * 10 / 1e6 + r * 0.125 / 1e6;
const tok = (inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens) => ({ inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens });
const add = (...rows) => Object.fromEntries(['inputTokens', 'outputTokens', 'cacheCreationTokens', 'cacheReadTokens', 'estimatedCostUsd', 'fallbackCostUsd'].map(k => [k, rows.reduce((s, r) => s + (r[k] || 0), 0)]));

// models, as the server publishes them: one row per provider and model, with the tools whose logs hold it
const M = {
  haiku: { model: 'claude-haiku-4-5', provider: 'claude', tools: ['claude'], ...tok(1e6, 2e5, 1e6, 4e7), estimatedCostUsd: haiku(1e6, 2e5, 1e6, 4e7) },
  gpt5: { model: 'gpt-5', provider: 'codex', tools: ['codex'], ...tok(2e6, 1e5, 0, 1e7), estimatedCostUsd: gpt5(2e6, 1e5, 0, 1e7) },
  // OMP routed through OpenCode Go, which logged this cost itself: no listed rate, nothing at the fallback
  deepseek: { model: 'deepseek-v4.1-flash', provider: 'opencode-go', tools: ['omp'], ...tok(1e5, 1e4, 0, 1e6), estimatedCostUsd: 2, fallbackCostUsd: 0, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  // OMP on the Qwen token plan: no logged cost and no listed rate, so the whole estimate is the fallback guess
  qwen: { model: 'qwen3.8-max', provider: 'qwen', tools: ['omp'], ...tok(2e5, 2e4, 0, 2e6), estimatedCostUsd: 5, fallbackCostUsd: 5, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  // Muse Code: OMP through the muse-code route (logged 0.5) and the Muse CLI (none logged), one server row
  spark: { model: 'muse-spark-1.3-contributor', provider: 'muse', tools: ['omp', 'muse'], ...tok(3.01e6, 1.01e5, 0, 3.1e6), estimatedCostUsd: 10.5, fallbackCostUsd: 10, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  // zcode on the Z.ai coding plan, at a listed rate
  glm: { model: 'GLM-5.3-Flash', provider: 'zai', tools: ['zcode'], ...tok(4e6, 1e5, 0, 2e7), estimatedCostUsd: 3.2, fallbackCostUsd: 0, costByType: { input: 2, output: 0.2, cacheWrite: 0, cacheRead: 1 }, costByTypeReconciled: true, rates: GLM },
  // a local vLLM server: a route no provider claims
  local: { model: 'qwen3.8-27b', provider: 'other', tools: ['omp'], ...tok(5e5, 5e4, 0, 1e6), estimatedCostUsd: 2.5, fallbackCostUsd: 2.5, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
};
const SPARK_OMP = { ...tok(1e4, 1e3, 0, 1e5), estimatedCostUsd: 0.5, fallbackCostUsd: 0 };
const SPARK_MUSE = { ...tok(3e6, 1e5, 0, 3e6), estimatedCostUsd: 10, fallbackCostUsd: 10 };
const strip = ({ model, provider, tools, rates, costByType, costByTypeReconciled, ...rest }) => rest;
const hour = (h, provider, ...rows) => ({ hour: at(h).replace('.000', ''), provider, ...add(...rows.map(strip)), costByType: null });
const byHour = [
  hour(26, 'claude', M.haiku),         // Sep 30 10:00
  hour(25, 'codex', M.gpt5),           // Sep 30 11:00
  hour(24, 'opencode-go', M.deepseek), // Sep 30 12:00 (OMP)
  hour(24, 'qwen', M.qwen),            // Sep 30 12:00 (OMP)
  hour(24, 'muse', SPARK_OMP),         // Sep 30 12:00 (OMP, muse-code route)
  hour(23, 'muse', SPARK_MUSE),        // Sep 30 13:00 (the Muse CLI), none of it logged
  hour(22, 'other', M.local),          // Sep 30 14:00 (OMP, vllm)
  hour(3, 'zai', M.glm),               // Oct 1 09:00 (zcode)
];
const provider = (id, label, sessionCount, usageEvents, tools, rows) => ({ provider: id, label, totals: add(...rows), sessionCount, usageEvents, tools });
const SOURCES = [
  { tool: 'claude', host: 'ubuntu', state: 'ok', lastScanAt: at(1), rowCount: 30, detail: null },
  { tool: 'codex', host: 'ubuntu', state: 'ok', lastScanAt: at(1), rowCount: 10, detail: null },
  { tool: 'omp', host: 'ubuntu', state: 'ok', lastScanAt: at(1), rowCount: 40, detail: null },
  { tool: 'omp', host: 'mac', state: 'cached', lastScanAt: at(2), rowCount: 12, detail: null },
  { tool: 'omp', host: 'windows', state: 'ok', lastScanAt: at(1), rowCount: 9, detail: null },
  { tool: 'muse', host: 'ubuntu', state: 'ok', lastScanAt: at(1), rowCount: 20, detail: null },
  { tool: 'muse', host: 'mac', state: 'ok', lastScanAt: at(1), rowCount: 0, detail: null },
  { tool: 'muse', host: 'windows', state: 'not_installed', lastScanAt: null, rowCount: 0, detail: null },
  { tool: 'zcode', host: 'ubuntu', state: 'ok', lastScanAt: at(1), rowCount: 15, detail: null },
  { tool: 'zcode', host: 'mac', state: 'unavailable', lastScanAt: null, rowCount: 0, detail: 'remote scan failed' },
  { tool: 'zcode', host: 'windows', state: 'not_installed', lastScanAt: null, rowCount: 0, detail: null },
  ...['ubuntu', 'mac', 'windows'].flatMap(host => ['antigravity', 'cursor'].map(tool => ({ tool, host, state: 'unavailable', lastScanAt: null, rowCount: 0, detail: 'no local usage log: usage is kept server-side' }))),
];
const PROVIDERS = [
  provider('claude', 'Claude', 3, 30, ['claude'], [M.haiku]), provider('codex', 'Codex', 2, 10, ['codex'], [M.gpt5]),
  provider('muse', 'Muse Code', 2, 21, ['omp', 'muse'], [M.spark]), provider('qwen', 'Qwen token plan', 1, 6, ['omp'], [M.qwen]),
  provider('zai', 'Z.ai coding plan', 2, 15, ['zcode'], [M.glm]), provider('opencode-go', 'OpenCode Go', 1, 8, ['omp'], [M.deepseek]),
  provider('other', 'Other', 1, 3, ['omp'], [M.local]),
];
const payload = (activity = {}, top = {}) => ({
  schemaVersion: 1,
  range: { preset: '7d', from: '2026-09-24T12:00:00Z', to: '2026-10-01T12:00:00Z', bucketMinutes: 60 },
  // the dashboard's provider table: its labels win
  providers: [{ provider: 'qwen', label: 'Qwen Token Plan' }, { provider: 'zai', label: 'Z.ai Coding Plan' }],
  ...top,
  activity: {
    status: 'ok', scope: 'multi-host-cli', fetchedAt: '2026-10-01T11:30:00Z', message: '',
    totals: add(...Object.values(M)), byHour, providers: PROVIDERS, models: Object.values(M),
    sessions: { total: 10, sample: [], truncated: true }, sources: SOURCES,
    ...activity,
  },
});
const state = (o = {}) => ({ range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', ...o });
const kpi = (view, key) => view.kpis.find(k => k.key === key);
const tokensOf = (...rows) => rows.reduce((s, r) => s + r.inputTokens + r.outputTokens + r.cacheCreationTokens + r.cacheReadTokens, 0);
const known = haiku(1e6, 2e5, 1e6, 4e7) + gpt5(2e6, 1e5, 0, 1e7) + 2 + 0.5 + 3.2;
const fmt = n => new Intl.NumberFormat().format(n);

/** Every `provider` value anywhere in the view, outside the Included usage disclosure. */
function providerValues(view) {
  const found = [];
  const walk = v => { if (Array.isArray(v)) v.forEach(walk); else if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) { if (k === 'provider' || k === 'p') found.push(x); walk(x); } };
  const { included, ...rest } = view;
  walk(rest);
  return found;
}

test('usage counts under the provider that served it; OMP and zcode are never providers', () => {
  for (const prov of ['all', 'claude', 'qwen', 'zai', 'muse', 'other']) {
    for (const range of ['24h', '7d']) {
      const view = usageView(payload(), state({ prov, range, donut: 'cost' }), { now });
      assert.ok(providerValues(view).every(p => ['', 'claude', 'codex', 'muse', 'qwen', 'zai', 'opencode-go', 'other'].includes(p)), `${prov} ${range}: ${providerValues(view)}`);
      assert.ok(view.picker.items.every(it => !['omp', 'zcode'].includes(it.value) && !/^(OMP|zcode)$/.test(it.label)));
      // tool names only say where usage comes from: Included usage, the scope note, a model's detail and the
      // summary's tips
      const { included, providers, scope, ...rest } = structuredClone(view);
      for (const r of rest.cbm.rows) { delete r.sub; delete r.tokTip; }
      assert.doesNotMatch(JSON.stringify(rest), /\b(OMP|zcode)\b/);
      assert.doesNotMatch(JSON.stringify(providers.items.map(({ tip, ...it }) => it)), /\b(OMP|zcode)\b/);
    }
  }
  const view = usageView(payload(), state(), { now });
  // a model keeps its provider's mark; one served by several providers, or by Other, has none
  const row = name => view.cbm.rows.find(r => r.name === name);
  assert.deepEqual([row('GLM-5.3-Flash').provider, row('qwen3.8-max').provider, row('deepseek-v4.1-flash').provider, row('qwen3.8-27b').provider], ['zai', 'qwen', 'opencode-go', '']);
  assert.match(row('GLM-5.3-Flash').sub, /^Z\.ai Coding Plan · zcode logs · /);
  assert.match(row('muse-spark-1.3-contributor').sub, /^Muse Code · OMP and Muse Code logs · /);
  assert.match(row('qwen3.8-27b').sub, /^Other · OMP logs · /);
  assert.equal(row('claude-haiku-4-5').provider, 'claude');
});

test('under All every provider merges into the totals, the model views, the trend and the heatmap', () => {
  const view = usageView(payload(), state({ range: '24h' }), { now });
  // 24H from Sep 30 12:00: the OMP, Muse and zcode hours are in; Claude and Codex (10:00, 11:00) are not
  assert.equal(kpi(view, 'tok').num, tokensOf(M.deepseek, M.qwen, M.spark, M.local, M.glm));
  const week = usageView(payload(), state(), { now });
  assert.equal(kpi(week, 'tok').num, tokensOf(...Object.values(M)));
  assert.deepEqual(week.cbm.rows.map(r => r.name).sort(), Object.values(M).map(m => m.model).sort());
  assert.equal(week.cbm.rows.find(r => r.name === 'muse-spark-1.3-contributor').tokTip, `${fmt(tokensOf(M.spark))} tokens · Muse Code · OMP and Muse Code logs`);
  assert.ok(week.donut.segs.some(s => s.name === 'GLM-5.3-Flash'));
  // the trend readout names each provider's tokens in the bucket (the cost split would not add up)
  const noon = view.trend.buckets.find(b => b.time.includes('· 12 PM TO'));
  assert.equal(noon.byProvider, `Muse Code ${tokC(tokensOf(SPARK_OMP))} · Qwen Token Plan ${tokC(tokensOf(M.qwen))} · OpenCode Go ${tokC(tokensOf(M.deepseek))}`);
  assert.ok(week.trend.buckets.every(b => !/Claude \$/.test(b.foot)));
  assert.match(week.trend.sub, /all providers$/);
  const tokensHeat = usageView(payload(), state({ range: '24h', heat: 'tokens' }), { now });
  assert.equal(tokensHeat.heat.cells[2 * 24 + 13].state, 2); // Wednesday Sep 30 13:00, the Muse CLI only
});

test('a picked provider shows only the usage it served, divided by model', () => {
  const zai = usageView(payload(), state({ prov: 'zai' }), { now });
  assert.equal(kpi(zai, 'tok').num, tokensOf(M.glm));
  assert.deepEqual(zai.cbm.rows.map(r => r.name), ['GLM-5.3-Flash']);
  assert.match(zai.trend.sub, /Z\.ai Coding Plan$/);
  assert.deepEqual(zai.providers.items.map(i => i.key), ['zai']);
  assert.equal(zai.sessions.stats.find(s => s.key === 'sess').num, 2);
  // the daily chart is that provider alone, in its own hue
  assert.deepEqual([zai.daily.showClaude, zai.daily.showCodex, zai.daily.showOther, zai.daily.otherLabel, zai.daily.otherHue], [false, false, true, 'Z.ai Coding Plan', 'zai']);
  assert.match(zai.daily.sub, /^Z\.ai Coding Plan · estimated, USD/);
  // Muse Code: the OMP route and the Muse CLI together, still one model row
  const muse = usageView(payload(), state({ prov: 'muse' }), { now });
  assert.equal(kpi(muse, 'tok').num, tokensOf(M.spark));
  assert.deepEqual(muse.cbm.rows.map(r => [r.name, r.cost, r.partial]), [['muse-spark-1.3-contributor', '$0.50', true]]);
  // Other: the routes no provider claims, cost not logged
  const other = usageView(payload(), state({ prov: 'other' }), { now });
  assert.deepEqual(other.cbm.rows.map(r => [r.name, r.cost]), [['qwen3.8-27b', 'Not logged']]);
  assert.equal(kpi(other, 'cost').text, 'Not logged');
  assert.match(other.daily.emptyText, /^No Other cost is logged in this range; the usage is in tokens above\./);
  const claude = usageView(payload(), state({ prov: 'claude' }), { now });
  assert.equal(kpi(claude, 'tok').num, tokensOf(M.haiku));
  assert.deepEqual(claude.cbm.rows.map(r => r.name), ['claude-haiku-4-5']);
  assert.equal(claude.sessions.stats.find(s => s.key === 'sess').num, 3);
  assert.match(claude.trend.sub, /Claude$/);
  assert.equal(kpi(claude, 'cost').sub.some(r => r.text === 'Partial'), false);
  // under All the Sessions number sums every provider's count (a session two providers served counts under
  // each): 12 = 3+2+2+1+2+1+1 over the seven PROVIDERS rows, so a provider dropping out would show here
  const all = usageView(payload(), state(), { now });
  assert.equal(all.sessions.stats.find(s => s.key === 'sess').num, 12);
  assert.match(all.sessions.note, /usage read from the Mac and Windows adds tokens but no sessions/);
  assert.equal(claude.sessions.note, '');
});

test('Included usage names the tools and computers read, and how usage is grouped', () => {
  const inc = usageView(payload(), state(), { now }).included;
  assert.equal(inc.shown, true);
  assert.equal(inc.line, 'Includes Claude Code, Codex, OMP, Muse Code and zcode on Ubuntu, Mac and Windows. Antigravity and Cursor don\'t keep local usage logs; their quota readings still show on Home.');
  assert.match(inc.foot, /grouped by the provider that served it \(the route each log records; a route no provider claims is under Other\)/);
  assert.equal(inc.label, 'Included usage · 1 cached, 1 unavailable');
  assert.deepEqual(inc.rows.map(r => r.tool), ['Claude Code', 'Codex', 'OMP', 'Muse Code', 'zcode']);
  const cell = (tool, i) => inc.rows.find(r => r.tool === tool).cells[i];
  assert.deepEqual([cell('OMP', 1).text, cell('OMP', 1).tone], ['Cached, 2h 0m ago', 'cached']);
  assert.deepEqual([cell('zcode', 1).text, cell('zcode', 1).tone], ['Unavailable', 'unavailable']);
  assert.equal(cell('Claude Code', 1).text, 'Not read');
  assert.ok(!JSON.stringify(inc.rows).includes('No usage log'), 'no per-host no-log rows');
  assert.equal(includedView(payload({ sources: [] }), now).shown, false);
});

test('Included usage lists generic JSONL sources with the other tools', () => {
  const sources = [
    ...SOURCES,
    { tool: 'jsonl', host: 'ubuntu', state: 'ok', lastScanAt: at(1), rowCount: 7, detail: null },
  ];
  const inc = includedView(payload({ sources }), now);
  assert.deepEqual(inc.rows.map(r => r.tool), ['Claude Code', 'Codex', 'OMP', 'Muse Code', 'zcode', 'Generic JSONL']);
  assert.match(inc.line, /Includes Claude Code, Codex, OMP, Muse Code, zcode and Generic JSONL on Ubuntu/);
  const cell = inc.rows.find(r => r.tool === 'Generic JSONL').cells[0];
  assert.deepEqual([cell.text, cell.tone], ['Read 1h 0m ago', 'ok']);
});

test('cost that is not logged reads "Not logged", never $0.00, and partial cost says so', () => {
  const view = usageView(payload(), state(), { now });
  const row = name => view.cbm.rows.find(r => r.name === name);
  assert.deepEqual([row('qwen3.8-max').cost, row('qwen3.8-max').costNa, row('qwen3.8-max').share, row('qwen3.8-max').w], ['Not logged', true, '', 0]);
  assert.ok(row('qwen3.8-max').types.every(t => t.none || t.cost === 'Not logged'));
  assert.deepEqual([row('deepseek-v4.1-flash').cost, row('deepseek-v4.1-flash').partial], ['$2.00', false]);
  assert.deepEqual([row('muse-spark-1.3-contributor').cost, row('muse-spark-1.3-contributor').partial], ['$0.50', true]);
  assert.equal(row('GLM-5.3-Flash').cost, '$3.20');
  assert.ok(view.cbm.rows.every(r => !(r.costNa && /\$/.test(r.cost))));
  const cost = kpi(view, 'cost');
  assert.ok(Math.abs(cost.num - known) < 1e-9);
  assert.equal(cost.sub[0].text, 'Partial');
  assert.match(view.trend.legend.find(l => l.key === 'cost').note, /partial/);
  // the average session cost comes from the priced rows and says partial (claude, codex, zai and
  // opencode-go have costs; muse, qwen and other do not)
  const avg = view.sessions.stats.find(s => s.key === 'avg');
  assert.ok(Math.abs(avg.num - (haiku(1e6, 2e5, 1e6, 4e7) + gpt5(2e6, 1e5, 0, 1e7) + 3.2 + 2) / 8) < 1e-9);
  assert.equal(avg.text, '$2.15');
  assert.match(avg.label, /partial/);
  // the provider summary: a not-logged provider says so in its tip, never $0.00
  assert.match(view.providers.items.find(i => i.key === 'qwen').tip, /estimated cost not logged\. From OMP logs\.$/);
  assert.match(view.providers.items.find(i => i.key === 'muse').tip, /\$0\.50 estimated cost, partial: some is not logged\. From OMP and Muse Code logs\.$/);
  // the daily readout: each provider's logged cost, "Not logged" where none is
  const daily = usageView(payload(), state({ range: '24h' }), { now }).daily;
  const one = daily.bars.find(b => b.rows.some(r => r.label === 'Muse Code' && r.value === 'Not logged'));
  assert.ok(one, JSON.stringify(daily.bars.map(b => b.rows)));
  assert.match(one.foot, /Partial: some cost is not logged/);
  const donut = usageView(payload(), state({ donut: 'cost' }), { now }).donut;
  assert.ok(donut.segs.every(s => s.name !== 'qwen3.8-max'));
  assert.equal(donut.centreLabel, 'estimated cost, partial');
});

test('a range with no logged cost at all reads "Not logged" everywhere, never $0.00', () => {
  const view = usageView(payload(), state({ range: '24h' }), { now });
  const muse = view.trend.buckets.find(b => b.tall === `${fmt(tokensOf(SPARK_MUSE))} tokens`);
  assert.equal(muse.cost, 'Not logged');
  assert.equal(muse.yCost, -1);
  assert.match(muse.foot, /^Partial: some cost here is not logged/);
  const only = payload({ byHour: [byHour[5]], models: [{ ...M.spark, ...SPARK_MUSE, tools: ['muse'] }], totals: add(SPARK_MUSE), providers: [provider('muse', 'Muse Code', 1, 20, ['muse'], [SPARK_MUSE])] });
  const v = usageView(only, state(), { now });
  for (const key of ['cost', 'in', 'out']) { assert.equal(kpi(v, key).text, 'Not logged'); assert.equal(kpi(v, key).has, false); }
  assert.ok(v.tokens.rows.every(r => r.cost === 'Not logged'));
  assert.equal(v.cache.saveText, 'Not logged');
  assert.equal(usageView(only, state({ donut: 'cost' }), { now }).donut.centre, 'Not logged');
  assert.ok(!JSON.stringify(v.kpis).includes('$0.00'));
});

test('not-logged parts come from fallbackCostUsd, for every provider including Claude and Codex', () => {
  assert.equal(notLoggedPart({ fallbackCostUsd: 2 }, 5), 2);
  assert.equal(notLoggedPart({ fallbackCostUsd: 9 }, 5), 5);
  assert.equal(notLoggedPart({ fallbackCostUsd: 0, costByType: null }, 5), 0);
  assert.equal(notLoggedPart({ costByType: null }, 5), 5);
  assert.equal(notLoggedPart({ costByType: { input: 1, output: 1, cacheWrite: 0, cacheRead: 0 } }, 5), 0);
  assert.equal(notLoggedPart({ fallbackCostUsd: 3 }, null), 0);
  // a model with no known rate (resolver source 'fallback') under Claude: "not logged", left out of the totals
  const unlisted = { model: 'claude-aac-unlisted-9', provider: 'claude', tools: ['claude'], ...tok(1e6, 1e5, 0, 0), estimatedCostUsd: 4.5, fallbackCostUsd: 4.5, costByType: null, costByTypeReconciled: false, rates: FALLBACK };
  assert.equal(notLoggedPart(unlisted, 4.5), 4.5);
  const p = payload({ totals: add(M.haiku, unlisted), byHour: [hour(26, 'claude', M.haiku), hour(25, 'claude', unlisted)], providers: [provider('claude', 'Claude', 5, 40, ['claude'], [M.haiku, unlisted])], models: [M.haiku, unlisted] });
  const view = usageView(p, state(), { now });
  assert.ok(Math.abs(kpi(view, 'cost').num - haiku(1e6, 2e5, 1e6, 4e7)) < 1e-9);
  assert.equal(kpi(view, 'cost').sub[0].text, 'Partial');
  assert.deepEqual(view.cbm.rows.find(r => r.name === 'claude-aac-unlisted-9').cost, 'Not logged');
  // rows of a provider neither the dashboard nor the response knows are dropped
  assert.equal(activityData(payload({ byHour: [{ ...byHour[0], provider: 'cliproxy' }] }), now).hours.length, 0);
});

test('Claude Opus 5.5 and Sonnet 5.5 cost at their listed rates, no longer "not logged" (MARKS-HIDPI task 2)', () => {
  // The server publishes the resolver's rates with each model; model-rates.mjs mirrors model-pricing.ts.
  const listed = model => { const r = modelRates(model); assert.equal(r.source, 'builtin'); return { inputPerMillion: r.in, outputPerMillion: r.out, cacheCreationPerMillion: r.cw, cacheReadPerMillion: r.cr, source: r.source }; };
  const row = (model, rates, t) => {
    const cost = { input: t.inputTokens * rates.inputPerMillion / 1e6, output: t.outputTokens * rates.outputPerMillion / 1e6, cacheWrite: t.cacheCreationTokens * rates.cacheCreationPerMillion / 1e6, cacheRead: t.cacheReadTokens * rates.cacheReadPerMillion / 1e6 };
    return { model, provider: 'claude', tools: ['claude'], ...t, estimatedCostUsd: cost.input + cost.output + cost.cacheWrite + cost.cacheRead, fallbackCostUsd: 0, costByType: cost, costByTypeReconciled: true, rates };
  };
  const opus = row('claude-opus-5-5', listed('claude-opus-5-5'), tok(1e6, 1e5, 2e5, 1e7));
  const sonnet = row('claude-sonnet-5-5', listed('claude-sonnet-5-5'), tok(5e5, 5e4, 0, 2e6));
  // $4 in, $20 out, $5 cache write, $0.20 cache read per million (platform.claude.com pricing, 2026-10-03)
  assert.ok(Math.abs(opus.estimatedCostUsd - (4 + 2 + 1 + 2)) < 1e-9);
  assert.ok(Math.abs(sonnet.estimatedCostUsd - (1 + 0.5 + 0 + 0.4)) < 1e-9);
  for (const r of [opus, sonnet]) assert.equal(notLoggedPart(r, r.estimatedCostUsd), 0);
  const p = payload({ totals: add(M.haiku, opus, sonnet), byHour: [hour(26, 'claude', M.haiku), hour(25, 'claude', opus, sonnet)], providers: [provider('claude', 'Claude', 5, 40, ['claude'], [M.haiku, opus, sonnet])], models: [M.haiku, opus, sonnet] });
  const view = usageView(p, state(), { now });
  assert.ok(Math.abs(kpi(view, 'cost').num - (haiku(1e6, 2e5, 1e6, 4e7) + opus.estimatedCostUsd + sonnet.estimatedCostUsd)) < 1e-9);
  assert.notEqual(kpi(view, 'cost').sub?.[0]?.text, 'Partial');
  for (const name of ['claude-opus-5-5', 'claude-sonnet-5-5']) {
    const shown = view.cbm.rows.find(r => r.name === name).cost;
    assert.notEqual(shown, 'Not logged');
    assert.match(String(shown), /\$\d/);
  }
});

test('each provider\'s models share its hue; models that are no provider\'s alone share one neutral family', () => {
  const list = Array.from({ length: 9 }, (_, i) => ({ key: `m${i}`, model: `m${i}`, provider: '', logged: 10 - i, total: 1 }))
    .concat([{ key: 'c', model: 'c', provider: 'claude', logged: 50, total: 1 }, { key: 'z1', model: 'z1', provider: 'zai', logged: 5, total: 1 }, { key: 'z2', model: 'z2', provider: 'zai', logged: 4, total: 1 }]);
  const shades = modelShades(list);
  const neutral = list.filter(m => m.provider === '').map(m => shades[m.key]);
  assert.equal(new Set(neutral).size, 9);
  for (let i = 1; i < neutral.length; i++) assert.ok(Math.abs(neutral[i] - neutral[i - 1]) > 0.2, `${neutral}`);
  assert.deepEqual([shades.c, shades.z1, shades.z2], [1, 1, 0.66]);
});

test('a listed zero rate reads "Free", never unknown: cost rows, detail, donut and totals', () => {
  // qwen3.8-27b on a local vLLM server, as the server publishes a free model: zero estimate, zero
  // fallback, a reconciled zero split and a listed (builtin) all-zero rate
  const ZERO_RATES = { inputPerMillion: 0, outputPerMillion: 0, cacheCreationPerMillion: 0, cacheReadPerMillion: 0, source: 'builtin' };
  const free = { model: 'qwen3.8-27b', provider: 'other', tools: ['omp'], ...tok(5e5, 5e4, 0, 1e6), estimatedCostUsd: 0, fallbackCostUsd: 0, costByType: { input: 0, output: 0, cacheWrite: 0, cacheRead: 0 }, costByTypeReconciled: true, rates: ZERO_RATES };
  const freeHour = { hour: at(3).replace('.000', ''), provider: 'other', ...add(strip(free)), costByType: null };
  const freeProvider = provider('other', 'Other', 1, 3, ['omp'], [free]);
  const activity = { totals: add(free), byHour: [freeHour], providers: [freeProvider], models: [free], sessions: { total: 1, sample: [], truncated: false } };
  const view = usageView(payload(activity), state({ donut: 'cost', donutOpen: ['_undrawn'] }), { now });
  const row = view.cbm.rows.find(r => r.name === 'qwen3.8-27b');
  assert.equal(row.cost, 'Free');
  assert.equal(row.partial, false);
  assert.match(row.rate, /^Free: this model has no per-token charge/);
  assert.ok(row.types.filter(t => t.tok !== 'None').every(t => t.cost === 'Free'), 'every used per-type cost reads Free');
  // the donut has no arcs to draw, but it is a free range, never an empty one
  assert.equal(view.donut.centre, 'Free');
  assert.equal(view.donut.centreLabel, 'estimated cost, free models');
  assert.equal(view.donut.empty, false);
  assert.equal(view.donut.legend.find(l => l.kind === 'child').seg.value, 'Free');
  // totals stay numeric: $0.00 is a true zero here, and nothing is partial
  assert.equal(kpi(view, 'cost').text, '$0.00');
  assert.doesNotMatch(JSON.stringify(kpi(view, 'cost').sub), /Partial/);
  assert.equal(view.trend.legend.find(l => l.key === 'cost').note, 'right axis, USD');
});
