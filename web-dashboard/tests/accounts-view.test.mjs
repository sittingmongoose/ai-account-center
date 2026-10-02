import test from 'node:test';
import assert from 'node:assert/strict';
import {
  ACCOUNTS_VIEW_VERSION, LIVE, accountsViewModel, updateResultsView, transportOf, transportNote, refreshFromPosition, refreshPosition,
  gate, flowView, lineView, networkView, devicesView, claudeDefaultProfile, TRUSTED_NOTE,
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

// The server's facts per provider (providers[] in the dashboard response), as the round-2 base reports them.
const KINDS = { claude: 'desktop-profile', codex: 'device-code', antigravity: 'supervised-cli', cursor: 'app-session', muse: 'device-code', 'kimi-code': 'api-key', qwen: 'browser-session', zai: 'api-key', 'opencode-go': 'api-key' };
const entry = (id, over = {}) => ({
  id, label: id, longLabel: id, iconKey: id, order: 0, visible: true, accountCount: 0, switchable: ['codex', 'antigravity'].includes(id),
  signIn: { kind: KINDS[id], label: '', platforms: ['ubuntu'], secureTransportRequired: true, available: true, unavailableReason: null, ...(over.signIn || {}) },
  extras: null,
  capabilities: { multiAccount: !['cursor', 'muse', 'qwen'].includes(id), add: true, signInAgain: KINDS[id] !== 'api-key', replaceKey: KINDS[id] === 'api-key', remove: true, recheck: !['codex', 'claude', 'antigravity'].includes(id), activate: false, autoSwitch: false, openApp: [], ...(over.capabilities || {}) },
  ...Object.fromEntries(Object.entries(over).filter(([k]) => !['signIn', 'capabilities'].includes(k))),
});
const providers = (over = {}) => Object.keys(KINDS).map(id => entry(id, over[id] || {}));
const reg = (id, provider, over = {}) => ({ id, provider, label: id, email: null, platform: 'ubuntu', credential: null, hidden: false, lifecycle: { state: 'ready', jobId: null },
  removeRefusal: null, ...over, actions: { signInAgain: true, replaceKey: false, remove: true, open: [], recheck: false, ...(over.actions || {}) } });
const registry = (accounts, trash = []) => ({ providers: [], accounts, jobs: [], trash });

test('the page is versioned and lists every provider from the registry, sign-in flows first', () => {
  const vm = accountsViewModel(data([account()], { providers: providers() }), { now });
  assert.equal(vm.version, ACCOUNTS_VIEW_VERSION);
  assert.equal(vm.version, 2);
  assert.deepEqual(vm.colA.map(p => p.id), ['claude', 'codex', 'antigravity']);
  assert.deepEqual(vm.colB.map(p => p.id), ['cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go']);
  // a provider with no account is still listed, with an empty line and its add action
  const kimi = provider(vm, 'kimi-code');
  assert.equal(kimi.count, 0);
  assert.equal(kimi.countText, '0 accounts');
  assert.equal(kimi.empty, 'No Kimi Code accounts yet.');
  assert.equal(kimi.foot[0].label, 'Add a Kimi Code key');
  assert.equal(kimi.foot[0].enabled, true);
  assert.equal(provider(vm, 'codex').countText, '1 account');
  assert.equal(LIVE.add && LIVE.remove && LIVE.signInAgain && LIVE.replaceKey && LIVE.visibilityServer && LIVE.passwordChange && LIVE.devices && LIVE.network, true);
  assert.equal(LIVE.purgeNow, false);
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

test('Codex rows: Activate, Sign in again and Remove are live from the server; the active row is refused inline', () => {
  const accounts = [
    account({ isActive: true }),
    account({ id: 'codex:two', email: 'two@example.test', label: 'two', capabilities: { codexProfile: 'two' } }),
  ];
  const r = registry([reg('codex:one', 'codex', { removeRefusal: 'account_active', actions: { signInAgain: false } }), reg('codex:two', 'codex')]);
  const vm = accountsViewModel(data(accounts, { providers: providers() }), { now, registry: r });
  const codex = provider(vm, 'codex');
  assert.equal(codex.slots, 3);
  assert.equal(codex.canSwitch, true);
  for (const row of codex.rows) assert.deepEqual(row.actions.map(a => a.kind), ['switch', 'button', 'icon']);
  const active = row(vm, 'codex:one'), other = row(vm, 'codex:two');
  assert.equal(active.active, true);
  assert.equal(other.actions[0].act, 'activate');
  assert.equal(other.actions[0].enabled, true);
  assert.equal(active.actions[0].enabled, false);
  assert.deepEqual(other.actions.slice(1).map(a => [a.act, a.enabled, a.coming, a.refused]), [['signin-again', true, false, false], ['remove', true, false, false]]);
  // the active account: both open their reason under the row instead of calling the server
  assert.deepEqual(active.actions.slice(1).map(a => [a.act, a.value, a.refused]), [['refuse', 'codex:one\naccount_active_signin', true], ['refuse', 'codex:one\naccount_active', true]]);
  assert.equal(codex.foot[0].act, 'add');
  assert.equal(codex.foot[0].enabled, true);
  assert.equal(codex.foot[0].coming, false);
  // no "coming" anywhere on a live Codex section
  assert.ok(codex.rows.flatMap(x => x.actions).every(a => !a.coming));
  // every live control is named for the harness
  assert.equal(other.actions[1].probe, 'signin-again:codex:two');
  assert.equal(other.actions[2].probe, 'remove:codex:two');
});

test('a provider whose sign-in needs a trusted connection says so; one with no flow on the server is "coming"', () => {
  const p = providers({ codex: { signIn: { available: false, unavailableReason: 'secure_transport_required' }, capabilities: { add: false, signInAgain: false } },
    antigravity: { signIn: { available: false, unavailableReason: 'not_implemented' }, capabilities: { add: false, signInAgain: false, remove: false } } });
  const ag = account({ id: 'antigravity:one', provider: 'antigravity', email: 'ag@example.test', capabilities: {} });
  const vm = accountsViewModel(data([account({ id: 'codex:two', capabilities: { codexProfile: 'two' } }), ag], { providers: p }), { now, registry: registry([reg('codex:two', 'codex'), reg('antigravity:one', 'antigravity', { actions: { remove: false } })]) });
  const add = provider(vm, 'codex').foot[0];
  assert.equal(add.enabled, false);
  assert.equal(add.coming, false);
  assert.match(add.tip, /trusted connection/);
  assert.match(row(vm, 'codex:two').actions[1].tip, /local network trust/);
  // Antigravity: Add is coming and the footer reads "Needs setup"
  const agp = provider(vm, 'antigravity');
  assert.equal(agp.foot[0].coming, true);
  assert.equal(agp.needs, true);
  assert.match(agp.how, /^Needs setup: /);
  assert.equal(row(vm, 'antigravity:one').actions.at(-1).coming, true);
  // the reason table behind it
  assert.deepEqual(gate(null, 'add', 'codex').coming, true);
  assert.equal(gate(entry('cursor', { accountCount: 1, capabilities: { add: false } }), 'add', 'cursor').reason, 'Cursor reads one account in this version.');
  assert.equal(gate(entry('codex', { signIn: { available: false, unavailableReason: 'tool_missing' }, capabilities: { add: false } }), 'add', 'codex').reason, 'The Codex command-line tool is not installed on the dashboard computer.');
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

test('Claude rows open on Mac or Windows; Remove is live, and a computer\'s default profile is refused with its reason', () => {
  const profiles = [{ id: 'a', mac: { canOpen: true }, windows: { canOpen: false } }, { id: 'home', mac: { canOpen: true, profilePath: '/Users/x/Library/Application Support/Claude' }, windows: { canOpen: true } }];
  const r = registry([reg('claude:a', 'claude', { actions: { remove: true } }), reg('claude:home', 'claude')]);
  const vm = accountsViewModel(data([claude('a'), claude('home')], { providers: providers() }), { now, profiles, platform: 'mac', registry: r });
  const a = row(vm, 'claude:a');
  assert.deepEqual(a.actions.map(x => [x.act, x.enabled]), [['launch', true], ['launch', false], ['remove', true]]);
  assert.equal(a.actions[0].value, 'a:mac');
  assert.equal(a.actions[0].platform, 'apple');
  assert.equal(a.srcSub, 'Mac and Windows');
  assert.equal(LIVE.openClaude, true);
  // the default profile (its folder is the app's own "Claude") is never offered for removal, whatever the server says
  const home = row(vm, 'claude:home');
  assert.deepEqual([home.actions[2].act, home.actions[2].value, home.actions[2].refused], ['refuse', 'claude:home\naccount_protected', true]);
  assert.match(home.actions[2].tip, /default Claude profile/);
  // and so is a profile the server refuses as protected or default
  const refused = accountsViewModel(data([claude('b')], { providers: providers() }), { now, registry: registry([reg('claude:b', 'claude', { removeRefusal: 'account_protected' })]) });
  assert.equal(row(refused, 'claude:b').actions[2].value, 'claude:b\naccount_protected');
  assert.equal(claudeDefaultProfile({ windows: { profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude' } }), true);
  assert.equal(claudeDefaultProfile({ windows: { profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude-party' } }), false);
  assert.equal(claudeDefaultProfile({ mac: { isDefault: true } }), true);
  // with Claude Remove off on the server it is "coming", never live
  const off = accountsViewModel(data([claude('a')], { providers: providers({ claude: { capabilities: { remove: false, add: false }, signIn: { available: false, unavailableReason: 'not_implemented' } } }) }), { now, profiles, registry: registry([reg('claude:a', 'claude', { actions: { remove: false } })]) });
  assert.deepEqual([row(off, 'claude:a').actions[2].coming, row(off, 'claude:a').actions[2].enabled], [true, false]);
  assert.equal(provider(off, 'claude').foot[0].coming, true);
});

test('the Claude trash lists each profile with Restore; Delete now waits for a server route', () => {
  const trash = [{ trashId: 'tr_1', provider: 'claude', label: 'party@example.test', trashedAt: at(-60), purgeAfter: '2026-10-31T15:16:00Z', state: 'trashed' },
    { trashId: 'tr_2', provider: 'claude', label: 'old@example.test', trashedAt: at(-60), purgeAfter: at(-1), state: 'deleting' }];
  const vm = accountsViewModel(data([], { providers: providers() }), { now, registry: registry([], trash) });
  const t = provider(vm, 'claude').trash;
  assert.deepEqual(t.map(x => x.id), ['tr_1', 'tr_2']);
  assert.equal(t[0].sub, 'moved to the trash 1h 0m ago · deleted for good Oct 31');
  assert.deepEqual(t[0].actions.map(a => [a.act, a.enabled, a.coming]), [['restore', true, false], ['purge', false, true]]);
  assert.equal(t[1].sub, 'Deleting for good');
  assert.equal(t[1].actions[0].enabled, false);
  // a restore under review shows its line
  const asking = accountsViewModel(data([], { providers: providers() }), { now, registry: registry([], trash), lines: { 'trash:tr_1': { kind: 'confirm', token: 't', effects: ['Its Claude data moves back on Mac and Windows.'] } } });
  const line = provider(asking, 'claude').trash[0].line;
  assert.deepEqual([line.shown, line.kind, line.lead], [true, 'confirm', 'Restore party@example.test?']);
  assert.deepEqual(line.actions.map(a => a.act), ['line-cancel', 'restore-commit']);
  assert.equal(provider(vm, 'codex').trash.length, 0);
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

test('API-key, browser and app providers: keys show their last 4, Replace key and Remove are live, sessions sign in and re-check', () => {
  const accounts = [
    account({ id: 'zai:acct:9f2c41d0', provider: 'zai', email: null, label: 'Work', capabilities: {} }),
    account({ id: 'opencode-go:usage', provider: 'opencode-go', email: 'k@example.test', label: 'OpenCode', capabilities: {} }),
    account({ id: 'plan-opencode-go-console-1', provider: 'opencode-go', email: 'w@example.test', label: 'Wallet', message: 'Console workspace', capabilities: {} }),
    account({ id: 'qwen:usage', provider: 'qwen', email: 'q@example.test', label: 'Qwen', platform: 'windows', capabilities: {} }),
    account({ id: 'cursor:usage', provider: 'cursor', email: 'c@example.test', label: 'Cursor', platform: 'mac', capabilities: {} }),
  ];
  const r = registry([
    reg('zai:acct:9f2c41d0', 'zai', { credential: { kind: 'aac-key', last4: 'x7Qa', fingerprint: 'sha256:0', storedOn: 'ubuntu' }, actions: { replaceKey: true, signInAgain: false } }),
    reg('opencode-go:usage', 'opencode-go', { credential: { kind: 'discover' }, actions: { replaceKey: false, signInAgain: false } }),
    reg('plan-opencode-go-console-1', 'opencode-go', { actions: { signInAgain: true, remove: false } }),
    reg('qwen:usage', 'qwen', { credential: { kind: 'discover' }, actions: { recheck: true } }),
    reg('cursor:usage', 'cursor', { credential: { kind: 'discover' }, actions: { recheck: true, open: ['mac'] } }),
  ]);
  const vm = accountsViewModel(data(accounts, { providers: providers({ muse: { signIn: { available: false, unavailableReason: 'not_implemented' }, capabilities: { signInAgain: false, add: false } } }) }), { now, registry: r });
  const zai = row(vm, 'zai:acct:9f2c41d0');
  assert.equal(zai.src, 'API key ending x7Qa');
  assert.equal(zai.srcSub, 'stored on Ubuntu');
  assert.deepEqual(zai.actions.map(a => [a.act, a.enabled]), [['replace-key', true], ['remove', true]]);
  // a key another app saved cannot be replaced from here, and says why
  const found = row(vm, 'opencode-go:usage');
  assert.equal(found.actions[0].enabled, false);
  assert.match(found.actions[0].tip, /another app saved/);
  // the console wallet signs in through the browser extension; its Remove waits for the server
  const wallet = row(vm, 'plan-opencode-go-console-1');
  assert.deepEqual(wallet.actions.map(a => [a.act, a.enabled, a.coming]), [['signin', true, false], ['remove', false, true]]);
  assert.equal(wallet.src, 'Console session in a browser');
  // Qwen is a browser session, never an API key (CONTRACT-registry-lifecycle 7)
  assert.equal(provider(vm, 'qwen').kindLabel, 'Console session by browser extension');
  assert.deepEqual(row(vm, 'qwen:usage').actions.map(a => [a.act, a.enabled]), [['signin', true], ['remove', true]]);
  assert.deepEqual(provider(vm, 'qwen').foot.map(a => [a.act, a.enabled]), [['session-signin', true], ['recheck', true]]);
  assert.deepEqual(row(vm, 'cursor:usage').actions.map(a => [a.kind, a.act || a.label]), [['quiet', 'Session from the app'], ['icon', 'remove']]);
  assert.deepEqual(provider(vm, 'cursor').foot.map(a => [a.act, a.value, a.enabled]), [['session-signin', 'cursor', true], ['recheck', 'cursor:usage', true]]);
  // Muse's sign-in is not on the server: "coming" with its reason; Re-check needs an account first
  const muse = provider(vm, 'muse');
  assert.deepEqual(muse.foot.map(a => [a.act, a.coming]), [['session-signin', true], ['recheck', false]]);
  assert.match(muse.foot[1].tip, /Sign in first/);
  assert.deepEqual(provider(vm, 'opencode-go').foot.map(a => a.label), ['Add another OpenCode Go key']);
});

test('"Show on dashboard" and "Show in tray" are saved on the server; the tray toggle waits for its route', () => {
  const kimi = account({ id: 'kimi-code:usage', provider: 'kimi-code', email: 'k@example.test', capabilities: {} });
  const hidden = accountsViewModel(data([kimi], { providers: providers(), settings: { refreshIntervalSeconds: 60, hiddenProviders: ['kimi-code'], hiddenAccountIds: [], visibilityAvailable: true } }), { now });
  const k = provider(hidden, 'kimi-code');
  assert.equal(k.visible, false);
  assert.equal(k.hiddenNote, 'Hidden on the dashboard');
  assert.equal(k.rows.length, 1);
  assert.equal(k.toggleEnabled, true);
  assert.match(k.toggleTip, /every browser/);
  // without settings.trayHiddenProviders the server has no tray route yet
  assert.deepEqual([k.trayVisible, k.trayEnabled, k.trayComing], [true, false, true]);
  const tray = accountsViewModel(data([kimi], { providers: providers(), settings: { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: ['kimi-code'] } }), { now });
  assert.deepEqual([provider(tray, 'kimi-code').trayVisible, provider(tray, 'kimi-code').trayEnabled, provider(tray, 'kimi-code').trayComing, provider(tray, 'kimi-code').visible], [false, true, false, true]);
  // an unreadable visibility file keeps the toggles from saving over it
  const broken = accountsViewModel(data([kimi], { settings: { hiddenProviders: [], visibilityAvailable: false } }), { now });
  assert.equal(provider(broken, 'kimi-code').toggleEnabled, false);
  // a hidden Codex keeps its switch slots on this page
  const codex = accountsViewModel(data([account({ isActive: true }), account({ id: 'codex:two', email: 'two@example.test', capabilities: { codexProfile: 'two' } })], { settings: { hiddenProviders: ['codex'] } }), { now });
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

test('connection facts and the sign-in block say only what the browser and server report', () => {
  assert.equal(transportOf('https:', 'aac.example.test'), 'https');
  assert.equal(transportOf('http:', 'localhost'), 'loopback');
  assert.equal(transportOf('http:', '[::1]'), 'loopback');
  assert.equal(transportOf('http:', '127.0.0.1'), 'loopback');
  assert.equal(transportOf('http:', '192.0.2.10'), 'http');
  // the sign-in page's note follows the trusted local network
  assert.equal(transportNote('http', { trustedLocalNetwork: true, connection: { peer: '192.168.50.20', trusted: true } }), TRUSTED_NOTE);
  assert.match(transportNote('http', { trustedLocalNetwork: false, connection: { trusted: false } }), /local network trust is off/);
  assert.match(transportNote('http', { trustedLocalNetwork: true, connection: { trusted: false } }), /not on your trusted local network/);
  assert.doesNotMatch(transportNote('http', { trustedLocalNetwork: false }), /HTTPS or an encrypted tunnel/);
  const signin = {
    session: { username: 'owner', sessionTimeoutHours: 24, expiresAt: new Date(now + 23 * 3_600_000).toISOString(), otherBrowsers: 2, passwordChangedAt: '2026-09-12T10:00:00Z', pairedDevices: 1, managedBy: 'config', secureTransport: true, secureOrigin: null },
    devices: [{ id: 'dev_1', name: 'Mac tray', platform: 'mac', appVersion: '2.1', pairedAt: at(-600), lastSeenAt: at(-2), lastSeenAddress: '192.0.2.20', rotatedAt: null, idleExpiresAt: at(60 * 24 * 80) }],
    network: { trustLocalNetwork: true, trustedNetworks: ['192.168.0.0/16'], connection: { peer: '192.168.50.20', trusted: true }, canTurnOn: false },
    pw: { open: true, busy: false, done: false, field: 'cur', error: "That isn't the current password. 4 tries left before a 15-minute pause.", nonce: 3 }, busy: '',
  };
  const vm = accountsViewModel(data([account()]), { now, username: 'owner', host: '192.0.2.10:3000', origin: 'http://192.0.2.10:3000', transport: 'http', sessionHours: 24, signin });
  assert.equal(vm.signin.username, 'owner');
  assert.equal(vm.signin.connection, 'Plain HTTP to 192.0.2.10:3000');
  assert.equal(vm.signin.session, 'ends in 23h 0m');
  assert.equal(vm.signin.othersText, '2 signed in');
  assert.equal(vm.signin.othersEnabled, true);
  assert.equal(vm.signin.passwordWhen, 'changed Sep 12');
  assert.deepEqual([vm.signin.passwordCan, vm.signin.passwordOpen, vm.signin.passwordField, vm.signin.passwordNonce], [true, true, 'cur', 3]);
  assert.equal(vm.signin.passwordOthers, '(2 signed in)');
  assert.deepEqual(vm.signin.devices.map(d => [d.id, d.name, d.platform]), [['dev_1', 'Mac tray', 'apple']]);
  assert.match(vm.signin.devices[0].sub, /^Mac · version 2\.1 · last seen 2m ago from 192\.0\.2\.20$/);
  assert.equal(vm.signin.network.line, 'This connection: 192.168.50.20, trusted local network');
  assert.equal(vm.signin.network.note, TRUSTED_NOTE);
  assert.deepEqual([vm.signin.network.act, vm.signin.network.actLabel], ['network-off', 'Turn off']);
  assert.equal(vm.signin.revokeAllEnabled, true);
  assert.equal(vm.connection[0].value, 'http://192.0.2.10:3000');
  assert.equal(vm.connection[1].value, 'Plain HTTP on your trusted local network');
  // without a trusted connection the password form stays closed and says why
  const untrusted = accountsViewModel(data([account()]), { now, transport: 'http', signin: { ...signin, session: { ...signin.session, secureTransport: false }, network: { ...signin.network, trustLocalNetwork: false, connection: { peer: '192.168.50.20', trusted: false } } } });
  assert.equal(untrusted.signin.passwordCan, false);
  assert.equal(untrusted.signin.passwordOpen, false);
  assert.match(untrusted.signin.passwordNote, /trusted connection/);
  assert.equal(untrusted.signin.network.line, 'This connection: 192.168.50.20, not trusted');
  assert.equal(untrusted.signin.network.act, '');
  // managed by environment variables: no form
  assert.match(accountsViewModel(data([account()]), { now, signin: { ...signin, session: { ...signin.session, managedBy: 'env' } } }).signin.passwordNote, /environment variables/);
  // nothing on the page is an example value: no example devices, no example addresses
  assert.doesNotMatch(JSON.stringify(vm), /example (devices|values|address)|192\.0\.2\.10:4317|jared-mac/);
  // no server facts yet: the block says so instead of inventing them
  const none = accountsViewModel(data([account()]), { now });
  assert.equal(none.signin.othersText, 'Not reported');
  assert.equal(none.signin.passwordCan, false);
  assert.equal(none.signin.session, 'lasts 24 hours');
});

test('the trusted local network line: this computer, trusted, not trusted, and Turn on only where it is allowed', () => {
  assert.equal(networkView({ trustLocalNetwork: false, connection: { peer: '127.0.0.1', trusted: false }, canTurnOn: true }, null, 'loopback').line, 'This connection: this computer');
  assert.deepEqual(['act', 'actLabel'].map(k => networkView({ trustLocalNetwork: false, connection: { peer: '127.0.0.1', trusted: false }, canTurnOn: true }, null, 'loopback')[k]), ['network-on', 'Turn on']);
  const lan = networkView({ trustLocalNetwork: false, connection: { peer: '192.168.50.20', trusted: false }, canTurnOn: false }, null, 'http');
  assert.equal(lan.act, '');
  assert.match(lan.note, /Turn it on there/);
  assert.equal(networkView(null, { trustedLocalNetwork: true, connection: { peer: '10.6.0.2', trusted: true } }, 'http').line, 'This connection: 10.6.0.2, trusted local network');
  assert.equal(networkView(null, null, 'http').known, false);
  assert.equal(networkView({ trustLocalNetwork: true, connection: { peer: '192.168.50.20', trusted: true } }, null, 'http', 'network').busy, true);
  assert.deepEqual(devicesView([{ id: 'dev_2', name: '', platform: 'windows', lastSeenAt: null, pairedAt: at(-5) }], now).map(d => [d.name, d.sub]), [['Windows tray', 'Windows · paired 5m ago, not seen since']]);
  assert.equal(devicesView([{ id: 'dev_3', platform: 'mac', lastSeenAt: null, pairedAt: null }], now)[0].sub, 'Mac · not seen since pairing');
});

test('flows: a Codex device-code sign-in from naming to signed in, with the code, the wait and the failures', () => {
  const name = flowView('codex', { type: 'job-add', step: 'name', name: 'codex-4', serial: 1 }, { now });
  assert.deepEqual([name.open, name.title, name.inputKind, name.inputSeed, name.cur], [true, 'Add a Codex account', 'name', 'codex-4', 0]);
  assert.deepEqual(name.steps, ['Name the profile', 'Approve the code', 'Signed in']);
  assert.deepEqual(name.actions.map(a => [a.act, a.style]), [['flow-submit', 'primary'], ['flow-cancel', 'ghost']]);
  const job = { id: 'job_1', provider: 'codex', kind: 'device-code', mode: 'add', state: 'waiting', profileName: 'codex-4', verification: { url: 'https://auth.openai.com/codex/device', userCode: 'ABCD-12345', expiresAt: at(14) }, result: null, error: null };
  const waiting = flowView('codex', { type: 'job-add', step: 'job', name: 'codex-4', job, serial: 1 }, { now, trustNote: TRUSTED_NOTE });
  assert.deepEqual([waiting.codeShown, waiting.codeUrl, waiting.codeText, waiting.cur], [true, 'https://auth.openai.com/codex/device', 'ABCD-12345', 1]);
  assert.match(waiting.codeExpires, /^Code expires at /);
  assert.equal(waiting.waiting, 'Waiting for approval');
  assert.deepEqual(waiting.actions.map(a => a.act), ['flow-open-url', 'flow-copy', 'flow-cancel']);
  assert.equal(waiting.note, TRUSTED_NOTE);
  // a redacted job (plain connection) never shows a code
  assert.equal(flowView('codex', { type: 'job-add', step: 'job', job: { ...job, verification: null } }, { now }).codeShown, false);
  const ok = flowView('codex', { type: 'job-add', step: 'job', name: 'codex-4', job: { ...job, state: 'succeeded', verification: null, result: { accountId: 'codex:codex-4', email: 'new@example.test', plan: 'pro' } } }, { now });
  assert.equal(ok.done, 'codex-4 is signed in as new@example.test');
  assert.deepEqual(ok.actions.map(a => a.act), ['flow-done']);
  for (const [code, words] of [['identity_mismatch', /different account/], ['duplicate_identity', /already saved/], ['timeout', /expired/], ['tool_missing', /not installed/], ['server_restarted', /restarted/], ['provider_denied', /refused or cancelled/], ['unexpected_output', /does not recognise/], ['write_failed', /could not be saved/]]) {
    const failed = flowView('codex', { type: 'job-add', step: 'job', job: { ...job, state: 'failed', error: { code } } }, { now });
    assert.match(failed.error, words, code);
    assert.deepEqual(failed.actions.map(a => a.act), ['flow-retry', 'flow-cancel']);
  }
  const again = flowView('codex', { type: 'job-again', step: 'job', email: 'one@example.test', job: { ...job, mode: 'signin-again' } }, { now });
  assert.equal(again.title, 'Approve the sign-in in any browser');
  assert.match(again.body, /history stay/);
  assert.deepEqual(again.steps, ['Approve the code', 'Signed in']);
  // a supervised sign-in asks for the code the provider shows
  const sup = flowView('antigravity', { type: 'job-add', step: 'job', job: { ...job, provider: 'antigravity', kind: 'supervised-cli', state: 'awaiting_code', verification: { url: 'https://accounts.google.com/o/oauth2/auth', userCode: null, expiresAt: null } } }, { now });
  assert.deepEqual([sup.inputKind, sup.codeInput], ['code', true]);
  assert.deepEqual(sup.actions.map(a => a.act), ['flow-submit', 'flow-open-url', 'flow-cancel']);
  assert.equal(flowView('codex', null).open, false);
});

test('flows: Claude profiles, API keys that are never echoed, and guided app or browser sign-ins', () => {
  const claudeName = flowView('claude', { type: 'claude-add', step: 'name', name: 'claude-2' });
  assert.deepEqual([claudeName.title, claudeName.inputSeed, claudeName.actions[0].label], ['Add a Claude account', 'claude-2', 'Create profile']);
  const created = flowView('claude', { type: 'claude-add', step: 'created', name: 'party' });
  assert.equal(created.done, 'Profile party created on Mac and Windows');
  assert.match(created.body, /Needs sign-in/);
  const key = flowView('zai', { type: 'key-add', step: 'key' }, { trustNote: TRUSTED_NOTE });
  assert.deepEqual([key.inputKind, key.inputPassword, key.labelField, key.inputSeed], ['key', true, true, '']);
  assert.equal(key.title, 'Add a Z.ai Coding Plan API key');
  const bad = flowView('zai', { type: 'key-add', step: 'key', error: { title: 'The provider refused the key', body: 'Nothing was stored.' } });
  assert.deepEqual([bad.error, bad.errorBody], ['The provider refused the key', 'Nothing was stored.']);
  const stored = flowView('zai', { type: 'key-add', step: 'done', result: { check: 'ok', account: { credential: { kind: 'aac-key', last4: 'x7Qa' } } } });
  assert.equal(stored.done, 'Key stored, ending in x7Qa');
  const replaced = flowView('kimi-code', { type: 'key-replace', step: 'done', result: { check: 'unverified', account: { credential: { last4: '9999' } } } });
  assert.equal(replaced.done, 'Key replaced, ending in 9999');
  assert.match(replaced.body, /could not be reached/);
  assert.equal(flowView('kimi-code', { type: 'key-replace', step: 'key' }).labelField, false);
  // the flow JSON never carries a key: only the field's kind
  assert.doesNotMatch(JSON.stringify(key), /secret|sk-/);
  const cursor = flowView('cursor', { type: 'guide', accountId: 'cursor:usage', guide: { kind: 'open-app', platforms: ['mac'] } });
  assert.deepEqual(cursor.actions.map(a => [a.act, a.value]), [['flow-open-app', 'cursor:mac'], ['flow-recheck', 'cursor'], ['flow-cancel', 'cursor']]);
  const qwen = flowView('qwen', { type: 'guide', accountId: 'qwen:usage', guide: { kind: 'browser-extension', platform: 'windows' } });
  assert.match(qwen.body, /^On Windows, open the Qwen console/);
  assert.equal(flowView('qwen', { type: 'guide', accountId: 'qwen:usage', guide: { kind: 'browser-extension', platform: 'windows' }, found: true }).done, 'Session found');
});

test('the line under a row: a remove to confirm with its effects, a refusal with its reason, and a request in flight', () => {
  const confirm = lineView('zai:acct:1', 'Work', { kind: 'confirm', token: 'x', effects: ['The stored key is deleted from Ubuntu.'] });
  assert.deepEqual([confirm.shown, confirm.kind, confirm.lead, confirm.text], [true, 'confirm', 'Remove Work?', 'The stored key is deleted from Ubuntu.']);
  assert.deepEqual(confirm.actions.map(a => [a.act, a.style]), [['line-cancel', 'ghost'], ['remove-commit', 'danger-solid']]);
  for (const [code, words] of [['account_active', /active account/], ['account_active_signin', /cannot sign in again/], ['account_default', /saved default/], ['account_protected', /default Claude profile/], ['last_account', /last account/], ['activation_running', /switch runs/], ['signin_running', /sign-in runs/], ['app_running', /open in Claude/], ['app_state_unknown', /could not be checked/]]) {
    const refused = lineView('r', 'x@example.test', { kind: 'refused', code });
    assert.equal(refused.kind, 'refuse');
    assert.match(refused.text, words, code);
    assert.deepEqual(refused.actions.map(a => a.act), ['line-cancel']);
  }
  assert.equal(lineView('r', 'x', { kind: 'removing' }).kind, 'busy');
  assert.equal(lineView('r', 'x', null).shown, false);
  // a row being removed fades
  const vm = accountsViewModel(data([account({ id: 'codex:two', capabilities: { codexProfile: 'two' } })], { providers: providers() }), { now, lines: { 'codex:two': { kind: 'removing' } } });
  assert.equal(row(vm, 'codex:two').gone, true);
});

test('the refresh slider is a log scale from 30 s to 60 min that snaps to its marks', () => {
  assert.equal(refreshFromPosition(0), 30);
  assert.equal(refreshFromPosition(1), 3600);
  assert.equal(refreshFromPosition(refreshPosition(300) + 0.02), 300);
  const between = refreshFromPosition((refreshPosition(300) + refreshPosition(600)) / 2);
  assert.ok(between > 300 && between < 600 && Number.isInteger(between));
  assert.equal(refreshPosition(5), 0);
});
