// The app frame's Home, Accounts & Settings, Details and switch approvals (G5): the frame reads the dashboard itself, as
// Analytics reads its own (frame-analytics.mjs), so no tab loads the Slint dashboard or its bridge any more. This is
// bridge.js's content-mode path moved into the top window: the same server calls, timers and guards, the same pure
// modules (view-model.mjs, accounts-view.mjs, accounts-controller.mjs, activation-confirmation.mjs,
// antigravity-confirmation.mjs, claude-open.mjs, account-actions.mjs, auth-view.mjs) and the same views, keys and
// action kinds as the seam (frame-seam.mjs VIEW_KEYS and PAGE_ACTIONS). Each function names the bridge.js one it
// follows. The classic app (bridge.js) is unchanged. Pure apart from the injected fetch, storage, clock, timers and
// sinks (tests/frame-dashboard.test.mjs).
//
// What it sends, through `emit(key, json)`: `dashboard`, `accounts`, `accounts-strength`, `details` ('' = closed),
// `activation` ('' = closed), `update`, `busy` ("true"/"false"), `refresh-interval`, plus `toast` ({kind, title, body,
// ms}), `chrome` ({statusLead, statusStrong, statusMore}) and `signed-out` ({state: 'expired'|'default', reason}).
// Until the first answer nothing is sent for Home but the chrome's "Refreshing": the page shows its loading state,
// never zeros.
import { dashboardViewModel, detailsViewModel, chromeView, updateViewModel, intervalLabel, parseIntervalLabel, isCatalogPending } from './view-model.mjs';
import { needToasts } from './claude-signins.mjs';
import { accountsViewModel } from './accounts-view.mjs';
import { transportOf } from './transport.mjs';
import { rememberSessionHours, rememberedSessionHours, forgetSignIn, signedInAt, rememberSeen, lastSeenAt, endedReason } from './auth-view.mjs';
import { createAccountsController, MUTATING_ACTIONS } from './accounts-controller.mjs';
import { signOutFailureText, copyText } from './account-actions.mjs';
import { createActivationConfirmation } from './activation-confirmation.mjs';
import { antigravityView, antigravitySettingsPatch, validAntigravityAuto } from './antigravity-data.mjs';
import { createAntigravityConfirmation } from './antigravity-confirmation.mjs';
import { createClaudeOpen, openProgress } from './claude-open.mjs';
import { PAGE_ACTIONS } from './frame-seam.mjs';
import { setDisplayTimeZone } from './time-format.mjs';
import { installRow as installRowFor, isAppleMobile } from './device.mjs';

/** Below this window width Details and the switch approval take one Back entry each (bridge.js belowD). */
export const OVERLAY_HISTORY_BELOW = 1280;
/** The frame's own commands (frame.rs): Refresh, Update apps and Sign out from the header. */
export const FRAME_COMMANDS = Object.freeze(new Set(['refresh', 'update-apps']));
/** The page actions this controller runs: the seam's allowlist without Analytics' (frame-analytics.mjs owns those). */
export const DASHBOARD_ACTIONS = Object.freeze(new Set([...PAGE_ACTIONS].filter((kind) => !kind.startsWith('analytics-'))));

const UPDATE_JOB_ID = /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i;
const PROFILE_ID = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;
// A password change, Sign out other browsers and Sign out all devices give this browser a new session id (bridge.js).
const ROTATES_SESSION = new Set(['/api/auth/password', '/api/auth/sessions/revoke-others', '/api/auth/devices/revoke-all']);

/**
 * The frame's dashboard controller. Injected: `fetch`, `storage` (localStorage or null), `location` ({protocol,
 * hostname, host, origin}), `now()`, `setTimeout`/`clearTimeout`, `setInterval`/`clearInterval`, `platform` ('mac' or
 * 'windows', as bridge.js picks it), `emit(key, json)`, `copy(text)`, `open(url)`, `installRow()` (the Settings "Home
 * screen app" row), `promptInstall()`, `overlay` ({depth(), push(), consume()}: the Back entries) and `onAccounts(vm)` /
 * `onToast(toast)` (e2e only).
 */
