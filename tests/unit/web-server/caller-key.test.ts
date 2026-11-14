/**
 * Who asked (CONTRACT-auth-devices section 6): a paired device, a signed-in
 * session, or the one `local` caller when dashboard sign-in is off. Reset
 * claims bind their transactions to it.
 */
import { describe, expect, it } from 'bun:test';
import type { Request } from 'express';

import { callerKey } from '../../../src/web-server/routes/caller-key';

describe('callerKey', () => {
  const request = (fields: Record<string, unknown>) => fields as unknown as Request;

  it('names a device, a signed-in session, or the local caller', () => {
    expect(callerKey(request({ auth: { kind: 'device', deviceId: 'dev_0123456789abcdef' } }))).toBe(
      'device:dev_0123456789abcdef'
    );
    const signedIn = request({ sessionID: 'sid-1', session: { authenticated: true } });
    expect(callerKey(signedIn)).toMatch(/^session:[0-9a-f]{64}$/);
    expect(callerKey(signedIn)).toBe(callerKey(signedIn));
    // A request without a signed-in session gets a fresh random id from express-session.
    expect(callerKey(request({ sessionID: 'random-1', session: {} }))).toBe('local');
    expect(callerKey(request({ sessionID: 'random-2', session: {} }))).toBe('local');
    expect(callerKey(request({}))).toBe('local');
  });
});
