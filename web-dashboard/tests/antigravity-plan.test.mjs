import test from 'node:test';
import assert from 'node:assert/strict';
import { dashboardViewModel, detailsViewModel } from '../public/view-model.mjs';
import {
  antigravityColumns,
  antigravityMissingWindow,
  antigravityView,
} from '../public/antigravity-data.mjs';

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

test('four pool columns keep the provider keys and show missing plan windows honestly', () => {
  const weekly = account('free', { quotaPolicy: 'weekly', thirdPartyModels: false }, [
    window('gemini-weekly', 0),
  ]);
  const old = account('old', null, [window('gemini-5h')]);
  const v = section([weekly, old]);
  assert.deepEqual(
    v.columns.map((column) => column.key),
    ['gemini-5h', 'gemini-weekly', '3p-5h', '3p-weekly']
  );
  assert.deepEqual(
    v.columns.map((column) => column.label),
    ['Gemini 5-hour', 'Gemini weekly', 'Claude and GPT 5-hour', 'Claude and GPT weekly']
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
  assert.equal(v.rows[0].cells[2].hasValue, true);
  assert.equal(v.rows[0].cells[2].valueText, '25');
  assert.equal(v.columns.at(-1).label, 'New provider quota');
  assert.equal(v.rows[0].cells.at(-1).naText, 'Unavailable');
  assert.deepEqual(
    antigravityColumns([
      account('old', null, [{ ...window('unknown'), label: 'Old provider quota' }]),
    ]),
    [{ key: 'unknown', label: 'Old provider quota' }]
  );
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
    section([{ ...empty, windows: [window('gemini-weekly')] }]).rows[0].cells[1].hasValue,
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

test('plan metadata leaves the two provider membership pools intact', () => {
  const rows = ['a', 'b'].map((id) =>
    account(
      id,
      { class: 'pro', quotaPolicy: 'five-hour-weekly' },
      ['gemini', '3p'].flatMap((pool) =>
        ['5h', 'weekly'].map((period) => ({
          ...window(`${pool}-${period}`),
          poolId: `${pool}:reported`,
          poolIdSource: 'provider-bucket-membership',
          poolLabel: pool === 'gemini' ? 'Gemini models' : 'Claude and GPT models',
        }))
      )
    )
  );
  const inventory = {
    schemaVersion: 1,
    hostId: 'ubuntu',
    activationSupported: true,
    profiles: rows.map((row, index) => ({
      id: index ? 'b' : 'a',
      email: row.email,
      hostId: 'ubuntu',
      available: true,
      selected: index === 0,
      runtimeVerified: true,
      verifiedAt: row.sampledAt,
    })),
  };
  const status = {
    enabled: false,
    thresholdUsedPercent: 95,
    pollIntervalSeconds: 60,
    maxQuotaAgeSeconds: 300,
    cooldownSeconds: 300,
    selectedHostIds: ['ubuntu'],
    requestedPoolId: null,
    outcome: 'setup_required',
    activationInProgress: false,
  };
  assert.deepEqual(antigravityView(data(rows), inventory, status, now).antigravitySharedPoolIds, [
    '3p:reported',
    'gemini:reported',
  ]);
  assert.deepEqual(
    rows.map((row) => new Set(row.windows.map((window) => window.poolId)).size),
    [2, 2]
  );
});
