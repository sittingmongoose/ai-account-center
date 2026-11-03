#!/usr/bin/env node
// browser_auth.mjs: the browser half of AI Account Center's Claude sign-in renewal.
//
// Each Claude account has its own Chrome profile (a separate --user-data-dir) that stays signed in
// to claude.ai, so approving a renewal needs no person. This helper drives that Chrome over the
// DevTools protocol, using Node's built-in WebSocket (Node 22 or later); it needs no packages.
//
//   node browser_auth.mjs authorize    <email> --profiles <dir>     authorize URL on stdin
//   node browser_auth.mjs request-link <email> --profiles <dir>     ask claude.ai to email a sign-in link
//   node browser_auth.mjs open-link    <email> --profiles <dir> [--wait-minutes N]   link on stdin
//   node browser_auth.mjs front        <email> --profiles <dir>     bring that account's window forward
//   node browser_auth.mjs session      <email> --profiles <dir> [--visit]   claude.ai session end (cookie expiry)
//   node browser_auth.mjs close        <email> --profiles <dir>     quit that account's Chrome if it runs
//
// stdout carries JSON lines only: {"event": ...} while it runs, then one {"result": ...} line.
// The authorize result is the only line that carries a secret (the one-use code); the caller reads
// it from the pipe and never logs it. URLs and links come in on stdin, never on the command line,
// so other processes cannot read them from the process list. stderr carries plain progress words
// with no codes, links or URLs.
import { spawn, execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import { homedir, platform } from 'node:os';
import { join } from 'node:path';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const log = (message) => process.stderr.write(`[browser] ${message}\n`);
const emit = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
/** A command's outcome; the top level reports it once the command's tab is closed. */
const done = (result, extra = {}) => ({ result, extra });
const finish = (result, extra = {}) => {
  emit({ result, ...extra });
  process.exit(result === 'code' || result === 'sent' || result === 'signed_in' || result === 'ok' ? 0 : 1);
};

const args = process.argv.slice(2);
const command = args[0];
const email = (args[1] || '').trim().toLowerCase();
const option = (name, fallback) => {
  const index = args.indexOf(name);
  return index > 0 && index + 1 < args.length ? args[index + 1] : fallback;
};
if (!command || !/^[^\s@/\\]+@[^\s@/\\]+\.[^\s@/\\]+$/.test(email)) {
  process.stderr.write('usage: browser_auth.mjs <authorize|request-link|open-link|front> <email> --profiles <dir>\n');
  process.exit(2);
}
if (typeof WebSocket !== 'function') finish('error', { reason: 'node_too_old', detail: 'Node 22 or later is needed (built-in WebSocket).' });

const profilesDir = option('--profiles', join(homedir(), '.ccs', 'claude-signin', 'chrome'));
const waitMinutes = Math.max(1, Math.min(240, Number(option('--wait-minutes', '20')) || 20));
let profileDir = join(profilesDir, email.replace(/[^a-z0-9@._-]+/g, '_'));

async function readStdinLine() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8').split(/\r?\n/)[0].trim();
}

// ------------------------------------------------------------------ Chrome per account

function chromeLaunch(flags) {
  const os = platform();
  // -g -j: start in the background, hidden; a window comes forward only when a person is needed.
  if (os === 'darwin') return ['open', ['-g', '-j', '-na', 'Google Chrome', '--args', ...flags]];
  if (os === 'win32') {
    const roots = [process.env.ProgramFiles, process.env['ProgramFiles(x86)'], process.env.LOCALAPPDATA].filter(Boolean);
    for (const root of roots) {
      const exe = join(root, 'Google', 'Chrome', 'Application', 'chrome.exe');
      if (existsSync(exe)) return [exe, flags];
    }
    return null;
  }
  for (const name of ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser']) {
    try {
      const found = execFileSync('which', [name], { encoding: 'utf8' }).trim();
      if (found) return [found, flags];
    } catch {
      /* try the next name */
    }
  }
  return null;
}

/** Chrome writes its DevTools port and browser path here; it names exactly this profile's browser. */
function activePort() {
  try {
    const [port, browserPath] = readFileSync(join(profileDir, 'DevToolsActivePort'), 'utf8').split(/\r?\n/);
    if (/^\d{2,5}$/.test(port) && browserPath?.startsWith('/devtools/browser/')) return { port: Number(port), browserPath };
  } catch {
    /* not running */
  }
  return null;
}

