import { test } from 'node:test';
import assert from 'node:assert/strict';
import { antigravityView, antigravitySettingsPatch, validAntigravityAuto } from '../public/antigravity-data.mjs';
import { allDetailWindows } from '../public/accounts-data.mjs';
const NOW = Date.parse('2026-10-01T17:00:00Z');
function fixture() {
  const inventory = { schemaVersion: 1, hostId: 'ubuntu', activationSupported: true, profiles: ['gmail', 'party'].map((id, index) => ({ id, email: `${id}@example.com`, plan: 'Google AI Pro', hostId: 'ubuntu', available: true, selected: index === 0, runtimeVerified: index === 0, verifiedAt: new Date(NOW - 1000).toISOString() })) };
  const accounts = inventory.profiles.map((p, index) => ({ id: `antigravity:profile:${p.id}`, provider: 'antigravity', providerLabel: 'Antigravity', label: p.email, email: p.email, plan: p.plan, platform: 'ubuntu', source: 'Antigravity saved login on Ubuntu', status: 'ok', message: null, fetchedAt: new Date(NOW).toISOString(), sampledAt: new Date(NOW - 1000).toISOString(), isActive: p.selected,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [], antigravityProfileId: p.id, antigravityHostIds: ['ubuntu'], antigravityCanActivate: true },
    windows: ['gemini', 'third-party'].flatMap(pool => [300, 10080].map(minutes => ({ key: `${pool}-${minutes}`, label: `${pool} ${minutes === 300 ? '5-hour' : 'weekly'}`, windowMinutes: minutes, kind: 'rate_limit', usedPercent: index ? 0 : 95.12345, remainingPercent: index ? 100 : 4.87655, resetAt: new Date(NOW + minutes * 60000).toISOString(), used: null, limit: null, unit: null,
      poolId: `${pool}@reported-bucket`, poolIdSource: 'provider-bucket-membership', poolLabel: pool === 'gemini' ? 'Gemini models' : 'Claude / GPT models', modelIds: pool === 'gemini' ? ['gemini-2.5-pro'] : ['claude-sonnet', 'gpt-model'] }))) }));
  const status = { enabled: false, thresholdUsedPercent: 95, pollIntervalSeconds: 60, maxQuotaAgeSeconds: 300, cooldownSeconds: 300, selectedHostIds: ['ubuntu'], requestedPoolId: null, outcome: 'setup_required', message: 'Choose a reported quota pool before enabling.', activationInProgress: false, lastCheckedAt: null, lastSwitchedAt: null, lastProfileId: null, lastHostId: null };
  return { data: { schemaVersion: 1, updatedAt: new Date(NOW).toISOString(), accounts }, inventory, status };
}
const view = f => antigravityView(f.data, f.inventory, f.status, NOW);
test('unconfigured controls stay unavailable without inventing a default setting or accounts', () => {
  const v = antigravityView({ accounts: [] }, null, null, NOW);
  assert.equal(v.antigravityAutoKnown, false); assert.equal(v.antigravityThresholdLabel, '—'); assert.equal(v.antigravityAutoAvailable, false); assert.equal(v.antigravityAccounts.length, 0);
});
test('two bound Ubuntu rows preserve real zero, percent precision, running distinction and all four Details windows', () => {
  const f = fixture(); f.inventory.profiles[0].runtimeVerified = false;
  const v = view(f);
  assert.equal(v.antigravityAccounts.length, 2); assert.equal(v.antigravityAccounts[0].selected, true); assert.equal(v.antigravityAccounts[0].runtimeVerified, false);
  assert.match(v.antigravityAccounts[0].status, /runtime unverified/); assert.equal(v.antigravityAccounts[0].canActivate, false);
  assert.equal(v.antigravityAccounts[1].canActivate, true); assert.equal(v.antigravityAccounts[1].five.amount, '0% used');
  assert.equal(v.antigravityAccounts[0].five.percent, 95.12345); assert.equal(v.antigravityAccounts[0].five.amount, '95.12% used');
  assert.equal(allDetailWindows(f.data, 'antigravity').length, 8); assert.equal(allDetailWindows(f.data, f.data.accounts[0].id).length, 4);
  assert.ok(allDetailWindows(f.data, 'antigravity').every(w => w.reset));
});
test('off95 USED is server-confirmed and enabling requires actual chosen common quota group', () => {
  const f = fixture(); let v = view(f);
  assert.equal(v.antigravityThresholdLabel, '95%'); assert.equal(v.antigravityAutoEnabled, false); assert.equal(v.antigravityAutoAvailable, false);
  assert.equal(v.antigravityPoolChoices.length, 3); assert.match(v.antigravityPoolChoices[1].label, /reported group/);
  const selected = v.antigravityPoolChoices[1]; assert.deepEqual(antigravitySettingsPatch(v, 'antigravity-pool', selected.label), { requestedPoolId: selected.id });
  f.status.requestedPoolId = selected.id; v = view(f);
  assert.deepEqual(antigravitySettingsPatch(v, 'antigravity-automatic', 'true'), { enabled: true });
  assert.deepEqual(antigravitySettingsPatch(v, 'antigravity-threshold', '85%'), { thresholdUsedPercent: 85 });
  assert.equal(antigravitySettingsPatch(v, 'antigravity-threshold', '100%'), null);
  assert.equal(antigravitySettingsPatch(v, 'antigravity-pool', 'Gemini guessed'), null);
});
test('global unsupported, missing capabilities and sign-in veto never offer activation or auto', () => {
  for (const mutate of [f => delete f.inventory.activationSupported, f => f.inventory.activationSupported = false, f => delete f.data.accounts[1].capabilities.antigravityCanActivate, f => f.data.accounts[1].status = 'needs_sign_in', f => f.inventory.profiles[1].available = false, f => f.data.accounts[1].capabilities.antigravityHostIds = ['windows']]) {
    const f = fixture(); mutate(f); const v = view(f);
    assert.equal(v.antigravityAccounts[1].canActivate, false); assert.equal(v.antigravityAutoAvailable, false); assert.equal(v.antigravityPoolAvailable, false);
  }
});
test('identity/account ambiguity and malformed inventory cannot enable automatic controls', () => {
  for (const mutate of [f => f.data.accounts[1].email = 'different@example.com', f => f.inventory.profiles[1].hostId = 'windows', f => f.inventory.profiles[1].selected = true, f => f.inventory.profiles[0].selected = false, f => f.inventory.profiles.push({...f.inventory.profiles[1]}), f => f.data.accounts.push({...f.data.accounts[1]}), f => f.inventory.profiles[1].available = 'true']) {
    const f = fixture(); mutate(f); const v = view(f); assert.equal(v.antigravityAutoAvailable, false); assert.equal(v.antigravityPoolAvailable, false);
  }
});
test('cached or stale display quotas retain amounts/reset/source but cannot authorize auto setup', () => {
  for (const mutate of [f => f.data.accounts[1].status = 'cached', f => f.data.accounts[1].sampledAt = new Date(NOW - 301000).toISOString(), f => f.data.accounts[1].sampledAt = new Date(NOW + 1000).toISOString(), f => f.data.accounts[1].windows.forEach(w => { w.status = 'cached'; w.sampledAt = new Date(NOW - 3600000).toISOString(); })]) {
    const f = fixture(); mutate(f); const v = view(f); assert.equal(v.antigravityPoolAvailable, false); assert.equal(v.antigravityAutoAvailable, false); assert.equal(v.antigravityAccounts[1].canActivate, true); assert.equal(v.antigravityAccounts[1].five.amount, '0% used'); assert.ok(v.antigravityAccounts[1].five.reset);
  }
});
test('same display label is not matching-pool proof; absent/changed constraints and reset dates fail closed', () => {
  for (const mutate of [f => f.data.accounts[1].windows.forEach(w => { w.poolId += '-other'; }), f => f.data.accounts[1].windows.forEach(w => { w.key += '-other'; }), f => f.data.accounts[1].windows.forEach(w => { w.resetAt = null; }), f => f.data.accounts[1].windows.forEach(w => { w.resetAt = new Date(NOW).toISOString(); }), f => f.data.accounts[1].windows.forEach(w => { w.poolIdSource = 'guessed'; }), f => f.data.accounts[1].windows = f.data.accounts[1].windows.filter(w => w.windowMinutes !== 10080)]) {
    const f = fixture(); mutate(f); const v = view(f); assert.equal(v.antigravityPoolAvailable, false); assert.equal(v.antigravityAutoAvailable, false);
  }
});
test('already-enabled auto can be safely turned off even when its configured pool becomes stale', () => {
  const f = fixture(); f.status.enabled = true; f.status.requestedPoolId = 'missing@pool'; f.data.accounts[1].status = 'cached'; const v = view(f);
  assert.equal(v.antigravityAutoEnabled, true); assert.equal(v.antigravityPoolLabel, 'Configured pool · unavailable');
  assert.deepEqual(antigravitySettingsPatch(v, 'antigravity-automatic', 'false'), {enabled:false}); assert.equal(antigravitySettingsPatch(v, 'antigravity-automatic', 'true'), null); assert.equal(antigravitySettingsPatch(v, 'antigravity-pool', v.antigravityPoolLabel), null);
});
test('activation busy lock disables all writes; server threshold remains USED with no Codex conversion', () => {
  const f = fixture(); f.status.thresholdUsedPercent = 81; f.status.activationInProgress = true; const v = view(f);
  assert.equal(v.antigravityThresholdLabel, '81%'); assert.equal(v.antigravityAutoAvailable, false); assert.equal(v.antigravitySettingsAvailable, false); assert.equal(v.antigravityAccounts[1].canActivate, false);
});
test('strict public auto values reject truthy strings, foreign hosts and unknown states', () => {
  const {status} = fixture(); assert.equal(validAntigravityAuto(status), true);
  for (const patch of [{enabled:'false'}, {activationInProgress:'false'}, {selectedHostIds:['mac']}, {outcome:'success'}, {requestedPoolId:'bad pool'}, {thresholdUsedPercent:95.5}, {maxQuotaAgeSeconds:901}, {cooldownSeconds:59}]) assert.equal(validAntigravityAuto({...status,...patch}), false);
});

