// The app frame's sign-in (PLAN step 4): the Leptos frame draws a real HTML form, and this decides every state and
// word of it. It makes the same server calls as bridge.js's sign-in (GET /api/auth/check and /api/auth/setup, POST
// /api/auth/login, /api/auth/setup and /api/auth/logout) and takes its words and rules from auth-view.mjs, unchanged.
// Pure apart from the injected fetch, storage, location and timers (tests/frame-session.test.mjs).
//
// The view it publishes (`onChange(view)`):
//   { signedIn, state, host, username, transportNote, sessionHours, setupForm, codeRequired, busy, nonce,
//     message, messageSub, notice, field, bannerTitle, bannerBody, bannerStrong, retrySeconds, limitSeconds,
//     offlineTitle, offlineStrong, offlineBody, offlineMeta, strength }
// `state` is one of loading, default, connecting, wrong, limited, setup, expired, offline, success.
import {
  strength as passwordStrength,
  validateSetup,
  triesLine,
  limitWindowMinutes,
  limitedView,
  rememberSignIn,
  forgetSignIn,
  lastSeenAt,
  endedReason,
  expiredBanner,
  triesFrom,
  retryFrom,
  loginFailure,
  setupFailure,
  offlineView,
  rememberSessionHours,
  rememberedSessionHours,
} from './auth-view.mjs';
import { transportOf, transportNote } from './accounts-view.mjs';
import { signOutFailureText } from './account-actions.mjs';

/** Offline at launch: try again this often while the page is shown (bridge.js OFFLINE_RETRY_MS). */
export const OFFLINE_RETRY_MS = 15_000;

/** One request, as bridge.js's sendNow: the payload, or a thrown error carrying status, payload, headers and `network`. */
export async function request(fetchImpl, path, options = {}) {
  let response;
  try {
    response = await fetchImpl(path, {
      credentials: 'same-origin',
      ...options,
      headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers },
    });
  } catch (cause) {
    throw Object.assign(new Error('The dashboard did not answer.'), { network: true, status: 0, payload: null, cause });
  }
  const payload = response.status === 204 ? null : await response.json().catch(() => null);
  if (!response.ok) {
    const error = new Error(payload?.error || `Request failed (${response.status}).`);
    error.status = response.status;
    error.payload = payload;
    error.headers = response.headers;
    error.retryAfter = response.headers?.get?.('Retry-After') || response.headers?.get?.('RateLimit-Reset') || '';
    throw error;
  }
  return payload;
}

/**
 * The frame's session. `fetch`, `storage` (localStorage or null), `location` ({ protocol, hostname, host }),
 * `now()`, `setInterval`/`clearInterval` and `visible()` are injected.
 */
