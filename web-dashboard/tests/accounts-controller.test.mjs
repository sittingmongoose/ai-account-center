// The Accounts & Settings controller against a fake server: every action sends the request the CLIENT API SHEET
// names, and every answer (success or error code) ends in the state and words the page shows.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createAccountsController, JOB_POLL_MS, LEGACY_HIDDEN_KEY } from '../public/accounts-controller.mjs';
import { flowView, lineView } from '../public/accounts-view.mjs';

const refusal = (status, code, extra = {}) => Object.assign(new Error(code), { status, payload: { error: 'fixed sentence', code, ...extra }, headers: { get: () => null } });

/** A fake server: `routes` maps "METHOD path" to an answer, a function of the body, or a list used in order. */
function harness({ routes = {}, data = null, storage = null } = {}) {
  const sent = [], toasts = [], timers = [];
  let dashboard = data || { schemaVersion: 1, accounts: [], settings: { hiddenProviders: [], hiddenAccountIds: [], visibilityAvailable: true }, providers: [] };
  let strengthView = null, opened = null, copied = null, refreshes = 0;
  const answer = async (req) => {
    sent.push(req);
    const key = `${req.method} ${req.path}`;
    let route = routes[key];
    if (Array.isArray(route)) route = route.length > 1 ? route.shift() : route[0];
    if (route === undefined) throw refusal(404, 'not_found');
    const value = typeof route === 'function' ? route(req.body) : route;
    if (value instanceof Error) throw value;
    return { status: value?.status ?? 200, payload: value?.payload ?? value };
  };
  const ctl = createAccountsController({
    call: answer,
    toast: (kind, title, body) => toasts.push({ kind, title, body }),
    refresh: async () => { refreshes++; },
    data: () => dashboard,
    setData: next => { dashboard = next; },
    strength: view => { strengthView = view; },
    copy: async value => { copied = value; return true; },
    open: url => { opened = url; },
    schedule: (fn, ms) => { timers.push({ fn, ms }); return timers.length; },
    cancel: () => {},
    storage,
  });
  return {
    ctl, sent, toasts, timers,
    get dashboard() { return dashboard; },
    get strength() { return strengthView; },
    get opened() { return opened; },
    get copied() { return copied; },
    get refreshes() { return refreshes; },
    async tick() { const t = timers.shift(); if (t) await t.fn(); },
  };
}
const memoryStorage = (init = {}) => {
  const map = new Map(Object.entries(init));
  return { getItem: k => map.has(k) ? map.get(k) : null, setItem: (k, v) => map.set(k, String(v)), removeItem: k => map.delete(k), map };
};
const last = list => list[list.length - 1];

test('Show on dashboard and Show in tray save on the server and update the page without a reload', async () => {
  // the server answers with every saved list; a body may update one list and leave the others (tray route)
  const saved = { hiddenProviders: ['zai'], hiddenAccountIds: ['codex:x'], trayHiddenProviders: [] };
  const h = harness({ routes: { 'PUT /api/accounts/visibility': body => Object.assign(saved, body) && { ...saved } },
    data: { schemaVersion: 1, accounts: [{ id: 'kimi-code:usage', provider: 'kimi-code' }], providers: [{ id: 'kimi-code', visible: true }], settings: { hiddenProviders: ['zai'], hiddenAccountIds: ['codex:x'], trayHiddenProviders: [] } } });
  assert.equal(await h.ctl.handle('accounts-show', 'kimi-code:hide'), true);
  assert.deepEqual(h.sent[0], { method: 'PUT', path: '/api/accounts/visibility', body: { hiddenProviders: ['zai', 'kimi-code'], hiddenAccountIds: ['codex:x'] } });
  assert.deepEqual(h.dashboard.settings.hiddenProviders, ['zai', 'kimi-code']);
  assert.equal(h.dashboard.providers[0].visible, false);
  assert.equal(h.dashboard.accounts[0].hidden, true);
  assert.equal(last(h.toasts).title, 'Kimi Code hidden from the dashboard');
  await h.ctl.handle('accounts-tray', 'kimi-code:hide');
  assert.deepEqual(h.sent[1].body, { trayHiddenProviders: ['kimi-code'] });
  assert.deepEqual(h.dashboard.settings.trayHiddenProviders, ['kimi-code']);
  assert.equal(h.dashboard.providers[0].trayVisible, false);
  await h.ctl.handle('accounts-show', 'kimi-code:show');
  assert.deepEqual(h.sent[2].body.hiddenProviders, ['zai']);
  // refused: the page keeps the server's lists and says why
  const bad = harness({ routes: { 'PUT /api/accounts/visibility': refusal(500, 'visibility_write_failed') } });
  await bad.ctl.handle('accounts-show', 'zai:hide');
  assert.equal(last(bad.toasts).title, 'Not saved');
  assert.deepEqual(bad.dashboard.settings.hiddenProviders, []);
  // without the tray field the server has no tray route: nothing is sent
  const old = harness();
  await old.ctl.handle('accounts-tray', 'zai:hide');
  assert.equal(old.sent.length, 0);
});

