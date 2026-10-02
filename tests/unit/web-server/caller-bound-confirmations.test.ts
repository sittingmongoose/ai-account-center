/**
 * B4 review fixes for confirmation tokens bound to their caller
 * (CONTRACT-auth-devices section 6): with dashboard sign-in off every caller
 * is the one `local` caller (express-session hands a request without a stored
 * session a new random id each time), Codex refuses a token it never offered,
 * and one caller cannot push another caller's binding out (Antigravity keeps
 * passing a token it never bound to its own issuer). Runs through the real
 * session middleware in a temporary CCS_HOME; activation is stubbed.
 */
import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import type { Request } from 'express';

import * as codexActivation from '../../../src/codex-auth/activate-codex-profile';
import {
  callerKey,
  ConfirmationBindings,
} from '../../../src/web-server/routes/caller-bound-confirmations';
import { Client, startAuthHarness, type Harness } from './dashboard-auth-harness';

let harness: Harness | null = null;
let restore: Array<{ mockRestore: () => void }> = [];

afterEach(async () => {
  for (const stub of restore) stub.mockRestore();
  restore = [];
  await harness?.close();
  harness = null;
});

function codexOffer(token: string): codexActivation.CodexActivationError {
  return new codexActivation.CodexActivationError('busy', 'private-error', {
    reason: 'running_processes',
    confirmation: {
      token,
      expiresAt: '2099-10-01T05:00:00.000Z',
      targetProfile: 'work',
      processes: [],
      warning: 'Stopping these programs interrupts active Codex work.',
    },
  });
}

function stubCodex() {
  const activate = spyOn(codexActivation, 'activateCodexProfile').mockResolvedValue({
    name: 'work',
    email: 'work@example.test',
    plan: 'plus',
    codexHome: '/fixture/.codex',
    previousEmail: null,
  } as never);
  restore.push(activate);
  return activate;
}

describe('with dashboard sign-in off', () => {
  it('confirms a Codex activation offered to this computer', async () => {
    harness = await startAuthHarness({ mode: 'off' });
    const activate = stubCodex();
    activate.mockRejectedValueOnce(codexOffer('o'.repeat(43)));
    const page = new Client(harness);
    const offer = await page.send('POST', '/api/codex/profiles/work/activate', {});
    expect([offer.status, offer.body.code]).toEqual([409, 'busy']);
    // No cookie is kept with sign-in off; a second tab is the same local caller.
    expect(page.cookie).toBe('');
    const confirm = await new Client(harness).send('POST', '/api/codex/profiles/work/activate', {
      confirmationToken: 'o'.repeat(43),
    });
    expect(confirm.status).toBe(200);
    expect(activate).toHaveBeenCalledTimes(2);
    expect(activate).toHaveBeenLastCalledWith('work', { confirmationToken: 'o'.repeat(43) });
  });
});

describe('with dashboard sign-in on', () => {
  it('keeps a Codex confirmation with the browser it was offered to', async () => {
    harness = await startAuthHarness();
    const activate = stubCodex();
    const owner = new Client(harness);
    const other = new Client(harness);
    expect((await owner.login()).status).toBe(200);
    expect((await other.login()).status).toBe(200);
    activate.mockRejectedValueOnce(codexOffer('b'.repeat(43)));
    expect((await owner.send('POST', '/api/codex/profiles/work/activate', {})).status).toBe(409);

    const stolen = await other.send('POST', '/api/codex/profiles/work/activate', {
      confirmationToken: 'b'.repeat(43),
    });
    expect([stolen.status, stolen.body.code]).toEqual([409, 'confirmation_stale']);
    const unknown = await owner.send('POST', '/api/codex/profiles/work/activate', {
      confirmationToken: 'u'.repeat(43),
    });
    expect([unknown.status, unknown.body.code]).toEqual([409, 'confirmation_stale']);
    expect(activate).toHaveBeenCalledTimes(1);

    const confirmed = await owner.send('POST', '/api/codex/profiles/work/activate', {
      confirmationToken: 'b'.repeat(43),
    });
    expect(confirmed.status).toBe(200);
    expect(activate).toHaveBeenLastCalledWith('work', { confirmationToken: 'b'.repeat(43) });
  });
});

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

describe('ConfirmationBindings', () => {
  it('refuses an unbound token in strict mode and passes it through otherwise', () => {
    expect(new ConfirmationBindings({ strict: true }).allows('never-issued', 'local')).toBe(false);
    expect(new ConfirmationBindings().allows('never-issued', 'local')).toBe(true);
  });

  it('never lets one caller push out another caller’s live binding', () => {
    for (const strict of [true, false]) {
      const bindings = new ConfirmationBindings({ strict });
      bindings.record('victim-token', 'device:a');
      for (let index = 0; index < 600; index += 1) {
        bindings.record(`flood-${index}`, 'device:b');
      }
      expect(bindings.allows('victim-token', 'device:a')).toBe(true);
      expect(bindings.allows('victim-token', 'device:b')).toBe(false);
      // The flooding caller keeps only its own newest offers.
      expect(bindings.allows('flood-599', 'device:b')).toBe(true);
      expect(bindings.allows('flood-0', 'device:b')).toBe(!strict);
    }
  });

  it('drops from the busiest caller when many callers fill the table', () => {
    const bindings = new ConfirmationBindings({ strict: true });
    bindings.record('quiet-token', 'session:quiet');
    for (let caller = 0; caller < 40; caller += 1) {
      for (let index = 0; index < 8; index += 1)
        bindings.record(`t-${caller}-${index}`, `c${caller}`);
    }
    expect(bindings.allows('quiet-token', 'session:quiet')).toBe(true);
    expect(bindings.allows('t-39-7', 'c39')).toBe(true);
  });

  it('expires a binding after ten minutes', () => {
    let now = Date.parse('2026-10-02T12:00:00.000Z');
    const clock = spyOn(Date, 'now').mockImplementation(() => now);
    restore.push(clock);
    const strict = new ConfirmationBindings({ strict: true });
    const lenient = new ConfirmationBindings();
    strict.record('token', 'device:a');
    lenient.record('token', 'device:a');
    now += 10 * 60 * 1000 - 1;
    expect(strict.allows('token', 'device:b')).toBe(false);
    expect(lenient.allows('token', 'device:b')).toBe(false);
    now += 2;
    expect(strict.allows('token', 'device:a')).toBe(false);
    expect(lenient.allows('token', 'device:b')).toBe(true);
  });
});
