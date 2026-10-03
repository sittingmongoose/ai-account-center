/**
 * CONTRACT-auth-devices section 12, tests 1-5, 11, 12 and 13: the password
 * change, sessions after it, first-run setup with the one-time code, exact
 * public paths, secure transport and environment-managed credentials. Every
 * test runs in a temporary CCS_HOME with bcrypt at cost 4.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import yaml from 'js-yaml';

import { prepareFirstRunSetupCode } from '../../../src/web-server/dashboard-auth-runtime';
import { loadOrCreateUnifiedConfig } from '../../../src/config/config-loader-facade';
import {
  Client,
  configYaml,
  LAN_HOST,
  LAN_PEER,
  pairTray,
  PASSWORD,
  startAuthHarness,
  USERNAME,
  type Harness,
} from './dashboard-auth-harness';

const NEW_PASSWORD = 'second-fixture-password';
let harness: Harness | null = null;

afterEach(async () => {
  await harness?.close();
  harness = null;
});

async function signedIn(h: Harness): Promise<Client> {
  const client = new Client(h);
  const response = await client.login();
  expect(response.status).toBe(200);
  return client;
}

function storedAuth() {
  return loadOrCreateUnifiedConfig().dashboard_auth;
}

describe('POST /api/auth/password', () => {
  it('changes the password, rotates the session and keeps the rest of dashboard_auth', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const before = browser.cookie;
    const response = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(response.body).toMatchObject({
      ok: true,
      signedOutBrowsers: 0,
      pairedDevices: 0,
      session: { expiresAt: expect.any(String) },
    });
    expect(typeof response.body.passwordChangedAt).toBe('string');
    expect(browser.cookie).not.toBe(before);
    expect(browser.cookie.length).toBeGreaterThan(0);

    const stored = storedAuth();
    expect(stored?.enabled).toBe(true);
    expect(stored?.username).toBe(USERNAME);
    expect(stored?.session_timeout_hours).toBe(24);
    expect(stored?.password_changed_at).toBe(response.body.passwordChangedAt as string);
    expect(stored?.password_hash.startsWith('$2')).toBe(true);

    expect((await new Client(harness).login(PASSWORD)).status).toBe(401);
    expect((await new Client(harness).login(NEW_PASSWORD)).status).toBe(200);
    // The rotated session still works.
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);
  });

  it('refuses a missing session, a device token, a bad origin, text bodies and unknown keys', async () => {
    harness = await startAuthHarness();
    const body = { currentPassword: PASSWORD, newPassword: NEW_PASSWORD };
    const anonymous = await new Client(harness).send('POST', '/api/auth/password', body);
    expect(anonymous.status).toBe(401);
    expect(anonymous.body.code).toBe('auth_required');

    const paired = await pairTray(harness);
    const tray = new Client(harness, String(paired.body.token));
    const device = await tray.send('POST', '/api/auth/password', body);
    expect([device.status, device.body.code]).toEqual([403, 'device_scope']);

    const browser = await signedIn(harness);
    const noOrigin = await browser.send('POST', '/api/auth/password', body, { origin: '' });
    expect([noOrigin.status, noOrigin.body.code]).toEqual([403, 'origin_required']);
    const foreign = await browser.send('POST', '/api/auth/password', body, {
      origin: 'https://evil.example',
    });
    expect([foreign.status, foreign.body.code]).toEqual([403, 'origin_required']);
    const text = await browser.send('POST', '/api/auth/password', JSON.stringify(body), {
      'content-type': 'text/plain',
    });
    expect([text.status, text.body.code]).toEqual([415, 'json_required']);
    const unknown = await browser.send('POST', '/api/auth/password', { ...body, extra: 1 });
    expect([unknown.status, unknown.body.code]).toEqual([400, 'invalid_body']);
    const big = await browser.send('POST', '/api/auth/password', {
      ...body,
      currentPassword: 'x'.repeat(5000),
    });
    expect([big.status, big.body.code]).toEqual([400, 'invalid_body']);
    const short = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: '1234567',
    });
    expect(short.body).toMatchObject({ code: 'weak_password', reason: 'too_short' });
    expect(short.status).toBe(400);
    const long = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: `${'a'.repeat(71)}é`,
    });
    expect(long.body).toMatchObject({ code: 'weak_password', reason: 'too_long' });
    // Eight code points that are more than eight bytes are fine.
    expect(Buffer.byteLength('éééééééé')).toBe(16);
    const same = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: PASSWORD,
    });
    expect([same.status, same.body.code]).toEqual([400, 'same_password']);
    // Nothing above changed the password.
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
  });

  it('allows five wrong current passwords, then answers 429 with Retry-After', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const hashBefore = storedAuth()?.password_hash;
    const left: unknown[] = [];
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const response = await browser.send('POST', '/api/auth/password', {
        currentPassword: `wrong-password-${attempt}`,
        newPassword: NEW_PASSWORD,
      });
      expect([response.status, response.body.code]).toEqual([401, 'wrong_password']);
      left.push(response.body.triesLeft);
    }
    expect(left).toEqual([4, 3, 2, 1, 0]);
    const sixth = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(sixth.status).toBe(429);
    expect(sixth.body.code).toBe('rate_limited');
    expect(Number(sixth.headers.get('retry-after'))).toBeGreaterThan(0);
    expect(sixth.body.retryAfterSeconds).toBe(Number(sixth.headers.get('retry-after')));
    expect(storedAuth()?.password_hash).toBe(hashBefore);
  });

  it('a successful change does not spend the budget', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const wrong = await browser.send('POST', '/api/auth/password', {
      currentPassword: 'wrong-password',
      newPassword: NEW_PASSWORD,
    });
    expect(wrong.body.triesLeft).toBe(4);
    const weak = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: 'short',
    });
    expect(weak.status).toBe(400);
    const again = await browser.send('POST', '/api/auth/password', {
      currentPassword: 'wrong-again',
      newPassword: NEW_PASSWORD,
    });
    expect(again.body.triesLeft).toBe(3);
  });
});

describe('sessions after a password change', () => {
  it('signs other browsers out by default, and keeps paired trays', async () => {
    harness = await startAuthHarness();
    const paired = await pairTray(harness);
    const tray = new Client(harness, String(paired.body.token));
    const first = await signedIn(harness);
    const second = await signedIn(harness);
    expect((await first.send('GET', '/api/auth/session')).body.otherBrowsers).toBe(1);

    const changed = await first.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(changed.status).toBe(200);
    expect(changed.body.signedOutBrowsers).toBe(1);
    expect(changed.body.pairedDevices).toBe(1);

    const revoked = await second.send('GET', '/api/auth/session');
    expect([revoked.status, revoked.body.code]).toEqual([401, 'session_revoked']);
    // The signed-out browser learns why from /check too, until it signs in again.
    const check = await second.send('GET', '/api/auth/check');
    expect(check.body).toMatchObject({ authenticated: false, signedOutReason: 'revoked' });
    expect((await first.send('GET', '/api/auth/session')).status).toBe(200);
    expect((await tray.send('GET', '/api/auth/devices/me')).status).toBe(200);
    expect((await second.login(NEW_PASSWORD)).status).toBe(200);
    expect((await second.send('GET', '/api/auth/check')).body.signedOutReason).toBeNull();
  });

  it('keeps other browsers with signOutOtherBrowsers false', async () => {
    harness = await startAuthHarness();
    const paired = await pairTray(harness);
    const tray = new Client(harness, String(paired.body.token));
    const first = await signedIn(harness);
    const second = await signedIn(harness);
    const changed = await first.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
      signOutOtherBrowsers: false,
    });
    expect(changed.body.signedOutBrowsers).toBe(0);
    expect((await second.send('GET', '/api/auth/session')).status).toBe(200);
    expect((await tray.send('GET', '/api/auth/devices/me')).status).toBe(200);
  });

  it('revoke-others signs the other browsers out and keeps this one', async () => {
    harness = await startAuthHarness();
    const first = await signedIn(harness);
    const second = await signedIn(harness);
    const third = await signedIn(harness);
    const response = await first.send('POST', '/api/auth/sessions/revoke-others', {});
    expect(response.status).toBe(200);
    expect(response.body).toEqual({ signedOutBrowsers: 2 });
    expect((await second.send('GET', '/api/accounts/settings')).body.code).toBe('session_revoked');
    expect((await third.send('GET', '/api/auth/session')).status).toBe(401);
    const summary = await first.send('GET', '/api/auth/session');
    expect(summary.status).toBe(200);
    expect(summary.body.otherBrowsers).toBe(0);
    expect(
      (await first.send('POST', '/api/auth/sessions/revoke-others', { extra: true })).status
    ).toBe(400);
  });

  it('GET /api/auth/session reports the session, devices and transport', async () => {
    harness = await startAuthHarness({
      dashboardTls: { public_origin: 'https://vm.tailnet.example.ts.net' },
    });
    const browser = await signedIn(harness);
    await pairTray(harness);
    const response = await browser.send('GET', '/api/auth/session');
    expect(response.status).toBe(200);
    expect(response.body).toEqual({
      username: USERNAME,
      sessionTimeoutHours: 720,
      sessionLifetimeDays: 30,
      expiresAt: expect.any(String),
      otherBrowsers: 0,
      passwordChangedAt: null,
      pairedDevices: 1,
      managedBy: 'config',
      secureTransport: true,
      secureOrigin: 'https://vm.tailnet.example.ts.net',
    });
    expect((await browser.send('GET', '/api/auth/session?x=1')).body.code).toBe('unexpected_query');
  });
});

describe('POST /api/auth/setup', () => {
  it('refuses an already configured server', async () => {
    harness = await startAuthHarness();
    const response = await new Client(harness).send('POST', '/api/auth/setup', {
      username: 'another',
      password: NEW_PASSWORD,
    });
    expect([response.status, response.body.code]).toEqual([409, 'already_configured']);
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
  });

  it('works from loopback without a code and signs the browser in', async () => {
    harness = await startAuthHarness({ mode: 'setup' });
    const check = await new Client(harness).send('GET', '/api/auth/setup');
    expect(check.body).toMatchObject({ configured: false, setupCodeRequired: false });
    const browser = new Client(harness);
    const response = await browser.send('POST', '/api/auth/setup', {
      username: 'jared-admin',
      password: NEW_PASSWORD,
    });
    expect(response.status).toBe(201);
    expect(response.body).toMatchObject({
      ok: true,
      username: 'jared-admin',
      session: { expiresAt: expect.any(String) },
    });
    const stored = storedAuth();
    expect(stored).toMatchObject({ enabled: true, username: 'jared-admin' });
    expect(stored?.session_timeout_hours).toBe(12);
    expect(typeof stored?.password_changed_at).toBe('string');
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);
    const again = await new Client(harness).send('POST', '/api/auth/setup', {
      username: 'jared-admin',
      password: 'third-fixture-password',
    });
    expect(again.status).toBe(409);
  });

  it('needs the one-time code from the LAN, over a secure transport', async () => {
    harness = await startAuthHarness({ mode: 'setup' });
    const printed: string[] = [];
    const code = await prepareFirstRunSetupCode((value) => printed.push(value));
    expect(code).toMatch(/^[2-9A-HJ-NP-Z]{4}-[2-9A-HJ-NP-Z]{4}$/);
    expect(printed).toEqual([code as string]);
    const codeFile = path.join(harness.ccsDir, 'auth', 'setup-code');
    expect(fs.statSync(codeFile).mode & 0o777).toBe(0o600);
    expect(fs.statSync(path.dirname(codeFile)).mode & 0o777).toBe(0o700);
    expect(fs.readFileSync(codeFile, 'utf8').trim()).toBe(code as string);

    harness.peer.address = LAN_PEER;
    harness.peer.host = LAN_HOST;
    const body = { username: 'jared-admin', password: NEW_PASSWORD };
    const plain = await new Client(harness).send('POST', '/api/auth/setup', {
      ...body,
      setupCode: code,
    });
    expect([plain.status, plain.body.code]).toEqual([403, 'secure_transport_required']);

    harness.peer.encrypted = true;
    const status = await new Client(harness).send('GET', '/api/auth/setup');
    expect(status.body).toMatchObject({ setupCodeRequired: true, secureTransport: true });
    expect(JSON.stringify(status.body)).not.toContain(String(code).slice(0, 4));
    const missing = await new Client(harness).send('POST', '/api/auth/setup', body);
    expect([missing.status, missing.body.code]).toEqual([403, 'setup_code_required']);
    const wrong = await new Client(harness).send('POST', '/api/auth/setup', {
      ...body,
      setupCode: 'ABCD-EFGH',
    });
    expect([wrong.status, wrong.body.code]).toEqual([403, 'setup_code_invalid']);
    const badName = await new Client(harness).send('POST', '/api/auth/setup', {
      ...body,
      username: '1bad name',
      setupCode: code,
    });
    expect([badName.status, badName.body.code]).toEqual([400, 'invalid_username']);

    const browser = new Client(harness);
    const right = await browser.send('POST', '/api/auth/setup', {
      ...body,
      setupCode: String(code).toLowerCase().replace('-', ''),
    });
    expect(right.status).toBe(201);
    expect(fs.existsSync(codeFile)).toBe(false);
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);
  });

  it('counts wrong LAN codes against the login limiter', async () => {
    harness = await startAuthHarness({ mode: 'setup' });
    const code = await prepareFirstRunSetupCode(() => undefined);
    harness.peer.address = LAN_PEER;
    harness.peer.host = LAN_HOST;
    harness.peer.encrypted = true;
    const body = { username: 'jared-admin', password: NEW_PASSWORD, setupCode: 'WXYZ-WXYZ' };
    for (let attempt = 0; attempt < 5; attempt += 1) {
      expect((await new Client(harness).send('POST', '/api/auth/setup', body)).status).toBe(403);
    }
    const limited = await new Client(harness).send('POST', '/api/auth/setup', {
      ...body,
      setupCode: code,
    });
    expect(limited.status).toBe(429);
    expect(limited.headers.get('retry-after')).toBeTruthy();
    expect(storedAuth()?.username).toBe('');
  });
});

describe('public paths are exact method and path pairs', () => {
  it('keeps /api/auth/setupx and /api/auth/devices private, and pair public', async () => {
    harness = await startAuthHarness();
    const anonymous = new Client(harness);
    expect((await anonymous.send('GET', '/api/auth/setupx')).status).toBe(401);
    expect((await anonymous.send('GET', '/api/auth/devices')).status).toBe(401);
    expect((await anonymous.send('POST', '/api/auth/check', {})).status).toBe(401);
    expect((await anonymous.send('GET', '/api/auth/check')).status).toBe(200);
    expect((await anonymous.send('GET', '/api/health')).status).toBe(200);
    const pair = await pairTray(harness);
    expect(pair.status).toBe(201);
  });
});

describe('secure transport', () => {
  it('refuses password change, LAN setup, pair and rotate over plain HTTP from the LAN', async () => {
    harness = await startAuthHarness();
    const paired = await pairTray(harness);
    const token = String(paired.body.token);
    const hashBefore = storedAuth()?.password_hash;
    harness.peer.address = LAN_PEER;
    harness.peer.host = LAN_HOST;
    // Plain-HTTP login from the LAN keeps working (compatibility, R01).
    const browser = await signedIn(harness);
    const change = await browser.send('POST', '/api/auth/password', {
      currentPassword: 'wrong-password',
      newPassword: NEW_PASSWORD,
    });
    expect([change.status, change.body.code]).toEqual([403, 'secure_transport_required']);
    const pair = await pairTray(harness, { password: 'wrong-password' });
    expect([pair.status, pair.body.code]).toEqual([403, 'secure_transport_required']);
    const rotate = await new Client(harness, token).send('POST', '/api/auth/devices/me/rotate');
    expect([rotate.status, rotate.body.code]).toEqual([403, 'secure_transport_required']);
    // None of them spent the login budget or compared a password.
    const login = await new Client(harness).login('wrong-password');
    expect(login.body.triesLeft).toBe(4);
    expect(storedAuth()?.password_hash).toBe(hashBefore);
  });

  it('LAN setup over plain HTTP is refused before the code is looked at', async () => {
    harness = await startAuthHarness({ mode: 'setup' });
    harness.peer.address = LAN_PEER;
    harness.peer.host = LAN_HOST;
    const response = await new Client(harness).send('POST', '/api/auth/setup', {
      username: 'jared-admin',
      password: NEW_PASSWORD,
    });
    expect([response.status, response.body.code]).toEqual([403, 'secure_transport_required']);
  });

  it('loopback needs a loopback Host; TLS and a configured proxy count', async () => {
    harness = await startAuthHarness();
    const anonymous = new Client(harness);
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(true);
    harness.peer.host = '192.0.2.10:3000';
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(false);
    harness.peer.headers = { 'x-forwarded-proto': 'https' };
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(false);
    harness.peer.encrypted = true;
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(true);
    harness.peer.encrypted = false;
    harness.peer.address = LAN_PEER;
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(false);
  });

  it('trusts X-Forwarded-Proto from a loopback peer only with trusted_proxy configured', async () => {
    harness = await startAuthHarness({
      dashboardTls: {
        trusted_proxy: 'tailscale-serve',
        public_origin: 'https://vm.tailnet.example.ts.net',
      },
    });
    const anonymous = new Client(harness);
    harness.peer.host = 'vm.tailnet.example.ts.net';
    harness.peer.headers = { 'x-forwarded-proto': 'https' };
    for (const route of ['/api/auth/check', '/api/auth/setup']) {
      const response = await anonymous.send('GET', route);
      expect(response.body).toMatchObject({
        secureTransport: true,
        secureOrigin: 'https://vm.tailnet.example.ts.net',
      });
    }
    harness.peer.address = LAN_PEER;
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(false);
    harness.peer.address = '127.0.0.1';
    harness.peer.headers = { 'x-forwarded-proto': 'http' };
    expect((await anonymous.send('GET', '/api/auth/check')).body.secureTransport).toBe(false);
  });

  it('reports no secure origin when none is configured', async () => {
    harness = await startAuthHarness();
    const check = await new Client(harness).send('GET', '/api/auth/check');
    expect(check.body).toMatchObject({ secureOrigin: null, signedOutReason: null });
  });
});

describe('environment-managed credentials', () => {
  it('reports managedBy env, refuses change and setup, and leaves config.yaml untouched', async () => {
    harness = await startAuthHarness({ mode: 'env' });
    const before = fs.readFileSync(configYaml(harness));
    const browser = await signedIn(harness);
    const summary = await browser.send('GET', '/api/auth/session');
    expect(summary.body).toMatchObject({ managedBy: 'env', passwordChangedAt: null });
    const change = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect([change.status, change.body.code]).toEqual([409, 'managed_by_env']);
    const setup = await new Client(harness).send('POST', '/api/auth/setup', {
      username: 'jared-admin',
      password: NEW_PASSWORD,
    });
    expect([setup.status, setup.body.code]).toEqual([409, 'managed_by_env']);
    expect((await new Client(harness).send('GET', '/api/auth/setup')).body.managedBy).toBe('env');
    expect(fs.readFileSync(configYaml(harness)).equals(before)).toBe(true);
    expect(
      (yaml.load(before.toString()) as { dashboard_auth: { username: string } }).dashboard_auth
        .username
    ).toBe('file-user');
  });
});
