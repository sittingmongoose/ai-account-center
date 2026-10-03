// Accounts & Settings view model, version 2 (Daylight Atlas, c-daylight-atlas/app-accounts.js and app-auth.js).
// Pure functions from the public API DTOs to the JSON that src/accounts.rs writes into the AcData global
// (ui/pages/accounts/ac-data.slint). Every label, every enabled or "coming" decision and every truthfulness rule
// lives here and is covered by tests/accounts-view.test.mjs; Slint only lays out and animates it.
//
// What a control may do comes from the server: `providers[]` (signIn, capabilities) in the dashboard response,
// and per account `actions` and `removeRefusal` in GET /api/accounts/registry. A control is "coming" only where
// the server has no flow for it yet (`not_implemented`, or a route that does not exist on this server); any other
// reason it is off is said in plain words. Nothing here ever shows an example value as real data.
import { PROVIDER_REGISTRY, dashboardViewModel, platformLabel, planLabel, relative, intervalLabel, run, statusWord, duration } from './view-model.mjs';
import { antigravityView } from './antigravity-data.mjs';
import { visibleUsageWindows } from './visible-usage.mjs';
import { unavailableText, jobErrorText, PROVIDER_LABELS } from './account-actions.mjs';
import { strength as passwordStrength } from './auth-view.mjs';

export const ACCOUNTS_VIEW_VERSION = 2;

/** Server routes the page binds; `true` once the route exists on the round-2 base (kept for the docs and tests). */
export const LIVE = Object.freeze({
  activate: true,            // POST /api/codex/profiles/:name/activate, POST /api/antigravity/profiles/:id/activate
  openClaude: true,          // POST /api/claude/desktop-profiles/:id/open
  antigravityPolicy: true,   // PUT /api/antigravity/auto-switch (toggle, threshold, pool, cooldown)
  codexPolicy: true,         // PUT /api/codex/profiles/auto-switch
  refreshInterval: true,     // PUT /api/accounts/settings
  add: true,                 // POST /api/accounts/add (per provider: providers[].capabilities.add)
  signInAgain: true,         // POST /api/accounts/:id/signin-again
  replaceKey: true,          // PUT /api/accounts/:id/key
  remove: true,              // POST /api/accounts/:id/remove (confirmation token)
  restore: true,             // POST /api/accounts/trash/:trashId/restore (confirmation token)
  purgeNow: true,            // POST /api/accounts/trash/:trashId/purge (token + typed DELETE)
  openApp: true,             // POST /api/accounts/:id/open (Cursor)
  recheck: true,             // POST /api/accounts/:id/recheck
  visibilityServer: true,    // PUT /api/accounts/visibility
  passwordChange: true,      // POST /api/auth/password
  otherBrowsers: true,       // GET /api/auth/session, POST /api/auth/sessions/revoke-others
  devices: true,             // GET /api/auth/devices, DELETE /api/auth/devices/:id, POST /api/auth/devices/revoke-all
  network: true,             // GET and PUT /api/auth/network
});

/**
 * Per provider: how it signs in (the section's kind line and the rows' source column), its row action slots
 * and the footer's how-to line. `slots` and `actsMin` keep every row's actions in fixed, aligned columns.
 * Qwen and the OpenCode console wallet are browser sessions, never API keys (CONTRACT-registry-lifecycle 7).
 */
export const ACCOUNT_KINDS = {
  claude: { kind: 'desktop', kindLabel: 'Desktop profile', icon: 'laptop', src: 'Claude desktop profile', slots: 3, actsMin: 206,
    how: 'Each account is its own Claude desktop profile; open it on Mac or Windows to sign in.' },
  codex: { kind: 'device', kindLabel: 'Device-code sign-in', icon: 'code', src: 'Codex CLI device login', slots: 3, actsMin: 262,
    how: 'Device-code sign-in: approve a short code in any browser.' },
  antigravity: { kind: 'cli', kindLabel: 'Supervised CLI login', icon: 'terminal', src: 'Antigravity CLI, supervised', slots: 3, actsMin: 262,
    how: 'The dashboard host runs and supervises the CLI login.' },
  cursor: { kind: 'app', kindLabel: 'Desktop app session', icon: 'app-window', src: 'Cursor desktop app', slots: 2, actsMin: 150,
    how: 'Sign in inside the Cursor desktop app; the dashboard reads that session.' },
  muse: { kind: 'app', kindLabel: 'Device-code sign-in', icon: 'app-window', src: 'Muse Code session', slots: 2, actsMin: 150,
    how: 'Device-code sign-in on the Mac; then Sync in the Brave extension for quota.' },
  'kimi-code': { kind: 'apikey', kindLabel: 'API key', icon: 'key', src: 'API key', slots: 2, actsMin: 156,
    how: 'Usage is read with an API key.' },
  qwen: { kind: 'browser', kindLabel: 'Console session by browser extension', icon: 'globe', src: 'Console session', slots: 2, actsMin: 156,
    how: 'Packs and plan come from the Qwen console session that the browser extension syncs.' },
  zai: { kind: 'apikey', kindLabel: 'API key', icon: 'key', src: 'API key', slots: 2, actsMin: 156,
    how: 'Usage is read with an API key.' },
  'opencode-go': { kind: 'apikey', kindLabel: 'API key, console wallet by browser session', icon: 'key', src: 'API key', slots: 2, actsMin: 156,
    how: 'API keys for usage; the console wallet needs a browser session.' },
};
/** Providers with their own sign-in flows sit in the first column on ultra-wide screens. */
const COL_A = ['claude', 'codex', 'antigravity'];
const SWITCHABLE = ['codex', 'antigravity'];

const finite = value => typeof value === 'number' && Number.isFinite(value);
const validDate = value => typeof value === 'string' && Number.isFinite(Date.parse(value));
const text = value => typeof value === 'string' ? value : '';
const STALE_MS = 30 * 60_000;
const dateTime = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
const dayFmt = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const clockFmt = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const article = word => /^[aeiou]/i.test(word) ? 'an' : 'a';
const isConsole = account => /console/i.test(text(account?.message)) || /^plan-opencode-go-console-/.test(text(account?.id));
const providerLabel = id => PROVIDER_LABELS[id] || PROVIDER_REGISTRY.find(row => row.id === id)?.label || id;

// ---------------------------------------------------------------- actions
/**
 * One fixed action slot. kind: switch | button | icon | quiet | empty. style: Button kinds. `refused` draws the
 * control dimmed but keeps it clickable: the click opens the reason under the row instead of calling the server.
 * `probe` names the control for the end-to-end harness (it is only reported while the page runs with ?e2e).
 */
export function action(fields = {}) {
  return { kind: 'button', act: '', value: '', label: '', icon: '', platform: '', style: 'default', enabled: false, coming: false, refused: false, busy: false, tip: '', probe: '', span: 1, ...fields };
}
const coming = (fields, tip) => action({ ...fields, enabled: false, coming: true, tip });
const emptySlot = () => action({ kind: 'empty' });

/** The server's facts for one provider (`providers[]` in the dashboard response), or null on an older server. */
function providerEntry(data, id) {
  return Array.isArray(data?.providers) ? data.providers.find(entry => entry?.id === id) || null : null;
}
/**
 * Whether a provider-level action is live, and if not, why. kind: add | signInAgain | replaceKey | remove | recheck.
 * Returns { live, coming, reason }.
 */
export function gate(entry, kind, provider) {
  if (!entry) return { live: false, coming: true, reason: 'This server does not list what this provider can do yet.' };
  const caps = entry.capabilities || {};
  const signIn = entry.signIn || {};
  if (caps[kind] === true) return { live: true, coming: false, reason: '' };
  if (kind === 'remove') return { live: false, coming: true, reason: `Removing ${providerLabel(provider)} accounts from the dashboard is not on this server yet.` };
  if (kind === 'recheck') return { live: false, coming: true, reason: 'Re-check is not on this server yet; readings follow the refresh interval.' };
  if (signIn.available === false && signIn.unavailableReason) {
    const why = unavailableText(signIn.unavailableReason, provider);
    if (why.text) return { live: false, coming: why.coming, reason: why.text };
  }
  if (kind === 'add') {
    if (caps.multiAccount === false && finite(entry.accountCount) && entry.accountCount > 0) return { live: false, coming: false, reason: `${providerLabel(provider)} reads one account in this version.` };
    if (finite(entry.accountCount) && entry.accountCount >= 16) return { live: false, coming: false, reason: `${providerLabel(provider)} already has the most accounts allowed.` };
  }
  return { live: false, coming: true, reason: 'Not on this server yet.' };
}
/** A control drawn from a gate: live (enabled), coming (dimmed with the caption) or off with its reason. */
function gated(fields, g, liveTip = '') {
  if (g.live) return action({ ...fields, enabled: true, tip: liveTip });
  return action({ ...fields, enabled: false, coming: g.coming, tip: g.reason });
}

