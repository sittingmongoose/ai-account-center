import { describe, expect, it } from 'bun:test';
import {
  codexAuthHash,
  codexProcessFingerprint,
  consumeCodexActivationConfirmation,
  isCodexActivationBody,
  issueCodexActivationConfirmation,
  type CodexActivationStopPlan,
} from '../../../src/codex-auth/codex-activation-confirmation';

function plan(): CodexActivationStopPlan {
  return {
    identities: [{ pid: 12, ppid: 1, startTime: '24', fingerprint: 'safe-digest' }],
    roots: [12],
    processes: [{ label: 'Codex CLI', pid: 12, role: 'cli' }],
  };
}

describe('opaque one-shot activation confirmations', () => {
  it('binds the target and auth digest, retains no auth or process arguments, and clones scope', () => {
    const approved = plan();
    const hash = codexAuthHash(Buffer.from('private-fixture-auth'));
    const offer = issueCodexActivationConfirmation('work', hash, approved, 1000);
    approved.identities[0].pid = 99;
    approved.roots.push(99);
    expect(offer.token).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(JSON.stringify(offer)).not.toContain('private-fixture-auth');
    expect(JSON.stringify(offer)).not.toContain('safe-digest');
    const consumed = consumeCodexActivationConfirmation(offer.token, 'work', hash, 1001);
    expect(consumed?.roots).toEqual([12]);
    expect(consumed?.identities[0].pid).toBe(12);
    expect(consumeCodexActivationConfirmation(offer.token, 'work', hash, 1002)).toBeUndefined();
  });

  it.each(['target', 'auth', 'expired'] as const)(
    'rejects %s mismatch and consumes the capability',
    (kind) => {
      const offer = issueCodexActivationConfirmation('work', 'auth-digest', plan(), 1000);
      expect(
        consumeCodexActivationConfirmation(
          offer.token,
          kind === 'target' ? 'another' : 'work',
          kind === 'auth' ? 'new-auth' : 'auth-digest',
          kind === 'expired' ? 61_000 : 1001
        )
      ).toBeUndefined();
      expect(
        consumeCodexActivationConfirmation(offer.token, 'work', 'auth-digest', 1002)
      ).toBeUndefined();
    }
  );

  it('hashes executable/cwd/arguments without returning prompts and distinguishes exec changes', () => {
    const original = { exe: '/bin/codex', cwd: '/work', args: ['/bin/codex', 'private-prompt'] };
    const first = codexProcessFingerprint(original);
    expect(first).toMatch(/^[a-f0-9]{64}$/);
    expect(first).not.toContain('private-prompt');
    expect(
      codexProcessFingerprint({ ...original, args: ['/bin/codex', 'new-private-prompt'] })
    ).not.toBe(first);
    expect(codexProcessFingerprint({ ...original, cwd: '/changed' })).not.toBe(first);
  });

  it.each(
    [
      null,
      [],
      '',
      true,
      { force: true },
      { pid: 12 },
      { confirmationToken: 'short' },
      { confirmationToken: 'x'.repeat(43), pid: 12 },
      { confirmationToken: 'x'.repeat(43), argv: [] },
    ].map((body) => ({ body }))
  )('rejects client-selected stop scope %j', ({ body }) => {
    expect(isCodexActivationBody(body)).toBe(false);
  });

  it('accepts only an empty initial request or the opaque confirmation token', () => {
    expect(isCodexActivationBody({})).toBe(true);
    expect(isCodexActivationBody({ confirmationToken: 'x'.repeat(43) })).toBe(true);
  });
});
