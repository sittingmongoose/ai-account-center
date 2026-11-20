import test from 'node:test';
import assert from 'node:assert/strict';
import { dashboardViewModel, detailsViewModel } from '../public/view-model.mjs';
import { antigravityColumns, antigravityMissingWindow } from '../public/antigravity-data.mjs';

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
    antigravityCanActivate: true,
  },
});
const data = (accounts) => ({ schemaVersion: 1, updatedAt: new Date(now).toISOString(), accounts });
const section = (accounts) =>
  dashboardViewModel(data(accounts), { now }).sections.find(
    (section) => section.id === 'antigravity'
  );

test('eight pool columns (T3 first, then the app) keep the provider keys and show missing plan windows honestly', () => {
  const weekly = account('free', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('gemini-weekly', 0),
  ]);
  const old = account('old', null, [window('gemini-5h')]);
  const v = section([weekly, old]);
  assert.deepEqual(
    v.columns.map((column) => column.key),
    [
      'cli-gemini-5h',
      'cli-gemini-weekly',
      'cli-3p-5h',
      'cli-3p-weekly',
      'gemini-5h',
      'gemini-weekly',
      '3p-5h',
      '3p-weekly',
    ]
  );
  assert.deepEqual(
    v.columns.map((column) => column.label),
    [
      'Gemini 5-hour, in T3 (Antigravity CLI)',
      'Gemini weekly, in T3 (Antigravity CLI)',
      'Claude and GPT 5-hour, in T3 (Antigravity CLI)',
      'Claude and GPT weekly, in T3 (Antigravity CLI)',
      'Gemini 5-hour, in the Antigravity app',
      'Gemini weekly, in the Antigravity app',
      'Claude and GPT 5-hour, in the Antigravity app',
      'Claude and GPT weekly, in the Antigravity app',
    ]
  );
  // A weekly-only plan: the same wording on both services' 5-hour and Claude columns.
  for (const i of [0, 4]) assert.equal(v.rows[0].cells[i].naText, 'Weekly only');
  assert.equal(v.rows[0].cells[5].valueText, '0');
  assert.equal(v.rows[0].cells[5].hasValue, true);
  for (const i of [2, 3, 6, 7]) {
    assert.equal(v.rows[0].cells[i].naText, 'Not on plan');
    assert.equal(v.rows[0].cells[i].hasValue, false);
    assert.equal(v.rows[0].cells[i].valueText, '');
  }
  // This reading has no T3 windows: T3's Gemini weekly is unavailable, never zero.
  assert.equal(v.rows[0].cells[1].naText, 'Unavailable');
  assert.equal(v.rows[0].cells[1].hasValue, false);
  assert.equal(v.rows[1].cells[5].naText, 'Unavailable');
  assert.equal(
    antigravityMissingWindow(
      account('plus', { quotaPolicy: 'weekly', thirdPartyModels: true }, []),
      '3p-5h'
    ),
    'Weekly only'
  );
});

test('reported windows win over plan metadata and unknown extra windows keep generic captions', () => {
  const row = account('a', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('3p-5h'),
    {
      ...window('new-pool'),
      label: 'New provider quota',
      usedPercent: null,
      remainingPercent: null,
    },
  ]);
  const v = section([row]);
  assert.equal(v.rows[0].cells[6].hasValue, true);
  assert.equal(v.rows[0].cells[6].valueText, '25');
  assert.equal(v.columns.at(-1).label, 'New provider quota');
  assert.equal(v.rows[0].cells.at(-1).naText, 'Unavailable');
  assert.deepEqual(
    antigravityColumns([
      account('old', null, [{ ...window('unknown'), label: 'Old provider quota' }]),
    ]),
    [{ key: 'unknown', label: 'Old provider quota' }]
  );
});

test('an extra pool another account reports reads Not reported, never Unavailable or zero', () => {
  // Shapes from the 2026-11-13 quota reads: the Pro account lists a Gemini 4 Argon weekly
  // bucket; the free-tier account's reading has only the two weekly buckets.
  const argon = {
    ...window('gemini-argon-wtu-weekly', 1),
    label: 'Gemini 4 Argon · Weekly',
  };
  const pro = account('pro', { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true }, [
    window('gemini-5h', 1),
    window('gemini-weekly', 1),
    window('3p-5h', 0),
    window('3p-weekly', 0),
    argon,
  ]);
  const free = account('free', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('gemini-weekly', 0),
    window('3p-weekly', 0),
  ]);
  const failed = { ...account('failed', null, []), status: 'unavailable' };
  // A cached reading is the last good one after a failed refresh (possibly from before Argon
  // existed), and an ok row can still carry cached windows: neither may claim "Not reported".
  const cached = {
    ...account('cached', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
      window('gemini-weekly', 0),
      window('3p-weekly', 0),
    ]),
    status: 'cached',
    sampledAt: new Date(now - 3 * 86400000).toISOString(),
  };
  const cachedWindows = account(
    'cached-windows',
    { quotaPolicy: 'weekly', thirdPartyModels: false },
    [{ ...window('gemini-weekly', 0), status: 'cached', cached: true }]
  );
  const v = section([pro, free, failed, cached, cachedWindows]);
  const at = v.columns.findIndex((column) => column.key === 'gemini-argon-wtu-weekly');
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