/** Where the account was read and how fresh it is ("sampled 1m ago"); the status word appears only for an exception. */
function rowStatus(account, now, reg) {
  if (reg?.lifecycle?.state === 'pending_sign_in' || account?.lifecycle?.state === 'pending_sign_in') {
    return { status: 'Needs sign-in', sampled: 'waiting for the first reading', sampledTip: 'Sign in inside the app; the row fills in after its first reading' };
  }
  if (['signing_in', 'verifying'].includes(reg?.lifecycle?.state || account?.lifecycle?.state)) {
    return { status: 'Signing in', sampled: 'sign-in running', sampledTip: 'A sign-in for this account is running' };
  }
  const at = validDate(account.sampledAt) ? account.sampledAt : validDate(account.fetchedAt) ? account.fetchedAt : null;
  const normal = account.status === 'ok' || account.status === 'cached';
  const stale = !!at && now - Date.parse(at) > STALE_MS;
  return {
    status: !normal ? statusWord(account) : stale ? 'Stale' : '',
    sampled: at ? `sampled ${relative(at, now)}` : 'no reading yet',
    sampledTip: at ? `Last reading ${dateTime.format(new Date(at))}${account.status === 'cached' ? ', from the cache' : ''}` : 'No reading has arrived yet',
  };
}
function sourceLines(provider, account, reg) {
  const def = ACCOUNT_KINDS[provider];
  if (provider === 'claude') {
    const platforms = Array.isArray(account.capabilities?.claudePlatforms) ? account.capabilities.claudePlatforms : [];
    return [def.src, platforms.length ? platforms.map(platformLabel).join(' and ') : platformLabel(account.platform)];
  }
  if (provider === 'opencode-go' && isConsole(account)) return ['Console session in a browser', 'Wallet and usage'];
  if (provider === 'qwen') return [def.src, 'by browser extension'];
  const credential = reg?.credential;
  if (credential?.kind === 'aac-key') {
    return [credential.last4 ? `API key ending ${credential.last4}` : 'API key', `stored on ${platformLabel(credential.storedOn || account.platform)}`];
  }
  return [def.src, platformLabel(account.platform)];
}

/**
 * A Claude profile that is a computer's default desktop profile: marked `isDefault`, or its app-data folder is
 * named "Claude" (any case: both hosts' file systems ignore it). The server applies the same rule.
 */
export function claudeDefaultProfile(profile) {
  if (!profile || typeof profile !== 'object') return false;
  const base = path => text(path).replace(/[\\/]+$/, '').split(/[\\/]/).pop().toLowerCase();
  return [profile.mac, profile.windows].some(launcher => launcher && (launcher.isDefault === true || base(launcher.profilePath) === 'claude'));
}

/** The remove control for a row: live, refused (reason under the row on click), or coming. */
function removeControl(provider, account, reg, entry, extra = {}) {
  const fields = { kind: 'icon', act: 'remove', value: account.id, icon: 'trash', label: 'Remove', style: 'ghost', probe: `remove:${account.id}` };
  // a refused control is clickable: it opens the reason under the row (act "refuse", value "<row id>\n<code>")
  const refuse = code => action({ ...fields, act: 'refuse', value: `${account.id}\n${code}`, enabled: true, refused: true, tip: REFUSAL_TIP[code] || 'It cannot be removed now.' });
  if (extra.protectedProfile) return refuse('account_protected');
  const g = gate(entry, 'remove', provider);
  if (!g.live) return action({ ...fields, enabled: false, coming: g.coming, tip: g.reason });
  if (!reg) return action({ ...fields, enabled: false, tip: 'Checking whether it can be removed' });
  if (reg.actions?.remove !== true) {
    return coming(fields, `Removing this ${providerLabel(provider)} account from the dashboard is not on this server yet.`);
  }
  if (reg.removeRefusal) return refuse(reg.removeRefusal);
  return action({ ...fields, enabled: true, tip: REMOVE_TIP[ACCOUNT_KINDS[provider].kind] || 'Remove' });
}
const REMOVE_TIP = {
  desktop: "Remove moves this profile's Claude data to a 30-day trash on Mac and Windows.",
  device: 'Remove deletes this saved Codex login.',
  cli: 'Remove deletes this saved Antigravity login on Ubuntu.',
  apikey: 'Remove deletes the stored key from the dashboard computer.',
  browser: 'Remove stops reading this account; the browser extension keeps its session.',
  app: 'Remove stops reading this account; the app keeps its session.',
};
const REFUSAL_TIP = {
  account_active: 'This is the active account. Activate another account first.',
  account_default: 'This is the default account. Make another account the default first.',
  account_protected: "This is this computer's default Claude profile. It can't be removed here.",
  last_account: 'This is the last account of this provider.',
  activation_running: 'An account switch is running.',
  signin_running: 'A sign-in for this account is running.',
};

function rowActions(provider, account, homeRow, canSwitch, ctx) {
  const def = ACCOUNT_KINDS[provider];
  const entry = providerEntry(ctx.data, provider);
  const reg = ctx.registry.get(account.id) || null;
  switch (def.kind) {
    case 'desktop': {
      const profile = homeRow?.profile || text(account.capabilities?.claudeProfileId);
      const meta = (ctx.profiles || []).find(row => row?.id === profile) || null;
      const open = (target, label, platform) => {
        const can = target === 'mac' ? !!homeRow?.canMac : !!homeRow?.canWindows;
        return action({ act: 'launch', value: `${profile}:${target}`, label, platform, enabled: LIVE.openClaude && can && !!profile, probe: `open-${target}:${account.id}`,
          tip: can ? `Open ${text(account.email) || 'this profile'} in its own Claude profile on ${label}, to use it or sign in again` : `Open on ${label} is not set up for this profile` });
      };
      const protectedProfile = claudeDefaultProfile(meta) || reg?.removeRefusal === 'account_protected' || reg?.removeRefusal === 'account_default';
      return [open('mac', 'Mac', 'apple'), open('windows', 'Windows', 'windows'), removeControl(provider, account, reg, entry, { protectedProfile })];
    }
    case 'device': {
      const g = gate(entry, 'signInAgain', provider);
      const fields = { act: 'signin-again', value: account.id, label: 'Sign in again', icon: 'login', probe: `signin-again:${account.id}` };
      const again = homeRow?.active
        ? action({ ...fields, act: 'refuse', value: `${account.id}\naccount_active_signin`, enabled: true, refused: true, tip: 'This is the active account. Activate another account first, then sign in again.' })
        : !g.live ? gated(fields, g)
          : reg && reg.actions?.signInAgain === false ? action({ ...fields, enabled: false, tip: 'A sign-in is not possible for this account now.' })
            : action({ ...fields, enabled: true, tip: 'Run the device-code login for this profile again; its history stays.' });
      return [
        action({ kind: 'switch', act: 'activate', value: homeRow?.profile || '', enabled: LIVE.activate && !!homeRow?.canActivate, tip: homeRow?.activateHint || '', probe: `activate:${account.id}` }),
        again,
        removeControl(provider, account, reg, entry),
      ];
    }
    case 'cli': {
      const signinFields = { act: 'signin-again', value: account.id, label: 'Sign in', icon: 'login', probe: `signin-again:${account.id}` };
      const signin = !reg ? action({ ...signinFields, enabled: false, tip: 'Checking what this account can do' })
        : reg.actions?.signInAgain !== true ? action({ ...signinFields, enabled: false, tip: 'Signing in is not possible for this account now.' })
          : action({ ...signinFields, enabled: true, tip: 'Sign in from a terminal on Ubuntu; the dashboard shows the command.' });
      return [
        canSwitch || homeRow?.active
          ? action({ kind: 'switch', act: 'antigravity-activate', value: homeRow?.profile || '', enabled: LIVE.activate && !!homeRow?.canActivate, tip: homeRow?.activateHint || '', probe: `activate:${account.id}` })
          : emptySlot(),
        signin,
        removeControl(provider, account, reg, entry),
      ];
    }
    case 'apikey': {
      if (isConsole(account)) {
        const fields = { act: 'signin', value: account.id, label: 'Sign in', icon: 'login', probe: `signin:${account.id}` };
        const live = reg ? reg.actions?.signInAgain === true : false;
        return [live ? action({ ...fields, enabled: true, tip: 'Open the OpenCode console in the browser with the extension and sign in there.' })
          : action({ ...fields, enabled: false, tip: reg ? 'Signing in to the console wallet is not available here.' : 'Checking what this account can do' }),
        removeControl(provider, account, reg, entry)];
      }
      const g = gate(entry, 'replaceKey', provider);
      const fields = { act: 'replace-key', value: account.id, label: 'Replace key', icon: 'key', style: 'accent-line', probe: `replace-key:${account.id}` };
      let replace;
      if (!g.live) replace = gated(fields, g);
      else if (!reg) replace = action({ ...fields, enabled: false, tip: 'Checking what this account can do' });
      else if (reg.credential?.kind && reg.credential.kind !== 'aac-key') replace = action({ ...fields, enabled: false, tip: `This account reads a key another app saved on ${platformLabel(account.platform)}. Add a key here to manage it from the dashboard.` });
      else if (reg.actions?.replaceKey !== true) replace = action({ ...fields, enabled: false, tip: 'Replacing this key is not possible now.' });
      else replace = action({ ...fields, enabled: true, tip: 'Store a new key; it is checked first and the old one stays if it is refused.' });
      return [replace, removeControl(provider, account, reg, entry)];
    }
    case 'browser': {
      const fields = { act: 'signin', value: account.id, label: 'Sign in', icon: 'login', probe: `signin:${account.id}` };
      const g = gate(entry, 'signInAgain', provider);
      const signin = g.live && (!reg || reg.actions?.signInAgain !== false)
        ? action({ ...fields, enabled: !!reg, tip: 'Open the console in the browser with the extension and sign in there; then re-check.' })
        : gated(fields, g.live ? { live: false, coming: false, reason: 'Signing in is not possible for this account now.' } : g);
      return [signin, removeControl(provider, account, reg, entry)];
    }
    case 'app':
      return [action({ kind: 'quiet', label: 'Session from the app' }), removeControl(provider, account, reg, entry)];
    default:
      return [];
  }
}

