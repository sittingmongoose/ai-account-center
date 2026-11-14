import { test } from 'node:test';
import assert from 'node:assert/strict';
import { antigravityView, antigravityServiceProblem } from '../public/antigravity-data.mjs';
import { allDetailWindows } from '../public/accounts-data.mjs';
const NOW = Date.parse('2026-10-01T17:00:00Z');
function fixture() {
  const inventory = { schemaVersion: 1, hostId: 'ubuntu', profiles: ['gmail', 'party'].map((id, index) => ({ id, email: `${id}@example.com`, plan: 'Google AI Pro', hostId: 'ubuntu', available: true, selected: index === 0, runtimeVerified: index === 0, verifiedAt: new Date(NOW - 1000).toISOString() })) };
  const accounts = inventory.profiles.map((p, index) => ({ id: `antigravity:profile:${p.id}`, provider: 'antigravity', providerLabel: 'Antigravity', label: p.email, email: p.email, plan: p.plan, platform: 'ubuntu', source: 'Antigravity saved login on Ubuntu', status: 'ok', message: null, fetchedAt: new Date(NOW).toISOString(), sampledAt: new Date(NOW - 1000).toISOString(), isActive: p.selected,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [], antigravityProfileId: p.id, antigravityHostIds: ['ubuntu'] },
    windows: ['gemini', 'third-party'].flatMap(pool => [300, 10080].map(minutes => ({ key: `${pool}-${minutes}`, label: `${pool} ${minutes === 300 ? '5-hour' : 'weekly'}`, windowMinutes: minutes, kind: 'rate_limit', usedPercent: index ? 0 : 95.12345, remainingPercent: index ? 100 : 4.87655, resetAt: new Date(NOW + minutes * 60000).toISOString(), used: null, limit: null, unit: null,
      poolId: `${pool}@reported-bucket`, poolIdSource: 'provider-bucket-membership', poolLabel: pool === 'gemini' ? 'Gemini models' : 'Claude / GPT models', modelIds: pool === 'gemini' ? ['gemini-2.5-pro'] : ['claude-sonnet', 'gpt-model'] }))) }));
  return { data: { schemaVersion: 1, updatedAt: new Date(NOW).toISOString(), accounts }, inventory };
}
const view = f => antigravityView(f.data, f.inventory);
test('no inventory binds no accounts and invents nothing', () => {
  const v = antigravityView({ accounts: [] }, null);
  assert.equal(v.antigravityAccounts.length, 0); assert.equal(v.antigravityUpdatePaused, null); assert.equal(v.antigravityServiceProblem, null);
});
test('two bound Ubuntu rows keep the live login, the running distinction and all four Details windows', () => {
  const f = fixture(); f.inventory.profiles[0].runtimeVerified = false;
  const v = view(f);
  assert.equal(v.antigravityAccounts.length, 2); assert.equal(v.antigravityAccounts[0].selected, true); assert.equal(v.antigravityAccounts[0].runtimeVerified, false);
  assert.match(v.antigravityAccounts[0].status, /runtime unverified/); assert.equal(v.antigravityAccounts[1].selected, false);
  for (const row of v.antigravityAccounts) assert.equal('canActivate' in row, false);
  assert.equal(allDetailWindows(f.data, 'antigravity').length, 8); assert.equal(allDetailWindows(f.data, f.data.accounts[0].id).length, 4);
  assert.ok(allDetailWindows(f.data, 'antigravity').every(w => w.reset));
});
test('paused update names the installed version; hostile or missing versions stay generic', () => {
  const f = fixture(); assert.equal(view(f).antigravityUpdatePaused, null);
  f.inventory.nativeUpdatePaused = { installedVersion: '1.2.17' };
  assert.equal(view(f).antigravityUpdatePaused, 'Antigravity updated to 1.2.17; its runtime is paused until reviewed');
  for (const installedVersion of [null, '1.2', '1.2.17; id', 'x'.repeat(129), 42]) {
    const g = fixture(); g.inventory.nativeUpdatePaused = { installedVersion };
    assert.equal(view(g).antigravityUpdatePaused, 'Antigravity updated; its runtime is paused until reviewed');
  }
});
test('a runtime service that cannot load its parser is named plainly; hostile fields stay generic', () => {
  const f = fixture(); assert.equal(view(f).antigravityServiceProblem, null);
  f.inventory.runtimeServiceProblem = { reason: 'missing-python-module', module: 'pyte', python: '3.14', builtFor: '3.13', exitStatus: null };
  assert.equal(view(f).antigravityServiceProblem, 'Runtime service failed: missing Python module pyte (Python 3.14; runtime built for 3.13); managed updates wait until the runtime bundle is rebuilt');
  assert.equal(antigravityServiceProblem({ runtimeServiceProblem: { reason: 'missing-python-module', module: 'pyte', python: '3.14', builtFor: '3.14' } }),
    'Runtime service failed: missing Python module pyte; managed updates wait until the runtime bundle is rebuilt');
  assert.equal(antigravityServiceProblem({ runtimeServiceProblem: { reason: 'missing-python-module', module: 'pyte; id', python: '3.14\n', builtFor: 13 } }),
    'Runtime service failed: missing Python module unknown; managed updates wait until the runtime bundle is rebuilt');
  assert.equal(antigravityServiceProblem({ runtimeServiceProblem: { reason: 'service-failed', exitStatus: 1 } }),
    'Runtime service failed (exit status 1); managed updates wait until its cause is fixed');
  assert.equal(antigravityServiceProblem({ runtimeServiceProblem: { reason: 'service-failed', exitStatus: 'x' } }),
    'Runtime service failed; managed updates wait until its cause is fixed');
  for (const runtimeServiceProblem of [null, 'missing', [], { reason: 'not running' }]) assert.equal(antigravityServiceProblem({ runtimeServiceProblem }), null);
});
