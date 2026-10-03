// The Accounts & Settings controller against a fake server: every action sends the request the CLIENT API SHEET
// names, and every answer (success or error code) ends in the state and words the page shows.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createAccountsController, JOB_POLL_MS, LEGACY_HIDDEN_KEY, MUTATING_ACTIONS } from '../public/accounts-controller.mjs';
import { flowView, lineView } from '../public/accounts-view.mjs';

const refusal = (status, code, extra = {}) => Object.assign(new Error(code), { status, payload: { error: 'fixed sentence', code, ...extra }, headers: { get: () => null } });

/** A fake server: `routes` maps "METHOD path" to an answer, a function of the body, or a list used in order. */
function harness({ routes = {}, data = null, storage = null, networkChanged } = {}) {
  const sent = [], toasts = [], timers = [];
  let dashboard = data || { schemaVersion: 1, accounts: [], settings: { hiddenProviders: [], hiddenAccountIds: [], visibilityAvailable: true }, providers: [] };
  let strengthView = null, opened = null, copied = null, refreshes = 0;
  const answer = async (req) => {
    sent.push(req);
    const key = `${req.method} ${req.path}`;
    let route = routes[key];
    if (Array.isArray(route)) route = route.length > 1 ? route.shift() : route[0];
    if (route === undefined) throw refusal(404, 'not_found');
    // a route may answer later (a Promise), to hold a request in flight
    const value = await (typeof route === 'function' ? route(req.body) : route);
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
    ...(networkChanged ? { networkChanged } : {}),
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
const flush = () => new Promise(resolve => setImmediate(resolve));

test('Show on dashboard and Show in tray save on the server and update the page without a reload', async () => {
  // the server answers with every saved list; a body may update one list and leave the others (tray route)
  const saved = { hiddenProviders: ['zai'], hiddenAccountIds: ['codex:x'], trayHiddenProviders: [] };
  const h = harness({ routes: { 'PUT /api/accounts/visibility': body => Object.assign(saved, body) && { ...saved } },
    data: { schemaVersion: 1, accounts: [{ id: 'kimi-code:usage', provider: 'kimi-code' }], providers: [{ id: 'kimi-code', visible: true }], settings: { hiddenProviders: ['zai'], hiddenAccountIds: ['codex:x'], trayHiddenProviders: [] } } });
  assert.equal(await h.ctl.handle('accounts-show', 'kimi-code:hide'), true);
  // only hiddenProviders is sent: hidden account ids another browser saved are never overwritten
  assert.deepEqual(h.sent[0], { method: 'PUT', path: '/api/accounts/visibility', body: { hiddenProviders: ['zai', 'kimi-code'] } });
  assert.deepEqual(h.dashboard.settings.hiddenAccountIds, ['codex:x']);
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

test("one account's Show on dashboard and Show in tray are independent and reach all four combinations", async () => {
  const saved = { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [], trayHiddenAccountIds: [] };
  const id = 'codex:party';
  const h = harness({
    routes: { 'PUT /api/accounts/visibility': body => ({ ...Object.assign(saved, body) }) },
    data: {
      schemaVersion: 1,
      accounts: [{ id, provider: 'codex', email: 'party@example.test', hidden: false, trayHidden: false }, { id: 'codex:other', provider: 'codex', hidden: false, trayHidden: false }],
      providers: [{ id: 'codex', visible: true, trayVisible: true }],
      settings: { ...saved },
    },
  });
  const flags = () => { const a = h.dashboard.accounts.find(row => row.id === id); return [a.hidden, a.trayHidden]; };
  // each switch sends only its own list, so the other choice is never part of the request
  assert.equal(await h.ctl.handle('account-show', `${id}|hide`), true);
  assert.deepEqual(last(h.sent), { method: 'PUT', path: '/api/accounts/visibility', body: { hiddenAccountIds: [id] } });
  assert.deepEqual(flags(), [true, false]);
  assert.equal(last(h.toasts).title, 'party@example.test hidden from the dashboard');
  await h.ctl.handle('account-tray', `${id}|hide`);
  assert.deepEqual(last(h.sent).body, { trayHiddenAccountIds: [id] });
  assert.deepEqual(flags(), [true, true]);
  assert.equal(last(h.toasts).title, 'party@example.test hidden from the trays');
  await h.ctl.handle('account-show', `${id}|show`);
  assert.deepEqual(last(h.sent).body, { hiddenAccountIds: [] });
  assert.deepEqual(flags(), [false, true]);
  await h.ctl.handle('account-tray', `${id}|show`);
  assert.deepEqual(last(h.sent).body, { trayHiddenAccountIds: [] });
  assert.deepEqual(flags(), [false, false]);
  // the other account and both provider switches were never touched
  const other = h.dashboard.accounts.find(row => row.id === 'codex:other');
  assert.deepEqual([other.hidden, other.trayHidden], [false, false]);
  assert.deepEqual([h.dashboard.providers[0].visible, h.dashboard.providers[0].trayVisible], [true, true]);
  assert.deepEqual(h.sent.map(r => Object.keys(r.body)), [['hiddenAccountIds'], ['trayHiddenAccountIds'], ['hiddenAccountIds'], ['trayHiddenAccountIds']]);
  // a malformed id or mode sends nothing; a server without the tray list has no tray switch to save
  await h.ctl.handle('account-show', 'not an id|hide');
  await h.ctl.handle('account-tray', `${id}|maybe`);
  assert.equal(h.sent.length, 4);
  const old = harness({ data: { schemaVersion: 1, accounts: [{ id, provider: 'codex' }], providers: [], settings: { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] } } });
  await old.ctl.handle('account-tray', `${id}|hide`);
  assert.equal(old.sent.length, 0);
  // both are changes: a pending account switch holds them
  assert.equal(MUTATING_ACTIONS.has('account-show'), true);
  assert.equal(MUTATING_ACTIONS.has('account-tray'), true);
});

test('visibility saves run one at a time, each from the lists the save before it left; the toggles wait for them', async () => {
  const saved = { hiddenProviders: [], hiddenAccountIds: ['codex:x'], trayHiddenProviders: [] };
  const gates = [];
  const h = harness({
    routes: {
      'PUT /api/accounts/visibility': body => new Promise(resolve => gates.push(() => resolve({ ...Object.assign(saved, body) }))),
      'POST /api/accounts/zai%3Ausage/recheck': { account: { id: 'zai:usage', status: 'ok' } },
    },
    data: { schemaVersion: 1, accounts: [{ id: 'zai:usage', provider: 'zai' }], providers: [{ id: 'zai', visible: true }, { id: 'kimi-code', visible: true }], settings: { ...saved } },
  });
  // two quick toggles: one save is sent, the other waits for it
  const first = h.ctl.handle('accounts-show', 'zai:hide');
  const second = h.ctl.handle('accounts-show', 'kimi-code:hide');
  await flush();
  assert.equal(h.sent.length, 1);
  assert.deepEqual(h.sent[0].body, { hiddenProviders: ['zai'] });
  assert.deepEqual(h.ctl.state.visPending, ['show:zai', 'show:kimi-code']);
  // another action ending meanwhile clears its own busy slot, not the visibility saves
  await h.ctl.handle('recheck', 'zai:usage');
  assert.equal(h.ctl.state.busyAct, '');
  assert.deepEqual(h.ctl.state.visPending, ['show:zai', 'show:kimi-code']);
  gates.shift()();
  await first;
  await flush();
  // the second save starts from what the first one saved, so neither choice is lost
  assert.equal(h.sent.filter(r => r.method === 'PUT').length, 2);
  assert.deepEqual(last(h.sent.filter(r => r.method === 'PUT')).body, { hiddenProviders: ['zai', 'kimi-code'] });
  assert.deepEqual(h.ctl.state.visPending, ['show:kimi-code']);
  gates.shift()();
  await second;
  assert.deepEqual(h.ctl.state.visPending, []);
  assert.deepEqual(h.dashboard.settings.hiddenProviders, ['zai', 'kimi-code']);
  assert.deepEqual(h.dashboard.settings.hiddenAccountIds, ['codex:x']);
  // a failed save releases its toggle too, and the next one still runs
  const bad = harness({ routes: { 'PUT /api/accounts/visibility': [refusal(500, 'visibility_write_failed'), body => ({ ...body, hiddenAccountIds: [], trayHiddenProviders: [] })] } });
  await Promise.all([bad.ctl.handle('accounts-show', 'zai:hide'), bad.ctl.handle('accounts-show', 'qwen:hide')]);
  assert.deepEqual(bad.ctl.state.visPending, []);
  assert.deepEqual(bad.sent.map(r => r.body), [{ hiddenProviders: ['zai'] }, { hiddenProviders: ['qwen'] }]);
});

test('a browser-only "Show on dashboard" choice moves to the server once, then the browser copy is cleared', async () => {
  const storage = memoryStorage({ [LEGACY_HIDDEN_KEY]: JSON.stringify(['kimi-code', 'nope']) });
  const h = harness({ storage, routes: { 'PUT /api/accounts/visibility': body => body } });
  assert.equal(await h.ctl.migrateLocalHidden(), true);
  assert.deepEqual(h.sent[0].body, { hiddenProviders: ['kimi-code'] });
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

test('a sign-in still running after a reload, or started in another browser, opens again from the registry', async () => {
  const job = { id: 'job_cccccccccccccccc', provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'codex-3', platform: 'ubuntu', state: 'waiting', verification: { url: 'https://auth.openai.com/codex/device', userCode: 'WXYZ-98765', expiresAt: null }, result: null, error: null };
  const again = { ...job, id: 'job_dddddddddddddddd', provider: 'muse', mode: 'signin-again', accountId: 'muse:usage', profileName: null, platform: 'mac' };
  const registry = { providers: [], accounts: [], jobs: [job, again, { ...job, id: 'job_eeeeeeeeeeeeeeee', provider: 'nope' }], trash: [] };
  const h = harness({ routes: {
    'GET /api/accounts/registry': registry,
    [`GET /api/accounts/signin-jobs/${job.id}`]: job,
    [`POST /api/accounts/signin-jobs/${job.id}/cancel`]: { ...job, state: 'cancelled' },
  }, data: { accounts: [{ id: 'muse:usage', provider: 'muse', email: 'muse@example.test' }], settings: {} } });
  await h.ctl.loadRegistry();
  const flow = h.ctl.state.flows.codex;
  assert.deepEqual([flow.type, flow.step, flow.name, flow.job.id], ['job-add', 'job', 'codex-3', job.id]);
  // the code and Cancel are back on the page, and the job is polled again
  const view = flowView('codex', flow);
  assert.equal(view.codeText, 'WXYZ-98765');
  assert.ok(view.actions.some(a => a.act === 'flow-cancel'));
  assert.ok(h.timers.some(t => t.ms === JOB_POLL_MS));
  // a Sign in again job opens as one, named by its account; a provider the page does not know is left alone
  const muse = h.ctl.state.flows.muse;
  assert.deepEqual([muse.type, muse.accountId, muse.email, muse.job.id], ['job-again', 'muse:usage', 'muse@example.test', again.id]);
  assert.equal(h.ctl.state.flows.nope, undefined);
  // a second read keeps the open flow as it is
  const serial = h.ctl.state.flows.codex.serial;
  await h.ctl.loadRegistry();
  assert.equal(h.ctl.state.flows.codex.serial, serial);
  // Cancel stops it on the server; a registry read that still lists it never opens it again
  await h.ctl.handle('flow-cancel', 'codex');
  assert.equal(h.ctl.state.flows.codex, undefined);
  await h.ctl.loadRegistry();
  assert.equal(h.ctl.state.flows.codex, undefined);
  // an open flow of the same provider is never replaced by a registry read
  const naming = harness({ routes: { 'GET /api/accounts/registry': registry } });
  await naming.ctl.handle('add', 'codex');
  await naming.ctl.loadRegistry();
  assert.equal(naming.ctl.state.flows.codex.step, 'name');
});

test('Add or Sign in again while a sign-in runs shows the running one instead of a refusal that points at nothing', async () => {
  const job = { id: 'job_ffffffffffffffff', provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'codex-3', platform: 'ubuntu', state: 'waiting', verification: { url: 'https://auth.openai.com/codex/device', userCode: 'WXYZ-98765', expiresAt: null }, result: null, error: null };
  const h = harness({ routes: {
    'POST /api/accounts/add': refusal(409, 'job_running', { jobId: job.id }),
    [`GET /api/accounts/signin-jobs/${job.id}`]: job,
    'POST /api/accounts/codex%3Aone/signin-again': refusal(409, 'job_running', { jobId: job.id }),
  }, data: { accounts: [{ id: 'codex:one', provider: 'codex', capabilities: { codexProfile: 'one' } }], settings: {} } });
  await h.ctl.handle('add', 'codex');
  await h.ctl.handle('flow-submit', 'codex\ncodex-9\n');
  assert.deepEqual(h.sent.map(r => `${r.method} ${r.path}`), ['POST /api/accounts/add', `GET /api/accounts/signin-jobs/${job.id}`]);
  const flow = h.ctl.state.flows.codex;
  assert.deepEqual([flow.type, flow.step, flow.job.id, flow.name, flow.error], ['job-add', 'job', job.id, 'codex-3', null]);
  assert.equal(flowView('codex', flow).codeText, 'WXYZ-98765');
  assert.equal(last(h.toasts).title, 'A sign-in was already running');
  // from a row's Sign in again too
  await h.ctl.handle('flow-cancel', 'codex');
  await h.ctl.handle('signin-again', 'codex:one');
  assert.equal(h.ctl.state.flows.codex.job.id, job.id);
  // a job that ended meanwhile: the refusal in plain words, nothing opened
  const ended = harness({ routes: { 'POST /api/accounts/add': refusal(409, 'job_running', { jobId: job.id }), [`GET /api/accounts/signin-jobs/${job.id}`]: { ...job, state: 'cancelled' } } });
  await ended.ctl.handle('add', 'codex');
  await ended.ctl.handle('flow-submit', 'codex\ncodex-9\n');
  assert.equal(ended.ctl.state.flows.codex.step, 'name');
  assert.match(ended.ctl.state.flows.codex.error.title, /already running/);
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

test('Antigravity Sign in shows the terminal command the server answers', async () => {
  const h = harness({ routes: {
    'POST /api/accounts/antigravity%3Aprofile%3Aparty/signin-again': refusal(409, 'preflight_failed', { fallback: { kind: 'terminal', host: 'ubuntu', command: 'ai-account-center antigravity signin party' } }),
    'GET /api/accounts/registry': { accounts: [] },
  }, data: { accounts: [{ id: 'antigravity:profile:party', provider: 'antigravity' }], settings: {} } });
  await h.ctl.handle('signin-again', 'antigravity:profile:party');
  assert.equal(last(h.toasts).title, 'Sign in from a terminal');
  assert.match(last(h.toasts).body, /ai-account-center antigravity signin party/);
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

test('Delete now from the Claude trash: typed DELETE, mistype keeps the token, then gone for good', async () => {
  const token = 'P'.repeat(43);
  const h = harness({ routes: {
    'POST /api/accounts/trash/tr_0123456789abcdef/purge': body => body.confirmationToken
      ? (body.confirm === 'DELETE' ? { purged: true, trashId: 'tr_0123456789abcdef' } : refusal(400, 'invalid_body'))
      : { confirmation: { token, effects: ['Its Claude data is deleted for good on Mac and Windows.'], expectsTyped: 'DELETE' } },
    'GET /api/accounts/registry': { accounts: [], trash: [] },
  } });
  h.ctl.state.registry = { accounts: [], trash: [{ trashId: 'tr_0123456789abcdef', provider: 'claude', label: 'party@example.test' }] };
  await h.ctl.handle('purge', 'tr_0123456789abcdef');
  assert.equal(h.ctl.state.flows.claude.type, 'purge');
  assert.equal(h.ctl.state.flows.claude.step, 'type');
  await h.ctl.handle('flow-submit', 'claude\ndelete\n');
  assert.equal(h.ctl.state.flows.claude.error.title, 'Type DELETE exactly as shown.');
  assert.equal(h.ctl.state.flows.claude.step, 'type');
  await h.ctl.handle('flow-submit', 'claude\nDELETE\n');
  assert.deepEqual(h.sent[2].body, { confirmationToken: token, confirm: 'DELETE' });
  assert.equal(h.ctl.state.flows.claude.step, 'done');
  assert.equal(last(h.toasts).title, 'Deleted party@example.test for good');
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

test('Muse Sign in opens a device-code job on the Mac once the server offers it', async () => {
  const job = { id: 'job_muse1', provider: 'muse', kind: 'device-code', mode: 'signin-again', accountId: 'muse:usage', platform: 'mac', state: 'starting', verification: null };
  const h = harness({ routes: {
    'POST /api/accounts/muse%3Ausage/signin-again': { status: 202, payload: { job } },
    'GET /api/accounts/registry': { accounts: [] },
  }, data: { accounts: [{ id: 'muse:usage', provider: 'muse', email: 'muse-user@example.test' }], settings: {} } });
  await h.ctl.handle('session-signin', 'muse');
  assert.equal(h.ctl.state.flows.muse.type, 'job-again');
  assert.equal(h.ctl.state.flows.muse.job.id, 'job_muse1');
});

test('the Sign-in & connection block: other browsers, network trust, devices and sign out all devices', async () => {
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

test('Turn off hands the saved trust to the bridge, so the sign-in page note follows it', async () => {
  const seen = [];
  const network = { trustLocalNetwork: true, trustedNetworks: [], connection: { peer: '192.168.50.20', trusted: true }, canTurnOn: false };
  const h = harness({ networkChanged: async view => { seen.push(view); }, routes: {
    'PUT /api/auth/network': body => ({ ...network, trustLocalNetwork: body.trustLocalNetwork, connection: { ...network.connection, trusted: body.trustLocalNetwork } }),
    'GET /api/auth/session': {}, 'GET /api/auth/devices': { devices: [] }, 'GET /api/auth/network': { ...network, trustLocalNetwork: false, connection: { ...network.connection, trusted: false } },
  } });
  await h.ctl.handle('network-off', '');
  assert.deepEqual(seen.map(v => [v.trustLocalNetwork, v.connection.trusted]), [[false, false]]);
  // a refused change hands nothing over
  const refused = harness({ networkChanged: async view => { seen.push(view); }, routes: { 'PUT /api/auth/network': refusal(403, 'loopback_required') } });
  await refused.ctl.handle('network-on', '');
  assert.equal(seen.length, 1);
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

test('the actions that send a change are named, so a pending account switch can hold them', async () => {
  const h = harness();
  for (const action of MUTATING_ACTIONS) assert.equal(await h.ctl.handle(action, ''), true, `${action} is this controller's`);
  // opening or closing a flow, typing, copying and cancelling a sign-in stay live and send no change
  const local = ['add', 'add-key', 'replace-key', 'refuse', 'line-cancel', 'pw-toggle', 'pw-typing', 'flow-copy', 'flow-open-url', 'flow-cancel', 'flow-done'];
  for (const action of local) assert.equal(MUTATING_ACTIONS.has(action), false, action);
  const quiet = harness();
  await quiet.ctl.handle('add', 'codex');
  await quiet.ctl.handle('add', 'zai');
  await quiet.ctl.handle('replace-key', 'zai:acct:1');
  await quiet.ctl.handle('pw-toggle', '');
  await quiet.ctl.handle('pw-typing', 'a\nb');
  await quiet.ctl.handle('refuse', 'codex:one\nlast_account');
  await quiet.ctl.handle('line-cancel', 'codex:one');
  await quiet.ctl.handle('flow-cancel', 'codex');
  assert.deepEqual(quiet.sent, []);
});

test('the session lifetime saves on the server and the page follows it without a reload', async () => {
  const session = { username: 'owner', sessionTimeoutHours: 720, sessionLifetimeDays: 30, otherBrowsers: 0 };
  const h = harness({
    routes: {
      'GET /api/auth/session': { ...session },
      'GET /api/auth/devices': { devices: [] },
      'GET /api/auth/network': { trustLocalNetwork: false, connection: {} },
      'PUT /api/auth/session-lifetime': body => ({ days: body.days, hours: body.days * 24, options: [1, 7, 30, 90, 365] }),
    },
  });
  await h.ctl.loadSignin();
  await h.ctl.handle('session-lifetime', '7');
  assert.deepEqual(last(h.sent), { method: 'PUT', path: '/api/auth/session-lifetime', body: { days: 7 } });
  assert.equal(h.ctl.state.signin.session.sessionLifetimeDays, 7);
  assert.equal(h.ctl.state.signin.session.sessionTimeoutHours, 168);
  assert.equal(last(h.toasts).title, 'Sessions now last 7 days');
  // picking the saved value again sends nothing
  const count = h.sent.length;
  await h.ctl.handle('session-lifetime', '7');
  assert.equal(h.sent.length, count);
  // a refused save says so and keeps the old value
  const bad = harness({
    routes: {
      'GET /api/auth/session': { ...session },
      'GET /api/auth/devices': { devices: [] },
      'GET /api/auth/network': { trustLocalNetwork: false, connection: {} },
      'PUT /api/auth/session-lifetime': refusal(400, 'invalid_body'),
    },
  });
  await bad.ctl.loadSignin();
  await bad.ctl.handle('session-lifetime', '90');
  assert.equal(bad.ctl.state.signin.session.sessionLifetimeDays, 30);
  assert.equal(last(bad.toasts).kind, 'err');
});

test('the time zone saves the whole preferences shape and the page follows it', async () => {
  const prefs = { timeZone: 'America/New_York', snapshotCleanup: { auto: true }, usageLogSources: [] };
  const h = harness({
    routes: {
      'GET /api/accounts/preferences': { ...prefs },
      'PUT /api/accounts/preferences': (body) => ({ ...body }),
    },
  });
  await h.ctl.loadPrefs();
  assert.equal(h.ctl.state.prefs.data.timeZone, 'America/New_York');
  await h.ctl.handle('time-zone', 'Asia/Tokyo');
  assert.deepEqual(last(h.sent), { method: 'PUT', path: '/api/accounts/preferences', body: { ...prefs, timeZone: 'Asia/Tokyo' } });
  assert.equal(h.ctl.state.prefs.data.timeZone, 'Asia/Tokyo');
  const count = h.sent.length;
  await h.ctl.handle('time-zone', 'Asia/Tokyo');
  assert.equal(h.sent.length, count);
  await h.ctl.handle('time-zone', '');
  assert.equal(h.sent.length, count);
});

test('removing a default Claude profile types its account email, and a mistype stays open', async () => {
  const h = harness({
    data: { accounts: [{ id: 'claude:home', provider: 'claude', label: 'home', email: 'home@example.com' }] },
    routes: {
      'POST /api/accounts/claude%3Ahome/remove': (body) => {
        if (!body.confirmationToken) return { confirmation: { token: 'tok-email', effects: ['Default.'], expectsTyped: 'email' } };
        if (body.confirm !== 'home@example.com') throw refusal(400, 'invalid_body');
        return { removed: true, trashId: 'tr_1', purgeAfter: null };
      },
    },
  });
  await h.ctl.handle('remove', 'claude:home');
  assert.equal(h.ctl.state.flows.claude.type, 'remove-email');
  assert.equal(h.ctl.state.flows.claude.accountId, 'claude:home');
  assert.equal(h.ctl.state.lines['claude:home'], undefined);
  await h.ctl.handle('flow-submit', 'claude\nwrong@example.com\n');
  assert.equal(h.ctl.state.flows.claude.error.title, 'That email does not match this account.');
  assert.deepEqual(last(h.sent), { method: 'POST', path: '/api/accounts/claude%3Ahome/remove', body: { confirmationToken: 'tok-email', confirm: 'wrong@example.com' } });
  await h.ctl.handle('flow-submit', 'claude\n\n');
  assert.equal(h.ctl.state.flows.claude.error.title, 'Type the account email first.');
  await h.ctl.handle('flow-submit', 'claude\nhome@example.com\n');
  assert.equal(h.ctl.state.flows.claude, undefined);
  assert.equal(last(h.toasts).title, 'Removed home@example.com');
});

test('snapshot cleanup toggles automatic retention and cleans up now with a note', async () => {
  const prefs = { timeZone: 'America/New_York', snapshotCleanup: { auto: true }, usageLogSources: [] };
  const h = harness({
    routes: {
      'GET /api/accounts/preferences': { ...prefs },
      'PUT /api/accounts/preferences': (body) => ({ ...body }),
      'POST /api/claude/history-snapshots/cleanup': { profiles: 2, targets: 3, kept: 9, deleted: 4, skipped: 1, failed: 0 },
    },
  });
  await h.ctl.loadPrefs();
  await h.ctl.handle('cleanup-auto', '');
  assert.deepEqual(last(h.sent), { method: 'PUT', path: '/api/accounts/preferences', body: { ...prefs, snapshotCleanup: { auto: false } } });
  await h.ctl.handle('cleanup-now', '');
  assert.deepEqual(last(h.sent), { method: 'POST', path: '/api/claude/history-snapshots/cleanup', body: {} });
  assert.equal(h.ctl.state.prefs.cleanupNote, 'Deleted 4 older snapshots, skipped 1 folder that was not AAC snapshots.');
  const idle = harness({
    routes: {
      'GET /api/accounts/preferences': { ...prefs },
      'POST /api/claude/history-snapshots/cleanup': { profiles: 1, targets: 1, kept: 2, deleted: 0, skipped: 0, failed: 0 },
    },
  });
  await idle.ctl.loadPrefs();
  await idle.ctl.handle('cleanup-now', '');
  assert.equal(idle.ctl.state.prefs.cleanupNote, 'Nothing to clean: every profile already keeps only its newest snapshots.');
});

test('extra usage-log locations add, validate and remove through one preferences save', async () => {
  const prefs = { timeZone: 'America/New_York', snapshotCleanup: { auto: true }, usageLogSources: [] };
  const h = harness({
    routes: {
      'GET /api/accounts/preferences': { ...prefs },
      'PUT /api/accounts/preferences': (body) => ({ ...body }),
    },
  });
  await h.ctl.loadPrefs();
  await h.ctl.handle('logsource-add', 'omp\nubuntu\n/home/u/extra-omp\n');
  assert.equal(h.ctl.state.prefs.logSourcesError ?? '', '');
  let body = last(h.sent).body;
  assert.equal(body.usageLogSources.length, 1);
  assert.match(body.usageLogSources[0].id, /^log-[a-z0-9]{8}$/);
  assert.deepEqual({ ...body.usageLogSources[0], id: 'x' }, { id: 'x', tool: 'omp', host: 'ubuntu', path: '/home/u/extra-omp' });
  const count = h.sent.length;
  // a relative path, a wrong host and a missing mapping refuse without saving
  await h.ctl.handle('logsource-add', 'omp\nubuntu\nrelative/path\n');
  assert.equal(h.sent.length, count);
  assert.match(h.ctl.state.prefs.logSourcesError, /absolute path/);
  await h.ctl.handle('logsource-add', 'muse\nwindows\nC:\\muse\n');
  assert.equal(h.sent.length, count);
  assert.match(h.ctl.state.prefs.logSourcesError, /only scanned on ubuntu, mac/);
  await h.ctl.handle('logsource-add', 'jsonl\nubuntu\n/var/log/h\n');
  assert.equal(h.sent.length, count);
  assert.match(h.ctl.state.prefs.logSourcesError, /field mapping/);
  await h.ctl.handle('logsource-add', 'jsonl\nubuntu\n/var/log/h\n{"timestamp":"ts"}');
  assert.equal(h.sent.length, count);
  assert.match(h.ctl.state.prefs.logSourcesError, /model/);
  // a mapped generic source saves with its mapping
  await h.ctl.handle('logsource-add', 'jsonl\nubuntu\n/var/log/h\n{"timestamp":"ts","model":"m","inputTokens":"usage.in"}');
  body = last(h.sent).body;
  assert.equal(body.usageLogSources.length, 2);
  assert.deepEqual(body.usageLogSources[1].fieldMapping, { timestamp: 'ts', model: 'm', inputTokens: 'usage.in' });
  // adding the same location again refuses as a duplicate
  const beforeDupe = h.sent.length;
  await h.ctl.handle('logsource-add', 'omp\nubuntu\n/home/u/extra-omp\n');
  assert.equal(h.sent.length, beforeDupe);
  assert.match(h.ctl.state.prefs.logSourcesError, /already listed/);
  // remove drops one entry; an unknown id sends nothing
  const [first, second] = h.ctl.state.prefs.data.usageLogSources;
  await h.ctl.handle('logsource-remove', first.id);
  body = last(h.sent).body;
  assert.deepEqual(body.usageLogSources.map((entry) => entry.id), [second.id]);
  const afterRemove = h.sent.length;
  await h.ctl.handle('logsource-remove', 'log-unknown');
  assert.equal(h.sent.length, afterRemove);
});

test('the log source check mirrors the server path and mapping rules', async () => {
  const { logSourceProblem } = await import('../public/accounts-controller.mjs');
  assert.equal(logSourceProblem('omp', 'mac', '/Users/u/x', undefined), '');
  assert.equal(logSourceProblem('omp', 'windows', 'C:\\x\\y', undefined), '');
  assert.equal(logSourceProblem('omp', 'windows', 'C:/x/y', undefined), '');
  assert.ok(logSourceProblem('cursor', 'mac', '/x', undefined));
  assert.ok(logSourceProblem('omp', 'mac', 'relative', undefined));
  assert.ok(logSourceProblem('omp', 'mac', '/x/../y', undefined));
  assert.ok(logSourceProblem('omp', 'ubuntu', 'C:\\x', undefined));
  assert.ok(logSourceProblem('zcode', 'windows', '/x.db', undefined));
  assert.ok(logSourceProblem('omp', 'mac', '/x', { timestamp: 'ts' }));
  assert.equal(logSourceProblem('jsonl', 'ubuntu', '/var/log/h', { timestamp: 'ts', model: 'm' }), '');
  assert.ok(logSourceProblem('jsonl', 'ubuntu', '/var/log/h', { timestamp: 'ts', model: '0bad' }));
  assert.ok(logSourceProblem('jsonl', 'ubuntu', '/var/log/h', { timestamp: 'ts', model: 'm', bogus: 'x' }));
});
