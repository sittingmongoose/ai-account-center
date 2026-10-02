import test from 'node:test';
import assert from 'node:assert/strict';
import {
  ACCOUNTS_VIEW_VERSION, LIVE, accountsViewModel, updateResultsView, transportOf, transportNote, refreshFromPosition, refreshPosition,
} from '../public/accounts-view.mjs';

const now = Date.parse('2026-10-01T15:16:00Z');
const at = minutes => new Date(now + minutes * 60_000).toISOString();
const window = (overrides = {}) => ({ key: 'seven_day', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 10, remainingPercent: 90, resetAt: at(60 * 24), windowMinutes: 10080, used: null, limit: null, unit: null, ...overrides });
const account = (overrides = {}) => ({ id: 'codex:one', provider: 'codex', providerLabel: 'Codex', label: 'one', email: 'one@example.test', plan: 'pro', platform: 'ubuntu', source: 'Codex saved login on Ubuntu', status: 'ok', message: null, fetchedAt: at(-1), sampledAt: at(-1), isActive: false, windows: [window()], capabilities: { codexProfile: 'one', claudeProfileId: null, claudePlatforms: [] }, ...overrides });
const claude = (id, extra = {}) => account({ id: `claude:${id}`, provider: 'claude', providerLabel: 'Claude', label: `Claude ${id}`, email: `${id}@example.test`, plan: 'max', platform: 'mac', capabilities: { codexProfile: null, claudeProfileId: id, claudePlatforms: ['mac', 'windows'] }, ...extra });
const data = (accounts, extra = {}) => ({ schemaVersion: 1, updatedAt: at(0), settings: { refreshIntervalSeconds: 60 }, accounts, codexAutoSwitch: { enabled: true, thresholdPercent: 5, pollIntervalSeconds: 60, outcome: 'healthy', message: 'Healthy', activationInProgress: false }, ...extra });
const all = vm => [...vm.colA, ...vm.colB];
const provider = (vm, id) => all(vm).find(p => p.id === id);
const row = (vm, id) => all(vm).flatMap(p => p.rows).find(r => r.id === id);

test('the page is versioned and lists every provider from the registry, sign-in flows first', () => {
  const vm = accountsViewModel(data([account()]), { now });
  assert.equal(vm.version, ACCOUNTS_VIEW_VERSION);
  assert.equal(vm.version, 1);
  assert.deepEqual(vm.colA.map(p => p.id), ['claude', 'codex', 'antigravity']);
  assert.deepEqual(vm.colB.map(p => p.id), ['cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go']);
  // a provider with no account is still listed, with an empty line and its add action
  const kimi = provider(vm, 'kimi-code');
  assert.equal(kimi.count, 0);
  assert.equal(kimi.countText, '0 accounts');
  assert.equal(kimi.empty, 'No Kimi Code accounts yet.');
  assert.equal(kimi.foot[0].label, 'Add a Kimi Code key');
  assert.equal(provider(vm, 'codex').countText, '1 account');
});

test('rows show the status only for an exception, the last sample and the source', () => {
  const vm = accountsViewModel(data([
    account(),
    account({ id: 'codex:two', email: 'two@example.test', label: 'two', sampledAt: at(-45), capabilities: { codexProfile: 'two' } }),
    account({ id: 'codex:three', email: 'three@example.test', label: 'three', status: 'needs_sign_in', capabilities: { codexProfile: 'three' } }),
    account({ id: 'codex:four', email: 'four@example.test', label: 'four', sampledAt: null, fetchedAt: null, capabilities: { codexProfile: 'four' } }),
  ]), { now });
  assert.equal(row(vm, 'codex:one').status, '');
  assert.equal(row(vm, 'codex:one').sampled, 'sampled 1m ago');
  assert.equal(row(vm, 'codex:two').status, 'Stale');
  assert.equal(row(vm, 'codex:three').status, 'Sign-in needed');
  assert.equal(row(vm, 'codex:four').sampled, 'no reading yet');
  assert.equal(row(vm, 'codex:one').meta, 'Pro · one');
  assert.equal(row(vm, 'codex:one').src, 'Codex CLI device login');
  assert.equal(row(vm, 'codex:one').srcSub, 'Ubuntu');
});

