// The Accounts & Settings controller: the state behind every sign-in and sign-out control (the inline flows,
// the lines under rows, the trash, show and hide, the password change, other browsers, paired trays and the
// trusted local network) and what each action sends. All I/O is injected, so tests/accounts-controller.test.mjs
// drives every action and every error code with a fake server; bridge.js wires it to fetch, toasts and Slint.
//
// deps:
//   call(request) -> { status, payload }       one request (account-actions.mjs `requests`); a refusal throws an
//                                               Error with { status, payload, headers }
//   changed()                                  the state changed: draw Accounts & Settings again
//   toast(kind, title, body, ms)               a result or a failure
//   refresh()                                  re-read the dashboard (after any saved change)
//   data() -> the dashboard response            (settings, accounts, providers) as last received
//   setData(next)                               replace the dashboard response (visibility saved)
//   strength(view)                              the change-password strength hint (one keystroke)
//   copy(text) -> Promise<boolean>, open(url)   the code and the verification page
//   schedule(fn, ms) / cancel(id), now()        timers and the clock (fake in tests)
//   storage                                     localStorage, for the one-time migration of "Show on dashboard"
//   networkChanged(view) -> Promise              local network trust was saved: the bridge's copy of
//                                               GET /api/auth/check follows it (the sign-in page's note)
import { requests, errorText, jobFinished, profileNameProblem, claudeIdProblem, keyProblem, suggestName, passwordProblem, passwordChangedToast, revokeAllToast, PROVIDER_LABELS } from './account-actions.mjs';
import { setDisplayTimeZone, DEFAULT_DISPLAY_TIME_ZONE } from './time-format.mjs';
import { strength as passwordStrength } from './auth-view.mjs';
import { statusWord } from './view-model.mjs';
import { lazyFormat } from './time-format.mjs';
import { LOG_SOURCE_HOSTS, LOG_SOURCE_TOOL_LABEL, LOG_SOURCE_PATH_HINT } from './accounts-view.mjs';

export const JOB_POLL_MS = 2_000;
export const LEGACY_HIDDEN_KEY = 'aac-hidden-providers';
const PROVIDERS = Object.keys(PROVIDER_LABELS);
const KEY_PROVIDERS = ['kimi-code', 'zai', 'opencode-go'];
const text = value => typeof value === 'string' ? value : '';
const label = provider => PROVIDER_LABELS[provider] || 'This provider';
const dayFmt = lazyFormat({ month: 'short', day: 'numeric' });
const REFUSALS = new Set(['account_active', 'account_default', 'account_protected', 'last_account', 'activation_running', 'signin_running', 'app_running', 'app_state_unknown']);
/**
 * The actions that send a change. While an account-switch confirmation is pending, bridge.js holds these (the
 * page did before this controller existed); the local ones (open or close a flow, copy the code, type, cancel a
 * sign-in) stay live.
 */
export const MUTATING_ACTIONS = Object.freeze(new Set([
  'accounts-show', 'accounts-tray', 'account-show', 'account-tray', 'signin-again', 'signin', 'session-signin', 'recheck', 'flow-submit', 'flow-retry',
  'flow-open-app', 'flow-recheck', 'remove', 'remove-commit', 'restore', 'restore-commit', 'purge', 'others-out', 'network-off',
  'network-on', 'pw-submit', 'device-revoke', 'devices-revoke-all', 'session-lifetime', 'time-zone',
  'cleanup-auto', 'cleanup-now', 'logsource-add', 'logsource-remove',
]));

const LOG_SOURCE_FIELD_PATH = /^[A-Za-z][A-Za-z0-9]*(?:\.[A-Za-z][A-Za-z0-9]*)*$/;
const LOG_SOURCE_MAPPING_KEYS = ['timestamp', 'model', 'inputTokens', 'outputTokens', 'cost'];

/**
 * Why an extra usage-log location is not addable ('' when it is). Mirrors the server's
 * `isUsageLogSource` so the form refuses what PUT /api/accounts/preferences would refuse.
 */
export function logSourceProblem(tool, host, location, mapping) {
  const hosts = LOG_SOURCE_HOSTS[tool];
  if (!hosts) return 'Pick a tool for the extra location.';
  if (!hosts.includes(host)) {
    const names = hosts.join(', ');
    return `${LOG_SOURCE_TOOL_LABEL[tool]} usage is only scanned on ${names}.`;
  }
  const hint = LOG_SOURCE_PATH_HINT[tool];
  if (typeof location !== 'string' || !location || location.length > 1024 || location.includes('\0'))
    return `Enter the absolute path to ${hint}.`;
  const absolute = location.startsWith('/') || (host === 'windows' && /^[A-Za-z]:[\\/]/.test(location));
  if (!absolute || location.includes('..')) return `Enter the absolute path to ${hint}, with no "..".`;
  if (tool === 'jsonl') {
    if (!mapping || typeof mapping !== 'object' || Array.isArray(mapping))
      return 'A generic JSONL source needs its row field mapping: timestamp and model at least.';
    for (const key of Object.keys(mapping)) {
      if (!LOG_SOURCE_MAPPING_KEYS.includes(key)) return `The mapping has no "${key}" field.`;
      if (typeof mapping[key] !== 'string' || !LOG_SOURCE_FIELD_PATH.test(mapping[key]))
        return `The "${key}" mapping is not a dot path (like usage.input_tokens).`;
    }
    if (typeof mapping.timestamp !== 'string') return 'The mapping needs a timestamp field.';
    if (typeof mapping.model !== 'string') return 'The mapping needs a model field.';
  } else if (mapping !== undefined && mapping !== null) {
    return 'Only generic JSONL sources take a field mapping.';
  }
  return '';
}

