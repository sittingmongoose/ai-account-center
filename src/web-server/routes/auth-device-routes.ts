import type { NextFunction, Request, Response, Router } from 'express';
import { loginRateLimiter, triesLeft } from '../middleware/auth-middleware';
import { authKind, requestDevice } from '../middleware/request-auth';
import { authNow, isoTime } from '../services/dashboard-auth-files';
import { bumpSessionEpoch, countOtherSessions } from '../services/dashboard-auth-state';
import {
  activeDevices,
  DeviceStoreError,
  deviceStoreAvailable,
  findActiveDevice,
  idleExpiresAt,
  pairDevice,
  revokeAllDevices,
  revokeDevice,
  rotateAfter,
  rotateDeviceToken,
  type ActiveDeviceRecord,
  type DevicePlatform,
} from '../services/dashboard-device-store';
import {
  audit,
  BCRYPT_HASH_PATTERN,
  dashboardAuthState,
  invalidBody,
  isLoginField,
  MAX_PASSWORD_LENGTH,
  MAX_USERNAME_LENGTH,
  passwordMatches,
  readAuthBody,
  readOptionalEmptyBody,
  requireAuthConfigured,
  requireSecureTransport,
  sendAuthError,
  startSignedInSession,
  timingSafeStringEqual,
} from './auth-route-helpers';

/**
 * Paired tray devices (CONTRACT-auth-devices sections 5, 6 and 7): pairing
 * with the dashboard password, the browser's device list, revoke and
 * revoke-all, and the tray's own `me` routes (read, rotate, disconnect).
 */
const INSTALL_ID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const APP_VERSION = /^[\x21-\x7e]{1,32}$/;
const DEVICE_ID = /^dev_[0-9a-f]{16}$/;
/** No control, format, surrogate, private-use or unassigned code points. */
const PRINTABLE = /^[^\p{Cc}\p{Cf}\p{Cs}\p{Co}\p{Cn}]+$/u;

interface PairRequest {
  username: string;
  password: string;
  name: string;
  platform: DevicePlatform;
  installId: string | null;
  appVersion: string | null;
}

function deviceName(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();
  const length = [...trimmed].length;
  return length >= 1 && length <= 64 && PRINTABLE.test(trimmed) ? trimmed : null;
}

function storeFailure(res: Response, error: unknown): void {
  if (error instanceof DeviceStoreError && error.code === 'too_many_devices') {
    sendAuthError(res, 409, 'too_many_devices', 'Twenty devices are paired. Revoke one first.');
    return;
  }
  if (error instanceof DeviceStoreError && error.code === 'unknown_device') {
    sendAuthError(res, 401, 'device_revoked', 'This device was signed out from the dashboard.');
    return;
  }
  sendAuthError(res, 503, 'auth_store_unavailable', 'Paired devices cannot be saved right now.');
}

/** Section 5 guards 0 to 3; the shared login limiter runs next. */
function pairPreflight(req: Request, res: Response, next: NextFunction): void {
  if (!requireSecureTransport(req, res)) return;
  if (!requireAuthConfigured(res)) return;
  const body = readAuthBody(
    req,
    res,
    'absent-or-same',
    ['username', 'password', 'deviceName', 'platform'],
    ['installId', 'appVersion']
  );
  if (!body) return;
  const name = deviceName(body.deviceName);
  const { username, password, platform, installId, appVersion } = body;
  if (
    !isLoginField(username, MAX_USERNAME_LENGTH) ||
    !isLoginField(password, MAX_PASSWORD_LENGTH) ||
    name === null ||
    (platform !== 'mac' && platform !== 'windows') ||
    (installId !== undefined && (typeof installId !== 'string' || !INSTALL_ID.test(installId))) ||
    (appVersion !== undefined && (typeof appVersion !== 'string' || !APP_VERSION.test(appVersion)))
  ) {
    invalidBody(res);
    return;
  }
  const request: PairRequest = {
    username,
    password,
    name,
    platform,
    installId: typeof installId === 'string' ? installId.toLowerCase() : null,
    appVersion: typeof appVersion === 'string' ? appVersion : null,
  };
  res.locals.pairRequest = request;
  next();
}

