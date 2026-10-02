/**
 * B4 review fixes (CONTRACT-auth-devices sections 3, 4, 5, 6 and 11):
 * - a password change that wrote the new hash answers 200 even when the
 *   session steps after it fail;
 * - a login or pairing whose bcrypt check overlapped a password change does
 *   not succeed with the replaced password;
 * - a device token anywhere in a request body is refused;
 * - an encoded device token in a URL never reaches the request log;
 * - the setup-code file is written like the other auth files;
 * - rotation over plain HTTP is refused before the bearer token is checked.
 * Temporary CCS_HOME, bcrypt cost 4, nothing live.
 */
import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import bcrypt from 'bcrypt';
import * as fs from 'fs';
import * as path from 'path';

import { mutateConfig } from '../../../src/config/config-loader-facade';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';
import { prepareFirstRunSetupCode } from '../../../src/web-server/dashboard-auth-runtime';
import { scrubLoggedUrl } from '../../../src/web-server/middleware/request-logging-middleware';
import * as authState from '../../../src/web-server/services/dashboard-auth-state';
import { withAuthWriteGate } from '../../../src/web-server/services/dashboard-auth-files';
import {
  Client,
  hashPassword,
  LAN_HOST,
  LAN_PEER,
  pairTray,
  PASSWORD,
  startAuthHarness,
  type Harness,
} from './dashboard-auth-harness';

const NEW_PASSWORD = 'second-fixture-password';
const TOKEN_BODY = 'Q'.repeat(43);
const TOKEN = `aacd_${TOKEN_BODY}`;
let harness: Harness | null = null;
let stubs: Array<{ mockRestore: () => void }> = [];

afterEach(async () => {
  for (const stub of stubs) stub.mockRestore();
  stubs = [];
  await harness?.close();
  harness = null;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
});

async function signedIn(current: Harness): Promise<Client> {
  const browser = new Client(current);
  expect((await browser.login()).status).toBe(200);
  return browser;
}

/** The first bcrypt check finishes only after a password change has stored a new hash. */
async function changePasswordDuringFirstCheck(): Promise<void> {
  const replacement = await hashPassword(NEW_PASSWORD);
  const realCompare = bcrypt.compare.bind(bcrypt) as (a: string, b: string) => Promise<boolean>;
  let changed = false;
  const compare = spyOn(bcrypt, 'compare').mockImplementation((async (
    data: string,
    hash: string
  ) => {
    const result = await realCompare(data, hash);
    if (!changed) {
      changed = true;
      await withAuthWriteGate(() =>
        mutateConfig((config) => {
          if (config.dashboard_auth) config.dashboard_auth.password_hash = replacement;
        })
      );
    }
    return result;
  }) as never);
  stubs.push(compare);
}

describe('password change after the new hash is written', () => {
  it('answers 200 and says so when signing out the other browsers fails', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const other = await signedIn(harness);
    const bump = spyOn(authState, 'bumpSessionEpoch').mockRejectedValueOnce(
      new Error('fixture: state.json could not be written')
    );
    stubs.push(bump);
    const before = browser.cookie;
    const response = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(response.status).toBe(200);
    expect(response.body).toMatchObject({
      ok: true,
      signedOutBrowsers: 0,
      code: 'session_rotation_failed',
      session: { expiresAt: expect.any(String) },
    });
    // This browser still got a fresh session; the new password is the one that works.
    expect(browser.cookie).not.toBe(before);
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);
    expect((await other.send('GET', '/api/auth/session')).status).toBe(200);
    expect((await new Client(harness).login(PASSWORD)).status).toBe(401);
    expect((await new Client(harness).login(NEW_PASSWORD)).status).toBe(200);
  });

  it('answers 200 with no session when this browser cannot be rotated', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const ensure = spyOn(authState, 'ensureSessionEpoch').mockRejectedValueOnce(
      new Error('fixture: no epoch')
    );
    stubs.push(ensure);
    const response = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
      signOutOtherBrowsers: false,
    });
    expect(response.status).toBe(200);
    expect(response.body).toMatchObject({
      ok: true,
      session: null,
      code: 'session_rotation_failed',
    });
    expect((await new Client(harness).login(NEW_PASSWORD)).status).toBe(200);
  });
});

describe('a password check that overlaps a password change', () => {
  it('does not sign a browser in with the replaced password', async () => {
    harness = await startAuthHarness();
    await changePasswordDuringFirstCheck();
    const browser = new Client(harness);
    const login = await browser.login(PASSWORD);
    expect([login.status, login.body.code]).toEqual([401, 'invalid_credentials']);
    expect((await browser.send('GET', '/api/auth/check')).body.authenticated).toBe(false);
    expect((await new Client(harness).login(NEW_PASSWORD)).status).toBe(200);
  });

  it('does not pair a tray with the replaced password', async () => {
    harness = await startAuthHarness();
    await changePasswordDuringFirstCheck();
    const pair = await pairTray(harness);
    expect([pair.status, pair.body.code]).toEqual([401, 'invalid_credentials']);
    expect(pair.body.token).toBeUndefined();
    expect((await pairTray(harness, { password: NEW_PASSWORD })).status).toBe(201);
  });
});