export function createFrameSession({
  fetch: fetchImpl,
  storage = null,
  location = { protocol: 'http:', hostname: '', host: '' },
  now = () => Date.now(),
  setInterval: every = (fn, ms) => globalThis.setInterval(fn, ms),
  clearInterval: stop = (id) => globalThis.clearInterval(id),
  visible = () => true,
} = {}) {
  const host = typeof location.host === 'string' ? location.host : '';
  const transport = transportOf(location.protocol, location.hostname);
  const listeners = new Set();
  let sessionHours = rememberedSessionHours(storage);
  let setupInfo = { form: false, codeRequired: false };
  let authCheck = null;
  let username = '';
  let busy = false;
  let checking = false;
  let nonce = 0;
  let offlineTimer = 0;
  let view = { signedIn: false, state: 'loading' };

  const send = (path, options) => request(fetchImpl, path, options);
  const post = (path, body) => send(path, { method: 'POST', body: JSON.stringify(body) });

  function publish(signedIn, state, extra = {}) {
    if (state !== 'offline' && offlineTimer) {
      stop(offlineTimer);
      offlineTimer = 0;
    } else if (state === 'offline' && !offlineTimer) {
      offlineTimer = every(() => {
        if (!view.signedIn && !busy && view.state === 'offline' && !checking && visible()) void check();
      }, OFFLINE_RETRY_MS);
    }
    view = {
      signedIn,
      state,
      host,
      username,
      transportNote: transportNote(transport, authCheck),
      sessionHours,
      setupForm: setupInfo.form,
      codeRequired: setupInfo.codeRequired,
      busy,
      nonce,
      ...extra,
    };
    for (const listener of listeners) listener(view);
  }

  function learnSessionHours(hours) {
    sessionHours = hours;
    rememberSessionHours(storage, hours);
  }

  async function loadSetup() {
    try {
      const setup = await send('/api/auth/setup');
      if (Number.isInteger(setup?.sessionTimeoutHours) && setup.sessionTimeoutHours > 0) learnSessionHours(setup.sessionTimeoutHours);
      setupInfo = { form: typeof setup?.setupCodeRequired === 'boolean', codeRequired: setup?.setupCodeRequired === true };
    } catch {}
  }

  function showSetup(extra = {}) {
    publish(
      false,
      'setup',
      setupInfo.form
        ? { strength: { ...passwordStrength(''), matches: false }, ...extra }
        : {
            bannerTitle: 'Set up sign-in on the server',
            bannerBody: 'Run ai-account-center dashboard auth setup on the dashboard host, then check again here.',
            ...extra,
          }
    );
  }

  function goOffline() {
    const words = offlineView(host, now(), now());
    publish(false, 'offline', {
      message: 'Unable to connect to AI Account Center. Try refreshing this page.',
      offlineTitle: words.title,
      offlineStrong: words.strong,
      offlineBody: words.body,
      offlineMeta: words.meta,
    });
  }

  async function refreshCheck() {
    try {
      const status = await send('/api/auth/check');
      if (status && typeof status === 'object') authCheck = status;
    } catch {}
  }

  /** The sign-in page after a sign-out or an ended session, drawn at once and again if the transport note changed. */
  function showSignedOut(state, extra) {
    publish(false, state, extra);
    const shown = transportNote(transport, authCheck);
    void refreshCheck().then(() => {
      if (!view.signedIn && view.state === state && transportNote(transport, authCheck) !== shown) publish(false, state, extra);
    });
  }

  /** The server no longer knows this browser's session: say whether it ran out, ended early or was revoked. */
  function ended(reason = endedReason(lastSeenAt(storage), sessionHours, now()) || 'ended') {
    forgetSignIn(storage);
    const banner = expiredBanner(reason, sessionHours);
    showSignedOut('expired', { bannerTitle: banner.title, bannerBody: banner.body });
  }

  async function check() {
    if (checking) return;
    checking = true;
    try {
      await loadSetup();
      const status = await send('/api/auth/check');
      authCheck = status && typeof status === 'object' ? status : null;
      username = typeof status?.username === 'string' ? status.username : '';
      if (status?.authenticated === true || status?.authRequired === false) {
        publish(true, 'default');
      } else if (status?.accessMode === 'setup') showSetup();
      else if (status?.signedOutReason === 'revoked') ended('revoked');
      else {
        const reason = endedReason(lastSeenAt(storage), sessionHours, now());
        if (reason) ended(reason);
        else publish(false, 'default');
      }
    } catch (error) {
      if (error?.network || error?.status >= 500) goOffline();
      else publish(false, 'default', { message: 'Unable to connect to AI Account Center. Try refreshing this page.' });
    } finally {
      checking = false;
    }
  }

  function signedIn(name) {
    username = name;
    rememberSignIn(storage, now());
    busy = false;
    publish(false, 'success');
    publish(true, 'default');
  }

  async function signIn({ username: user = '', password = '', remember = true } = {}) {
    if (busy) return;
    if (!user || !password) {
      nonce++;
      publish(false, 'default', { message: 'Enter your username and password.' });
      return;
    }
    busy = true;
    publish(false, 'connecting');
    try {
      const result = await post('/api/auth/login', { username: user, password, rememberMe: remember });
      signedIn(typeof result?.username === 'string' ? result.username : user);
    } catch (error) {
      busy = false;
      nonce++;
      const window = limitWindowMinutes(error.headers);
      if (error.status === 429) {
        const limit = limitedView(retryFrom(error), now());
        publish(false, 'limited', {
          bannerTitle: limit.title,
          bannerBody: limit.body,
          bannerStrong: limit.until,
          retrySeconds: limit.seconds,
          limitSeconds: window * 60,
        });
      } else if (error.status === 401) {
        publish(false, 'wrong', { message: "Username or password isn't right.", messageSub: triesLine(triesFrom(error), window) });
      } else if (error.status === 400 && /not configured/i.test(error.payload?.error || '')) showSetup();
      else if (error.network || error.status >= 500) goOffline();
      else publish(false, 'default', { message: loginFailure(error) });
    }
  }

  async function createSignIn({ username: user = '', password = '', confirm = '', code = '' } = {}) {
    if (busy || !setupInfo.form) return;
    const bad = validateSetup({ username: user, password, confirm, code }, { codeRequired: setupInfo.codeRequired });
    if (bad) {
      nonce++;
      showSetup({ field: bad[0], message: bad[1], strength: { ...passwordStrength(password), matches: !!confirm && confirm === password } });
      return;
    }
    busy = true;
    showSetup();
    try {
      const result = await post('/api/auth/setup', {
        username: user.trim(),
        password,
        ...(setupInfo.codeRequired ? { setupCode: code.trim() } : {}),
      });
      signedIn(typeof result?.username === 'string' ? result.username : user.trim());
    } catch (error) {
      busy = false;
      nonce++;
      if (error.payload?.code === 'already_configured') {
        await check();
        return;
      }
      const [field, message] = setupFailure(error, authCheck);
      showSetup({ field, message });
    }
  }

  /** The setup form's live strength line while the password is typed. */
  function typing(password = '', confirm = '') {
    if (view.state !== 'setup' || !setupInfo.form) return;
    publish(false, 'setup', { ...view, strength: { ...passwordStrength(password), matches: !!confirm && confirm === password } });
  }

  /** Sign out without the dashboard's page: the same call and words as bridge.js's signOut. */
  async function signOut() {
    if (busy) return { ok: false };
    try {
      await post('/api/auth/logout', {});
    } catch (error) {
      if (error?.payload?.code !== 'session_revoked') return { ok: false, failure: signOutFailureText(error) };
    }
    forgetSignIn(storage);
    showSignedOut('default', { notice: true, message: 'Signed out.' });
    return { ok: true };
  }

  /**
   * Sign out when the dashboard's own Sign out never answered: the server says whether that sign-out already ended
   * the session (then only this page follows); only a session still signed in gets this page's one logout.
   */
  async function finishSignOut() {
    if (busy || !view.signedIn) return { ok: true };
    let status;
    try {
      status = await send('/api/auth/check');
    } catch (error) {
      return { ok: false, failure: signOutFailureText(error) };
    }
    if (status?.authenticated !== true && status?.authRequired !== false) {
      forgetSignIn(storage);
      showSignedOut('default', { notice: true, message: 'Signed out.' });
      return { ok: true };
    }
    return signOut();
  }

  /** The dashboard's own page signed out (its Sign out): show the same signed-out page here. */
  function signedOutElsewhere() {
    forgetSignIn(storage);
    showSignedOut('default', { notice: true, message: 'Signed out.' });
  }

  /** The limited state's countdown ran out. */
  function limitOver() {
    if (!view.signedIn && !busy && view.state === 'limited') publish(false, 'default', { notice: true, message: 'Sign-in is open again.' });
  }

  return {
    start: check,
    recheck: () => (!view.signedIn && !busy ? check() : Promise.resolve()),
    signIn,
    createSignIn,
    typing,
    signOut,
    finishSignOut,
    signedOutElsewhere,
    ended,
    limitOver,
    view: () => view,
    onChange(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

/**
 * Installs the session for the Leptos frame: `window.aacFrameSession` holds the calls, and every change is sent as an
 * `aac-session` event whose detail is the view as JSON. Nothing starts until the frame calls `start()`.
 */
export function installFrameSession(win = globalThis) {
  const session = createFrameSession({
    fetch: (...args) => win.fetch(...args),
    storage: (() => {
      try {
        return win.localStorage;
      } catch {
        return null;
      }
    })(),
    location: win.location,
    visible: () => win.document?.visibilityState !== 'hidden',
  });
  session.onChange((view) => {
    try {
      win.dispatchEvent(new win.CustomEvent('aac-session', { detail: JSON.stringify(view) }));
    } catch {}
  });
  win.aacFrameSession = session;
  return session;
}