function footActions(provider, accounts, ctx) {
  const def = ACCOUNT_KINDS[provider];
  const entry = providerEntry(ctx.data, provider);
  const label = providerLabel(provider);
  const flowOpen = !!ctx.flows?.[provider];
  const add = gate(entry, 'add', provider);
  switch (def.kind) {
    case 'desktop': case 'device': case 'cli':
      return [gated({ act: 'add', value: provider, label: 'Add account', icon: 'plus', style: 'primary', probe: `add:${provider}`, enabled: !flowOpen }, add,
        provider === 'claude' ? 'Create a new Claude desktop profile on Mac and Windows, then sign in inside the app.'
          : provider === 'codex' ? 'Start a device-code sign-in for a new Codex profile; you approve a short code in any browser.'
            : 'Run the Antigravity CLI login on Ubuntu under supervision.')];
    case 'app': case 'browser': {
      const first = accounts[0] || null;
      const reg = first ? ctx.registry.get(first.id) : null;
      const signInGate = first ? gate(entry, 'signInAgain', provider) : add;
      const signin = gated({ act: 'session-signin', value: provider, label: 'Sign in', icon: def.kind === 'browser' ? 'globe' : 'login', probe: `session-signin:${provider}` }, signInGate,
        provider === 'muse' ? 'Start a device-code sign-in on the Mac; approve a code in any browser, then Sync in the Brave extension.'
          : def.kind === 'browser' ? 'Open the console in the browser with the extension and sign in there; then re-check.' : `Open ${label} on its computer and sign in there; then re-check.`);
      const recheckGate = first ? (reg ? (reg.actions?.recheck === true ? { live: true } : { live: false, coming: false, reason: 'Re-check is not available for this account.' }) : gate(entry, 'recheck', provider))
        : { live: false, coming: false, reason: `Sign in first; there is no ${label} account to check yet.` };
      const recheck = gated({ act: 'recheck', value: first?.id || provider, label: 'Re-check', icon: 'refresh', probe: `recheck:${provider}`, busy: ctx.busyAct === `recheck:${first?.id}` }, recheckGate, 'Read the session again now.');
      return [signin, recheck];
    }
    case 'apikey': {
      const hasKey = accounts.some(account => !isConsole(account));
      const fields = hasKey
        ? { act: 'add-key', value: provider, label: `Add another ${label} key`, icon: 'plus', style: 'ghost', probe: `add-key:${provider}` }
        : { act: 'add-key', value: provider, label: `Add ${article(label)} ${label} key`, icon: 'key', style: 'primary', probe: `add-key:${provider}` };
      return [gated(fields, add, 'Each key becomes its own account; the key is stored on the dashboard computer and never shown again.')];
    }
    default:
      return [];
  }
}

// ---------------------------------------------------------------- the inline flows (add, sign in again, keys, guides)
const STEPS = {
  'job-add': ['Name the profile', 'Approve the code', 'Signed in'],
  'job-again': ['Approve the code', 'Signed in'],
  'claude-add': ['Name the profile', 'Create it', 'Sign in'],
  'key-add': ['Paste the key', 'Check', 'Stored'],
  'key-replace': ['Paste the key', 'Check', 'Stored'],
  guide: ['Open', 'Sign in', 'Re-check'],
  purge: ['Type DELETE', 'Deleted'],
};
const btn = (act, value, label, fields = {}) => action({ act, value, label, enabled: true, probe: `${act}:${value}`, ...fields });
const expiresLine = (iso, now) => validDate(iso) ? `Code expires at ${clockFmt.format(new Date(iso))}${Date.parse(iso) - now < 120_000 ? ' (soon)' : ''}` : '';

/**
 * The flow panel under a provider section. `f` is bridge.js's flow state:
 * { type, step, provider, accountId?, email?, name?, job?, error?: {title, body}, result?, guide?, fallback?, busy? }
 */
