// Account and sign-in actions: what each control sends and what each answer means, as pure functions that
// bridge.js uses and tests/account-actions.test.mjs covers. The routes and codes are the CLIENT API SHEET of the
// round-2 base (CONTRACT-registry-lifecycle 6, CONTRACT-auth-devices 3 to 6, the trusted local network rule).
//
// Every server error is `{ error, code, ...extra }`; the page never shows the server's sentence. It shows its own
// calm, specific text per code (errorText), so a refusal says what happened and what to do next.

const finite = value => typeof value === 'number' && Number.isFinite(value);
const text = value => typeof value === 'string' ? value : '';
const enc = value => encodeURIComponent(String(value));

/** Provider labels as the registry names them (view-model.mjs PROVIDER_REGISTRY). */
export const PROVIDER_LABELS = Object.freeze({
  claude: 'Claude', codex: 'Codex', antigravity: 'Antigravity', cursor: 'Cursor', muse: 'Muse Code',
  'kimi-code': 'Kimi Code', qwen: 'Qwen Token Plan', zai: 'Z.ai Coding Plan', 'opencode-go': 'OpenCode Go',
});
const label = provider => PROVIDER_LABELS[provider] || 'This provider';

// ---------------------------------------------------------------- requests
/**
 * The request each action sends: { method, path, body }. `body` is a plain object (JSON) or undefined.
 * Mutations always carry a JSON body: the server wants `application/json` even for an empty `{}`.
 */
export const requests = Object.freeze({
  registry: () => ({ method: 'GET', path: '/api/accounts/registry' }),
  // only the list that changed: the server merges a partial body, so a list another browser saved is kept
  visibility: hiddenProviders => ({ method: 'PUT', path: '/api/accounts/visibility', body: { hiddenProviders: [...hiddenProviders] } }),
  trayVisibility: trayHiddenProviders => ({ method: 'PUT', path: '/api/accounts/visibility', body: { trayHiddenProviders: [...trayHiddenProviders] } }),
  // one account's own switches: each sends only its own list, so the dashboard and tray choices never touch
  accountVisibility: hiddenAccountIds => ({ method: 'PUT', path: '/api/accounts/visibility', body: { hiddenAccountIds: [...hiddenAccountIds] } }),
  trayAccountVisibility: trayHiddenAccountIds => ({ method: 'PUT', path: '/api/accounts/visibility', body: { trayHiddenAccountIds: [...trayHiddenAccountIds] } }),
  addCodex: profileName => ({ method: 'POST', path: '/api/accounts/add', body: { provider: 'codex', profileName } }),
  addClaude: (profileId, name) => ({ method: 'POST', path: '/api/accounts/add', body: { provider: 'claude', profileId, ...(name ? { label: name } : {}) } }),
  addKey: (provider, key, name) => ({ method: 'POST', path: '/api/accounts/add', body: { provider, key, ...(name ? { label: name } : {}) } }),
  addSession: provider => ({ method: 'POST', path: '/api/accounts/add', body: { provider } }),
  addSupervised: (provider, profileName) => ({ method: 'POST', path: '/api/accounts/add', body: { provider, profileName } }),
  signInAgain: id => ({ method: 'POST', path: `/api/accounts/${enc(id)}/signin-again`, body: {} }),
  replaceKey: (id, key) => ({ method: 'PUT', path: `/api/accounts/${enc(id)}/key`, body: { key } }),
  removeAsk: id => ({ method: 'POST', path: `/api/accounts/${enc(id)}/remove`, body: {} }),
  removeCommit: (id, confirmationToken) => ({ method: 'POST', path: `/api/accounts/${enc(id)}/remove`, body: { confirmationToken } }),
  openApp: (id, platform) => ({ method: 'POST', path: `/api/accounts/${enc(id)}/open`, body: { platform } }),
  recheck: id => ({ method: 'POST', path: `/api/accounts/${enc(id)}/recheck`, body: {} }),
  restoreAsk: trashId => ({ method: 'POST', path: `/api/accounts/trash/${enc(trashId)}/restore`, body: {} }),
  restoreCommit: (trashId, confirmationToken) => ({ method: 'POST', path: `/api/accounts/trash/${enc(trashId)}/restore`, body: { confirmationToken } }),
  job: jobId => ({ method: 'GET', path: `/api/accounts/signin-jobs/${enc(jobId)}` }),
  cancelJob: jobId => ({ method: 'POST', path: `/api/accounts/signin-jobs/${enc(jobId)}/cancel`, body: {} }),
  submitCode: (jobId, code) => ({ method: 'POST', path: `/api/accounts/signin-jobs/${enc(jobId)}/code`, body: { code } }),
  session: () => ({ method: 'GET', path: '/api/auth/session' }),
  devices: () => ({ method: 'GET', path: '/api/auth/devices' }),
  network: () => ({ method: 'GET', path: '/api/auth/network' }),
  setNetwork: on => ({ method: 'PUT', path: '/api/auth/network', body: { trustLocalNetwork: on === true } }),
  password: (currentPassword, newPassword, signOutOtherBrowsers) => ({ method: 'POST', path: '/api/auth/password', body: { currentPassword, newPassword, signOutOtherBrowsers: signOutOtherBrowsers !== false } }),
  revokeOthers: () => ({ method: 'POST', path: '/api/auth/sessions/revoke-others', body: {} }),
  revokeDevice: id => ({ method: 'DELETE', path: `/api/auth/devices/${enc(id)}`, body: {} }),
  revokeAll: () => ({ method: 'POST', path: '/api/auth/devices/revoke-all', body: { signOutOtherBrowsers: true } }),
  logout: () => ({ method: 'POST', path: '/api/auth/logout', body: {} }),
});

