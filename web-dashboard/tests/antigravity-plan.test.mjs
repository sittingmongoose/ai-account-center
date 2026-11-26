import test from 'node:test';
import assert from 'node:assert/strict';
import { dashboardViewModel, detailsViewModel } from '../public/view-model.mjs';
import { antigravityColumns, antigravityMissingWindow } from '../public/antigravity-data.mjs';
import { accountsViewModel } from '../public/accounts-view.mjs';

const now = Date.parse('2026-11-02T00:00:00Z');
const window = (key, usedPercent = 25) => ({
  key,
  label: key,
  kind: 'rate_limit',
  usedPercent,
  remainingPercent: 100 - usedPercent,
  windowMinutes: key.endsWith('5h') ? 300 : 10080,
  resetAt: new Date(now + 3600000).toISOString(),
});
const account = (id, plan, windows) => ({
  id: `antigravity:profile:${id}`,
  provider: 'antigravity',
  providerLabel: 'Antigravity',
  plan: 'Reported plan',
  email: `${id}@example.com`,
  platform: 'ubuntu',
  status: 'ok',
  fetchedAt: new Date(now).toISOString(),
  sampledAt: new Date(now).toISOString(),
  windows,
  ...(plan ? { antigravityPlan: plan } : {}),
  capabilities: {
    antigravityProfileId: id,
    antigravityHostIds: ['ubuntu'],
  },
});
const data = (accounts) => ({ schemaVersion: 1, updatedAt: new Date(now).toISOString(), accounts });
const section = (accounts) =>
  dashboardViewModel(data(accounts), { now }).sections.find(
    (section) => section.id === 'antigravity'
  );

test('Home shows T3 pool columns only, keeps the provider keys and shows missing plan windows honestly', () => {
  const weekly = account('free', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('cli-gemini-weekly', 0),
  ]);
  const old = account('old', null, [window('cli-gemini-5h')]);
  const v = section([weekly, old]);
  assert.deepEqual(
    v.columns.map((column) => column.key),
    ['cli-gemini-5h', 'cli-gemini-weekly', 'cli-3p-5h', 'cli-3p-weekly']
  );
  assert.deepEqual(
    v.columns.map((column) => column.label),
    [
      'Gemini 5-hour, in T3 (Antigravity CLI)',
      'Gemini weekly, in T3 (Antigravity CLI)',
      'Claude and GPT 5-hour, in T3 (Antigravity CLI)',
      'Claude and GPT weekly, in T3 (Antigravity CLI)',
    ]
  );
  assert.equal(v.rows[0].cells[0].naText, 'Weekly only');
  assert.equal(v.rows[0].cells[1].valueText, '0');
  assert.equal(v.rows[0].cells[1].hasValue, true);
  for (const i of [2, 3]) {
    assert.equal(v.rows[0].cells[i].naText, 'Not on plan');
    assert.equal(v.rows[0].cells[i].hasValue, false);
    assert.equal(v.rows[0].cells[i].valueText, '');
  }
  assert.equal(v.rows[1].cells[1].naText, 'Unavailable');
  assert.equal(
    antigravityMissingWindow(
      account('plus', { quotaPolicy: 'weekly', thirdPartyModels: true }, []),
      'cli-3p-5h'
    ),
    'Weekly only'
  );
});

test('reported windows win over plan metadata and unknown extra windows keep generic captions', () => {
  const row = account('a', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('cli-3p-5h'),
    {
      ...window('cli-new-pool'),
      label: 'New provider quota',
      usedPercent: null,
      remainingPercent: null,
    },
  ]);
  const v = section([row]);
  assert.equal(v.rows[0].cells[2].hasValue, true);
  assert.equal(v.rows[0].cells[2].valueText, '25');
  assert.equal(v.columns.at(-1).label, 'New provider quota');
  assert.equal(v.rows[0].cells.at(-1).naText, 'Unavailable');
  assert.deepEqual(
    antigravityColumns([
      account('old', null, [{ ...window('cli-unknown'), label: 'Old provider quota' }]),
    ]),
    [{ key: 'cli-unknown', label: 'Old provider quota' }]
  );
});

