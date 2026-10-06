import test from 'node:test';
import assert from 'node:assert/strict';
import {
  VIEW_MODEL_VERSION,
  dashboardViewModel,
  detailsViewModel,
  chromeView,
  updateViewModel,
  providerRegistry,
  meterView,
  amountView,
  intervalLabel,
  parseIntervalLabel,
  valueText,
} from '../public/view-model.mjs';

const now = Date.parse('2026-10-01T15:16:00Z');
const at = (minutes) => new Date(now + minutes * 60_000).toISOString();
const window = (overrides = {}) => ({
  key: 'seven_day',
  label: 'Weekly usage',
  kind: 'rate_limit',
  usedPercent: 10,
  remainingPercent: 90,
  resetAt: at(60 * 24),
  windowMinutes: 10080,
  used: null,
  limit: null,
  unit: null,
  ...overrides,
});
const account = (overrides = {}) => ({
  id: 'codex:one',
  provider: 'codex',
  providerLabel: 'Codex',
  label: 'Codex',
  email: 'one@example.test',
  plan: 'pro',
  platform: 'ubuntu',
  source: 'Native quota',
  status: 'ok',
  message: null,
  fetchedAt: at(-1),
  sampledAt: at(-1),
  isActive: false,
  windows: [window()],
  capabilities: { codexProfile: 'one', claudeProfileId: null, claudePlatforms: [] },
  ...overrides,
});
const claude = (id, plan, windows) =>
  account({
    id: `claude:${id}`,
    provider: 'claude',
    providerLabel: 'Claude',
    email: `${id}@example.test`,
    plan,
    platform: 'mac',
    capabilities: { codexProfile: null, claudeProfileId: id, claudePlatforms: ['mac', 'windows'] },
    windows,
  });
const data = (accounts, extra = {}) => ({
  schemaVersion: 1,
  updatedAt: at(0),
  settings: { refreshIntervalSeconds: 60 },
  accounts,
  codexAutoSwitch: {
    enabled: true,
    thresholdPercent: 5,
    pollIntervalSeconds: 60,
    outcome: 'healthy',
    message: 'Healthy',
    activationInProgress: false,
  },
  ...extra,
});
const section = (vm, id) => vm.sections.find((row) => row.id === id);

