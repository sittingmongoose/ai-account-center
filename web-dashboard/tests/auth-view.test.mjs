import test from 'node:test';
import assert from 'node:assert/strict';
import {
  strength, validateSetup, limitWindowMinutes, triesLeft, triesLine, retrySeconds, limitedView,
  rememberSignIn, forgetSignIn, signedInAt, endedReason, expiredBanner,
} from '../public/auth-view.mjs';

const headers = values => ({ get: name => values[name] ?? null });

test('password strength is a hint with the concept levels', () => {
  assert.deepEqual(strength(''), { lv: 0, pct: 0, word: '', hint: 'At least 8 characters. Longer is stronger.' });
  assert.equal(strength('abc').word, 'Too short');
  assert.equal(strength('abc').hint, '5 more characters needed');
  assert.equal(strength('password123').word, 'Weak');
  assert.equal(strength('abcdefghij').word, 'Fair');
  assert.equal(strength('abcdefghijklmn').word, 'Good');
  assert.equal(strength('summit-ledger-42').word, 'Strong');
  assert.equal(strength('Summit-Ledger-42-Atlas!').word, 'Strong');
  assert.equal(strength('x'.repeat(73)).word, 'Too long');
});

test('the first-run form checks the username, password, confirmation and code', () => {
  const ok = { username: 'jared', password: 'summit-ledger-42', confirm: 'summit-ledger-42', code: 'K7QF-2MXD' };
  assert.equal(validateSetup(ok), null);
  assert.equal(validateSetup(ok, { codeRequired: true }), null);
  assert.equal(validateSetup({ ...ok, username: '1x' })[0], 'user');
  assert.equal(validateSetup({ ...ok, password: 'short', confirm: 'short' })[0], 'pass');
  assert.equal(validateSetup({ ...ok, confirm: 'other' })[0], 'confirm');
  assert.equal(validateSetup({ ...ok, code: '' }, { codeRequired: true })[0], 'code');
  assert.equal(validateSetup({ ...ok, code: 'ABC' }, { codeRequired: true })[1], "That setup code isn't right. It has 8 letters and digits.");
});

test('tries left and the pause come from the login limiter headers', () => {
  const h = headers({ 'RateLimit-Policy': '5;w=900', 'RateLimit-Remaining': '3' });
  assert.equal(limitWindowMinutes(h), 15);
  assert.equal(triesLeft(h), 3);
  assert.equal(triesLine(3, 15), '3 tries left before sign-in pauses for 15 minutes.');
  assert.equal(triesLine(1, 15), '1 try left before sign-in pauses for 15 minutes.');
  assert.match(triesLine(0, 15), /^That was the last try/);
  assert.equal(triesLine(null), '');
  assert.equal(triesLeft(headers({})), null);
  assert.equal(limitWindowMinutes(headers({})), 15);
  assert.equal(retrySeconds(headers({ 'Retry-After': '877' })), 877);
  assert.equal(retrySeconds(headers({ 'RateLimit-Reset': '30' })), 30);
  assert.equal(retrySeconds(headers({})), null);
});

test('the rate-limited banner says when sign-in opens again, and never invents a time', () => {
  const now = Date.parse('2026-10-02T09:00:00Z');
  const view = limitedView(877, now);
  assert.equal(view.title, 'Try again in 15 minutes');
  assert.equal(view.seconds, 877);
  assert.ok(view.until.length > 0);
  assert.equal(limitedView(40, now).title, 'Try again in 40 seconds');
  assert.equal(limitedView(null, now).title, 'Try again later');
  assert.equal(limitedView(null, now).until, '');
});

