import test from 'node:test';
import assert from 'node:assert/strict';
import { ANALYTICS_PROVIDERS, activityView, analyticsChoiceId, analyticsSlintModel, analyticsView, buildQuotaPlot } from '../public/analytics-data.mjs';

const range = { preset: '7d', from: '2026-09-24T00:00:00Z', to: '2026-10-01T00:00:00Z', bucketMinutes: 60 };
const sample = (overrides = {}) => ({ sampledAt: '2026-09-30T00:00:00Z', observedAt: '2026-09-30T00:01:00Z', usedPercent: 42, remainingPercent: 58, used: null, limit: null, remaining: null, status: 'ok', source: 'Native subscription', platform: 'ubuntu', resetAt: '2026-10-02T00:00:00Z', expiresAt: null, isActive: false, ...overrides });
const metric = (overrides = {}) => ({ key: 'weekly', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 42, remainingPercent: 58, used: null, remaining: null, limit: null, unit: null, windowMinutes: 10080, resetAt: '2026-10-02T00:00:00Z', expiresAt: null, points: [sample()], ...overrides });
const account = (overrides = {}) => ({ id: 'codex-one', provider: 'codex', providerLabel: 'Codex', email: 'one@example.test', label: 'Codex account', platform: 'ubuntu', status: 'ok', source: 'Native quota', plan: 'Pro', isActive: true, sampleCount: 1, windows: [metric()], ...overrides });
const payload = (overrides = {}) => ({ schemaVersion: 1, updatedAt: '2026-10-01T00:00:00Z', range, filters: { platform: 'mac', provider: 'all', account: 'all' }, history: { retentionDays: 30, sampleCount: 1, message: 'Real observations only' }, summary: { accountCount: 1, availableAccounts: 1, sampleCount: 1, activeCodexAccountId: 'codex-one' }, providers: [{ provider: 'codex', accountCount: 1, availableAccounts: 1, latestSampleAt: '2026-09-30T00:00:00Z' }], accounts: [account()], ...overrides });
const now = Date.parse('2026-10-01T00:00:00Z');
const key = (id, window, metadata = {}) => JSON.stringify([id, window, metadata.label ?? 'Weekly usage', metadata.unit ?? '', metadata.kind ?? 'rate_limit']);
const totals = { inputTokens: 100, outputTokens: 25, cacheCreationTokens: 10, cacheReadTokens: 15, estimatedCostUsd: 0.0125 };
const native = (overrides = {}) => ({ status: 'ok', scope: 'ubuntu-local-cli', totals, message: 'All available records read; 7 files have unfinished records.', fetchedAt: range.to, providers: [{ provider: 'codex', label: 'Codex', totals, usageEvents: 2, sessionCount: 1 }], byDay: [{ date: '2026-09-30', provider: 'codex', ...totals }], byHour: [{ hour: '2026-09-30T01:00Z', provider: 'codex', ...totals }], models: [{ model: 'test-model', provider: 'codex', ...totals }], ...overrides });

