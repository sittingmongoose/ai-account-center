import bcrypt from 'bcrypt';
import crypto from 'crypto';
import type { NextFunction, Request, Response } from 'express';
import rateLimit, { ipKeyGenerator } from 'express-rate-limit';
import {
  getDashboardAuthConfig,
  loadOrCreateUnifiedConfig,
} from '../../config/config-loader-facade';
import { createLogger } from '../../services/logging';
import {
  isDashboardWebSocketOriginAllowed,
  retryAfterSeconds,
} from '../middleware/auth-middleware';
import { isSecureTransport } from '../middleware/secure-transport';
import { closeStaleSessionClients } from '../dashboard-events';
import {
  ensureSessionEpoch,
  forgetSession,
  isSessionEpochCurrent,
  noteSession,
  sessionKey,
} from '../services/dashboard-auth-state';
import { getDashboardTlsSettings } from '../services/dashboard-tls-config';

/**
 * Shared rules for the dashboard sign-in routes (CONTRACT-auth-devices
 * section 2): `Cache-Control: no-store`, JSON bodies of at most 4 KB with a
 * strict schema, fixed public error sentences with stable codes, the Origin
 * rule for browser mutations, secure transport where a password, code or
 * token crosses the wire, and one way to start a signed-in session.
 */
export const MAX_AUTH_BODY_BYTES = 4 * 1024;
export const MAX_USERNAME_LENGTH = 256;
export const MAX_PASSWORD_LENGTH = 1024;
export const BCRYPT_HASH_PATTERN = /^\$2[aby]?\$\d{2}\$.{53}$/;

const logger = createLogger('dashboard-auth');

export function sendAuthError(
  res: Response,
  status: number,
  code: string,
  error: string,
  extra: Record<string, unknown> = {}
): void {
  res.setHeader('Cache-Control', 'no-store');
  res.status(status).json({ error, code, ...extra });
}

/** Audit lines carry names and counts only, never a value that was typed. */
export function audit(event: string, message: string, context: Record<string, unknown> = {}): void {
  try {
    logger.info(event, message, context);
  } catch {
    /* Logging never changes the answer. */
  }
}

/** Timing-safe comparison of UTF-8 strings (different lengths compare false). */
export function timingSafeStringEqual(a: string, b: string): boolean {
  const left = Buffer.from(a, 'utf8');
  const right = Buffer.from(b, 'utf8');
  if (left.length !== right.length) {
    crypto.timingSafeEqual(left, left);
    return false;
  }
  return crypto.timingSafeEqual(left, right);
}

/** bcrypt check that never throws: a comparison that fails is a mismatch. */
export async function passwordMatches(password: string, hash: string): Promise<boolean> {
  try {
    return await bcrypt.compare(password, hash);
  } catch {
    return false;
  }
}

export type CredentialSource = 'config' | 'env';

/** `env` when the username or hash comes from the environment (rule 6: read-only). */
export function credentialSource(): CredentialSource {
  return process.env.CCS_DASHBOARD_USERNAME !== undefined ||
    process.env.CCS_DASHBOARD_PASSWORD_HASH !== undefined
    ? 'env'
    : 'config';
}

export interface DashboardAuthState {
  enabled: boolean;
  configured: boolean;
  managedBy: CredentialSource;
  username: string;
  passwordHash: string;
  sessionTimeoutHours: number;
}

export function dashboardAuthState(): DashboardAuthState {
  const config = getDashboardAuthConfig();
  return {
    enabled: config.enabled,
    configured: Boolean(config.username && config.password_hash),
    managedBy: credentialSource(),
    username: config.username,
    passwordHash: config.password_hash,
    sessionTimeoutHours: config.session_timeout_hours ?? 24,
  };
}

/** `password_changed_at` from config.yaml; null when unknown or set by the environment. */
export function passwordChangedAt(): string | null {
  if (credentialSource() === 'env') return null;
  try {
    const value = loadOrCreateUnifiedConfig().dashboard_auth?.password_changed_at;
    return typeof value === 'string' && !Number.isNaN(Date.parse(value)) ? value : null;
  } catch {
    return null;
  }
}

export function secureOrigin(): string | null {
  return getDashboardTlsSettings().publicOrigin;
}

/** Rule 9: false after answering 403 `secure_transport_required`. */
export function requireSecureTransport(req: Request, res: Response): boolean {
  if (isSecureTransport(req)) return true;
  sendAuthError(
    res,
    403,
    'secure_transport_required',
    'Use the secure dashboard address (HTTPS or a tunnel) for passwords, setup codes and device tokens.',
    { secureOrigin: secureOrigin() }
  );
  return false;
}

/** Rule 5: false after answering 409 when dashboard sign-in is off or has no password yet. */
export function requireAuthConfigured(res: Response, state = dashboardAuthState()): boolean {
  if (state.enabled && state.configured) return true;
  sendAuthError(res, 409, 'auth_not_configured', 'Dashboard sign-in is not set up on this server.');
  return false;
}

export type OriginRule = 'required' | 'absent-or-same';

function bodyTooLarge(req: Request): boolean {
  const declared = Number(req.headers['content-length']);
  if (Number.isFinite(declared) && declared > MAX_AUTH_BODY_BYTES) return true;
  try {
    return Buffer.byteLength(JSON.stringify(req.body ?? null), 'utf8') > MAX_AUTH_BODY_BYTES;
  } catch {
    return true;
  }
}

export function invalidBody(res: Response): void {
  sendAuthError(res, 400, 'invalid_body', 'The request body is not valid for this action.');
}