export function flowView(provider, f, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const closed = { open: false, key: '', title: '', body: '', steps: [], cur: 0, inputKind: '', inputLabel: '', inputPlaceholder: '', inputSeed: '', inputPassword: false, labelField: false,
    codeShown: false, codeUrl: '', codeText: '', codeExpires: '', codeInput: false, waiting: '', done: '', error: '', errorBody: '', note: '', actions: [] };
  if (!f) return closed;
  const label = providerLabel(provider);
  const v = { ...closed, open: true, key: `${f.type}:${f.accountId || ''}:${f.serial || 0}` };
  const cancel = btn('flow-cancel', provider, 'Cancel', { style: 'ghost' });
  const close = btn('flow-cancel', provider, 'Close', { style: 'ghost' });
  const doneBtn = btn('flow-done', provider, 'Done', { style: 'default' });
  const err = () => { if (f.error) { v.error = text(f.error.title); v.errorBody = text(f.error.body); } };
  const trustNote = ctx.trustNote || '';
  switch (f.type) {
    case 'job-add': case 'job-again': {
      v.steps = STEPS[f.type];
      const again = f.type === 'job-again';
      const job = f.job || null;
      const who = again ? text(f.email) || 'this account' : text(f.name) || 'the new profile';
      if (f.step === 'name') {
        v.cur = 0;
        v.title = `Add ${article(label)} ${label} account`;
        v.body = "Name the profile. It keeps this account's sign-in apart from the others.";
        Object.assign(v, { inputKind: 'name', inputLabel: 'Profile name', inputPlaceholder: 'codex-2', inputSeed: text(f.name) });
        v.actions = [btn('flow-submit', provider, 'Continue', { style: 'primary', busy: !!f.busy, enabled: !f.busy }), cancel];
        err();
        return v;
      }
      if (f.step === 'starting' || !job) {
        v.cur = again ? 0 : 1;
        v.title = again ? `Sign in again as ${who}` : `Signing in ${who}`;
        v.waiting = `Starting the ${label} sign-in on ${provider === 'muse' ? 'the Mac' : 'Ubuntu'}`;
        v.actions = [cancel];
        return v;
      }
      const state = job.state;
      if (state === 'succeeded') {
        v.cur = v.steps.length;
        const email = text(job.result?.email);
        v.done = again ? 'Signed in again' : `${text(job.profileName) || who} is signed in${email ? ` as ${email}` : ''}`;
        v.body = again ? `${who} refreshes on the next cycle.` : `It joins the ${label} list as an inactive account; its first reading arrives with the next refresh.`;
        v.actions = [doneBtn];
        return v;
      }
      if (['failed', 'expired', 'cancelled'].includes(state)) {
        v.cur = again ? 0 : 1;
        v.title = state === 'cancelled' ? 'Sign-in cancelled' : 'The sign-in did not finish';
        v.error = state === 'cancelled' ? 'Nothing was saved.' : jobErrorText(job);
        v.actions = [btn('flow-retry', provider, 'Start again', { style: 'primary' }), close];
        return v;
      }
      v.cur = again ? 0 : 1;
      if (state === 'verifying') {
        v.title = again ? `Sign in again as ${who}` : `Signing in ${who}`;
        v.waiting = 'Approved. Saving the sign-in on the dashboard computer';
        v.actions = [cancel];
        return v;
      }
      const ver = job.verification;
      v.title = job.kind === 'supervised-cli' ? 'Sign in with Google in a browser' : 'Approve the sign-in in any browser';
      if (ver && text(ver.url)) {
        v.codeShown = true;
        v.codeUrl = text(ver.url);
        v.codeText = text(ver.userCode);
        v.codeExpires = expiresLine(ver.expiresAt, now);
        v.body = job.kind === 'supervised-cli'
          ? 'Open the sign-in page the CLI prepared, choose the account, then paste the code Google shows.'
          : again ? 'Open the verification page and enter the code. The profile and its history stay as they are.'
            : `Open the verification page, sign in to the account you want as ${who}, and enter this code.`;
      } else {
        v.body = 'The page and code are only shown on a trusted connection. Open this page on the dashboard computer, or turn on local network trust there.';
      }
      if (state === 'awaiting_code') {
        Object.assign(v, { codeInput: true, inputKind: 'code', inputLabel: 'Code from the sign-in page', inputPlaceholder: 'Paste the code', inputSeed: '' });
        v.actions = [btn('flow-submit', provider, 'Submit code', { style: 'primary', busy: !!f.busy, enabled: !f.busy }),
          ...(v.codeUrl ? [btn('flow-open-url', provider, 'Open sign-in page', { icon: 'external' })] : []), cancel];
      } else {
        v.waiting = `Waiting for approval${job.kind === 'device-code' ? '' : ' in the browser'}`;
        v.actions = [...(v.codeUrl ? [btn('flow-open-url', provider, 'Open verification page', { icon: 'external', style: 'primary' })] : []),
          ...(v.codeText ? [btn('flow-copy', provider, 'Copy code', { icon: 'copy' })] : []), cancel];
      }
      v.note = trustNote;
      err();
      return v;
    }
    case 'claude-add': {
      v.steps = STEPS[f.type];
      if (f.step === 'name') {
        v.title = 'Add a Claude account';
        v.body = 'Each Claude account gets its own desktop profile on Mac and Windows, so several stay signed in side by side.';
        Object.assign(v, { inputKind: 'name', inputLabel: 'Profile name', inputPlaceholder: 'work-2', inputSeed: text(f.name) });
        v.actions = [btn('flow-submit', provider, 'Create profile', { style: 'primary', busy: !!f.busy, enabled: !f.busy }), cancel];
        err();
        return v;
      }
      if (f.step === 'creating') {
        v.cur = 1;
        v.title = `Creating ${text(f.name)}`;
        v.waiting = 'Creating the profile and its launchers on Mac and Windows';
        return v;
      }
      v.cur = 2;
      v.done = `Profile ${text(f.name)} created on Mac and Windows`;
      v.body = `Open Claude (${text(f.name)}) from Applications on the Mac or the Start menu on Windows and sign in there. The row says "Needs sign-in" until its first reading confirms the account.`;
      v.actions = [doneBtn];
      return v;
    }
    case 'key-add': case 'key-replace': {
      v.steps = STEPS[f.type];
      const replace = f.type === 'key-replace';
      if (f.step === 'key') {
        v.title = replace ? `Replace the ${label} API key` : f.second ? `Add another ${label} key` : `Add ${article(label)} ${label} API key`;
        v.body = 'The key is stored on the dashboard computer and never shown again, here or anywhere else.';
        Object.assign(v, { inputKind: 'key', inputLabel: 'API key', inputPlaceholder: 'Paste the key', inputPassword: true, labelField: !replace });
        v.actions = [btn('flow-submit', provider, replace ? 'Replace key' : 'Save key', { style: 'primary', busy: !!f.busy, enabled: !f.busy }), cancel];
        v.note = trustNote;
        err();
        return v;
      }
      if (f.step === 'checking') {
        v.cur = 1;
        v.title = 'Checking the key';
        v.waiting = `Asking ${label} for usage with the new key`;
        return v;
      }
      v.cur = 3;
      const last4 = text(f.result?.account?.credential?.last4);
      v.done = `${replace ? 'Key replaced' : 'Key stored'}${last4 ? `, ending in ${last4}` : ''}`;
      v.body = f.result?.check === 'unverified'
        ? `${label} could not be reached to check it, so it was kept; the row shows its readings after the next refresh.`
        : 'Only the dashboard computer keeps it; this page no longer has it.';
      v.actions = [doneBtn];
      return v;
    }
    case 'guide': {
      v.steps = STEPS.guide;
      const g = f.guide || {};
      v.title = `Sign in to ${label}`;
      if (g.kind === 'open-app') {
        const where = (Array.isArray(g.platforms) ? g.platforms : []).map(platformLabel).join(' or ') || 'its computer';
        v.body = `Open ${label} on ${where} and sign in there, then re-check. The dashboard reads that session.`;
        v.actions = [...(Array.isArray(g.platforms) ? g.platforms : []).filter(p => ['mac', 'windows'].includes(p)).map(p =>
          btn('flow-open-app', `${provider}:${p}`, `Open on ${platformLabel(p)}`, { platform: p === 'mac' ? 'apple' : 'windows', busy: ctx.busyAct === `open:${provider}:${p}` }))];
      } else {
        const where = platformLabel(g.platform || (provider === 'qwen' ? 'windows' : 'mac'));
        v.body = provider === 'qwen'
          ? `On ${where}, open the Qwen console in the browser that has the AI Account Center extension and sign in there. The extension syncs the session; then re-check.`
          : `On ${where}, open the console in the browser that has the AI Account Center extension and sign in there. The extension syncs the session; then re-check.`;
      }
      v.cur = f.checked ? 2 : 1;
      v.actions.push(btn('flow-recheck', provider, 'Re-check', { icon: 'refresh', style: 'primary', busy: !!f.busy, enabled: !f.busy && !!f.accountId }));
      v.actions.push(close);
      if (f.found) { v.cur = 3; v.done = 'Session found'; v.body = 'Readings continue on the normal refresh interval.'; v.actions = [doneBtn]; }
      err();
      return v;
    }
    case 'purge': {
      v.steps = STEPS.purge;
      const who = text(f.label) || 'this profile';
      if (f.step === 'asking') {
        v.title = `Delete ${who} now?`;
        v.waiting = 'Checking what deleting it does';
        v.actions = [cancel];
        return v;
      }
      if (f.step === 'done') {
        v.cur = 2;
        v.done = `${who} deleted for good`;
        v.body = 'Its Claude data is gone on Mac and Windows. The trash no longer lists it.';
        v.actions = [doneBtn];
        return v;
      }
      v.title = `Delete ${who} now?`;
      const effects = (Array.isArray(f.effects) ? f.effects : []).filter(e => typeof e === 'string').join(' ');
      v.body = effects || 'Its Claude data is deleted for good on Mac and Windows. This cannot be undone.';
      Object.assign(v, { inputKind: 'purge', inputLabel: 'Type DELETE to confirm', inputPlaceholder: 'DELETE', inputSeed: '' });
      v.actions = [btn('flow-submit', provider, 'Delete now', { style: 'danger-solid', busy: !!f.busy, enabled: !f.busy }), cancel];
      err();
      return v;
    }
    default:
      return closed;
  }
}