test('quota history lists every account once by provider, summarises its main window and never defaults to active Codex', () => {
  const accounts = [
    account({ id: 'active', email: 'z@example.test', windows: [metric({ key: 'rolling', label: '5-hour', windowMinutes: 300 }), metric({ key: 'seven_day', label: 'Weekly usage', usedPercent: 61.237, remainingPercent: 38.763, points: [sample({ sampledAt: '2026-09-29T00:00:00Z', usedPercent: 40 }), sample({ usedPercent: 61.237 })] })] }),
    account({ id: 'other', email: 'a@example.test', isActive: false, windows: [metric({ key: 'seven_day', label: 'Weekly usage', usedPercent: null, remainingPercent: null, points: [] })] }),
    account({ id: 'claude', provider: 'claude', providerLabel: 'Claude', isActive: false, windows: [metric({ key: 'seven_day_fable', label: 'Fable · weekly' }), metric({ key: 'seven_day', label: 'Weekly usage', usedPercent: 12 })] }),
  ];
  const view = analyticsView(payload({ accounts, summary: { activeCodexAccountId: 'active' } }), {}, now);
  assert.equal(view.version, 6);
  assert.equal(view.selection.metricKey, 'all');
  // grouped in provider order, every account exactly once, ordered by identity, not by active state
  assert.deepEqual(view.quotaHistory.map(group => group.provider), ['claude', 'codex']);
  assert.deepEqual(view.quotaHistory.flatMap(group => group.rows.map(row => row.id)), ['claude', 'other', 'active']);
  const rows = Object.fromEntries(view.quotaHistory.flatMap(group => group.rows).map(row => [row.id, row]));
  // the main window is the canonical weekly window, never Fable and never the 5-hour window
  assert.equal(rows.claude.windowLabel, 'Weekly usage');
  assert.equal(rows.active.windowLabel, 'Weekly usage');
  assert.equal(rows.active.key, key('active', 'seven_day'));
  // at most two decimals; a missing current reading stays unavailable, never zero
  assert.equal(rows.active.valueText, '61.24'); assert.equal(rows.active.value, 61.237); assert.equal(rows.active.hasValue, true);
  assert.equal(rows.other.hasValue, false); assert.equal(rows.other.valueText, ''); assert.equal(rows.other.sparkPoints, 0);
  // the sparkline is the window's own history: two real observations, no invented points
  assert.equal(rows.active.sparkPoints, 2); assert.match(rows.active.spark, /^M\S+ \S+ L\S+ \S+$/);
  assert.equal(rows.active.reset, 'resets in 1d 0h');
  assert.equal(rows.active.active, true); assert.equal(rows.other.active, false);
  // the Slint seam carries the same groups
  const slint = analyticsSlintModel(view);
  assert.equal(slint.version, 6); assert.deepEqual(slint.quotaGroups, view.quotaHistory); assert.equal(slint.head.range, '7d');
  // per-window history data stays available for a row's focus chart; nothing is summed across accounts
  assert.equal(view.quotaCharts.some(chart => chart.key === rows.active.key), true);
  assert.deepEqual(analyticsView(payload({ accounts: accounts.toReversed() }), {}, now).quotaHistory, view.quotaHistory);
});
test('an explicit history selection identifies account and window even with duplicate window labels', () => {
  const accounts = [account(), account({ id: 'codex-two', email: 'two@example.test', windows: [metric(), metric({ key: 'model-weekly' })] })];
  const selected = key('codex-two', 'model-weekly');
  const view = analyticsView(payload({ accounts }), { metricKey: selected }, now);
  assert.equal(view.quotaCharts.length, 1);
  assert.equal(view.quotaCharts[0].key, selected);
  assert.equal(analyticsChoiceId(view, 'metric', view.metricValue), selected);
  assert.equal(new Set(view.choices.metrics.map(row => row.label)).size, 4);
  assert.equal(analyticsView(payload({ accounts }), { metricKey: 'forged' }, now).quotaCharts.length, 3);
});
test('hidden product metadata is absent from Analytics selections and all histories', () => {
  const hidden = metric({ key: 'extra_additional_1', label: 'Chat pass · weekly' });
  const codex = analyticsView(payload({ accounts: [account({ windows: [hidden, metric()] })] }), { metricKey: key('codex-one', hidden.key) }, now);
  assert.equal(codex.quotaCharts.length, 1);
  assert.doesNotMatch(JSON.stringify(codex), /Chat pass/);
  const qwen = analyticsView(payload({ accounts: [account({ provider: 'qwen', windows: [metric({ key: 'monthly', label: 'Monthly', expiresAt: null }), metric({ key: 'subscription', label: 'Plan subscription', expiresAt: '2026-11-01T00:00:00Z' })] })] }), {}, now);
  assert.equal(qwen.quotaCharts.length, 1);
  assert.doesNotMatch(JSON.stringify(qwen), /Plan subscription/);
  const zai = analyticsView(payload({ accounts: [account({ provider: 'zai', windows: [metric({ key: 'reset-packs-5h', label: 'Available 5-hour reset packs', used: null, limit: null, remaining: 0, resetAt: null, expiresAt: null }), metric()] })] }), {}, now);
  assert.equal(zai.quotaCharts.length, 1);
  assert.doesNotMatch(JSON.stringify(zai), /reset packs/);
});
test('quota history preserves timestamps and missing readings do not become zero samples', () => {
  const view = buildQuotaPlot(metric({ points: [sample({ sampledAt: range.from, usedPercent: 0 }), sample({ usedPercent: null, remainingPercent: null }), sample({ sampledAt: range.to, usedPercent: 100 })] }), range, now);
  assert.deepEqual(view.points.map(point => [point.x, point.percent]), [[0, 0], [1, 100]]);
  assert.match(view.points[0].label, /0% used/);
  assert.match(view.chartNote, /Gaps are unavailable samples/);
});
test('unavailable, invalid, out-of-range and negative percentage observations remain gaps', () => {
  const points = [sample({ status: 'unavailable' }), sample({ sampledAt: 'bad-date' }), sample({ sampledAt: '2026-09-01T00:00:00Z' }), sample({ sampledAt: '2026-10-02T00:00:00Z' }), sample({ usedPercent: -1, remainingPercent: null }), sample({ usedPercent: NaN, remainingPercent: null })];
  assert.equal(buildQuotaPlot(metric({ points }), range, now).points.length, 0);
  assert.equal(buildQuotaPlot(metric(), { from: 'bad-date', to: range.to }, now).chartHasPoints, false);
});
test('latest disabled/unlimited or absent current quota does not erase valid prior history', () => {
  for (const state of [{ unlimited: true }, { enabled: false }, { usedPercent: null, remainingPercent: null }]) {
    assert.equal(buildQuotaPlot(metric(state), range, now).points.length, 1);
    assert.equal(buildQuotaPlot(metric({ ...state, points: [] }), range, now).points.length, 0);
  }
});
test('remaining percentage derives used percentage without combining independent quotas', () => {
  const view = buildQuotaPlot(metric({ points: [sample({ usedPercent: null, remainingPercent: 25 })] }), range, now);
  assert.equal(view.points[0].percent, 75);
  assert.match(view.points[0].label, /75% used/);
});
test('percentage histories share a comparable scale and actual over-100 usage is preserved', () => {
  const windows = [metric({ key: 'over', points: [sample({ usedPercent: 120.5 })] }), metric({ key: 'normal', points: [sample({ usedPercent: 60.25 })] })];
  const view = analyticsView(payload({ accounts: [account({ windows })] }), {}, now);
  assert.deepEqual(view.quotaCharts.map(row => row.top), ['120.5%', '120.5%']);
  assert.equal(view.quotaCharts.find(row => row.key === key('codex-one', 'over')).points[0].percent, 100);
  assert.equal(view.quotaCharts.find(row => row.key === key('codex-one', 'normal')).points[0].percent, 50);
  assert.match(view.quotaCharts.find(row => row.key === key('codex-one', 'over')).points[0].label, /120.5% used/);
});
test('signed balances retain their own units, scale and actual zero while percentages stay separate', () => {
  const windows = [metric(), metric({ key: 'credits', kind: 'balance', unit: 'credits', remaining: -5, points: [sample({ remaining: -5 }), sample({ sampledAt: range.to, remaining: 15 })] }), metric({ key: 'usd', kind: 'balance', unit: 'USD', remaining: 0, points: [sample({ remaining: 0 })] })];
  const view = analyticsView(payload({ accounts: [account({ windows })] }), {}, now);
  const weekly = view.quotaCharts.find(row => row.key === key('codex-one', 'weekly'));
  const credits = view.quotaCharts.find(row => row.key === key('codex-one', 'credits', { unit: 'credits', kind: 'balance' }));
  const usd = view.quotaCharts.find(row => row.key === key('codex-one', 'usd', { unit: 'USD', kind: 'balance' }));
  assert.equal(weekly.top, '100%');
  assert.equal(credits.bottom, '-5 credits');
  assert.equal(credits.top, '15 credits');
  assert.match(usd.points[0].label, /0 USD remaining/);
  assert.doesNotMatch(credits.points[0].label, /%/);
});
test('observations retain reset, expiry, cache, source, platform and active provenance', () => {
  const view = buildQuotaPlot(metric({ points: [sample({ status: 'cached', expiresAt: '2026-10-03T00:00:00Z', isActive: true })] }), range, now);
  for (const value of ['Resets', 'Expires', 'Native subscription', 'ubuntu', 'Cached observation', 'Active Codex account']) assert.ok(view.points[0].label.includes(value));
  assert.match(view.chartNote, /15 minutes old/);
});
test('all nine providers use one overview without fabricated native metrics for other providers', () => {
  const view = analyticsView(payload(), {}, now);
  assert.deepEqual(view.providers.map(provider => provider.id), ANALYTICS_PROVIDERS.map(provider => provider[0]));
  assert.equal(view.activityHasData, false);
  assert.equal(view.activitySummaries.length, 0);
  assert.equal(view.accounts[0].active, true);
  assert.match(view.historyNote, /not additive token or cost totals/);
  assert.match(view.overviewNote, /1 accounts.*1 observations/);
});
test('server provider/account filters map exact IDs while catalog retains available account choices', () => {
  const second = account({ id: 'codex-two', email: 'two@example.test', isActive: false });
  const view = analyticsView(payload({ filters: { provider: 'codex', account: 'codex-two' }, accounts: [account(), second] }), { catalog: [account(), second, account({ id: 'claude-one', provider: 'claude', providerLabel: 'Claude' })] }, now);
  assert.equal(view.choices.accounts.length, 3);
  assert.equal(analyticsChoiceId(view, 'provider', 'Claude'), 'claude');
  assert.equal(analyticsChoiceId(view, 'account', view.accountValue), 'codex-two');
  assert.equal(analyticsChoiceId(view, 'account', 'Forged label'), null);
  assert.deepEqual(view.quotaCharts.map(row => row.key), [key('codex-two', 'weekly')]);
  assert.equal(view.providers.find(provider => provider.id === 'claude').availability, 'Outside filter');
});
test('missing history is explicit, does not select an active account, and never shows a fake zero', () => {
  const view = analyticsView(payload({ accounts: [], summary: {}, history: { status: 'unavailable' }, providers: [] }), {}, now);
  assert.deepEqual(view.quotaCharts, []);
  assert.deepEqual(view.choices.metrics, [{ id: 'all', label: 'All histories' }]);
  assert.match(view.historyNote, /History starts/);
});
test('native charts keep four token categories, API estimate, session and parsed-entry counters separate', () => {
  const view = activityView(native(), range);
  assert.equal(view.activityHasData, true);
  assert.deepEqual(view.activitySummaries.map(row => row.value), ['100', '25', '10', '15', '$0.01', '1']);
  assert.match(view.activitySummaries[5].note, /2 parsed usage-log entries/);
  assert.match(view.activitySummaries[5].note, /last active in range/);
  assert.match(view.activityNote, /cannot be attributed/);
  assert.match(view.activityNote, /7 files have unfinished records/);
  assert.match(view.activityPoints[0].label, /150 actual tokens/);
  assert.equal(view.activityStackPoints[0].input, 100 / 150 * 100);
  assert.equal(view.activityStackPoints[0].cacheCreated, 10 / 150 * 100);
  assert.match(view.activityCostPoints[0].label, /estimated API-equivalent cost/);
  assert.equal(view.activityModels[0].cacheCreated, '10');
  assert.equal(view.activityModels[0].cacheRead, '15');
  assert.doesNotMatch(JSON.stringify(view), /cost per session|cost\/session|HTTP requests/i);
});
test('daily buckets combine disjoint providers but reject duplicate, unknown and malformed rows', () => {
  const good = { date: '2026-09-30', provider: 'codex', ...totals };
  const view = activityView(native({ status: 'cached', byDay: [good, good, { ...good, provider: 'claude' }, { ...good, provider: 'qwen' }, { ...good, outputTokens: null }, { ...good, date: '2026-02-30' }] }), range);
  assert.equal(view.activityPoints.length, 1);
  assert.match(view.activityPoints[0].label, /300 actual tokens/);
  assert.match(view.activityCostPoints[0].label, /\$0.03/);
  assert.match(view.activityNote, /Cached/);
});
test('hourly selection uses actual UTC hour buckets with selected-range positions and gaps', () => {
  const view = activityView(native({ byHour: [{ hour: '2026-09-24T00:00Z', provider: 'codex', ...totals }, { hour: '2026-09-24T02:00:00Z', provider: 'codex', ...totals }, { hour: '2026-09-24T03:01Z', provider: 'codex', ...totals }, { hour: '2026-09-23T12:00Z', provider: 'codex', ...totals }] }), range, 'Hourly');
  assert.equal(view.activityIntervalValue, 'Hourly');
  assert.equal(view.activityStackPoints.length, 2);
  assert.equal(view.activityStackPoints[0].x, 0);
  assert.equal(view.activityStackPoints[1].x, 2 / 168);
  assert.match(view.activityStackPoints[1].label, /02:00 UTC/);
});
test('missing monetary estimates remain gaps; genuine measured zero is still shown', () => {
  const view = activityView(native({ byDay: [{ date: '2026-09-29', provider: 'codex', ...totals, estimatedCostUsd: null }, { date: '2026-09-30', provider: 'codex', ...totals, estimatedCostUsd: 0 }] }), range);
  assert.equal(view.activityStackPoints.length, 2);
  assert.equal(view.activityCostPoints.length, 1);
  assert.match(view.activityCostPoints[0].label, /\$0.00/);
  const mixed = activityView(native({ byDay: [{ date: '2026-09-30', provider: 'codex', ...totals }, { date: '2026-09-30', provider: 'claude', ...totals, estimatedCostUsd: null }] }), range);
  assert.equal(mixed.activityCostPoints.length, 0);
  assert.match(mixed.activityPoints[0].label, /Unavailable estimated/);
});
test('model ranking and token shares qualify the top-model subset and preserve provider identity', () => {
  const view = activityView(native({ models: [{ model: 'same', provider: 'codex', ...totals, estimatedCostUsd: 2 }, { model: 'same', provider: 'claude', ...totals, inputTokens: 250, estimatedCostUsd: 4 }, { model: 'unknown-cost', provider: 'codex', ...totals, estimatedCostUsd: null }] }), range);
  assert.equal(view.activityModels.length, 3);
  assert.equal(view.activityModels[0].provider, 'Claude');
  assert.equal(view.activityModels[0].costPercent, 100);
  assert.equal(view.activityModels[1].costPercent, 50);
  assert.equal(view.activityModels[2].hasCost, false);
  assert.equal(view.activityModels[0].tokenPercent, 50);
  assert.match(view.activityModelNote, /up to 30/);
  assert.match(view.activityModelNote, /listed models only/);
  assert.equal(view.activityModels.reduce((sum, row) => sum + row.tokenPercent, 0), 100);
});
test('account-specific unavailable activity never falls back to other-account native totals', () => {
  for (const status of ['unavailable', 'loading']) {
    const view = activityView(native({ status }), range);
    assert.equal(view.activityHasData, false);
    for (const field of ['activitySummaries', 'activityPoints', 'activityStackPoints', 'activityCostPoints', 'activityModels']) assert.deepEqual(view[field], []);
  }
});
test('all displayed numeric labels have at most two decimals while raw samples retain precision', () => {
  const data = payload({ accounts: [account({ windows: [metric({ points: [sample({ usedPercent: 12.3456 })] })] })], activity: native({ totals: { ...totals, estimatedCostUsd: 1.23456 }, byDay: [{ date: '2026-09-30', provider: 'codex', ...totals, estimatedCostUsd: 1.23456 }], models: [{ model: 'test', provider: 'codex', ...totals, estimatedCostUsd: 1.23456 }] }) });
  const view = analyticsView(data, {}, now);
  assert.match(view.quotaCharts[0].points[0].label, /12.35% used/);
  const strings = value => typeof value === 'string' ? [value] : value && typeof value === 'object' ? Object.values(value).flatMap(strings) : [];
  assert.ok(strings(view).every(value => !/\b\d+\.\d{3,}\b/.test(value)));
  assert.equal(data.accounts[0].windows[0].points[0].usedPercent, 12.3456);
  assert.equal(data.activity.totals.estimatedCostUsd, 1.23456);
});

