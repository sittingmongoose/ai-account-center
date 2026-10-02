import test from 'node:test';
import assert from 'node:assert/strict';

// Analytics with the usage of OMP, Muse Code and zcode (activity.sources, fallbackCostUsd): they merge into the
// model views and the totals and never appear as providers; cost that is not logged reads "Not logged", never
// $0.00, and totals that leave it out say "partial". Local time is pinned so buckets are deterministic.
process.env.TZ = 'UTC';
const { usageView, activityData, includedView, notLoggedPart, modelShades } = await import('../public/analytics-usage.mjs');

const now = Date.parse('2026-10-01T12:00:00Z');
const at = h => new Date(now - h * 3_600_000).toISOString();
const FALLBACK = { inputPerMillion: 3, outputPerMillion: 15, cacheCreationPerMillion: 3.75, cacheReadPerMillion: 0.3, source: 'fallback' };
const GLM = { inputPerMillion: 0.5, outputPerMillion: 2, cacheCreationPerMillion: 0, cacheReadPerMillion: 0.05, source: 'models-dev' };
// Claude Haiku 4.5 is priced 1 / 5 / 1.25 / 0.1 and GPT-5 1.25 / 10 / 0 / 0.125 per million (model-pricing.ts).
const haiku = (i, o, w, r) => i * 1 / 1e6 + o * 5 / 1e6 + w * 1.25 / 1e6 + r * 0.1 / 1e6;
const gpt5 = (i, o, w, r) => i * 1.25 / 1e6 + o * 10 / 1e6 + r * 0.125 / 1e6;
const tok = (inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens) => ({ inputTokens, outputTokens, cacheCreationTokens, cacheReadTokens });
const add = (...rows) => Object.fromEntries(['inputTokens', 'outputTokens', 'cacheCreationTokens', 'cacheReadTokens', 'estimatedCostUsd', 'fallbackCostUsd'].map(k => [k, rows.reduce((s, r) => s + (r[k] || 0), 0)]));

