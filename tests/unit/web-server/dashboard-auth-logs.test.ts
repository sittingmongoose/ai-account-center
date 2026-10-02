/**
 * CONTRACT-auth-devices section 12 test 10 and section 11: with the structured
 * log on at debug level and redaction off, setup, pairing, a password change,
 * rotation, a failed login and a token sent in a URL leave no password, setup
 * code, device token, token hash, bcrypt hash or session id in the log or on
 * the console, while the audit lines are there.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';

import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';
import { redactContext } from '../../../src/services/logging/log-redaction';
import { loadOrCreateUnifiedConfig } from '../../../src/config/config-loader-facade';
import { prepareFirstRunSetupCode } from '../../../src/web-server/dashboard-auth-runtime';
import { settleAuthWrites } from '../../../src/web-server/services/dashboard-auth-files';
import { hashDeviceToken } from '../../../src/web-server/services/dashboard-device-store';
import {
  Client,
  LAN_HOST,
  LAN_PEER,
  startAuthHarness,
  type Harness,
} from './dashboard-auth-harness';

const SETUP_PASSWORD = 'setup-secret-password-1';
const CHANGED_PASSWORD = 'changed-secret-password-2';
const WRONG_PASSWORD = 'wrong-secret-password-3';
const consoleText: string[] = [];
const originalConsole = { ...console };
let harness: Harness | null = null;

beforeEach(() => {
  clearRecentLogEntries();
  for (const name of ['log', 'info', 'warn', 'error', 'debug'] as const) {
    console[name] = (...args: unknown[]) => {
      consoleText.push(
        args.map((arg) => (typeof arg === 'string' ? arg : JSON.stringify(arg))).join(' ')
      );
    };
  }
});

afterEach(async () => {
  Object.assign(console, originalConsole);
  consoleText.splice(0);
  await harness?.close();
  harness = null;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
});

function sessionId(cookie: string): string {
  const value = decodeURIComponent(cookie.replace(/^connect\.sid=/, ''));
  return value.replace(/^s:/, '').split('.')[0];
}

describe('dashboard sign-in logging', () => {
  it('keeps every secret out of the log and the console', async () => {
    harness = await startAuthHarness({ mode: 'setup', logging: true });
    invalidateLoggingConfigCache();
    const code = String(await prepareFirstRunSetupCode(() => undefined));

    // First run from the LAN over TLS with the code.
    harness.peer.address = LAN_PEER;
    harness.peer.host = LAN_HOST;
    harness.peer.encrypted = true;
    const browser = new Client(harness);
    const setup = await browser.send('POST', '/api/auth/setup', {
      username: 'log-admin',
      password: SETUP_PASSWORD,
      setupCode: code,
    });
    expect(setup.status).toBe(201);
    const firstSession = sessionId(browser.cookie);

    // Pair a tray, rotate its token, use the new one, send one in a URL.
    const pair = await new Client(harness).send(
      'POST',
      '/api/auth/devices/pair',
      { username: 'log-admin', password: SETUP_PASSWORD, deviceName: 'log-mac', platform: 'mac' },
      { origin: '' }
    );
    expect(pair.status).toBe(201);
    const token = String(pair.body.token);
    const rotated = await new Client(harness, token).send('POST', '/api/auth/devices/me/rotate');
    expect(rotated.status).toBe(200);
    const newToken = String(rotated.body.token);
    expect((await new Client(harness, newToken).send('GET', '/api/auth/devices/me')).status).toBe(
      200
    );
    expect((await new Client(harness, token).send('GET', '/api/auth/devices/me')).status).toBe(401);
    const inUrl = await new Client(harness).send(
      'GET',
      `/api/accounts/dashboard?platform=mac&t=${newToken}`
    );
    expect(inUrl.status).toBe(400);

    // Change the password, then a failed login.
    const change = await browser.send('POST', '/api/auth/password', {
      currentPassword: SETUP_PASSWORD,
      newPassword: CHANGED_PASSWORD,
    });
    expect(change.status).toBe(200);
    const secondSession = sessionId(browser.cookie);
    const failed = await new Client(harness).send('POST', '/api/auth/login', {
      username: WRONG_PASSWORD,
      password: WRONG_PASSWORD,
    });
    expect(failed.status).toBe(401);
    expect(
      (await browser.send('DELETE', `/api/auth/devices/${String(pair.body.deviceId)}`)).status
    ).toBe(204);
    await settleAuthWrites();

    const logged = JSON.stringify(getRecentLogEntries());
    for (const event of [
      'auth.setup.completed',
      'auth.device.paired',
      'auth.device.rotated',
      'auth.device.rejected',
      'auth.password.changed',
      'auth.login.failed',
      'auth.device.revoked',
      'request.completed',
    ]) {
      expect([event, logged.includes(event)]).toEqual([event, true]);
    }
    const bcryptHash = String(loadOrCreateUnifiedConfig().dashboard_auth?.password_hash);
    const secrets = [
      SETUP_PASSWORD,
      CHANGED_PASSWORD,
      WRONG_PASSWORD,
      code,
      code.replace('-', ''),
      token,
      newToken,
      hashDeviceToken(token),
      hashDeviceToken(newToken),
      bcryptHash,
      firstSession,
      secondSession,
    ];
    for (const text of [logged, consoleText.join('\n')]) {
      for (const secret of secrets) {
        expect([secret.slice(0, 6), text.includes(secret)]).toEqual([secret.slice(0, 6), false]);
      }
    }
    // The failed login names its address and reason, never what was typed.
    const failure = getRecentLogEntries().find((entry) => entry.event === 'auth.login.failed');
    expect(failure?.context).toMatchObject({ reason: 'invalid_credentials' });
  });
});

describe('log redaction keys', () => {
  it('redacts the new credential keys when redaction is on', () => {
    const redacted = redactContext({
      currentPassword: 'a',
      newPassword: 'b',
      deviceToken: 'c',
      setupCode: 'd',
      token: 'e',
      adminPassword: 'f',
      clientSecretValue: 'g',
      tokenSha256: 'h',
      note: 'Bearer aacd_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefg',
      url: '/api/x?t=aacd_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefg',
      count: 3,
    });
    for (const key of [
      'currentPassword',
      'newPassword',
      'deviceToken',
      'setupCode',
      'token',
      'adminPassword',
      'clientSecretValue',
      'tokenSha256',
    ]) {
      expect([key, redacted[key]]).toEqual([key, '[redacted]']);
    }
    expect(String(redacted.note)).not.toContain('aacd_ABC');
    expect(String(redacted.url)).not.toContain('aacd_ABC');
    expect(redacted.count).toBe(3);
  });
});