test('a browser-only "Show on dashboard" choice moves to the server once, then the browser copy is cleared', async () => {
  const storage = memoryStorage({ [LEGACY_HIDDEN_KEY]: JSON.stringify(['kimi-code', 'nope']) });
  const h = harness({ storage, routes: { 'PUT /api/accounts/visibility': body => body } });
  assert.equal(await h.ctl.migrateLocalHidden(), true);
  assert.deepEqual(h.sent[0].body, { hiddenProviders: ['kimi-code'], hiddenAccountIds: [] });
  assert.equal(storage.map.has(LEGACY_HIDDEN_KEY), false);
  assert.equal(await h.ctl.migrateLocalHidden(), false);
  assert.equal(h.sent.length, 1);
  // nothing stored: nothing sent; an unreadable visibility file: wait
  const empty = harness({ storage: memoryStorage() });
  assert.equal(await empty.ctl.migrateLocalHidden(), false);
  assert.equal(empty.sent.length, 0);
  const unreadable = harness({ storage: memoryStorage({ [LEGACY_HIDDEN_KEY]: '["zai"]' }), data: { settings: { visibilityAvailable: false } } });
  assert.equal(await unreadable.ctl.migrateLocalHidden(), false);
  assert.equal(unreadable.sent.length, 0);
  // a failed save keeps the browser copy for the next load
  const fails = harness({ storage: memoryStorage({ [LEGACY_HIDDEN_KEY]: '["zai"]' }), routes: { 'PUT /api/accounts/visibility': refusal(500, 'visibility_write_failed') } });
  assert.equal(await fails.ctl.migrateLocalHidden(), false);
});

test('Codex Add: name, device code, approval and signed in, polling the job every 2 s', async () => {
  const job = { id: 'job_0123456789abcdef', provider: 'codex', kind: 'device-code', mode: 'add', state: 'waiting', profileName: 'codex-2', verification: { url: 'https://auth.openai.com/codex/device', userCode: 'ABCD-12345', expiresAt: null }, result: null, error: null };
  const h = harness({ routes: {
    'POST /api/accounts/add': { status: 202, payload: { job: { ...job, state: 'starting', verification: null } } },
    [`GET /api/accounts/signin-jobs/${job.id}`]: [job, { ...job, state: 'verifying' }, { ...job, state: 'succeeded', verification: null, result: { accountId: 'codex:codex-2', email: 'two@example.test', plan: 'pro' } }],
    'GET /api/accounts/registry': { providers: [], accounts: [], jobs: [], trash: [] },
  }, data: { accounts: [{ id: 'codex:one', provider: 'codex', capabilities: { codexProfile: 'one' } }], settings: {} } });
  await h.ctl.handle('add', 'codex');
  assert.deepEqual([h.ctl.state.flows.codex.type, h.ctl.state.flows.codex.step, h.ctl.state.flows.codex.name], ['job-add', 'name', 'codex-2']);
  // a bad name never leaves the page
  await h.ctl.handle('flow-submit', 'codex\none\n');
  assert.equal(h.sent.length, 0);
  assert.match(h.ctl.state.flows.codex.error.title, /already a profile name/);
  await h.ctl.handle('flow-submit', 'codex\ncodex-2\n');
  assert.deepEqual(h.sent[0], { method: 'POST', path: '/api/accounts/add', body: { provider: 'codex', profileName: 'codex-2' } });
  assert.equal(h.ctl.state.flows.codex.step, 'job');
  assert.equal(h.timers[0].ms, JOB_POLL_MS);
  await h.tick();
  assert.equal(h.ctl.state.flows.codex.job.verification.userCode, 'ABCD-12345');
  await h.ctl.handle('flow-copy', 'codex');
  assert.equal(h.copied, 'ABCD-12345');
  await h.ctl.handle('flow-open-url', 'codex');
  assert.equal(h.opened, 'https://auth.openai.com/codex/device');
  await h.tick();
  assert.equal(h.ctl.state.flows.codex.job.state, 'verifying');
  await h.tick();
  assert.equal(h.ctl.state.flows.codex.job.state, 'succeeded');
  assert.equal(last(h.toasts).title, 'codex-2 added');
  assert.equal(h.timers.length, 0, 'polling stops when the job ends');
  assert.equal(flowView('codex', h.ctl.state.flows.codex).done, 'codex-2 is signed in as two@example.test');
  await h.ctl.handle('flow-done', 'codex');
  assert.equal(h.ctl.state.flows.codex, undefined);
});