test('same-provider identities with two plans map to distinct exact account choices', () => {
  const first = account({ id: 'go', provider: 'opencode-go' });
  const second = account({ ...first, id: 'wallet' });
  const view = analyticsView(payload({ accounts: [first, second] }), {}, now);
  const choices = view.choices.accounts.filter(row => row.id !== 'all');
  assert.equal(new Set(choices.map(row => row.label)).size, 2);
  for (const row of choices) assert.equal(analyticsChoiceId(view, 'account', row.label), row.id);
});
test('account filter guards against unattributed activity even if a malformed reply carries all-account totals', () => {
  const view = analyticsView(payload({ filters: { provider: 'all', account: 'codex-one' }, activity: native() }), {}, now);
  assert.equal(view.activityHasData, false);
  assert.equal(view.activityModels.length, 0);
  assert.equal(view.quotaCharts.length, 1);
});
test('entirely missing cost series is labeled unavailable rather than showing a zero estimate', () => {
  const view = activityView(native({ byDay: [{ date: '2026-09-30', provider: 'codex', ...totals, estimatedCostUsd: null }] }), range);
  assert.equal(view.activityCostTop, 'Unavailable');
  assert.deepEqual(view.activityCostPoints, []);
});

test('cached historical windows label original sample time separately from later recording time', () => {
  const original='2026-09-25T00:00:00Z';
  const plot=buildQuotaPlot(metric({ status:'cached', sampledAt:original, points:[sample({ sampledAt:original, observedAt:'2026-09-30T00:00:00Z', status:'cached', usedPercent:0 })] }),range,now);
  assert.equal(plot.points[0].x,1/7);
  assert.match(plot.points[0].label,/^Sampled /);
  assert.match(plot.points[0].label,/Recorded /);
  assert.match(plot.points[0].label,/Cached observation/);
  assert.match(plot.points[0].label,/0% used/);
  const unchanged=buildQuotaPlot(metric({points:[sample({sampledAt:original,observedAt:original})]}),range,now);
  assert.doesNotMatch(unchanged.points[0].label,/Recorded /);
});

