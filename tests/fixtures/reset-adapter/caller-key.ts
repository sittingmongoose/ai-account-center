import { createHash } from 'crypto';
import type { Request } from 'express';
import { authKind, requestDevice } from '../../../src/web-server/middleware/request-auth';

/**
 * Who asked: a paired device, a signed-in browser session (by a hash of its
 * id), or `local`. Reset-claim transactions are bound to the caller that
 * prepared them (CONTRACT-auth-devices 6).
 *
 * Only a signed-in session has a stable id: express-session gives every
 * request without a stored session a fresh random one. With dashboard sign-in
 * off, writes are already limited to this computer (localhost access), so
 * every such caller is the one `local` caller.
 */
export function callerKey(req: Request): string {
  const device = requestDevice(req);
  if (device) return `device:${device.deviceId}`;
  if (authKind(req) === 'session') {
    const sessionId = (req as Request & { sessionID?: string }).sessionID;
    if (sessionId) return `session:${createHash('sha256').update(sessionId).digest('hex')}`;
  }
  return 'local';
}
