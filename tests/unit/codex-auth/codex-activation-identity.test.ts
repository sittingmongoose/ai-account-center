import { describe, expect, it } from 'bun:test';
import {
  decodeCodexActivationIdentity,
  matchesCodexActivationIdentity,
} from '../../../src/codex-auth/codex-activation-identity';

function token(auth: unknown, extra: Record<string, unknown> = {}): string {
  const header = Buffer.from('{"alg":"none"}').toString('base64url');
  const payload = Buffer.from(
    JSON.stringify({ email: 'shared@example.test', 'https://api.openai.com/auth': auth, ...extra })
  ).toString('base64url');
  return `${header}.${payload}.fakesig`;
}

describe('private Codex activation identity', () => {
  it('accepts current and JWT-only legacy workspace bindings', () => {
    const jwt = token({ chatgpt_account_id: 'workspace', chatgpt_user_id: 'user' });
    expect(decodeCodexActivationIdentity(jwt, 'workspace')).toEqual({
      accountId: 'workspace',
      userId: 'user',
      email: 'shared@example.test',
    });
    expect(decodeCodexActivationIdentity(jwt, undefined)).toEqual(
      decodeCodexActivationIdentity(jwt, 'workspace')
    );
    expect(decodeCodexActivationIdentity(jwt, null)).toEqual(
      decodeCodexActivationIdentity(jwt, 'workspace')
    );
  });

  it.each([
    ['missing JWT workspace', undefined, 'workspace'],
    ['empty JWT workspace', '', undefined],
    ['whitespace JWT workspace', ' workspace ', undefined],
    ['nonstring JWT workspace', 42, undefined],
    ['conflicting stored workspace', 'workspace', 'other'],
    ['empty stored workspace', 'workspace', ''],
    ['whitespace stored workspace', 'workspace', ' workspace '],
    ['nonstring stored workspace', 'workspace', 42],
  ])('rejects %s', (_label, accountId, storedAccountId) => {
    expect(
      decodeCodexActivationIdentity(token({ chatgpt_account_id: accountId }), storedAccountId)
    ).toBeNull();
  });

  it('uses the supported user_id fallback and rejects contradictory user claims', () => {
    expect(
      decodeCodexActivationIdentity(
        token({ chatgpt_account_id: 'workspace', user_id: 'user' }),
        undefined
      )?.userId
    ).toBe('user');
    expect(
      decodeCodexActivationIdentity(
        token({ chatgpt_account_id: 'workspace', chatgpt_user_id: 'user', user_id: 'other' }),
        undefined
      )
    ).toBeNull();
  });

  it.each([
    ['user', { chatgpt_user_id: 42 }, {}],
    ['empty user', { chatgpt_user_id: '' }, {}],
    ['subject', {}, { sub: [] }],
    ['issuer', {}, { iss: ' issuer ' }],
  ])('rejects malformed optional %s without throwing', (_label, claims, extra) => {
    expect(
      decodeCodexActivationIdentity(
        token({ chatgpt_account_id: 'workspace', ...claims }, extra),
        undefined
      )
    ).toBeNull();
  });

  it('does not substitute email, user or subject for a different workspace', () => {
    const identity = {
      accountId: 'personal',
      userId: 'user',
      subject: 'subject',
      issuer: 'issuer',
      email: 'shared@example.test',
    };
    expect(matchesCodexActivationIdentity(identity, { ...identity, accountId: 'work' })).toBe(
      false
    );
  });

  it.each(['userId', 'subject', 'issuer'] as const)(
    'retains the known %s through subsequent reads',
    (key) => {
      const identity = {
        accountId: 'workspace',
        userId: 'user',
        subject: 'subject',
        issuer: 'issuer',
        email: 'shared@example.test',
      };
      expect(matchesCodexActivationIdentity(identity, { ...identity, [key]: 'changed' })).toBe(
        false
      );
      expect(matchesCodexActivationIdentity(identity, { ...identity, [key]: undefined })).toBe(
        false
      );
    }
  );

  it('accepts display-email rotation only with a retained strong principal', () => {
    const legacy = { accountId: 'workspace', email: 'old@example.test' };
    expect(matchesCodexActivationIdentity(legacy, { ...legacy, email: 'new@example.test' })).toBe(
      false
    );
    const user = { ...legacy, userId: 'user' };
    expect(matchesCodexActivationIdentity(user, { ...user, email: 'new@example.test' })).toBe(true);
    const subject = { ...legacy, subject: 'subject', issuer: 'issuer' };
    expect(matchesCodexActivationIdentity(subject, { ...subject, email: 'new@example.test' })).toBe(
      true
    );
  });
});
