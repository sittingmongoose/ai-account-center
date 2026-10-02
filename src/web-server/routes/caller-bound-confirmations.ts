import { createHash } from 'crypto';
import type { Request } from 'express';
import { authKind, requestDevice } from '../middleware/request-auth';

/**
 * Who asked: a paired device, a signed-in browser session (by a hash of its
 * id), or `local`. A confirmation token is bound to the caller it was issued
 * to (CONTRACT-auth-devices 6).
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

/** Both issuers' tokens live 60 seconds or less; a binding outlives its token. */
const CONFIRMATION_BINDING_TTL_MS = 10 * 60 * 1000;
/** A caller's own newest offers; its older ones are dropped first. */
const MAX_BINDINGS_PER_CALLER = 8;
const MAX_CONFIRMATION_BINDINGS = 256;

export interface ConfirmationBindingOptions {
  /**
   * Refuse a token this router has no binding for. Use it when every token
   * the route accepts is issued through the same route (Codex). Without it a
   * token issued elsewhere passes through to its issuer, which still checks
   * it (Antigravity's issuer is not changed here).
   */
  strict?: boolean;
}

/**
 * The tokens this router handed out, by SHA-256, and to whom. One caller can
 * never push another caller's live binding out: each caller keeps at most
 * `MAX_BINDINGS_PER_CALLER`, and the overall cap drops from the caller that
 * holds the most.
 */
export class ConfirmationBindings {
  private readonly bindings = new Map<string, { caller: string; until: number }>();
  private readonly strict: boolean;

  constructor(options: ConfirmationBindingOptions = {}) {
    this.strict = options.strict === true;
  }

  private key(token: string): string {
    return createHash('sha256').update(token).digest('hex');
  }

  private oldestOf(caller: string): string | undefined {
    for (const [key, binding] of this.bindings) if (binding.caller === caller) return key;
    return undefined;
  }

  private busiestCaller(): string | undefined {
    const counts = new Map<string, number>();
    let busiest: string | undefined;
    for (const { caller } of this.bindings.values()) {
      const count = (counts.get(caller) ?? 0) + 1;
      counts.set(caller, count);
      if (busiest === undefined || count > (counts.get(busiest) ?? 0)) busiest = caller;
    }
    return busiest;
  }

  private drop(key: string | undefined): boolean {
    return key !== undefined && this.bindings.delete(key);
  }

  record(token: string, caller: string): void {
    const now = Date.now();
    for (const [key, binding] of this.bindings) if (binding.until <= now) this.bindings.delete(key);
    const key = this.key(token);
    this.bindings.delete(key);
    let own = 0;
    for (const binding of this.bindings.values()) if (binding.caller === caller) own += 1;
    for (; own >= MAX_BINDINGS_PER_CALLER; own -= 1) this.drop(this.oldestOf(caller));
    while (this.bindings.size >= MAX_CONFIRMATION_BINDINGS) {
      const busiest = this.busiestCaller();
      if (busiest === undefined || !this.drop(this.oldestOf(busiest))) break;
    }
    this.bindings.set(key, { caller, until: now + CONFIRMATION_BINDING_TTL_MS });
  }

  /**
   * Whether `caller` may use `token`: the binding names it. A token with no
   * live binding is refused in strict mode and passed through otherwise.
   */
  allows(token: string, caller: string): boolean {
    const binding = this.bindings.get(this.key(token));
    if (!binding || binding.until <= Date.now()) return !this.strict;
    return binding.caller === caller;
  }
}
