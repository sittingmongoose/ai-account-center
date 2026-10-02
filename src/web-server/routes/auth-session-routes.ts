import bcrypt from 'bcrypt';
import type { NextFunction, Request, Response, Router } from 'express';
import { mutateConfig } from '../../config/config-loader-facade';
import {
  loginRateLimiter,
  markCredentialsAccepted,
  markCredentialsRejected,
  triesLeft,
} from '../middleware/auth-middleware';
import { authKind } from '../middleware/request-auth';
import { isDirectLoopbackRequest, isSecureTransport } from '../middleware/secure-transport';
import {
  authNow,
  isoTime,
  passwordHashCost,
  withAuthWriteGate,
} from '../services/dashboard-auth-files';
import { bumpSessionEpoch, countOtherSessions } from '../services/dashboard-auth-state';
import { activeDevices } from '../services/dashboard-device-store';
import {
  consumeSetupCode,
  hasActiveSetupCode,
  setupCodeMatches,
} from '../services/dashboard-setup-code';
import {
  passwordChangeLimiter,
  passwordChangeServerLimiter,
  sessionRotationLimiter,
  signInLimiters,
} from './auth-rate-limits';
import {
  audit,
  BCRYPT_HASH_PATTERN,
  dashboardAuthState,
  invalidBody,
  MAX_PASSWORD_LENGTH,
  newPasswordProblem,
  passwordChangedAt,
  passwordMatches,
  readAuthBody,
  requireAuthConfigured,
  requireSecureTransport,
  secureOrigin,
  sendAuthError,
  sessionExpiresAt,
  startSignedInSession,
  weakPassword,
  type SignedInSession,
} from './auth-route-helpers';

/**
 * CONTRACT-auth-devices sections 3 and 4: the password change, the session
 * summary, "sign out other browsers" and the first-run setup.
 */
const USERNAME_PATTERN = /^[A-Za-z][A-Za-z0-9_-]{2,63}$/;

function managedByEnv(res: Response): void {
  sendAuthError(
    res,
    409,
    'managed_by_env',
    'The dashboard password is set by an environment variable on the server.'
  );
}

/** A browser session (401 `auth_required`); a device token is 403 `device_scope`. */
export function requireBrowserSession(req: Request, res: Response): boolean {
  const kind = authKind(req);
  if (kind === 'session') return true;
  if (kind === 'device') {
    sendAuthError(res, 403, 'device_scope', 'This action needs a signed-in dashboard session.');
  } else {
    sendAuthError(res, 401, 'auth_required', 'Authentication required');
  }
  return false;
}

interface PasswordChange {
  currentPassword: string;
  newPassword: string;
  signOutOtherBrowsers: boolean;
}

/** Guards 0 to 3 of section 3, before the limiter, so a refused request spends no budget. */
function passwordPreflight(req: Request, res: Response, next: NextFunction): void {
  if (!requireSecureTransport(req, res)) return;
  const state = dashboardAuthState();
  if (!requireAuthConfigured(res, state)) return;
  if (state.managedBy === 'env') return managedByEnv(res);
  if (!requireBrowserSession(req, res)) return;
  const body = readAuthBody(
    req,
    res,
    'required',
    ['currentPassword', 'newPassword'],
    ['signOutOtherBrowsers']
  );
  if (!body) return;
  const { currentPassword, newPassword, signOutOtherBrowsers } = body;
  if (
    typeof currentPassword !== 'string' ||
    currentPassword.length === 0 ||
    currentPassword.length > MAX_PASSWORD_LENGTH ||
    typeof newPassword !== 'string' ||
    newPassword.length > MAX_PASSWORD_LENGTH ||
    (signOutOtherBrowsers !== undefined && typeof signOutOtherBrowsers !== 'boolean')
  ) {
    invalidBody(res);
    return;
  }
  const change: PasswordChange = {
    currentPassword,
    newPassword,
    signOutOtherBrowsers: signOutOtherBrowsers !== false,
  };
  res.locals.passwordChange = change;
  next();
}

