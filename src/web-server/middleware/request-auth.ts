import type { Request } from 'express';

/**
 * Who made a request (CONTRACT-auth-devices section 6): a signed-in browser
 * session, a paired device token, or nobody. A device token is checked first
 * and never falls back to a cookie session. Device tokens are set by the
 * auth-devices middleware as `req.auth = { kind: 'device', ... }`; until it
 * ships, no request is a device.
 */
export type RequestAuthKind = 'session' | 'device' | null;

export function authKind(req: Request): RequestAuthKind {
  const auth = (req as Request & { auth?: { kind?: unknown } }).auth;
  if (auth && typeof auth === 'object' && auth.kind === 'device') return 'device';
  return req.session?.authenticated === true ? 'session' : null;
}