async function pair(req: Request, res: Response): Promise<void> {
  const request = res.locals.pairRequest as PairRequest;
  delete res.locals.pairRequest;
  const state = dashboardAuthState();
  if (!BCRYPT_HASH_PATTERN.test(state.passwordHash)) {
    sendAuthError(res, 500, 'invalid_hash', 'The stored password hash is not valid.');
    return;
  }
  const usernameMatch = timingSafeStringEqual(request.username, state.username);
  const passwordMatch = await passwordMatches(request.password, state.passwordHash);
  if (!usernameMatch || !passwordMatch) {
    audit('auth.login.failed', 'Tray pairing sign-in failed', {
      remoteAddress: req.socket.remoteAddress ?? null,
      reason: 'invalid_credentials',
    });
    sendAuthError(res, 401, 'invalid_credentials', 'Invalid credentials', {
      triesLeft: triesLeft(req),
    });
    return;
  }
  if (!deviceStoreAvailable()) {
    storeFailure(res, new DeviceStoreError('store_unavailable'));
    return;
  }
  try {
    const { device, token, replacedDeviceId } = await pairDevice({
      name: request.name,
      platform: request.platform,
      installId: request.installId,
      appVersion: request.appVersion,
      address: req.ip ?? req.socket.remoteAddress ?? null,
    });
    if (replacedDeviceId) {
      audit('auth.device.revoked', 'Tray device revoked', {
        deviceId: replacedDeviceId,
        by: 'self',
      });
    }
    audit('auth.device.paired', 'Tray device paired', {
      deviceId: device.id,
      platform: device.platform,
      name: device.name,
    });
    res.status(201).json({
      deviceId: device.id,
      token,
      name: device.name,
      platform: device.platform,
      pairedAt: device.pairedAt,
      rotateAfter: isoTime(rotateAfter(device)),
    });
  } catch (error) {
    storeFailure(res, error);
  }
}

/** The device list never carries a hash, an install id or a token. */
function publicDevice(device: ActiveDeviceRecord): Record<string, unknown> {
  return {
    id: device.id,
    name: device.name,
    platform: device.platform,
    appVersion: device.appVersion,
    pairedAt: device.pairedAt,
    lastSeenAt: device.lastSeenAt,
    lastSeenAddress: device.lastSeenAddress,
    rotatedAt: device.rotatedAt,
    idleExpiresAt: isoTime(idleExpiresAt(device)),
  };
}

function browserOnly(req: Request, res: Response): boolean {
  if (!requireAuthConfigured(res)) return false;
  const kind = authKind(req);
  if (kind === 'session') return true;
  if (kind === 'device') {
    sendAuthError(res, 403, 'device_scope', 'This action needs a signed-in dashboard session.');
  } else {
    sendAuthError(res, 401, 'auth_required', 'Authentication required');
  }
  return false;
}

function deviceOnly(req: Request, res: Response): ActiveDeviceRecord | null {
  if (!requireAuthConfigured(res)) return null;
  const auth = requestDevice(req);
  if (!auth) {
    sendAuthError(res, 403, 'device_required', "This route is for a paired tray's own token.");
    return null;
  }
  const device = findActiveDevice(auth.deviceId);
  if (!device) {
    sendAuthError(res, 401, 'device_revoked', 'This device was signed out from the dashboard.');
    return null;
  }
  return device;
}

function listDevices(req: Request, res: Response): void {
  if (!browserOnly(req, res)) return;
  if (req.originalUrl.includes('?')) {
    sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return;
  }
  if (!deviceStoreAvailable()) return storeFailure(res, null);
  res.json({ devices: activeDevices().map(publicDevice) });
}