test('both quota services show side by side, T3 first, each with its own numbers', () => {
  const pro = account('gmail', { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true }, [
    ...tonightT3,
    ...tonightApp,
  ]);
  const v = section([pro]);
  assert.deepEqual(
    v.columns.map((column) => column.key),
    [
      'cli-gemini-5h',
      'cli-gemini-weekly',
      'cli-3p-5h',
      'cli-3p-weekly',
      'gemini-5h',
      'gemini-weekly',
      '3p-5h',
      '3p-weekly',
      'cli-gemini-argon-wtu-weekly',
      'gemini-argon-wtu-weekly',
    ]
  );
  const cell = (key) => v.rows[0].cells[v.columns.findIndex((column) => column.key === key)];
  assert.equal(cell('cli-gemini-5h').valueText, '29.68');
  assert.equal(cell('cli-gemini-weekly').valueText, '8.48');
  assert.equal(cell('gemini-weekly').valueText, '0.09');
  assert.equal(cell('gemini-5h').valueText, '0');
  assert.equal(cell('gemini-argon-wtu-weekly').valueText, '37.83');
  assert.equal(cell('cli-gemini-argon-wtu-weekly').valueText, '0');
  assert.equal(v.columns.at(-2).label, 'Gemini 4 Argon · Weekly · In T3 (Antigravity CLI)');
  assert.equal(v.columns.at(-1).label, 'Gemini 4 Argon · Weekly · In the Antigravity app');
  assert.equal(v.columns[0].shortLabel, 'T3 Gemini 5hr');
  assert.equal(v.columns[4].shortLabel, 'App Gemini 5hr');
});

test('one service failing shows that service unavailable, never zero, while the other still shows', () => {
  const plan = { quotaPolicy: 'five-hour-weekly', thirdPartyModels: true };
  const appOnly = account('app-only', plan, tonightApp); // T3's service did not answer
  const t3Only = account('t3-only', plan, tonightT3); // the app's service did not answer
  const v = section([appOnly, t3Only]);
  const at = (key) => v.columns.findIndex((column) => column.key === key);
  for (const key of [
    'cli-gemini-5h',
    'cli-gemini-weekly',
    'cli-3p-5h',
    'cli-3p-weekly',
    'cli-gemini-argon-wtu-weekly',
  ]) {
    const missing = v.rows[0].cells[at(key)];
    assert.equal(missing.naText, 'Unavailable', key);
    assert.equal(missing.hasValue, false, key);
    assert.equal(missing.valueText, '', key);
  }
  for (const key of [
    'gemini-5h',
    'gemini-weekly',
    '3p-5h',
    '3p-weekly',
    'gemini-argon-wtu-weekly',
  ]) {
    const missing = v.rows[1].cells[at(key)];
    assert.equal(missing.naText, 'Unavailable', key);
    assert.equal(missing.hasValue, false, key);
  }
  assert.equal(v.rows[0].cells[at('gemini-weekly')].valueText, '0.09');
  assert.equal(v.rows[1].cells[at('cli-gemini-5h')].valueText, '29.68');
  assert.equal(antigravityMissingWindow(appOnly, 'cli-gemini-argon-wtu-weekly'), 'Unavailable');
  assert.equal(antigravityMissingWindow(appOnly, 'gemini-new-pool'), 'Not reported');
});

test('a free account reads 0 on both services, weekly only, Claude and GPT still on the plan', () => {
  const zero = { 'gemini-weekly': 0, '3p-weekly': 0 };
  const free = account('party', { quotaPolicy: 'weekly', thirdPartyModels: true }, [
    ...service('cli-', T3_WHERE, zero),
    ...service('', APP_WHERE, zero),
  ]);
  const v = section([free]);
  const cells = Object.fromEntries(v.columns.map((column, i) => [column.key, v.rows[0].cells[i]]));
  for (const key of ['cli-gemini-weekly', 'cli-3p-weekly', 'gemini-weekly', '3p-weekly']) {
    assert.equal(cells[key].hasValue, true, key);
    assert.equal(cells[key].valueText, '0', key);
  }
  for (const key of ['cli-gemini-5h', 'cli-3p-5h', 'gemini-5h', '3p-5h'])
    assert.equal(cells[key].naText, 'Weekly only', key);
});

test('no-quota plans with no windows use the existing row text and retain reported windows when present', () => {
  const empty = account(
    'workspace',
    { quotaPolicy: 'none', models: [], creditsOverage: false },
    []
  );
  const v = section([empty, account('pro', null, [window('gemini-5h')])]);
  assert.equal(v.rows[0].cells.length, v.columns.length);
  assert.ok(
    v.rows[0].cells.every((cell) => cell.key === '' && !cell.hasValue && cell.naText === '')
  );
  assert.equal(v.rows[0].amountsLine, 'No Antigravity quota on this plan');
  assert.equal(v.rows[0].amountsRuns[0].text, 'No Antigravity quota on this plan');
  assert.equal(
    section([{ ...empty, windows: [window('gemini-weekly')] }]).rows[0].cells[5].hasValue,
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