export function createFrameDashboard({
  fetch: fetchImpl,
  storage = null,
  location = { protocol: 'http:', hostname: '', host: '', origin: '' },
  now = () => Date.now(),
  setTimeout: later = (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: cancelLater = (id) => globalThis.clearTimeout(id),
  setInterval: every = (fn, ms) => globalThis.setInterval(fn, ms),
  clearInterval: cancelEvery = (id) => globalThis.clearInterval(id),
  platform = 'mac',
  emit = () => {},
  copy = async () => false,
  open = () => {},
  installRow = () => 'hidden',
  promptInstall = async () => {},
  overlay = { depth: () => 0, push() {}, consume() {} },
  onAccounts = null,
  onToast = null,
}) {
  let started = false;
  let stopped = false;
  // The session this controller served is over (signed out, or ended under it); stop() follows when the page goes.
  let ended = false;
  let authenticated = false;
  let busy = false;
  // `serverData` is the dashboard response as received; `data` is the copy the pages draw from (bridge.js).
  let serverData = null;
  let data = null;
  let profiles = [];
  let antigravityInventory = null;
  let antigravityAuto = null;
  let updateJob = null;
  let updateStatusPending = false;
  let updateCompletionId = null;
  let updateDoneTimer = 0;
  let refreshing = null;
  let refreshGeneration = 0;
  let username = '';
  let refreshIntervalSeconds = 60;
  let refreshSettingsKnown = false;
  let usageTimer = 0;
  let openDetailsId = '';
  let activationDialogOpen = false;
  let currentPage = 'home';
  let onlineState = true;
  let sessionHours = rememberedSessionHours(storage);
  // GET /api/auth/check as last read: access mode, trusted local network and this connection (bridge.js authCheck)
  let authCheck = null;
  const transport = transportOf(location.protocol, location.hostname);
  const origin = typeof location.origin === 'string' ? location.origin : '';
  const host = typeof location.host === 'string' ? location.host : '';
  const intervals = [];
  const pushed = new Map();
  // Each new person-only step of the Claude sign-in renewal (a CAPTCHA, a missing mail) is raised once.
  const seenSignInNeeds = new Set();

  // The controller's lifetime (Astra max M1). The old iframe took its pending work with it when it was removed; this
  // controller stays in the window after its page has gone, so its work ends here instead: once the session is over or
  // the controller stopped, nothing it queued, awaited or scheduled reaches the network (the browser would send it with
  // the next session's cookie), draws a view, raises a toast or starts a timer. Requests in flight are aborted.
  const lifetime = typeof AbortController === 'function' ? new AbortController() : null;
  const gone = () => stopped || ended;
  const dropped = () => Object.assign(new Error('This page has signed out.'), { dropped: true, status: 0, payload: null });
  /** A timer that never fires for a controller that has gone, and is never set by one. */
  const schedule = (fn, ms) => (gone() ? 0 : later(() => { if (!gone()) fn(); }, ms));

  const view = (key, json) => { if (!gone()) emit(key, json); };
  /** Home and Accounts drew this JSON already: identical pushes change nothing (bridge.js pushModel). */
  const pushModel = (key, json) => {
    if (pushed.get(key) === json) return;
    pushed.set(key, json);
    view(key, json);
  };

  // ---------------------------------------------------------------- feedback (bridge.js setBusy, toast, failure)
  function setBusy(value) { busy = value; view('busy', value ? 'true' : 'false'); }
  function toast(kind, title, body = '', ms = 4800) {
    if (gone()) return;
    if (onToast) onToast({ kind, title, body });
    emit('toast', JSON.stringify({ kind, title, body, ms }));
  }
  function failure(message, title = 'That did not work') { toast('err', title, message, 6400); }

  // ---------------------------------------------------------------- the network (bridge.js send, sendNow)
  let inFlight = 0;
  let idleWaiters = [];
  let rotation = null;
  /** One request: { status, payload } on success; a refusal throws with its status, payload and headers. */
  async function send(path, options = {}) {
    const rotates = String(options.method || 'GET').toUpperCase() === 'POST' && ROTATES_SESSION.has(path);
    while (rotation) await rotation;
    // made, or still waiting, when the session ended: dropped before it reaches the network
    if (gone()) throw dropped();
    let release = null;
    if (rotates) {
      rotation = new Promise((resolve) => { release = resolve; });
      if (inFlight > 0) await new Promise((resolve) => idleWaiters.push(resolve));
    }
    inFlight++;
    try {
      if (gone()) throw dropped();
      return await sendNow(path, options);
    } finally {
      inFlight--;
      if (inFlight === 0) { const waiters = idleWaiters; idleWaiters = []; for (const wake of waiters) wake(); }
      if (rotates) { rotation = null; release(); }
    }
  }
  async function sendNow(path, options) {
    let response;
    try {
      response = await fetchImpl(path, { credentials: 'same-origin', ...options, ...(lifetime ? { signal: lifetime.signal } : {}), headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
    } catch (cause) {
      if (gone()) throw dropped();
      setOnline(false);
      throw Object.assign(new Error('The dashboard did not answer.'), { network: true, status: 0, payload: null, cause });
    }
    // an answer that lands after the session ended belongs to no one: nothing acts on it
    if (gone()) throw dropped();
    setOnline(true);
    const payload = response.status === 204 ? null : await response.json().catch(() => null);
    if (gone()) throw dropped();
    if (!response.ok) {
      // signed out from another browser (sign out other browsers, a password change, sign out all devices). A 401 that
      // is about the session ends it here; a refused password (wrong_password) is only an answer.
      const code = payload?.code;
      if (response.status === 401 && path !== '/api/auth/login' && authenticated && authCheck?.accessMode !== 'open' && (!code || code === 'auth_required' || code === 'session_revoked')) sessionEnded(code === 'session_revoked' ? 'revoked' : undefined);
      const error = new Error(payload?.error || `Request failed (${response.status}).`);
      error.status = response.status;
      error.payload = payload;
      error.headers = response.headers;
      error.retryAfter = response.headers?.get?.('Retry-After') || response.headers?.get?.('RateLimit-Reset') || '';
      throw error;
    }
    return { status: response.status, payload };
  }
  async function request(path, options = {}) { return (await send(path, options)).payload; }
  const mutation = (path, body, method = 'POST') => request(path, { method, body: JSON.stringify(body) });
  /** One request built by account-actions.mjs `requests` ({ method, path, body }). */
  const call = (req) => send(req.path, { method: req.method, ...(req.body !== undefined ? { body: JSON.stringify(req.body) } : {}) });
  function setOnline(value) {
    const next = value !== false;
    if (next === onlineState) return;
    onlineState = next;
    // everything returns to normal on the next successful API call: the status line first
    if (next && authenticated && data) renderChrome(false);
  }

  // Accounts & Settings: every sign-in and sign-out control (accounts-controller.mjs), wired as bridge.js wires it.
  const accounts = createAccountsController({
    call,
    changed: () => renderAccounts(),
    toast: (kind, title, body, ms) => toast(kind, title, body, ms),
    refresh: () => refresh(true),
    data: () => serverData,
    // A save's answer is newer than any dashboard read already in flight: drop that read and start a fresh one.
    setData: (next) => {
      const reading = !!refreshing;
      refreshGeneration++;
      serverData = next; data = next; render();
      if (reading) void refresh(false, true);
    },
    strength: (v) => view('accounts-strength', JSON.stringify(v)),
    copy: (value) => copy(value),
    open: (url) => { try { open(url); } catch {} },
    storage,
    schedule,
    cancel: cancelLater,
    now,
    // Turn off (or on): the saved check follows at once, then is read again (secureTransport moves with it)
    networkChanged: async (net) => {
      if (authCheck && net && typeof net.trustLocalNetwork === 'boolean') {
        authCheck = { ...authCheck, trustedLocalNetwork: net.trustLocalNetwork, ...(net.connection && typeof net.connection === 'object' ? { connection: net.connection } : {}) };
      }
      await refreshAuthCheck();
    },
  });

  // Claude "Open" progress (claude-open.mjs), as bridge.js runs it.
  const claudeOpen = createClaudeOpen({
    post: async (id, target) => {
      const { status, payload } = await send(`/api/claude/desktop-profiles/${encodeURIComponent(id)}/open`, {
        method: 'POST', body: JSON.stringify({ platform: target }), headers: { Prefer: 'respond-async' },
      });
      return { status, body: payload };
    },
    list: async () => {
      const payload = await request('/api/claude/desktop-profiles');
      if (Array.isArray(payload?.profiles)) profiles = payload.profiles;
      return payload;
    },
    schedule,
    cancel: cancelLater,
    clock: now,
    changed: () => { if (authenticated) render(); },
    finished: (id, v) => {
      if (!authenticated) return;
      const where = v.platform === 'windows' ? 'Windows' : 'Mac';
      if (v.state === 'opened') toast('ok', `Claude opened on ${where}`, 'It opened in its own desktop profile.');
      else if (v.state === 'failed' || v.state === 'blocked_uncertain') failure(v.text, `Claude did not open on ${where}`);
      else toast('info', `Opening Claude on ${where}`, v.text, 7000);
    },
  });

  // ---------------------------------------------------------------- rendering (bridge.js)
  function context(extra = {}) {
    return { profiles, platform, antigravityInventory, antigravityAuto, refreshing: !!refreshing && extra.refreshing !== false, intervalSeconds: refreshIntervalSeconds, username, host, openProgress: openProgress(claudeOpen.views(), profiles), ...extra };
  }
  function antigravityModel() { return antigravityView(data, antigravityInventory, antigravityAuto); }
  function render() {
    if (!data || stopped) return;
    for (const item of needToasts(data, seenSignInNeeds)) toast(item.kind, item.title, item.body, 20000);
    pushModel('dashboard', JSON.stringify(dashboardViewModel(data, context({ refreshing: false }))));
    if (openDetailsId) renderDetails(openDetailsId);
    renderAccounts();
  }
  /** The session lifetime the server reported: kept for the sign-in page, which a proxy visitor is not told. */
  function learnSessionHours(hours) {
    if (!Number.isInteger(hours) || hours <= 0) return;
    sessionHours = hours;
    rememberSessionHours(storage, hours);
  }
  /** Accounts & Settings (version 3, accounts-view.mjs): built while the page is open. */
  function renderAccounts() {
    if (stopped || (!serverData && authCheck?.accessMode !== 'open') || currentPage !== 'accounts') return;
    const st = accounts.state;
    // Settings reads the session (GET /api/auth/session) and changes its lifetime: the sign-in page follows.
    learnSessionHours(st.signin?.session?.sessionTimeoutHours);
    try {
      const vm = accountsViewModel(serverData || { accounts: [], providers: [] }, context({
        refreshing: false,
        refreshSeconds: refreshIntervalSeconds, refreshKnown: refreshSettingsKnown, updateJob,
        serverVersion: serverData?.server?.version,
        origin, transport, sessionHours, signedInAt: signedInAt(storage),
        registry: st.registry, flows: st.flows, lines: st.lines, busyAct: st.busyAct, visPending: st.visPending, check: authCheck, signin: st.signin, prefs: st.prefs,
      }));
      // the Settings "Home screen app" row state (device.mjs installRow); accounts-view.mjs does not compute it
      vm.install = installRow();
      if (onAccounts) onAccounts(vm);
      pushModel('accounts', JSON.stringify(vm));
    } catch (error) { try { console.error('Accounts & Settings could not be drawn.', error); } catch {} }
  }
  function renderChrome(isRefreshing = false) {
    if (stopped || !(data || isRefreshing)) return;
    const chrome = chromeView(data, { now: now(), refreshing: isRefreshing, intervalSeconds: refreshIntervalSeconds, username, host });
    // the connection dropped; the readings stay on screen and the status line says so
    if (!onlineState && !isRefreshing && chrome.statusLead === 'Updated') chrome.statusLead = 'Offline';
    pushModel('chrome', JSON.stringify({ statusLead: chrome.statusLead, statusStrong: chrome.statusStrong, statusMore: chrome.statusMore }));
  }
  function renderDetails(id) {
    const v = data ? detailsViewModel(data, id, context({ refreshing: false })) : null;
    if (!v) { openDetailsId = ''; view('details', ''); return; }
    openDetailsId = id;
    view('details', JSON.stringify(v));
  }
  function pendingActivation() { return activationConfirmation.hasPending() || antigravityConfirmation.hasPending(); }

  // ---------------------------------------------------------------- the switch approval (bridge.js)
  function showActivationDialog(confirmation) {
    if (!activationDialogOpen) {
      // Details hands its Back entry to the dialog; no asynchronous Back followed by a push.
      const replacesDetails = !!openDetailsId;
      openDetailsId = '';
      if (replacesDetails) view('details', '');
      if (!replacesDetails || overlay.depth() === 0) overlay.push();
      activationDialogOpen = true;
    }
    view('activation', JSON.stringify({ ...confirmation, expiresAt: `Review valid until ${new Date(confirmation.expiresAt).toLocaleString()}` }));
  }
  function closeActivationDialog() {
    if (activationDialogOpen) {
      activationDialogOpen = false;
      overlay.consume();
    }
    view('activation', '');
  }

  const activationConfirmation = createActivationConfirmation({
    activate: (target, body) => mutation(`/api/codex/profiles/${encodeURIComponent(target)}/activate`, body),
    prompt: showActivationDialog,
    close: closeActivationDialog,
    busy: (inProgress) => setBusy(inProgress),
    success: async (result) => {
      if (data && typeof result?.name === 'string') {
        for (const account of data.accounts) if (account.provider === 'codex') account.isActive = account.capabilities?.codexProfile === result.name;
        render();
      }
      toast('ok', 'Codex account switched', `${result?.email || result?.name || 'The selected account'} is now active on Ubuntu.`);
      await refresh(true);
    },
    error: (message) => failure(message, 'Codex switch failed'),
    now,
  });

  const antigravityConfirmation = createAntigravityConfirmation({
    activate: (target, body) => mutation(`/api/antigravity/profiles/${encodeURIComponent(target)}/activate`, body),
    confirm: (target, body) => mutation(`/api/antigravity/profiles/${encodeURIComponent(target)}/confirm`, body),
    recover: () => mutation('/api/antigravity/recover', { hostId: 'ubuntu' }),
    prompt: showActivationDialog,
    close: closeActivationDialog,
    busy: (inProgress) => setBusy(inProgress),
    success: async (result) => {
      // Selection/running proof comes from the next inventory, never an optimistic UI guess.
      if (['completed', 'aborted', 'restored-previous', 'no-recovery-pending'].includes(result?.status)) {
        const who = result?.email || 'The live login';
        const what = result?.status === 'completed' ? `${who} was activated.`
          : result?.status === 'aborted' ? `The stuck switch was undone; ${who} is still active.`
          : result?.status === 'restored-previous' ? `${who} was restored from its saved login.`
          : 'Nothing was stuck.';
        toast('ok', 'Antigravity recovery finished on Ubuntu', `${what} Checking the native identity now.`);
      } else {
        toast('ok', 'Antigravity switched on Ubuntu', `${result?.email || 'The selected login'} was activated. Checking the native identity now.`);
      }
      antigravityInventory = null; render();
      await refresh(true);
    },
    error: (message) => failure(message, 'Antigravity switch failed'),
    now,
  });

  // ---------------------------------------------------------------- reading (bridge.js refresh)
  /** `gate`: the first read waits for the settings and the time zone, which bridge.js applied before it. */
  async function refresh(force = false, restart = false, gate = null) {
    if (!authenticated) return;
    if (authCheck?.accessMode === 'open') {
      renderAccounts();
      const v = dashboardViewModel(null, context());
      v.chrome.statusLead = 'Dashboard sign-in is off';
      v.chrome.statusMore = 'Sign in to read account usage';
      v.sections = []; v.cards = [];
      view('dashboard', JSON.stringify(v));
      return;
    }
    if (refreshing && !force && !restart) return refreshing;
    const generation = ++refreshGeneration;
    renderChrome(true);
    const operation = (async () => {
      try {
        const [next, inventory, antigravityProfiles, antigravityStatus] = await Promise.all([
          request(`/api/accounts/dashboard?platform=${platform}&refresh=${force}`),
          request('/api/claude/desktop-profiles').catch(() => ({ profiles })),
          request('/api/antigravity/profiles').catch(() => null),
          request('/api/antigravity/auto-switch').catch(() => null),
          gate,
        ]);
        if (generation !== refreshGeneration || !authenticated) return;
        if (next?.schemaVersion !== 1 || !Array.isArray(next.accounts)) throw new Error('Unsupported account dashboard response.');
        serverData = next;
        data = next;
        // The server accepted the session just now: a later sign-out within the lifetime ended early.
        rememberSeen(storage, now());
        if (Number.isInteger(next.settings?.refreshIntervalSeconds)) applyRefreshInterval(next.settings.refreshIntervalSeconds);
        profiles = Array.isArray(inventory?.profiles) ? inventory.profiles : profiles;
        antigravityInventory = antigravityProfiles;
        antigravityAuto = validAntigravityAuto(antigravityStatus) ? antigravityStatus : validAntigravityAuto(next.antigravityAutoSwitch) ? next.antigravityAutoSwitch : null;
        render();
        // the page's actions and refusals follow the accounts: read them again with each refresh while it is open
        if (currentPage === 'accounts') void accounts.loadRegistry();
      } catch (error) {
        if (generation === refreshGeneration && authenticated) failure(`${error.message} The last received readings stay visible.`, 'Refresh failed');
      }
    })();
    refreshing = operation;
    await operation;
    if (refreshing === operation) { refreshing = null; renderChrome(false); }
  }

  // ---------------------------------------------------------------- settings, updates, session (bridge.js)
  function applyRefreshInterval(seconds, confirmed = true) {
    if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
    refreshIntervalSeconds = seconds;
    refreshSettingsKnown = confirmed;
    pushRefreshInterval(seconds, confirmed);
    renderAccounts();
    if (usageTimer) cancelEvery(usageTimer);
    usageTimer = gone() ? 0 : every(() => { if (authenticated && !busy && !pendingActivation()) void refresh(); }, seconds * 1000);
  }
  function pushRefreshInterval(seconds, confirmed) { view('refresh-interval', JSON.stringify({ seconds, confirmed })); }
  async function loadSettings() {
    if (authCheck?.accessMode === 'open') return;
    // The two independent reads fly together; each still applies (or fails) on its own, in order.
    const [settings, prefs] = await Promise.all([
      request('/api/accounts/settings').catch(() => null),
      request('/api/accounts/preferences').catch(() => null),
    ]);
    // the display time zone is shared with Analytics: a page that has gone sets nothing
    if (gone()) return;
    try { applyRefreshInterval(settings?.refreshIntervalSeconds); } catch {}
    try { if (prefs && typeof prefs.timeZone === 'string') setDisplayTimeZone(prefs.timeZone); } catch {}
  }
  function renderUpdate(done = false) {
    view('update', JSON.stringify(updateViewModel(updateJob, { done })));
    renderAccounts();
  }
  async function updateStatus() {
    if (!authenticated || updateStatusPending) return;
    updateStatusPending = true;
    const observedJob = updateJob;
    const runningId = updateJob?.state === 'running' ? updateJob.id : null;
    try {
      const result = await request('/api/app-updates/status');
      if (!authenticated || updateJob !== observedJob) return;
      updateJob = result?.job || null;
      if (runningId && updateJob?.id === runningId && updateJob.state !== 'running' && updateCompletionId !== updateJob.id) {
        updateCompletionId = updateJob.id;
        // On finish the button morphs to a check for 2 s and the result appears in a toast.
        const results = Array.isArray(updateJob.results) ? updateJob.results : [];
        const count = (status) => results.filter((row) => row.status === status).length;
        const failed = count('failed') + count('restart_failed');
        const skippedN = count('skipped'), unknownN = count('unknown'), actionN = count('action_required'), heldN = count('held');
        const catalogN = results.filter(isCatalogPending).length;
        const incomplete = updateJob.state === 'failed' || failed > 0 || unknownN > 0;
        const title = incomplete ? 'App updates finished with issues' : count('updated') > 0 ? 'App updates finished' : 'App checks finished';
        toast(incomplete ? 'err' : 'ok', title, `${count('updated')} updated${catalogN ? `, ${catalogN} model${catalogN === 1 ? '' : 's'} not yet registered` : ''}, ${failed} failed, ${unknownN} unknown, ${heldN} held, ${count('current')} already current, ${count('not_installed')} not installed${actionN ? `, ${actionN} need action` : ''}${skippedN ? `, ${skippedN} skipped` : ''}. Details under Accounts & Settings.`, 7000);
        renderUpdate(true);
        cancelLater(updateDoneTimer);
        updateDoneTimer = schedule(() => renderUpdate(false), 2000);
        return;
      }
      renderUpdate(false);
    } catch { renderUpdate(false); } finally { updateStatusPending = false; }
  }
  /** GET /api/auth/setup: the session lifetime, which behind the LAN proxy is told only to a signed-in browser. */
  async function loadAuthSetup() {
    try {
      const setup = await request('/api/auth/setup');
      if (Number.isInteger(setup?.sessionTimeoutHours) && setup.sessionTimeoutHours > 0) { learnSessionHours(setup.sessionTimeoutHours); renderAccounts(); }
    } catch {}
  }
  /** GET /api/auth/check again; the copy in hand stays when it cannot be read. */
  async function refreshAuthCheck() {
    try { const status = await request('/api/auth/check'); if (status && typeof status === 'object') authCheck = status; } catch {}
  }
  /** Drop everything this session read (bridge.js signOut and sessionEnded); its work ends with it. */
  function forgetSession() {
    ended = true;
    lifetime?.abort();
    authenticated = false;
    refreshGeneration++; openDetailsId = ''; pushed.clear();
    data = null; serverData = null; profiles = []; antigravityInventory = null; antigravityAuto = null;
    claudeOpen.reset();
    accounts.reset();
  }
  /** The server no longer knows this browser's session: say whether it ran out, ended early or was revoked. */
  function sessionEnded(reason = endedReason(lastSeenAt(storage), sessionHours, now()) || 'ended') {
    if (!authenticated) return;
    forgetSignIn(storage);
    forgetSession();
    emit('signed-out', JSON.stringify({ state: 'expired', reason }));
  }
  /**
   * Sign out. It is never held by a running save or Claude Open (`busy`), only by a switch confirmation that is waiting
   * on screen. A failure is said in the page's own words, and this browser then stays signed in.
   */
  async function signOut() {
    if (!authenticated) return { ok: false };
    if (pendingActivation()) { toast('info', 'Finish the account switch first', 'Confirm or cancel the switch, then sign out.'); return { ok: false, held: true }; }
    if (accounts.state.signin.busy === 'logout') return { ok: false };
    accounts.state.signin.busy = 'logout'; renderAccounts();
    try { await mutation('/api/auth/logout', {}); }
    catch (error) {
      accounts.state.signin.busy = ''; renderAccounts();
      // a session another browser already signed out is signed out here too
      if (error?.payload?.code !== 'session_revoked') {
        if (authenticated) { const t = signOutFailureText(error); failure(t.body, t.title); }
        return { ok: false };
      }
    }
    if (!authenticated) return { ok: true };
    forgetSignIn(storage);
    forgetSession();
    emit('signed-out', JSON.stringify({ state: 'default', reason: '' }));
    return { ok: true };
  }

  // ---------------------------------------------------------------- actions (bridge.js ccsDashboardAction)
  async function run(action, value) {
    try {
      if (action === 'activation-cancel') { const agy = antigravityConfirmation.hasPending(); if ((agy ? antigravityConfirmation : activationConfirmation).cancel()) toast('info', 'Switch canceled', `Running ${agy ? 'Antigravity' : 'Codex'} programs were left unchanged.`); return; }
      if (action === 'activation-confirm') { if (authenticated) await (antigravityConfirmation.hasPending() ? antigravityConfirmation : activationConfirmation).confirm(); return; }
      if (action === 'install-app') { await promptInstall(); renderAccounts(); return; }
      // One entry per Details session: a second row tapped while it is open swaps in place without stacking entries.
      if (action === 'details') { if (!openDetailsId) overlay.push(); renderDetails(value); return; }
      if (action === 'details-closed') { openDetailsId = ''; view('details', ''); overlay.consume(); return; }
      if (!authenticated) return;
      // Accounts & Settings: a change waits while an account-switch confirmation is pending.
      if (MUTATING_ACTIONS.has(action) && pendingActivation()) { toast('info', 'Finish the account switch first', 'Confirm or cancel the switch, then try again.'); return; }
      if (await accounts.handle(action, value)) return;
      if (busy || pendingActivation()) return;
      if (action === 'refresh') { await refresh(true); return; }
      if (action === 'launch') {
        const [id, target] = value.split(':');
        // the profile ids come from the server's inventory (GET /api/claude/desktop-profiles), never a fixed list
        const profile = profiles.find((row) => row?.id === id);
        if (!profile || !PROFILE_ID.test(id) || !['mac', 'windows'].includes(target)) throw new Error('Unknown Claude profile.');
        const launcher = profile[target];
        if (target === 'windows' && launcher?.canOpen !== true) {
          if (platform === 'windows' && launcher?.launchUri === `ccs-claude://launch/${id}`) { open(launcher.launchUri, true); return; }
          throw new Error('Remote Windows launcher is unavailable.');
        }
        // An Open of this profile that is already running (here, in a tray or another browser) is followed, never sent again.
        if (claudeOpen.running(id) || openProgress(null, profiles).has(id)) { toast('info', 'Claude is already opening', 'Its progress shows on the row.'); return; }
        let outcome;
        setBusy(true);
        try { outcome = await claudeOpen.start(id, target); }
        finally { setBusy(false); }
        if (outcome === 'opened' && authenticated) toast('info', `Opening Claude on ${target === 'mac' ? 'Mac' : 'Windows'}`, 'It opens in its own desktop profile.');
        return;
      }
      if (action === 'activate') {
        refreshGeneration++;
        const account = data?.accounts?.find((row) => row.capabilities?.codexProfile === value);
        if (!account || account.isActive) return;
        await activationConfirmation.begin(value); return;
      }
      if (action === 'antigravity-activate') {
        const account = antigravityModel().antigravityAccounts.find((row) => row.profile === value);
        if (!account?.canActivate) { render(); failure('Ubuntu activation is unavailable until this login and the native runtime are verified.', 'Antigravity'); return; }
        refreshGeneration++;
        await antigravityConfirmation.begin(value); return;
      }
      if (['antigravity-automatic', 'antigravity-threshold', 'antigravity-pool', 'antigravity-cooldown'].includes(action)) {
        const model = antigravityModel();
        const seconds = Number(value);
        // the cooldown is part of the same existing PUT /api/antigravity/auto-switch (60 to 3600 seconds)
        const patch = action === 'antigravity-cooldown'
          ? (model.antigravitySettingsAvailable && Number.isInteger(seconds) && seconds >= 60 && seconds <= 3600 ? { cooldownSeconds: seconds } : null)
          : antigravitySettingsPatch(model, action, value);
        if (!patch) { render(); failure('Antigravity controls need verified Ubuntu support, two accounts and a fresh reported quota pool.', 'Antigravity'); return; }
        refreshGeneration++;
        setBusy(true);
        try {
          const status = await mutation('/api/antigravity/auto-switch', patch, 'PUT');
          if (!validAntigravityAuto(status)) throw new Error('Automatic switching settings could not be confirmed. Refresh before trying again.');
          antigravityAuto = status; render();
          if (action === 'antigravity-automatic') toast('info', status.enabled ? `Antigravity auto-switch on at ${status.thresholdUsedPercent}% used` : 'Antigravity auto-switch off',
            status.enabled ? 'The active login switches when its watched pool reaches the threshold.' : 'Logins only switch when you press Activate.');
        } finally { setBusy(false); }
        return;
      }
      if (action === 'automatic') {
        refreshGeneration++;
        setBusy(true);
        try {
          const status = await mutation('/api/codex/profiles/auto-switch', { enabled: value === 'true' }, 'PUT'); if (data) data.codexAutoSwitch = status; render();
          // The toast repeats what the server confirmed, not what was asked for.
          const on = status?.enabled === true, used = Number.isInteger(status?.thresholdPercent) ? 100 - status.thresholdPercent : null;
          toast('info', on ? `Codex auto-switch on${used !== null ? ` at ${used}% used` : ''}` : 'Codex auto-switch off',
            on ? 'The active account switches when its weekly or 5-hour window reaches the threshold.' : 'Accounts only switch when you press Activate.');
        }
        finally { setBusy(false); }
        return;
      }
      if (action === 'threshold') {
        refreshGeneration++;
        const used = Number(String(value).replace('%', ''));
        if (!Number.isInteger(used) || used < 1 || used > 99) return;
        setBusy(true);
        try { const status = await mutation('/api/codex/profiles/auto-switch', { thresholdPercent: 100 - used }, 'PUT'); if (data) data.codexAutoSwitch = status; render(); }
        finally { setBusy(false); }
        return;
      }
      if (action === 'refresh-interval') {
        const seconds = parseIntervalLabel(value) ?? Number(String(value).replace(/s$/, ''));
        if (!Number.isInteger(seconds) || seconds < 30 || seconds > 3600) return;
        setBusy(true);
        try { const result = await mutation('/api/accounts/settings', { refreshIntervalSeconds: seconds }, 'PUT'); applyRefreshInterval(result.refreshIntervalSeconds); toast('ok', 'Refresh interval saved', `Usage refreshes every ${intervalLabel(result.refreshIntervalSeconds)}.`); }
        finally { setBusy(false); }
        return;
      }
      if (action === 'update-apps') {
        try {
          const result = await mutation('/api/app-updates/start', {}); updateJob = result.job;
        } catch (error) {
          const job = error?.payload?.job;
          if (error?.status !== 409 || !job || typeof job.id !== 'string' || !UPDATE_JOB_ID.test(job.id) || job.state !== 'running' || !Number.isFinite(Date.parse(job.startedAt)) || !Array.isArray(job.results) || job.results.length > 40) throw error;
          updateJob = job;
        }
        renderUpdate(false); return;
      }
      if (action === 'update-cancel') {
        const result = await mutation('/api/app-updates/cancel', {}); updateJob = result.job;
        renderUpdate(false); return;
      }
    } catch (error) {
      if (data && (action === 'threshold' || action === 'automatic' || action.startsWith('antigravity-'))) render();
      if (action === 'refresh-interval') pushRefreshInterval(refreshIntervalSeconds, refreshSettingsKnown);
      if (authenticated) failure(error?.message || 'Unable to complete the action.');
    }
  }

  /** The frame's Back over an open overlay: it closes the top-most one (bridge.js popstate). */
  function back() {
    if (activationDialogOpen) { void run('activation-cancel', ''); return true; }
    if (openDetailsId) { openDetailsId = ''; view('details', ''); return true; }
    return false;
  }

  return {
    /**
     * The frame is signed in: read everything bridge.js read when it started in the frame's iframe (checkSession,
     * loadSettings, refresh, updateStatus, afterSignIn), the independent reads together, then arm its timers.
     * `check` is the frame session's GET /api/auth/check (it read it to sign in), `username` its user.
     */
    async start({ check = null, username: name = '' } = {}) {
      if (started || stopped) return;
      started = true;
      authenticated = true;
      username = typeof name === 'string' ? name : '';
      authCheck = check && typeof check === 'object' ? check : null;
      // what the frame's session learned while signing in (frame-session.mjs keeps it in the same storage)
      sessionHours = rememberedSessionHours(storage) || sessionHours;
      if (!authCheck) await refreshAuthCheck();
      else void refreshAuthCheck().then(() => renderAccounts());
      if (gone()) return;
      // Behind the LAN proxy the session length is only told to a signed-in browser (bridge.js checkSession).
      if (authCheck?.authenticated === true) void loadAuthSetup();
      const open = authCheck?.accessMode === 'open';
      const settings = loadSettings();
      await Promise.all([refresh(false, false, settings), ...(open ? [] : [updateStatus()])]);
      if (gone()) return;
      if (!open) { try { await accounts.migrateLocalHidden(); } catch {} }
      if (currentPage === 'accounts' && !open) await accounts.loadAll();
      if (gone()) return;
      applyRefreshInterval(refreshIntervalSeconds, refreshSettingsKnown);
      intervals.push(every(() => { activationConfirmation.expire(); antigravityConfirmation.expire(); }, 1_000));
      intervals.push(every(() => { if (authenticated && updateJob?.state === 'running') void updateStatus(); }, 3_000));
      intervals.push(every(() => { if (authenticated && updateJob?.state !== 'running') void updateStatus(); }, 30_000));
      // "Updated 1m ago" and the reset countdowns stay current between refreshes.
      intervals.push(every(() => { if (authenticated && !refreshing) renderChrome(false); }, 15_000));
      intervals.push(every(() => { if (authenticated && !refreshing && !busy) render(); }, 60_000));
    },
    /** The tab on screen (frame.rs `show`): Accounts & Settings reads its own blocks when it comes on screen. */
    page(id) {
      if (stopped || typeof id !== 'string') return;
      const page = id === 'accounts' ? 'accounts' : id === 'home' ? 'home' : 'other';
      if (page === currentPage) return;
      currentPage = page;
      // the last page id, so a fresh sign-in in the classic app can reopen it (bridge.js navigate)
      if (page !== 'other') { try { storage?.setItem('aac-last-page', page); } catch {} }
      if (page === 'accounts' && authenticated) { renderAccounts(); if (authCheck?.accessMode !== 'open') { void accounts.loadAll(); void updateStatus(); } }
    },
    /** One of the page's action kinds (frame-seam.mjs PAGE_ACTIONS); false, and nothing run, for any other. */
    act(kind, value) {
      if (stopped || typeof kind !== 'string' || !DASHBOARD_ACTIONS.has(kind)) return false;
      void run(kind, typeof value === 'string' ? value : '');
      return true;
    },
    /** The frame's own commands: Refresh and Update apps (frame.rs). */
    command(action) {
      if (stopped || !FRAME_COMMANDS.has(action)) return false;
      void run(action, '');
      return true;
    },
    signOut,
    back,
    /** The window came back on screen or online: Update apps reads its state (bridge.js focus listener). */
    focused() { if (authenticated) void updateStatus(); },
    online(value) { setOnline(value); },
    /** The install prompt was captured or used up, or the display mode changed: the "Home screen app" row follows. */
    installChanged() { if (!stopped) renderAccounts(); },
    /** The frame signed out (the page is gone): nothing more is read or sent. */
    stop() {
      if (stopped) return;
      stopped = true;
      lifetime?.abort();
      authenticated = false;
      refreshGeneration++;
      claudeOpen.reset();
      accounts.reset();
      for (const id of intervals.splice(0)) cancelEvery(id);
      if (usageTimer) cancelEvery(usageTimer);
      usageTimer = 0;
      cancelLater(updateDoneTimer);
    },
    /** For tests and the kit. */
    state() {
      return { started, stopped, authenticated, busy, currentPage, openDetailsId, activationDialogOpen, pending: pendingActivation(), refreshIntervalSeconds, hasData: !!data };
    },
  };
}

let installs = 0;

/**
 * Starts the frame's dashboard: `window.aacFrameDashboard` holds the calls (a previous one is stopped) and its `id`
 * (numbered from 1, so a page drives and stops only the controller it installed, dash.rs), and every view
 * is sent as an `aac-dash` event whose detail is `{key, json}`. Details and the switch approval take one Back entry each
 * below 1280 px, as bridge.js gave them; a Back press over one closes it. With `?e2e`, `window.__aacLastAccounts` keeps
 * the last Accounts view and `window.__aacToasts` every toast (as bridge.js keeps them for the kit).
 */
export function installFrameDashboard(win = globalThis) {
  win.aacFrameDashboard?.stop?.();
  const search = String(win.location?.search || '');
  const agent = String(win.navigator?.userAgent || '');
  const e2e = /[?&]e2e\b/.test(search);
  const storage = (() => { try { return win.localStorage; } catch { return null; } })();
  const send = (key, json) => {
    try { win.dispatchEvent(new win.CustomEvent('aac-dash', { detail: { key, json } })); } catch {}
  };
  // ---- Back entries (bridge.js pushOverlayEntry, consumeOverlayEntry and its popstate branch)
  let depth = 0;
  let consuming = false;
  const below = () => (Number(win.innerWidth) || 0) < OVERLAY_HISTORY_BELOW;
  const overlay = {
    depth: () => depth,
    push() {
      if (!below()) return;
      depth++;
      try { win.history.pushState({ aac: 'overlay' }, ''); } catch {}
    },
    consume() {
      if (!below() || depth <= 0) return;
      depth--;
      // Only our own entry is taken back: after a tab change the entry on top is the frame's, and Back would undo it.
      if (win.history?.state?.aac !== 'overlay') return;
      consuming = true;
      try { win.history.back(); } catch { consuming = false; }
    },
  };
  // ---- the Settings "Home screen app" row (bridge.js currentInstallRow and promptInstall). Chromium's install prompt is
  // captured by the page itself before any module loads (leptos/index.html keeps it as `__aacInstallPrompt`).
  const installRow = () => {
    let standalone = false;
    let appleMobile = false;
    try { standalone = win.matchMedia?.('(display-mode: standalone)')?.matches === true || win.navigator?.standalone === true; } catch {}
    try { appleMobile = isAppleMobile(agent, { touchPoints: win.navigator?.maxTouchPoints ?? 0 }); } catch {}
    return installRowFor({ standalone, deferredPrompt: !!win.__aacInstallPrompt, appleMobile });
  };
  /** The `install-app` action: show the captured install prompt once, then drop it (bridge.js promptInstall). */
  const promptInstall = async () => {
    const prompt = win.__aacInstallPrompt;
    win.__aacInstallPrompt = null;
    if (!prompt || typeof prompt.prompt !== 'function') return;
    try { await prompt.prompt(); } catch {}
  };
  const controller = createFrameDashboard({
    fetch: (...args) => win.fetch(...args),
    storage,
    location: win.location,
    platform: /Windows/i.test(agent) ? 'windows' : 'mac',
    emit: send,
    copy: (value) => copyText(value, win.document, win.navigator),
    open: (url, here = false) => {
      if (here) { win.location.href = url; return; }
      try { win.open?.(url, '_blank', 'noopener'); } catch {}
    },
    installRow,
    promptInstall,
    overlay,
    onAccounts: e2e ? (vm) => { win.__aacLastAccounts = vm; } : null,
    onToast: e2e ? (t) => { (win.__aacToasts ||= []).push(t); } : null,
  });
  const onPop = () => {
    // Our own history.back() consuming a closed overlay's entry: nothing to answer.
    if (consuming) { consuming = false; return; }
    // A Back press over an open overlay closes the top-most one (the switch dialog is cancelled, Details closes).
    if (depth > 0) { depth--; controller.back(); }
  };
  const onFocus = () => controller.focused();
  const onOnline = () => controller.online(true);
  const onOffline = () => controller.online(false);
  const onInstall = () => controller.installChanged();
  let standaloneQuery = null;
  try { standaloneQuery = win.matchMedia?.('(display-mode: standalone)') ?? null; } catch {}
  win.addEventListener?.('popstate', onPop);
  win.addEventListener?.('focus', onFocus);
  win.addEventListener?.('online', onOnline);
  win.addEventListener?.('offline', onOffline);
  // the page captured or lost the install prompt (leptos/index.html), or the app was installed while open
  win.addEventListener?.('aac-install', onInstall);
  try { standaloneQuery?.addEventListener?.('change', onInstall); } catch {}
  // It starts from the frame session's check and user (frame-session.mjs): they were read to sign in.
  const start = controller.start;
  controller.start = (options = {}) => {
    let check = null;
    let username = '';
    try {
      check = win.aacFrameSession?.authCheck?.() ?? null;
      username = win.aacFrameSession?.view?.()?.username ?? '';
    } catch {}
    return start({ check, username, ...options });
  };
  const stop = controller.stop;
  controller.stop = () => {
    win.removeEventListener?.('popstate', onPop);
    win.removeEventListener?.('focus', onFocus);
    win.removeEventListener?.('online', onOnline);
    win.removeEventListener?.('offline', onOffline);
    win.removeEventListener?.('aac-install', onInstall);
    try { standaloneQuery?.removeEventListener?.('change', onInstall); } catch {}
    stop();
  };
  controller.id = ++installs;
  win.aacFrameDashboard = controller;
  return controller;
}