test('Codex Add and Sign in again: every refusal ends in plain words, and Cancel stops the job', async () => {
  for (const [error, words] of [
    [refusal(403, 'secure_transport_required', { fallback: { kind: 'terminal', host: 'ubuntu', command: 'ai-account-center codex-auth login <profile-name>' } }), /trusted connection.*codex-auth login/s],
    [refusal(409, 'tool_missing'), /not installed/],
    [refusal(409, 'id_in_use'), /already used/],
    [refusal(409, 'job_running', { jobId: 'job_1' }), /already running/],
    [refusal(409, 'too_many_jobs'), /Too many sign-ins/],
    [refusal(409, 'too_many_accounts'), /limit/],
    [refusal(500, 'internal_error'), /Nothing was changed/],
  ]) {
    const h = harness({ routes: { 'POST /api/accounts/add': error } });
    await h.ctl.handle('add', 'codex');
    await h.ctl.handle('flow-submit', 'codex\ncodex-9\n');
    const flow = h.ctl.state.flows.codex;
    assert.equal(flow.step, 'name', error.payload.code);
    assert.match(`${flow.error.title} ${flow.error.body}`, words, error.payload.code);
  }
  // Sign in again on the live active account is refused under the row
  const active = harness({ routes: { 'POST /api/accounts/codex%3Aone/signin-again': refusal(409, 'account_active') }, data: { accounts: [{ id: 'codex:one', provider: 'codex' }], settings: {} } });
  await active.ctl.handle('signin-again', 'codex:one');
  assert.deepEqual(active.ctl.state.lines['codex:one'], { kind: 'refused', code: 'account_active_signin' });
  // Cancel: the job is cancelled on the server and the panel closes
  const job = { id: 'job_aaaaaaaaaaaaaaaa', provider: 'codex', kind: 'device-code', mode: 'signin-again', state: 'waiting', verification: null };
  const h = harness({ routes: {
    'POST /api/accounts/codex%3Aone/signin-again': { status: 202, payload: { job } },
    [`POST /api/accounts/signin-jobs/${job.id}/cancel`]: { ...job, state: 'cancelled' },
  }, data: { accounts: [{ id: 'codex:one', provider: 'codex', email: 'one@example.test' }], settings: {} } });
  await h.ctl.handle('signin-again', 'codex:one');
  assert.equal(h.ctl.state.flows.codex.type, 'job-again');
  assert.equal(h.ctl.state.flows.codex.email, 'one@example.test');
  await h.ctl.handle('flow-cancel', 'codex');
  assert.deepEqual(last(h.sent), { method: 'POST', path: `/api/accounts/signin-jobs/${job.id}/cancel`, body: {} });
  assert.equal(h.ctl.state.flows.codex, undefined);
  assert.equal(last(h.toasts).title, 'Sign-in cancelled');
  // a cancel during the install keeps following the job
  const late = harness({ routes: {
    'POST /api/accounts/codex%3Aone/signin-again': { status: 202, payload: { job } },
    [`POST /api/accounts/signin-jobs/${job.id}/cancel`]: { ...job, state: 'verifying' },
  }, data: { accounts: [{ id: 'codex:one', provider: 'codex' }], settings: {} } });
  await late.ctl.handle('signin-again', 'codex:one');
  await late.ctl.handle('flow-cancel', 'codex');
  assert.equal(late.ctl.state.flows.codex.job.state, 'verifying');
  // a job the server no longer knows ends as restarted
  const lost = harness({ routes: { 'POST /api/accounts/codex%3Aone/signin-again': { status: 202, payload: { job } } }, data: { accounts: [{ id: 'codex:one', provider: 'codex' }], settings: {} } });
  await lost.ctl.handle('signin-again', 'codex:one');
  await lost.tick();
  assert.equal(lost.ctl.state.flows.codex.job.state, 'failed');
  assert.match(flowView('codex', lost.ctl.state.flows.codex).error, /restarted/);
});

test('a supervised sign-in sends the pasted code once', async () => {
  const job = { id: 'job_bbbbbbbbbbbbbbbb', provider: 'antigravity', kind: 'supervised-cli', mode: 'add', state: 'awaiting_code', verification: { url: 'https://accounts.google.com/o/oauth2/auth', userCode: null, expiresAt: null } };
  const h = harness({ routes: {
    'POST /api/accounts/add': { status: 202, payload: { job } },
    [`POST /api/accounts/signin-jobs/${job.id}/code`]: { ...job, state: 'verifying' },
    [`GET /api/accounts/signin-jobs/${job.id}`]: { ...job, state: 'verifying' },
  } });
  await h.ctl.handle('add', 'antigravity');
  await h.ctl.handle('flow-submit', 'antigravity\nwork\n');
  assert.deepEqual(h.sent[0].body, { provider: 'antigravity', profileName: 'work' });
  await h.ctl.handle('flow-submit', 'antigravity\n4/0Abc-def\n');
  assert.deepEqual(last(h.sent), { method: 'POST', path: `/api/accounts/signin-jobs/${job.id}/code`, body: { code: '4/0Abc-def' } });
  assert.equal(h.ctl.state.flows.antigravity.job.state, 'verifying');
  const wrong = harness({ routes: { 'POST /api/accounts/add': { status: 202, payload: { job } }, [`POST /api/accounts/signin-jobs/${job.id}/code`]: refusal(409, 'code_not_expected') } });
  await wrong.ctl.handle('add', 'antigravity');
  await wrong.ctl.handle('flow-submit', 'antigravity\nwork\n');
  await wrong.ctl.handle('flow-submit', 'antigravity\ncode\n');
  assert.match(wrong.ctl.state.flows.antigravity.error.title, /Not waiting for a code/);
});

