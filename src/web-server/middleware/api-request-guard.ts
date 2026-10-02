import type { NextFunction, Request, Response } from 'express';
import { createLogger } from '../../services/logging';
import {
  forgetSession,
  isSessionEpochCurrent,
  noteSession,
} from '../services/dashboard-auth-state';
import { authenticateDeviceToken, hashDeviceToken } from '../services/dashboard-device-store';
import { getDashboardTlsSettings } from '../services/dashboard-tls-config';
import type { DeviceRequestAuth } from './request-auth';
import { isSecureTransport } from './secure-transport';

/**
 * The /api guard when dashboard auth is on (CONTRACT-auth-devices sections 2,
 * 3 and 6). In order:
 *
 * 1. A device token in the query string or body: 400 `token_in_query`.
 * 2. Public routes, as exact method and path pairs (any letter case).
 * 3. `Authorization: Bearer` is checked before the cookie and never falls back
 *    to it: a bad token is 401 `invalid_token`, `device_revoked` or
 *    `device_expired`; a good one may reach only the tray routes, everything
 *    else is 403 `device_scope`.
 * 4. A browser session from an older epoch is signed out (401
 *    `session_revoked`); a current one passes; nothing else does (401).
 *
 * Paths here are relative to the /api mount.
 */
const logger = createLogger('dashboard-auth');

type Method = 'GET' | 'POST' | 'PUT' | 'DELETE';

/** Public routes: exact method and path pairs, relative to /api. */
const PUBLIC_ROUTES: ReadonlyArray<readonly [Method, string]> = [
  ['POST', '/auth/login'],
  ['GET', '/auth/check'],
  ['GET', '/auth/setup'],
  ['POST', '/auth/setup'],
  ['POST', '/auth/devices/pair'],
  ['GET', '/health'],
];

/** The tray routes a device token may call (contract section 6), relative to /api. */
const DEVICE_SCOPE: ReadonlyArray<readonly [Method, RegExp]> = [
  ['GET', /^\/accounts\/dashboard\/?$/],
  ['POST', /^\/codex\/profiles\/[^/]+\/activate\/?$/],
  ['PUT', /^\/codex\/profiles\/auto-switch\/?$/],
  ['POST', /^\/claude\/desktop-profiles\/[^/]+\/open\/?$/],
  ['GET', /^\/claude\/desktop-profiles\/?$/],
  ['GET', /^\/antigravity\/profiles\/?$/],
  ['GET', /^\/antigravity\/auto-switch\/?$/],
  ['PUT', /^\/antigravity\/auto-switch\/?$/],
  ['POST', /^\/antigravity\/profiles\/[^/]+\/activate\/?$/],
  ['POST', /^\/antigravity\/profiles\/[^/]+\/confirm\/?$/],
  ['GET', /^\/bar\/auth\/?$/],
  ['GET', /^\/auth\/devices\/me\/?$/],
  ['POST', /^\/auth\/devices\/me\/rotate\/?$/],
  ['DELETE', /^\/auth\/devices\/me\/?$/],
];

/** GET routes also answer HEAD. */
function methodMatches(allowed: Method, method: string | undefined): boolean {
  const actual = (method ?? 'GET').toUpperCase();
  return actual === allowed || (allowed === 'GET' && actual === 'HEAD');
}

export function isPublicApiRoute(method: string | undefined, apiPath: string): boolean {
  const lower = apiPath.toLowerCase();
  return PUBLIC_ROUTES.some(
    ([allowed, route]) =>
      methodMatches(allowed, method) && (lower === route || lower === `${route}/`)
  );
}

/** Case-sensitive on purpose: the routers are, so a case variant is never in scope. */
export function isDeviceScopeRoute(method: string | undefined, apiPath: string): boolean {
  return DEVICE_SCOPE.some(
    ([allowed, pattern]) => methodMatches(allowed, method) && pattern.test(apiPath)
  );
}

const TOKEN_SHAPE = /aacd_[A-Za-z0-9_-]{16,}/;

/** A whole device token (`aacd_` and 43 base64url characters), anywhere in a string. */
const BODY_TOKEN = /aacd_[A-Za-z0-9_-]{43}/;

/**
 * A device token in any string of a parsed body, at any depth, as a value or
 * a key. The walk is iterative, so a deeply nested body cannot overflow the
 * stack; the JSON parser's size limit bounds it.
 */
function bodyCarriesToken(body: unknown): boolean {
  const pending: unknown[] = [body];
  while (pending.length > 0) {
    const value = pending.pop();
    if (typeof value === 'string') {
      if (BODY_TOKEN.test(value)) return true;
    } else if (value && typeof value === 'object') {
      for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
        if (BODY_TOKEN.test(key)) return true;
        pending.push(entry);
      }
    }
  }
  return false;
}

/** A device token anywhere but the Authorization header (the request log records the URL). */
export function requestCarriesTokenOutsideHeader(req: Request): boolean {
  const url = typeof req.originalUrl === 'string' ? req.originalUrl : (req.url ?? '');
  const query = url.includes('?') ? url.slice(url.indexOf('?')) : '';
  let decoded = query;
  try {
    decoded = decodeURIComponent(query);
  } catch {
    /* Keep the raw query. */
  }
  return TOKEN_SHAPE.test(query) || TOKEN_SHAPE.test(decoded) || bodyCarriesToken(req.body);
}

function headerValue(value: string | string[] | undefined): string | undefined {
  return Array.isArray(value) ? value[0] : value;
}