describe('device tokens outside the Authorization header', () => {
  it('refuses a token in any body string, at any depth, or as a key', async () => {
    harness = await startAuthHarness();
    const bodies = [
      { username: 'x', password: 'y', note: TOKEN },
      { username: 'x', password: 'y', a: { b: [[{ c: `prefix ${TOKEN} suffix` }]] } },
      { username: 'x', password: 'y', [TOKEN]: true },
    ];
    for (const body of bodies) {
      const response = await new Client(harness).send('POST', '/api/auth/login', body);
      expect([response.status, response.body.code]).toEqual([400, 'token_in_query']);
    }
    // A string that only looks like the start of a token is not refused.
    const lookalike = await new Client(harness).send('POST', '/api/auth/login', {
      username: 'x',
      password: `aacd_${'Q'.repeat(42)}`,
    });
    expect(lookalike.body.code).toBe('invalid_credentials');
  });

  it('keeps a percent-encoded token out of the request log', async () => {
    harness = await startAuthHarness({ logging: true });
    invalidateLoggingConfigCache();
    clearRecentLogEntries();
    for (const query of [`?t=%61acd_${TOKEN_BODY}`, `?t=%2561acd_${TOKEN_BODY}`]) {
      await new Client(harness).send('GET', `/api/auth/check${query}`);
    }
    const logged = JSON.stringify(getRecentLogEntries());
    expect(logged).toContain('request.completed');
    expect(logged).not.toContain(TOKEN_BODY);
  });

  it('scrubs raw, encoded and twice-encoded tokens and leaves other URLs alone', () => {
    for (const url of [
      `/api/x?t=${TOKEN}`,
      `/api/x?t=%61acd_${TOKEN_BODY}`,
      `/api/x?t=aacd%5F${TOKEN_BODY}`,
      `/api/x?t=%2561acd_${TOKEN_BODY}`,
      `/api/%61acd_${TOKEN_BODY}/x`,
    ]) {
      const scrubbed = scrubLoggedUrl(url);
      expect(scrubbed).not.toContain(TOKEN_BODY);
      expect(scrubbed).toContain('aacd_[redacted]');
    }
    for (const url of ['/api/accounts?x=%E0%A4%A', '/api/x?q=a%20b', '/api/health']) {
      expect(scrubLoggedUrl(url)).toBe(url);
    }
  });
});

describe('setup-code file', () => {
  it('never follows a link planted at the old temporary name or at the file itself', async () => {
    harness = await startAuthHarness({ mode: 'setup' });
    const authDir = path.join(harness.ccsDir, 'auth');
    fs.mkdirSync(authDir, { recursive: true, mode: 0o700 });
    const outside = path.join(harness.home, 'outside.txt');
    fs.writeFileSync(outside, 'keep');
    fs.symlinkSync(outside, path.join(authDir, `setup-code.${process.pid}.tmp`));
    fs.symlinkSync(outside, path.join(authDir, 'setup-code'));
    const code = await prepareFirstRunSetupCode(() => undefined);
    expect(code).toMatch(/^[2-9A-HJ-NP-Z]{4}-[2-9A-HJ-NP-Z]{4}$/);
    expect(fs.readFileSync(outside, 'utf8')).toBe('keep');
    const file = path.join(authDir, 'setup-code');
    expect(fs.lstatSync(file).isFile()).toBe(true);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(fs.readFileSync(file, 'utf8').trim()).toBe(code as string);
    // No temporary file of the new write is left behind.
    expect(fs.readdirSync(authDir).filter((name) => name.startsWith('.setup-code.'))).toEqual([]);
  });
});

describe('token rotation over plain HTTP', () => {
  it('is refused before the token is checked, so last-seen is not stamped', async () => {
    harness = await startAuthHarness();
    const pair = await pairTray(harness);
    expect(pair.status).toBe(201);
    const token = pair.body.token as string;
    harness.peer.address = LAN_PEER;
    harness.peer.host = LAN_HOST;
    const rotate = await new Client(harness, token).send('POST', '/api/auth/devices/me/rotate');
    expect([rotate.status, rotate.body.code]).toEqual([403, 'secure_transport_required']);
    expect(rotate.headers.get('cache-control')).toBe('no-store');
    harness.peer.address = '127.0.0.1';
    harness.peer.host = null;
    const list = await (await signedIn(harness)).send('GET', '/api/auth/devices');
    expect((list.body.devices as Array<{ lastSeenAt: string | null }>)[0].lastSeenAt).toBeNull();
  });
});
