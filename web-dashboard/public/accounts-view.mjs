// Accounts & Settings view model, version 1 (Daylight Atlas, c-daylight-atlas/app-accounts.js and app-auth.js).
// Pure functions from the public API DTOs to the JSON that src/accounts.rs writes into the AcData global
// (ui/pages/accounts/ac-data.slint). Every label, every "coming" decision and every truthfulness rule lives
// here and is covered by tests/accounts-view.test.mjs; Slint only lays out and animates it.
//
// Only the APIs that exist on the server today are live (see LIVE below). Every other action is drawn in
// its approved place, dimmed, with a "coming" caption and a tip that says what it will do. The contracts
// for those routes are CONTRACT-registry-lifecycle.md and CONTRACT-auth-devices.md; the status file
// worktrees/status/W4.md lists each one. Nothing here ever shows an example value as real data.
import { PROVIDER_REGISTRY, dashboardViewModel, platformLabel, planLabel, relative, intervalLabel, run, statusWord, duration } from './view-model.mjs';
import { antigravityView } from './antigravity-data.mjs';
import { visibleUsageWindows } from './visible-usage.mjs';

export const ACCOUNTS_VIEW_VERSION = 1;

/**
 * Which actions the server can do today. Everything false is drawn as "coming". When a route lands, flip its
 * entry here and add its handler in bridge.js (the client-bindings task does both).
 */
export const LIVE = Object.freeze({
  activate: true,            // POST /api/codex/profiles/:name/activate, POST /api/antigravity/profiles/:id/activate
  openClaude: true,          // POST /api/claude/desktop-profiles/:id/open
  antigravityPolicy: true,   // PUT /api/antigravity/auto-switch (toggle, threshold, pool, cooldown)
  codexPolicy: true,         // PUT /api/codex/profiles/auto-switch
  refreshInterval: true,     // PUT /api/accounts/settings
  add: false,                // POST /api/accounts/add
  signInAgain: false,        // POST /api/accounts/:id/signin-again
  replaceKey: false,         // PUT /api/accounts/:id/key
  remove: false,             // POST /api/accounts/:id/remove
  openApp: false,            // POST /api/accounts/:id/open (Cursor, Muse)
  recheck: false,            // POST /api/accounts/:id/recheck
  visibilityServer: false,   // PUT /api/accounts/visibility (until then: this browser only)
  passwordChange: false,     // POST /api/auth/password
  otherBrowsers: false,      // GET /api/auth/session, POST /api/auth/sessions/revoke-others
  devices: false,            // GET /api/auth/devices, DELETE /api/auth/devices/:id, POST /api/auth/devices/revoke-all
});

/** The fixed sentence a "coming" action shows on hover. */
const COMING = 'Coming with the server update. ';
const SOON = {
  add: {
    claude: `${COMING}Add account creates a new Claude desktop profile on Mac and Windows; you then sign in inside the app.`,
    codex: `${COMING}Add account starts a device-code sign-in for a new Codex profile; you approve a short code in any browser.`,
    antigravity: `${COMING}Add account runs the Antigravity CLI login on Ubuntu under supervision; you sign in to Google in a browser.`,
  },
  signInAgain: `${COMING}Sign in again runs the device-code login for this profile; its history stays. The active account cannot be signed in again until another one is active.`,
  remove: {
    claude: `${COMING}Remove moves this profile's Claude data to a 30-day trash on Mac and Windows.`,
    codex: `${COMING}Remove deletes this saved Codex login. The active account and the saved default cannot be removed.`,
    antigravity: `${COMING}Remove deletes this saved Antigravity login on Ubuntu. The active account cannot be removed.`,
    apikey: `${COMING}Remove deletes the stored key from the dashboard host.`,
    browser: `${COMING}Remove stops reading this account; the browser extension keeps its session.`,
  },
  replaceKey: `${COMING}Replace key stores a new key on the dashboard host, only over HTTPS or an encrypted tunnel, and checks it first.`,
  addKey: `${COMING}Each key becomes its own account; the key is stored on the dashboard host and never shown again.`,
  openApp: `${COMING}Sign in opens the app on that computer so you can sign in there; the dashboard then reads the session.`,
  recheck: `${COMING}Re-check reads the session again now. Until then readings follow the refresh interval.`,
  extension: `${COMING}The console session comes from the browser extension; Sign in opens the console so the extension can sync it.`,
};

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
  antigravity: { kind: 'cli', kindLabel: 'Supervised CLI login', icon: 'terminal', src: 'Antigravity CLI, supervised', slots: 2, actsMin: 160,
    how: 'The dashboard host runs and supervises the CLI login.' },
  cursor: { kind: 'app', kindLabel: 'Desktop app session', icon: 'app-window', src: 'Cursor desktop app', slots: 1, actsMin: 150,
    how: 'Sign in inside the Cursor desktop app; the dashboard reads that session.' },
  muse: { kind: 'app', kindLabel: 'Desktop app session', icon: 'app-window', src: 'Muse Code app', slots: 1, actsMin: 150,
    how: 'Sign in inside the Muse Code app; the dashboard reads that session.' },
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
const article = word => /^[aeiou]/i.test(word) ? 'an' : 'a';
const isConsole = account => /console/i.test(text(account?.message)) || /^plan-opencode-go-console-/.test(text(account?.id));