/** The bearer token when the Authorization header uses the Bearer scheme; '' when it is empty. */
export function bearerToken(req: Pick<Request, 'headers'>): string | null {
  const header = headerValue(req.headers.authorization);
  if (header === undefined) return null;
  const match = /^\s*bearer(?:\s+(.*))?$/i.exec(header);
  if (!match) return null;
  return (match[1] ?? '').trim();
}

function reject(res: Response, status: number, code: string, error: string): void {
  res.setHeader('Cache-Control', 'no-store');
  res.status(status).json({ error, code });
}

const DEVICE_ERRORS: Record<string, string> = {
  device_revoked: 'This device was signed out from the dashboard.',
  device_expired: 'This device token expired after 90 days without use.',
  invalid_token: 'The device token is not valid.',
};

function auditRejected(reason: string, deviceId: string | null): void {
  try {
    logger.info('auth.device.rejected', 'A device token was refused', {
      reason,
      ...(deviceId ? { deviceId } : {}),
    });
  } catch {
    /* Logging never changes the answer. */
  }
}

/**
 * Rotation hands out a new token, so it needs a secure transport (section 2a).
 * Over plain HTTP it is refused before the bearer is looked at, so the refused
 * request neither stamps `lastSeenAt` nor ends a rotation's grace period.
 * Returns true after answering 403 `secure_transport_required`.
 */
function requiresSecureTransportFirst(req: Request, res: Response, apiPath: string): boolean {
  const rotate =
    (req.method ?? '').toUpperCase() === 'POST' && /^\/auth\/devices\/me\/rotate\/?$/.test(apiPath);
  if (!rotate || isSecureTransport(req)) return false;
  res.setHeader('Cache-Control', 'no-store');
  res.status(403).json({
    error:
      'Use the secure dashboard address (HTTPS or a tunnel) for passwords, setup codes and device tokens.',
    code: 'secure_transport_required',
    secureOrigin: getDashboardTlsSettings().publicOrigin,
  });
  return true;
}

/** Bearer path: true after `req.auth` is set, false after answering. */
function authenticateBearer(
  req: Request,
  res: Response,
  token: string,
  apiPath: string | null
): boolean {
  // req.ip is the client behind a trusted local TLS proxy, else the peer address.
  const result = authenticateDeviceToken(token, req.ip ?? req.socket?.remoteAddress ?? null);
  if (!result.ok) {
    if (result.code === 'store_unavailable') {
      reject(res, 503, 'auth_store_unavailable', 'Paired devices cannot be checked right now.');
      return false;
    }
    auditRejected(result.code, result.deviceId);
    reject(res, 401, result.code, DEVICE_ERRORS[result.code]);
    return false;
  }
  if (apiPath === null || !isDeviceScopeRoute(req.method, apiPath)) {
    auditRejected('device_scope', result.device.id);
    reject(res, 403, 'device_scope', 'This action needs a signed-in dashboard session.');
    return false;
  }
  const auth: DeviceRequestAuth = {
    kind: 'device',
    deviceId: result.device.id,
    platform: result.device.platform,
    viaPreviousToken: result.viaPreviousToken,
    tokenSha256: hashDeviceToken(token),
  };
  (req as Request & { auth?: DeviceRequestAuth }).auth = auth;
  return true;
}

/**
 * A session from an older epoch is turned into a signed-out one that says so
 * (`revoked`), and stays that way until the next sign-in regenerates it.
 * Returns whether the request carries a current signed-in session.
 */
export function settleSessionEpoch(req: Request): boolean {
  const session = req.session;
  if (!session || session.authenticated !== true) return false;
  if (!isSessionEpochCurrent(session.epoch)) {
    forgetSession(req.sessionID);
    session.authenticated = false;
    delete session.username;
    delete session.epoch;
    session.revoked = true;
    return false;
  }
  if (req.sessionID) {
    noteSession(req.sessionID, session.epoch ?? 0, session.cookie?.expires ?? null);
  }
  return true;
}

export interface GuardRejections {
  /** The 401 body for a request without a session (path-specific). */
  unauthenticated: (res: Response) => void;
  /** Whether the request used the exact `/api` mount; only then can a device be in scope. */
  canonicalMount: boolean;
}

/** Runs with dashboard auth on, for an API path relative to /api. */
export function guardApiRequest(
  req: Request,
  res: Response,
  next: NextFunction,
  apiPath: string,
  rejections: GuardRejections
): void {
  const existing = (req as Request & { auth?: DeviceRequestAuth }).auth;
  if (existing?.kind === 'device') {
    // The global guard already checked the token; the /api guard re-checks the scope.
    if (rejections.canonicalMount && isDeviceScopeRoute(req.method, apiPath)) return next();
    reject(res, 403, 'device_scope', 'This action needs a signed-in dashboard session.');
    return;
  }
  if (requestCarriesTokenOutsideHeader(req)) {
    reject(res, 400, 'token_in_query', 'Send the device token only in the Authorization header.');
    return;
  }
  if (isPublicApiRoute(req.method, apiPath)) {
    settleSessionEpoch(req);
    return next();
  }
  const token = bearerToken(req);
  if (token !== null) {
    if (requiresSecureTransportFirst(req, res, apiPath)) return;
    if (authenticateBearer(req, res, token, rejections.canonicalMount ? apiPath : null)) next();
    return;
  }
  if (settleSessionEpoch(req)) return next();
  if (req.session?.revoked === true) {
    reject(res, 401, 'session_revoked', 'This browser was signed out. Sign in again.');
    return;
  }
  rejections.unauthenticated(res);
}