async function changePassword(req: Request, res: Response): Promise<void> {
  const change = res.locals.passwordChange as PasswordChange;
  delete res.locals.passwordChange;
  const problem = newPasswordProblem(change.newPassword);
  if (problem) return weakPassword(res, problem);
  if (change.newPassword === change.currentPassword) {
    sendAuthError(res, 400, 'same_password', 'The new password must differ from the current one.');
    return;
  }
  const state = dashboardAuthState();
  if (!BCRYPT_HASH_PATTERN.test(state.passwordHash)) {
    sendAuthError(res, 500, 'invalid_hash', 'The stored password hash is not valid.');
    return;
  }
  // Verify and write inside the one auth write gate, so two changes never cross.
  const outcome = await withAuthWriteGate(async () => {
    const current = dashboardAuthState();
    if (!(await passwordMatches(change.currentPassword, current.passwordHash))) return null;
    const hash = await bcrypt.hash(change.newPassword, passwordHashCost());
    const changedAt = isoTime(authNow());
    mutateConfig((config) => {
      const existing = config.dashboard_auth;
      config.dashboard_auth = {
        enabled: existing?.enabled ?? true,
        username: existing?.username ?? current.username,
        password_hash: hash,
        session_timeout_hours: existing?.session_timeout_hours ?? 24,
        password_changed_at: changedAt,
      };
    });
    return changedAt;
  });
  if (outcome === null) {
    sendAuthError(res, 401, 'wrong_password', 'The current password is not correct.', {
      triesLeft: triesLeft(req),
    });
    return;
  }
  // The password has changed from here on: whatever fails next is reported
  // with the 200, never as an error the client would retry with the old one.
  let signedOutBrowsers = 0;
  let session: SignedInSession | null = null;
  let rotationFailed = false;
  if (change.signOutOtherBrowsers) {
    try {
      const others = countOtherSessions(req.sessionID);
      await bumpSessionEpoch();
      signedOutBrowsers = others;
    } catch {
      rotationFailed = true;
    }
  }
  try {
    const username = req.session.username ?? dashboardAuthState().username;
    session = await startSignedInSession(req, username);
  } catch {
    rotationFailed = true;
  }
  const pairedDevices = activeDevices().length;
  audit('auth.password.changed', 'Dashboard password changed', {
    signedOutBrowsers,
    pairedDevices,
    ...(rotationFailed ? { sessionRotation: 'failed' } : {}),
  });
  res.json({
    ok: true,
    passwordChangedAt: outcome,
    signedOutBrowsers,
    pairedDevices,
    session,
    // Sign in again with the new password; other browsers may still be signed in.
    ...(rotationFailed ? { code: 'session_rotation_failed' } : {}),
  });
}

/** Refused requests answer before the limiter, so they spend nothing. */
function revokeOthersPreflight(req: Request, res: Response, next: NextFunction): void {
  if (!requireAuthConfigured(res)) return;
  if (!requireBrowserSession(req, res)) return;
  if (!readAuthBody(req, res, 'required', [])) return;
  next();
}

async function revokeOtherSessions(req: Request, res: Response): Promise<void> {
  const state = dashboardAuthState();
  const signedOutBrowsers = countOtherSessions(req.sessionID);
  await bumpSessionEpoch();
  await startSignedInSession(req, req.session.username ?? state.username);
  audit('auth.sessions.revoked', 'Other dashboard browsers signed out', {
    count: signedOutBrowsers,
  });
  res.json({ signedOutBrowsers });
}

function sessionSummary(req: Request, res: Response): void {
  const state = dashboardAuthState();
  if (!requireAuthConfigured(res, state)) return;
  if (!requireBrowserSession(req, res)) return;
  if (req.originalUrl.includes('?')) {
    sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return;
  }
  res.json({
    username: req.session.username ?? state.username,
    sessionTimeoutHours: state.sessionTimeoutHours,
    expiresAt: sessionExpiresAt(req),
    otherBrowsers: countOtherSessions(req.sessionID),
    passwordChangedAt: passwordChangedAt(),
    pairedDevices: activeDevices().length,
    managedBy: state.managedBy,
    secureTransport: isSecureTransport(req),
    secureOrigin: secureOrigin(),
  });
}

