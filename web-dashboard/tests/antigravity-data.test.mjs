import { test } from 'node:test';
import assert from 'node:assert/strict';
import { antigravityView } from '../public/antigravity-data.mjs';
import { allDetailWindows } from '../public/accounts-data.mjs';
const NOW = Date.parse('2026-10-01T17:00:00Z');
function fixture() {
  const inventory = { schemaVersion: 1, hostId: 'ubuntu', profiles: ['gmail', 'party'].map((id, index) => ({ id, email: `${id}@example.com`, plan: 'Google AI Pro', hostId: 'ubuntu', available: true, selected: index === 0, verifiedAt: new Date(NOW - 1000).toISOString() })) };
  const accounts = inventory.profiles.map((p, index) => ({ id: `antigravity:profile:${p.id}`, provider: 'antigravity', providerLabel: 'Antigravity', label: p.email, email: p.email, plan: p.plan, platform: 'ubuntu', source: 'Antigravity saved login on Ubuntu', status: 'ok', message: null, fetchedAt: new Date(NOW).toISOString(), sampledAt: new Date(NOW - 1000).toISOString(), isActive: p.selected,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [], antigravityProfileId: p.id, antigravityHostIds: ['ubuntu'] },
    windows: ['gemini', 'third-party'].flatMap(pool => [300, 10080].map(minutes => ({ key: `${pool}-${minutes}`, label: `${pool} ${minutes === 300 ? '5-hour' : 'weekly'}`, windowMinutes: minutes, kind: 'rate_limit', usedPercent: index ? 0 : 95.12345, remainingPercent: index ? 100 : 4.87655, resetAt: new Date(NOW + minutes * 60000).toISOString(), used: null, limit: null, unit: null,
      poolId: `${pool}@reported-bucket`, poolIdSource: 'provider-bucket-membership', poolLabel: pool === 'gemini' ? 'Gemini models' : 'Claude / GPT models', modelIds: pool === 'gemini' ? ['gemini-2.5-pro'] : ['claude-sonnet', 'gpt-model'] }))) }));
  return { data: { schemaVersion: 1, updatedAt: new Date(NOW).toISOString(), accounts }, inventory };
}
const view = f => antigravityView(f.data, f.inventory);
test('no inventory binds no accounts and invents nothing', () => {
  const v = antigravityView({ accounts: [] }, null);
  assert.equal(v.antigravityAccounts.length, 0);
  assert.equal('antigravityUpdatePaused' in v, false);
  assert.equal('antigravityServiceProblem' in v, false);
});
test('two bound Ubuntu rows keep the live login, omit retired runtime fields and keep all four Details windows', () => {
  const f = fixture();
  const v = view(f);
  assert.equal(v.antigravityAccounts.length, 2);
  assert.equal(v.antigravityAccounts[0].selected, true);
  assert.equal('runtimeVerified' in v.antigravityAccounts[0], false);
  assert.equal(v.antigravityAccounts[0].status, 'Selected');
  assert.doesNotMatch(v.antigravityAccounts[0].status, /unverified|running verified/i);
  assert.equal(v.antigravityAccounts[1].selected, false);
  for (const row of v.antigravityAccounts) assert.equal('canActivate' in row, false);
  assert.equal(allDetailWindows(f.data, 'antigravity').length, 8);
  assert.equal(allDetailWindows(f.data, f.data.accounts[0].id).length, 4);
  assert.ok(allDetailWindows(f.data, 'antigravity').every(w => w.reset));
});
test('leftover retired runtime markers are ignored', () => {
  const f = fixture();
  f.inventory.nativeUpdatePaused = { installedVersion: '1.2.17' };
  f.inventory.runtimeServiceProblem = { reason: 'missing-python-module', module: 'pyte', python: '3.14', builtFor: '3.13', exitStatus: null };
  f.inventory.profiles[0].runtimeVerified = false;
  const v = view(f);
  assert.equal('antigravityUpdatePaused' in v, false);
  assert.equal('antigravityServiceProblem' in v, false);
  assert.equal('runtimeVerified' in v.antigravityAccounts[0], false);
});
test('Home and Details omit retired managed-runtime unverified text', async () => {
  const { dashboardViewModel, detailsViewModel } = await import('../public/view-model.mjs');
  const f = fixture();
  f.inventory.profiles[0].runtimeVerified = false;
  const ctx = { now: NOW, antigravityInventory: f.inventory };
  const rows = dashboardViewModel(f.data, ctx).sections.find((s) => s.id === 'antigravity').rows;
  assert.doesNotMatch(rows[0].meta, /unverified/i);
  assert.doesNotMatch(rows[1].meta, /unverified/i);
  const details = detailsViewModel(f.data, f.data.accounts[0].id, ctx);
  assert.equal(details.facts.some((x) => x.label === 'Runtime'), false);
  assert.equal(details.facts.find((x) => x.label === 'Status').value, 'Live');
});
