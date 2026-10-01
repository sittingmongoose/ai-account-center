import test from 'node:test';
import assert from 'node:assert/strict';
import { ANALYTICS_PROVIDERS, activityView, analyticsChoiceId, analyticsView, buildQuotaPlot } from '../public/analytics-data.mjs';

const range = { preset: '7d', from: '2026-09-24T00:00:00Z', to: '2026-10-01T00:00:00Z', bucketMinutes: 60 };
const sample = (overrides = {}) => ({ sampledAt: '2026-09-30T00:00:00Z', observedAt: '2026-09-30T00:01:00Z', usedPercent: 42, remainingPercent: 58, used: null, limit: null, remaining: null, status: 'ok', source: 'Native subscription', platform: 'ubuntu', resetAt: '2026-10-02T00:00:00Z', expiresAt: null, isActive: false, ...overrides });
const metric = (overrides = {}) => ({ key: 'weekly', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 42, remainingPercent: 58, used: null, remaining: null, limit: null, unit: null, windowMinutes: 10080, resetAt: '2026-10-02T00:00:00Z', expiresAt: null, points: [sample()], ...overrides });
const account = (overrides = {}) => ({ id: 'codex-one', provider: 'codex', providerLabel: 'Codex', email: 'one@example.test', label: 'Codex account', platform: 'ubuntu', status: 'ok', source: 'Native quota', plan: 'Pro', isActive: true, sampleCount: 1, windows: [metric()], ...overrides });
const payload = (overrides = {}) => ({ schemaVersion: 1, updatedAt: '2026-10-01T00:00:00Z', range, filters: { platform: 'mac', provider: 'all', account: 'all' }, history: { retentionDays: 30, sampleCount: 1, message: 'Real observations only' }, summary: { accountCount: 1, availableAccounts: 1, sampleCount: 1, activeCodexAccountId: 'codex-one' }, providers: [{ provider: 'codex', accountCount: 1, availableAccounts: 1, latestSampleAt: '2026-09-30T00:00:00Z' }], accounts: [account()], ...overrides });
const now = Date.parse('2026-10-01T00:00:00Z');