// ---------------------------------------------------------------- the line under a row (remove, refusals) and the trash
/**
 * `l` is bridge.js's line state for a row: { kind: 'asking'|'confirm'|'refused'|'removing', token?, effects?, code? }.
 */
export function lineView(rowId, label, l, ctx = {}) {
  const none = { shown: false, kind: '', icon: '', lead: '', text: '', actions: [] };
  if (!l) return none;
  const keep = btn('line-cancel', rowId, 'Keep', { style: 'ghost' });
  if (l.kind === 'refused') {
    return { shown: true, kind: 'refuse', icon: 'alert', lead: label, text: REFUSAL_LINE[l.code] || 'cannot be removed now.', actions: [btn('line-cancel', rowId, 'OK')] };
  }
  if (l.kind === 'asking' || l.kind === 'removing') {
    return { shown: true, kind: 'busy', icon: 'trash', lead: label, text: l.kind === 'removing' ? (ctx.restore ? 'Restoring…' : 'Removing…') : 'Checking what removing it does…', actions: [] };
  }
  const effects = (Array.isArray(l.effects) ? l.effects : []).filter(e => typeof e === 'string').join(' ');
  const verb = ctx.restore ? 'Restore' : 'Remove';
  return {
    shown: true, kind: 'confirm', icon: ctx.restore ? 'rotate' : 'trash', lead: `${verb} ${label}?`,
    text: effects || (ctx.restore ? 'It moves back from the trash.' : 'It stops being read here.'),
    actions: [keep, btn(ctx.restore ? 'restore-commit' : 'remove-commit', rowId, verb, { style: ctx.restore ? 'accent-line' : 'danger-solid' })],
  };
}
const REFUSAL_LINE = {
  account_active: 'is the active account, so it cannot be removed. Activate another account first.',
  account_active_signin: 'is the active account, so it cannot sign in again. Activate another account first, then sign in again.',
  account_default: 'is the saved default. Make another account the default first.',
  account_protected: "is this computer's default Claude profile. It can't be removed here.",
  last_account: 'is the last account of this provider, so it stays.',
  activation_running: 'cannot be removed while an account switch runs. Try again when it finishes.',
  signin_running: 'cannot be removed while its sign-in runs. Finish or cancel the sign-in first.',
  app_running: 'is open in Claude on one of the computers. Quit it there first.',
  app_state_unknown: 'could not be checked: whether Claude is open is unknown. Try again later.',
};

function trashRows(entries, ctx, now) {
  return (Array.isArray(entries) ? entries : []).filter(e => e && e.provider === 'claude').map(entry => {
    const deleting = entry.state === 'deleting';
    const when = validDate(entry.purgeAfter) ? dayFmt.format(new Date(entry.purgeAfter)) : '';
    const restore = deleting
      ? action({ act: 'restore', value: entry.trashId, label: 'Restore', icon: 'rotate', enabled: false, tip: 'It is being deleted for good.', probe: `restore:${entry.trashId}` })
      : action({ act: 'restore', value: entry.trashId, label: 'Restore', icon: 'rotate', enabled: true, tip: 'Move its Claude data back on Mac and Windows and list it again.', probe: `restore:${entry.trashId}` });
    const purge = deleting
      ? action({ act: 'purge', value: entry.trashId, label: 'Delete now', style: 'ghost', enabled: false, tip: 'It is being deleted for good.', probe: `purge:${entry.trashId}` })
      : action({ act: 'purge', value: entry.trashId, label: 'Delete now', style: 'ghost', enabled: LIVE.purgeNow, coming: !LIVE.purgeNow, tip: LIVE.purgeNow ? 'Delete its Claude data for good on Mac and Windows now. Type DELETE to confirm.' : 'Deleting from the trash before the 30 days is not on this server yet; the trash empties itself.', probe: `purge:${entry.trashId}` });
    return {
      id: entry.trashId, label: text(entry.label) || 'Claude profile',
      sub: deleting ? 'Deleting for good' : `${validDate(entry.trashedAt) ? `moved to the trash ${relative(entry.trashedAt, now)}` : 'in the trash'}${when ? ` · deleted for good ${when}` : ''}`,
      actions: [restore, purge],
      line: lineView(entry.trashId, text(entry.label) || 'this profile', ctx.lines?.[`trash:${entry.trashId}`], { restore: true }),
    };
  });
}

// ---------------------------------------------------------------- the Antigravity policy box and the policies
const COOLDOWNS = [300, 900, 1800, 3600];
const minutes = seconds => seconds % 60 === 0 ? `${seconds / 60} min` : intervalLabel(seconds);
/** Reported quota pools of the Antigravity accounts, labelled as the provider reports them. */
function reportedPoolLabels(accounts) {
  const pools = new Map();
  for (const account of accounts) for (const w of visibleUsageWindows('antigravity', account.windows)) {
    if (typeof w.poolId === 'string' && w.poolId && !pools.has(w.poolId)) pools.set(w.poolId, text(w.poolLabel) || `Quota pool ${pools.size + 1}`);
  }
  return pools;
}
function antigravityPolicy(data, ctx, section, accounts) {
  const native = antigravityView(data, ctx.antigravityInventory, ctx.antigravityAuto, ctx.now);
  const status = native.antigravityAutoKnown ? ctx.antigravityAuto : null;
  const live = !!section?.auto?.shown;
  const settings = LIVE.antigravityPolicy && native.antigravitySettingsAvailable === true;
  // Pools: the selectable choices once both accounts share a fresh pool; before that the reported pools, greyed.
  let pools = native.antigravityPoolChoices.filter(choice => choice.id !== null).map(choice => ({ value: choice.label, label: choice.label, icon: '' }));
  let pool = native.antigravityPoolChoices.find(choice => choice.id === status?.requestedPoolId && choice.id !== null)?.label || '';
  if (!pools.length) {
    const reported = reportedPoolLabels(accounts);
    pools = [...reported.values()].map(label => ({ value: label, label, icon: '' }));
    pool = status?.requestedPoolId && reported.has(status.requestedPoolId) ? reported.get(status.requestedPoolId) : '';
  }
  const cooldown = status && finite(status.cooldownSeconds) ? status.cooldownSeconds : null;
  const cooldowns = COOLDOWNS.map(seconds => ({ value: String(seconds), label: minutes(seconds), icon: '' }));
  if (cooldown !== null && !COOLDOWNS.includes(cooldown)) cooldowns.push({ value: String(cooldown), label: minutes(cooldown), icon: '' });
  const toggleEnabled = settings && section.auto.available && (section.auto.enabled || section.auto.canEnable);
  return {
    shown: accounts.length > 0,
    live,
    known: !!status,
    noteStrong: !status ? 'Automatic switching status is unavailable.' : live ? '' : 'Auto-switch is off until a second account is signed in.',
    note: !status ? 'The server did not report the Antigravity policy; the controls stay off.'
      : live ? text(status.message) || 'Switching moves between the Antigravity accounts above.'
        : 'Switching moves between Antigravity accounts.',
    enabled: status?.enabled === true && live,
    toggleEnabled,
    toggleTip: !status ? 'The server did not report the Antigravity policy' : !live ? 'Starts once a second Antigravity account is signed in'
      : toggleEnabled ? '' : section.auto.message || 'Needs verified Ubuntu support and a fresh reported quota pool on both accounts',
    threshold: status ? status.thresholdUsedPercent : -1,
    min: status ? Math.min(50, status.thresholdUsedPercent) : 50,
    max: 99,
    stepEnabled: settings,
    pools, pool, poolEnabled: settings && native.antigravityPoolAvailable === true,
    cooldowns, cooldown: cooldown === null ? '' : String(cooldown), cooldownEnabled: settings,
  };
}
function policies(data, home, ag) {
  const auto = data?.codexAutoSwitch;
  const codexKnown = !!auto && finite(auto.thresholdPercent) && auto.thresholdPercent >= 0 && auto.thresholdPercent <= 100;
  const codex = home.sections.find(section => section.id === 'codex');
  const codexUsed = codexKnown ? 100 - auto.thresholdPercent : -1;
  const agSection = home.sections.find(section => section.id === 'antigravity');
  const rows = [{
    provider: 'codex', name: 'Codex', known: codexKnown, enabled: auto?.enabled === true,
    toggleEnabled: LIVE.codexPolicy && codexKnown && auto?.activationInProgress !== true,
    threshold: codexUsed, min: codexKnown ? Math.min(50, codexUsed) : 50, max: 99, stepEnabled: LIVE.codexPolicy && codexKnown,
    wait: false, sub: codexKnown ? '' : 'The server did not report the Codex policy.',
    tip: codex?.auto?.message || text(auto?.message) || '',
  }];
  rows.push({
    provider: 'antigravity', name: 'Antigravity', known: ag.known, enabled: ag.enabled, toggleEnabled: ag.toggleEnabled,
    threshold: ag.threshold, min: ag.min, max: ag.max, stepEnabled: ag.stepEnabled && ag.live,
    wait: !ag.live, sub: !ag.known ? 'The server did not report the Antigravity policy.' : ag.live ? '' : 'Off until a second account is signed in',
    tip: ag.toggleTip || agSection?.auto?.message || '',
  });
  return rows;
}

