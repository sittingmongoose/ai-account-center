import { createHash } from 'crypto';
import type { NextFunction, Request, Response } from 'express';
import rateLimit, { MemoryStore } from 'express-rate-limit';
import { isDashboardAuthEnabled } from '../../config/config-loader-facade';
import {
  credentialsWereRejected,
  retryAfterSeconds,
  type RateLimitProperty,
} from '../middleware/auth-middleware';
import { rateLimitClientKey } from '../middleware/rate-limit-keys';
import { isForwardedThroughTrustedProxy } from '../middleware/secure-transport';
import { dashboardAuthState, sendAuthError } from './auth-route-helpers';

/**
 * The limits on guessing a dashboard secret (CONTRACT-auth-devices sections
 * 3, 4, 5 and 10), besides the per-IP `loginRateLimiter`:
 *
 * - `passwordChangeLimiter`: 5 wrong current passwords per 15 minutes per IP
 *   and account. The key never uses the session id, because "sign out other
 *   browsers" and "sign out all devices" hand the caller a new one.
 * - `passwordChangeServerLimiter`: 10 wrong current passwords per hour for the
 *   whole server, whatever the address.
 * - `signInServerLimiter`: 30 refused sign-ins (password, pairing or setup
 *   code) per hour, shared by every request whose address came from
 *   `X-Forwarded-For` through a trusted proxy hop. Anything on the proxy's
 *   computer (any process on the VM for a local proxy; the NAS and every
 *   container on it for the LAN HTTPS proxy) can choose that header, so a
 *   per-client key alone does not hold there.
 *
 * Nobody on the LAN can lock out the person at the VM: a request whose address
 * is its real peer is keyed by that peer and never meets the server-wide sign-in
 * budget, and a forwarded request is keyed `proxy:<client>`
 * (middleware/rate-limit-keys.ts), so even a forwarded `127.0.0.1` or a
 * forwarded LAN address spends only a `proxy:` budget, never the one of the
 * browser on the dashboard computer or of a LAN computer.
 * - `sessionRotationLimiter`: 10 "sign out other browsers" per 15 minutes per
 *   IP and account.
 *
 * Every 429 is `{error, code:'rate_limited', retryAfterSeconds}` with
 * `Retry-After`. The stores are in memory, like the login limiter's.
 */
const FIFTEEN_MINUTES = 15 * 60 * 1000;
const ONE_HOUR = 60 * 60 * 1000;
const SERVER_KEY = 'server';

export const PASSWORD_CHANGE_FAILURES_PER_CLIENT = 5;
export const PASSWORD_CHANGE_FAILURES_PER_SERVER = 10;
export const SIGN_IN_FAILURES_PER_SERVER = 30;
export const SESSION_ROTATIONS_PER_CLIENT = 10;

const stores = {
  passwordChange: new MemoryStore(),
  passwordChangeServer: new MemoryStore(),
  signInServer: new MemoryStore(),
  sessionRotation: new MemoryStore(),
};

/** Client key plus a hash of the signed-in account name; independent of the session id. */
function clientAccountKey(req: Request): string {
  const username = req.session?.username ?? dashboardAuthState().username;
  const account = createHash('sha256').update(username, 'utf8').digest('hex').slice(0, 32);
  return `${rateLimitClientKey(req)}|${account}`;
}

function limitedResponse(
  message: string,
  property: RateLimitProperty
): (req: Request, res: Response) => void {
  return (req, res) => {
    const seconds = retryAfterSeconds(req, property);
    res.setHeader('Retry-After', String(seconds));
    sendAuthError(res, 429, 'rate_limited', message, { retryAfterSeconds: seconds });
  };
}

const WRONG_PASSWORDS = 'Too many wrong passwords. Try again later.';
const TOO_MANY_SIGN_INS = 'Too many login attempts. Please try again later.';

/** Only a wrong current password (401) counts against the password-change limits. */
const onlyWrongPasswordCounts = (_req: Request, res: Response): boolean => res.statusCode !== 401;

export const passwordChangeLimiter = rateLimit({
  windowMs: FIFTEEN_MINUTES,
  max: PASSWORD_CHANGE_FAILURES_PER_CLIENT,
  store: stores.passwordChange,
  skipSuccessfulRequests: true,
  requestWasSuccessful: onlyWrongPasswordCounts,
  standardHeaders: true,
  legacyHeaders: false,
  keyGenerator: clientAccountKey,
  handler: limitedResponse(WRONG_PASSWORDS, 'rateLimit'),
});

export const passwordChangeServerLimiter = rateLimit({
  windowMs: ONE_HOUR,
  max: PASSWORD_CHANGE_FAILURES_PER_SERVER,
  store: stores.passwordChangeServer,
  requestPropertyName: 'serverRateLimit',
  skipSuccessfulRequests: true,
  requestWasSuccessful: onlyWrongPasswordCounts,
  // The per-client limiter already answers with the standard headers.
  standardHeaders: false,
  legacyHeaders: false,
  keyGenerator: () => SERVER_KEY,
  handler: limitedResponse(WRONG_PASSWORDS, 'serverRateLimit'),
});

/**
 * Runs after `loginRateLimiter`, so a client that is already over its own
 * budget never spends the server's. Counts only refused credentials.
 */
export const signInServerLimiter = rateLimit({
  windowMs: ONE_HOUR,
  max: SIGN_IN_FAILURES_PER_SERVER,
  store: stores.signInServer,
  requestPropertyName: 'serverRateLimit',
  skipSuccessfulRequests: true,
  requestWasSuccessful: (_req, res) => !credentialsWereRejected(res),
  standardHeaders: false,
  legacyHeaders: false,
  skip: (req) => !isDashboardAuthEnabled() || !isForwardedThroughTrustedProxy(req),
  keyGenerator: () => SERVER_KEY,
  handler: limitedResponse(TOO_MANY_SIGN_INS, 'serverRateLimit'),
});

export const sessionRotationLimiter = rateLimit({
  windowMs: FIFTEEN_MINUTES,
  max: SESSION_ROTATIONS_PER_CLIENT,
  store: stores.sessionRotation,
  standardHeaders: true,
  legacyHeaders: false,
  keyGenerator: clientAccountKey,
  handler: limitedResponse('Too many sign-outs in a short time. Try again later.', 'rateLimit'),
});

/** `loginRateLimiter` then `signInServerLimiter`, for a handler that calls one middleware. */
export function signInLimiters(
  perClient: (req: Request, res: Response, next: NextFunction) => void
): (req: Request, res: Response, next: NextFunction) => void {
  return (req, res, next) => perClient(req, res, () => signInServerLimiter(req, res, next));
}

/** Tests only: forget every count. */
export async function resetAuthRateLimitsForTests(): Promise<void> {
  await Promise.all(Object.values(stores).map((store) => store.resetAll()));
}
