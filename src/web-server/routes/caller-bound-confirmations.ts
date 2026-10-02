import { createHash } from 'crypto';
import type { Request } from 'express';
import { requestDevice } from '../middleware/request-auth';

/**
 * Who asked: a device id, or a hash of the browser session id. A confirmation
 * token is bound to the caller it was issued to (CONTRACT-auth-devices 6).
 */
export function callerKey(req: Request): string {
  const device = requestDevice(req);
  if (device) return `device:${device.deviceId}`;
  const sessionId = (req as Request & { sessionID?: string }).sessionID;
  return sessionId ? `session:${createHash('sha256').update(sessionId).digest('hex')}` : 'session';
}

const CONFIRMATION_BINDING_TTL_MS = 10 * 60 * 1000;
const MAX_CONFIRMATION_BINDINGS = 64;

export class ConfirmationBindings {
  private readonly bindings = new Map<string, { caller: string; until: number }>();

  private key(token: string): string {
    return createHash('sha256').update(token).digest('hex');
  }

  record(token: string, caller: string): void {
    const now = Date.now();
    for (const [key, binding] of this.bindings) if (binding.until <= now) this.bindings.delete(key);
    while (this.bindings.size >= MAX_CONFIRMATION_BINDINGS) {
      const oldest = this.bindings.keys().next().value;
      if (oldest === undefined) break;
      this.bindings.delete(oldest);
    }
    this.bindings.set(this.key(token), { caller, until: now + CONFIRMATION_BINDING_TTL_MS });
  }

  /** False only when the token was issued here to another caller. */
  allows(token: string, caller: string): boolean {
    const binding = this.bindings.get(this.key(token));
    return !binding || binding.until <= Date.now() || binding.caller === caller;
  }
}