test('Codex rows: Activate is live, Sign in again and Remove are coming, the slots are fixed', () => {
  const vm = accountsViewModel(data([
    account({ isActive: true }),
    account({ id: 'codex:two', email: 'two@example.test', label: 'two', capabilities: { codexProfile: 'two' } }),
  ]), { now });
  const codex = provider(vm, 'codex');
  assert.equal(codex.slots, 3);
  assert.equal(codex.canSwitch, true);
  for (const r of codex.rows) assert.deepEqual(r.actions.map(a => a.kind), ['switch', 'button', 'icon']);
  const active = row(vm, 'codex:one'), other = row(vm, 'codex:two');
  assert.equal(active.active, true);
  assert.equal(active.activeLabel, 'on Ubuntu');
  assert.equal(other.actions[0].act, 'activate');
  assert.equal(other.actions[0].value, 'two');
  assert.equal(other.actions[0].enabled, true);
  assert.equal(active.actions[0].enabled, false);
  assert.deepEqual(other.actions.slice(1).map(a => [a.act, a.coming, a.enabled]), [['signin-again', true, false], ['remove', true, false]]);
  assert.match(other.actions[1].tip, /^Coming with the server update\./);
  assert.equal(codex.foot[0].act, 'add');
  assert.equal(codex.foot[0].coming, true);
});

test('activating an account past the switch point asks first, as on Home', () => {
  const vm = accountsViewModel(data([
    account({ isActive: true }),
    account({ id: 'codex:two', email: 'two@example.test', label: 'two', windows: [window({ usedPercent: 99, remainingPercent: 1 })], capabilities: { codexProfile: 'two' } }),
  ]), { now });
  const other = row(vm, 'codex:two');
  assert.equal(other.confirm, true);
  assert.equal(other.activateKind, 'activate');
  assert.equal(other.confirmRuns[0].text, '99% used');
});

test('Claude rows open on Mac or Windows when the profile has a launcher; Remove is coming', () => {
  const profiles = [{ id: 'a', mac: { canOpen: true }, windows: { canOpen: false } }];
  const vm = accountsViewModel(data([claude('a')]), { now, profiles, platform: 'mac' });
  const r = row(vm, 'claude:a');
  assert.deepEqual(r.actions.map(a => [a.act, a.enabled]), [['launch', true], ['launch', false], ['remove', false]]);
  assert.equal(r.actions[0].value, 'a:mac');
  assert.equal(r.actions[0].platform, 'apple');
  assert.equal(r.srcSub, 'Mac and Windows');
  assert.equal(LIVE.openClaude, true);
});

test('Antigravity: one account shows no Activate slot, and the policy waits for a second account', () => {
  const ag = account({ id: 'antigravity:one', provider: 'antigravity', email: 'ag@example.test', label: 'Antigravity one', plan: 'Google AI Pro', capabilities: {} });
  const auto = { enabled: false, thresholdUsedPercent: 90, pollIntervalSeconds: 60, maxQuotaAgeSeconds: 300, cooldownSeconds: 900, selectedHostIds: ['ubuntu'], requestedPoolId: null, outcome: 'setup_required', message: 'Needs two accounts', activationInProgress: false };
  const vm = accountsViewModel(data([ag]), { now, antigravityAuto: auto });
  const r = row(vm, 'antigravity:one');
  assert.deepEqual(r.actions.map(a => a.kind), ['empty', 'icon']);
  assert.equal(provider(vm, 'antigravity').ag, true);
  assert.equal(vm.ag.live, false);
  assert.equal(vm.ag.known, true);
  assert.equal(vm.ag.toggleEnabled, false);
  assert.equal(vm.ag.threshold, 90);
  assert.equal(vm.ag.cooldown, '900');
  assert.deepEqual(vm.ag.cooldowns.map(c => c.label), ['5 min', '15 min', '30 min', '60 min']);
  assert.equal(vm.ag.noteStrong, 'Auto-switch is off until a second account is signed in.');
  const policy = vm.policies.find(p => p.provider === 'antigravity');
  assert.equal(policy.wait, true);
  assert.equal(policy.sub, 'Off until a second account is signed in');
  // an unusual cooldown is shown as its own choice instead of being rounded
  assert.equal(accountsViewModel(data([ag]), { now, antigravityAuto: { ...auto, cooldownSeconds: 120 } }).ag.cooldowns.at(-1).label, '2 min');
  // without a reported status the controls stay off and say why
  const unknown = accountsViewModel(data([ag]), { now });
  assert.equal(unknown.ag.known, false);
  assert.equal(unknown.ag.threshold, -1);
  assert.match(unknown.ag.noteStrong, /unavailable/);
});