test('current empty Zai pack summaries stay hidden unless usable past pack counts are genuinely positive', () => {
  const pack=metric({key:'reset-packs-5h',label:'Available 5-hour reset packs',kind:'balance',unit:'resets',usedPercent:null,remainingPercent:null,remaining:0,resetAt:null,expiresAt:null,points:[sample({sampledAt:'2026-09-25T00:00:00Z',usedPercent:null,remainingPercent:null,remaining:3})]});
  const make=points=>analyticsView(payload({accounts:[account({provider:'zai',providerLabel:'Z.ai',windows:[{...pack,points},metric()]})]}),{},now);
  const positive=make(pack.points);
  const history=positive.quotaCharts.find(row=>row.key===key('codex-one',pack.key,pack));
  assert.ok(history);
  assert.equal(history.points.length,1);
  assert.match(history.points[0].label,/3 resets remaining/);
  assert.equal(history.points[0].x,1/7);
  const used=make([sample({usedPercent:null,remainingPercent:null,remaining:null,used:2})]).quotaCharts.find(row=>row.key===key('codex-one',pack.key,pack));
  assert.match(used.points[0].label,/2 resets used/);
  for(const points of [[sample({remaining:0})],[sample({remaining:3,status:'unavailable'})],[sample({remaining:3,sampledAt:'2026-08-01T00:00:00Z'})],[sample({remaining:null,usedPercent:99})]]){
    assert.equal(make(points).quotaCharts.some(row=>row.key===key('codex-one',pack.key,pack)),false);
  }
});