async function browserEndpoint() {
  const active = activePort();
  if (!active) return null;
  try {
    const reply = await fetch(`http://127.0.0.1:${active.port}/json/version`, { signal: AbortSignal.timeout(3000) });
    const info = await reply.json();
    const url = String(info.webSocketDebuggerUrl || '');
    if (url.endsWith(active.browserPath)) return url;
  } catch {
    /* stale file from a Chrome that has exited */
  }
  return null;
}

/**
 * A profile kept by the claude-relogin prototype (~/.claude-relogin/chrome/<email>) is used where it
 * is when this account has no profile of its own yet, so an account already signed in there stays
 * signed in and the prototype keeps working.
 */
function adoptPrototypeProfile() {
  if (existsSync(profileDir)) return;
  const old = join(homedir(), '.claude-relogin', 'chrome', email.replace(/[^a-z0-9@.]+/g, '_'));
  try {
    if (statSync(join(old, 'Local State')).isFile()) profileDir = old;
  } catch {
    /* none to adopt */
  }
}

async function ensureChrome() {
  adoptPrototypeProfile();
  let url = await browserEndpoint();
  if (url) return url;
  mkdirSync(profileDir, { recursive: true });
  const launch = chromeLaunch([
    `--user-data-dir=${profileDir}`,
    '--remote-debugging-port=0',
    '--remote-allow-origins=http://127.0.0.1',
    '--no-first-run',
    '--no-default-browser-check',
    '--window-size=1100,900',
    'about:blank',
  ]);
  if (!launch) finish('error', { reason: 'no_chrome', detail: 'Google Chrome (or Chromium) is not installed here.' });
  const child = spawn(launch[0], launch[1], { stdio: 'ignore', detached: true });
  child.on('error', () => {});
  child.unref();
  for (let i = 0; i < 60 && !url; i++) {
    await sleep(500);
    url = await browserEndpoint();
  }
  if (!url) finish('error', { reason: 'chrome_no_devtools', detail: 'Chrome started but its DevTools port never answered.' });
  return url;
}

// ------------------------------------------------------------------ minimal DevTools client

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.next = 1;
    this.pending = new Map();
    this.ws.onmessage = (message) => {
      const data = JSON.parse(String(message.data));
      const waiter = data.id && this.pending.get(data.id);
      if (!waiter) return;
      this.pending.delete(data.id);
      if (data.error) waiter.reject(new Error(data.error.message || 'devtools error'));
      else waiter.resolve(data.result);
    };
    this.ws.onclose = () => {
      for (const waiter of this.pending.values()) waiter.reject(new Error('devtools closed'));
      this.pending.clear();
    };
  }
  open() {
    return new Promise((resolve, reject) => {
      this.ws.onopen = () => resolve();
      this.ws.onerror = () => reject(new Error('devtools connection failed'));
    });
  }
  send(method, params = {}, sessionId, timeoutMs = 30000) {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`devtools ${method} timed out`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (value) => (clearTimeout(timer), resolve(value)),
        reject: (error) => (clearTimeout(timer), reject(error)),
      });
      this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }
  close() {
    try {
      this.ws.close();
    } catch {
      /* already closed */
    }
  }
}

class Tab {
  constructor(cdp, targetId, sessionId) {
    Object.assign(this, { cdp, targetId, sessionId });
  }
  static async open(cdp) {
    const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    await cdp.send('Page.enable', {}, sessionId);
    return new Tab(cdp, targetId, sessionId);
  }
  async eval(expression, timeoutMs = 15000) {
    const reply = await this.cdp.send(
      'Runtime.evaluate',
      { expression, returnByValue: true, awaitPromise: true },
      this.sessionId,
      timeoutMs
    );
    if (reply.exceptionDetails) throw new Error('page script failed');
    return reply.result?.value;
  }
  async go(url) {
    await this.cdp.send('Page.navigate', { url }, this.sessionId);
    await this.settle();
  }
  async settle(ms = 20000) {
    const end = Date.now() + ms;
    await sleep(500);
    while (Date.now() < end) {
      const ready = await this.eval('document.readyState').catch(() => null);
      if (ready === 'complete' || ready === 'interactive') break;
      await sleep(300);
    }
    await sleep(1500);
  }
  url() {
    return this.eval('location.href').catch(() => '');
  }
  text() {
    return this.eval('document.body ? document.body.innerText : ""').catch(() => '');
  }
  async front() {
    await this.cdp.send('Page.bringToFront', {}, this.sessionId).catch(() => {});
    try {
      const { windowId } = await this.cdp.send('Browser.getWindowForTarget', { targetId: this.targetId });
      await this.cdp.send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'normal' } });
    } catch {
      /* window state is best effort */
    }
    raiseApp();
  }
  close() {
    return this.cdp.send('Target.closeTarget', { targetId: this.targetId }).catch(() => {});
  }
}