test('API keys: add with a label, replace, and every refusal; the key never stays in the state', async () => {
  const account = { id: 'zai:acct:9f2c41d0', provider: 'zai', credential: { kind: 'aac-key', last4: 'x7Qa', fingerprint: 'sha256:0123', storedOn: 'ubuntu' } };
  const h = harness({ routes: { 'POST /api/accounts/add': { status: 201, payload: { account, check: 'ok' } }, 'GET /api/accounts/registry': { accounts: [], trash: [] } } });
  await h.ctl.handle('add-key', 'zai');
  assert.deepEqual([h.ctl.state.flows.zai.type, h.ctl.state.flows.zai.step, h.ctl.state.flows.zai.second], ['key-add', 'key', false]);
  await h.ctl.handle('flow-submit', 'zai\nshort\n');
  assert.equal(h.sent.length, 0);
  assert.match(h.ctl.state.flows.zai.error.title, /too short/);
  await h.ctl.handle('flow-submit', 'zai\nzk-0123456789abcdefx7Qa\nWork');
  assert.deepEqual(h.sent[0], { method: 'POST', path: '/api/accounts/add', body: { provider: 'zai', key: 'zk-0123456789abcdefx7Qa', label: 'Work' } });
  assert.equal(h.ctl.state.flows.zai.step, 'done');
  assert.equal(flowView('zai', h.ctl.state.flows.zai).done, 'Key stored, ending in x7Qa');
  assert.doesNotMatch(JSON.stringify(h.ctl.state), /zk-0123456789abcdef/);
  assert.equal(last(h.toasts).body, 'It ends in x7Qa; the key itself is never shown again.');
  for (const [error, words] of [[refusal(422, 'key_rejected'), /refused the key.*Nothing was stored/s], [refusal(409, 'duplicate_key'), /already saved/], [refusal(403, 'secure_transport_required'), /trusted connection/], [refusal(500, 'key_store_unavailable'), /could not be stored/], [refusal(409, 'too_many_accounts'), /limit/]]) {
    const bad = harness({ routes: { 'POST /api/accounts/add': error } });
    await bad.ctl.handle('add-key', 'zai');
    await bad.ctl.handle('flow-submit', 'zai\nzk-0123456789abcdef\n');
    assert.equal(bad.ctl.state.flows.zai.step, 'key', error.payload.code);
    assert.match(`${bad.ctl.state.flows.zai.error.title} ${bad.ctl.state.flows.zai.error.body}`, words, error.payload.code);
  }
  const replace = harness({ routes: { 'PUT /api/accounts/zai%3Aacct%3A9f2c41d0/key': { account: { ...account, credential: { ...account.credential, last4: '9999' } }, check: 'unverified' } }, data: { accounts: [account], settings: {} } });
  await replace.ctl.handle('replace-key', 'zai:acct:9f2c41d0');
  assert.equal(replace.ctl.state.flows.zai.type, 'key-replace');
  await replace.ctl.handle('flow-submit', 'zai\nzk-new-key-0009999\n');
  assert.deepEqual(replace.sent[0], { method: 'PUT', path: '/api/accounts/zai%3Aacct%3A9f2c41d0/key', body: { key: 'zk-new-key-0009999' } });
  assert.equal(flowView('zai', replace.ctl.state.flows.zai).done, 'Key replaced, ending in 9999');
  const kept = harness({ routes: { 'PUT /api/accounts/zai%3Aacct%3A9f2c41d0/key': refusal(422, 'key_rejected') }, data: { accounts: [account], settings: {} } });
  await kept.ctl.handle('replace-key', 'zai:acct:9f2c41d0');
  await kept.ctl.handle('flow-submit', 'zai\nzk-new-key-0009999\n');
  assert.equal(kept.ctl.state.flows.zai.error.body, 'The old key is still in use.');
  const notOurs = harness({ routes: { 'PUT /api/accounts/zai%3Ausage/key': refusal(409, 'not_aac_owned') }, data: { accounts: [{ id: 'zai:usage', provider: 'zai' }], settings: {} } });
  await notOurs.ctl.handle('replace-key', 'zai:usage');
  await notOurs.ctl.handle('flow-submit', 'zai\nzk-new-key-0009999\n');
  assert.match(notOurs.ctl.state.flows.zai.error.title, /another app/);
});

