import type { Request } from 'express';
import type { CredentialTransport } from './secure-transport';

/**
 * Who made a request (CONTRACT-auth-devices section 6): a signed-in browser
 * session, a paired device token, or nobody. A device token is checked first
 * and never falls back to a cookie session. The /api guard sets
 * `req.auth = { kind: 'device', ... }` for a valid bearer token on a tray
 * route; every other route refuses a device before its handler runs.
 */
export type RequestAuthKind = 'session' | 'device' | null;

export interface DeviceRequestAuth {
  kind: 'device';
  deviceId: string;
  platform: 'mac' | 'windows';
  /** The request used the previous token of a rotation (still inside its grace period). */
  viaPreviousToken: boolean;
  /** SHA-256 of the presented token; the token itself is never kept. */
  tokenSha256: string;
}

declare module 'express-session' {
  interface SessionData {
    /** The session epoch this sign-in belongs to (contract section 3). */
    epoch: number;
    /** Set when "sign out other browsers" ended this session; cleared by the next sign-in. */
    revoked: boolean;
    /**
     * How the sign-in that started this session reached the dashboard
     * (secure-transport.ts `CredentialTransport`). Through the LAN HTTPS proxy
     * only `encrypted` and `loopback` sessions are accepted; a missing value
     * (a session from before this field) counts as plain.
     */
    signedInOver: CredentialTransport;
  }
}

export function authKind(req: Request): RequestAuthKind {
  const auth = (req as Request & { auth?: { kind?: unknown } }).auth;
  if (auth && typeof auth === 'object' && auth.kind === 'device') return 'device';
  return req.session?.authenticated === true ? 'session' : null;
}

/** The device behind a bearer request, or null. */
export function requestDevice(req: Request): DeviceRequestAuth | null {
  const auth = (req as Request & { auth?: DeviceRequestAuth }).auth;
  return auth?.kind === 'device' ? auth : null;
}
