/**
 * Confirmation tokens (CONTRACT-registry-lifecycle section 5): one-shot,
 * 120 s, bound to action, subject, session and reviewed state, at most 32,
 * and at most 8 per session.
 */
import { describe, expect, it } from 'bun:test';
import {
  AccountConfirmationStore,
  CONFIRMATION_TTL_MS,
  MAX_PENDING_CONFIRMATIONS,
  MAX_PENDING_PER_SESSION,
  sessionKey,
  stateFingerprint,
  type ConfirmationBinding,
} from '../../../src/web-server/services/account-confirmations';

function binding(extra: Partial<ConfirmationBinding> = {}): ConfirmationBinding {
  return {
    action: 'remove',
    subject: 'codex:party',
    sessionKey: sessionKey('session-a'),
    stateFingerprint: stateFingerprint({ entry: 'party', active: 'gmail' }),
    ...extra,
  };
}

describe('account confirmation tokens', () => {
  it('issues a 43-character base64url token that works exactly once', () => {
    let now = 1_000_000;
    const store = new AccountConfirmationStore(() => now);
    const issued = store.issue(binding());
    expect(issued.token).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(issued.expiresAt).toBe(new Date(now + CONFIRMATION_TTL_MS).toISOString());
    now += 1000;
    expect(store.consume(issued.token, binding())).toBe(true);
    expect(store.consume(issued.token, binding())).toBe(false);
  });

  it('is deleted on the first attempt even when that attempt fails', () => {
    const store = new AccountConfirmationStore();
    const issued = store.issue(binding());
    expect(store.consume(issued.token, binding({ subject: 'codex:gmail' }))).toBe(false);
    expect(store.consume(issued.token, binding())).toBe(false);
    expect(store.size).toBe(0);
  });

  it('refuses another session, action, subject or a changed state', () => {
    const store = new AccountConfirmationStore();
    for (const change of [
      { sessionKey: sessionKey('session-b') },
      { action: 'trash-restore' as const },
      { subject: 'codex:other' },
      { stateFingerprint: stateFingerprint({ entry: 'party', active: 'party' }) },
    ]) {
      const issued = store.issue(binding());
      expect(store.consume(issued.token, binding(change))).toBe(false);
    }
  });

  it('expires at 120 s on the injected clock', () => {
    let now = 5_000;
    const store = new AccountConfirmationStore(() => now);
    const early = store.issue(binding());
    now += CONFIRMATION_TTL_MS - 1;
    expect(store.consume(early.token, binding())).toBe(true);
    const late = store.issue(binding());
    now += CONFIRMATION_TTL_MS;
    expect(store.consume(late.token, binding())).toBe(false);
  });

  it('keeps at most 32 pending and evicts the oldest', () => {
    const store = new AccountConfirmationStore();
    const session = (index: number) => ({ sessionKey: sessionKey(`session-${index % 5}`) });
    const first = store.issue(binding(session(0)));
    for (let index = 1; index <= MAX_PENDING_CONFIRMATIONS; index += 1) {
      store.issue(binding(session(index)));
    }
    expect(store.size).toBe(MAX_PENDING_CONFIRMATIONS);
    expect(store.consume(first.token, binding(session(0)))).toBe(false);
  });

  it('lets one session evict only its own oldest tokens, never another session', () => {
    const store = new AccountConfirmationStore();
    const other = binding({ sessionKey: sessionKey('session-b') });
    const kept = store.issue(other);
    const own = [store.issue(binding())];
    for (let index = 0; index < 3 * MAX_PENDING_CONFIRMATIONS; index += 1) {
      own.push(store.issue(binding()));
    }
    expect(store.size).toBe(MAX_PENDING_PER_SESSION + 1);
    expect(store.consume(own[0].token, binding())).toBe(false);
    expect(store.consume(own.at(-1)?.token, binding())).toBe(true);
    expect(store.consume(kept.token, other)).toBe(true);
  });

  it('never keeps the raw token or session id', () => {
    const store = new AccountConfirmationStore();
    const issued = store.issue(binding());
    const internals = JSON.stringify([
      ...(store as unknown as { pending: Map<string, unknown> }).pending,
    ]);
    expect(internals).not.toContain(issued.token);
    expect(internals).not.toContain('session-a');
    expect(store.consume('not a token', binding())).toBe(false);
    expect(store.consume(42, binding())).toBe(false);
  });
});