// models, as the server publishes them (one row per tool and model; fallback rates mean no listed rate)
const M = {
  haiku: { model: 'claude-haiku-4-5', provider: 'claude', ...tok(1e6, 2e5, 1e6, 4e7), estimatedCostUsd: haiku(1e6, 2e5, 1e6, 4e7) },
  gpt5: { model: 'gpt-5', provider: 'codex', ...tok(2e6, 1e5, 0, 1e7), estimatedCostUsd: gpt5(2e6, 1e5, 0, 1e7) },
  // OMP logged this cost itself: no listed rate, nothing priced at the fallback
  deepseek: { model: 'deepseek-v4.1-flash', provider: 'omp', ...tok(1e5, 1e4, 0, 1e6), estimatedCostUsd: 2, fallbackCostUsd: 0, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  // no logged cost and no listed rate: the whole estimate is the server's fallback guess
  qwen: { model: 'qwen3.8-max', provider: 'omp', ...tok(2e5, 2e4, 0, 2e6), estimatedCostUsd: 5, fallbackCostUsd: 5, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  sparkOmp: { model: 'muse-spark-1.3-contributor', provider: 'omp', ...tok(1e4, 1e3, 0, 1e5), estimatedCostUsd: 0.5, fallbackCostUsd: 0, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  sparkMuse: { model: 'muse-spark-1.3-contributor', provider: 'muse', ...tok(3e6, 1e5, 0, 3e6), estimatedCostUsd: 10, fallbackCostUsd: 10, costByType: null, costByTypeReconciled: false, rates: FALLBACK },
  glm: { model: 'GLM-5.3-Flash', provider: 'zcode', ...tok(4e6, 1e5, 0, 2e7), estimatedCostUsd: 3.2, fallbackCostUsd: 0, costByType: { input: 2, output: 0.2, cacheWrite: 0, cacheRead: 1 }, costByTypeReconciled: true, rates: GLM },
};
const strip = ({ model, ...rest }) => rest;
const hour = (h, provider, ...models) => ({ hour: at(h).replace('.000', ''), provider, ...add(...models.map(strip)), costByType: null });
const byHour = [
  hour(26, 'claude', M.haiku),          // Sep 30 10:00
  hour(25, 'codex', M.gpt5),            // Sep 30 11:00
  hour(24, 'omp', M.deepseek, M.qwen, M.sparkOmp), // Sep 30 12:00, partly not logged
  hour(23, 'muse', M.sparkMuse),        // Sep 30 13:00, none of it logged
  hour(3, 'zcode', M.glm),              // Oct 1 09:00
];
const provider = (id, label, sessionCount, usageEvents, rows) => ({ provider: id, label, totals: add(...rows), sessionCount, usageEvents });
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
const payload = (activity = {}) => ({
  schemaVersion: 1,
  range: { preset: '7d', from: '2026-09-24T12:00:00Z', to: '2026-10-01T12:00:00Z', bucketMinutes: 60 },
  activity: {
    status: 'ok', scope: 'multi-host-cli', fetchedAt: '2026-10-01T11:30:00Z', message: '',
    totals: add(...Object.values(M)), byHour,
    providers: [
      provider('claude', 'Claude Code logs', 3, 30, [M.haiku]), provider('codex', 'Codex logs', 2, 10, [M.gpt5]),
      provider('omp', 'OMP logs', 4, 40, [M.deepseek, M.qwen, M.sparkOmp]), provider('muse', 'Muse logs', 1, 20, [M.sparkMuse]),
      provider('zcode', 'zcode logs', 2, 15, [M.glm]),
    ],
    models: Object.values(M),
    sources: SOURCES,
    ...activity,
  },
});
const state = (o = {}) => ({ range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', ...o });
const kpi = (view, key) => view.kpis.find(k => k.key === key);
const tokensOf = (...rows) => rows.reduce((s, r) => s + r.inputTokens + r.outputTokens + r.cacheCreationTokens + r.cacheReadTokens, 0);
const known = haiku(1e6, 2e5, 1e6, 4e7) + gpt5(2e6, 1e5, 0, 1e7) + 2 + 0.5 + 3.2;
const OTHER = ['omp', 'muse', 'zcode'];
const SERVER_LABELS = /OMP logs|Muse logs|zcode logs/;

/** Every `provider` value anywhere in the view, outside the Included usage disclosure. */
function providerValues(view) {
  const found = [];
  const walk = v => { if (Array.isArray(v)) v.forEach(walk); else if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) { if (k === 'provider' || k === 'p') found.push(x); walk(x); } };
  const { included, ...rest } = view;
  walk(rest);
  return found;
}

test('(1) OMP, Muse and zcode never appear as providers: no provider value, label, legend or row', () => {
  for (const prov of ['all', 'claude', 'codex']) {
    for (const range of ['24h', '7d']) {
      const view = usageView(payload(), state({ prov, range, donut: 'cost' }), { now });
      assert.ok(providerValues(view).every(p => !OTHER.includes(p)), `${prov} ${range}: ${providerValues(view)}`);
      const { included, ...rest } = view;
      assert.doesNotMatch(JSON.stringify(rest), SERVER_LABELS);
      assert.doesNotMatch(JSON.stringify(rest), /\b(OMP|Muse Code|zcode)\b/);
    }
  }
  const view = usageView(payload(), state(), { now });
  // per-provider views: the session rows and the daily chart are Claude Code and Codex only
  assert.deepEqual(view.sessions.rows.map(r => r.provider), ['claude', 'codex']);
  assert.equal(view.daily.showClaude, true);
  assert.equal(view.daily.showCodex, true);
  assert.match(view.daily.sub, /^Claude Code and Codex · estimated, USD/);
  // a model that is not one provider's alone has no mark and the neutral family ('')
  const glm = view.cbm.rows.find(r => r.name === 'GLM-5.3-Flash');
  assert.equal(glm.provider, '');
  assert.equal(glm.sub.startsWith('Included logs'), true);
  assert.equal(view.cbm.rows.find(r => r.name === 'claude-haiku-4-5').provider, 'claude');
});

test('(1) under All their usage merges into the totals, the model views, the trend and the heatmap', () => {
  const view = usageView(payload(), state({ range: '24h' }), { now });
  // 24H from Sep 30 12:00: the OMP, Muse and zcode hours are in; Claude and Codex (10:00, 11:00) are not
  assert.equal(kpi(view, 'tok').num, tokensOf(M.deepseek, M.qwen, M.sparkOmp, M.sparkMuse, M.glm));
  const week = usageView(payload(), state(), { now });
  assert.equal(kpi(week, 'tok').num, tokensOf(...Object.values(M)));
  // one row per model: muse-spark from OMP and Muse is a single row with both tools' tokens
  const names = week.cbm.rows.map(r => r.name);
  assert.equal(names.filter(n => n === 'muse-spark-1.3-contributor').length, 1);
  assert.deepEqual([...names].sort(), ['GLM-5.3-Flash', 'claude-haiku-4-5', 'deepseek-v4.1-flash', 'gpt-5', 'muse-spark-1.3-contributor', 'qwen3.8-max'].sort());
  assert.equal(week.cbm.rows.find(r => r.name === 'muse-spark-1.3-contributor').tokTip, `${new Intl.NumberFormat().format(tokensOf(M.sparkOmp, M.sparkMuse))} tokens`);
  // the token donut has every model; the trend readout and heatmap count the other tools' hours
  assert.ok(week.donut.segs.some(s => s.name === 'GLM-5.3-Flash'));
  const muse = view.trend.buckets.find(b => b.time.includes('· 1 PM TO'));
  assert.ok(muse, 'the Muse hour has a bucket');
  assert.equal(muse.tall, `${new Intl.NumberFormat().format(tokensOf(M.sparkMuse))} tokens`);
  const tokensHeat = usageView(payload(), state({ range: '24h', heat: 'tokens' }), { now });
  assert.equal(tokensHeat.heat.cells[2 * 24 + 13].state, 2); // Wednesday Sep 30 13:00, Muse only
  // the readout's Claude Code / Codex split would not add up with other logs in range, so it is left out
  assert.ok(week.trend.buckets.every(b => !/Claude Code \$/.test(b.foot)));
  assert.match(week.trend.sub, /all included logs/);
});

test('(1) a picked provider shows only its own logs; the other tools count under All only', () => {
  const claude = usageView(payload(), state({ prov: 'claude' }), { now });
  assert.equal(kpi(claude, 'tok').num, tokensOf(M.haiku));
  assert.deepEqual(claude.cbm.rows.map(r => r.name), ['claude-haiku-4-5']);
  assert.equal(claude.sessions.stats.find(s => s.key === 'sess').num, 3);
  assert.equal(claude.sessions.note, '');
  assert.match(claude.trend.sub, /Claude Code$/);
  // Claude Code alone has every cost logged: nothing partial
  assert.equal(kpi(claude, 'cost').sub.some(r => r.text === 'Partial'), false);
  const codex = usageView(payload(), state({ prov: 'codex' }), { now });
  assert.equal(kpi(codex, 'tok').num, tokensOf(M.gpt5));
  assert.deepEqual(codex.cbm.rows.map(r => r.name), ['gpt-5']);
  // under All the session totals count every log; the rows stay the two providers
  const all = usageView(payload(), state(), { now });
  assert.equal(all.sessions.stats.find(s => s.key === 'sess').num, 3 + 2 + 4 + 1 + 2);
  assert.match(all.sessions.note, /every included log/);
});

test('(2) Included usage names the tools and computers read, and each source state and last scan', () => {
  const inc = usageView(payload(), state(), { now }).included;
  assert.equal(inc.shown, true);
  assert.equal(inc.line, 'Includes Claude Code, Codex, OMP, Muse Code and zcode on Ubuntu, Mac and Windows. Antigravity and Cursor keep no local usage log.');
  assert.equal(inc.label, 'Included usage · 1 cached, 1 unavailable');
  assert.deepEqual(inc.hosts, ['UBUNTU', 'MAC', 'WINDOWS']);
  assert.deepEqual(inc.rows.map(r => r.tool), ['Claude Code', 'Codex', 'OMP', 'Muse Code', 'zcode', 'Antigravity', 'Cursor']);
  const cell = (tool, i) => inc.rows.find(r => r.tool === tool).cells[i];
  assert.deepEqual([cell('OMP', 0).text, cell('OMP', 0).tone], ['Read 1h 0m ago', 'ok']);
  assert.deepEqual([cell('OMP', 1).text, cell('OMP', 1).tone], ['Cached, 2h 0m ago', 'cached']);
  assert.deepEqual([cell('zcode', 1).text, cell('zcode', 1).tone], ['Unavailable', 'unavailable']);
  assert.match(cell('zcode', 1).tip, /^Remote scan failed\. Never scanned\./);
  assert.equal(cell('Muse Code', 2).text, 'Not installed');
  assert.equal(cell('Claude Code', 1).text, 'Not read');
  assert.equal(cell('Antigravity', 0).text, 'No usage log');
  assert.match(cell('OMP', 0).tip, /40 usage events kept · Last scan/);
  // no sources (an older server, or nothing read yet): no disclosure
  assert.equal(includedView(payload({ sources: [] }), now).shown, false);
  assert.equal(includedView({}, now).shown, false);
});

test('(3) cost that is not logged reads "Not logged", never $0.00, and partial cost says so', () => {
  const view = usageView(payload(), state(), { now });
  const row = name => view.cbm.rows.find(r => r.name === name);
  // no logged cost and no listed rate: not logged, no bar, no share
  assert.deepEqual([row('qwen3.8-max').cost, row('qwen3.8-max').costNa, row('qwen3.8-max').share, row('qwen3.8-max').w], ['Not logged', true, '', 0]);
  assert.ok(row('qwen3.8-max').types.every(t => t.none || t.cost === 'Not logged'));
  // logged by the tool without a listed rate: a real cost
  assert.deepEqual([row('deepseek-v4.1-flash').cost, row('deepseek-v4.1-flash').partial], ['$2.00', false]);
  // part logged (OMP), part not (Muse): the logged part, marked partial
  assert.deepEqual([row('muse-spark-1.3-contributor').cost, row('muse-spark-1.3-contributor').partial, row('muse-spark-1.3-contributor').costNa], ['$0.50', true, false]);
  assert.match(row('muse-spark-1.3-contributor').rate, /partial/);
  // a listed rate whose split reconciles: exact
  assert.equal(row('GLM-5.3-Flash').cost, '$3.20');
  assert.ok(view.cbm.rows.every(r => !(r.costNa && /\$/.test(r.cost))));
  assert.match(view.cbm.foot, /muse-spark-1\.3-contributor and qwen3\.8-max have cost with no logged amount/);
  // the totals add only what is logged and say partial
  const cost = kpi(view, 'cost');
  assert.ok(Math.abs(cost.num - known) < 1e-9);
  assert.equal(cost.sub[0].text, 'Partial');
  assert.equal(kpi(view, 'in').sub[0].text, 'Partial');
  assert.equal(kpi(view, 'in').apport, true);
  assert.match(view.trend.legend.find(l => l.key === 'cost').note, /partial/);
  assert.match(view.tokens.sub, /cost partial$/);
  assert.equal(view.sessions.stats.find(s => s.key === 'avg').text, 'Not logged');
  assert.equal(view.sessions.rows.find(r => r.provider === 'claude').per, '$' + (haiku(1e6, 2e5, 1e6, 4e7) / 3).toFixed(2));
  // the cost donut leaves out what is not logged and says partial
  const donut = usageView(payload(), state({ donut: 'cost' }), { now }).donut;
  assert.ok(donut.segs.every(s => s.name !== 'qwen3.8-max'));
  assert.equal(donut.centreLabel, 'estimated cost, partial');
});

test('(3) a range with no logged cost at all reads "Not logged" everywhere, never $0.00', () => {
  // Sep 30 13:00 to 14:00: the Muse hour alone
  const s = state({ range: 'custom', from: Date.parse('2026-09-30T13:00:00Z'), to: Date.parse('2026-09-30T14:00:00Z') });
  const view = usageView(payload(), { ...s, range: '24h' }, { now });
  const muse = view.trend.buckets.find(b => b.tall === `${new Intl.NumberFormat().format(tokensOf(M.sparkMuse))} tokens`);
  assert.equal(muse.cost, 'Not logged');
  // no cost marker at $0 where nothing is logged
  assert.equal(muse.yCost, -1);
  assert.match(muse.foot, /^Partial: some cost here is not logged/);
  const heat = usageView(payload(), state({ range: '24h' }), { now }).heat;
  assert.match(heat.cells[2 * 24 + 13].tip, /cost not logged/);
  assert.match(heat.sub, /^Estimated cost, partial,/);
  // a payload of the Muse usage alone: every cost total reads Not logged
  const only = payload({ byHour: [byHour[3]], models: [M.sparkMuse], totals: add(M.sparkMuse), providers: [provider('muse', 'Muse logs', 1, 20, [M.sparkMuse])] });
  const v = usageView(only, state(), { now });
  for (const key of ['cost', 'in', 'out']) { assert.equal(kpi(v, key).text, 'Not logged'); assert.equal(kpi(v, key).has, false); }
  assert.ok(v.tokens.rows.every(r => r.cost === 'Not logged'));
  assert.equal(v.cache.saveText, 'Not logged');
  assert.equal(v.cache.ccostText, 'Not logged');
  const vd = usageView(only, state({ donut: 'cost' }), { now }).donut;
  assert.equal(vd.centre, 'Not logged');
  assert.equal(vd.empty, false);
  assert.ok(!JSON.stringify(v.kpis).includes('$0.00'));
});

test('(3) not-logged parts come from fallbackCostUsd, or from an unknown split without a listed rate', () => {
  assert.equal(notLoggedPart({ fallbackCostUsd: 2 }, 5), 2);
  assert.equal(notLoggedPart({ fallbackCostUsd: 9 }, 5), 5);
  assert.equal(notLoggedPart({ fallbackCostUsd: 0, costByType: null }, 5), 0);
  // a response from before fallbackCostUsd: an unknown split without a listed rate is wholly not logged
  assert.equal(notLoggedPart({ costByType: null }, 5), 5);
  assert.equal(notLoggedPart({ costByType: null, rates: FALLBACK }, 5), 5);
  assert.equal(notLoggedPart({ costByType: { input: 1, output: 1, cacheWrite: 0, cacheRead: 0 } }, 5), 0);
  assert.equal(notLoggedPart({}, 5), 0);
  assert.equal(notLoggedPart({ fallbackCostUsd: 3 }, null), 0);
  const A = activityData(payload({ byHour: [{ ...byHour[2], fallbackCostUsd: undefined }] }), now);
  assert.deepEqual([A.hours[0].cost, A.hours[0].unk], [0, true]);
  // rows of a tool the page does not know are dropped, as before
  assert.equal(activityData(payload({ byHour: [{ ...byHour[0], provider: 'qwen' }] }), now).hours.length, 0);
});

test('(1) models that are no provider\'s alone share one neutral family, with a distinct tone each', () => {
  const list = Array.from({ length: 9 }, (_, i) => ({ key: `m${i}`, model: `m${i}`, provider: '', logged: 10 - i, total: 1 }))
    .concat([{ key: 'c', model: 'c', provider: 'claude', logged: 50, total: 1 }]);
  const shades = modelShades(list);
  const neutral = list.filter(m => m.provider === '').map(m => shades[m.key]);
  assert.equal(new Set(neutral).size, 9);
  assert.equal(neutral[0], 1);
  assert.ok(neutral.every(v => v >= 0.28 - 1e-9 && v <= 1));
  // neighbours by size are never close in tone
  for (let i = 1; i < neutral.length; i++) assert.ok(Math.abs(neutral[i] - neutral[i - 1]) > 0.2, `${neutral}`);
  assert.equal(shades.c, 1);
});