// ---------------------------------------------------------------- actions
/** One fixed action slot. kind: switch | button | icon | quiet | empty. style: Button kinds. */
export function action(fields = {}) {
  return { kind: 'button', act: '', value: '', label: '', icon: '', platform: '', style: 'default', enabled: false, coming: false, tip: '', span: 1, ...fields };
}
const coming = (fields, tip) => action({ ...fields, enabled: false, coming: true, tip });
const emptySlot = () => action({ kind: 'empty' });
const removeSlot = (kind, provider) => coming({ kind: 'icon', act: 'remove', icon: 'trash', label: 'Remove', style: 'ghost' },
  SOON.remove[provider] || SOON.remove[kind] || SOON.remove.apikey);

/** Where the account was read and how fresh it is ("sampled 1m ago"); the status word appears only for an exception. */
function rowStatus(account, now) {
  const at = validDate(account.sampledAt) ? account.sampledAt : validDate(account.fetchedAt) ? account.fetchedAt : null;
  const normal = account.status === 'ok' || account.status === 'cached';
  const stale = !!at && now - Date.parse(at) > STALE_MS;
  return {
    status: !normal ? statusWord(account) : stale ? 'Stale' : '',
    sampled: at ? `sampled ${relative(at, now)}` : 'no reading yet',
    sampledTip: at ? `Last reading ${dateTime.format(new Date(at))}${account.status === 'cached' ? ', from the cache' : ''}` : 'No reading has arrived yet',
  };
}
function sourceLines(provider, account) {
  const def = ACCOUNT_KINDS[provider];
  if (provider === 'claude') {
    const platforms = Array.isArray(account.capabilities?.claudePlatforms) ? account.capabilities.claudePlatforms : [];
    return [def.src, platforms.length ? platforms.map(platformLabel).join(' and ') : platformLabel(account.platform)];
  }
  if (provider === 'opencode-go' && isConsole(account)) return ['Console session in a browser', 'Wallet and usage'];
  if (provider === 'qwen') return [def.src, 'by browser extension'];
  return [def.src, platformLabel(account.platform)];
}

function rowActions(provider, account, homeRow, canSwitch) {
  const def = ACCOUNT_KINDS[provider];
  switch (def.kind) {
    case 'desktop': {
      const profile = homeRow?.profile || text(account.capabilities?.claudeProfileId);
      const open = (target, label, platform) => {
        const can = target === 'mac' ? !!homeRow?.canMac : !!homeRow?.canWindows;
        return action({ act: 'launch', value: `${profile}:${target}`, label, platform, enabled: LIVE.openClaude && can && !!profile,
          tip: can ? `Open ${text(account.email) || 'this profile'} in its own Claude profile on ${label}` : `Open on ${label} is not set up for this profile` });
      };
      return [open('mac', 'Mac', 'apple'), open('windows', 'Windows', 'windows'), removeSlot('desktop', provider)];
    }
    case 'device':
      return [
        action({ kind: 'switch', act: 'activate', value: homeRow?.profile || '', enabled: LIVE.activate && !!homeRow?.canActivate, tip: homeRow?.activateHint || '' }),
        coming({ act: 'signin-again', label: 'Sign in again', icon: 'login' }, SOON.signInAgain),
        removeSlot('device', provider),
      ];
    case 'cli':
      return [
        canSwitch || homeRow?.active
          ? action({ kind: 'switch', act: 'antigravity-activate', value: homeRow?.profile || '', enabled: LIVE.activate && !!homeRow?.canActivate, tip: homeRow?.activateHint || '' })
          : emptySlot(),
        removeSlot('cli', provider),
      ];
    case 'apikey':
      return [
        isConsole(account)
          ? coming({ act: 'signin', label: 'Sign in', icon: 'login' }, SOON.extension)
          : coming({ act: 'replace-key', label: 'Replace key', icon: 'key', style: 'accent-line' }, SOON.replaceKey),
        removeSlot('apikey', provider),
      ];
    case 'browser':
      return [coming({ act: 'signin', label: 'Sign in', icon: 'login' }, SOON.extension), removeSlot('browser', provider)];
    case 'app':
      return [action({ kind: 'quiet', label: 'Session from the app' })];
    default:
      return [];
  }
}