test('an extra pool another account reports reads Not reported, never Unavailable or zero', () => {
  // Shapes from the 2026-11-13 quota reads: the Pro account lists a Gemini 4 Argon weekly
  // bucket; the free-tier account's reading has only the two weekly buckets.
  const argon = {
    ...window('cli-gemini-argon-wtu-weekly', 1),
    label: 'Gemini 4 Argon · Weekly',
  };
  const pro = account('pro', { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true }, [
    window('cli-gemini-5h', 1),
    window('cli-gemini-weekly', 1),
    window('cli-3p-5h', 0),
    window('cli-3p-weekly', 0),
    argon,
  ]);
  const free = account('free', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('cli-gemini-weekly', 0),
    window('cli-3p-weekly', 0),
  ]);
  const failed = { ...account('failed', null, []), status: 'unavailable' };
  // A cached reading is the last good one after a failed refresh (possibly from before Argon
  // existed), and an ok row can still carry cached windows: neither may claim "Not reported".
  const cached = {
    ...account('cached', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
      window('cli-gemini-weekly', 0),
      window('cli-3p-weekly', 0),
    ]),
    status: 'cached',
    sampledAt: new Date(now - 3 * 86400000).toISOString(),
  };
  const cachedWindows = account(
    'cached-windows',
    { quotaPolicy: 'weekly', thirdPartyModels: false },
    [{ ...window('cli-gemini-weekly', 0), status: 'cached', cached: true }]
  );
  const v = section([pro, free, failed, cached, cachedWindows]);
  const at = v.columns.findIndex((column) => column.key === 'cli-gemini-argon-wtu-weekly');
  assert.equal(v.columns[at].label, 'Gemini 4 Argon · Weekly');
  assert.equal(v.rows[0].cells[at].hasValue, true);
  assert.equal(v.rows[0].cells[at].valueText, '1');
  const missing = v.rows[1].cells[at];
  assert.equal(missing.hasValue, false);
  assert.equal(missing.valueText, '');
  assert.equal(missing.naText, 'Not reported');
  // Plain like "Weekly only": no second line, so it never takes the pending-reset look.
  assert.equal(missing.naSub, '');
  // The canonical columns keep their plan wording.
  assert.equal(v.rows[1].cells[0].naText, 'Weekly only');
  assert.equal(v.rows[1].cells[0].naSub, '');
  // An account with no reading at all is still unavailable, not "not reported".
  assert.equal(v.rows[2].cells[at].naText, 'Unavailable');
  assert.equal(v.rows[2].cells[at].naSub, '');
  for (const row of [3, 4]) {
    assert.equal(v.rows[row].cells[at].naText, 'Unavailable');
    assert.equal(v.rows[row].cells[at].hasValue, false);
    assert.equal(v.rows[row].cells[at].valueText, '');
  }
});

// Google's two separate Antigravity limits: T3 (the Antigravity CLI, keys "cli-...") and the
// Antigravity app. Shapes from the 2026-11-19 ~23:56Z readings, after a ~25M-token Gemini 4 Neon run
// in T3: the CLI's Gemini windows moved, the app's did not. Never added together.
const service = (prefix, where, values) =>
  Object.entries(values).map(([key, used]) => ({
    ...window(prefix + key, used),
    label: `${{ 'gemini-5h': 'Gemini Models · 5-hour', 'gemini-weekly': 'Gemini Models · Weekly', '3p-5h': 'Claude and GPT models · 5-hour', '3p-weekly': 'Claude and GPT models · Weekly', 'gemini-argon-wtu-weekly': 'Gemini 4 Argon · Weekly' }[key]} · ${where}`,
  }));
