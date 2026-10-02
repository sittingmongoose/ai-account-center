/**
 * Dashboard Authentication Middleware
 * Session-based auth with httpOnly cookies for CCS dashboard.
 */

import type { IncomingMessage } from 'http';
import type { NextFunction, Request, Response } from 'express';
import session from 'express-session';
import rateLimit from 'express-rate-limit';

import crypto from 'crypto';
import * as net from 'net';
import fs from 'fs';
import path from 'path';
import {
  getCcsDir,
  getDashboardAuthConfig,
  isDashboardAuthEnabled,
} from '../../config/config-loader-facade';
import { bearerToken, guardApiRequest } from './api-request-guard';
import { authKind } from './request-auth';
import { isSessionEpochCurrent } from '../services/dashboard-auth-state';

// Extend Express Request with session
declare module 'express-session' {
  interface SessionData {
    authenticated: boolean;
    username: string;
  }
}

/**
 * Whether a request path is API traffic, in any letter case. Express matches
 * routes case-insensitively unless case-sensitive routing is enabled, so a
 * case-sensitive test here could let /API/... reach an API router unguarded.
 */
export function isApiRequestPath(requestPath: string): boolean {
  const pathLower = requestPath.toLowerCase();
  return pathLower === '/api' || pathLower.startsWith('/api/');
}

/**
 * `/api/accounts` and `/api/auth` and below, in any letter case
 * (CONTRACT-registry-lifecycle section 1, CONTRACT-auth-devices section 2).
 */
function isCodedApiPath(requestPath: string): boolean {
  const pathLower = requestPath.toLowerCase();
  return ['/api/accounts', '/api/auth'].some(
    (prefix) => pathLower === prefix || pathLower.startsWith(`${prefix}/`)
  );
}

/**
 * A 401 is never cached. Under /api/accounts and /api/auth it also carries the
 * stable code `auth_required`; elsewhere the body stays as it was.
 * `fullPath` is the request path including the /api mount.
 */
function rejectWithoutSession(res: Response, fullPath: string): void {
  res.setHeader('Cache-Control', 'no-store');
  res
    .status(401)
    .json(
      isCodedApiPath(fullPath)
        ? { error: 'Authentication required', code: 'auth_required' }
        : { error: 'Authentication required' }
    );
}

/** Path to persistent session secret file */
function getSessionSecretPath() {
  return path.join(getCcsDir(), '.session-secret');
}

/**
 * Generate or retrieve persistent session secret.
 * Priority: ENV var > persisted file > generate new
 */
function getSessionSecret(): string {
  // 1. Check ENV var first
  if (process.env.CCS_SESSION_SECRET) {
    return process.env.CCS_SESSION_SECRET;
  }

  const secretPath = getSessionSecretPath();

  // 2. Try to read persisted secret
  try {
    if (fs.existsSync(secretPath)) {
      const secret = fs.readFileSync(secretPath, 'utf-8').trim();
      if (secret.length >= 32) {
        return secret;
      }
    }
  } catch {
    // Ignore read errors, generate new secret
  }

  // 3. Generate and persist new random secret
  const newSecret = crypto.randomBytes(32).toString('hex');
  try {
    const dir = path.dirname(secretPath);
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }
    fs.writeFileSync(secretPath, newSecret, { mode: 0o600 });
  } catch (err) {
    // Log warning - sessions won't persist across restarts
    console.warn('[!] Failed to persist session secret:', (err as Error).message);
  }

  return newSecret;
}

/** Seconds until a limiter window resets, at least 1 (the 429 body and `Retry-After`). */
export function retryAfterSeconds(req: Request): number {
  const reset = (req as Request & { rateLimit?: { resetTime?: Date } }).rateLimit?.resetTime;
  const seconds = reset ? Math.ceil((reset.getTime() - Date.now()) / 1000) : 15 * 60;
  return Math.max(1, seconds);
}