test('Claude Add: the profile is created on both hosts; a host that cannot be reached changes nothing', async () => {
  const h = harness({ routes: { 'POST /api/accounts/add': { status: 201, payload: { account: { id: 'claude:party' }, launchers: { mac: 'created', windows: 'created' } } }, 'GET /api/accounts/registry': { accounts: [], trash: [] } },
    data: { accounts: [{ id: 'claude:home', provider: 'claude', capabilities: { claudeProfileId: 'home' } }], settings: {} } });
  await h.ctl.handle('add', 'claude');
  assert.equal(h.ctl.state.flows.claude.name, 'claude-2');
  await h.ctl.handle('flow-submit', 'claude\nHome\n');
  assert.match(h.ctl.state.flows.claude.error.title, /lowercase/);
  await h.ctl.handle('flow-submit', 'claude\nhome\n');
  assert.match(h.ctl.state.flows.claude.error.title, /already a Claude profile/);
  await h.ctl.handle('flow-submit', 'claude\nparty\n');
  assert.deepEqual(h.sent[0], { method: 'POST', path: '/api/accounts/add', body: { provider: 'claude', profileId: 'party' } });
  assert.equal(h.ctl.state.flows.claude.step, 'created');
  for (const [error, words] of [[refusal(502, 'host_unreachable', { host: 'windows' }), /^Windows could not be reached Nothing was changed/], [refusal(409, 'not_implemented'), /Not on this server yet/], [refusal(409, 'id_in_use'), /already used/], [refusal(409, 'not_configured'), /no launcher/]]) {
    const bad = harness({ routes: { 'POST /api/accounts/add': error } });
    await bad.ctl.handle('add', 'claude');
    await bad.ctl.handle('flow-submit', 'claude\nparty\n');
    assert.equal(bad.ctl.state.flows.claude.step, 'name');
    assert.match(`${bad.ctl.state.flows.claude.error.title} ${bad.ctl.state.flows.claude.error.body}`, words, error.payload.code);
  }
});

test('Remove: the confirmation names the effects, the token is sent back once, and refusals stay under the row', async () => {
  const token = 'T'.repeat(43);
  const h = harness({ routes: {
    'POST /api/accounts/claude%3Aparty/remove': body => body.confirmationToken
      ? { removed: true, trashId: 'tr_0123456789abcdef', purgeAfter: '2026-11-01T12:00:00.000Z' }
      : { confirmation: { token, expiresAt: '2026-10-02T12:02:00Z', effects: ['Its Claude data moves to the trash on Mac and Windows for 30 days.'] } },
    'GET /api/accounts/registry': { accounts: [], trash: [] },
  }, data: { accounts: [{ id: 'claude:party', provider: 'claude', email: 'party@example.test' }], settings: {} } });
  await h.ctl.handle('remove', 'claude:party');
  assert.deepEqual(h.sent[0].body, {});
  const line = lineView('claude:party', 'party@example.test', h.ctl.state.lines['claude:party']);
  assert.deepEqual([line.kind, line.lead, line.text], ['confirm', 'Remove party@example.test?', 'Its Claude data moves to the trash on Mac and Windows for 30 days.']);
  await h.ctl.handle('remove-commit', 'claude:party');
  assert.deepEqual(h.sent[1].body, { confirmationToken: token });
  assert.equal(h.ctl.state.lines['claude:party'], undefined);
  assert.equal(last(h.toasts).title, 'Removed party@example.test');
  assert.match(last(h.toasts).body, /trash on Mac and Windows until Nov 1/);
  assert.equal(h.refreshes, 1);
  // Keep closes the line and sends nothing
  await h.ctl.handle('remove', 'claude:party');
  await h.ctl.handle('line-cancel', 'claude:party');
  assert.equal(h.ctl.state.lines['claude:party'], undefined);
  for (const code of ['account_active', 'account_default', 'account_protected', 'last_account', 'activation_running', 'signin_running', 'app_running', 'app_state_unknown']) {
    const r = harness({ routes: { 'POST /api/accounts/codex%3Aone/remove': refusal(409, code) } });
    await r.ctl.handle('remove', 'codex:one');
    assert.deepEqual(r.ctl.state.lines['codex:one'], { kind: 'refused', code });
  }
  for (const [error, title] of [[refusal(409, 'confirmation_stale'), 'It changed'], [refusal(500, 'remove_failed'), 'Not removed'], [refusal(409, 'trash_cross_volume', { host: 'mac' }), 'Trash is on another disk'], [refusal(502, 'host_unreachable', { host: 'mac' }), 'The Mac could not be reached']]) {
    const r = harness({ routes: { 'POST /api/accounts/claude%3Aparty/remove': body => body.confirmationToken ? error : { confirmation: { token, effects: [] } }, 'GET /api/accounts/registry': { accounts: [] } } });
    await r.ctl.handle('remove', 'claude:party');
    await r.ctl.handle('remove-commit', 'claude:party');
    assert.equal(last(r.toasts).title, title, error.payload.code);
    assert.equal(r.ctl.state.lines['claude:party'], undefined);
  }
  // a refused control opens its reason without a request
  const local = harness();
  await local.ctl.handle('refuse', 'claude:home\naccount_protected');
  assert.deepEqual(local.ctl.state.lines['claude:home'], { kind: 'refused', code: 'account_protected' });
  assert.equal(local.sent.length, 0);
  // Remove that the server has no flow for
  const off = harness({ routes: { 'POST /api/accounts/antigravity%3Aone/remove': refusal(409, 'not_implemented') } });
  await off.ctl.handle('remove', 'antigravity:one');
  assert.equal(last(off.toasts).title, 'Not on this server yet');
});

