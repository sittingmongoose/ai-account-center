import test from 'node:test';
import assert from 'node:assert/strict';
import {
  VIEW_MODEL_VERSION, dashboardViewModel, detailsViewModel, chromeView, updateViewModel, providerRegistry,
  meterView, amountView, intervalLabel, parseIntervalLabel, valueText,
} from '../public/view-model.mjs';

const now = Date.parse('2026-10-01T15:16:00Z');
const at = minutes => new Date(now + minutes * 60_000).toISOString();
const window = (overrides = {}) => ({ key: 'seven_day', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 10, remainingPercent: 90, resetAt: at(60 * 24), windowMinutes: 10080, used: null, limit: null, unit: null, ...overrides });
const account = (overrides = {}) => ({ id: 'codex:one', provider: 'codex', providerLabel: 'Codex', label: 'Codex', email: 'one@example.test', plan: 'pro', platform: 'ubuntu', source: 'Native quota', status: 'ok', message: null, fetchedAt: at(-1), sampledAt: at(-1), isActive: false, windows: [window()], capabilities: { codexProfile: 'one', claudeProfileId: null, claudePlatforms: [] }, ...overrides });
const claude = (id, plan, windows) => account({ id: `claude:${id}`, provider: 'claude', providerLabel: 'Claude', email: `${id}@example.test`, plan, platform: 'mac', capabilities: { codexProfile: null, claudeProfileId: id, claudePlatforms: ['mac', 'windows'] }, windows });
const data = (accounts, extra = {}) => ({ schemaVersion: 1, updatedAt: at(0), settings: { refreshIntervalSeconds: 60 }, accounts, codexAutoSwitch: { enabled: true, thresholdPercent: 5, pollIntervalSeconds: 60, outcome: 'healthy', message: 'Healthy', activationInProgress: false }, ...extra });
const section = (vm, id) => vm.sections.find(row => row.id === id);