// ---------------------------------------------------------------- local checks (before anything is sent)
/** Codex profile names: the CLI's own rule (letters, digits, - and _, starting with a letter or digit). */
export function profileNameProblem(name, taken = []) {
  const value = String(name ?? '').trim();
  if (!value) return 'Name the profile first.';
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$/.test(value)) return 'Use letters, digits, - or _, starting with a letter or digit (at most 32).';
  if (taken.some(other => String(other).toLowerCase() === value.toLowerCase())) return `${value} is already a profile name.`;
  return '';
}
/** Claude profile ids: `^[a-z][a-z0-9-]{1,31}$` (the server's rule). */
export function claudeIdProblem(id, taken = []) {
  const value = String(id ?? '').trim();
  if (!value) return 'Name the profile first.';
  if (!/^[a-z][a-z0-9-]{1,31}$/.test(value)) return 'Use 2 to 32 lowercase letters, digits or -, starting with a letter.';
  if (taken.some(other => String(other).toLowerCase() === value)) return `${value} is already a Claude profile.`;
  return '';
}
/** An API key: 8 to 512 printable ASCII characters, no whitespace (the server's rule). The key is never echoed. */
export function keyProblem(key) {
  const value = String(key ?? '');
  if (!value) return 'Paste the key first.';
  if (/\s/.test(value)) return 'The key has a space or line break in it. Paste it again.';
  if (!/^[\x21-\x7e]+$/.test(value)) return 'The key has a character a key cannot contain. Paste it again.';
  if (value.length < 8) return 'That is too short to be a key.';
  if (value.length > 512) return 'That is too long to be a key.';
  return '';
}
/** A suggested name for a new profile that is not taken: "codex-4". */
export function suggestName(prefix, taken = []) {
  const lower = new Set(taken.map(name => String(name).toLowerCase()));
  // the next number after the profiles there are ("codex-4" beside three), skipping a name in use
  for (let n = Math.max(1, taken.length + 1); n < 100; n++) { const name = `${prefix}-${n}`; if (!lower.has(name)) return name; }
  return `${prefix}-new`;
}
const bytes = value => new TextEncoder().encode(value).length;
/** The password-change form, checked before it is sent. Returns [field, message] or null. */
export function passwordProblem({ current = '', next = '', confirm = '' } = {}) {
  if (!current) return ['cur', 'Enter your current password.'];
  if (Array.from(next).length < 8) return ['new', 'Use at least 8 characters.'];
  if (bytes(next) > 72) return ['new', 'Keep it within 72 bytes; bcrypt ignores the rest.'];
  if (next === current) return ['new', 'Choose a password different from the current one.'];
  if (confirm !== next) return ['conf', "Doesn't match the new password."];
  return null;
}