/** Brings this profile's Chrome process to the front of the desktop (Mac: by process id). */
function raiseApp() {
  if (platform() !== 'darwin') return;
  try {
    const ps = execFileSync('ps', ['-axo', 'pid=,command='], { encoding: 'utf8', maxBuffer: 1 << 24 });
    const line = ps.split('\n').find((row) => row.includes(`--user-data-dir=${profileDir}`) && !row.includes('--type='));
    const pid = line ? Number(line.trim().split(/\s+/)[0]) : 0;
    const script = pid
      ? `tell application "System Events" to set frontmost of (first process whose unix id is ${pid}) to true`
      : 'tell application "Google Chrome" to activate';
    execFileSync('osascript', ['-e', script], { timeout: 10000, stdio: 'ignore' });
  } catch {
    try {
      execFileSync('osascript', ['-e', 'tell application "Google Chrome" to activate'], { timeout: 10000, stdio: 'ignore' });
    } catch {
      /* best effort */
    }
  }
}

// ------------------------------------------------------------------ claude.ai pages

const ON_LOGIN = /^https:\/\/claude\.ai\/(login|magic-link)/;
// claude.ai pages carry an invisible hCaptcha all the time; only a challenge a person can see counts.
const CHALLENGE_SCRIPT = `(() => [...document.querySelectorAll('iframe')].some((f) => {
  const src = f.src || '';
  if (!/hcaptcha\\.com|challenges\\.cloudflare\\.com/.test(src)) return false;
  const r = f.getBoundingClientRect();
  if (r.width < 60 || r.height < 60) return false;
  return typeof f.checkVisibility === 'function'
    ? f.checkVisibility({ visibilityProperty: true, opacityProperty: true })
    : getComputedStyle(f).visibility !== 'hidden';
}))()`;

const INTERSTITIAL = /performing security verification|verify you are human|just a moment\.\.\./i;

/** A check only a person may pass: a visible hCaptcha/Turnstile, or Cloudflare's interstitial page. */
async function challenged(tab) {
  if (await tab.eval(CHALLENGE_SCRIPT).catch(() => false)) return true;
  const title = await tab.eval('document.title').catch(() => '');
  return INTERSTITIAL.test(title) || INTERSTITIAL.test((await tab.text()).slice(0, 400));
}

/**
 * Waits while a person-only check is on screen: the window comes forward once, the caller hears
 * {"event":"captcha"}, and this returns true once the check has gone (false when time ran out).
 */
async function waitOutChallenge(tab, state) {
  if (!(await challenged(tab))) return true;
  if (!state.told) {
    state.told = true;
    await tab.front();
    emit({ event: 'captcha' });
    log(`claude.ai is showing a check only a person may pass for ${email}; waiting up to ${waitMinutes} min`);
  }
  const end = Date.now() + waitMinutes * 60000;
  while (Date.now() < end) {
    await sleep(3000);
    if (!(await challenged(tab))) {
      emit({ event: 'captcha_cleared' });
      await sleep(2000);
      return true;
    }
  }
  return false;
}

const clickByText = (label) => `(() => {
  const wanted = ${JSON.stringify(label.toLowerCase())};
  const button = [...document.querySelectorAll('button, [role=button], a')]
    .find((el) => (el.innerText || el.textContent || '').trim().toLowerCase() === wanted && !el.disabled);
  if (!button) return false;
  button.scrollIntoView({ block: 'center' });
  button.click();
  return true;
})()`;

