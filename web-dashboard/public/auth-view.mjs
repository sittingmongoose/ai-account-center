// The sign-in page's words and rules (c-daylight-atlas/app-auth.js), as pure functions bridge.js uses to build
// the AuthView it hands Slint (ui/shell/signin.slint). Covered by tests/auth-view.test.mjs.
//
// What the server says today (src/web-server/routes/auth-routes.ts and auth-middleware.ts):
// - GET /api/auth/check: accessMode open | login | setup, authenticated, username.
// - GET /api/auth/setup: enabled, configured, sessionTimeoutHours.
// - POST /api/auth/login: 200, 400, 401; express-rate-limit (draft-6 headers) adds RateLimit-Remaining and
//   RateLimit-Policy ("5;w=900") to every answer and Retry-After to a 429. Five tries per 15 minutes per address.
// The first-run form appears only when GET /api/auth/setup reports `setupCodeRequired` as a boolean, the signal
// CONTRACT-auth-devices.md section 4 adds together with POST /api/auth/setup. Until then the setup state shows the
// command that sets sign-in up on the server.

const clock = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const bytes = value => new TextEncoder().encode(value).length;

/** Password strength, a hint and never a gate (the server's rule is 8 code points to 72 bytes). lv 0..5. */
export function strength(password) {
  const pw = String(password || '');
  if (!pw) return { lv: 0, pct: 0, word: '', hint: 'At least 8 characters. Longer is stronger.' };
  const len = Array.from(pw).length;
  const kinds = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^A-Za-z0-9]/].filter(r => r.test(pw)).length;
  const plain = /^(.)\1+$/.test(pw) || /^(password|12345678|qwerty|letmein|admin)/i.test(pw);
  if (len < 8) return { lv: 1, pct: 14 + len * 2, word: 'Too short', hint: `${8 - len} more character${8 - len === 1 ? '' : 's'} needed` };
  if (bytes(pw) > 72) return { lv: 1, pct: 100, word: 'Too long', hint: 'Keep it within 72 bytes' };
  if (plain) return { lv: 2, pct: 30, word: 'Weak', hint: 'Easy to guess; avoid common words' };
  const score = (len >= 20 ? 3 : len >= 14 ? 2 : len >= 10 ? 1 : 0) + (kinds >= 3 ? 1 : 0) + (kinds >= 2 ? 0.5 : 0);
  if (score >= 3) return { lv: 5, pct: 100, word: 'Strong', hint: `${len} characters, ${kinds} kind${kinds === 1 ? '' : 's'}` };
  if (score >= 2) return { lv: 4, pct: 80, word: 'Good', hint: `${len} characters; 14 or more is stronger` };
  if (score >= 1) return { lv: 3, pct: 58, word: 'Fair', hint: 'Add length: a few words with dashes works well' };
  return { lv: 2, pct: 36, word: 'Weak', hint: 'Add length or mix letters, digits and symbols' };
}

/** First-run form check. Returns [field, message] for the first problem, or null. */
export function validateSetup({ username = '', password = '', confirm = '', code = '' } = {}, { codeRequired = false } = {}) {
  const user = String(username).trim();
  if (!/^[A-Za-z][A-Za-z0-9_-]{2,63}$/.test(user)) return ['user', 'Start the username with a letter; use 3 or more letters, numbers, - or _.'];
  if (Array.from(String(password)).length < 8) return ['pass', 'Use at least 8 characters for the password.'];
  if (bytes(String(password)) > 72) return ['pass', 'Keep the password within 72 bytes; bcrypt ignores the rest.'];
  if (confirm !== password) return ['confirm', "The confirmation doesn't match the password."];
  if (codeRequired) {
    const k = String(code).replace(/[^A-Za-z0-9]/g, '');
    if (!k) return ['code', 'Enter the setup code from the server. It proves you can reach that machine.'];
    if (k.length !== 8) return ['code', "That setup code isn't right. It has 8 letters and digits."];
  }
  return null;
}

const header = (headers, name) => { try { return headers?.get?.(name) ?? ''; } catch { return ''; } };
/** The limiter's window in minutes from RateLimit-Policy ("5;w=900"); 15 when it is not sent. */
export function limitWindowMinutes(headers) {
  const match = /w=(\d+)/.exec(header(headers, 'RateLimit-Policy'));
  const seconds = match ? Number(match[1]) : 900;
  return Math.max(1, Math.round(seconds / 60));
}
/** Tries left from RateLimit-Remaining, or null when the server does not say. */
export function triesLeft(headers) {
  const raw = header(headers, 'RateLimit-Remaining');
  const n = raw === '' || raw === null ? NaN : Number(raw);
  return Number.isInteger(n) && n >= 0 ? n : null;
}
export function triesLine(left, windowMinutes = 15) {
  if (left === null || left === undefined) return '';
  if (left === 0) return `That was the last try; the next one pauses sign-in for ${windowMinutes} minutes.`;
  return `${left} ${left === 1 ? 'try' : 'tries'} left before sign-in pauses for ${windowMinutes} minutes.`;
}
/** Seconds to wait from Retry-After (or RateLimit-Reset); null when the server does not say. */
export function retrySeconds(headers) {
  for (const name of ['Retry-After', 'RateLimit-Reset']) {
    const raw = header(headers, name);
    if (!raw) continue;
    const n = Number(raw);
    if (Number.isFinite(n) && n > 0) return Math.ceil(n);
    const at = Date.parse(raw);
    if (Number.isFinite(at)) return Math.max(1, Math.ceil((at - Date.now()) / 1000));
  }
  return null;
}
/** The rate-limited banner: "Try again in 15 minutes", the clock time it opens, and the countdown's start. */
export function limitedView(seconds, now = Date.now()) {
  if (!Number.isInteger(seconds) || seconds <= 0) {
    return { title: 'Try again later', body: 'Too many tries from this address. Sign-in opens again after a short pause.', until: '', seconds: 0 };
  }
  const minutes = Math.ceil(seconds / 60);
  const title = seconds < 60 ? `Try again in ${seconds} seconds` : `Try again in ${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`;
  const until = clock.format(new Date(now + seconds * 1000));
  return { title, body: 'Too many tries from this address. Sign-in opens again at', until, seconds };
}

const SESSION_KEY = 'aac-signed-in-at';
/** Remember when this browser signed in (a per-viewer convenience), so the page can say a session ended. */
export function rememberSignIn(storage, now = Date.now()) { try { storage?.setItem(SESSION_KEY, String(now)); } catch {} }
export function forgetSignIn(storage) { try { storage?.removeItem(SESSION_KEY); } catch {} }
export function signedInAt(storage) {
  try { const n = Number(storage?.getItem(SESSION_KEY)); return Number.isFinite(n) && n > 0 ? n : null; } catch { return null; }
}
/**
 * When the server reports no session but this browser had one: why it ended. 'expired' after the session
 * length, 'ended' before it (the server restarted or signed it out), null when this browser never signed in.
 */
export function endedReason(at, hours = 24, now = Date.now()) {
  if (!Number.isFinite(at)) return null;
  return now - at >= hours * 3_600_000 - 60_000 ? 'expired' : 'ended';
}
export function expiredBanner(reason, hours = 24) {
  return reason === 'expired'
    ? { title: `Signed out after ${hours} hours`, body: `Sessions on this dashboard last ${hours} hours. Your trays stayed connected.` }
    : { title: 'Your session ended', body: 'The dashboard signed this browser out, for example after it restarted. Your trays stayed connected.' };
}