// ---------------------------------------------------------------- Update apps results
const UPDATE_HOSTS = [['mac', 'Mac', 'apple'], ['windows', 'Windows', 'windows'], ['ubuntu', 'Ubuntu', 'ubuntu']];
const RESULT = {
  updated: ['Updated', 'good'], current: ['Already current', ''], not_installed: ['Not installed', ''],
  failed: ['Failed', 'crit'], restart_failed: ['Updated, restart failed', 'crit'],
  skipped: ['Skipped: cancelled', ''],
};
// An unknown row on an unreachable host names the computer; any other unknown stays a plain word.
const unknownWord = (row, label) => text(row.message).includes('not reachable') ? `Unknown: ${label} not reachable` : 'Unknown';
export function updateResultsView(job, now = Date.now()) {
  const results = Array.isArray(job?.results) ? job.results.filter(row => row && typeof row === 'object') : [];
  const running = job?.state === 'running';
  const cancelling = running && job.cancelRequested === true;
  const count = status => results.filter(row => row.status === status).length;
  const failed = count('failed') + count('restart_failed');
  const total = finite(job?.expectedResults) && job.expectedResults > 0 ? job.expectedResults : 21;
  let headRuns;
  if (!job) headRuns = [run('No run yet. '), run('Update apps'), run(' in the header runs one.')];
  else if (running) headRuns = cancelling
    ? [run('Cancelling…', true), run(` · ${results.length} of ${total} done`)]
    : [run('Running now', true), run(` · ${results.length} of ${total} done`)];
  else {
    const when = validDate(job.finishedAt) ? job.finishedAt : job.startedAt;
    const parts = [`${results.length} ${results.length === 1 ? 'result' : 'results'}`];
    if (failed) parts.push(`${failed} failed`);
    if (count('skipped')) parts.push(`${count('skipped')} skipped`);
    if (count('unknown')) parts.push(`${count('unknown')} unknown`);
    if (job.cancelRequested === true) parts.push('cancelled');
    headRuns = [run('Last run '), run(validDate(when) ? relative(when, now) : 'time unknown', true),
      run(` · ${parts.join(', ')}`)];
  }
  const order = ['ubuntu', 'mac', 'windows'];   // the order the server runs the hosts in
  const activeIndex = running ? order.indexOf(job.activePlatform) : -1;
  const hosts = job ? UPDATE_HOSTS.map(([id, label, platform]) => {
    const rows = results.filter(row => row.platform === id);
    const items = rows.map((row, index) => {
      const [word, tone] = row.status === 'unknown' ? [unknownWord(row, label), ''] : (RESULT[row.status] || ['Unknown result', '']);
      const versions = text(row.previousVersion) && text(row.version) && row.previousVersion !== row.version ? `${row.previousVersion} to ${row.version}` : text(row.version) ? `version ${row.version}` : '';
      return {
        key: `${id}|${text(row.appId) || index}`, app: text(row.appLabel) || text(row.appId) || 'App',
        result: row.status === 'updated' && text(row.version) ? `Updated to ${row.version}` : word, tone, running: false,
        tip: [text(row.message), versions].filter(Boolean).join(' · '),
      };
    });
    if (!rows.length) {
      const index = order.indexOf(id);
      const state = running ? (index === activeIndex ? 'run' : index > activeIndex ? 'wait' : 'none') : 'none';
      items.push({ key: `${id}|state`, app: state === 'run' ? 'Checking the apps' : state === 'wait' ? 'Waiting for its turn' : 'No results from this computer',
        result: state === 'run' ? 'Running' : '', tone: state === 'run' ? 'run' : '', running: state === 'run', tip: '' });
    } else if (running && order.indexOf(id) === activeIndex) {
      items.push({ key: `${id}|more`, app: 'More apps', result: 'Running', tone: 'run', running: true, tip: '' });
    }
    return { id, label, platform, items };
  }) : [];
  return { shown: !!job, running, cancelling, headRuns, hosts };
}

// ---------------------------------------------------------------- connection, the trusted local network and sign-in
/** How this browser reaches the dashboard: https, loopback (this computer) or plain http on the network. */
export function transportOf(protocol, hostname) {
  if (protocol === 'https:') return 'https';
  const host = String(hostname || '').replace(/^\[|\]$/g, '').toLowerCase();
  if (host === 'localhost' || host === '::1' || /^127\./.test(host)) return 'loopback';
  return 'http';
}
const TRANSPORT_TEXT = {
  https: host => `HTTPS to ${host}`,
  loopback: host => `This computer, ${host}`,
  http: host => `Plain HTTP to ${host}`,
};
export const TRUSTED_NOTE = 'Local network trusted. Passwords, keys and sign-in codes cross your home network without encryption.';
/**
 * The sign-in card's network note. `check` is GET /api/auth/check ({ secureTransport, trustedLocalNetwork,
 * connection: { trusted } }); without it the note follows the address alone.
 */
export function transportNote(transport, check = null) {
  if (transport === 'https') return 'This address uses HTTPS, so the password is encrypted on its way to the dashboard.';
  if (transport === 'loopback') return 'You are on the dashboard computer itself, so the password never crosses the network.';
  if (check?.connection?.trusted === true) return TRUSTED_NOTE;
  if (check?.trustedLocalNetwork === true) return 'This address is not on your trusted local network. Sign-in works, but password changes, keys and tray pairing stay off here.';
  if (check && check.trustedLocalNetwork === false) return 'This address is plain HTTP and local network trust is off, so the password crosses your network unencrypted. Password changes, keys and tray pairing work once trust is turned on from the dashboard computer (Accounts & Settings, Dashboard sign-in).';
  return 'This address is plain HTTP, so the password crosses your network unencrypted.';
}

/** True when a GET /api/auth/network answer says whether the trust is on. */
const networkKnown = net => !!net && typeof net === 'object' && typeof net.trustLocalNetwork === 'boolean';

/** "This connection: 192.168.50.20, trusted local network" (GET /api/auth/network or /check). */
export function networkView(net, check, transport, busyAct = '') {
  const source = net || (check ? { trustLocalNetwork: check.trustedLocalNetwork === true, connection: check.connection, canTurnOn: transport === 'loopback' } : null);
  if (!source) return { known: false, on: false, line: 'Not reported', note: '', act: '', actLabel: '', actEnabled: false, tip: '', busy: false };
  const on = source.trustLocalNetwork === true;
  const peer = text(source.connection?.peer) || 'unknown';
  const here = source.canTurnOn === true || transport === 'loopback';
  const line = source.connection?.trusted === true ? `This connection: ${peer}, trusted local network`
    : here ? 'This connection: this computer'
      : transport === 'https' ? `This connection: ${peer}, encrypted`
        : `This connection: ${peer}, not trusted`;
  const note = on ? TRUSTED_NOTE
    : 'Local network trust is off. Password changes, keys, sign-in codes and tray pairing work only on the dashboard computer itself.';
  if (on) return { known: true, on, line, note, act: 'network-off', actLabel: 'Turn off', actEnabled: true, tip: 'Stop trusting the local network; it can be turned on again only from the dashboard computer.', busy: busyAct === 'network' };
  if (source.canTurnOn === true) return { known: true, on, line, note, act: 'network-on', actLabel: 'Turn on', actEnabled: true, tip: 'Trust plain HTTP from your home network and WireGuard addresses for passwords, keys and tray pairing.', busy: busyAct === 'network' };
  return { known: true, on, line, note: `${note} Turn it on there, in this section.`, act: '', actLabel: '', actEnabled: false, tip: '', busy: false };
}