test('actual pool labels that imitate generated suffixes still map uniquely to exact pool IDs', () => {
  const f = fixture();
  for (const row of f.data.accounts) row.windows = ['Quota · 1', 'Quota', 'Quota'].flatMap((label, index) => [300,10080].map(minutes => ({
    ...row.windows.find(w => w.windowMinutes === minutes), key: `pool-${index}-${minutes}`, poolId: `pool-${index}`, poolIdSource: 'provider-id', poolLabel: label,
  })));
  const v = view(f);
  assert.equal(v.antigravitySharedPoolIds.length, 3);
  assert.equal(new Set(v.antigravityPoolChoices.map(c => c.label)).size, v.antigravityPoolChoices.length);
  for (const choice of v.antigravityPoolChoices) assert.deepEqual(antigravitySettingsPatch(v, 'antigravity-pool', choice.label), {requestedPoolId:choice.id});
});
test('paused update names the installed version; hostile or missing versions stay generic', () => {
  const f = fixture(); assert.equal(view(f).antigravityUpdatePaused, null);
  f.inventory.nativeUpdatePaused = { installedVersion: '1.2.17' };
  assert.equal(view(f).antigravityUpdatePaused, 'Antigravity updated to 1.2.17; switching paused until reviewed');
  for (const installedVersion of [null, '1.2', '1.2.17; id', 'x'.repeat(129), 42]) {
    const g = fixture(); g.inventory.nativeUpdatePaused = { installedVersion };
    assert.equal(view(g).antigravityUpdatePaused, 'Antigravity updated; switching paused until reviewed');
  }
});