test('the view model is versioned and lists Claude, Codex and Antigravity sections, then provider cards', () => {
  const vm = dashboardViewModel(data([
    account(), claude('a', 'max', [window()]),
    account({ id: 'antigravity:a', provider: 'antigravity', email: 'ag@example.test', windows: [window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly' })], capabilities: {} }),
    account({ id: 'kimi-code:a', provider: 'kimi-code', email: 'k@example.test', windows: [window({ key: 'weekly', label: 'Weekly' })] }),
    account({ id: 'cursor:a', provider: 'cursor', email: 'c@example.test', windows: [window({ key: 'plan-reported', label: 'Included usage', windowMinutes: null })] }),
  ]), { now });
  assert.equal(vm.version, VIEW_MODEL_VERSION);
  assert.equal(vm.version, 2);
  assert.deepEqual(vm.sections.map(row => row.id), ['claude', 'codex', 'antigravity']);
  assert.deepEqual(vm.cards.map(card => card.id), ['cursor:a', 'kimi-code:a']);
  assert.deepEqual(vm.cards.map(card => card.provider), ['cursor', 'kimi-code']);
  assert.deepEqual(vm.registry.map(row => row.id), ['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go']);
  assert.equal(vm.registry.find(row => row.id === 'muse').count, 0);
  // no Antigravity section without Antigravity accounts
  assert.equal(section(dashboardViewModel(data([account()]), { now }), 'antigravity'), undefined);
});

test('hidden providers sent by the backend leave the dashboard and are marked in the registry', () => {
  const accounts = [account(), account({ id: 'kimi-code:a', provider: 'kimi-code', windows: [window({ key: 'weekly' })] })];
  const vm = dashboardViewModel(data(accounts, { settings: { refreshIntervalSeconds: 60, hiddenProviders: ['kimi-code', 'codex', 'not-a-provider'] } }), { now });
  assert.equal(vm.cards.length, 0);
  assert.equal(section(vm, 'codex'), undefined);
  assert.equal(vm.registry.find(row => row.id === 'kimi-code').visible, false);
  assert.equal(vm.registry.find(row => row.id === 'cursor').visible, true);
  assert.deepEqual(providerRegistry(data(accounts)).filter(row => !row.visible), []);
});

test('Claude shows a Fable meter only for Max plans: real when reported, "Not reported yet" when absent, never zero', () => {
  const fable = window({ key: 'seven_day_fable', label: 'Weekly Fable usage', usedPercent: 0, remainingPercent: 100 });
  const vm = dashboardViewModel(data([
    claude('max-reported', 'max', [window({ key: 'five_hour', label: 'Five-hour usage', windowMinutes: 300 }), window(), fable]),
    claude('max-missing', 'max_20x', [window()]),
    claude('pro', 'pro', [window(), { ...fable }]),
  ]), { now });
  const claudeSection = section(vm, 'claude');
  assert.deepEqual(claudeSection.columns.map(c => c.label), ['5-hour', 'Weekly', 'Fable']);
  const [reported, missing, pro] = claudeSection.rows;
  assert.equal(reported.cells[2].hasValue, true); assert.equal(reported.cells[2].valueText, '0'); assert.equal(reported.cells[2].key, 'claude:max-reported|seven_day_fable');
  assert.equal(missing.cells[2].hasValue, false); assert.equal(missing.cells[2].naText, 'Not reported yet'); assert.equal(missing.cells[2].valueText, '');
  // Pro: no Fable cell on Home even when a Fable window exists (it may appear in Details)
  assert.equal(pro.cells[2].key, ''); assert.equal(pro.cells[2].hasValue, false); assert.equal(pro.cells[2].naText, '');
  // a missing 5-hour reading is unavailable, not zero
  assert.equal(missing.cells[0].hasValue, false); assert.equal(missing.cells[0].naText, 'Unavailable');
  // no Max account: no Fable column at all
  assert.deepEqual(section(dashboardViewModel(data([claude('pro', 'pro', [window()])]), { now }), 'claude').columns.map(c => c.key), ['five', 'weekly']);
});

test('Codex: no 5-hour column unless one is reported, Chat pass stays hidden, the notch follows auto-switch', () => {
  const chat = window({ key: 'extra_additional_1', label: 'Chat pass · weekly', usedPercent: 50 });
  const active = account({ id: 'codex:a', email: 'a@example.test', isActive: true, windows: [window({ usedPercent: 92.125, remainingPercent: 7.875 }), chat], capabilities: { codexProfile: 'a' } });
  const other = account({ id: 'codex:b', email: 'b@example.test', windows: [window({ usedPercent: 9 }), window({ key: 'extra_additional_2', label: 'Additional rate limit', windowMinutes: 300, usedPercent: 99 })], capabilities: { codexProfile: 'b' } });
  let codex = section(dashboardViewModel(data([active, other]), { now }), 'codex');
  assert.deepEqual(codex.columns.map(c => c.key), ['weekly']);
  assert.equal(codex.switchable, true); assert.equal(codex.kind, 'switchable');
  assert.equal(JSON.stringify(codex).includes('Chat pass'), false);
  assert.equal(codex.rows[0].cells[0].valueText, '92.13');
  assert.equal(codex.rows[0].cells[0].notch, 95); assert.equal(codex.rows[0].cells[0].notchFaint, false);
  assert.equal(codex.rows[1].cells[0].notchFaint, true);
  assert.equal(codex.auto.thresholdLabel, '95%'); assert.equal(codex.auto.thresholdUsed, 95);
  assert.equal(codex.activeId, 'codex:a'); assert.equal(codex.rows[0].active, true); assert.equal(codex.rows[0].activeLabel, 'on Ubuntu');
  assert.equal(codex.rows[0].canActivate, false); assert.equal(codex.rows[1].canActivate, true); assert.equal(codex.rows[1].activateKind, 'activate'); assert.equal(codex.rows[1].profile, 'b');
  // auto-switch off: no notch
  const off = data([active, other]); off.codexAutoSwitch = { ...off.codexAutoSwitch, enabled: false };
  assert.equal(section(dashboardViewModel(off, { now }), 'codex').rows[0].cells[0].notch, null);
  // one account reports the canonical five-hour window: the column appears, the other row gets an empty cell
  const plus = account({ id: 'codex:c', email: 'c@example.test', windows: [window({ key: 'five_hour', label: '5h', windowMinutes: 300, usedPercent: 0, remainingPercent: 100 }), window()], capabilities: { codexProfile: 'c' } });
  codex = section(dashboardViewModel(data([active, plus]), { now }), 'codex');
  assert.deepEqual(codex.columns.map(c => c.key), ['five', 'weekly']);
  assert.equal(codex.rows[0].cells[0].key, ''); assert.equal(codex.rows[1].cells[0].hasValue, true); assert.equal(codex.rows[1].cells[0].valueText, '0');
});

test('Antigravity is the same switchable section, read-only until the native inventory verifies it', () => {
  const windows = [
    window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly', usedPercent: 0.0886 }),
    window({ key: 'gemini-5h', label: 'Gemini Models · 5-hour', windowMinutes: 300, usedPercent: 0 }),
    window({ key: '3p-weekly', label: 'Claude and GPT models · Weekly' }),
    window({ key: '3p-5h', label: 'Claude and GPT models · 5-hour', windowMinutes: 300 }),
  ];
  const ag = section(dashboardViewModel(data([account({ id: 'antigravity:a', provider: 'antigravity', email: 'ag@example.test', plan: 'Google AI Pro', windows, capabilities: {} })]), { now }), 'antigravity');
  assert.equal(ag.switchable, true);
  assert.deepEqual(ag.columns.map(c => c.label), ['Gemini 5-hour', 'Gemini weekly', 'Claude and GPT 5-hour', 'Claude and GPT weekly']);
  assert.equal(ag.rows[0].cells[1].valueText, '0.09');
  assert.equal(ag.rows[0].canActivate, false); assert.equal(ag.rows[0].active, false); assert.equal(ag.rows[0].activateKind, 'antigravity-activate');
  assert.equal(ag.auto.available, false); assert.match(ag.auto.message, /second Antigravity account/);
});

test('meters keep raw overage and at most two decimals; packs, balances and spend are amounts, never meters', () => {
  const over = meterView(account(), window({ usedPercent: 129.567, used: 648, limit: 500, unit: 'requests' }), { now });
  assert.equal(over.value, 129.567); assert.equal(over.valueText, '129.57'); assert.equal(over.overText, '29.57'); assert.equal(over.amount, '648 of 500 requests');
  const missing = meterView(account(), window({ usedPercent: null, remainingPercent: null }), { now });
  assert.equal(missing.hasValue, false); assert.equal(missing.valueText, ''); assert.equal(missing.reset, ''); assert.equal(missing.naText, 'Unavailable');
  const noReset = meterView(account(), window({ resetAt: null }), { now });
  assert.equal(noReset.reset, 'no reset reported'); assert.equal(noReset.resetExact, '');
  assert.equal(meterView(account(), window({ resetAt: at(90) }), { now }).resetSoon, true);
  assert.equal(meterView(account(), window({ resetAt: at(90) }), { now }).reset, 'resets in 1h 30m');
  assert.equal(valueText(0.0886), '0.09');
  const qwen = account({ id: 'qwen:a', provider: 'qwen', windows: [
    window({ key: 'monthly', label: 'Monthly', usedPercent: 25.061, windowMinutes: null, used: 45109.83, limit: 180000, unit: 'credits' }),
    window({ key: 'subscription', label: 'Plan subscription', usedPercent: null, remainingPercent: null, expiresAt: at(60 * 24 * 19) }),
    window({ key: 'addon-pack-1', label: 'Additional credit pack 1', kind: 'balance', usedPercent: 100, remaining: 0, limit: 20000, unit: 'credits', expiresAt: at(60 * 24 * 7) }),
  ] });
  const card = dashboardViewModel(data([qwen]), { now }).cards[0];
  assert.deepEqual(card.meters.map(m => m.label), ['Monthly']);
  assert.equal(card.amounts.length, 1); assert.equal(card.amounts[0].label, 'Credit pack 1'); assert.equal(card.amounts[0].value, '0'); assert.equal(card.amounts[0].spent, true);
  assert.equal(JSON.stringify(card).includes('Plan subscription'), false);
  assert.equal(amountView(account(), { key: 'credits_balance', label: 'Extra usage credits', kind: 'balance', remaining: 62500, unit: 'credits' }).value, '62,500 credits');
  assert.equal(amountView(account(), { key: 'extra_usage', label: 'Extra usage', kind: 'extra_usage', enabled: false, limit: 50, unit: 'USD' }).value, 'Off');
  const zai = account({ id: 'zai:a', provider: 'zai', windows: [window({ key: 'usage-2', label: 'Weekly · Tokens' }), { key: 'reset-packs-5h', label: 'Available 5-hour reset packs', kind: 'balance', unit: 'packs', remaining: 0 }] });
  assert.equal(dashboardViewModel(data([zai]), { now }).cards[0].amounts.length, 0);
});

test('codex rows summarise credits and banked resets of that account only', () => {
  const row = section(dashboardViewModel(data([account({ windows: [window(), { key: 'credits_balance', label: 'Extra usage credits', kind: 'balance', unit: 'credits', remaining: 62500, enabled: true }, { key: 'banked_resets_0', label: 'Banked resets', kind: 'balance', unit: 'resets', remaining: 1 }] })]), { now }), 'codex').rows[0];
  assert.equal(row.amountsLine, '62.5K credits · 1 banked');
});

test('the header status line is derived from the data and never claims live for cached readings', () => {
  assert.deepEqual(chromeView(data([account({ status: 'cached' })]), { now, intervalSeconds: 60 }).statusMore, 'cached readings');
  const live = chromeView(data([account()]), { now: Date.parse(at(0)) + 3_000 });
  assert.equal(live.statusLead, 'Updated'); assert.equal(live.statusStrong, 'just now'); assert.equal(live.statusMore, 'live');
  assert.equal(chromeView(data([account()]), { now: Date.parse(at(0)) + 125_000 }).statusStrong, '2m ago');
  assert.equal(chromeView(data([account()]), { now, refreshing: true }).refreshing, true);
  assert.equal(chromeView(null, { now }).statusLead, 'Account data unavailable');
});

test('Details list every visible window, the amounts and the provenance of one account', () => {
  const d = data([claude('m', 'max', [window({ key: 'five_hour', label: 'Five-hour usage', windowMinutes: 300 }), window(), { key: 'prepaid_balance', label: 'Prepaid balance', kind: 'balance', unit: 'USD', remaining: 0 }])]);
  const details = detailsViewModel(d, 'claude:m', { now });
  assert.equal(details.title, 'm@example.test');
  assert.deepEqual(details.meters.map(m => m.label), ['5-hour usage', 'Weekly usage']);
  assert.equal(details.amounts[0].value, '$0.00');
  assert.match(details.note, /Fable usage is not reported yet/);
  assert.deepEqual(details.facts.map(f => f.label), ['Status', 'Sampled', 'Fetched', 'Source', 'Platform', 'Profile']);
  assert.equal(detailsViewModel(d, 'nope', { now }), null);
});

test('refresh interval labels round-trip and Update apps reflects the job truthfully', () => {
  for (const seconds of [30, 60, 90, 120, 300, 3600]) assert.equal(parseIntervalLabel(intervalLabel(seconds)), seconds);
  assert.equal(intervalLabel(90), '1 min 30 s');
  assert.equal(parseIntervalLabel('soon'), null);
  assert.deepEqual(updateViewModel({ state: 'running', activePlatform: 'mac', results: [{ status: 'updated' }, { status: 'current' }] }), { running: true, done: false, count: 2, total: 21, tip: 'Updating apps on Mac · running apps may restart', summary: '2 results' });
  assert.equal(updateViewModel({ state: 'completed', results: [{ status: 'failed' }] }, { done: true }).done, true);
  assert.equal(updateViewModel(null).count, 0);
});