test('API-key and session providers: their actions are coming; a console wallet row signs in instead', () => {
  const vm = accountsViewModel(data([
    account({ id: 'opencode-go:usage', provider: 'opencode-go', email: 'k@example.test', label: 'OpenCode', capabilities: {} }),
    account({ id: 'plan-opencode-go-console-1', provider: 'opencode-go', email: 'w@example.test', label: 'Wallet', message: 'Console workspace', capabilities: {} }),
    account({ id: 'qwen:usage', provider: 'qwen', email: 'q@example.test', label: 'Qwen', platform: 'windows', capabilities: {} }),
    account({ id: 'cursor:usage', provider: 'cursor', email: 'c@example.test', label: 'Cursor', capabilities: {} }),
  ]), { now });
  assert.deepEqual(row(vm, 'opencode-go:usage').actions.map(a => a.act), ['replace-key', 'remove']);
  assert.deepEqual(row(vm, 'plan-opencode-go-console-1').actions.map(a => a.act), ['signin', 'remove']);
  assert.equal(row(vm, 'plan-opencode-go-console-1').src, 'Console session in a browser');
  assert.ok(all(vm).flatMap(p => p.rows).flatMap(r => r.actions).filter(a => ['replace-key', 'remove', 'signin'].includes(a.act)).every(a => a.coming && !a.enabled));
  // Qwen is a browser session, never an API key (CONTRACT-registry-lifecycle 7)
  assert.equal(provider(vm, 'qwen').kindLabel, 'Console session by browser extension');
  assert.deepEqual(provider(vm, 'opencode-go').foot.map(a => a.label), ['Re-check', 'Add another OpenCode Go key']);
  assert.deepEqual(row(vm, 'cursor:usage').actions.map(a => [a.kind, a.label]), [['quiet', 'Session from the app']]);
  assert.deepEqual(provider(vm, 'cursor').foot.map(a => [a.act, a.coming]), [['open-app', true], ['recheck', true]]);
});

test('"Show on dashboard": hidden in this browser or by the server, and the page still lists the rows', () => {
  const kimi = account({ id: 'kimi-code:usage', provider: 'kimi-code', email: 'k@example.test', capabilities: {} });
  const local = accountsViewModel(data([kimi]), { now, localHidden: new Set(['kimi-code']) });
  assert.equal(provider(local, 'kimi-code').visible, false);
  assert.equal(provider(local, 'kimi-code').hiddenNote, 'Hidden in this browser');
  assert.equal(provider(local, 'kimi-code').rows.length, 1);
  assert.equal(provider(local, 'kimi-code').toggleEnabled, true);
  const server = accountsViewModel(data([kimi]), { now, serverHidden: new Set(['kimi-code']) });
  assert.equal(provider(server, 'kimi-code').hiddenNote, 'Hidden everywhere');
  assert.equal(provider(server, 'kimi-code').toggleEnabled, LIVE.visibilityServer);
  // a hidden Codex keeps its switch slots on this page
  const codex = accountsViewModel(data([account({ isActive: true }), account({ id: 'codex:two', email: 'two@example.test', capabilities: { codexProfile: 'two' } })]), { now, localHidden: new Set(['codex']) });
  assert.equal(row(codex, 'codex:two').actions[0].enabled, true);
});

test('policies: Codex in % used from the server, live controls', () => {
  const vm = accountsViewModel(data([account()]), { now });
  const codex = vm.policies.find(p => p.provider === 'codex');
  assert.equal(codex.threshold, 95);
  assert.equal(codex.enabled, true);
  assert.equal(codex.toggleEnabled, true);
  const unknown = accountsViewModel(data([account()], { codexAutoSwitch: null }), { now }).policies.find(p => p.provider === 'codex');
  assert.equal(unknown.known, false);
  assert.equal(unknown.threshold, -1);
});

test('Update apps results are grouped by computer, never invented', () => {
  assert.equal(updateResultsView(null, now).shown, false);
  assert.deepEqual(updateResultsView(null, now).headRuns.map(r => r.text).join(''), 'No run yet. Update apps in the header runs one.');
  const result = (platform, appLabel, status, extra = {}) => ({ appId: appLabel.toLowerCase().replace(/ /g, '-'), appLabel, platform, status, previousVersion: null, version: null, message: 'Done', ...extra });
  const done = updateResultsView({ state: 'failed', startedAt: at(-30), finishedAt: at(-20), activePlatform: null, results: [
    result('ubuntu', 'Codex CLI', 'updated', { previousVersion: '1.0.0', version: '1.1.0' }),
    result('mac', 'Claude Desktop', 'current'),
    result('mac', 'Muse Code', 'failed'),
  ] }, now);
  assert.equal(done.headRuns.map(r => r.text).join(''), 'Last run 20m ago · 3 results, 1 failed');
  assert.deepEqual(done.hosts.map(h => h.id), ['mac', 'windows', 'ubuntu']);
  assert.deepEqual(done.hosts[0].items.map(i => [i.app, i.result, i.tone]), [['Claude Desktop', 'Already current', ''], ['Muse Code', 'Failed', 'crit']]);
  assert.deepEqual(done.hosts[1].items.map(i => i.app), ['No results from this computer']);
  assert.equal(done.hosts[2].items[0].result, 'Updated to 1.1.0');
  assert.match(done.hosts[2].items[0].tip, /1\.0\.0 to 1\.1\.0/);
  const running = updateResultsView({ state: 'running', startedAt: at(-1), finishedAt: null, activePlatform: 'mac', results: [result('ubuntu', 'Codex CLI', 'current')] }, now);
  assert.equal(running.headRuns.map(r => r.text).join(''), 'Running now · 1 of 21 done');
  assert.deepEqual(running.hosts.map(h => h.items.map(i => i.result)), [['Running'], [''], ['Already current']]);
  assert.equal(running.hosts[0].items[0].running, true);
  assert.equal(running.hosts[1].items[0].app, 'Waiting for its turn');
});

