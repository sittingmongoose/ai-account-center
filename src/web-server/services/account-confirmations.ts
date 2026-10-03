import { createHash, randomBytes } from 'crypto';

/**
 * One confirmation helper for every destructive or risky account action
 * (CONTRACT-registry-lifecycle section 5).
 *
 * - The token is 32 random bytes in base64url (43 characters). Only its
 *   SHA-256 is kept, in memory.
 * - It is bound to the action, the subject (account or trash id), the browser
 *   session (SHA-256 of the session id) and a fingerprint of the state the
 *   user reviewed. Any difference at consume time is stale.
 * - TTL 120 s. One-shot: the record is deleted on the first consume attempt,
 *   whether it succeeds or not. At most 32 are pending; the oldest is evicted.
 *   One session holds at most 8 of them and evicts only its own oldest, so it
 *   cannot push other sessions' tokens out.
 * - Tokens are never logged.
 */
export type ConfirmationAction = 'remove' | 'trash-restore' | 'trash-purge' | 'signin-again-active';

export interface ConfirmationBinding {
  action: ConfirmationAction;
  subject: string;
  /** SHA-256 of the session id (sessionKey()); a device can never consume a browser's token. */
  sessionKey: string;
  /** Hash of the reviewed state, e.g. the store entry, the active ids and the default ids. */
  stateFingerprint: string;
}

export interface IssuedConfirmation {
  token: string;
  expiresAt: string;
}

export const CONFIRMATION_TTL_MS = 120_000;
export const MAX_PENDING_CONFIRMATIONS = 32;
export const MAX_PENDING_PER_SESSION = 8;
export const CONFIRMATION_TOKEN_PATTERN = /^[A-Za-z0-9_-]{43}$/;

interface PendingConfirmation extends ConfirmationBinding {
  expiresAt: number;
}

function digest(value: string): string {
  return createHash('sha256').update(value, 'utf8').digest('hex');
}

/** The session part of a binding: never the raw session id. */
export function sessionKey(sessionId: string | undefined): string {
  return digest(`aac-session\0${sessionId ?? ''}`);
}

/** A stable fingerprint of any JSON-serializable state. */
export function stateFingerprint(state: unknown): string {
  return digest(JSON.stringify(state) ?? 'null');
}

export class AccountConfirmationStore {
  private readonly pending = new Map<string, PendingConfirmation>();

  constructor(private readonly now: () => number = Date.now) {}

  private prune(now: number, session: string): void {
    for (const [key, record] of this.pending) if (record.expiresAt <= now) this.pending.delete(key);
    const own = [...this.pending].filter(([, record]) => record.sessionKey === session);
    for (const [key] of own.slice(0, Math.max(0, own.length - MAX_PENDING_PER_SESSION + 1))) {
      this.pending.delete(key);
    }
    while (this.pending.size >= MAX_PENDING_CONFIRMATIONS) {
      const oldest = this.pending.keys().next().value;
      if (oldest === undefined) break;
      this.pending.delete(oldest);
    }
  }

  issue(binding: ConfirmationBinding): IssuedConfirmation {
    const now = this.now();
    this.prune(now, binding.sessionKey);
    const token = randomBytes(32).toString('base64url');
    const expiresAt = now + CONFIRMATION_TTL_MS;
    this.pending.set(digest(token), { ...binding, expiresAt });
    return { token, expiresAt: new Date(expiresAt).toISOString() };
  }

  /** True only for a live token issued for exactly this binding; always one-shot. */
  consume(token: unknown, binding: ConfirmationBinding): boolean {
    if (typeof token !== 'string' || !CONFIRMATION_TOKEN_PATTERN.test(token)) return false;
    const key = digest(token);
    const record = this.pending.get(key);
    this.pending.delete(key);
    return (
      record !== undefined &&
      record.expiresAt > this.now() &&
      record.action === binding.action &&
      record.subject === binding.subject &&
      record.sessionKey === binding.sessionKey &&
      record.stateFingerprint === binding.stateFingerprint
    );
  }

  get size(): number {
    return this.pending.size;
  }
}

let shared: AccountConfirmationStore | null = null;

export function getAccountConfirmations(): AccountConfirmationStore {
  shared ??= new AccountConfirmationStore();
  return shared;
}

/** The contract's names for the shared store. */
export function issueConfirmation(binding: ConfirmationBinding): IssuedConfirmation {
  return getAccountConfirmations().issue(binding);
}

export function consumeConfirmation(token: unknown, binding: ConfirmationBinding): boolean {
  return getAccountConfirmations().consume(token, binding);
}