// ---------------------------------------------------------------- why an action is not available
/** `providers[].signIn.unavailableReason` in plain words. `coming` is true only when the server has no flow yet. */
export function unavailableText(reason, provider) {
  switch (reason) {
    case 'not_implemented': return { coming: true, text: `${label(provider)} sign-in from the dashboard is not on this server yet.` };
    case 'secure_transport_required': return { coming: false, text: 'Needs a trusted connection. Turn on local network trust on the dashboard computer (Settings, Dashboard sign-in), or use the dashboard there.' };
    case 'tool_missing': return { coming: false, text: `The ${label(provider)} command-line tool is not installed on the dashboard computer.` };
    // Antigravity signs in from a terminal on Ubuntu (the contract's terminal fallback): say the command
    case 'preflight_failed': return provider === 'antigravity'
      ? { coming: false, text: `Run ${ANTIGRAVITY_SIGNIN} <profile-name> in a terminal on Ubuntu.` }
      : { coming: false, text: 'Needs setup: the isolated sign-in check did not pass on the dashboard computer.' };
    case 'isolation_unproven': return { coming: false, text: `Needs setup: a second ${label(provider)} account cannot be kept apart yet.` };
    case 'extension_update_required': return { coming: false, text: 'Needs an update of the browser extension first.' };
    default: return { coming: false, text: '' };
  }
}

/** The Antigravity terminal sign-in (AGY lane), and the only fallback command the page will show. */
export const ANTIGRAVITY_SIGNIN = 'ai-account-center antigravity signin';
const TERMINAL_COMMAND = /^ai-account-center antigravity signin [a-z][a-z0-9_-]{0,47}$/;
/** A server `fallback: {kind:'terminal', host:'ubuntu', command}` the page may show, or ''. */
export function terminalCommand(payload) {
  const f = payload?.fallback;
  return f && f.kind === 'terminal' && f.host === 'ubuntu' && TERMINAL_COMMAND.test(text(f.command)) ? f.command : '';
}

// ---------------------------------------------------------------- errors
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const waitText = seconds => finite(seconds) && seconds > 0
  ? (seconds < 60 ? plural(Math.ceil(seconds), 'second', 'seconds') : plural(Math.ceil(seconds / 60), 'minute', 'minutes'))
  : 'a moment';
const HOSTS = { mac: 'the Mac', windows: 'Windows', ubuntu: 'Ubuntu' };

/**
 * Calm, specific words for an error answer. `error` is what `send()` throws ({ status, payload }) or a payload.
 * ctx: { provider, what, name, host } fills the sentence. Returns { title, body }.
 */