test('Restore from the Claude trash: confirmation, then back on both hosts', async () => {
  const token = 'R'.repeat(43);
  const h = harness({ routes: {
    'POST /api/accounts/trash/tr_0123456789abcdef/restore': body => body.confirmationToken ? { restored: true, accountId: 'claude:party' } : { confirmation: { token, effects: ['Its Claude data moves back on Mac and Windows.'] } },
    'GET /api/accounts/registry': { accounts: [], trash: [] },
  } });
  h.ctl.state.registry = { accounts: [], trash: [{ trashId: 'tr_0123456789abcdef', provider: 'claude', label: 'party@example.test' }] };
  await h.ctl.handle('restore', 'tr_0123456789abcdef');
  assert.equal(h.ctl.state.lines['trash:tr_0123456789abcdef'].kind, 'confirm');
  await h.ctl.handle('restore-commit', 'tr_0123456789abcdef');
  assert.deepEqual(h.sent[1].body, { confirmationToken: token });
  assert.equal(last(h.toasts).title, 'Restored party@example.test');
  for (const [error, title] of [[refusal(409, 'not_implemented'), 'Not on this server yet'], [refusal(404, 'unknown_trash'), 'No longer in the trash'], [refusal(409, 'id_in_use'), 'Name already used']]) {
    const r = harness({ routes: { 'POST /api/accounts/trash/tr_1/restore': error, 'GET /api/accounts/registry': { accounts: [] } } });
    await r.ctl.handle('restore', 'tr_1');
    assert.equal(last(r.toasts).title, title, error.payload.code);
  }
  const fails = harness({ routes: { 'POST /api/accounts/trash/tr_1/restore': body => body.confirmationToken ? refusal(500, 'restore_failed') : { confirmation: { token, effects: [] } }, 'GET /api/accounts/registry': { accounts: [] } } });
  await fails.ctl.handle('restore', 'tr_1');
  await fails.ctl.handle('restore-commit', 'tr_1');
  assert.equal(last(fails.toasts).title, 'Not restored');
});

test('guided sign-ins: Qwen and Cursor open their guide, add their one account first, re-check and open the app', async () => {
  const h = harness({ routes: {
    'POST /api/accounts/qwen%3Ausage/signin-again': { guide: { kind: 'browser-extension', platform: 'windows' } },
    'POST /api/accounts/qwen%3Ausage/recheck': [{ account: { id: 'qwen:usage', status: 'needs_sign_in' } }, { account: { id: 'qwen:usage', status: 'ok' } }],
    'GET /api/accounts/registry': { accounts: [] },
  }, data: { accounts: [{ id: 'qwen:usage', provider: 'qwen' }], settings: {} } });
  await h.ctl.handle('session-signin', 'qwen');
  assert.deepEqual(h.ctl.state.flows.qwen.guide, { kind: 'browser-extension', platform: 'windows' });
  await h.ctl.handle('flow-recheck', 'qwen');
  assert.equal(h.ctl.state.flows.qwen.found, false);
  assert.match(h.ctl.state.flows.qwen.error.title, /No session found yet/);
  await h.ctl.handle('flow-recheck', 'qwen');
  assert.equal(h.ctl.state.flows.qwen.found, true);
  // Cursor with no account: Add, then its guide; Open on Mac
  const c = harness({ routes: {
    'POST /api/accounts/add': { status: 201, payload: { account: { id: 'cursor:usage' } } },
    'POST /api/accounts/cursor%3Ausage/signin-again': { guide: { kind: 'open-app', platforms: ['mac'] } },
    'POST /api/accounts/cursor%3Ausage/open': { opened: true },
    'GET /api/accounts/registry': { accounts: [] },
  } });
  await c.ctl.handle('session-signin', 'cursor');
  assert.deepEqual(c.sent[0], { method: 'POST', path: '/api/accounts/add', body: { provider: 'cursor' } });
  assert.equal(c.ctl.state.flows.cursor.accountId, 'cursor:usage');
  await c.ctl.handle('flow-open-app', 'cursor:mac');
  assert.deepEqual(last(c.sent), { method: 'POST', path: '/api/accounts/cursor%3Ausage/open', body: { platform: 'mac' } });
  assert.equal(last(c.toasts).title, 'Opening Cursor on the Mac');
  for (const [error, title] of [[refusal(409, 'not_configured'), 'Not set up there'], [refusal(502, 'host_unreachable', { host: 'mac' }), 'The Mac could not be reached']]) {
    const o = harness({ routes: { 'POST /api/accounts/cursor%3Ausage/signin-again': { guide: { kind: 'open-app', platforms: ['mac'] } }, 'POST /api/accounts/cursor%3Ausage/open': error }, data: { accounts: [{ id: 'cursor:usage', provider: 'cursor' }], settings: {} } });
    await o.ctl.handle('session-signin', 'cursor');
    await o.ctl.handle('flow-open-app', 'cursor:mac');
    assert.equal(last(o.toasts).title, title);
  }
  // Re-check from the footer, and its pause
  const r = harness({ routes: { 'POST /api/accounts/muse%3Ausage/recheck': refusal(429, 'rate_limited', { retryAfterSeconds: 8 }) }, data: { accounts: [{ id: 'muse:usage', provider: 'muse' }], settings: {} } });
  await r.ctl.handle('recheck', 'muse:usage');
  assert.deepEqual([last(r.toasts).kind, last(r.toasts).title, last(r.toasts).body], ['info', 'Muse Code was checked a moment ago', 'Try again in 8 seconds.']);
  const many = harness({ routes: { 'POST /api/accounts/add': refusal(409, 'single_account_provider') } });
  await many.ctl.handle('session-signin', 'qwen');
  assert.equal(last(many.toasts).title, 'One account only');
  const muse = harness({ routes: { 'POST /api/accounts/muse%3Ausage/signin-again': refusal(409, 'not_implemented') }, data: { accounts: [{ id: 'muse:usage', provider: 'muse' }], settings: {} } });
  await muse.ctl.handle('session-signin', 'muse');
  assert.equal(last(muse.toasts).title, 'Not on this server yet');
});