function footActions(provider, accounts) {
  const def = ACCOUNT_KINDS[provider];
  const label = PROVIDER_REGISTRY.find(row => row.id === provider)?.label || provider;
  switch (def.kind) {
    case 'desktop': case 'device': case 'cli':
      return [coming({ act: 'add', label: 'Add account', icon: 'plus', style: 'primary' }, SOON.add[provider])];
    case 'app':
      return [coming({ act: 'open-app', label: 'Sign in', icon: 'login' }, SOON.openApp), coming({ act: 'recheck', label: 'Re-check', icon: 'refresh' }, SOON.recheck)];
    case 'browser':
      return [coming({ act: 'signin', label: 'Sign in', icon: 'globe' }, SOON.extension), coming({ act: 'recheck', label: 'Re-check', icon: 'refresh' }, SOON.recheck)];
    case 'apikey': {
      const hasKey = accounts.some(account => !isConsole(account));
      const add = hasKey
        ? coming({ act: 'add-key', label: `Add another ${label} key`, icon: 'plus', style: 'ghost' }, SOON.addKey)
        : coming({ act: 'add-key', label: `Add ${article(label)} ${label} key`, icon: 'key', style: 'primary' }, SOON.addKey);
      return provider === 'opencode-go' ? [coming({ act: 'recheck', label: 'Re-check', icon: 'refresh' }, SOON.recheck), add] : [add];
    }
    default:
      return [];
  }
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

// ---------------------------------------------------------------- connection and sign-in facts
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
/** The sign-in card's network note (the concept's wording on plain HTTP). */
export function transportNote(transport) {
  if (transport === 'https') return 'This address uses HTTPS, so the password is encrypted on its way to the dashboard.';
  if (transport === 'loopback') return 'You are on the dashboard host itself, so the password never crosses the network.';
  return 'This address is plain HTTP, so the password crosses your network unencrypted. Password changes and tray pairing need HTTPS or an encrypted tunnel.';
}

function signinFacts(ctx) {
  const now = ctx.now;
  const hours = Number.isInteger(ctx.sessionHours) && ctx.sessionHours > 0 ? ctx.sessionHours : 24;
  const host = text(ctx.host) || 'this dashboard';
  const transport = ctx.transport || 'http';
  const ends = finite(ctx.signedInAt) ? ctx.signedInAt + hours * 3_600_000 : null;
  return {
    username: text(ctx.username) || 'Not reported',
    connection: TRANSPORT_TEXT[transport](host),
    session: ends && ends > now ? `ends in ${duration(ends - now)}` : `lasts ${hours} hours`,
    sessionSub: ends && ends > now ? `sessions last ${hours} hours` : '',
    sessionTip: ends && ends > now ? `Signed in ${dateTime.format(new Date(ctx.signedInAt))}; the session ends ${dateTime.format(new Date(ends))}` : `Sessions on this dashboard last ${hours} hours from sign-in`,
    otherTip: `${COMING}Other browsers lists the browsers signed in to this dashboard and signs them out with one click.`,
    passwordTip: `${COMING}Change password checks the current password, needs HTTPS or an encrypted tunnel, and signs other browsers out. Until then run ai-account-center dashboard auth setup on the dashboard host, then sign in again on each tray.`,
    devicesNote: 'No tray apps are paired yet. Today each tray signs in with the dashboard password it keeps, so a password change means signing in again on each tray.',
    devicesTip: `${COMING}Each paired tray is listed here with its computer and when it last checked in, with Revoke. After a password change the page says "Mac tray and Windows tray stay signed in."`,
    revokeAllTip: `${COMING}Sign out all devices signs out every tray and every other browser; this browser stays signed in.`,
    pairingNote: [run('How pairing works.', true), run(" Each tray trades the password once, over HTTPS or an encrypted tunnel, for its own device token, so changing the password doesn't break it. A tray keeps its current sign-in until its token works. Revoke a tray to sign it out.")],
  };
}
function connectionFacts(ctx) {
  const transport = ctx.transport || 'http';
  const hours = Number.isInteger(ctx.sessionHours) && ctx.sessionHours > 0 ? ctx.sessionHours : 24;
  return [
    { label: 'Dashboard', value: text(ctx.origin) || 'Unknown', mono: true },
    { label: 'Transport', value: transport === 'https' ? 'HTTPS' : transport === 'loopback' ? 'This computer only' : 'Plain HTTP on your network', mono: false },
    { label: 'Tray sign-in', value: 'Each tray keeps the dashboard password; device tokens are coming', mono: false },
    { label: 'Sessions', value: `${hours} hours from sign-in`, mono: false },
  ];
}

// ---------------------------------------------------------------- the page
/**
 * ctx: { now, profiles, platform, antigravityInventory, antigravityAuto, localHidden (Set), serverHidden (Set),
 *        refreshSeconds, refreshKnown, updateJob, username, host, origin, transport, sessionHours, signedInAt,
 *        serverVersion }
 */
export function accountsViewModel(data, ctx = {}) {
  const now = ctx.now ?? Date.now();
  const c = { ...ctx, now };
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  const serverHidden = c.serverHidden instanceof Set ? c.serverHidden : new Set();
  const localHidden = c.localHidden instanceof Set ? c.localHidden : new Set();
  // Every provider's rows and switch state, even for a provider hidden on Home.
  const unhidden = data ? { ...data, settings: { ...(data.settings || {}), hiddenProviders: [] } } : data;
  const home = dashboardViewModel(unhidden, { ...c, refreshing: false });
  const homeRows = new Map(home.sections.flatMap(section => section.rows.map(row => [row.id, row])));
  const agAccounts = accounts.filter(account => account.provider === 'antigravity');
  const agSection = home.sections.find(section => section.id === 'antigravity');
  const ag = antigravityPolicy(data, c, agSection, agAccounts);

  const providers = PROVIDER_REGISTRY.map(entry => {
    const def = ACCOUNT_KINDS[entry.id];
    const mine = accounts.filter(account => account.provider === entry.id);
    const section = home.sections.find(s => s.id === entry.id);
    const canSwitch = SWITCHABLE.includes(entry.id) && !!section?.canSwitch;
    const hiddenServer = serverHidden.has(entry.id), hiddenLocal = localHidden.has(entry.id);
    const rows = mine.map(account => {
      const homeRow = homeRows.get(account.id);
      const [src, srcSub] = sourceLines(entry.id, account);
      const { status, sampled: sampledLine, sampledTip: sampledLineTip } = rowStatus(account, now);
      // a Claude Open in progress (claude-open.mjs) takes the "sampled" line while it runs
      const opening = entry.id === 'claude' && c.openProgress instanceof Map ? c.openProgress.get(homeRow?.profile || text(account.capabilities?.claudeProfileId)) : null;
      const sampled = opening?.text || sampledLine;
      const sampledTip = opening?.text || sampledLineTip;
      const email = text(account.email) || text(account.label) || 'Account identity unavailable';
      const meta = [planLabel(text(account.plan)), text(account.label) && text(account.label) !== email ? text(account.label) : ''].filter(Boolean).join(' · ');
      const switchable = SWITCHABLE.includes(entry.id);
      return {
        id: account.id, provider: entry.id, email, meta, srcInline: src, status, sampled, sampledTip, src, srcSub,
        active: switchable && !!homeRow?.active, activeLabel: homeRow?.activeLabel || '',
        canActivate: !!homeRow?.canActivate, activateKind: homeRow?.activateKind || '', profile: homeRow?.profile || '',
        activateHint: homeRow?.activateHint || '', confirm: !!homeRow?.confirm, confirmRuns: homeRow?.confirmRuns || [],
        actions: rowActions(entry.id, account, homeRow, canSwitch),
      };
    });
    const count = rows.length;
    return {
      id: entry.id, label: entry.longLabel, kindLabel: def.kindLabel, kindIcon: def.icon, count,
      countText: `${count} ${count === 1 ? 'account' : 'accounts'}`,
      column: COL_A.includes(entry.id) ? 'a' : 'b',
      visible: !hiddenServer && !hiddenLocal,
      // Shown under the toggle while hidden, so it is clear who else follows the choice.
      hiddenNote: hiddenServer ? 'Hidden everywhere' : hiddenLocal ? 'Hidden in this browser' : '',
      toggleEnabled: !hiddenServer || LIVE.visibilityServer,
      toggleTip: hiddenServer && !LIVE.visibilityServer ? 'Hidden by the server setting; changing it from here is coming'
        : 'Saved in this browser. The trays and other browsers follow once the server stores it (coming).',
      switchable: SWITCHABLE.includes(entry.id), canSwitch, slots: def.slots, actsMin: def.actsMin,
      how: def.how, foot: footActions(entry.id, mine),
      empty: count ? '' : `No ${entry.label} accounts yet.`,
      ag: entry.id === 'antigravity' && ag.shown,
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
    signin: signinFacts(c),
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