export function errorText(error, ctx = {}) {
  const payload = error?.payload ?? (error && typeof error === 'object' && 'code' in error ? error : null);
  const code = text(payload?.code);
  const status = finite(error?.status) ? error.status : null;
  const p = ctx.provider;
  const name = text(ctx.name);
  const host = HOSTS[payload?.host] || HOSTS[ctx.host] || 'A computer';
  const t = (title, body = '') => ({ title, body, code });
  switch (code) {
    // the session and request guards
    case 'auth_required': return t('Signed out', 'This browser is no longer signed in. Sign in again to continue.');
    case 'session_revoked': return t('Signed out from another browser', 'Sign in again to continue.');
    case 'device_scope': return t('Not allowed from a tray', 'This needs a browser session on the dashboard.');
    case 'origin_required': return t('Refused for safety', 'The request did not come from this dashboard page. Reload the page and try again.');
    case 'json_required': case 'invalid_json': case 'unexpected_query': case 'body_too_large':
      return t('The request was not understood', 'Reload the page and try again.');
    case 'invalid_body': return t('That was not accepted', 'Something in the request was not in the expected form. Check what you typed and try again.');
    case 'not_found': return t('Not on this server', 'This dashboard does not have that route. It may need an update.');
    case 'token_in_query': return t('Refused for safety', 'A device token must never be sent in an address.');
    case 'internal_error': return t('Something failed on the dashboard', 'Nothing was changed. Try again.');
    // dashboard sign-in
    case 'auth_not_configured': return t('Sign-in is not set up', 'Set up sign-in on the dashboard computer first.');
    case 'managed_by_env': return t('Managed on the server', 'The sign-in comes from environment variables on the dashboard computer, so it is changed there.');
    case 'weak_password': return payload?.reason === 'too_long'
      ? t('Password too long', 'Keep it within 72 bytes; bcrypt ignores the rest.')
      : t('Password too short', 'Use at least 8 characters.');
    case 'same_password': return t('Same password', 'Choose a password different from the current one.');
    case 'wrong_password': {
      const left = finite(payload?.triesLeft) ? payload.triesLeft : null;
      return t("That isn't the current password", left === null ? '' : left === 0
        ? 'That was the last try before a 15-minute pause.'
        : `${plural(left, 'try', 'tries')} left before a 15-minute pause.`);
    }
    case 'invalid_credentials': {
      const left = finite(payload?.triesLeft) ? payload.triesLeft : null;
      return t("Username or password isn't right.", left === null ? '' : left === 0
        ? 'That was the last try; the next one pauses sign-in for 15 minutes.'
        : `${plural(left, 'try', 'tries')} left before sign-in pauses for 15 minutes.`);
    }
    case 'rate_limited': return p
      ? t(`${label(p)} was checked a moment ago`, `Try again in ${waitText(payload?.retryAfterSeconds)}.`)
      : t('Paused after too many tries', `Try again in ${waitText(payload?.retryAfterSeconds)}.`);
    case 'invalid_hash': return t('The saved password cannot be checked', 'Run ai-account-center dashboard auth setup on the dashboard computer to set it again.');
    case 'secure_transport_required': return p
      ? t('Needs a trusted connection', `Keys and sign-in codes are only sent on a trusted connection. Turn on local network trust on the dashboard computer (Settings, Dashboard sign-in), or use the dashboard there.${payload?.fallback?.command ? ` Or run on Ubuntu: ${payload.fallback.command}` : ''}`)
      : t('Needs a trusted connection', 'Turn on local network trust on the dashboard computer (Settings, Dashboard sign-in), or do this on the dashboard computer itself.');
    case 'loopback_required': return t('Turn it on at the dashboard computer', 'Local network trust can only be turned on from the dashboard computer itself, or in config.yaml.');
    case 'write_failed': return t('Not saved', 'The change could not be saved safely. Nothing was changed. Try again.');
    case 'unknown_device': return t('Already signed out', 'That tray was already signed out.');
    case 'auth_store_unavailable': return t('Device list unavailable', 'The paired-device file could not be read safely. Nothing was changed.');
    case 'too_many_devices': return t('Too many paired trays', 'Sign out a tray you no longer use first.');
    case 'already_configured': return t('Sign-in is already set up', 'Sign in with the existing username and password.');
    case 'setup_code_required': return t('Enter the setup code', 'It has 8 letters and digits, printed in the server\'s terminal and saved in ~/.ccs/auth/setup-code.');
    case 'setup_code_invalid': return t("That setup code isn't right", 'It has 8 letters and digits, printed in the server\'s terminal and saved in ~/.ccs/auth/setup-code.');
    case 'invalid_username': return t('Username not accepted', 'Start with a letter; use 3 or more letters, numbers, - or _.');
    // visibility
    case 'visibility_unavailable': return t('Visibility unavailable', 'The saved choices could not be read safely, so nothing was changed; the last saved choices stay in force. Fix or remove ~/.ccs/account-visibility.json to change them again.');
    case 'visibility_write_failed': return t('Not saved', 'The visibility choice could not be saved safely. Try again.');
    // account lifecycle
    case 'registry_unavailable': return t('Account list unavailable', 'The account list could not be read safely. Nothing was changed.');
    case 'invalid_account': return t('Account not recognised', 'That account id is not one the dashboard knows. The list is refreshed.');
    case 'unknown_account': return t('Account not found', 'It may have been removed in another browser. The list is refreshed.');
    case 'not_implemented': return t('Not on this server yet', `${label(p)} cannot do this from the dashboard yet.`);
    case 'not_configured': return t('Not set up there', `${label(p)} has no launcher on that computer.`);
    case 'not_removable': return t('Cannot be removed here', 'This account is missing details the dashboard needs to remove it safely.');
    case 'single_account_provider': return t('One account only', `${label(p)} reads one account in this version.`);
    case 'too_many_accounts': return t('Account limit reached', `${label(p)} already has the most accounts allowed. Remove one first.`);
    case 'id_in_use': return t('Name already used', `${name || 'That name'} is already used by another account, or by one in the trash.`);
    case 'duplicate_key': return t('Key already saved', 'That key is already saved for another account. Nothing was stored.');
    case 'key_rejected': return t('The provider refused the key', ctx.what === 'replace' ? 'The old key is still in use.' : 'Nothing was stored.');
    case 'key_store_unavailable': return t('Key not stored', 'The key could not be stored safely. Nothing was saved.');
    case 'not_aac_owned': return t('Key belongs to another app', 'This account reads a key another app saved. Add a key here to manage it from the dashboard.');
    case 'use_replace_key': return t('Use Replace key', 'This account signs in with a key; replace the key instead.');
    case 'account_active': return t('This is the active account', ctx.what === 'signin-again'
      ? 'Activate another account first, then sign in again.'
      : 'Activate another account first, then remove it.');
    case 'account_default': return t('This is the default account', p === 'claude'
      ? "This is this computer's default Claude profile. It can't be removed here."
      : 'Make another account the default first.');
    case 'account_protected': return t('Protected profile', "This is this computer's default Claude profile. It can't be removed here.");
    case 'last_account': return t('Last account', `This is the last ${label(p)} account, so it stays.`);
    case 'activation_running': return t('A switch is running', 'Wait for the account switch to finish, then try again.');
    case 'signin_running': return t('A sign-in is running', 'Wait for the sign-in to finish or cancel it, then try again.');
    case 'job_running': return t('A sign-in is already running', `Finish or cancel the open ${label(p)} sign-in first.`);
    case 'too_many_jobs': return t('Too many sign-ins running', 'Finish or cancel one of them first.');
    case 'app_running': return t('The app is open', 'Quit Claude for this profile on that computer, then try again.');
    case 'app_state_unknown': return t('Could not check the app', 'Whether Claude is open could not be checked. Nothing was changed. Try again later.');
    case 'trash_cross_volume': return t('Trash is on another disk', `${host} keeps the trash on another disk, so nothing was moved.`);
    case 'remove_failed': return t('Not removed', 'The account could not be removed safely. Nothing was changed.');
    case 'restore_failed': return t('Not restored', 'The profile could not be put back safely. It stays in the trash.');
    case 'unknown_trash': return t('No longer in the trash', 'It was restored or deleted already. The list is refreshed.');
    case 'confirmation_stale': return t('It changed', 'The account changed since you reviewed it. Review it again.');
    case 'host_unreachable': return t(`${host === 'A computer' ? 'A computer' : host[0].toUpperCase() + host.slice(1)} could not be reached`, 'Nothing was changed. Check that it is on and try again.');
    case 'tool_missing': return t('Sign-in tool missing', `The ${label(p)} command-line tool is not installed on the dashboard computer.`);
    case 'unknown_job': return t('That sign-in has ended', 'Start it again if you still need it.');
    case 'code_not_expected': return t('Not waiting for a code', 'This sign-in is not asking for a code now.');
    case 'preflight_failed': {
      const command = terminalCommand(payload);
      if (command) return t('Sign in from a terminal', `Run this on Ubuntu: ${command}. The dashboard picks the account up when it is done.`);
      return t('Needs setup', unavailableText(code, p).text);
    }
    case 'isolation_unproven': case 'extension_update_required':
      return t('Needs setup', unavailableText(code, p).text);
    case 'session_rotation_failed': return t('Password changed', 'This browser may need to sign in again.');
    default: break;
  }
  if (status === 500 || status === 502 || status === 503) return t('That did not work', 'Something failed on the dashboard. Nothing was changed. Try again.');
  if (status === 404) return t('Not found', 'It may have been removed already. The list is refreshed.');
  if (error?.network) return t('Dashboard unreachable', 'The dashboard did not answer. Check the connection and try again.');
  return t('That did not work', 'Try again.');
}