const T3_WHERE = 'In T3 (Antigravity CLI)';
const APP_WHERE = 'In the Antigravity app';
const tonightT3 = service('cli-', T3_WHERE, {
  'gemini-weekly': 8.48,
  'gemini-5h': 29.68,
  '3p-weekly': 0,
  '3p-5h': 0,
  'gemini-argon-wtu-weekly': 0,
});
const tonightApp = service('', APP_WHERE, {
  'gemini-weekly': 0.09,
  'gemini-5h': 0,
  '3p-weekly': 0,
  '3p-5h': 0,
  'gemini-argon-wtu-weekly': 37.83,
});

// A quota service that failed this read arrives as its pools with no numbers and status "unavailable".
const unavailable = (windows) =>
  windows.map((w) => ({
    ...w,
    usedPercent: null,
    remainingPercent: null,
    resetAt: null,
    status: 'unavailable',
  }));

test('Home and Accounts carry no app pool columns; Details still shows both services', () => {
  // The owner, 2026-11-19: the Antigravity app's numbers belong in Details, not on the main page.
  const pro = account('gmail', { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true }, [
    ...tonightT3,
    ...tonightApp,
  ]);
  const vm = dashboardViewModel(data([pro]), { now });
  const v = vm.sections.find((section) => section.id === 'antigravity');
  assert.deepEqual(
    v.columns.map((column) => column.key),
    [
      'cli-gemini-5h',
      'cli-gemini-weekly',
      'cli-3p-5h',
      'cli-3p-weekly',
      'cli-gemini-argon-wtu-weekly',
    ]
  );
  const cell = (key) => v.rows[0].cells[v.columns.findIndex((column) => column.key === key)];
  assert.equal(cell('cli-gemini-5h').valueText, '29.68');
  assert.equal(cell('cli-gemini-weekly').valueText, '8.48');
  assert.equal(cell('cli-gemini-argon-wtu-weekly').valueText, '0');
  assert.equal(v.columns.at(-1).label, 'Gemini 4 Argon · Weekly · In T3 (Antigravity CLI)');
  assert.equal(v.columns[0].shortLabel, 'T3 Gemini 5hr');
  // No app column, label or number anywhere on Home.
  const home = JSON.stringify(vm.sections);
  assert.equal(home.includes('In the Antigravity app'), false);
  assert.equal(home.includes('37.83'), false);
  assert.equal(home.includes('0.09'), false);
  // The Accounts page carries no app pool either.
  const accounts = JSON.stringify(accountsViewModel(data([pro]), { now }));
  assert.equal(accounts.includes('In the Antigravity app'), false);
  assert.equal(accounts.includes('37.83'), false);
  // Details: both services, each labelled, never summed.
  const details = detailsViewModel(data([pro]), pro.id, { now });
  const meter = (key) => details.meters.find((m) => m.key === `${pro.id}|${key}`);
  assert.equal(meter('cli-gemini-5h')?.valueText, '29.68');
  assert.equal(meter('gemini-weekly')?.valueText, '0.09');
  assert.equal(meter('gemini-argon-wtu-weekly')?.valueText, '37.83');
  assert.equal(meter('gemini-weekly')?.label, 'Gemini Models · Weekly · In the Antigravity app');
  assert.equal(
    meter('cli-gemini-weekly')?.label,
    'Gemini Models · Weekly · In T3 (Antigravity CLI)'
  );
  assert.equal(details.meters.length, 10);
});