test('the view model is versioned and lists Claude, Codex and Antigravity sections, then provider cards', () => {
  const vm = dashboardViewModel(
    data([
      account(),
      claude('a', 'max', [window()]),
      account({
        id: 'antigravity:a',
        provider: 'antigravity',
        email: 'ag@example.test',
        windows: [window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly' })],
        capabilities: {},
      }),
      account({
        id: 'kimi-code:a',
        provider: 'kimi-code',
        email: 'k@example.test',
        windows: [window({ key: 'weekly', label: 'Weekly' })],
      }),
      account({
        id: 'cursor:a',
        provider: 'cursor',
        email: 'c@example.test',
        windows: [window({ key: 'plan-reported', label: 'Included usage', windowMinutes: null })],
      }),
    ]),
    { now }
  );
  assert.equal(vm.version, VIEW_MODEL_VERSION);
  assert.equal(vm.version, 2);
  assert.deepEqual(
    vm.sections.map((row) => row.id),
    ['claude', 'codex', 'antigravity']
  );
  assert.deepEqual(
    vm.cards.map((card) => card.id),
    ['cursor:a', 'kimi-code:a']
  );
  assert.deepEqual(
    vm.cards.map((card) => card.provider),
    ['cursor', 'kimi-code']
  );
  assert.deepEqual(
    vm.registry.map((row) => row.id),
    ['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go']
  );
  assert.equal(vm.registry.find((row) => row.id === 'muse').count, 0);
  // no Antigravity section without Antigravity accounts
  assert.equal(section(dashboardViewModel(data([account()]), { now }), 'antigravity'), undefined);
});

test('hidden providers sent by the backend leave the dashboard and are marked in the registry', () => {
  const accounts = [
    account(),
    account({ id: 'kimi-code:a', provider: 'kimi-code', windows: [window({ key: 'weekly' })] }),
  ];
  const vm = dashboardViewModel(
    data(accounts, {
      settings: {
        refreshIntervalSeconds: 60,
        hiddenProviders: ['kimi-code', 'codex', 'not-a-provider'],
      },
    }),
    { now }
  );
  assert.equal(vm.cards.length, 0);
  assert.equal(section(vm, 'codex'), undefined);
  assert.equal(vm.registry.find((row) => row.id === 'kimi-code').visible, false);
  assert.equal(vm.registry.find((row) => row.id === 'cursor').visible, true);
  assert.deepEqual(
    providerRegistry(data(accounts)).filter((row) => !row.visible),
    []
  );
});

test('Claude shows a Fable meter only for Max plans: real when reported, "Not reported yet" when absent, never zero', () => {
  const fable = window({
    key: 'seven_day_fable',
    label: 'Weekly Fable usage',
    usedPercent: 0,
    remainingPercent: 100,
  });
  const vm = dashboardViewModel(
    data([
      claude('max-reported', 'max', [
        window({ key: 'five_hour', label: 'Five-hour usage', windowMinutes: 300 }),
        window(),
        fable,
      ]),
      claude('max-missing', 'max_20x', [window()]),
      claude('pro', 'pro', [window(), { ...fable }]),
    ]),
    { now }
  );
  const claudeSection = section(vm, 'claude');
  assert.deepEqual(
    claudeSection.columns.map((c) => c.label),
    ['5-hour', 'Weekly', 'Fable']
  );
  const [reported, missing, pro] = claudeSection.rows;
  assert.equal(reported.cells[2].hasValue, true);
  assert.equal(reported.cells[2].valueText, '0');
  assert.equal(reported.cells[2].key, 'claude:max-reported|seven_day_fable');
  assert.equal(missing.cells[2].hasValue, false);
  assert.equal(missing.cells[2].naText, 'Not reported yet');
  assert.equal(missing.cells[2].valueText, '');
  // Pro: no Fable cell on Home even when a Fable window exists (it may appear in Details)
  assert.equal(pro.cells[2].key, '');
  assert.equal(pro.cells[2].hasValue, false);
  assert.equal(pro.cells[2].naText, '');
  // a missing 5-hour reading is unavailable, not zero
  assert.equal(missing.cells[0].hasValue, false);
  assert.equal(missing.cells[0].naText, 'Unavailable');
  // no Max account: no Fable column at all
  assert.deepEqual(
    section(
      dashboardViewModel(data([claude('pro', 'pro', [window()])]), { now }),
      'claude'
    ).columns.map((c) => c.key),
    ['five', 'weekly']
  );
});

test('Codex: no 5-hour column unless one is reported, Chat pass stays hidden, the notch follows auto-switch', () => {
  const chat = window({ key: 'extra_additional_1', label: 'Chat pass · weekly', usedPercent: 50 });
  const active = account({
    id: 'codex:a',
    email: 'a@example.test',
    isActive: true,
    windows: [window({ usedPercent: 92.125, remainingPercent: 7.875 }), chat],
    capabilities: { codexProfile: 'a' },
  });
  const other = account({
    id: 'codex:b',
    email: 'b@example.test',
    windows: [
      window({ usedPercent: 9 }),
      window({
        key: 'extra_additional_2',
        label: 'Additional rate limit',
        windowMinutes: 300,
        usedPercent: 99,
      }),
    ],
    capabilities: { codexProfile: 'b' },
  });
  let codex = section(dashboardViewModel(data([active, other]), { now }), 'codex');
  assert.deepEqual(
    codex.columns.map((c) => c.key),
    ['weekly']
  );
  assert.equal(codex.switchable, true);
  assert.equal(codex.kind, 'switchable');
  assert.equal(JSON.stringify(codex).includes('Chat pass'), false);
  assert.equal(codex.rows[0].cells[0].valueText, '92.13');
  assert.equal(codex.rows[0].cells[0].notch, 95);
  assert.equal(codex.rows[0].cells[0].notchFaint, false);
  assert.equal(codex.rows[1].cells[0].notchFaint, true);
  assert.equal(codex.auto.thresholdLabel, '95%');
  assert.equal(codex.auto.thresholdUsed, 95);
  assert.equal(codex.activeId, 'codex:a');
  assert.equal(codex.rows[0].active, true);
  assert.equal(codex.rows[0].activeLabel, 'on Ubuntu');
  assert.equal(codex.rows[0].canActivate, false);
  assert.equal(codex.rows[1].canActivate, true);
  assert.equal(codex.rows[1].activateKind, 'activate');
  assert.equal(codex.rows[1].profile, 'b');
  // auto-switch off: no notch
  const off = data([active, other]);
  off.codexAutoSwitch = { ...off.codexAutoSwitch, enabled: false };
  assert.equal(section(dashboardViewModel(off, { now }), 'codex').rows[0].cells[0].notch, null);
  // one account reports the canonical five-hour window: the column appears, the other row gets an empty cell
  const plus = account({
    id: 'codex:c',
    email: 'c@example.test',
    windows: [
      window({
        key: 'five_hour',
        label: '5h',
        windowMinutes: 300,
        usedPercent: 0,
        remainingPercent: 100,
      }),
      window(),
    ],
    capabilities: { codexProfile: 'c' },
  });
  codex = section(dashboardViewModel(data([active, plus]), { now }), 'codex');
  assert.deepEqual(
    codex.columns.map((c) => c.key),
    ['five', 'weekly']
  );
  assert.equal(codex.rows[0].cells[0].key, '');
  assert.equal(codex.rows[1].cells[0].hasValue, true);
  assert.equal(codex.rows[1].cells[0].valueText, '0');
});

test('Antigravity is the same switchable section, read-only until the native inventory verifies it', () => {
  const windows = [
    window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly', usedPercent: 0.0886 }),
    window({
      key: 'gemini-5h',
      label: 'Gemini Models · 5-hour',
      windowMinutes: 300,
      usedPercent: 0,
    }),
    window({ key: '3p-weekly', label: 'Claude and GPT models · Weekly' }),
    window({ key: '3p-5h', label: 'Claude and GPT models · 5-hour', windowMinutes: 300 }),
  ];
  const ag = section(
    dashboardViewModel(
      data([
        account({
          id: 'antigravity:a',
          provider: 'antigravity',
          email: 'ag@example.test',
          plan: 'Google AI Pro',
          windows,
          capabilities: {},
        }),
      ]),
      { now }
    ),
    'antigravity'
  );
  assert.equal(ag.switchable, true);
  assert.deepEqual(
    ag.columns.map((c) => c.label),
    ['Gemini 5-hour', 'Gemini weekly', 'Claude and GPT 5-hour', 'Claude and GPT weekly']
  );
  assert.equal(ag.rows[0].cells[1].valueText, '0.09');
  assert.equal(ag.rows[0].canActivate, false);
  assert.equal(ag.rows[0].active, false);
  assert.equal(ag.rows[0].activateKind, 'antigravity-activate');
  assert.equal(ag.auto.available, false);
  assert.match(ag.auto.message, /second Antigravity account/);
});

test('meters keep raw overage and at most two decimals; packs, balances and spend are amounts, never meters', () => {
  const over = meterView(
    account(),
    window({ usedPercent: 129.567, used: 648, limit: 500, unit: 'requests' }),
    { now }
  );
  assert.equal(over.value, 129.567);
  assert.equal(over.valueText, '129.57');
  assert.equal(over.overText, '29.57');
  assert.equal(over.amount, '648 of 500 requests');
  const missing = meterView(account(), window({ usedPercent: null, remainingPercent: null }), {
    now,
  });
  assert.equal(missing.hasValue, false);
  assert.equal(missing.valueText, '');
  assert.equal(missing.reset, '');
  assert.equal(missing.naText, 'Unavailable');
  const noReset = meterView(account(), window({ resetAt: null }), { now });
  assert.equal(noReset.reset, 'no reset reported');
  assert.equal(noReset.resetExact, '');
  assert.equal(meterView(account(), window({ resetAt: at(90) }), { now }).resetSoon, true);
  assert.equal(
    meterView(account(), window({ resetAt: at(90) }), { now }).reset,
    'resets in 1h 30m'
  );
  assert.equal(valueText(0.0886), '0.09');
  const qwen = account({
    id: 'qwen:a',
    provider: 'qwen',
    windows: [
      window({
        key: 'monthly',
        label: 'Monthly',
        usedPercent: 25.061,
        windowMinutes: null,
        used: 45109.83,
        limit: 180000,
        unit: 'credits',
      }),
      window({
        key: 'subscription',
        label: 'Plan subscription',
        usedPercent: null,
        remainingPercent: null,
        expiresAt: at(60 * 24 * 19),
      }),
      window({
        key: 'addon-pack-1',
        label: 'Additional credit pack 1',
        kind: 'balance',
        usedPercent: 100,
        remaining: 0,
        limit: 20000,
        unit: 'credits',
        expiresAt: at(60 * 24 * 7),
      }),
    ],
  });
  const card = dashboardViewModel(data([qwen]), { now }).cards[0];
  assert.deepEqual(
    card.meters.map((m) => m.label),
    ['Monthly']
  );
  assert.equal(card.amounts.length, 1);
  assert.equal(card.amounts[0].label, 'Credit pack 1');
  assert.equal(card.amounts[0].value, '0');
  assert.equal(card.amounts[0].spent, true);
  // The subscription is never a row of its own: its end date is the card's one-line note (the concept's
  // "Plan subscription ends ..."), taken from the expiry the monthly window carries.
  assert.equal(
    [...card.meters, ...card.amounts].some((row) => /subscription/i.test(row.label)),
    false
  );
  assert.match(card.planNote, /^Plan subscription ends /);
  assert.equal(
    amountView(account(), {
      key: 'credits_balance',
      label: 'Extra usage credits',
      kind: 'balance',
      remaining: 62500,
      unit: 'credits',
    }).value,
    '62,500 credits'
  );
  assert.equal(
    amountView(account(), {
      key: 'extra_usage',
      label: 'Extra usage',
      kind: 'extra_usage',
      enabled: false,
      limit: 50,
      unit: 'USD',
    }).value,
    'Off'
  );
  const zai = account({
    id: 'zai:a',
    provider: 'zai',
    windows: [
      window({ key: 'usage-2', label: 'Weekly · Tokens' }),
      {
        key: 'reset-packs-5h',
        label: 'Available 5-hour reset packs',
        kind: 'balance',
        unit: 'packs',
        remaining: 0,
      },
    ],
  });
  assert.equal(dashboardViewModel(data([zai]), { now }).cards[0].amounts.length, 0);
});

test('codex rows summarise credits and banked resets of that account only', () => {
  const row = section(
    dashboardViewModel(
      data([
        account({
          windows: [
            window(),
            {
              key: 'credits_balance',
              label: 'Extra usage credits',
              kind: 'balance',
              unit: 'credits',
              remaining: 62500,
              enabled: true,
            },
            {
              key: 'banked_resets_0',
              label: 'Banked resets',
              kind: 'balance',
              unit: 'resets',
              remaining: 1,
            },
          ],
        }),
      ]),
      { now }
    ),
    'codex'
  ).rows[0];
  assert.equal(row.amountsLine, '62.5K credits · 1 banked');
});

test('the header status line is derived from the data and never claims live for cached readings', () => {
  assert.deepEqual(
    chromeView(data([account({ status: 'cached' })]), { now, intervalSeconds: 60 }).statusMore,
    'cached readings'
  );
  const live = chromeView(data([account()]), { now: Date.parse(at(0)) + 3_000 });
  assert.equal(live.statusLead, 'Updated');
  assert.equal(live.statusStrong, 'just now');
  assert.equal(live.statusMore, 'live');
  assert.equal(
    chromeView(data([account()]), { now: Date.parse(at(0)) + 125_000 }).statusStrong,
    '2m ago'
  );
  assert.equal(chromeView(data([account()]), { now, refreshing: true }).refreshing, true);
  assert.equal(chromeView(null, { now }).statusLead, 'Account data unavailable');
});

test('Details list every visible window, the amounts and the provenance of one account', () => {
  const d = data([
    claude('m', 'max', [
      window({ key: 'five_hour', label: 'Five-hour usage', windowMinutes: 300 }),
      window(),
      {
        key: 'prepaid_balance',
        label: 'Prepaid balance',
        kind: 'balance',
        unit: 'USD',
        remaining: 0,
      },
    ]),
  ]);
  const details = detailsViewModel(d, 'claude:m', { now });
  assert.equal(details.title, 'm@example.test');
  assert.deepEqual(
    details.meters.map((m) => m.label),
    ['5-hour usage', 'Weekly usage']
  );
  assert.equal(details.amounts[0].value, '$0.00');
  assert.match(details.note, /Fable usage is not reported yet/);
  assert.deepEqual(
    details.facts.map((f) => f.label),
    ['Status', 'Sampled', 'Fetched', 'Source', 'Platform', 'Profile']
  );
  assert.equal(detailsViewModel(d, 'nope', { now }), null);
});

test('refresh interval labels round-trip and Update apps reflects the job truthfully', () => {
  for (const seconds of [30, 60, 90, 120, 300, 3600])
    assert.equal(parseIntervalLabel(intervalLabel(seconds)), seconds);
  assert.equal(intervalLabel(90), '1 min 30 s');
  assert.equal(parseIntervalLabel('soon'), null);
  assert.deepEqual(
    updateViewModel({
      state: 'running',
      activePlatform: 'mac',
      results: [{ status: 'updated' }, { status: 'current' }],
    }),
    {
      running: true,
      done: false,
      count: 2,
      total: 21,
      tip: 'Updating apps on Mac · running apps may restart',
      summary: '2 results',
    }
  );
  assert.equal(
    updateViewModel({
      state: 'running',
      activePlatform: 'ubuntu',
      hosts: {
        ubuntu: { state: 'running', currentApp: 'codex-cli', phase: 'updating' },
        mac: { state: 'running', currentApp: null, phase: 'checking' },
        windows: { state: 'running', currentApp: 'omp', phase: 'checking' },
      },
      results: [],
    }).tip,
    'Updating apps on Ubuntu, Mac and Windows · running apps may restart'
  );
  assert.equal(
    updateViewModel({
      state: 'running',
      activePlatform: 'windows',
      hosts: {
        ubuntu: { state: 'done', currentApp: null, phase: null },
        mac: { state: 'done', currentApp: null, phase: null },
        windows: { state: 'running', currentApp: null, phase: 'checking' },
      },
      results: [],
    }).tip,
    'Updating apps on Windows · running apps may restart'
  );
  assert.equal(
    updateViewModel({ state: 'completed', results: [{ status: 'failed' }] }, { done: true }).done,
    true
  );
  assert.equal(updateViewModel(null).count, 0);
  assert.deepEqual(
    updateViewModel({
      state: 'completed',
      results: [
        { status: 'updated' },
        { status: 'action_required' },
        { status: 'action_required' },
      ],
    }).summary,
    '3 results · 2 need action'
  );
  assert.match(
    updateViewModel({ state: 'completed', results: [{ status: 'action_required' }] }).tip,
    /1 need action/
  );
  const held = updateViewModel({
    state: 'completed',
    results: [{ status: 'current' }, { status: 'held' }],
  });
  assert.equal(held.summary, '2 results · 1 held');
  assert.match(held.tip, /2 results, 1 held\./);
});

test('Home headers, footer and the inline confirmation read the data truthfully (W2)', () => {
  const runs = (list) => list.map((r) => (r.strong ? `*${r.text}*` : r.text)).join('');
  const a = account({
    id: 'codex:a',
    email: 'a@example.test',
    isActive: true,
    windows: [window({ usedPercent: 9 })],
    capabilities: { codexProfile: 'a' },
  });
  const b = account({
    id: 'codex:b',
    email: 'b@example.test',
    windows: [
      window({ usedPercent: 99 }),
      {
        key: 'credits_balance',
        label: 'Extra usage credits',
        kind: 'balance',
        unit: 'credits',
        remaining: 62500,
      },
      {
        key: 'banked_resets_0',
        label: 'Banked resets',
        kind: 'balance',
        unit: 'resets',
        remaining: 1,
      },
    ],
    capabilities: { codexProfile: 'b' },
  });
  const c = account({
    id: 'codex:c',
    email: 'c@example.test',
    windows: [window({ usedPercent: 16 })],
    capabilities: { codexProfile: 'c' },
  });
  const vm = dashboardViewModel(
    data([a, b, c, claude('m', 'max', [window()])], {
      codexAutoSwitch: {
        enabled: true,
        thresholdPercent: 5,
        pollIntervalSeconds: 60,
        outcome: 'healthy',
        message: 'The active Codex account has enough remaining quota.',
        activationInProgress: false,
        lastCheckedAt: at(-1),
      },
    }),
    { now }
  );
  const codex = section(vm, 'codex');
  assert.equal(runs(codex.metaRuns), '*3* accounts · *a* active');
  assert.equal(codex.metaRuns.find((r) => r.text === 'a').tone, 'good');
  assert.equal(runs(section(vm, 'claude').metaRuns), '*1* account · desktop profiles');
  assert.equal(codex.canSwitch, true);
  assert.equal(codex.auto.shown, true);
  assert.equal(codex.auto.min, 50);
  assert.equal(codex.auto.max, 99);
  // the footer keeps the backend's own words while the active account is below the switch point
  assert.deepEqual(codex.foot, {
    shown: true,
    warn: false,
    runs: [
      { text: 'The active Codex account has enough remaining quota.', strong: false, tone: '' },
    ],
    when: 'Checked 1m ago · every 1 min',
  });
  // activating an account past the switch point asks first, with its real figure; below it, it does not
  assert.equal(codex.rows[1].confirm, true);
  assert.equal(codex.rows[2].confirm, false);
  assert.equal(codex.rows[0].confirm, false);
  assert.equal(
    runs(codex.rows[1].confirmRuns),
    '*99% used*, above the 95% switch point. Auto-switch would move off it again on its next check. Activate anyway?'
  );
  assert.equal(runs(codex.rows[1].amountsRuns), '*62.5K* credits · *1* banked');
  assert.equal(codex.rows[1].amountsLine, '62.5K credits · 1 banked');
  // the active account past the point: the footer warns and names the account auto-switch moves to
  const hot = data([account({ ...a, windows: [window({ usedPercent: 97 })] }), b, c]);
  let foot = section(dashboardViewModel(hot, { now }), 'codex').foot;
  assert.equal(foot.warn, true);
  assert.equal(
    runs(foot.runs),
    '*a* is above the 95% switch point; auto-switch moves to *c* on the next check'
  );
  // a stuck monitor speaks on Home too: the server's reason plus the vetted candidate, never the unvetted "next"
  hot.codexAutoSwitch = {
    ...hot.codexAutoSwitch,
    outcome: 'waiting_idle',
    message: 'Waiting for Codex to finish active work before switching accounts.',
    candidate: 'c',
  };
  foot = section(dashboardViewModel(hot, { now }), 'codex').foot;
  assert.equal(foot.warn, true);
  assert.match(
    runs(foot.runs),
    /Waiting for Codex to finish active work before switching accounts\./
  );
  assert.match(
    runs(foot.runs),
    /Will switch to c@example\.test when Codex goes idle\. Press Activate on c@example\.test to switch now\./
  );
  assert.doesNotMatch(runs(foot.runs), /on the next check/);
  hot.codexAutoSwitch = {
    ...hot.codexAutoSwitch,
    outcome: 'no_quota',
    message: 'The reading is out of date.',
    candidate: undefined,
  };
  foot = section(dashboardViewModel(hot, { now }), 'codex').foot;
  assert.equal(foot.warn, true);
  assert.match(runs(foot.runs), /The reading is out of date\./);
  hot.codexAutoSwitch = { ...hot.codexAutoSwitch, enabled: false };
  foot = section(dashboardViewModel(hot, { now }), 'codex').foot;
  assert.equal(
    runs(foot.runs),
    '*a* is above 95% used; auto-switch is off, so it stays active until you switch'
  );
  // unknown auto-switch status: no confirm is invented and the footer says so
  const unknown = section(
    dashboardViewModel(data([a, b], { codexAutoSwitch: null }), { now }),
    'codex'
  );
  assert.equal(unknown.rows[1].confirm, false);
  assert.equal(unknown.auto.known, false);
  assert.equal(runs(unknown.foot.runs), 'Automatic switching status unavailable');
  assert.equal(unknown.foot.when, '');
  // one account: nothing to switch between
  assert.equal(section(dashboardViewModel(data([a]), { now }), 'codex').canSwitch, false);
  // one Antigravity account: the policy waits for a second one and says so instead of drawing a control
  const ag = section(
    dashboardViewModel(
      data([
        account({
          id: 'antigravity:a',
          provider: 'antigravity',
          email: 'ag@example.test',
          windows: [window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly' })],
          capabilities: {},
        }),
      ]),
      { now }
    ),
    'antigravity'
  );
  assert.equal(ag.canSwitch, false);
  assert.equal(ag.auto.shown, false);
  assert.equal(runs(ag.auto.offRuns), 'Auto-switch *off* · needs a second account');
  assert.equal(runs(ag.metaRuns), 'Google Antigravity CLI · *1* account');
  assert.deepEqual(ag.foot, { shown: false, warn: false, runs: [], when: '' });
  const paused = section(
    dashboardViewModel(
      data([
        account({
          id: 'antigravity:a',
          provider: 'antigravity',
          email: 'ag@example.test',
          windows: [window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly' })],
          capabilities: {},
        }),
      ]),
      { now, antigravityInventory: { nativeUpdatePaused: { installedVersion: '1.2.17' } } }
    ),
    'antigravity'
  );
  assert.equal(paused.foot.shown, true);
  assert.equal(paused.foot.warn, true);
  assert.equal(
    runs(paused.foot.runs),
    'Antigravity updated to 1.2.17; switching paused until reviewed'
  );
  const failed = section(
    dashboardViewModel(
      data([
        account({
          id: 'antigravity:a',
          provider: 'antigravity',
          email: 'ag@example.test',
          windows: [window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly' })],
          capabilities: {},
        }),
      ]),
      {
        now,
        antigravityInventory: {
          nativeUpdatePaused: { installedVersion: '1.2.17' },
          runtimeServiceProblem: {
            reason: 'missing-python-module',
            module: 'pyte',
            python: '3.14',
            builtFor: '3.13',
            exitStatus: null,
          },
        },
      }
    ),
    'antigravity'
  );
  assert.equal(failed.foot.shown, true);
  assert.equal(failed.foot.warn, true);
  assert.equal(
    runs(failed.foot.runs),
    'Runtime service failed: missing Python module pyte (Python 3.14; runtime built for 3.13); switching is off until the runtime bundle is rebuilt'
  );
  assert.equal(failed.rows[0].canActivate, false);
  assert.equal(
    failed.rows[0].activateHint,
    'Runtime service failed: missing Python module pyte (Python 3.14; runtime built for 3.13); switching is off until the runtime bundle is rebuilt'
  );
});

test('hiding one of two accounts from the dashboard keeps switching for the other (Codex and Antigravity)', () => {
  const ag = (id) =>
    account({
      id: `antigravity:${id}`,
      provider: 'antigravity',
      email: `${id}@example.test`,
      windows: [window({ key: 'gemini-weekly', label: 'Gemini Models · Weekly' })],
      capabilities: {},
    });
  const accounts = [
    account({ id: 'codex:a', isActive: true }),
    account({ id: 'codex:b', email: 'b@example.test' }),
    ag('x'),
    ag('y'),
  ];
  const byList = data(accounts, {
    settings: { refreshIntervalSeconds: 60, hiddenAccountIds: ['codex:b', 'antigravity:y'] },
  });
  const vm = dashboardViewModel(byList, { now });
  const codex = section(vm, 'codex');
  const agy = section(vm, 'antigravity');
  // Home lists only the shown rows, but the "two accounts" rule counts every account, as the server does
  assert.deepEqual(
    codex.rows.map((row) => row.id),
    ['codex:a']
  );
  assert.deepEqual(
    agy.rows.map((row) => row.id),
    ['antigravity:x']
  );
  assert.equal(codex.canSwitch, true);
  assert.equal(agy.canSwitch, true);
  assert.equal(agy.auto.shown, true);
  assert.deepEqual(agy.auto.offRuns, []);
  assert.notEqual(agy.auto.message, 'Automatic switching needs a second Antigravity account.');
  // the row's own flag (accounts[].hidden) leaves Home the same way and keeps switching too
  const byFlag = dashboardViewModel(
    data(accounts.map((a) => (a.id === 'antigravity:y' ? { ...a, hidden: true } : a))),
    { now }
  );
  assert.deepEqual(
    section(byFlag, 'antigravity').rows.map((row) => row.id),
    ['antigravity:x']
  );
  assert.equal(section(byFlag, 'antigravity').canSwitch, true);
  // hidden only from the tray: Home keeps both rows
  const trayOnly = dashboardViewModel(
    data(accounts.map((a) => (a.id === 'antigravity:y' ? { ...a, trayHidden: true } : a))),
    { now }
  );
  assert.equal(section(trayOnly, 'antigravity').rows.length, 2);
});

test('provider cards carry the concept footer, plan note and shared pack expiry without inventing readings (W2)', () => {
  const qwen = account({
    id: 'qwen:a',
    provider: 'qwen',
    email: 'q@example.test',
    plan: 'pro',
    platform: 'windows',
    status: 'cached',
    sampledAt: at(-2),
    windows: [
      window({
        key: 'monthly',
        label: 'Monthly',
        usedPercent: 25.061,
        windowMinutes: null,
        used: 45109.83,
        limit: 180000,
        unit: 'credits',
      }),
      window({
        key: 'subscription',
        label: 'Plan subscription',
        usedPercent: null,
        remainingPercent: null,
        expiresAt: at(60 * 24 * 19),
      }),
      window({
        key: 'addon-pack-1',
        label: 'Additional credit pack 1',
        kind: 'balance',
        usedPercent: null,
        remaining: 20000,
        limit: 20000,
        unit: 'credits',
        expiresAt: at(60 * 24 * 7),
      }),
      window({
        key: 'addon-pack-2',
        label: 'Additional credit pack 2',
        kind: 'balance',
        usedPercent: null,
        remaining: 0,
        limit: 20000,
        unit: 'credits',
        expiresAt: at(60 * 24 * 7),
      }),
    ],
  });
  const muse = account({
    id: 'muse:a',
    provider: 'muse',
    email: 'm@example.test',
    plan: 'Muse Code High Usage',
    platform: 'mac',
    status: 'cached',
    sampledAt: at(-120),
    windows: [
      window({ key: 'weekly', label: 'Weekly', usedPercent: null, remainingPercent: null }),
    ],
  });
  const kimi = account({
    id: 'kimi-code:a',
    provider: 'kimi-code',
    email: 'k@example.test',
    plan: null,
    status: 'needs_sign_in',
    windows: [window({ key: 'weekly', label: 'Weekly' })],
  });
  const [k, m, q] = ['kimi-code', 'muse', 'qwen'].map((p) =>
    dashboardViewModel(data([qwen, muse, kimi]), { now }).cards.find((card) => card.provider === p)
  );
  assert.equal(q.plan, 'Pro');
  assert.equal(q.flag, '');
  assert.equal(q.sampled, 'sampled 2m ago');
  assert.equal(q.platform, 'Windows');
  assert.match(q.planNote, /^Plan subscription ends /);
  assert.match(q.packsNote, /^All 2 packs expire /);
  assert.equal(
    q.amounts.every((row) => !/Expires/.test(row.sub)),
    true,
    'the shared expiry is said once'
  );
  assert.equal(q.meters[0].amount, '45.11K of 180,000 credits');
  // the provider's own name is not repeated as the plan; an old sample is flagged, an unread window stays unavailable
  assert.equal(m.plan, 'High Usage');
  assert.equal(m.flag, 'Stale');
  assert.equal(m.meters.length, 1);
  assert.equal(m.meters[0].hasValue, false);
  assert.equal(m.meters[0].naText, 'Unavailable');
  assert.equal(k.plan, '');
  assert.equal(k.flag, 'Sign-in needed');
});

test('Details carries the row slot and the same inline confirmation (W2)', () => {
  const a = account({
    id: 'codex:a',
    email: 'a@example.test',
    isActive: true,
    windows: [window({ usedPercent: 9 })],
    capabilities: { codexProfile: 'a' },
  });
  const b = account({
    id: 'codex:b',
    email: 'b@example.test',
    windows: [window({ usedPercent: 99 })],
    capabilities: { codexProfile: 'b' },
  });
  const details = detailsViewModel(data([a, b]), 'codex:b', { now });
  assert.equal(details.canSwitch, true);
  assert.equal(details.confirm, true);
  assert.equal(details.platform, 'Ubuntu');
  assert.equal(details.confirmRuns[0].text, '99% used');
  assert.equal(details.subLead, 'Codex · Pro');
  const active = detailsViewModel(data([a, b]), 'codex:a', { now });
  assert.equal(active.active, true);
  assert.equal(active.activeLabel, 'on Ubuntu');
  assert.equal(active.confirm, false);
  assert.equal(detailsViewModel(data([a]), 'codex:a', { now }).canSwitch, false);
});

test('F6: a reading sampled before a reset that has passed shows "Reset at … · new reading pending", never the old percent or 0%', async () => {
  const { pendingReset, currentUsedPercent } = await import('../public/view-model.mjs');
  const passed = window({
    key: 'five_hour',
    label: 'Five-hour usage',
    windowMinutes: 300,
    usedPercent: 88,
    remainingPercent: 12,
    resetAt: at(-30),
  });
  const before = claude('pending', 'max', [
    passed,
    window({ usedPercent: 40, remainingPercent: 60 }),
  ]);
  before.sampledAt = at(-45);
  const vm = dashboardViewModel(data([before]), { now });
  const cell = section(vm, 'claude').rows[0].cells[0];
  assert.equal(pendingReset(before, passed, now), true);
  assert.equal(currentUsedPercent(before, passed, now), null);
  assert.equal(cell.hasValue, false);
  assert.equal(cell.valueText, '');
  assert.equal(cell.value, 0);
  assert.equal(cell.notch, null);
  assert.match(cell.naText, /^Reset at \S/);
  assert.equal(cell.naSub, 'New reading pending');
  assert.match(cell.resetExact, /^Reset at .* · new reading pending$/);
  assert.doesNotMatch(JSON.stringify(cell), /88|"0%"/);
  // the weekly window has not reset: its reading stays
  assert.equal(section(vm, 'claude').rows[0].cells[1].valueText, '40');

  // sampled after the reset: a real new reading, shown as usual
  const after = { ...before, sampledAt: at(-10) };
  assert.equal(pendingReset(after, passed, now), false);
  assert.equal(meterView(after, passed, { now }).valueText, '88');
  // the window's own sample time comes first
  assert.equal(pendingReset(after, { ...passed, sampledAt: at(-40) }, now), true);
  assert.equal(pendingReset(before, { ...passed, sampledAt: at(-5) }, now), false);
  // unknown sample time: pending
  assert.equal(pendingReset({ ...before, sampledAt: null }, passed, now), true);
  // a reset still ahead is not pending
  assert.equal(pendingReset(before, { ...passed, resetAt: at(30) }, now), false);
  // amounts, unlimited and switched-off windows are left alone
  assert.equal(pendingReset(before, { ...passed, kind: 'spend' }, now), false);
  assert.equal(pendingReset(before, { ...passed, unlimited: true }, now), false);
  assert.equal(pendingReset(before, { ...passed, enabled: false }, now), false);
  // Details reads the same
  const details = detailsViewModel(data([before]), 'claude:pending', { now });
  const meter = details.meters.find((m) => m.key === 'claude:pending|five_hour');
  assert.equal(meter.hasValue, false);
  assert.equal(meter.naSub, 'New reading pending');
});

test('F6: a Codex window pending its reset has no notch and does not count toward the switch point', () => {
  const pendingWeek = window({ usedPercent: 99, remainingPercent: 1, resetAt: at(-5) });
  const active = account({
    id: 'codex:a',
    email: 'a@example.test',
    isActive: true,
    sampledAt: at(-20),
    windows: [pendingWeek],
    capabilities: { codexProfile: 'a' },
  });
  const other = account({
    id: 'codex:b',
    email: 'b@example.test',
    windows: [window({ usedPercent: 10, remainingPercent: 90 })],
    capabilities: { codexProfile: 'b' },
  });
  const vm = dashboardViewModel(data([active, other]), { now });
  const codex = section(vm, 'codex');
  const weekly = codex.rows[0].cells.at(-1);
  assert.equal(weekly.hasValue, false);
  assert.equal(weekly.notch, null);
  assert.equal(weekly.naSub, 'New reading pending');
  // the footer no longer says the active account is above the switch point on the stale 99%
  assert.doesNotMatch(codex.foot.runs.map((r) => r.text).join(''), /above/);
});

test('a limit of 100 with no unit only restates the percent, so cards show no "N of 100" caption', () => {
  const kimi = account({
    id: 'kimi-code:a',
    provider: 'kimi-code',
    email: 'k@example.test',
    windows: [
      window({
        key: '5h',
        label: '5 hours',
        windowMinutes: 300,
        usedPercent: 99,
        remainingPercent: 1,
        used: 99,
        limit: 100,
        unit: null,
      }),
      window({
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 20,
        remainingPercent: 80,
        used: 20,
        limit: 100,
        unit: null,
      }),
    ],
  });
  const card = dashboardViewModel(data([kimi]), { now }).cards[0];
  assert.deepEqual(
    card.meters.map((m) => m.amount),
    ['', '']
  );
  // another provider with the same percent-scale window loses its caption too
  assert.equal(
    meterView(
      account(),
      window({ usedPercent: 50, remainingPercent: 50, used: 50, limit: 100, unit: null }),
      { now }
    ).amount,
    ''
  );
  // counts that carry real information stay: another limit, or a named unit
  assert.equal(
    meterView(account(), window({ used: 340, limit: 500, unit: 'requests' }), { now }).amount,
    '340 of 500 requests'
  );
  assert.equal(
    meterView(account(), window({ used: 50, limit: 100, unit: 'requests' }), { now }).amount,
    '50 of 100 requests'
  );
});

test('Home Claude rows say which computer needs a sign-in before Open (fake profiles)', () => {
  const needs = {
    ...claude('fake-one', 'max', [window()]),
    status: 'cached',
    signInNeeded: ['windows'],
  };
  const vm = dashboardViewModel(data([needs]), { now });
  const r = section(vm, 'claude').rows.find((x) => x.id === 'claude:fake-one');
  assert.equal(r.status, 'Sign-in needed on Windows');
  assert.match(r.meta, /· Sign-in needed on Windows$/);
  const both = dashboardViewModel(
    data([
      {
        ...claude('fake-two', 'pro', []),
        status: 'needs_sign_in',
        signInNeeded: ['windows', 'mac'],
      },
    ]),
    { now }
  );
  const r2 = section(both, 'claude').rows.find((x) => x.id === 'claude:fake-two');
  assert.equal(r2.meta, 'Pro · Sign-in needed on Mac and Windows');
  const plain = dashboardViewModel(data([claude('fake-three', 'max', [window()])]), { now });
  assert.doesNotMatch(
    section(plain, 'claude').rows.find((x) => x.id === 'claude:fake-three').meta,
    /Sign-in/
  );
});