async function authorize() {
  const url = await readStdinLine();
  if (!/^https:\/\/claude\.(com|ai)\/[^\s]*oauth\/authorize\?/.test(url)) return done('error', { reason: 'bad_url' });
  const cdp = new Cdp(await ensureChrome());
  await cdp.open();
  const tab = await Tab.open(cdp);
  try {
    await tab.go(url);
    let found = false;
    const human = { told: false };
    for (let i = 0; i < 40 && !found; i++) {
      if (!(await waitOutChallenge(tab, human))) return done('captcha_timeout');
      const href = await tab.url();
      if (ON_LOGIN.test(href)) return done('signed_out');
      found = await tab.eval(`[...document.querySelectorAll('button')].some((b) => b.innerText.trim() === 'Authorize')`).catch(() => false);
      if (!found) await sleep(1500);
    }
    if (!found) {
      const body = await tab.text();
      if (process.env.BROWSER_AUTH_DEBUG) {
        log('page ' + (await tab.url()).split('?')[0]);
        log('buttons ' + JSON.stringify(await tab.eval(`[...document.querySelectorAll('button')].map((b) => b.innerText.trim()).slice(0, 20)`).catch(() => [])));
        log('text ' + JSON.stringify(body.slice(0, 300)));
      }
      if (/continue with (email|google)|log in|sign in/i.test(body) && !/logged in as/i.test(body)) return done('signed_out');
      return done('error', { reason: 'no_authorize_button' });
    }
    const who = (await tab.text()).match(/Logged in as\s+(\S+@\S+)/i);
    const signedInAs = who ? who[1].replace(/[.,;:]+$/, '').toLowerCase() : '';
    if (signedInAs !== email) return done('wrong_account', { signedInAs: signedInAs || null });
    // Authorize can take 15 s or more to answer; reloading cancels it, so wait long before one retry.
    let reached = false;
    for (let attempt = 0; attempt < 2 && !reached; attempt++) {
      if (attempt) {
        log('Authorize did not finish in 90 s; trying once more');
        await tab.go(url);
      }
      await sleep(2500);
      await tab.eval(clickByText('Authorize'));
      for (let i = 0; i < 90 && !reached; i++) {
        await sleep(1000);
        reached = /\/oauth\/code\/callback/.test(await tab.url());
        if (!reached && !(await waitOutChallenge(tab, human))) return done('captcha_timeout');
      }
    }
    if (!reached) return done('error', { reason: 'authorize_timeout' });
    let code = null;
    for (let i = 0; i < 30 && !code; i++) {
      await sleep(1000);
      code = await tab
        .eval(`(() => {
          const re = /[A-Za-z0-9_-]{20,}#[A-Za-z0-9_-]{20,}/;
          for (const el of document.querySelectorAll('input,textarea,code,pre')) {
            const v = el.value || el.textContent || '';
            if (re.test(v)) return v.match(re)[0];
          }
          const m = (document.body ? document.body.innerText : '').match(re);
          return m ? m[0] : null;
        })()`)
        .catch(() => null);
    }
    if (!code) return done('error', { reason: 'no_code' });
    await tab.go('about:blank').catch(() => {});
    return done('code', { code });
  } finally {
    await tab.close();
    cdp.close();
  }
}

async function requestLink() {
  const cdp = new Cdp(await ensureChrome());
  await cdp.open();
  const tab = await Tab.open(cdp);
  try {
    await tab.go('https://claude.ai/login');
    const human = { told: false };
    if (!(await waitOutChallenge(tab, human))) return done('captcha_timeout');
    if (!ON_LOGIN.test(await tab.url())) return done('signed_in');
    let ready = false;
    for (let i = 0; i < 30 && !ready; i++) {
      ready = await tab.eval(`!!document.querySelector('input[type=email]')`).catch(() => false);
      if (!ready) {
        if (!(await waitOutChallenge(tab, human))) return done('captcha_timeout');
        await sleep(1000);
      }
    }
    if (!ready) return done('error', { reason: 'no_email_box' });
    await tab.eval(`(() => {
      const box = document.querySelector('input[type=email]');
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
      box.focus();
      setter.call(box, ${JSON.stringify(email)});
      box.dispatchEvent(new Event('input', { bubbles: true }));
      box.dispatchEvent(new Event('change', { bubbles: true }));
      return true;
    })()`);
    await sleep(800);
    if (!(await tab.eval(clickByText('Continue with email')))) return done('error', { reason: 'no_continue_button' });
    for (let i = 0; i < 15; i++) {
      await sleep(1000);
      if (!(await waitOutChallenge(tab, human))) return done('captcha_timeout');
      if (/check your (email|inbox)|sent|magic link|verification/i.test(await tab.text())) break;
    }
    return done('sent');
  } finally {
    await tab.close();
    cdp.close();
  }
}