interface SetupRequest {
  username: string;
  password: string;
  setupCode: string | null;
  loopback: boolean;
}

/** Section 4 guards up to the code check; the login limiter runs only for LAN callers. */
function setupPreflight(req: Request, res: Response, next: NextFunction): void {
  const state = dashboardAuthState();
  if (state.managedBy === 'env') return managedByEnv(res);
  if (state.configured) {
    sendAuthError(res, 409, 'already_configured', 'Dashboard sign-in is already set up.');
    return;
  }
  const loopback = isDirectLoopbackRequest(req);
  if (!loopback && !requireSecureTransport(req, res)) return;
  const body = readAuthBody(req, res, 'absent-or-same', ['username', 'password'], ['setupCode']);
  if (!body) return;
  const { username, password, setupCode } = body;
  if (
    typeof username !== 'string' ||
    typeof password !== 'string' ||
    password.length > MAX_PASSWORD_LENGTH ||
    (setupCode !== undefined && (typeof setupCode !== 'string' || setupCode.length > 32))
  ) {
    invalidBody(res);
    return;
  }
  if (!USERNAME_PATTERN.test(username)) {
    sendAuthError(res, 400, 'invalid_username', 'Choose a username of 3 to 64 letters or digits.');
    return;
  }
  const problem = newPasswordProblem(password);
  if (problem) return weakPassword(res, problem);
  if (!loopback && (typeof setupCode !== 'string' || setupCode.length === 0)) {
    sendAuthError(res, 403, 'setup_code_required', 'Enter the setup code the server printed.');
    return;
  }
  const request: SetupRequest = {
    username,
    password,
    setupCode: typeof setupCode === 'string' ? setupCode : null,
    loopback,
  };
  res.locals.setupRequest = request;
  if (loopback) {
    next();
    return;
  }
  // Wrong codes count against the login limiter's key (per IP) and the server-wide budget.
  signInLimiters(loginRateLimiter)(req, res, next);
}

async function completeSetup(req: Request, res: Response): Promise<void> {
  const request = res.locals.setupRequest as SetupRequest;
  delete res.locals.setupRequest;
  if (!request.loopback && !(hasActiveSetupCode() && setupCodeMatches(request.setupCode ?? ''))) {
    markCredentialsRejected(res);
    sendAuthError(res, 403, 'setup_code_invalid', 'The setup code is not valid.', {
      triesLeft: triesLeft(req),
    });
    return;
  }
  markCredentialsAccepted(res);
  const hash = await bcrypt.hash(request.password, passwordHashCost());
  const written = await withAuthWriteGate(async () => {
    // Re-checked inside the gate: setup never overwrites a password.
    const current = dashboardAuthState();
    if (current.configured || current.managedBy === 'env') return false;
    const changedAt = isoTime(authNow());
    mutateConfig((config) => {
      config.dashboard_auth = {
        enabled: true,
        username: request.username,
        password_hash: hash,
        session_timeout_hours: config.dashboard_auth?.session_timeout_hours ?? 24,
        password_changed_at: changedAt,
      };
    });
    await consumeSetupCode();
    return true;
  });
  if (!written) {
    sendAuthError(res, 409, 'already_configured', 'Dashboard sign-in is already set up.');
    return;
  }
  const session = await startSignedInSession(req, request.username);
  audit('auth.setup.completed', 'Dashboard sign-in set up', {});
  res.status(201).json({ ok: true, username: request.username, session });
}

export function registerAuthSessionRoutes(router: Router): void {
  router.post(
    '/password',
    passwordPreflight,
    passwordChangeLimiter,
    passwordChangeServerLimiter,
    changePassword
  );
  router.get('/session', sessionSummary);
  router.post(
    '/sessions/revoke-others',
    revokeOthersPreflight,
    sessionRotationLimiter,
    revokeOtherSessions
  );
  router.post('/setup', setupPreflight, completeSetup);
}