test('an unavailable T3 side shows Unavailable on Home and the app numbers never replace it', () => {
  const plan = { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true };
  const appOnly = account('app-only', plan, tonightApp); // T3's service gave nothing at all
  const failedT3 = account('failed-t3', plan, [...unavailable(tonightT3), ...tonightApp]);
  const t3Only = account('t3-only', plan, tonightT3); // the app's service did not answer
  const v = section([appOnly, failedT3, t3Only]);
  const at = (key) => v.columns.findIndex((column) => column.key === key);
  assert.deepEqual(
    v.columns.map((column) => column.key),
    [
      'cli-gemini-5h',
      'cli-gemini-weekly',
      'cli-3p-5h',
      'cli-3p-weekly',
      'cli-gemini-argon-wtu-weekly',
    ]
  );
  for (const row of [0, 1])
    for (const key of v.columns.map((column) => column.key)) {
      const missing = v.rows[row].cells[at(key)];
      assert.equal(missing.naText, 'Unavailable', `${row} ${key}`);
      assert.equal(missing.hasValue, false, `${row} ${key}`);
      assert.equal(missing.valueText, '', `${row} ${key}`);
    }
  assert.equal(v.rows[2].cells[at('cli-gemini-5h')].valueText, '29.68');
  assert.equal(JSON.stringify(v).includes('37.83'), false); // the app's Argon never stands in
  assert.equal(antigravityMissingWindow(appOnly, 'cli-gemini-argon-wtu-weekly'), 'Unavailable');
});

test('a free account reads 0 on T3 weekly only on Home, and 0 on both services in Details', () => {
  const zero = { 'gemini-weekly': 0, '3p-weekly': 0 };
  const free = account('party', { quotaPolicy: 'weekly', thirdPartyModels: true }, [
    ...service('cli-', T3_WHERE, zero),
    ...service('', APP_WHERE, zero),
  ]);
  const v = section([free]);
  const cells = Object.fromEntries(v.columns.map((column, i) => [column.key, v.rows[0].cells[i]]));
  assert.deepEqual(Object.keys(cells), [
    'cli-gemini-5h',
    'cli-gemini-weekly',
    'cli-3p-5h',
    'cli-3p-weekly',
  ]);
  for (const key of ['cli-gemini-weekly', 'cli-3p-weekly']) {
    assert.equal(cells[key].hasValue, true, key);
    assert.equal(cells[key].valueText, '0', key);
  }
  for (const key of ['cli-gemini-5h', 'cli-3p-5h'])
    assert.equal(cells[key].naText, 'Weekly only', key);
  const meters = detailsViewModel(data([free]), free.id, { now }).meters.map((m) => [
    m.key.split('|')[1],
    m.valueText,
  ]);
  assert.deepEqual(meters, [
    ['cli-gemini-weekly', '0'],
    ['cli-3p-weekly', '0'],
    ['gemini-weekly', '0'],
    ['3p-weekly', '0'],
  ]);
});

test('a failed T3 service shows Unavailable in Details with a notice while the app stays live', () => {
  const pro = account('gmail', { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true }, [
    ...unavailable(tonightT3),
    ...tonightApp,
  ]);
  const details = detailsViewModel(data([pro]), pro.id, { now });
  const notice = details.facts.find((fact) => fact.label === 'Notice');
  assert.equal(
    notice?.value,
    'T3 (Antigravity CLI) quota could not be read this time; its pools show Unavailable. The other readings are live.'
  );
  assert.equal(details.facts.filter((fact) => fact.label === 'Notice').length, 1);
  const meter = (key) => details.meters.find((m) => m.key === `${pro.id}|${key}`);
  for (const key of ['cli-gemini-5h', 'cli-gemini-weekly', 'cli-gemini-argon-wtu-weekly']) {
    assert.equal(meter(key)?.hasValue, false, key);
    assert.equal(meter(key)?.naText, 'Unavailable', key);
    assert.equal(meter(key)?.valueText, '', key);
  }
  assert.equal(meter('gemini-weekly')?.valueText, '0.09');
  assert.equal(meter('gemini-argon-wtu-weekly')?.valueText, '37.83');
  // Home keeps the failed service's known pool columns, Argon included, as Unavailable.
  const v = section([pro]);
  const at = v.columns.findIndex((column) => column.key === 'cli-gemini-argon-wtu-weekly');
  assert.notEqual(at, -1);
  assert.equal(v.rows[0].cells[at].naText, 'Unavailable');
  assert.equal(v.rows[0].cells[at].hasValue, false);
  // A healthy reading has no notice.
  const healthy = account('ok', { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true }, [
    ...tonightT3,
    ...tonightApp,
  ]);
  assert.equal(
    detailsViewModel(data([healthy]), healthy.id, { now }).facts.some((f) => f.label === 'Notice'),
    false
  );
});