async function openLink() {
  const link = await readStdinLine();
  if (!/^https:\/\/claude\.ai\/magic-link#\S+$/.test(link)) return done('error', { reason: 'bad_link' });
  const cdp = new Cdp(await ensureChrome());
  await cdp.open();
  const tab = await Tab.open(cdp);
  let keepOpen = false;
  try {
    await tab.go(link);
    const end = Date.now() + waitMinutes * 60000;
    let told = false;
    while (Date.now() < end) {
      await sleep(2000);
      const href = await tab.url();
      if (href.startsWith('https://claude.ai/') && !ON_LOGIN.test(href)) return done('signed_in');
      if (!told && (await challenged(tab))) {
        told = true;
        await tab.front();
        emit({ event: 'captcha' });
        log(`claude.ai is showing a CAPTCHA for ${email}; waiting up to ${waitMinutes} min for a person`);
      }
    }
    // Leave the page where it is: a person can still finish it, and the next run finds the profile signed in.
    keepOpen = told;
    return done(told ? 'captcha_timeout' : 'timeout');
  } finally {
    if (!keepOpen) await tab.close();
    cdp.close();
  }
}

async function front() {
  const cdp = new Cdp(await ensureChrome());
  await cdp.open();
  const { targetInfos } = await cdp.send('Target.getTargets');
  const page = targetInfos.find((t) => t.type === 'page' && /claude\.ai/.test(t.url)) || targetInfos.find((t) => t.type === 'page');
  if (page) {
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId: page.targetId, flatten: true });
    await new Tab(cdp, page.targetId, sessionId).front();
  } else raiseApp();
  cdp.close();
  return done('ok');
}

/**
 * When this account's claude.ai session ends: the expiry of its session cookie, read from Chrome's
 * cookie store. Only the cookie's name and expiry are read, never its value. With --visit it first
 * opens claude.ai briefly (a person-only check is not waited for) to learn whether a visit extends
 * the session.
 */
async function session() {
  adoptPrototypeProfile();
  const running = !!(await browserEndpoint()) || existsSync(profileDir);
  if (!running) return done('ok', { signedIn: false, sessionEndsAt: null, profile: false });
  const cdp = new Cdp(await ensureChrome());
  await cdp.open();
  const read = async () => {
    const { cookies } = await cdp.send('Storage.getCookies');
    const key = cookies.find((c) => c.name === 'sessionKey' && /(^|\.)claude\.ai$/.test(c.domain));
    const ends = key && key.expires > 0 ? new Date(key.expires * 1000).toISOString() : null;
    return { signedIn: !!key && (!ends || Date.parse(ends) > Date.now()), sessionEndsAt: ends };
  };
  try {
    const before = await read();
    if (!args.includes('--visit') || !before.signedIn) return done('ok', { ...before, profile: true });
    const tab = await Tab.open(cdp);
    let challengedVisit = false;
    try {
      await tab.go('https://claude.ai/new');
      await sleep(4000);
      challengedVisit = await challenged(tab);
    } finally {
      await tab.close();
    }
    const after = await read();
    return done('ok', { ...after, profile: true, visited: true, challenged: challengedVisit, before: before.sessionEndsAt });
  } finally {
    cdp.close();
  }
}

/** Quits this account's Chrome (cookies stay on disk); nothing to do when it is not running. */
async function close() {
  const url = await browserEndpoint();
  if (!url) return done('ok', { closed: false });
  const cdp = new Cdp(url);
  await cdp.open();
  await cdp.send('Browser.close').catch(() => {});
  cdp.close();
  return done('ok', { closed: true });
}

const commands = { authorize, 'request-link': requestLink, 'open-link': openLink, front, session, close };
if (!commands[command]) finish('error', { reason: 'unknown_command' });
adoptPrototypeProfile();
// Each command returns its result; its own finally has closed the tab before the process exits.
commands[command]()
  .then((outcome) => finish(outcome.result, outcome.extra))
  .catch((error) => finish('error', { reason: 'exception', detail: String(error?.message || error).slice(0, 160) }));