const DEVICE_PLATFORM = { mac: ['apple', 'Mac'], windows: ['windows', 'Windows'] };
export function devicesView(devices, now) {
  return (Array.isArray(devices) ? devices : []).filter(d => d && typeof d.id === 'string').map(d => {
    const [glyph, where] = DEVICE_PLATFORM[d.platform] || ['', 'Unknown computer'];
    const seen = validDate(d.lastSeenAt) ? `last seen ${relative(d.lastSeenAt, now)}${text(d.lastSeenAddress) ? ` from ${d.lastSeenAddress}` : ''}`
      : validDate(d.pairedAt) ? `paired ${relative(d.pairedAt, now)}, not seen since` : 'not seen since pairing';
    return {
      id: d.id, name: text(d.name) || `${where} tray`, platform: glyph,
      sub: `${where}${text(d.appVersion) ? ` · version ${d.appVersion}` : ''} · ${seen}`,
      tip: validDate(d.pairedAt) ? `Paired ${dateTime.format(new Date(d.pairedAt))}${validDate(d.idleExpiresAt) ? `; signed out if unused until ${dayFmt.format(new Date(d.idleExpiresAt))}` : ''}` : '',
      probe: `device:${d.id}`,
    };
  });
}

function signinFacts(ctx, now) {
  const s = ctx.signin || {};
  const session = s.session || null;
  const hours = Number.isInteger(session?.sessionTimeoutHours) && session.sessionTimeoutHours > 0 ? session.sessionTimeoutHours
    : Number.isInteger(ctx.sessionHours) && ctx.sessionHours > 0 ? ctx.sessionHours : 24;
  const host = text(ctx.host) || 'this dashboard';
  const transport = ctx.transport || 'http';
  const ends = session && validDate(session.expiresAt) ? Date.parse(session.expiresAt)
    : finite(ctx.signedInAt) ? ctx.signedInAt + hours * 3_600_000 : null;
  const others = session && Number.isInteger(session.otherBrowsers) ? session.otherBrowsers : null;
  const env = session?.managedBy === 'env';
  const secure = session ? session.secureTransport === true : (ctx.check?.secureTransport === true);
  const pw = s.pw || {};
  const network = networkView(s.network, ctx.check, transport, s.busy);
  const devices = devicesView(s.devices, now);
  const pairedCount = devices.length;
  const pwBlocked = env ? 'The password comes from environment variables on the dashboard computer, so it is changed there.'
    : !secure ? (network.on
      ? 'This connection is not on your trusted local network, so the password cannot be changed from here.'
      : 'Password changes need a trusted connection. Turn on local network trust above from the dashboard computer, or change it there.')
      : '';
  return {
    username: text(session?.username) || text(ctx.username) || 'Not reported',
    connection: TRANSPORT_TEXT[transport](host),
    session: ends && ends > now ? `ends in ${duration(ends - now)}` : `lasts ${hours} hours`,
    sessionSub: ends && ends > now ? `sessions last ${hours} hours` : '',
    sessionTip: ends && ends > now ? `This session ends ${dateTime.format(new Date(ends))}` : `Sessions on this dashboard last ${hours} hours from sign-in`,
    othersText: others === null ? 'Not reported' : others === 0 ? 'None signed in' : `${others} signed in`,
    othersEnabled: others !== null && others > 0 && s.busy !== 'others',
    othersBusy: s.busy === 'others',
    othersTip: others === null ? 'The server did not report other browsers' : 'Sign out every other browser signed in to this dashboard; this one stays signed in.',
    passwordWhen: session && validDate(session.passwordChangedAt) ? `changed ${dayFmt.format(new Date(session.passwordChangedAt))}` : session ? 'not changed here yet' : '',
    passwordCan: !!session && !pwBlocked,
    passwordNote: pwBlocked,
    passwordOpen: pw.open === true && !pwBlocked,
    passwordBusy: pw.busy === true,
    passwordDone: pw.done === true,
    passwordField: text(pw.field),
    passwordError: text(pw.error),
    passwordNonce: Number.isInteger(pw.nonce) ? pw.nonce : 0,
    passwordOthers: others && others > 0 ? `(${others} signed in)` : '(none signed in)',
    strength: { ...passwordStrength(''), matches: false, ...(pw.strength || {}) },
    devices,
    devicesNote: s.devicesError ? 'The paired-device list could not be read safely.'
      : 'No tray apps are paired. A tray pairs itself the first time you sign in on it.',
    devicesKnown: Array.isArray(s.devices),
    revokeBusy: text(s.busy).startsWith('revoke:') ? text(s.busy).slice(7) : '',
    revokeAllEnabled: (pairedCount > 0 || (others ?? 0) > 0) && s.busy !== 'all',
    revokeAllBusy: s.busy === 'all',
    signOutBusy: s.busy === 'logout',
    network,
    pairingNote: [run('How pairing works.', true), run(" Each tray trades the password once, over a trusted connection, for its own device token, so changing the password doesn't break it. A tray keeps its current sign-in until its token works. Revoke a tray to sign it out.")],
  };
}
function connectionFacts(ctx) {
  const transport = ctx.transport || 'http';
  const hours = Number.isInteger(ctx.sessionHours) && ctx.sessionHours > 0 ? ctx.sessionHours : 24;
  const trusted = networkKnown(ctx.signin?.network) ? ctx.signin.network.trustLocalNetwork === true : ctx.check?.trustedLocalNetwork === true;
  return [
    { label: 'Dashboard', value: text(ctx.origin) || 'Unknown', mono: true },
    { label: 'Transport', value: transport === 'https' ? 'HTTPS' : transport === 'loopback' ? 'This computer only' : trusted ? 'Plain HTTP on your trusted local network' : 'Plain HTTP on your network' },
    { label: 'Tray sign-in', value: 'Device tokens; each paired tray is listed under Dashboard sign-in', mono: false },
    { label: 'Sessions', value: `${hours} hours from sign-in`, mono: false },
  ];
}

// ---------------------------------------------------------------- one account's Show on dashboard / Show in tray
/**
 * The two switches on an account row. They are independent: "Show on dashboard" reads only
 * `settings.hiddenAccountIds` and "Show in tray" only `settings.trayHiddenAccountIds`, so all four combinations
 * (shown in both, hidden only from the dashboard, hidden only from the tray, hidden from both) are drawn as saved.
 * A server without the tray list draws "Show in tray" off and flat with its reason. While the account's provider is
 * hidden on a surface (`providerDashHidden`, `providerTrayHidden`), that surface's switch is drawn flat with its saved
 * value kept, because the provider's switch wins there; the other surface's switch is not affected.
 */
export function accountSwitches(id, { idsKnown, hiddenIds, trayIdsKnown, trayHiddenIds, visibilityOk, visWaiting, providerDashHidden = false, providerTrayHidden = false }) {
  const shownDash = !hiddenIds.has(id);
  const shownTray = trayIdsKnown ? !trayHiddenIds.has(id) : true;
  const withProvider = 'Hidden with its provider. Turn the provider\'s switch on first.';
  return {
    shownDash,
    shownTray,
    dashEnabled: idsKnown && visibilityOk && !providerDashHidden && !visWaiting('acct-show:'),
    trayAcctEnabled: trayIdsKnown && visibilityOk && !providerTrayHidden && !visWaiting('acct-tray:'),
    // the tips do not depend on the state: a tip stays on screen while the pointer rests after a click
    dashTip: !visibilityOk ? 'The saved choices could not be read safely, so changing them waits for the next refresh.'
      : providerDashHidden ? withProvider
      : 'Show this account on the dashboard. The trays keep their own switch.',
    trayAcctTip: !trayIdsKnown ? 'Hiding one account in the trays is not on this server yet.'
      : !visibilityOk ? 'The saved choices could not be read safely, so changing them waits for the next refresh.'
      : providerTrayHidden ? withProvider
      : 'Show this account in the Mac and Windows trays. The dashboard keeps its own switch.',
  };
}

// ---------------------------------------------------------------- the page
/**
 * ctx: { now, profiles, platform, antigravityInventory, antigravityAuto, refreshSeconds, refreshKnown, updateJob,
 *        username, host, origin, transport, sessionHours, signedInAt, serverVersion, openProgress,
 *        registry (GET /api/accounts/registry), flows ({provider: state}), lines ({rowId|trash:id: state}),
 *        busyAct, visPending (visibility saves waiting), check (GET /api/auth/check),
 *        signin ({ session, devices, network, pw, busy }) }
 */