/** A refused sign-out in plain words, never the server's sentence: this browser is still signed in. */
export function signOutFailureText(error) {
  if (error?.network) return { title: 'Not signed out', body: 'The dashboard did not answer, so this browser is still signed in. Check the connection and try again.' };
  // a refusal with its own code says why (a 4xx); a failure on the dashboard itself, coded or not, says so plainly
  if (text(error?.payload?.code) && finite(error?.status) && error.status < 500) { const t = errorText(error); return { title: t.title, body: t.body }; }
  return { title: 'Not signed out', body: 'Sign-out failed on the dashboard. Try again.' };
}

/** A finished or failed sign-in job in plain words (SignInJob.error.code). */
export function jobErrorText(job) {
  const code = text(job?.error?.code);
  const p = job?.provider;
  switch (code) {
    case 'tool_missing': return `The ${label(p)} command-line tool is not installed on the dashboard computer.`;
    case 'unexpected_output': return 'The sign-in tool answered in a way the dashboard does not recognise. Nothing was saved.';
    case 'identity_mismatch': return 'You signed in as a different account. Sign in with the same account; nothing was changed.';
    case 'duplicate_identity': return 'That account is already saved as another profile. Nothing was added.';
    case 'timeout': return 'The code expired before it was approved. Start again for a new code.';
    case 'provider_denied': return 'The sign-in was refused or cancelled at the provider. Nothing was saved.';
    case 'write_failed': return 'The new sign-in could not be saved safely. Nothing was changed.';
    case 'server_restarted': return 'The dashboard restarted during the sign-in. Start again.';
    default: return job?.state === 'cancelled' ? 'The sign-in was cancelled. Nothing was saved.' : 'The sign-in did not finish. Nothing was saved.';
  }
}

