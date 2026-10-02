/**
 * Dashboard Authentication Routes
 * Handles login, logout, session check, and setup status.
 */

import type { Request, Response } from 'express';
import bcrypt from 'bcrypt';
import crypto from 'crypto';

import type { DashboardAuthConfig } from '../../config/unified-config-types';
import { isLoopbackRemoteAddress, loginRateLimiter } from '../middleware/auth-middleware';
import { getDashboardAuthConfig } from '../../config/config-loader-facade';
import { createApiRouter } from './api-router';

/** Login field bounds; bcrypt reads only the first 72 bytes of a password. */
const MAX_USERNAME_LENGTH = 256;
const MAX_PASSWORD_LENGTH = 1024;

/**
 * Timing-safe string comparison to prevent timing attacks.
 * Returns true if strings match, false otherwise.
 */
function timingSafeEqual(a: string, b: string): boolean {
  // Compare UTF-8 bytes: strings of equal length can differ in byte length,
  // and crypto.timingSafeEqual throws on buffers of different lengths.
  const left = Buffer.from(a, 'utf8');
  const right = Buffer.from(b, 'utf8');
  if (left.length !== right.length) {
    // Still compare to avoid length-based timing leak
    crypto.timingSafeEqual(left, left);
    return false;
  }
  return crypto.timingSafeEqual(left, right);
}

function isLoginField(value: unknown, maxLength: number): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= maxLength;
}

const router = createApiRouter();

export type DashboardAccessMode = 'open' | 'login' | 'setup';

export interface DashboardAccessState {
  authRequired: boolean;
  authEnabled: boolean;
  authConfigured: boolean;
  isLocalAccess: boolean;
  accessMode: DashboardAccessMode;
}

export function resolveDashboardAccessState(
  authConfig: DashboardAuthConfig,
  remoteAddress: string | undefined
): DashboardAccessState {
  const isLocalAccess = isLoopbackRemoteAddress(remoteAddress);
  const authConfigured = Boolean(authConfig.username && authConfig.password_hash);

  if (!authConfig.enabled) {
    return {
      authRequired: false,
      authEnabled: false,
      authConfigured,
      isLocalAccess,
      accessMode: 'open',
    };
  }

  if (authConfigured) {
    return {
      authRequired: true,
      authEnabled: true,
      authConfigured: true,
      isLocalAccess,
      accessMode: 'login',
    };
  }

  return {
    authRequired: true,
    authEnabled: true,
    authConfigured,
    isLocalAccess,
    accessMode: 'setup',
  };
}

/**
 * POST /api/auth/login
 * Authenticate user with username/password.
 * Rate limited: 5 attempts per 15 minutes.
 */
router.post('/login', loginRateLimiter, async (req: Request, res: Response) => {
  const body: unknown = req.body;
  const fields: Record<string, unknown> =
    body && typeof body === 'object' ? (body as Record<string, unknown>) : {};
  const { username, password } = fields;

  if (
    !isLoginField(username, MAX_USERNAME_LENGTH) ||
    !isLoginField(password, MAX_PASSWORD_LENGTH)
  ) {
    res.status(400).json({ error: 'Username and password required' });
    return;
  }

  const authConfig = getDashboardAuthConfig();

  // Check if auth is configured
  if (!authConfig.enabled || !authConfig.username || !authConfig.password_hash) {
    res.status(400).json({ error: 'Authentication not configured' });
    return;
  }

  // Validate bcrypt hash format to prevent bcrypt.compare errors
  const isBcryptHash = /^\$2[aby]?\$\d{2}\$.{53}$/.test(authConfig.password_hash);
  if (!isBcryptHash) {
    res.status(500).json({ error: 'Invalid password hash format in config' });
    return;
  }

  // Verify credentials (timing-safe comparison for username). A comparison
  // that fails is a failed login, never an error that leaves the request.
  let usernameMatch = false;
  let passwordMatch = false;
  try {
    usernameMatch = timingSafeEqual(username, authConfig.username);
    passwordMatch = await bcrypt.compare(password, authConfig.password_hash);
  } catch {
    usernameMatch = false;
    passwordMatch = false;
  }

  if (!usernameMatch || !passwordMatch) {
    res.status(401).json({ error: 'Invalid credentials' });
    return;
  }

  // Regenerate session to prevent session fixation, then set auth
  req.session.regenerate((err) => {
    if (err) {
      res.status(500).json({ error: 'Session error' });
      return;
    }
    req.session.authenticated = true;
    req.session.username = username;
    res.json({ success: true, username });
  });
});

/**
 * POST /api/auth/logout
 * Clear session and log out user.
 */
router.post('/logout', (req: Request, res: Response) => {
  req.session.destroy((err) => {
    if (err) {
      res.status(500).json({ error: 'Failed to logout' });
      return;
    }
    res.clearCookie('connect.sid');
    res.json({ success: true });
  });
});

/**
 * GET /api/auth/check
 * Check if user is authenticated and if auth is required.
 */
router.get('/check', (req: Request, res: Response) => {
  const authConfig = getDashboardAuthConfig();
  const accessState = resolveDashboardAccessState(authConfig, req.socket.remoteAddress);

  res.json({
    ...accessState,
    authenticated: req.session?.authenticated ?? false,
    username: req.session?.username ?? null,
  });
});

/**
 * GET /api/auth/setup
 * Check if authentication is properly configured.
 */
router.get('/setup', (_req: Request, res: Response) => {
  const authConfig = getDashboardAuthConfig();

  res.json({
    enabled: authConfig.enabled,
    configured: !!(authConfig.username && authConfig.password_hash),
    sessionTimeoutHours: authConfig.session_timeout_hours ?? 24,
  });
});

export default router;