test('quota history preserves actual timestamps and creates no invented zero samples', () => {
  const view = buildQuotaPlot(metric({ points: [sample({ sampledAt: range.from, usedPercent: 0 }), sample({ usedPercent: null, remainingPercent: null }), sample({ sampledAt: range.to, usedPercent: 100 })] }), range, now);
  assert.equal(view.points.length, 2);
  assert.deepEqual(view.points.map(point => [point.x, point.percent]), [[0, 0], [1, 100]]);
  assert.match(view.points[0].label, /0% used/);
  assert.match(view.chartNote, /Gaps are unavailable samples/);
});
test('unavailable responses, invalid dates and outside-range observations remain gaps', () => {
  const points = [sample({ status: 'unavailable' }), sample({ sampledAt: 'bad-date' }), sample({ sampledAt: '2026-09-01T00:00:00Z' }), sample({ sampledAt: '2026-10-02T00:00:00Z' }), sample({ usedPercent: -1, remainingPercent: null }), sample({ usedPercent: NaN, remainingPercent: null })];
  assert.equal(buildQuotaPlot(metric({ points }), range, now).points.length, 0);
  assert.equal(buildQuotaPlot(metric(), { from: 'bad-date', to: range.to }, now).chartHasPoints, false);
});
test('remaining percent maps to actual used percent without summing quotas', () => {
  const view = buildQuotaPlot(metric({ points: [sample({ usedPercent: null, remainingPercent: 25 })] }), range, now);
  assert.equal(view.points[0].percent, 75);
  assert.match(view.points[0].label, /75% used/);
});
test('signed credit balances retain actual units and normalize only plot coordinates', () => {
  const window = metric({ key: 'credits', label: 'Credits', kind: 'balance', unit: 'credits', usedPercent: null, remainingPercent: null, remaining: -5, points: [sample({ remaining: -5 }), sample({ sampledAt: range.to, remaining: 15 })] });
  const view = buildQuotaPlot(window, range, now);
  assert.equal(view.metricPercent, false);
  assert.equal(view.chartBottom, '-5 credits');
  assert.equal(view.chartTop, '15 credits');
  assert.deepEqual(view.points.map(point => point.percent), [0, 100]);
  assert.match(view.points[0].label, /-5 credits remaining/);
  assert.doesNotMatch(view.points[0].label, /%/);
});
test('zero balances and reported spend without a limit remain usable', () => {
  const balance = buildQuotaPlot(metric({ kind: 'balance', unit: 'credits', remaining: 0, points: [sample({ remaining: 0 })] }), range, now);
  assert.equal(balance.points.length, 1);
  assert.match(balance.points[0].label, /0 credits remaining/);
  const spend = buildQuotaPlot(metric({ kind: 'spend', unit: 'USD', usedPercent: null, remainingPercent: null, used: 12.5, points: [sample({ usedPercent: null, remainingPercent: null, used: 12.5 })] }), range, now);
  assert.equal(spend.metricPercent, false);
  assert.match(spend.points[0].label, /12.5 USD used/);
});
test('unlimited and disabled quota windows are not rendered as percentage consumption', () => {
  for (const state of [{ unlimited: true }, { enabled: false }]) assert.equal(buildQuotaPlot(metric(state), range, now).points.length, 0);
});
test('observations retain reset, expiration, cache, source and active-account provenance', () => {
  const view = buildQuotaPlot(metric({ points: [sample({ status: 'cached', expiresAt: '2026-10-03T00:00:00Z', isActive: true })] }), range, now);
  for (const value of ['Resets', 'Expires', 'Native subscription', 'ubuntu', 'Cached observation', 'Active Codex account']) assert.ok(view.points[0].label.includes(value));
  assert.match(view.chartNote, /15 minutes old/);
});
test('all nine providers appear without fabricated extra token or cost metrics', () => {
  const view = analyticsView(payload(), {}, now);
  assert.equal(view.providers.length, 9);
  assert.deepEqual(view.providers.map(provider => provider.id), ANALYTICS_PROVIDERS.map(provider => provider[0]));
  assert.equal(view.activityHasData, false);
  assert.equal(view.activitySummaries.length, 0);
  assert.equal(view.accounts[0].active, true);
  assert.match(view.historyNote, /not additive token or cost totals/);
  assert.equal(view.summaries[2].value, '1');
});
test('server account/provider filters map exact IDs and catalog survives filtered results', () => {
  const second = account({ id: 'codex-two', email: 'two@example.test', isActive: false });
  const view = analyticsView(payload({ filters: { provider: 'codex', account: 'codex-two' }, accounts: [second] }), { catalog: [account(), second, account({ id: 'claude-one', provider: 'claude', providerLabel: 'Claude' })] }, now);
  assert.equal(view.choices.accounts.length, 3);
  assert.equal(analyticsChoiceId(view, 'provider', 'Claude'), 'claude');
  assert.equal(analyticsChoiceId(view, 'account', view.accountValue), 'codex-two');
  assert.equal(analyticsChoiceId(view, 'account', 'Forged label'), null);
  assert.equal(analyticsChoiceId(view, 'metric', view.metricValue), 'weekly');
  assert.match(view.chartTitle, /two@example.test/);
  assert.equal(view.providers.find(provider => provider.id === 'claude').availability, 'Outside filter');
  assert.equal(view.providers.find(provider => provider.id === 'claude').accounts, '—');
});
test('explicit metric choice and duplicate labels identify their exact windows', () => {
  const view = analyticsView(payload({ accounts: [account({ windows: [metric(), metric({ key: 'model-weekly', usedPercent: 9 })] })] }), { metricKey: 'model-weekly' }, now);
  assert.equal(view.selection.metricKey, 'model-weekly');
  assert.match(view.metricValue, /model-weekly/);
  assert.equal(analyticsChoiceId(view, 'metric', view.metricValue), 'model-weekly');
});
test('current quotas show reported reset/expiry and signed overage without a fabricated expiry', () => {
  const view = analyticsView(payload({ accounts: [account({ windows: [metric({ kind: 'balance', unit: 'credits', used: 30, limit: 20, remaining: -10, usedPercent: null, remainingPercent: null, expiresAt: null }), metric({ key: 'pack', label: 'Extra pack', kind: 'extra_usage', remaining: 100, expiresAt: '2026-10-15T00:00:00Z' })] })] }), {}, now);
  assert.match(view.metrics[0].amount, /30 \/ 20 credits used/);
  assert.match(view.metrics[0].amount, /-10 credits remaining/);
  assert.equal(view.metrics[0].expiration, '');
  assert.match(view.metrics[1].expiration, /Expires/);
});
test('missing history is explicit and does not become a zero chart', () => {
  const view = analyticsView(payload({ accounts: [], summary: {}, history: { status: 'unavailable' }, providers: [] }), {}, now);
  assert.equal(view.chartHasPoints, false);
  assert.deepEqual(view.points, []);
  assert.match(view.historyNote, /History starts/);
  assert.equal(view.metricValue, 'No metrics available');
});
test('reported usage over 100 percent remains visible in current values and history', () => {
  const view = analyticsView(payload({ accounts: [account({ windows: [metric({ usedPercent: 120.5, remainingPercent: null, points: [sample({ usedPercent: 120.5, remainingPercent: null })] })] })] }), {}, now);
  assert.match(view.metrics[0].amount, /120\.5%/);
  assert.equal(view.chartHasPoints, true);
  assert.equal(view.chartTop, '120.5%');
  assert.match(view.points[0].label, /120\.5% used/);
});
const totals = { inputTokens: 100, outputTokens: 25, cacheCreationTokens: 10, cacheReadTokens: 15, estimatedCostUsd: 0.0125 };
test('retained native analytics distinguishes actual tokens from estimated API cost', () => {
  const activity = { status: 'ok', scope: 'ubuntu-local-cli', totals, message: 'Local usage events', fetchedAt: range.to, providers: [{ provider: 'codex', label: 'Codex', totals, usageEvents: 2, sessionCount: 1 }], byDay: [{ date: '2026-09-30', provider: 'codex', ...totals }], models: [{ model: 'test-model', provider: 'codex', ...totals }] };
  const view = activityView(activity, range);
  assert.equal(view.activityHasData, true);
  assert.equal(view.activitySummaries[0].value, '100');
  assert.equal(view.activitySummaries[2].value, '25');
  assert.match(view.activitySummaries[3].note, /not your bill/);
  assert.match(view.activityNote, /cannot be attributed/);
  assert.match(view.activityPoints[0].label, /150 actual tokens/);
  assert.equal(view.activityModels[0].provider, 'Codex');
  assert.match(view.activityProviders[0].reset, /2 usage events/);
});
test('daily native events combine independent providers and reject duplicate/unknown/malformed rows', () => {
  const good = { date: '2026-09-30', provider: 'codex', ...totals };
  const view = activityView({ status: 'cached', totals, byDay: [good, good, { ...good, provider: 'claude' }, { ...good, provider: 'qwen' }, { ...good, outputTokens: null }, { ...good, date: 'bad-date' }] }, range);
  assert.equal(view.activityPoints.length, 1);
  assert.match(view.activityPoints[0].label, /300 actual tokens/);
  assert.match(view.activityNote, /Cached/);
});
test('account-specific or unavailable native activity never falls back to other-account totals', () => {
  for (const state of ['unavailable', 'loading']) {
    const view = activityView({ status: state, totals, models: [{ model: 'wrong', provider: 'codex', ...totals }], byDay: [{ date: '2026-09-30', provider: 'codex', ...totals }] }, range);
    assert.equal(view.activityHasData, false);
    assert.deepEqual(view.activitySummaries, []);
    assert.deepEqual(view.activityPoints, []);
    assert.deepEqual(view.activityModels, []);
  }
});