/** True for the job states that end it. */
export const jobFinished = job => ['succeeded', 'failed', 'expired', 'cancelled'].includes(job?.state);

// ---------------------------------------------------------------- toasts after a success
const names = list => list.length <= 1 ? (list[0] || '') : `${list.slice(0, -1).join(', ')} and ${list[list.length - 1]}`;
const DEVICE_WORD = { mac: 'Mac tray', windows: 'Windows tray' };
/** "Password changed. Mac tray and Windows tray stay signed in." from the paired devices the server lists. */
export function passwordChangedToast(result, devices = []) {
  const list = (Array.isArray(devices) ? devices : []).map(device => text(device?.name) || DEVICE_WORD[device?.platform] || 'A tray');
  const count = finite(result?.pairedDevices) ? result.pairedDevices : list.length;
  let title = 'Password changed.';
  if (list.length === 1) title = `Password changed. ${list[0]} stays signed in.`;
  else if (list.length > 1) title = `Password changed. ${names(list)} stay signed in.`;
  else if (count > 0) title = `Password changed. ${plural(count, 'paired tray stays', 'paired trays stay')} signed in.`;
  const others = finite(result?.signedOutBrowsers) ? result.signedOutBrowsers : 0;
  const body = others > 0
    ? `${plural(others, 'other browser was', 'other browsers were')} signed out; this one stays signed in.`
    : 'This browser stays signed in.';
  return { title, body: result?.code === 'session_rotation_failed' ? `${body} If the page asks, sign in again with the new password.` : body };
}
export function revokeAllToast(result) {
  const trays = finite(result?.revokedDevices) ? result.revokedDevices : 0;
  const browsers = finite(result?.signedOutBrowsers) ? result.signedOutBrowsers : 0;
  const parts = [trays ? `${plural(trays, 'tray shows', 'trays show')} the pairing screen` : 'No trays were paired', browsers ? `${plural(browsers, 'other browser was', 'other browsers were')} signed out` : ''];
  return { title: 'Signed out all devices', body: `${parts.filter(Boolean).join('; ')}. This browser stays signed in.` };
}

// ---------------------------------------------------------------- copying a code (plain HTTP has no async clipboard)
export async function copyText(value, doc = globalThis.document, nav = globalThis.navigator) {
  try { if (nav?.clipboard?.writeText) { await nav.clipboard.writeText(value); return true; } } catch {}
  try {
    const area = doc.createElement('textarea');
    area.value = value; area.setAttribute('readonly', ''); area.style.position = 'fixed'; area.style.opacity = '0';
    doc.body.appendChild(area); area.select();
    const ok = doc.execCommand('copy');
    area.remove();
    return ok === true;
  } catch { return false; }
}