async function revokeOne(req: Request, res: Response): Promise<void> {
  if (!browserOnly(req, res)) return;
  if (!readOptionalEmptyBody(req, res, 'required')) return;
  const id = req.params.id;
  try {
    if (!DEVICE_ID.test(id) || !(await revokeDevice(id, 'dashboard'))) {
      sendAuthError(res, 404, 'unknown_device', 'That device is not paired.');
      return;
    }
  } catch (error) {
    storeFailure(res, error);
    return;
  }
  audit('auth.device.revoked', 'Tray device revoked', { deviceId: id, by: 'dashboard' });
  res.status(204).end();
}

async function revokeAll(req: Request, res: Response): Promise<void> {
  if (!browserOnly(req, res)) return;
  const body = readAuthBody(req, res, 'required', [], ['signOutOtherBrowsers']);
  if (!body) return;
  if (body.signOutOtherBrowsers !== undefined && typeof body.signOutOtherBrowsers !== 'boolean') {
    invalidBody(res);
    return;
  }
  const ids = activeDevices().map((device) => device.id);
  let revokedDevices = 0;
  try {
    revokedDevices = await revokeAllDevices();
  } catch (error) {
    storeFailure(res, error);
    return;
  }
  for (const deviceId of ids) {
    audit('auth.device.revoked', 'Tray device revoked', { deviceId, by: 'revoke-all' });
  }
  let signedOutBrowsers = 0;
  if (body.signOutOtherBrowsers !== false) {
    signedOutBrowsers = countOtherSessions(req.sessionID);
    await bumpSessionEpoch();
    audit('auth.sessions.revoked', 'Other dashboard browsers signed out', {
      count: signedOutBrowsers,
    });
  }
  // The current browser stays signed in, on a fresh session id.
  await startSignedInSession(req, req.session.username ?? dashboardAuthState().username);
  res.json({ revokedDevices, signedOutBrowsers });
}

function readMe(req: Request, res: Response): void {
  const device = deviceOnly(req, res);
  if (!device) return;
  if (req.originalUrl.includes('?')) {
    sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return;
  }
  // A request made with the previous token of a rotation is told to rotate now.
  const rotateAt = requestDevice(req)?.viaPreviousToken ? authNow() : rotateAfter(device);
  res.json({
    id: device.id,
    name: device.name,
    platform: device.platform,
    pairedAt: device.pairedAt,
    rotateAfter: isoTime(rotateAt),
    idleExpiresAt: isoTime(idleExpiresAt(device)),
  });
}

async function rotateMe(req: Request, res: Response): Promise<void> {
  if (!requireSecureTransport(req, res)) return;
  const device = deviceOnly(req, res);
  if (!device) return;
  if (!readOptionalEmptyBody(req, res, 'absent-or-same')) return;
  const presented = requestDevice(req)?.tokenSha256 ?? '';
  try {
    const rotated = await rotateDeviceToken(device.id, presented);
    audit('auth.device.rotated', 'Tray device token rotated', { deviceId: device.id });
    res.json({ token: rotated.token, rotateAfter: isoTime(rotateAfter(rotated.device)) });
  } catch (error) {
    storeFailure(res, error);
  }
}

async function disconnectMe(req: Request, res: Response): Promise<void> {
  const device = deviceOnly(req, res);
  if (!device) return;
  if (!readOptionalEmptyBody(req, res, 'absent-or-same')) return;
  try {
    await revokeDevice(device.id, 'self');
  } catch (error) {
    storeFailure(res, error);
    return;
  }
  audit('auth.device.revoked', 'Tray device revoked', { deviceId: device.id, by: 'self' });
  res.status(204).end();
}

export function registerAuthDeviceRoutes(router: Router): void {
  router.post('/devices/pair', pairPreflight, loginRateLimiter, pair);
  router.get('/devices', listDevices);
  router.post('/devices/revoke-all', revokeAll);
  router.get('/devices/me', readMe);
  router.post('/devices/me/rotate', rotateMe);
  router.delete('/devices/me', disconnectMe);
  router.delete('/devices/:id', revokeOne);
}