test('the Dashboard sign-in block: other browsers, network trust, devices and sign out all devices', async () => {
  const session = { username: 'owner', otherBrowsers: 2, managedBy: 'config', secureTransport: true };
  const devices = [{ id: 'dev_0123456789abcdef', name: 'Mac tray', platform: 'mac' }, { id: 'dev_fedcba9876543210', name: 'Windows tray', platform: 'windows' }];
  const network = { trustLocalNetwork: true, trustedNetworks: [], connection: { peer: '192.168.50.20', trusted: true }, canTurnOn: false };
  const h = harness({ routes: {
    'GET /api/auth/session': session, 'GET /api/auth/devices': { devices }, 'GET /api/auth/network': network,
    'POST /api/auth/sessions/revoke-others': { signedOutBrowsers: 2 },
    'PUT /api/auth/network': body => ({ ...network, trustLocalNetwork: body.trustLocalNetwork, connection: { ...network.connection, trusted: body.trustLocalNetwork } }),
    'DELETE /api/auth/devices/dev_0123456789abcdef': { status: 204, payload: null },
    'POST /api/auth/devices/revoke-all': { revokedDevices: 2, signedOutBrowsers: 1 },
  } });
  await h.ctl.loadSignin();
  assert.equal(h.ctl.state.signin.session.otherBrowsers, 2);
  assert.equal(h.ctl.state.signin.devices.length, 2);
  await h.ctl.handle('others-out', '');
  assert.deepEqual(h.sent.find(r => r.path === '/api/auth/sessions/revoke-others'), { method: 'POST', path: '/api/auth/sessions/revoke-others', body: {} });
  assert.equal(h.toasts[0].title, 'Signed out 2 other browsers');
  await h.ctl.handle('network-off', '');
  assert.deepEqual(h.sent.find(r => r.method === 'PUT').body, { trustLocalNetwork: false });
  assert.equal(last(h.toasts).title, 'Local network trust turned off');
  await h.ctl.handle('device-revoke', 'dev_0123456789abcdef');
  assert.ok(h.sent.some(r => r.method === 'DELETE' && r.path === '/api/auth/devices/dev_0123456789abcdef'));
  assert.equal(last(h.toasts).title, 'Mac tray signed out');
  await h.ctl.handle('devices-revoke-all', '');
  assert.deepEqual(h.sent.find(r => r.path === '/api/auth/devices/revoke-all').body, { signOutOtherBrowsers: true });
  assert.deepEqual([last(h.toasts).title, last(h.toasts).body], ['Signed out all devices', '2 trays show the pairing screen; 1 other browser was signed out. This browser stays signed in.']);
  // refusals
  for (const [route, action, value, error, title] of [
    ['PUT /api/auth/network', 'network-on', '', refusal(403, 'loopback_required'), 'Turn it on at the dashboard computer'],
    ['PUT /api/auth/network', 'network-off', '', refusal(500, 'write_failed'), 'Not saved'],
    ['DELETE /api/auth/devices/dev_x', 'device-revoke', 'dev_x', refusal(404, 'unknown_device'), 'Already signed out'],
    ['DELETE /api/auth/devices/dev_x', 'device-revoke', 'dev_x', refusal(503, 'auth_store_unavailable'), 'Device list unavailable'],
    ['POST /api/auth/devices/revoke-all', 'devices-revoke-all', '', refusal(503, 'auth_store_unavailable'), 'Device list unavailable'],
    ['POST /api/auth/sessions/revoke-others', 'others-out', '', refusal(429, 'rate_limited', { retryAfterSeconds: 600 }), 'Paused after too many tries'],
  ]) {
    const r = harness({ routes: { [route]: error } });
    await r.ctl.handle(action, value);
    assert.equal(r.toasts[0].title, title, error.payload.code);
  }
});