test('Update apps cancel and readiness states read as plain text', () => {
  const result = (platform, appLabel, status, extra = {}) => ({ appId: appLabel.toLowerCase().replace(/ /g, '-'), appLabel, platform, status, previousVersion: null, version: null, message: 'Done', ...extra });
  const cancelling = updateResultsView({ state: 'running', startedAt: at(-1), finishedAt: null, activePlatform: 'mac', cancelRequested: true, expectedResults: 21, results: [result('ubuntu', 'Codex CLI', 'current')] }, now);
  assert.equal(cancelling.cancelling, true);
  assert.equal(cancelling.headRuns.map(r => r.text).join(''), 'Cancelling… · 1 of 21 done');
  const idle = updateResultsView({ state: 'running', startedAt: at(-1), finishedAt: null, activePlatform: 'mac', results: [result('ubuntu', 'Codex CLI', 'current')] }, now);
  assert.equal(idle.cancelling, false);
  const done = updateResultsView({ state: 'completed', startedAt: at(-30), finishedAt: at(-20), activePlatform: null, cancelRequested: true, results: [
    result('ubuntu', 'Codex CLI', 'current'),
    result('mac', 'Muse Code', 'skipped', { message: 'Skipped: cancelled' }),
    result('windows', 'OMP', 'unknown', { message: 'Unknown: this computer is not reachable.' }),
    result('windows', 'Claude Code', 'unknown', { message: 'Unknown: the readiness check could not run.' }),
  ] }, now);
  assert.equal(done.headRuns.map(r => r.text).join(''), 'Last run 20m ago · 4 results, 1 skipped, 2 unknown, cancelled');
  assert.deepEqual(done.hosts[0].items.map(i => i.result), ['Skipped: cancelled']);
  assert.deepEqual(done.hosts[1].items.map(i => i.result), ['Unknown: Windows not reachable', 'Unknown']);
  assert.equal(done.hosts[1].items[0].tip, 'Unknown: this computer is not reachable.');
});

test('connection facts and the sign-in block say only what the browser and server report', () => {
  assert.equal(transportOf('https:', 'aac.example.test'), 'https');
  assert.equal(transportOf('http:', 'localhost'), 'loopback');
  assert.equal(transportOf('http:', '[::1]'), 'loopback');
  assert.equal(transportOf('http:', '127.0.0.1'), 'loopback');
  assert.equal(transportOf('http:', '192.0.2.10'), 'http');
  assert.match(transportNote('http'), /^This address is plain HTTP/);
  const vm = accountsViewModel(data([account()]), { now, username: 'owner', host: '192.0.2.10:3000', origin: 'http://192.0.2.10:3000', transport: 'http', sessionHours: 24, signedInAt: now - 60 * 60_000 });
  assert.equal(vm.signin.username, 'owner');
  assert.equal(vm.signin.connection, 'Plain HTTP to 192.0.2.10:3000');
  assert.equal(vm.signin.session, 'ends in 23h 0m');
  assert.equal(vm.signin.sessionSub, 'sessions last 24 hours');
  assert.match(vm.signin.passwordTip, /ai-account-center dashboard auth setup/);
  assert.equal(vm.connection[0].value, 'http://192.0.2.10:3000');
  assert.equal(accountsViewModel(data([account()]), { now }).signin.session, 'lasts 24 hours');
  // nothing on the page is an example value: no example devices, no example addresses
  assert.doesNotMatch(JSON.stringify(vm), /example (devices|values|address)|192\.0\.2\.10:4317|jared-mac/);
});

test('the refresh slider is a log scale from 30 s to 60 min that snaps to its marks', () => {
  assert.equal(refreshFromPosition(0), 30);
  assert.equal(refreshFromPosition(1), 3600);
  assert.equal(refreshFromPosition(refreshPosition(300) + 0.02), 300);
  const between = refreshFromPosition((refreshPosition(300) + refreshPosition(600)) / 2);
  assert.ok(between > 300 && between < 600 && Number.isInteger(between));
  assert.equal(refreshPosition(5), 0);
});