export function accountsViewModel(data, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const registryAccounts = Array.isArray(ctx.registry?.accounts) ? ctx.registry.accounts : [];
  const c = { ...ctx, now, data, registry: new Map(registryAccounts.map(account => [account.id, account])) };
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const settings = data?.settings || {};
  const serverHidden = new Set(Array.isArray(settings.hiddenProviders) ? settings.hiddenProviders : []);
  const trayKnown = Array.isArray(settings.trayHiddenProviders);
  const trayHidden = new Set(trayKnown ? settings.trayHiddenProviders : []);
  // each account's own switches: the dashboard list and the tray list, read separately and never mixed
  const idsKnown = Array.isArray(settings.hiddenAccountIds);
  const hiddenIds = new Set(idsKnown ? settings.hiddenAccountIds : []);
  const trayIdsKnown = Array.isArray(settings.trayHiddenAccountIds);
  const trayHiddenIds = new Set(trayIdsKnown ? settings.trayHiddenAccountIds : []);
  const visibilityOk = settings.visibilityAvailable !== false;
  // Every provider's rows and switch state, even for a provider or account hidden on Home.
  const unhidden = data ? { ...data, settings: { ...settings, hiddenProviders: [], hiddenAccountIds: [] } } : data;
  const home = dashboardViewModel(unhidden, { ...c, refreshing: false });
  const homeRows = new Map(home.sections.flatMap(section => section.rows.map(row => [row.id, row])));
  const agAccounts = accounts.filter(account => account.provider === 'antigravity');
  const agSection = home.sections.find(section => section.id === 'antigravity');
  const ag = antigravityPolicy(data, c, agSection, agAccounts);
  // GET /api/auth/network (read with the block, and after Turn off) is newer than the check read at sign-in
  const trustNote = (networkKnown(c.signin?.network) ? c.signin.network.connection?.trusted === true : c.check?.connection?.trusted === true) ? TRUSTED_NOTE : '';
  const visWaiting = prefix => (Array.isArray(c.visPending) ? c.visPending : []).some(key => String(key).startsWith(prefix));

  const providers = PROVIDER_REGISTRY.map(entry => {
    const def = ACCOUNT_KINDS[entry.id];
    const server = providerEntry(data, entry.id);
    const mine = accounts.filter(account => account.provider === entry.id);
    // accounts the registry lists that the dashboard has no row for yet (a profile created a moment ago)
    for (const reg of registryAccounts) if (reg.provider === entry.id && !mine.some(account => account.id === reg.id)) {
      mine.push({ id: reg.id, provider: reg.provider, email: reg.email, label: reg.label, plan: null, platform: reg.platform, status: 'needs_sign_in', windows: [], capabilities: {}, lifecycle: reg.lifecycle });
    }
    const section = home.sections.find(s => s.id === entry.id);
    const canSwitch = SWITCHABLE.includes(entry.id) && !!section?.canSwitch;
    const hiddenServer = serverHidden.has(entry.id);
    const rows = mine.map(account => {
      const homeRow = homeRows.get(account.id);
      const reg = c.registry.get(account.id) || null;
      const [src, srcSub] = sourceLines(entry.id, account, reg);
      const { status, sampled: sampledLine, sampledTip: sampledLineTip } = rowStatus(account, now, reg);
      // a Claude Open in progress (claude-open.mjs) takes the "sampled" line while it runs
      const opening = entry.id === 'claude' && c.openProgress instanceof Map ? c.openProgress.get(homeRow?.profile || text(account.capabilities?.claudeProfileId)) : null;
      const sampled = opening?.text || sampledLine;
      const sampledTip = opening?.text || sampledLineTip;
      const email = text(account.email) || text(account.label) || 'Account identity unavailable';
      const meta = [planLabel(text(account.plan)), text(account.label) && text(account.label) !== email ? text(account.label) : ''].filter(Boolean).join(' · ');
      const switchable = SWITCHABLE.includes(entry.id);
      const line = c.lines?.[account.id];
      return {
        id: account.id, provider: entry.id, email, meta, srcInline: src, status, sampled, sampledTip, src, srcSub,
        active: switchable && !!homeRow?.active, activeLabel: homeRow?.activeLabel || '',
        canActivate: !!homeRow?.canActivate, activateKind: homeRow?.activateKind || '', profile: homeRow?.profile || '',
        activateHint: homeRow?.activateHint || '', confirm: !!homeRow?.confirm, confirmRuns: homeRow?.confirmRuns || [],
        actions: rowActions(entry.id, account, homeRow, canSwitch, c),
        line: lineView(account.id, email, line),
        gone: line?.kind === 'removing',
        ...accountSwitches(account.id, {
          idsKnown, hiddenIds, trayIdsKnown, trayHiddenIds, visibilityOk, visWaiting,
          providerDashHidden: hiddenServer, providerTrayHidden: trayKnown && trayHidden.has(entry.id),
        }),
      };
    });
    const count = rows.length;
    const addGate = gate(server, 'add', entry.id);
    const needs = entry.id === 'antigravity' && !addGate.live ? `Needs setup: ${addGate.reason}` : '';
    return {
      id: entry.id, label: entry.longLabel, kindLabel: def.kindLabel, kindIcon: def.icon, count,
      countText: `${count} ${count === 1 ? 'account' : 'accounts'}`,
      column: COL_A.includes(entry.id) ? 'a' : 'b',
      visible: !hiddenServer,
      hiddenNote: hiddenServer ? 'Hidden on the dashboard' : '',
      toggleEnabled: visibilityOk && !visWaiting('show:'),
      toggleTip: visibilityOk ? 'Saved on the dashboard: every browser follows this choice.' : 'The saved choices could not be read safely, so changing them waits for the next refresh.',
      trayVisible: trayKnown ? !trayHidden.has(entry.id) : true,
      trayEnabled: trayKnown && visibilityOk && !visWaiting('tray:'),
      trayComing: !trayKnown,
      trayTip: trayKnown ? 'Saved on the dashboard: the Mac and Windows trays follow this choice.' : 'Showing or hiding a provider in the trays is not on this server yet.',
      switchable: SWITCHABLE.includes(entry.id), canSwitch, slots: def.slots, actsMin: def.actsMin,
      how: needs || def.how, needs: !!needs, foot: footActions(entry.id, mine, c),
      empty: count ? '' : `No ${entry.label} accounts yet.`,
      ag: entry.id === 'antigravity' && ag.shown,
      flow: flowView(entry.id, c.flows?.[entry.id], { now, trustNote, busyAct: c.busyAct }),
      trash: entry.id === 'claude' ? trashRows(ctx.registry?.trash, c, now) : [],
      rows,
    };
  });
  return {
    version: ACCOUNTS_VIEW_VERSION,
    colA: providers.filter(p => p.column === 'a'),
    colB: providers.filter(p => p.column === 'b'),
    ag,
    policies: policies(data, home, ag),
    refresh: { seconds: Number.isInteger(c.refreshSeconds) ? c.refreshSeconds : 60, known: c.refreshKnown === true },
    update: updateResultsView(c.updateJob, now),
    signin: signinFacts(c, now),
    connection: connectionFacts(c),
    about: { version: text(c.serverVersion) ? `Version ${c.serverVersion}` : 'Daylight Atlas dashboard' },
  };
}

// ---------------------------------------------------------------- the refresh slider (log scale, 30 s to 60 min)
export const REFRESH_SNAPS = [30, 60, 120, 300, 600, 900, 1800, 3600];
const LMIN = Math.log(30), LMAX = Math.log(3600);
export const refreshPosition = seconds => (Math.log(Math.min(3600, Math.max(30, seconds))) - LMIN) / (LMAX - LMIN);
/** A slider position (0..1) to whole seconds, snapping to the marks within 3.5% of the track. */
export function refreshFromPosition(position) {
  const x = Math.min(1, Math.max(0, Number(position) || 0));
  const near = REFRESH_SNAPS.find(seconds => Math.abs(refreshPosition(seconds) - x) < 0.035);
  return near ?? Math.min(3600, Math.max(30, Math.round(Math.exp(LMIN + x * (LMAX - LMIN)))));
}