test('the password change: local checks, the server answer, the toast that names the trays, and every refusal', async () => {
  const devices = [{ id: 'dev_1', name: 'Mac tray', platform: 'mac' }, { id: 'dev_2', name: 'Windows tray', platform: 'windows' }];
  const h = harness({ routes: {
    'POST /api/auth/password': { ok: true, passwordChangedAt: '2026-10-02T15:00:00Z', signedOutBrowsers: 1, pairedDevices: 2, session: { expiresAt: '2026-10-03T15:00:00Z' } },
    'GET /api/auth/session': { otherBrowsers: 0 }, 'GET /api/auth/devices': { devices }, 'GET /api/auth/network': {},
  } });
  h.ctl.state.signin.devices = devices;
  await h.ctl.handle('pw-toggle', '');
  assert.equal(h.ctl.state.signin.pw.open, true);
  await h.ctl.handle('pw-typing', 'summit-ledger-42\nsummit-ledger-42');
  assert.deepEqual([h.strength.word, h.strength.matches], ['Strong', true]);
  for (const [value, field, words] of [['\nnew-password-1\nnew-password-1\n1', 'cur', /current password/], ['old-pass-1\nshort\nshort\n1', 'new', /8 characters/], ['same-pass-1\nsame-pass-1\nsame-pass-1\n1', 'new', /different/], ['old-pass-1\nnew-password-1\nnew-password-2\n1', 'conf', /match/]]) {
    const nonce = h.ctl.state.signin.pw.nonce;
    await h.ctl.handle('pw-submit', value);
    assert.equal(h.ctl.state.signin.pw.field, field);
    assert.match(h.ctl.state.signin.pw.error, words);
    assert.equal(h.ctl.state.signin.pw.nonce, nonce + 1, 'a refused form shakes');
  }
  assert.equal(h.sent.length, 0);
  await h.ctl.handle('pw-submit', 'old-pass-1\nnew-password-1\nnew-password-1\n0');
  assert.deepEqual(h.sent[0], { method: 'POST', path: '/api/auth/password', body: { currentPassword: 'old-pass-1', newPassword: 'new-password-1', signOutOtherBrowsers: false } });
  assert.equal(h.ctl.state.signin.pw.done, true);
  assert.deepEqual([last(h.toasts).title, last(h.toasts).body], ['Password changed. Mac tray and Windows tray stay signed in.', '1 other browser was signed out; this one stays signed in.']);
  await h.tick();
  assert.deepEqual([h.ctl.state.signin.pw.open, h.ctl.state.signin.pw.done], [false, false]);
  for (const [error, field, words] of [
    [refusal(401, 'wrong_password', { triesLeft: 3 }), 'cur', /isn't the current password.*3 tries left/s],
    [refusal(401, 'wrong_password', { triesLeft: 0 }), 'cur', /last try/],
    [refusal(429, 'rate_limited', { retryAfterSeconds: 840 }), 'cur', /14 minutes/],
    [refusal(400, 'weak_password', { reason: 'too_long' }), 'new', /72 bytes/],
    [refusal(400, 'same_password'), 'new', /different/],
    [refusal(403, 'secure_transport_required', { secureOrigin: null }), 'form', /trusted connection/],
    [refusal(409, 'managed_by_env'), 'form', /environment variables/],
    [refusal(500, 'invalid_hash'), 'form', /dashboard auth setup/],
  ]) {
    const r = harness({ routes: { 'POST /api/auth/password': error } });
    await r.ctl.handle('pw-toggle', '');
    await r.ctl.handle('pw-submit', 'old-pass-1\nnew-password-1\nnew-password-1\n1');
    assert.equal(r.ctl.state.signin.pw.field, field, error.payload.code);
    assert.match(r.ctl.state.signin.pw.error, words, error.payload.code);
    assert.equal(r.ctl.state.signin.pw.busy, false);
  }
  // a success whose session could not be renewed says so
  const rot = harness({ routes: { 'POST /api/auth/password': { ok: true, signedOutBrowsers: 0, pairedDevices: 0, session: null, code: 'session_rotation_failed' }, 'GET /api/auth/session': {}, 'GET /api/auth/devices': { devices: [] }, 'GET /api/auth/network': {} } });
  await rot.ctl.handle('pw-toggle', '');
  await rot.ctl.handle('pw-submit', 'old-pass-1\nnew-password-1\nnew-password-1\n1');
  assert.match(last(rot.toasts).body, /sign in again/);
});

test('the actions this controller does not own fall through to bridge.js', async () => {
  const h = harness();
  for (const action of ['activate', 'launch', 'logout', 'refresh', 'theme', 'details']) assert.equal(await h.ctl.handle(action, ''), false);
});