export function createAccountsController(deps) {
  const {
    call, changed = () => {}, toast = () => {}, refresh = async () => {}, data = () => null, setData = () => {},
    strength = () => {}, copy = async () => false, open = () => {}, schedule = setTimeout, cancel = clearTimeout,
    now = Date.now, storage = null, networkChanged = async () => {},
  } = deps;
  const state = {
    registry: null,
    flows: {},
    lines: {},
    busyAct: '',
    // "Show on dashboard" and "Show in tray" saves queued or in flight: 'show:<provider>' and 'tray:<provider>',
    // and one account's own switches: 'acct-show:<id>' and 'acct-tray:<id>'
    visPending: [],
    signin: { session: null, devices: null, devicesError: false, network: null, pw: blankPassword(), busy: '' },
    prefs: { data: null, busy: '', cleanupBusy: false, cleanupNote: '' },
  };
  const timers = {};
  // sign-in jobs this page closed (Cancel, Done, Close): a registry read never opens them again
  const dismissedJobs = new Set();
  // visibility saves run one at a time, each from the lists the one before it saved
  let visibilityChain = Promise.resolve();
  let serial = 0;
  let migrated = false;

  function blankPassword() { return { open: false, busy: false, done: false, field: '', error: '', nonce: 0, strength: null }; }
  const fail = (error, ctx = {}, kind = 'err') => { const t = errorText(error, ctx); toast(kind, t.title, t.body, 6400); return t; };
  const accountOf = id => (data()?.accounts || []).find(account => account?.id === id)
    || (state.registry?.accounts || []).find(account => account?.id === id) || null;
  const accountsOf = provider => (data()?.accounts || []).filter(account => account?.provider === provider);
  const providerOf = id => accountOf(id)?.provider || (String(id).includes(':') ? String(id).split(':')[0] : String(id).startsWith('plan-opencode-go-console-') ? 'opencode-go' : '');
  const nameOf = id => { const a = accountOf(id); return text(a?.email) || text(a?.label) || 'this account'; };

  // ------------------------------------------------------------ loading
  async function loadRegistry() {
    try { state.registry = (await call(requests.registry())).payload || null; }
    catch (error) {
      // an older server has no registry: the page then draws only what the dashboard response says
      if (error?.status !== 404) state.registry = state.registry || null;
    }
    resumeJobs();
    changed();
  }
  /**
   * A sign-in still running on the server that this page shows no flow for (the page was reloaded, or the
   * sign-in was started in another browser) opens its flow again, so its code, its status and Cancel are
   * never lost while the job waits.
   */
  function resumeJobs() {
    for (const job of Array.isArray(state.registry?.jobs) ? state.registry.jobs : []) resumeJob(job);
  }
  /** Follows a running job in its provider's flow. `replace` lets it take over a flow that is open. */
  function resumeJob(job, replace = false) {
    const provider = text(job?.provider), id = text(job?.id);
    if (!PROVIDERS.includes(provider) || !id || jobFinished(job)) return false;
    const open = state.flows[provider];
    if (open?.job?.id === id) return true;
    if (!replace && (open || dismissedJobs.has(id))) return false;
    const again = job.mode === 'signin-again';
    const accountId = text(job.accountId) || null;
    startJob(provider, { serial: ++serial, provider, type: again ? 'job-again' : 'job-add', accountId, name: text(job.profileName), email: again ? nameOf(accountId) : '' }, job);
    return true;
  }
  /**
   * A 409 `job_running` names the sign-in that is already running for this provider or account: the page shows
   * that one (its code and Cancel) instead of a refusal that points at nothing.
   */
  async function adoptRunning(provider, error) {
    const jobId = text(error?.payload?.jobId);
    if (error?.payload?.code !== 'job_running' || !jobId) return false;
    try {
      const { payload } = await call(requests.job(jobId));
      if (payload?.provider !== provider || !resumeJob(payload, true)) return false;
    } catch { return false; }
    toast('info', 'A sign-in was already running', `It is shown under ${label(provider)}. Finish it or cancel it before starting another.`);
    return true;
  }
  async function loadSignin() {
    const s = state.signin;
    const [session, devices, network] = await Promise.allSettled([call(requests.session()), call(requests.devices()), call(requests.network())]);
    s.session = session.status === 'fulfilled' ? session.value.payload : null;
    if (devices.status === 'fulfilled') { s.devices = Array.isArray(devices.value.payload?.devices) ? devices.value.payload.devices : []; s.devicesError = false; }
    else { s.devices = null; s.devicesError = devices.reason?.status === 503; }
    s.network = network.status === 'fulfilled' ? network.value.payload : null;
    changed();
  }
  async function loadPrefs() {
    try {
      const { payload } = await call(requests.preferences());
      if (payload && typeof payload === 'object') {
        state.prefs.data = payload;
        if (typeof payload.timeZone === 'string') setDisplayTimeZone(payload.timeZone);
      }
    } catch { state.prefs.data = state.prefs.data || null; }
    changed();
  }
  async function loadAll() { await Promise.all([loadRegistry(), loadSignin(), loadPrefs()]); }
  /** After a saved change: the dashboard response, then the registry (its actions and refusals follow it). */
  async function reload() { await refresh(); await loadRegistry(); }

  // ------------------------------------------------------------ "Show on dashboard" and "Show in tray"
  function lists() {
    const settings = data()?.settings || {};
    return {
      hiddenProviders: Array.isArray(settings.hiddenProviders) ? settings.hiddenProviders : [],
      hiddenAccountIds: Array.isArray(settings.hiddenAccountIds) ? settings.hiddenAccountIds : [],
    };
  }
  function applyVisibility(saved) {
    const current = data();
    if (!current || !saved) return;
    const settings = { ...(current.settings || {}) };
    if (Array.isArray(saved.hiddenProviders)) settings.hiddenProviders = saved.hiddenProviders;
    if (Array.isArray(saved.hiddenAccountIds)) settings.hiddenAccountIds = saved.hiddenAccountIds;
    if (Array.isArray(saved.trayHiddenProviders)) settings.trayHiddenProviders = saved.trayHiddenProviders;
    if (Array.isArray(saved.trayHiddenAccountIds)) settings.trayHiddenAccountIds = saved.trayHiddenAccountIds;
    const hidden = new Set(settings.hiddenProviders || []);
    const tray = new Set(settings.trayHiddenProviders || []);
    const hiddenIds = new Set(settings.hiddenAccountIds || []);
    const trayIds = new Set(settings.trayHiddenAccountIds || []);
    const trayKnown = Array.isArray(settings.trayHiddenProviders) || Array.isArray(settings.trayHiddenAccountIds);
    setData({
      ...current, settings,
      providers: Array.isArray(current.providers) ? current.providers.map(p => ({ ...p, visible: !hidden.has(p.id), ...('trayVisible' in p || Array.isArray(settings.trayHiddenProviders) ? { trayVisible: !tray.has(p.id) } : {}) })) : current.providers,
      // `hidden` follows only the dashboard lists and `trayHidden` only the tray lists
      accounts: Array.isArray(current.accounts) ? current.accounts.map(a => ({ ...a, hidden: hidden.has(a.provider) || hiddenIds.has(a.id), ...('trayHidden' in a || trayKnown ? { trayHidden: tray.has(a.provider) || trayIds.has(a.id) } : {}) })) : current.accounts,
    });
  }
  /** Runs one visibility save after the ones before it; the chain itself never rejects. */
  function serialVisibility(task) {
    const run = visibilityChain.then(task);
    visibilityChain = run.catch(() => {});
    return run;
  }
  /**
   * One toggle's save. Its key stays in `visPending` (the toggles wait) until it is saved; another action ending
   * meanwhile does not release them. Each save reads the lists the save before it left, and sends only its own.
   */
  function queueVisibility(key, task) {
    state.visPending = [...state.visPending, key]; changed();
    return serialVisibility(task).catch(() => {}).finally(() => {
      const at = state.visPending.indexOf(key);
      if (at >= 0) state.visPending = [...state.visPending.slice(0, at), ...state.visPending.slice(at + 1)];
      changed();
    });
  }
  function setShown(provider, show) {
    if (!PROVIDERS.includes(provider)) return;
    return queueVisibility(`show:${provider}`, async () => {
      const next = new Set(lists().hiddenProviders);
      if (show) next.delete(provider); else next.add(provider);
      try {
        const { payload } = await call(requests.visibility([...next]));
        applyVisibility(payload);
        toast('info', show ? `${label(provider)} shown on the dashboard` : `${label(provider)} hidden from the dashboard`, 'Saved on the dashboard: every browser follows this choice.');
      } catch (error) { fail(error, { provider }); }
    });
  }
  function setTrayShown(provider, show) {
    if (!PROVIDERS.includes(provider) || !Array.isArray(data()?.settings?.trayHiddenProviders)) return;
    return queueVisibility(`tray:${provider}`, async () => {
      const current = data()?.settings?.trayHiddenProviders;
      if (!Array.isArray(current)) return;
      const next = new Set(current);
      if (show) next.delete(provider); else next.add(provider);
      try {
        const { payload } = await call(requests.trayVisibility([...next]));
        applyVisibility(payload);
        toast('info', show ? `${label(provider)} shown in the trays` : `${label(provider)} hidden from the trays`, 'Saved on the dashboard: the Mac and Windows trays follow on their next refresh.');
      } catch (error) { fail(error, { provider }); }
    });
  }
  /** An account id the server accepts in a visibility list (account-visibility.ts). */
  const ACCOUNT_ID = /^(?:[a-z0-9][a-z0-9-]{0,31}(?::[A-Za-z0-9@._+-]{1,128}){1,2}|plan-opencode-go-console-mac-[a-f0-9]{12})$/;
  /** One account's "Show on dashboard": only `hiddenAccountIds` is sent, so its tray switch is never touched. */
  function setAccountShown(id, show) {
    const settings = data()?.settings;
    if (!ACCOUNT_ID.test(id) || !Array.isArray(settings?.hiddenAccountIds)) return;
    return queueVisibility(`acct-show:${id}`, async () => {
      const current = data()?.settings?.hiddenAccountIds;
      if (!Array.isArray(current)) return;
      const next = new Set(current);
      if (show) next.delete(id); else next.add(id);
      try {
        const { payload } = await call(requests.accountVisibility([...next]));
        applyVisibility(payload);
        toast('info', show ? `${nameOf(id)} shown on the dashboard` : `${nameOf(id)} hidden from the dashboard`, 'Saved on the dashboard: every browser follows this choice. The trays keep their own switch.');
      } catch (error) { fail(error, { provider: providerOf(id) }); }
    });
  }
  /** One account's "Show in tray": only `trayHiddenAccountIds` is sent, so its dashboard switch is never touched. */
  function setAccountTrayShown(id, show) {
    if (!ACCOUNT_ID.test(id) || !Array.isArray(data()?.settings?.trayHiddenAccountIds)) return;
    return queueVisibility(`acct-tray:${id}`, async () => {
      const current = data()?.settings?.trayHiddenAccountIds;
      if (!Array.isArray(current)) return;
      const next = new Set(current);
      if (show) next.delete(id); else next.add(id);
      try {
        const { payload } = await call(requests.trayAccountVisibility([...next]));
        applyVisibility(payload);
        toast('info', show ? `${nameOf(id)} shown in the trays` : `${nameOf(id)} hidden from the trays`, 'Saved on the dashboard: the Mac and Windows trays follow on their next refresh. The dashboard keeps its own switch.');
      } catch (error) { fail(error, { provider: providerOf(id) }); }
    });
  }
  /**
   * Once: a "Show on dashboard" choice an older build saved in this browser moves to the server, then the
   * browser's copy is cleared. Nothing is sent when there is nothing to move or the server cannot store it.
   */
  async function migrateLocalHidden() {
    if (migrated || !storage) return false;
    let local = [];
    try { const parsed = JSON.parse(storage.getItem(LEGACY_HIDDEN_KEY) || '[]'); local = Array.isArray(parsed) ? parsed.filter(id => PROVIDERS.includes(id)) : []; }
    catch { local = []; }
    const settings = data()?.settings;
    if (!settings || settings.visibilityAvailable === false) return false;
    migrated = true;
    if (!local.length) { try { storage.removeItem(LEGACY_HIDDEN_KEY); } catch {} return false; }
    try {
      await serialVisibility(async () => {
        const { hiddenProviders } = lists();
        const union = [...new Set([...hiddenProviders, ...local])];
        if (union.length !== hiddenProviders.length) applyVisibility((await call(requests.visibility(union))).payload);
      });
      try { storage.removeItem(LEGACY_HIDDEN_KEY); } catch {}
      changed();
      return true;
    } catch { migrated = false; return false; }
  }

  // ------------------------------------------------------------ flows
  function setFlow(provider, flow) { if (flow) state.flows[provider] = { serial: ++serial, ...flow, provider }; else delete state.flows[provider]; changed(); }
  function stopPolling(provider) { if (timers[provider]) { cancel(timers[provider]); delete timers[provider]; } }
  function closeFlow(provider) {
    stopPolling(provider);
    const id = text(state.flows[provider]?.job?.id);
    if (id) dismissedJobs.add(id);
    delete state.flows[provider]; changed();
  }
  function takenProfiles(provider) {
    return accountsOf(provider).map(a => text(a.capabilities?.codexProfile) || text(a.label)).concat((state.registry?.accounts || []).filter(a => a.provider === provider).map(a => text(a.label)));
  }
  function takenClaude() {
    return accountsOf('claude').map(a => text(a.capabilities?.claudeProfileId)).filter(Boolean)
      .concat((state.registry?.trash || []).map(t => text(t.label)));
  }
  function startJob(provider, flow, job) {
    stopPolling(provider);
    state.flows[provider] = { ...flow, step: 'job', job, busy: false, error: null };
    changed();
    if (!jobFinished(job)) timers[provider] = schedule(() => pollJob(provider), JOB_POLL_MS);
    else if (job.state === 'succeeded') void reload();
  }
  async function pollJob(provider) {
    delete timers[provider];
    const flow = state.flows[provider];
    if (!flow?.job?.id) return;
    try {
      const { payload } = await call(requests.job(flow.job.id));
      if (state.flows[provider] !== flow && state.flows[provider]?.job?.id !== flow.job.id) return;
      const job = payload && typeof payload === 'object' ? payload : flow.job;
      state.flows[provider] = { ...state.flows[provider], job };
      changed();
      if (jobFinished(job)) {
        if (job.state === 'succeeded') {
          toast('ok', flow.type === 'job-again' ? `${label(provider)}: signed in again` : `${text(job.profileName) || label(provider)} added`, job.result?.email ? `Signed in as ${job.result.email}.` : 'Its first reading arrives with the next refresh.');
          void reload();
        }
        return;
      }
    } catch (error) {
      if (error?.status === 404) {
        state.flows[provider] = { ...state.flows[provider], job: { ...flow.job, state: 'failed', error: { code: 'server_restarted' } } };
        changed();
        return;
      }
      // a passing network failure: keep polling
    }
    if (state.flows[provider]?.job?.id === flow.job.id) timers[provider] = schedule(() => pollJob(provider), JOB_POLL_MS);
  }
  /** A 202 { job } or a 200 { guide } from Add or Sign in again opens the matching flow. */
  function followAnswer(provider, answer, base) {
    if (answer?.payload?.job) { startJob(provider, base, answer.payload.job); return true; }
    if (answer?.payload?.guide) { setFlow(provider, { type: 'guide', accountId: base.accountId, guide: answer.payload.guide }); return true; }
    return false;
  }

  async function begin(provider) {
    if (provider === 'claude') { setFlow('claude', { type: 'claude-add', step: 'name', name: suggestName('claude', takenClaude()) }); return; }
    if (KEY_PROVIDERS.includes(provider)) return addKey(provider);
    if (['codex', 'muse', 'antigravity'].includes(provider)) {
      setFlow(provider, { type: 'job-add', step: 'name', name: suggestName(provider, takenProfiles(provider)) });
    }
  }
  function addKey(provider) {
    const second = accountsOf(provider).some(a => !/console/i.test(text(a.message)) && !String(a.id).startsWith('plan-'));
    setFlow(provider, { type: 'key-add', step: 'key', second });
  }
  async function signInAgain(id, flowBase = null) {
    const provider = providerOf(id);
    const base = flowBase || { type: 'job-again', accountId: id, email: nameOf(id) };
    state.busyAct = `signin-again:${id}`; changed();
    try {
      const answer = await call(requests.signInAgain(id));
      if (!followAnswer(provider, answer, base)) toast('info', `${label(provider)}`, 'Nothing to do for this account.');
    } catch (error) {
      if (error?.payload?.code === 'account_active') state.lines[id] = { kind: 'refused', code: 'account_active_signin' };
      else if (!(await adoptRunning(provider, error))) fail(error, { provider, what: 'signin-again' });
    } finally { state.busyAct = ''; changed(); }
  }
  /** Footer "Sign in" of an app or browser-session provider: add its one account first when it has none. */
  async function sessionSignIn(provider) {
    const existing = accountsOf(provider)[0] || (state.registry?.accounts || []).find(a => a.provider === provider);
    if (existing) return signInAgain(existing.id, { type: 'job-again', accountId: existing.id, email: nameOf(existing.id) });
    state.busyAct = `session-signin:${provider}`; changed();
    try {
      const { payload } = await call(requests.addSession(provider));
      const id = payload?.account?.id;
      await reload();
      if (id) { state.busyAct = ''; await signInAgain(id, { type: 'job-again', accountId: id, email: label(provider) }); }
    } catch (error) { fail(error, { provider }); }
    finally { state.busyAct = ''; changed(); }
  }

  async function submit(provider, input, extra) {
    const flow = state.flows[provider];
    if (!flow || flow.busy) return;
    const put = patch => { if (state.flows[provider] === flow || state.flows[provider]?.serial === flow.serial) { state.flows[provider] = { ...state.flows[provider], ...patch }; changed(); } };
    // the code a supervised sign-in asks for
    if (flow.step === 'job' && flow.job?.state === 'awaiting_code') {
      const code = String(input || '').trim();
      if (!code) { put({ error: { title: 'Paste the code first.', body: '' } }); return; }
      put({ busy: true, error: null });
      try { const { payload } = await call(requests.submitCode(flow.job.id, code)); put({ busy: false, job: payload || flow.job }); stopPolling(provider); timers[provider] = schedule(() => pollJob(provider), JOB_POLL_MS); }
      catch (error) { put({ busy: false, error: errorText(error, { provider }) }); }
      return;
    }
    if (flow.type === 'job-add' && flow.step === 'name') {
      const name = String(input || '').trim();
      const problem = profileNameProblem(name, takenProfiles(provider));
      if (problem) { put({ name, error: { title: problem, body: '' } }); return; }
      put({ name, busy: true, error: null });
      try {
        const answer = await call(provider === 'codex' ? requests.addCodex(name) : requests.addSupervised(provider, name));
        if (!followAnswer(provider, answer, { ...state.flows[provider], name })) put({ busy: false });
      } catch (error) {
        if (await adoptRunning(provider, error)) return;
        put({ busy: false, error: errorText(error, { provider, name }) });
      }
      return;
    }
    if (flow.type === 'claude-add' && flow.step === 'name') {
      const id = String(input || '').trim();
      const problem = claudeIdProblem(id, takenClaude());
      if (problem) { put({ name: id, error: { title: problem, body: '' } }); return; }
      put({ name: id, step: 'creating', busy: true, error: null });
      try {
        await call(requests.addClaude(id, text(extra).trim()));
        put({ step: 'created', busy: false });
        toast('ok', `Claude profile ${id} created`, 'Open it on Mac or Windows and sign in there.');
        void reload();
      } catch (error) { put({ step: 'name', busy: false, error: errorText(error, { provider: 'claude', name: id }) }); }
      return;
    }
    if ((flow.type === 'key-add' || flow.type === 'key-replace') && flow.step === 'key') {
      const key = String(input || '');
      const problem = keyProblem(key);
      if (problem) { put({ error: { title: problem, body: '' } }); return; }
      put({ step: 'checking', busy: true, error: null });
      try {
        const { payload } = await call(flow.type === 'key-replace' ? requests.replaceKey(flow.accountId, key) : requests.addKey(provider, key, text(extra).trim()));
        put({ step: 'done', busy: false, result: { check: payload?.check, account: payload?.account } });
        const last4 = text(payload?.account?.credential?.last4);
        toast('ok', flow.type === 'key-replace' ? `${label(provider)} key replaced` : `${label(provider)} key stored`, last4 ? `It ends in ${last4}; the key itself is never shown again.` : 'The key itself is never shown again.');
        void reload();
      } catch (error) { put({ step: 'key', busy: false, error: errorText(error, { provider, what: flow.type === 'key-replace' ? 'replace' : 'add' }) }); }
      return;
    }
    if (flow.type === 'purge' && flow.step === 'type') {
      await commitPurge(provider, input);
    }
    if (flow.type === 'remove-email' && flow.step === 'type') {
      await commitRemoveEmail(provider, input);
    }
  }
  async function cancelFlow(provider) {
    const flow = state.flows[provider];
    if (!flow) return;
    const job = flow.job;
    if (job?.id && !jobFinished(job)) {
      try {
        const { payload } = await call(requests.cancelJob(job.id));
        // a login being installed cannot be stopped half-way: keep following it
        if (payload?.state === 'verifying') { state.flows[provider] = { ...flow, job: payload }; changed(); toast('info', 'Finishing the sign-in', 'It was already approved, so it finishes saving first.'); return; }
        toast('info', 'Sign-in cancelled', 'Nothing was saved.');
      } catch (error) { if (error?.status !== 404) fail(error, { provider }); }
    }
    closeFlow(provider);
  }
  async function retry(provider) {
    const flow = state.flows[provider];
    if (!flow) return;
    if (flow.type === 'job-again') { closeFlow(provider); await signInAgain(flow.accountId, { type: 'job-again', accountId: flow.accountId, email: flow.email }); return; }
    if (flow.type === 'job-add') setFlow(provider, { type: 'job-add', step: 'name', name: flow.name || suggestName(provider, takenProfiles(provider)) });
  }
  async function recheck(id, inFlow = false) {
    const provider = providerOf(id);
    state.busyAct = `recheck:${id}`;
    if (inFlow && state.flows[provider]) state.flows[provider] = { ...state.flows[provider], busy: true, error: null };
    changed();
    try {
      const { payload } = await call(requests.recheck(id));
      const account = payload?.account;
      const found = account && ['ok', 'cached'].includes(account.status);
      if (inFlow && state.flows[provider]) {
        state.flows[provider] = { ...state.flows[provider], busy: false, checked: true, found: !!found,
          error: found ? null : { title: 'No session found yet', body: `${account ? statusWord(account) : 'No reading'}. Sign in there, then re-check.` } };
      } else if (found) toast('ok', `${label(provider)} re-checked`, 'Session found; readings continue on the refresh interval.');
      else toast('info', `${label(provider)} re-checked`, `${account ? statusWord(account) : 'No reading'}. Sign in, then re-check.`);
      void reload();
    } catch (error) {
      if (inFlow && state.flows[provider]) state.flows[provider] = { ...state.flows[provider], busy: false, error: errorText(error, { provider }) };
      else fail(error, { provider }, error?.payload?.code === 'rate_limited' ? 'info' : 'err');
    } finally { state.busyAct = ''; changed(); }
  }
  async function openApp(provider, platform) {
    const flow = state.flows[provider];
    if (!flow?.accountId || !['mac', 'windows'].includes(platform)) return;
    state.busyAct = `open:${provider}:${platform}`; changed();
    try { await call(requests.openApp(flow.accountId, platform)); toast('info', `Opening ${label(provider)} on ${platform === 'mac' ? 'the Mac' : 'Windows'}`, 'Sign in there, then re-check.'); }
    catch (error) { fail(error, { provider }); }
    finally { state.busyAct = ''; changed(); }
  }

  // ------------------------------------------------------------ remove and restore (two calls, one confirmation)
  async function askRemove(id) {
    state.lines[id] = { kind: 'asking' }; changed();
    try {
      const { payload } = await call(requests.removeAsk(id));
      const c = payload?.confirmation;
      if (!c?.token) throw Object.assign(new Error('no confirmation'), { status: 500 });
      // a default Claude profile removes only after its account email is typed: the typed flow
      if (c.expectsTyped === 'email') {
        delete state.lines[id];
        setFlow(providerOf(id), { type: 'remove-email', step: 'type', accountId: id, name: nameOf(id), token: c.token, effects: Array.isArray(c.effects) ? c.effects : [] });
        changed();
        return;
      }
      state.lines[id] = { kind: 'confirm', token: c.token, effects: Array.isArray(c.effects) ? c.effects : [] };
    } catch (error) {
      const code = error?.payload?.code;
      if (REFUSALS.has(code)) state.lines[id] = { kind: 'refused', code };
      else { delete state.lines[id]; fail(error, { provider: providerOf(id) }); }
    }
    changed();
  }
  async function commitRemove(id) {
    const line = state.lines[id];
    if (line?.kind !== 'confirm') return;
    const provider = providerOf(id), who = nameOf(id);
    state.lines[id] = { ...line, kind: 'removing' }; changed();
    try {
      const { payload } = await call(requests.removeCommit(id, line.token));
      delete state.lines[id];
      const purge = Date.parse(payload?.purgeAfter);
      toast('ok', `Removed ${who}`, payload?.trashId
        ? `Its Claude data is in the trash on Mac and Windows${Number.isFinite(purge) ? ` until ${dayFmt.format(new Date(purge))}` : ''}; Restore brings it back.`
        : KEY_PROVIDERS.includes(provider) ? 'The stored key was deleted from the dashboard computer.' : `${label(provider)} no longer reads it.`);
      await reload();
    } catch (error) {
      const code = error?.payload?.code;
      if (REFUSALS.has(code)) state.lines[id] = { kind: 'refused', code };
      else { delete state.lines[id]; fail(error, { provider }); if (code === 'confirmation_stale' || code === 'unknown_account') void reload(); }
    }
    changed();
  }
  async function askRestore(trashId) {
    const key = `trash:${trashId}`;
    state.lines[key] = { kind: 'asking' }; changed();
    try {
      const { payload } = await call(requests.restoreAsk(trashId));
      const c = payload?.confirmation;
      if (!c?.token) throw Object.assign(new Error('no confirmation'), { status: 500 });
      state.lines[key] = { kind: 'confirm', token: c.token, effects: Array.isArray(c.effects) ? c.effects : [] };
    } catch (error) { delete state.lines[key]; fail(error, { provider: 'claude' }); if (error?.payload?.code === 'unknown_trash') void loadRegistry(); }
    changed();
  }
  async function commitRestore(trashId) {
    const key = `trash:${trashId}`;
    const line = state.lines[key];
    if (line?.kind !== 'confirm') return;
    const entry = (state.registry?.trash || []).find(t => t.trashId === trashId);
    state.lines[key] = { ...line, kind: 'removing' }; changed();
    try {
      await call(requests.restoreCommit(trashId, line.token));
      delete state.lines[key];
      toast('ok', `Restored ${text(entry?.label) || 'the Claude profile'}`, 'Its data is back on Mac and Windows and it is listed again.');
      await reload();
    } catch (error) { delete state.lines[key]; fail(error, { provider: 'claude' }); void loadRegistry(); }
    changed();
  }
  /** A default Claude profile: commit its removal with the typed account email. */
  async function commitRemoveEmail(provider, typed) {
    const flow = state.flows[provider];
    if (!flow || flow.type !== 'remove-email' || flow.busy) return;
    const confirm = String(typed || '').trim();
    const put = patch => { if (state.flows[provider]?.serial === flow.serial) { state.flows[provider] = { ...state.flows[provider], ...patch }; changed(); } };
    if (!confirm) { put({ error: { title: 'Type the account email first.', body: '' } }); return; }
    const who = text(flow.name) || 'this account';
    put({ busy: true, error: null });
    try {
      const { payload } = await call(requests.removeCommit(flow.accountId, flow.token, confirm));
      closeFlow(provider);
      const purge = Date.parse(payload?.purgeAfter);
      toast('ok', `Removed ${who}`, payload?.trashId
        ? `Its Claude data is in the trash on Mac and Windows${Number.isFinite(purge) ? ` until ${dayFmt.format(new Date(purge))}` : ''}; Restore brings it back.`
        : 'It is no longer read here.');
      await reload();
    } catch (error) {
      if (error?.payload?.code === 'invalid_body') put({ busy: false, error: { title: 'That email does not match this account.', body: 'Type it exactly as the row shows it.' } });
      else { put({ busy: false, error: errorText(error, { provider }) }); if (error?.payload?.code === 'confirmation_stale' || error?.payload?.code === 'unknown_account') void reload(); }
    }
  }
  /** Delete now: ask for the confirmation, then open the typed-DELETE flow under Claude. */
  async function askPurge(trashId) {
    const entry = (state.registry?.trash || []).find(t => t.trashId === trashId);
    const label = text(entry?.label) || 'this profile';
    setFlow('claude', { type: 'purge', step: 'asking', trashId, label });
    try {
      const { payload } = await call(requests.purgeAsk(trashId));
      const c = payload?.confirmation;
      if (!c?.token) throw Object.assign(new Error('no confirmation'), { status: 500 });
      setFlow('claude', { type: 'purge', step: 'type', trashId, label, token: c.token, effects: Array.isArray(c.effects) ? c.effects : [] });
    } catch (error) {
      closeFlow('claude');
      fail(error, { provider: 'claude' });
      if (error?.payload?.code === 'unknown_trash') void loadRegistry();
    }
  }
  async function commitPurge(provider, typed) {
    const flow = state.flows[provider];
    if (!flow || flow.type !== 'purge' || flow.busy) return;
    const confirm = String(typed || '').trim();
    const put = patch => { if (state.flows[provider]?.serial === flow.serial) { state.flows[provider] = { ...state.flows[provider], ...patch }; changed(); } };
    if (!confirm) { put({ error: { title: 'Type DELETE first.', body: '' } }); return; }
    put({ busy: true, error: null });
    try {
      await call(requests.purgeCommit(flow.trashId, flow.token, confirm));
      put({ step: 'done', busy: false });
      toast('ok', `Deleted ${text(flow.label) || 'the Claude profile'} for good`, 'Its Claude data is gone on Mac and Windows.');
      void reload();
    } catch (error) {
      if (error?.payload?.code === 'invalid_body') put({ busy: false, error: { title: 'Type DELETE exactly as shown.', body: '' } });
      else { put({ busy: false, error: errorText(error, { provider: 'claude' }) }); if (error?.payload?.code === 'unknown_trash' || error?.payload?.code === 'confirmation_stale') void loadRegistry(); }
    }
  }

  // ------------------------------------------------------------ the Dashboard sign-in block
  async function signOutOthers() {
    const s = state.signin;
    s.busy = 'others'; changed();
    try {
      const { payload } = await call(requests.revokeOthers());
      const n = Number.isInteger(payload?.signedOutBrowsers) ? payload.signedOutBrowsers : 0;
      toast('ok', n ? `Signed out ${n} other browser${n === 1 ? '' : 's'}` : 'No other browser was signed in', 'This browser and the paired trays stay signed in.');
    } catch (error) { fail(error); }
    finally { s.busy = ''; await loadSignin(); }
  }
  async function setNetwork(on) {
    const s = state.signin;
    s.busy = 'network'; changed();
    try {
      const { payload } = await call(requests.setNetwork(on));
      s.network = payload || s.network;
      try { await networkChanged(s.network); } catch {}
      toast('info', on ? 'Local network trusted' : 'Local network trust turned off',
        on ? 'Passwords, keys and sign-in codes may now cross your home network without encryption.'
          : 'Password changes, keys, sign-in codes and tray pairing now work only on the dashboard computer itself.');
    } catch (error) { fail(error); }
    finally { s.busy = ''; await loadSignin(); }
  }
  const LIFETIME_DAYS = [1, 7, 30, 90, 365];
  const LIFETIME_WORD = { 1: '1 day', 7: '7 days', 30: '30 days', 90: '90 days', 365: '1 year' };
  async function setLifetime(value) {
    const days = Number(value);
    if (!LIFETIME_DAYS.includes(days)) return;
    const s = state.signin;
    if (s.session && s.session.sessionLifetimeDays === days) return;
    s.busy = 'lifetime'; changed();
    try {
      const { payload } = await call(requests.sessionLifetime(days));
      if (s.session) s.session = { ...s.session, sessionTimeoutHours: payload?.hours ?? days * 24, sessionLifetimeDays: payload?.days ?? days };
      toast('info', `Sessions now last ${LIFETIME_WORD[days]}`, 'Browsers already signed in keep their own session; the new lifetime applies from the next sign-in.');
    } catch (error) { fail(error); }
    finally { s.busy = ''; changed(); }
  }
  /** Save the whole preferences shape with one field changed; the page follows it without a reload. */
  async function savePrefs(next, note) {
    const p = state.prefs;
    p.busy = note; changed();
    try {
      const { payload } = await call(requests.savePreferences(next));
      p.data = payload && typeof payload === 'object' ? payload : next;
      if (typeof p.data.timeZone === 'string') setDisplayTimeZone(p.data.timeZone);
    } catch (error) { fail(error); }
    finally { p.busy = ''; changed(); }
  }
  async function setTimeZone(zone) {
    const value = String(zone || '');
    if (!value || value.length > 64) return;
    const current = state.prefs.data;
    if (current?.timeZone === value) return;
    await savePrefs({
      timeZone: value,
      snapshotCleanup: current?.snapshotCleanup || { auto: true },
      usageLogSources: Array.isArray(current?.usageLogSources) ? current.usageLogSources : [],
    }, 'timezone');
  }
  async function toggleCleanupAuto() {
    const current = state.prefs.data;
    if (!current || state.prefs.busy) return;
    await savePrefs({
      timeZone: current.timeZone || DEFAULT_DISPLAY_TIME_ZONE,
      snapshotCleanup: { auto: current.snapshotCleanup?.auto === false },
      usageLogSources: Array.isArray(current.usageLogSources) ? current.usageLogSources : [],
    }, 'cleanup-auto');
  }
  async function cleanupNow() {
    const p = state.prefs;
    if (!p.data || p.cleanupBusy || p.busy) return;
    p.cleanupBusy = true; p.cleanupNote = ''; changed();
    try {
      const { payload } = await call(requests.cleanupSnapshots());
      const deleted = Number(payload?.deleted) || 0;
      const skipped = Number(payload?.skipped) || 0;
      const failed = Number(payload?.failed) || 0;
      const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
      p.cleanupNote = deleted === 0 && failed === 0
        ? 'Nothing to clean: every profile already keeps only its newest snapshots.'
        : `Deleted ${plural(deleted, 'older snapshot', 'older snapshots')}${skipped ? `, skipped ${plural(skipped, 'folder', 'folders')} that ${skipped === 1 ? 'was' : 'were'} not AAC snapshots` : ''}${failed ? `, ${plural(failed, 'target', 'targets')} unreachable` : ''}.`;
    } catch (error) { fail(error); }
    finally { p.cleanupBusy = false; changed(); }
  }
  async function addLogSource(v) {
    const current = state.prefs.data;
    if (!current || state.prefs.busy) return;
    // "tool\nhost\npath\nmapping-json?": the mapping JSON holds no raw newlines, and a path holding
    // one fails the absolute-path check below, so the split cannot smuggle a bad path through.
    const [tool = '', host = '', location = '', ...rest] = String(v ?? '').split('\n');
    const json = rest.join('\n');
    let mapping;
    if (json) {
      try { mapping = JSON.parse(json); } catch { mapping = null; }
      if (!mapping || typeof mapping !== 'object' || Array.isArray(mapping)) {
        state.prefs.logSourcesError = 'The field mapping is not valid JSON.';
        changed();
        return;
      }
    }
    const problem = logSourceProblem(tool, host, location, mapping);
    if (problem) { state.prefs.logSourcesError = problem; changed(); return; }
    const saved = Array.isArray(current.usageLogSources) ? current.usageLogSources : [];
    const sameMapping = (a, b) => {
      if (a === undefined || a === null) return b === undefined || b === null;
      if (b === undefined || b === null) return false;
      return ['timestamp', 'model', 'inputTokens', 'outputTokens', 'cost']
        .every(key => (a[key] ?? undefined) === (b[key] ?? undefined));
    };
    if (saved.some(entry => entry?.tool === tool && entry?.host === host && entry?.path === location && sameMapping(entry?.fieldMapping, mapping))) {
      state.prefs.logSourcesError = 'This location is already listed.';
      changed();
      return;
    }
    let id = '';
    do { id = `log-${Math.random().toString(36).slice(2, 10)}`; } while (saved.some(entry => entry?.id === id));
    state.prefs.logSourcesError = '';
    await savePrefs({
      timeZone: current.timeZone || DEFAULT_DISPLAY_TIME_ZONE,
      snapshotCleanup: current.snapshotCleanup || { auto: true },
      usageLogSources: [
        ...saved,
        mapping === undefined
          ? { id, tool, host, path: location }
          : { id, tool, host, path: location, fieldMapping: mapping },
      ],
    }, 'logsources');
  }
  async function removeLogSource(id) {
    const current = state.prefs.data;
    if (!current || state.prefs.busy) return;
    const saved = Array.isArray(current.usageLogSources) ? current.usageLogSources : [];
    if (!saved.some(entry => entry?.id === id)) return;
    state.prefs.logSourcesError = '';
    await savePrefs({
      timeZone: current.timeZone || DEFAULT_DISPLAY_TIME_ZONE,
      snapshotCleanup: current.snapshotCleanup || { auto: true },
      usageLogSources: saved.filter(entry => entry?.id !== id),
    }, 'logsources');
  }
  function togglePassword() {
    const pw = state.signin.pw;
    if (pw.busy) return;
    state.signin.pw = { ...blankPassword(), open: !pw.open, nonce: pw.nonce };
    strength({ ...passwordStrength(''), matches: false });
    changed();
  }
  function typingPassword(value) {
    const [next = '', confirm = ''] = String(value).split('\n');
    strength({ ...passwordStrength(next), matches: !!confirm && confirm === next });
  }
  async function changePassword(value) {
    const pw = state.signin.pw;
    if (pw.busy || !pw.open) return;
    const [current = '', next = '', confirm = '', others = '1'] = String(value).split('\n');
    const bad = passwordProblem({ current, next, confirm });
    if (bad) { state.signin.pw = { ...pw, field: bad[0], error: bad[1], nonce: pw.nonce + 1 }; changed(); return; }
    state.signin.pw = { ...pw, busy: true, field: '', error: '' }; changed();
    try {
      const { payload } = await call(requests.password(current, next, others !== '0'));
      state.signin.pw = { ...state.signin.pw, busy: false, done: true };
      const t = passwordChangedToast(payload, state.signin.devices || []);
      toast('ok', t.title, t.body, 7000);
      changed();
      schedule(() => { state.signin.pw = { ...blankPassword(), nonce: state.signin.pw.nonce }; strength({ ...passwordStrength(''), matches: false }); changed(); void loadSignin(); }, 1300);
    } catch (error) {
      const code = error?.payload?.code;
      const t = errorText(error);
      const field = code === 'wrong_password' || code === 'rate_limited' ? 'cur' : code === 'weak_password' || code === 'same_password' ? 'new' : 'form';
      state.signin.pw = { ...state.signin.pw, busy: false, field, error: [t.title, t.body].filter(Boolean).join('. ').replace(/\.\./g, '.'), nonce: state.signin.pw.nonce + 1 };
      changed();
    }
  }
  async function revokeDevice(id) {
    const s = state.signin;
    const device = (s.devices || []).find(d => d.id === id);
    s.busy = `revoke:${id}`; changed();
    try {
      await call(requests.revokeDevice(id));
      toast('ok', `${text(device?.name) || 'The tray'} signed out`, 'It shows its pairing screen until someone signs in on it again.');
    } catch (error) { fail(error); }
    finally { s.busy = ''; await loadSignin(); }
  }
  async function revokeAll() {
    const s = state.signin;
    s.busy = 'all'; changed();
    try { const t = revokeAllToast((await call(requests.revokeAll())).payload); toast('ok', t.title, t.body, 7000); }
    catch (error) { fail(error); }
    finally { s.busy = ''; await loadSignin(); }
  }

  // ------------------------------------------------------------ the action seam
  /** Returns true when the action was one of this controller's. */
  async function handle(action, value) {
    const v = String(value ?? '');
    switch (action) {
      case 'accounts-show': { const [p, mode] = v.split(':'); if (mode === 'show' || mode === 'hide') await setShown(p, mode === 'show'); return true; }
      case 'accounts-tray': { const [p, mode] = v.split(':'); if (mode === 'show' || mode === 'hide') await setTrayShown(p, mode === 'show'); return true; }
      // one account: "<account id>|show" or "<account id>|hide" (ids hold ':' but never '|')
      case 'account-show': { const [id, mode] = v.split('|'); if (mode === 'show' || mode === 'hide') await setAccountShown(id, mode === 'show'); return true; }
      case 'account-tray': { const [id, mode] = v.split('|'); if (mode === 'show' || mode === 'hide') await setAccountTrayShown(id, mode === 'show'); return true; }
      case 'add': await begin(v); return true;
      case 'add-key': if (KEY_PROVIDERS.includes(v)) addKey(v); return true;
      case 'replace-key': { const p = providerOf(v); if (KEY_PROVIDERS.includes(p)) setFlow(p, { type: 'key-replace', step: 'key', accountId: v }); return true; }
      case 'signin-again': case 'signin': await signInAgain(v); return true;
      case 'session-signin': await sessionSignIn(v); return true;
      case 'recheck': await recheck(v); return true;
      case 'flow-submit': { const [p = '', input = '', extra = ''] = v.split('\n'); await submit(p, input, extra); return true; }
      case 'flow-cancel': await cancelFlow(v); return true;
      case 'flow-done': closeFlow(v); void reload(); return true;
      case 'flow-retry': await retry(v); return true;
      case 'flow-copy': {
        const code = text(state.flows[v]?.job?.verification?.userCode);
        if (code) { const ok = await copy(code); toast(ok ? 'ok' : 'info', ok ? 'Code copied' : 'Copy the code by hand', ok ? 'Paste it on the verification page.' : 'This browser did not allow copying.'); }
        return true;
      }
      case 'flow-open-url': { const url = text(state.flows[v]?.job?.verification?.url); if (/^https:\/\//.test(url)) open(url); return true; }
      case 'flow-open-app': { const [p, platform] = v.split(':'); await openApp(p, platform); return true; }
      case 'flow-recheck': { const id = state.flows[v]?.accountId; if (id) await recheck(id, true); return true; }
      case 'remove': await askRemove(v); return true;
      case 'refuse': { const [id, code] = v.split('\n'); if (id && code) { state.lines[id] = { kind: 'refused', code }; changed(); } return true; }
      case 'line-cancel': delete state.lines[v]; delete state.lines[`trash:${v}`]; changed(); return true;
      case 'remove-commit': await commitRemove(v); return true;
      case 'restore': await askRestore(v); return true;
      case 'restore-commit': await commitRestore(v); return true;
      case 'purge': await askPurge(v); return true;
      case 'others-out': await signOutOthers(); return true;
      case 'network-off': await setNetwork(false); return true;
      case 'network-on': await setNetwork(true); return true;
      case 'session-lifetime': await setLifetime(v); return true;
      case 'time-zone': await setTimeZone(v); return true;
      case 'cleanup-auto': await toggleCleanupAuto(); return true;
      case 'cleanup-now': await cleanupNow(); return true;
      case 'logsource-add': await addLogSource(v); return true;
      case 'logsource-remove': await removeLogSource(v); return true;
      case 'pw-toggle': togglePassword(); return true;
      case 'pw-typing': typingPassword(v); return true;
      case 'pw-submit': await changePassword(v); return true;
      case 'device-revoke': await revokeDevice(v); return true;
      case 'devices-revoke-all': await revokeAll(); return true;
      default: return false;
    }
  }
  function reset() {
    for (const p of Object.keys(timers)) stopPolling(p);
    dismissedJobs.clear();
    state.registry = null; state.flows = {}; state.lines = {}; state.busyAct = ''; state.visPending = [];
    state.signin = { session: null, devices: null, devicesError: false, network: null, pw: blankPassword(), busy: '' };
  }
  return { state, handle, loadRegistry, loadSignin, loadPrefs, loadAll, migrateLocalHidden, reset, pollJob, applyVisibility };
}