/** Tries left in the current limiter window (the failed request already counted). */
export function triesLeft(req: Request): number {
  const remaining = (req as Request & { rateLimit?: { remaining?: number } }).rateLimit?.remaining;
  return typeof remaining === 'number' ? Math.max(0, remaining) : 0;
}

/**
 * Rate limiter for login attempts: 5 failed attempts per 15 minutes per IP;
 * successful requests are not counted (CONTRACT-auth-devices section 10).
 * Pairing and a LAN setup code share this key and budget.
 */
export const loginRateLimiter = rateLimit({
  windowMs: 15 * 60 * 1000, // 15 minutes
  max: 5, // 5 attempts
  skipSuccessfulRequests: true,
  standardHeaders: true,
  legacyHeaders: false,
  skip: () => !isDashboardAuthEnabled(),
  handler: (req, res) => {
    const seconds = retryAfterSeconds(req);
    res.setHeader('Retry-After', String(seconds));
    res.setHeader('Cache-Control', 'no-store');
    res.status(429).json({
      error: 'Too many login attempts. Please try again later.',
      code: 'rate_limited',
      retryAfterSeconds: seconds,
    });
  },
});

/**
 * Create session middleware configured for CCS dashboard.
 */
export function createSessionMiddleware(): (
  req: Request,
  res: Response,
  next: NextFunction
) => void {
  const authConfig = getDashboardAuthConfig();
  const maxAge = (authConfig.session_timeout_hours ?? 24) * 60 * 60 * 1000;

  return session({
    secret: getSessionSecret(),
    resave: false,
    saveUninitialized: false,
    cookie: {
      // Secure whenever the request arrived over TLS (in-process, or a trusted
      // loopback proxy once `trust proxy` is set); plain HTTP keeps working.
      secure: 'auto',
      httpOnly: true,
      maxAge,
      sameSite: 'strict',
    },
  });
}

/**
 * Auth middleware that protects all routes except public paths.
 * Only active when dashboard_auth.enabled = true.
 */
export function authMiddleware(req: Request, res: Response, next: NextFunction): void {
  // Skip auth if disabled
  if (!isDashboardAuthEnabled()) {
    return next();
  }

  // Allow static assets and SPA routes (non-API, in any letter case)
  if (!isApiRequestPath(req.path)) {
    return next();
  }

  // Public routes (exact method and path), bearer tokens, then the session.
  const apiPath = req.path.slice('/api'.length) || '/';
  guardApiRequest(req, res, next, apiPath, {
    unauthenticated: (response) => rejectWithoutSession(response, req.path),
    canonicalMount: req.path === '/api' || req.path.startsWith('/api/'),
  });
}

/**
 * The same session guard, installed on the /api router itself so that every
 * request reaching an API route passes it whatever the casing of its mount
 * path. Inside the router, req.path is relative to /api.
 */
export function apiAuthMiddleware(req: Request, res: Response, next: NextFunction): void {
  if (!isDashboardAuthEnabled()) {
    return next();
  }

  guardApiRequest(req, res, next, req.path, {
    unauthenticated: (response) => rejectWithoutSession(response, `/api${req.path}`),
    // The /api router is mounted case-sensitively, so it only sees the exact mount.
    canonicalMount: true,
  });
}

/**
 * Route-level access check that does not rely on the global guards.
 * With dashboard auth enabled it requires an authenticated session (401);
 * with auth disabled it keeps the existing localhost-only rule (403).
 */
export function requireDashboardSession(
  req: Request,
  res: Response,
  localAccessError?: string
): boolean {
  if (!isDashboardAuthEnabled()) {
    return requireLocalAccessWhenAuthDisabled(req, res, localAccessError);
  }

  // A device reaches a route only when the /api guard put it in scope.
  if (authKind(req) !== null) {
    return true;
  }

  res.status(401).json({ error: 'Authentication required' });
  return false;
}