test('a failed service is never a live reading of that service, so its missing pools stay Unavailable', () => {
  const plan = { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true };
  const failedT3 = account('failed-t3', plan, [...unavailable(tonightT3), ...tonightApp]);
  assert.equal(antigravityMissingWindow(failedT3, 'cli-new-pool'), 'Unavailable');
  assert.equal(antigravityMissingWindow(failedT3, 'new-pool'), 'Not reported');
});

test('a spend-only, extra-usage-only or passed-reset reading is not live, so missing pools stay Unavailable', () => {
  // The Mac tray's isMeter rule: amounts (balance, spend, extra_usage) are not meters, and a
  // reading sampled before its reset no longer describes the window (resetPassed). Neither
  // proves Google reported no such quota, so an extra pool cell must not read "Not reported".
  const plan = { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true };
  const amountOnly = (id, kind) => account(id, plan, [
    { ...window('cli-gemini-5h'), kind },
    { ...window('gemini-5h'), kind },
  ]);
  const passedReset = account('passed-reset', plan, [
    { ...window('cli-gemini-5h'), resetPassed: true },
    { ...window('gemini-5h'), resetPassed: true },
  ]);
  for (const row of [amountOnly('spend-only', 'spend'), amountOnly('extra-only', 'extra_usage'), passedReset]) {
    assert.equal(antigravityMissingWindow(row, 'cli-new-pool'), 'Unavailable');
    assert.equal(antigravityMissingWindow(row, 'new-pool'), 'Unavailable');
  }
  // A live meter beside an amount or a passed-reset window still reports the extra pool.
  const live = account('live', plan, [
    window('cli-gemini-5h'),
    { ...window('cli-gemini-weekly'), kind: 'spend' },
    { ...window('gemini-5h'), resetPassed: true },
    window('gemini-weekly'),
  ]);
  assert.equal(antigravityMissingWindow(live, 'cli-new-pool'), 'Not reported');
  assert.equal(antigravityMissingWindow(live, 'new-pool'), 'Not reported');
});

test('an app-only row never adds app columns to Home, even listed first', () => {
  const plan = { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true };
  const appOnly = account('app-only', plan, tonightApp);
  const both = account('both', plan, [...tonightT3, ...tonightApp]);
  const keys = section([appOnly, both]).columns.map((column) => column.key);
  assert.deepEqual(keys, [
    'cli-gemini-5h',
    'cli-gemini-weekly',
    'cli-3p-5h',
    'cli-3p-weekly',
    'cli-gemini-argon-wtu-weekly',
  ]);
});

test('no-quota plans with no windows use the existing row text and retain reported windows when present', () => {
  const empty = account(
    'workspace',
    { quotaPolicy: 'none', models: [], creditsOverage: false },
    []
  );
  const v = section([empty, account('pro', null, [window('cli-gemini-5h')])]);
  assert.equal(v.rows[0].cells.length, v.columns.length);
  assert.ok(
    v.rows[0].cells.every((cell) => cell.key === '' && !cell.hasValue && cell.naText === '')
  );
  assert.equal(v.rows[0].amountsLine, 'No Antigravity quota on this plan');
  assert.equal(v.rows[0].amountsRuns[0].text, 'No Antigravity quota on this plan');
  assert.equal(
    section([{ ...empty, windows: [window('cli-gemini-weekly')] }]).rows[0].cells[1].hasValue,
    true
  );
  assert.equal(section([account('unknown', null, [])]).rows[0].amountsLine, '');
});