test('large activity KPIs are compact while exact totals remain in notes and raw charts', () => {
  const actual = { inputTokens: 131201584, outputTokens: 49047924, cacheCreationTokens: 614767835, cacheReadTokens: 30004891853, estimatedCostUsd: 13439.5300625 };
  const input = native({ totals: actual, byDay: [{ date: '2026-09-30', provider: 'codex', ...actual }], models: [{ model: 'large', provider: 'codex', ...actual }] });
  const before = structuredClone(input);
  const view = activityView(input, range);
  assert.deepEqual(view.activitySummaries.slice(0, 5).map(row => row.value), ['131.2M', '49.05M', '614.77M', '30B', '$13.44K']);
  for (const [index, total] of Object.values(actual).slice(0, 4).entries()) assert.ok(view.activitySummaries[index].note.includes(total.toLocaleString()));
  assert.match(view.activitySummaries[4].note, /\$13,439\.53.*not your bill/);
  assert.match(view.activityStackPoints[0].label, /30,004,891,853 cache read/);
  assert.equal(view.activityModels[0].cacheRead, '30,004,891,853');
  assert.equal(view.activityModels[0].cost, '$13,439.53');
  assert.deepEqual(input, before);
});

test('changed history labels, units and kinds have separate exact selectors without merging observations', () => {
  const oldMoney = metric({ key: 'extra_usage', label: 'Extra usage', kind: 'extra_usage', unit: 'USD', usedPercent: null, remainingPercent: null, points: [sample({ usedPercent: null, remainingPercent: null, remaining: 50 })] });
  const newPercent = metric({ key: 'extra_usage', label: 'Extra usage', kind: 'extra_usage' });
  const oldWeekly = metric();
  const newLabel = metric({ label: 'Weekly limit' });
  const newKind = metric({ kind: 'spend' });
  const windows = [oldMoney, newPercent, oldWeekly, newLabel, newKind];
  const input = payload({ accounts: [account({ windows })] });
  const before = structuredClone(input);
  const all = analyticsView(input, {}, now);
  assert.equal(all.quotaCharts.length, windows.length);
  assert.equal(new Set(all.quotaCharts.map(row => row.key)).size, windows.length);
  assert.equal(new Set(all.choices.metrics.map(row => row.label)).size, windows.length + 1);
  for (const window of windows) {
    const selected = key('codex-one', window.key, window);
    const choice = all.choices.metrics.find(row => row.id === selected);
    assert.equal(analyticsChoiceId(all, 'metric', choice.label), selected);
    const view = analyticsView(input, { metricKey: selected }, now);
    assert.equal(view.quotaCharts.length, 1);
    assert.equal(view.quotaCharts[0].key, selected);
    assert.equal(view.quotaCharts[0].points.length, window.points.length);
    if (window === oldMoney) assert.match(view.quotaCharts[0].points[0].label, /50 USD remaining/);
    if (window === newPercent) assert.match(view.quotaCharts[0].points[0].label, /42% used/);
  }
  const reversed = analyticsView(payload({ accounts: [account({ windows: windows.toReversed() })] }), {}, now);
  assert.deepEqual(reversed.quotaCharts.map(row => row.key), all.quotaCharts.map(row => row.key));
  assert.deepEqual(input, before);
});

test('malformed history identity fields cannot stringify objects or bypass the window-kind allowlist', () => {
  const malformed = metric({ key: 'safe', label: { private: 'not a label' }, unit: ['not a unit'], kind: { private: 'not a kind' } });
  const unsupportedKind = metric({ key: 'unknown-kind', kind: 'unrecognized' });
  const view = analyticsView(payload({ accounts: [null, { ...account(), id: { unsafe: 'identity' } }, account({ windows: [null, [], 7, metric({ key: { unsafe: 'key' } }), malformed, unsupportedKind] })] }), {}, now);
  assert.equal(view.quotaCharts.length, 2);
  assert.deepEqual(JSON.parse(view.quotaCharts.find(row => row.key.includes('safe')).key), ['codex-one', 'safe', '', '', '']);
  assert.equal(JSON.parse(view.quotaCharts.find(row => row.key.includes('unknown-kind')).key).at(-1), '');
  assert.doesNotMatch(JSON.stringify(view.choices.metrics), /private|unsafe|unrecognized|not a label|not a unit|not a kind/);
});
