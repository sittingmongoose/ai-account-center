// How this browser reaches the dashboard, and the sign-in card's network note. Small on purpose: the app frame's sign-in
// (frame-session.mjs) imports it before its runtime starts, so it must not pull in the accounts view
// (accounts-view.mjs re-exports these for its own callers).

/** How this browser reaches the dashboard: https, loopback (this computer) or plain http on the network. */
export function transportOf(protocol, hostname) {
  if (protocol === 'https:') return 'https';
  const host = String(hostname || '').replace(/^\[|\]$/g, '').toLowerCase();
  if (host === 'localhost' || host === '::1' || /^127\./.test(host)) return 'loopback';
  return 'http';
}
export const TRANSPORT_TEXT = {
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
  if (check && check.trustedLocalNetwork === false) return 'This address is plain HTTP and local network trust is off, so the password crosses your network unencrypted. Password changes, keys and tray pairing work once trust is turned on from the dashboard computer (Accounts & Settings, Sign-in & connection).';
  return 'This address is plain HTTP, so the password crosses your network unencrypted.';
}