/**
 * Origin, query, JSON, size and an exact key set. Returns the body, or null
 * after answering 403 `origin_required`, 400 `unexpected_query`, 415
 * `json_required` or 400 `invalid_body`.
 */
export function readAuthBody(
  req: Request,
  res: Response,
  origin: OriginRule,
  required: readonly string[],
  optional: readonly string[] = []
): Record<string, unknown> | null {
  const header = req.headers.origin;
  const originMissing = typeof header !== 'string' || header.length === 0;
  if (
    (origin === 'required' && originMissing) ||
    (!originMissing && !isDashboardWebSocketOriginAllowed(req))
  ) {
    sendAuthError(res, 403, 'origin_required', 'This change requires the dashboard origin.');
    return null;
  }
  if (req.originalUrl.includes('?')) {
    sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return null;
  }
  if (!req.is('application/json')) {
    // The JSON parser left a body of another type unread: drain it so the connection ends cleanly.
    req.resume();
    sendAuthError(res, 415, 'json_required', 'This change requires application/json.');
    return null;
  }
  const body: unknown = req.body;
  if (bodyTooLarge(req) || !body || typeof body !== 'object' || Array.isArray(body)) {
    invalidBody(res);
    return null;
  }
  const record = body as Record<string, unknown>;
  const keys = Object.keys(record);
  if (
    !required.every((key) => Object.prototype.hasOwnProperty.call(record, key)) ||
    !keys.every((key) => required.includes(key) || optional.includes(key))
  ) {
    invalidBody(res);
    return null;
  }
  return record;
}

/** A DELETE may come without a body; when one is sent it must be the JSON `{}`. */
export function readOptionalEmptyBody(req: Request, res: Response, origin: OriginRule): boolean {
  const length = Number(req.headers['content-length'] ?? 0);
  const hasBody = (Number.isFinite(length) && length > 0) || req.headers['transfer-encoding'];
  if (!hasBody) {
    const header = req.headers.origin;
    const missing = typeof header !== 'string' || header.length === 0;
    if (
      (origin === 'required' && missing) ||
      (!missing && !isDashboardWebSocketOriginAllowed(req))
    ) {
      sendAuthError(res, 403, 'origin_required', 'This change requires the dashboard origin.');
      return false;
    }
    if (req.originalUrl.includes('?')) {
      sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
      return false;
    }
    return true;
  }
  return readAuthBody(req, res, origin, []) !== null;
}

export function isLoginField(value: unknown, maxLength: number): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= maxLength;
}

/** Contract 3.5: 8 or more characters, at most 72 UTF-8 bytes (bcrypt ignores the rest). */
export function newPasswordProblem(value: string): 'too_short' | 'too_long' | null {
  if ([...value].length < 8) return 'too_short';
  if (Buffer.byteLength(value, 'utf8') > 72) return 'too_long';
  return null;
}

export function weakPassword(res: Response, reason: 'too_short' | 'too_long'): void {
  sendAuthError(
    res,
    400,
    'weak_password',
    reason === 'too_short'
      ? 'The new password needs at least 8 characters.'
      : 'The new password is longer than 72 bytes.',
    { reason }
  );
}

function sessionCall(run: (done: (error?: unknown) => void) => void): Promise<void> {
  return new Promise((resolve, reject) => run((error) => (error ? reject(error) : resolve())));
}

/**
 * Regenerate the session (a new id, so no fixation), sign it in for `username`
 * in the current epoch, and save it before answering so the response carries
 * the new cookie. Returns the session's expiry.
 */
export async function startSignedInSession(
  req: Request,
  username: string
): Promise<{ expiresAt: string | null }> {
  const epoch = await ensureSessionEpoch();
  const previous = req.sessionID;
  await sessionCall((done) => req.session.regenerate(done));
  forgetSession(previous);
  req.session.authenticated = true;
  req.session.username = username;
  req.session.epoch = epoch;
  await sessionCall((done) => req.session.save(done));
  const expires = req.session.cookie?.expires ?? null;
  if (req.sessionID) noteSession(req.sessionID, epoch, expires);
  // Open /ws connections of browsers this epoch no longer includes close now.
  closeStaleSessionClients((value) => isSessionEpochCurrent(value));
  return { expiresAt: expires ? expires.toISOString() : null };
}

/** The current session's expiry, as ISO time. */
export function sessionExpiresAt(req: Request): string | null {
  const expires = req.session?.cookie?.expires;
  return expires instanceof Date ? expires.toISOString() : null;
}

/**
 * Contract 3.4: 5 failed current-password checks per 15 minutes, keyed on the
 * session and the IP; only a wrong current password counts.
 */
export const passwordChangeLimiter = rateLimit({
  windowMs: 15 * 60 * 1000,
  max: 5,
  skipSuccessfulRequests: true,
  requestWasSuccessful: (_req, res) => res.statusCode !== 401,
  standardHeaders: true,
  legacyHeaders: false,
  keyGenerator: (req) =>
    `${ipKeyGenerator(req.ip ?? req.socket?.remoteAddress ?? 'unknown')}|${sessionKey(req.sessionID ?? '')}`,
  handler: (req: Request, res: Response) => {
    const seconds = retryAfterSeconds(req);
    res.setHeader('Retry-After', String(seconds));
    sendAuthError(res, 429, 'rate_limited', 'Too many wrong passwords. Try again later.', {
      retryAfterSeconds: seconds,
    });
  },
});

/** Every response under /api/auth is `no-store` (rule 1). */
export function noStore(_req: Request, res: Response, next: NextFunction): void {
  res.setHeader('Cache-Control', 'no-store');
  next();
}
