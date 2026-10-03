/**
 * Dashboard Authentication Routes
 * Handles login, logout, session check and setup status here; the password
 * change, sessions, first-run setup and paired devices are registered from
 * auth-session-routes.ts and auth-device-routes.ts (CONTRACT-auth-devices).
 */

import type { Request, Response } from 'express';

import type { DashboardAuthConfig } from '../../config/unified-config-types';
import {
  isLoopbackRemoteAddress,
  loginRateLimiter,
  markCredentialsAccepted,
  markCredentialsRejected,
  triesLeft,
} from '../middleware/auth-middleware';
import { getDashboardAuthConfig } from '../../config/config-loader-facade';
import {
  describeConnection,
  isDirectLoopbackRequest,
  isSecureTransport,
  localNetworkTrust,
} from '../middleware/secure-transport';
import { forgetSession } from '../services/dashboard-auth-state';
import { createApiRouter } from './api-router';
import { signInServerLimiter } from './auth-rate-limits';
import {
  audit,
  BCRYPT_HASH_PATTERN,
  credentialSource,
  dashboardAuthState,
  isLoginField,
  MAX_PASSWORD_LENGTH,
  MAX_USERNAME_LENGTH,
  noStore,
  passwordMatches,
  secureOrigin,
  startSessionForPassword,
  timingSafeStringEqual,
} from './auth-route-helpers';
import { registerAuthSessionRoutes } from './auth-session-routes';
import { registerAuthDeviceRoutes } from './auth-device-routes';
import { registerAuthNetworkRoutes } from './auth-network-routes';
import { registerAuthLifetimeRoutes } from './auth-lifetime-routes';

const router = createApiRouter();
router.use(noStore);

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

/** Section 4: a first run from anywhere but loopback needs the one-time setup code. */
function setupCodeRequired(req: Request, configured: boolean): boolean {
  return !configured && !isDirectLoopbackRequest(req);
}

function invalidCredentials(req: Request, res: Response): void {
  markCredentialsRejected(res);
  // The submitted username is never logged: people type passwords into it.
  audit('auth.login.failed', 'Dashboard sign-in failed', {
    remoteAddress: req.socket.remoteAddress ?? null,
    reason: 'invalid_credentials',
  });
  res.status(401).json({
    error: 'Invalid credentials',
    code: 'invalid_credentials',
    triesLeft: triesLeft(req),
  });
}

/**
 * POST /api/auth/login
 * Authenticate user with username/password.
 * Rate limited: 5 failed attempts per 15 minutes per IP, plus the server-wide
 * budget for addresses taken from X-Forwarded-For (auth-rate-limits.ts).
 */
async function login(req: Request, res: Response): Promise<void> {
  const body: unknown = req.body;
  const fields: Record<string, unknown> =
    body && typeof body === 'object' ? (body as Record<string, unknown>) : {};
  const { username, password, rememberMe } = fields;

  if (
    !isLoginField(username, MAX_USERNAME_LENGTH) ||
    !isLoginField(password, MAX_PASSWORD_LENGTH)
  ) {
    res.status(400).json({ error: 'Username and password required' });
    return;
  }
  // "Remember me" (default on): off gives a browser-session cookie that dies
  // with the browser; on uses the configured session lifetime.
  if (rememberMe !== undefined && typeof rememberMe !== 'boolean') {
    res.status(400).json({ error: 'Username and password required' });
    return;
  }
  const remember = rememberMe !== false;

  const authConfig = getDashboardAuthConfig();

  // Check if auth is configured
  if (!authConfig.enabled || !authConfig.username || !authConfig.password_hash) {
    res.status(400).json({ error: 'Authentication not configured' });
    return;
  }

  // Validate bcrypt hash format to prevent bcrypt.compare errors
  if (!BCRYPT_HASH_PATTERN.test(authConfig.password_hash)) {
    res.status(500).json({ error: 'Invalid password hash format in config' });
    return;
  }

  // Verify credentials (timing-safe comparison for username). A comparison
  // that fails is a failed login, never an error that leaves the request.
  let usernameMatch = false;
  try {
    usernameMatch = timingSafeStringEqual(username, authConfig.username);
  } catch {
    usernameMatch = false;
  }
  const checkedHash = authConfig.password_hash;
  const passwordMatch = await passwordMatches(password, checkedHash);

  if (!usernameMatch || !passwordMatch) {
    invalidCredentials(req, res);
    return;
  }

  // Regenerate session to prevent session fixation, then sign in within the
  // current epoch, unless the password was changed while it was being checked.
  let session: Awaited<ReturnType<typeof startSessionForPassword>>;
  try {
    session = await startSessionForPassword(req, username, checkedHash, { remember });
  } catch {
    markCredentialsAccepted(res);
    res.status(500).json({ error: 'Session error' });
    return;
  }
  if (session === null) {
    invalidCredentials(req, res);
    return;
  }
  res.json({ success: true, username });
}

router.post('/login', loginRateLimiter, signInServerLimiter, login);

/**
 * POST /api/auth/logout
 * Clear session and log out user.
 */
router.post('/logout', (req: Request, res: Response) => {
  forgetSession(req.sessionID);
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
    // CONTRACT-auth-devices sections 2a, 4 and 10 (additive fields).
    signedOutReason: req.session?.revoked === true ? 'revoked' : null,
    setupCodeRequired: setupCodeRequired(req, accessState.authConfigured),
    secureTransport: isSecureTransport(req),
    secureOrigin: secureOrigin(),
    // Section 2a rule 4 (amended 2026-10-02): the owner's switch, and this connection.
    trustedLocalNetwork: localNetworkTrust().enabled,
    connection: describeConnection(req),
  });
});

/**
 * GET /api/auth/setup
 * Check if authentication is properly configured.
 */
router.get('/setup', (req: Request, res: Response) => {
  const authConfig = getDashboardAuthConfig();
  const configured = !!(authConfig.username && authConfig.password_hash);

  res.json({
    enabled: authConfig.enabled,
    configured,
    sessionTimeoutHours: dashboardAuthState().sessionTimeoutHours,
    sessionLifetimeDays: dashboardAuthState().sessionLifetimeDays,
    // CONTRACT-auth-devices sections 2a and 4 (additive); the code itself is never returned.
    setupCodeRequired: setupCodeRequired(req, configured),
    managedBy: credentialSource(),
    secureTransport: isSecureTransport(req),
    secureOrigin: secureOrigin(),
    trustedLocalNetwork: localNetworkTrust().enabled,
    connection: describeConnection(req),
  });
});

registerAuthSessionRoutes(router);
registerAuthDeviceRoutes(router);
registerAuthNetworkRoutes(router);
registerAuthLifetimeRoutes(router);

export default router;