test('a session that ended is told apart from one that ran out', () => {
  const store = new Map();
  const storage = { setItem: (k, v) => store.set(k, v), getItem: k => store.get(k) ?? null, removeItem: k => store.delete(k) };
  const now = Date.parse('2026-10-02T09:00:00Z');
  assert.equal(signedInAt(storage), null);
  rememberSignIn(storage, now - 25 * 3_600_000);
  assert.equal(endedReason(signedInAt(storage), 24, now), 'expired');
  assert.equal(endedReason(now - 3_600_000, 24, now), 'ended');
  assert.equal(endedReason(null, 24, now), null);
  forgetSignIn(storage);
  assert.equal(signedInAt(storage), null);
  assert.equal(expiredBanner('expired', 24).title, 'Signed out after 1 day');
  assert.equal(expiredBanner('expired', 720).title, 'Signed out after 30 days');
  assert.equal(expiredBanner('expired', 8760).title, 'Signed out after 1 year');
  assert.equal(expiredBanner('expired', 36).title, 'Signed out after 36 hours');
  assert.equal(expiredBanner('ended', 24).title, 'Your session ended');
  // storage that throws (private windows) is not an error
  const broken = { setItem() { throw new Error('no'); }, getItem() { throw new Error('no'); }, removeItem() { throw new Error('no'); } };
  rememberSignIn(broken); forgetSignIn(broken);
  assert.equal(signedInAt(broken), null);
});

test('the sign-in page reads tries and the pause from the answer, falling back to the limiter headers', async () => {
  const { triesFrom, retryFrom, loginFailure, setupFailure, expiredBanner: banner } = await import('../public/auth-view.mjs');
  const headers = map => ({ get: name => map[name] ?? null });
  assert.equal(triesFrom({ payload: { code: 'invalid_credentials', triesLeft: 3 }, headers: headers({ 'RateLimit-Remaining': '4' }) }), 3);
  assert.equal(triesFrom({ payload: {}, headers: headers({ 'RateLimit-Remaining': '2' }) }), 2);
  assert.equal(triesFrom({ payload: null, headers: headers({}) }), null);
  assert.equal(retryFrom({ payload: { code: 'rate_limited', retryAfterSeconds: 812.4 }, headers: headers({ 'Retry-After': '900' }) }), 813);
  assert.equal(retryFrom({ payload: {}, headers: headers({ 'Retry-After': '900' }) }), 900);
  // a 500 is never a wrong password
  assert.equal(loginFailure({ status: 500, payload: { error: 'Session error' } }), 'Sign-in failed on the dashboard. Try again.');
  assert.equal(loginFailure({ network: true }), 'The dashboard did not answer. Check the connection and try again.');
  // the revoked banner (sign out other browsers, a password change or sign out all devices)
  assert.deepEqual(banner('revoked').title, 'Signed out from another browser');
  // first-run refusals
  const refusal = (code, extra = {}) => ({ status: 403, payload: { code, ...extra } });
  assert.deepEqual(setupFailure(refusal('setup_code_invalid', { triesLeft: 2 })), ['code', "That setup code isn't right. It has 8 letters and digits. 2 tries left."]);
  assert.equal(setupFailure(refusal('setup_code_required'))[0], 'code');
  assert.match(setupFailure(refusal('secure_transport_required'), { trustedLocalNetwork: false })[1], /local network trust, which is off/);
  assert.match(setupFailure(refusal('secure_transport_required'), { trustedLocalNetwork: true })[1], /not on your trusted local network/);
  assert.doesNotMatch(setupFailure(refusal('secure_transport_required'), null)[1], /HTTPS|tunnel/);
  assert.equal(setupFailure(refusal('invalid_username'))[0], 'user');
  assert.match(setupFailure(refusal('weak_password', { reason: 'too_long' }))[1], /72 bytes/);
  assert.match(setupFailure(refusal('managed_by_env'))[1], /environment variables/);
  assert.match(setupFailure({ status: 500, payload: null })[1], /could not be created on the dashboard/);
});

test('the Slint sign-in value carries Remember me; two parts remember', async () => {
  const { parseLoginValue } = await import('../public/auth-view.mjs');
  assert.deepEqual(parseLoginValue('owner\ns3cret\n1'), { username: 'owner', password: 's3cret', remember: true });
  assert.deepEqual(parseLoginValue('owner\ns3cret\n0'), { username: 'owner', password: 's3cret', remember: false });
  assert.deepEqual(parseLoginValue('  owner  \npass'), { username: 'owner', password: 'pass', remember: true });
  assert.deepEqual(parseLoginValue(''), { username: '', password: '', remember: true });
});