export function isLoopbackRemoteAddress(value: string | undefined): boolean {
  if (!value) return false;
  const normalized = value.trim().replace(/^\[|\]$/g, '');
  return (
    normalized === '::1' ||
    normalized === '127.0.0.1' ||
    normalized.startsWith('127.') ||
    normalized === '::ffff:127.0.0.1' ||
    normalized.startsWith('::ffff:127.')
  );
}

function isLoopbackHostname(value: string | undefined): boolean {
  if (!value) return false;
  const normalized = value
    .trim()
    .toLowerCase()
    .replace(/^\[|\]$/g, '');
  return (
    normalized === 'localhost' ||
    normalized.endsWith('.localhost') ||
    (net.isIP(normalized) !== 0 && isLoopbackRemoteAddress(normalized))
  );
}

function getSingleHeader(value: string | string[] | undefined): string | undefined {
  return Array.isArray(value) ? value[0] : value;
}

function parseHostHeader(value: string | undefined): URL | null {
  if (!value) return null;

  try {
    return new URL(`http://${value}`);
  } catch {
    return null;
  }
}

function isHttpOrigin(origin: URL): boolean {
  return origin.protocol === 'http:' || origin.protocol === 'https:';
}

export function isDashboardWebSocketOriginAllowed(req: IncomingMessage): boolean {
  const originHeader = getSingleHeader(req.headers.origin);
  if (!originHeader) return true;

  let origin: URL;
  try {
    origin = new URL(originHeader);
  } catch {
    return false;
  }

  if (!isHttpOrigin(origin)) {
    return false;
  }

  const host = parseHostHeader(getSingleHeader(req.headers.host));
  if (!host) {
    return false;
  }

  if (origin.host.toLowerCase() === host.host.toLowerCase()) {
    return true;
  }

  return (
    isLoopbackHostname(origin.hostname) &&
    isLoopbackHostname(host.hostname) &&
    origin.port === host.port
  );
}

export function isDashboardWebSocketUpgradeAllowed(req: IncomingMessage): boolean {
  if (!isDashboardWebSocketOriginAllowed(req)) {
    return false;
  }

  if (!isDashboardAuthEnabled()) {
    return isLoopbackRemoteAddress(req.socket.remoteAddress);
  }

  // Device tokens never open /ws (CONTRACT-auth-devices section 6), and a
  // browser signed out by "sign out other browsers" does not either.
  if (bearerToken(req as Request) !== null) return false;
  const session = (req as Request).session;
  return Boolean(session?.authenticated) && isSessionEpochCurrent(session?.epoch);
}

export function getDashboardWebSocketRejectionStatus(req?: IncomingMessage): 401 | 403 {
  if (req && !isDashboardWebSocketOriginAllowed(req)) {
    return 403;
  }

  if (req && isDashboardAuthEnabled() && bearerToken(req as Request) !== null) {
    return 403;
  }

  if (!isDashboardAuthEnabled()) return 403;

  return 401;
}

export function requireLocalAccessWhenAuthDisabled(
  req: Request,
  res: Response,
  error = 'This endpoint requires localhost access when dashboard auth is disabled.'
): boolean {
  if (isDashboardAuthEnabled()) {
    return true;
  }

  if (!isLoopbackRemoteAddress(req.socket.remoteAddress)) {
    res.status(403).json({ error });
    return false;
  }

  const host = parseHostHeader(getSingleHeader(req.headers.host));
  if (!host || !isLoopbackHostname(host.hostname)) {
    res.status(403).json({ error });
    return false;
  }

  const originHeader = getSingleHeader(req.headers.origin);
  if (originHeader) {
    let origin: URL;
    try {
      origin = new URL(originHeader);
    } catch {
      res.status(403).json({ error });
      return false;
    }

    const isSameHost = origin.host.toLowerCase() === host.host.toLowerCase();
    const isLoopbackAlias =
      isHttpOrigin(origin) && isLoopbackHostname(origin.hostname) && origin.port === host.port;

    if (!isSameHost && !isLoopbackAlias) {
      res.status(403).json({ error });
      return false;
    }
  }

  return true;
}