test('details carry optional plan summary, models, overage hint and family note through existing rows', () => {
  const plan = {
    class: 'ultra-20x',
    quotaPolicy: 'five-hour-weekly',
    summary: '20x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit.',
    models: ['Gemini 3.8 Flash', 'Claude Sonnet 5.5'],
    thirdPartyModels: true,
    creditsOverage: true,
  };
  const credits = {
    key: 'google-ai-credits',
    label: 'Google AI credits',
    kind: 'balance',
    remaining: 12,
    unit: 'credits',
  };
  const row = account('a', plan, [window('gemini-weekly'), credits]);
  const v = detailsViewModel(data([row]), row.id, { now });
  assert.equal(v.facts.find((fact) => fact.label === 'Models').value, 'Gemini; Claude 5.5');
  const note = v.note.replace(/\s+/g, ' ');
  assert.ok(note.includes(plan.summary));
  assert.ok(note.includes(`Models: ${plan.models.join(', ')}`));
  assert.ok(note.includes('Family members sharing this plan may share one quota pool.'));
  assert.ok(v.note.split('\n').every((line) => line.length <= 40));
  assert.equal(v.amounts[0].label, 'AI credits (overage)');
  assert.equal(
    v.amounts[0].sub,
    'Used only after the plan quota runs out, when AI Credit Overages is on.'
  );
  assert.equal(
    detailsViewModel(
      data([{ ...row, antigravityPlan: { creditsOverage: false, models: [] } }]),
      row.id,
      { now }
    ).amounts[0].sub,
    'Not usable for Antigravity on this plan.'
  );
  const legacy = detailsViewModel(data([{ ...row, antigravityPlan: undefined }]), row.id, { now });
  assert.ok(
    !legacy.facts.some((fact) => ['Plan', 'Models', 'Family sharing'].includes(fact.label))
  );
});

test('details group the 2026-11-19 Ultra models, Orchestrators included, under short names', () => {
  const plan = {
    class: 'ultra',
    quotaPolicy: 'five-hour-weekly',
    summary: 'Refreshes every 5 hours up to a weekly limit.',
    models: [
      'Gemini 4 Neon',
      'Gemini 4 Carbon',
      'Gemini 4 Argon',
      'Gemini 4 Hydrogen',
      'Orchestrator (Neon)',
      'Orchestrator (Carbon)',
      'Orchestrator (Argon)',
      'Gemini 3.8 Flash',
      'Claude Sonnet 5.5',
      'Claude Opus 5.5',
      'GPT-OSS-120B',
    ],
    thirdPartyModels: true,
    creditsOverage: true,
  };
  const row = account('ultra', plan, [window('gemini-weekly'), window('gemini-5h')]);
  const v = detailsViewModel(data([row]), row.id, { now });
  assert.equal(
    v.facts.find((fact) => fact.label === 'Models').value,
    'Gemini; Orchestrator; Claude 5.5; GPT-OSS'
  );
  assert.ok(v.note.replace(/\s+/g, ' ').includes('Orchestrator (Carbon)'));
});

test('a passed-reset extra pool reads Unavailable on Home', () => {
  // M1: one account reports Gemini 4 Argon (the shared CLI column). The other account's only
  // CLI meters passed their resets, so they no longer describe those windows and cannot prove
  // Google reported no Argon quota there: the cell is Unavailable, exactly as Details says.
  const plan = { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true };
  const pro = account('argon', plan, [...tonightT3, ...tonightApp]);
  const passed = account('passed', plan, [
    { ...window('cli-gemini-weekly'), resetPassed: true },
    { ...window('cli-3p-weekly'), resetPassed: true },
  ]);
  const v = section([pro, passed]);
  const at = v.columns.findIndex((column) => column.key === 'cli-gemini-argon-wtu-weekly');
  assert.notEqual(at, -1);
  assert.equal(v.rows[1].cells[at].naText, 'Unavailable');
  assert.equal(v.rows[1].cells[at].hasValue, false);
});
