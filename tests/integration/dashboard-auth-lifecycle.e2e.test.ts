/**
 * End-to-end dashboard sign-in, sign-out and account lifecycle over real HTTP.
 *
 * Starts the REAL dashboard server in-process via startServer() with a
 * temporary HOME, CCS_HOME and free port, then drives every case with plain
 * fetch and a cookie jar. No product code is changed; no real providers, no
 * ssh, no network beyond localhost and this VM's own LAN address.
 *
 * Collector seam: runAdditionalUsageSource is faked in-process (the B3b unit
 * tests' fake probe idea) so API-key checks never reach real providers. The
 * fake returns ok for aac-key probes and unavailable otherwise, without
 * spawning Python. Codex device-code uses a fake `codex` CLI via
 * CCS_CODEX_PATH (detectCodexCli's own seam).
 */
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  mock,
  spyOn,
} from 'bun:test';
import fs from 'fs';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import { startServer } from '../../src/web-server';
import { createEmptyUnifiedConfig } from '../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../src/config/unified-config-loader';
import { invalidateConfigCache, mutateConfig } from '../../src/config/config-loader-facade';
import { invalidateDashboardNetworkSettings } from '../../src/web-server/services/dashboard-network-config';
import { loginRateLimiter } from '../../src/web-server/middleware/auth-middleware';
import { setTrustedProxyResolver } from '../../src/web-server/middleware/secure-transport';
import { resetAuthRateLimitsForTests } from '../../src/web-server/routes/auth-rate-limits';
import { CodexAutoSwitchService } from '../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../src/web-server/services/account-analytics-service';
import { resetDashboardAuthStateForTests } from '../../src/web-server/services/dashboard-auth-state';
import {
  setAuthClockForTests,
  setPasswordHashCostForTests,
  settleAuthWrites,
} from '../../src/web-server/services/dashboard-auth-files';
import { resetDeviceStoreForTests } from '../../src/web-server/services/dashboard-device-store';
import { resetSetupCodeForTests } from '../../src/web-server/services/dashboard-setup-code';
import * as usageTransport from '../../src/web-server/services/additional-usage-transport';

const USERNAME = 'e2e-admin';
const PASSWORD = 'first-password-123';
const NEW_PASSWORD = 'second-password-456';
const LAN_ADDRESS = '192.168.50.179';

const ENVIRONMENT = [
  'HOME',
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
  'CCS_DASHBOARD_TLS_TRUSTED_PROXY',
  'CCS_SESSION_SECRET',
  'CCS_CODEX_PATH',
] as const;

const RATE_LIMIT_KEYS = [
  '127.0.0.1',
  '::ffff:127.0.0.1',
  '::1',
  LAN_ADDRESS,
  `::ffff:${LAN_ADDRESS}`,
];

function fakeCodexIdToken(email: string, accountId: string): string {
  const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
  return [
    part({ alg: 'none' }),
    part({
      email,
      'https://api.openai.com/auth': {
        chatgpt_plan_type: 'pro',
        chatgpt_account_id: accountId,
        chatgpt_user_id: `user-${accountId}`,
      },
    }),
    'sig',
  ].join('.');
}

const FAKE_CODEX_EMAIL = 'e2e-codex@example.com';
const FAKE_CODEX_ACCOUNT = 'acct-e2e-1';

/** One shared fake `codex` CLI for every test (CCS_CODEX_PATH seam). */
let fakeCodexDir = '';
let fakeCodexPath = '';

beforeAll(() => {
  fakeCodexDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-e2e-fake-codex-'));
  fakeCodexPath = path.join(fakeCodexDir, 'codex');
  const token = fakeCodexIdToken(FAKE_CODEX_EMAIL, FAKE_CODEX_ACCOUNT);
  fs.writeFileSync(
    fakeCodexPath,
    [
      '#!/bin/bash',
      '# Fake `codex login --device-auth`: prints URL+code, waits for the poll,',
      '# writes a valid login into $CODEX_HOME/auth.json, exits 0.',
      'echo "https://auth.openai.com/codex/device"',
      'echo "ABCD-12345"',
      'sleep 2',
      'cat > "$CODEX_HOME/auth.json" <<\'EOF\'',
      `{"tokens":{"id_token":"${token}","access_token":"e2e-access","refresh_token":"e2e-refresh"}}`,
      'EOF',
      'exit 0',
      '',
    ].join('\n'),
    { mode: 0o755 }
  );
  fs.chmodSync(fakeCodexPath, 0o755);
});

afterAll(() => {
  if (fakeCodexDir) fs.rmSync(fakeCodexDir, { recursive: true, force: true });
});

interface E2E {
  root: string;
  homeDir: string;
  ccsHome: string;
  ccsDir: string;
  staticDir: string;
  instance: Awaited<ReturnType<typeof startServer>>;
  /** Loopback base (always). */
  base: string;
  /** LAN base (only when host is 0.0.0.0). */
  lanBase: string | null;
  port: number;
  saved: Record<string, string | undefined>;
  stubs: Array<{ mockRestore: () => void }>;
}

/** A browser (or tray): own cookie jar, JSON, Origin handling. */
class Jar {
  cookie = '';
  bearer: string | null = null;

  constructor(
    readonly base: string,
    readonly origin: string | null = base
  ) {}

  withBearer(token: string | null): Jar {
    const jar = new Jar(this.base, this.origin);
    jar.cookie = this.cookie;
    jar.bearer = token;
    return jar;
  }

  async send(
    method: string,
    route: string,
    body?: unknown,
    headers: Record<string, string> = {}
  ): Promise<{ status: number; body: Record<string, unknown>; headers: Headers; text: string }> {
    const requestHeaders: Record<string, string> = { ...headers };
    const hasBody = body !== undefined;
    if (hasBody && !('content-type' in requestHeaders) && !('Content-Type' in requestHeaders)) {
      requestHeaders['content-type'] = 'application/json';
    }
    if (
      method !== 'GET' &&
      method !== 'HEAD' &&
      this.origin !== null &&
      !('origin' in requestHeaders)
    ) {
      requestHeaders.origin = this.origin;
    }
    if (this.cookie) requestHeaders.cookie = this.cookie;
    if (this.bearer !== null) requestHeaders.authorization = `Bearer ${this.bearer}`;
    const response = await fetch(`${this.base}${route}`, {
      method,
      headers: requestHeaders,
      body: hasBody ? (typeof body === 'string' ? body : JSON.stringify(body)) : undefined,
    });
    const setCookies =
      typeof response.headers.getSetCookie === 'function'
        ? response.headers.getSetCookie()
        : [response.headers.get('set-cookie') ?? ''];
    for (const value of setCookies) {
      const match = /(?:^|,\s*)(connect\.sid=[^;]*)/.exec(value);
      if (match) this.cookie = match[1];
    }
    const text = await response.text();
    let parsed: Record<string, unknown> = {};
    try {
      parsed = text ? (JSON.parse(text) as Record<string, unknown>) : {};
    } catch {
      parsed = {};
    }
    return { status: response.status, body: parsed, headers: response.headers, text };
  }
}

let e2e: E2E | null = null;

function fakeCollector(source: usageTransport.AdditionalUsageSource): Promise<string> {
  const credential = source.account?.credential;
  const isKey = credential?.kind === 'aac-key';
  const now = new Date().toISOString();
  if (isKey) {
    return Promise.resolve(
      JSON.stringify({
        status: 'ok',
        email: null,
        plan: 'Test Plan',
        fetchedAt: now,
        sampledAt: now,
        windows: [{ key: 'weekly', label: 'Weekly', usedPercent: 10 }],
      })
    );
  }
  return Promise.resolve(
    JSON.stringify({
      status: 'unavailable',
      email: null,
      plan: null,
      fetchedAt: null,
      sampledAt: null,
      windows: [],
      message: "No saved credential was found in this computer's existing account stores.",
    })
  );
}

async function startE2E(host: string = '127.0.0.1'): Promise<E2E> {
  const saved = Object.fromEntries(ENVIRONMENT.map((name) => [name, process.env[name]]));
  for (const name of ENVIRONMENT) delete process.env[name];
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-e2e-'));
  const homeDir = path.join(root, 'home');
  const ccsHome = path.join(root, 'ccs-home');
  const staticDir = path.join(root, 'ui');
  fs.mkdirSync(homeDir, { recursive: true });
  fs.mkdirSync(ccsHome, { recursive: true });
  fs.mkdirSync(staticDir, { recursive: true });
  fs.writeFileSync(path.join(staticDir, 'index.html'), '<canvas id="slint-dashboard"></canvas>');
  const ccsDir = path.join(ccsHome, '.ccs');
  process.env.HOME = homeDir;
  process.env.CCS_HOME = ccsHome;
  process.env.CCS_DIR = ccsDir;
  process.env.CODEX_HOME = path.join(homeDir, '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(homeDir, '.claude');
  process.env.CCS_SESSION_SECRET = 'isolated-e2e-session-secret';
  process.env.CCS_CODEX_PATH = fakeCodexPath;
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });

  resetDashboardAuthStateForTests();
  resetDeviceStoreForTests();
  resetSetupCodeForTests();
  setAuthClockForTests(null);
  setPasswordHashCostForTests(4);
  setTrustedProxyResolver(() => null);
  invalidateConfigCache();
  for (const key of RATE_LIMIT_KEYS) loginRateLimiter.resetKey(key);
  await resetAuthRateLimitsForTests();

  const stubs: Array<{ mockRestore: () => void }> = [
    spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {}),
    spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {}),
    spyOn(usageTransport, 'runAdditionalUsageSource').mockImplementation(fakeCollector),
  ];

  const config = createEmptyUnifiedConfig();
  config.dashboard_auth = {
    enabled: true,
    username: '',
    password_hash: '',
    session_timeout_hours: 24,
  };
  saveUnifiedConfig(config);
  invalidateConfigCache();

  const instance = await startServer({ port: 0, host, staticDir });
  const port = (instance.server.address() as AddressInfo).port;
  const base = `http://127.0.0.1:${port}`;
  const lanBase = host === '0.0.0.0' ? `http://${LAN_ADDRESS}:${port}` : null;
  const ctx: E2E = {
    root,
    homeDir,
    ccsHome,
    ccsDir,
    staticDir,
    instance,
    base,
    lanBase,
    port,
    saved,
    stubs,
  };
  e2e = ctx;
  return ctx;
}

async function stopE2E(ctx: E2E | null): Promise<void> {
  if (!ctx) return;
  if (e2e === ctx) e2e = null;
  try {
    for (const client of ctx.instance.wss.clients) client.terminate();
    ctx.instance.cleanup();
    const server = ctx.instance.server;
    const closed = new Promise<void>((resolve) => server.close(() => resolve()));
    server.closeAllConnections();
    await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 500))]);
    if (ctx.instance.httpsServer?.listening) {
      await new Promise<void>((resolve) => ctx.instance.httpsServer?.close(() => resolve()));
    }
  } finally {
    await settleAuthWrites().catch(() => undefined);
    for (const stub of ctx.stubs.splice(0)) {
      try {
        stub.mockRestore();
      } catch {
        /* already restored */
      }
    }
    mock.restore();
    for (const key of RATE_LIMIT_KEYS) {
      try {
        loginRateLimiter.resetKey(key);
      } catch {
        /* limiter without store */
      }
    }
    await resetAuthRateLimitsForTests().catch(() => undefined);
    setAuthClockForTests(null);
    setPasswordHashCostForTests(null);
    setTrustedProxyResolver(() => null);
    resetDashboardAuthStateForTests();
    resetDeviceStoreForTests();
    resetSetupCodeForTests();
    for (const name of ENVIRONMENT) {
      const value = ctx.saved[name];
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    invalidateConfigCache();
    fs.rmSync(ctx.root, { recursive: true, force: true });
  }
}

beforeEach(() => {
  e2e = null;
});

afterEach(async () => {
  await stopE2E(e2e);
});

/** Configure a fresh server via first-run setup from loopback; returns signed-in jar. */
async function setupFresh(
  ctx: E2E,
  username: string = USERNAME,
  password: string = PASSWORD
): Promise<Jar> {
  const jar = new Jar(ctx.base);
  const response = await jar.send('POST', '/api/auth/setup', { username, password });
  expect(response.status).toBe(201);
  expect(response.body).toMatchObject({ ok: true, username });
  expect(jar.cookie).toContain('connect.sid=');
  return jar;
}

async function loginFresh(
  ctx: E2E,
  username: string = USERNAME,
  password: string = PASSWORD
): Promise<Jar> {
  const jar = new Jar(ctx.base);
  const response = await jar.send('POST', '/api/auth/login', { username, password });
  expect(response.status).toBe(200);
  return jar;
}

/** True when this VM can reach its own LAN address (the trusted-network cases need it). */
async function lanReachable(lanBase: string): Promise<boolean> {
  try {
    const probe = await fetch(`${lanBase}/api/auth/check`);
    await probe.text().catch(() => undefined);
    return probe.status === 200;
  } catch {
    return false;
  }
}

/** The temporary config.yaml of a running E2E server, read back from disk. */
function configYamlText(ctx: E2E): string {
  return fs.readFileSync(path.join(ctx.ccsDir, 'config.yaml'), 'utf8');
}

/** Write `dashboard_network` through the real loader, as editing config.yaml would. */
function writeDashboardNetwork(block: {
  trust_local_network: boolean;
  trusted_networks?: string[];
}): void {
  mutateConfig((config) => {
    config.dashboard_network = { ...block };
  });
  invalidateDashboardNetworkSettings();
}

describe('dashboard auth lifecycle e2e (real HTTP)', () => {
  it('case 1: first-run setup from loopback signs in, then check shows signed in', async () => {
    const ctx = await startE2E();
    const jar = new Jar(ctx.base);
    const setupStatus = await jar.send('GET', '/api/auth/setup');
    expect(setupStatus.status).toBe(200);
    expect(setupStatus.body).toMatchObject({ enabled: true, configured: false });
    expect(setupStatus.body.setupCodeRequired).toBe(false);

    const setup = await jar.send('POST', '/api/auth/setup', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(setup.status).toBe(201);
    expect(setup.body).toMatchObject({ ok: true, username: USERNAME });
    expect(setup.headers.get('cache-control')).toBe('no-store');

    const check = await jar.send('GET', '/api/auth/check');
    expect(check.status).toBe(200);
    expect(check.body).toMatchObject({
      authenticated: true,
      username: USERNAME,
      accessMode: 'login',
      authConfigured: true,
    });

    const again = await jar.send('POST', '/api/auth/setup', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(again.status).toBe(409);
    expect(again.body.code).toBe('already_configured');
  });

  it('case 2: login and logout ends the session', async () => {
    const ctx = await startE2E();
    await setupFresh(ctx);
    const jar = await loginFresh(ctx);
    const check = await jar.send('GET', '/api/auth/check');
    expect(check.body).toMatchObject({ authenticated: true, username: USERNAME });

    const savedCookie = jar.cookie;
    const logout = await jar.send('POST', '/api/auth/logout', {});
    expect(logout.status).toBe(200);

    jar.cookie = savedCookie;
    const protectedGet = await jar.send('GET', '/api/auth/session');
    expect(protectedGet.status).toBe(401);
    expect(protectedGet.body.code).toBe('auth_required');
    expect(protectedGet.headers.get('cache-control')).toBe('no-store');
  });

  it('case 3: wrong password counts down triesLeft, then rate limits', async () => {
    const ctx = await startE2E();
    await setupFresh(ctx);
    const jar = new Jar(ctx.base);
    const seen: number[] = [];
    for (let attempt = 0; attempt < 5; attempt++) {
      const response = await jar.send('POST', '/api/auth/login', {
        username: USERNAME,
        password: 'wrong-password-000',
      });
      expect(response.status).toBe(401);
      expect(response.body.code).toBe('invalid_credentials');
      expect(typeof response.body.triesLeft).toBe('number');
      seen.push(response.body.triesLeft as number);
    }
    expect(seen).toEqual([4, 3, 2, 1, 0]);
    const limited = await jar.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: 'wrong-password-000',
    });
    expect(limited.status).toBe(429);
    expect(limited.body.code).toBe('rate_limited');
    expect(typeof limited.body.retryAfterSeconds).toBe('number');
    expect(limited.headers.get('retry-after')).toBe(String(limited.body.retryAfterSeconds));
  });

  it('case 4: password change rotates credentials and signs out other browsers by default', async () => {
    const ctx = await startE2E();
    const first = await setupFresh(ctx);
    const second = await loginFresh(ctx);

    const change = await first.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(200);
    expect(change.body).toMatchObject({ ok: true, signedOutBrowsers: 1 });
    expect(typeof change.body.passwordChangedAt).toBe('string');

    const firstStillIn = await first.send('GET', '/api/auth/session');
    expect(firstStillIn.status).toBe(200);

    const secondOut = await second.send('GET', '/api/auth/session');
    expect(secondOut.status).toBe(401);
    expect(secondOut.body.code).toBe('session_revoked');

    const oldLogin = await new Jar(ctx.base).send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    // The login limiter already counted other failures in this file's process;
    // a wrong password is 401 here because this test made no failed logins.
    expect(oldLogin.status).toBe(401);
    expect(oldLogin.body.code).toBe('invalid_credentials');

    const newLogin = await loginFresh(ctx, USERNAME, NEW_PASSWORD);
    expect(newLogin.cookie).toContain('connect.sid=');

    const short = await first.send('POST', '/api/auth/password', {
      currentPassword: NEW_PASSWORD,
      newPassword: 'short',
    });
    expect(short.status).toBe(400);
    expect(short.body).toMatchObject({ code: 'weak_password', reason: 'too_short' });

    const long = await first.send('POST', '/api/auth/password', {
      currentPassword: NEW_PASSWORD,
      newPassword: 'x'.repeat(73),
    });
    expect(long.status).toBe(400);
    expect(long.body).toMatchObject({ code: 'weak_password', reason: 'too_long' });

    const same = await first.send('POST', '/api/auth/password', {
      currentPassword: NEW_PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(same.status).toBe(400);
    expect(same.body.code).toBe('same_password');

    const wrong = await first.send('POST', '/api/auth/password', {
      currentPassword: 'wrong-current-000',
      newPassword: 'another-valid-789',
    });
    expect(wrong.status).toBe(401);
    expect(wrong.body.code).toBe('wrong_password');
    expect(typeof wrong.body.triesLeft).toBe('number');
  });

  it('case 5: sign out other browsers keeps this browser signed in', async () => {
    const ctx = await startE2E();
    const first = await setupFresh(ctx);
    const second = await loginFresh(ctx);

    const revoke = await first.send('POST', '/api/auth/sessions/revoke-others', {});
    expect(revoke.status).toBe(200);
    expect(revoke.body).toMatchObject({ signedOutBrowsers: 1 });

    const firstStillIn = await first.send('GET', '/api/auth/session');
    expect(firstStillIn.status).toBe(200);
    const secondOut = await second.send('GET', '/api/auth/session');
    expect(secondOut.status).toBe(401);
    expect(secondOut.body.code).toBe('session_revoked');
  });

  it('case 6: device pairing lifecycle keeps working across a password change', async () => {
    const ctx = await startE2E();
    const browser = await setupFresh(ctx);

    const native = new Jar(ctx.base, null);
    const pair = await native.send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-mac', platform: 'mac' },
      { origin: '' }
    );
    expect(pair.status).toBe(201);
    const deviceId = pair.body.deviceId as string;
    const token = pair.body.token as string;
    expect(deviceId).toMatch(/^dev_[0-9a-f]{16}$/);
    expect(token).toMatch(/^aacd_[A-Za-z0-9_-]{43}$/);

    const device = new Jar(ctx.base, null).withBearer(token);
    const me = await device.send('GET', '/api/auth/devices/me');
    expect(me.status).toBe(200);
    expect(me.body).toMatchObject({ id: deviceId, platform: 'mac' });
    expect(JSON.stringify(me.body)).not.toContain(token);

    const dashboard = await device.send(
      'GET',
      '/api/accounts/dashboard?platform=mac&refresh=false'
    );
    expect(dashboard.status).toBe(200);

    const offScope = await device.send('GET', '/api/auth/devices');
    expect(offScope.status).toBe(403);
    expect(offScope.body.code).toBe('device_scope');

    const change = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(200);
    const stillWorks = await device.send('GET', '/api/auth/devices/me');
    expect(stillWorks.status).toBe(200);

    const list = await browser.send('GET', '/api/auth/devices');
    expect(list.status).toBe(200);
    const devices = list.body.devices as Array<Record<string, unknown>>;
    expect(devices.map((entry) => entry.id)).toContain(deviceId);
    expect(JSON.stringify(list.body)).not.toContain(token);
    expect(JSON.stringify(list.body)).not.toContain('tokenSha256');

    const revoke = await browser.send('DELETE', `/api/auth/devices/${deviceId}`, undefined, {
      'content-length': '0',
    });
    expect([200, 204]).toContain(revoke.status);
    const revoked = await device.send('GET', '/api/auth/devices/me');
    expect(revoked.status).toBe(401);
    expect(revoked.body.code).toBe('device_revoked');

    const pairA = await native.send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: NEW_PASSWORD, deviceName: 'e2e-mac-a', platform: 'mac' },
      { origin: '' }
    );
    const pairB = await native.send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: NEW_PASSWORD, deviceName: 'e2e-win-b', platform: 'windows' },
      { origin: '' }
    );
    expect(pairA.status).toBe(201);
    expect(pairB.status).toBe(201);
    const tokenA = pairA.body.token as string;
    const tokenB = pairB.body.token as string;
    const revokeAll = await browser.send('POST', '/api/auth/devices/revoke-all', {});
    expect(revokeAll.status).toBe(200);
    expect(revokeAll.body.revokedDevices).toBe(2);
    for (const dead of [tokenA, tokenB]) {
      const probe = await new Jar(ctx.base, null)
        .withBearer(dead)
        .send('GET', '/api/auth/devices/me');
      expect(probe.status).toBe(401);
      expect(probe.body.code).toBe('device_revoked');
    }
    const browserStillIn = await browser.send('GET', '/api/auth/session');
    expect(browserStillIn.status).toBe(200);

    const pairC = await native.send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: NEW_PASSWORD, deviceName: 'e2e-rotate', platform: 'mac' },
      { origin: '' }
    );
    expect(pairC.status).toBe(201);
    const oldToken = pairC.body.token as string;
    const rotate = await new Jar(ctx.base, null)
      .withBearer(oldToken)
      .send('POST', '/api/auth/devices/me/rotate', {});
    expect(rotate.status).toBe(200);
    const newToken = rotate.body.token as string;
    expect(newToken).toMatch(/^aacd_[A-Za-z0-9_-]{43}$/);
    expect(newToken).not.toBe(oldToken);

    const oldStillWorks = await new Jar(ctx.base, null)
      .withBearer(oldToken)
      .send('GET', '/api/auth/devices/me');
    expect(oldStillWorks.status).toBe(200);
    const firstUse = await new Jar(ctx.base, null)
      .withBearer(newToken)
      .send('GET', '/api/auth/devices/me');
    expect(firstUse.status).toBe(200);
    const oldNowDead = await new Jar(ctx.base, null)
      .withBearer(oldToken)
      .send('GET', '/api/auth/devices/me');
    expect(oldNowDead.status).toBe(401);
  });

  it('case 7: a token in the URL is refused', async () => {
    const ctx = await startE2E();
    await setupFresh(ctx);
    const native = new Jar(ctx.base, null);
    const pair = await native.send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-token-url', platform: 'mac' },
      { origin: '' }
    );
    expect(pair.status).toBe(201);
    const token = pair.body.token as string;

    const device = new Jar(ctx.base, null).withBearer(token);
    const leaked = await device.send('GET', `/api/auth/devices/me?token=${token}`);
    expect(leaked.status).toBe(400);
    expect(leaked.body.code).toBe('token_in_query');

    const publicLeak = await new Jar(ctx.base).send('GET', `/api/auth/check?token=${token}`);
    expect(publicLeak.status).toBe(400);
    expect(publicLeak.body.code).toBe('token_in_query');
  });

  it('case 8: hidden providers round trip and reach the dashboard DTO', async () => {
    const ctx = await startE2E();
    const browser = await setupFresh(ctx);
    const saved = { hiddenProviders: ['kimi-code'], hiddenAccountIds: ['codex:e2e-hidden'] };
    const put = await browser.send('PUT', '/api/accounts/visibility', saved);
    expect(put.status).toBe(200);
    // The store answers with every list; the ones this body left out read empty.
    expect(put.body).toEqual({ ...saved, trayHiddenProviders: [], trayHiddenAccountIds: [] });
    const get = await browser.send('GET', '/api/accounts/visibility');
    expect(get.status).toBe(200);
    expect(get.body).toEqual({ ...saved, trayHiddenProviders: [], trayHiddenAccountIds: [] });

    const dashboard = await browser.send(
      'GET',
      '/api/accounts/dashboard?platform=mac&refresh=false'
    );
    expect(dashboard.status).toBe(200);
    const body = dashboard.body as {
      providers: Array<{ id: string; visible: boolean }>;
      settings: { hiddenProviders: string[]; hiddenAccountIds: string[] };
      accounts: Array<{ provider: string; hidden: boolean; id: string }>;
    };
    expect(body.settings.hiddenProviders).toEqual(['kimi-code']);
    expect(body.settings.hiddenAccountIds).toEqual(['codex:e2e-hidden']);
    const kimi = body.providers.find((entry) => entry.id === 'kimi-code');
    expect(kimi?.visible).toBe(false);
    for (const entry of body.providers) {
      if (entry.id !== 'kimi-code') expect(entry.visible).toBe(true);
    }
    for (const account of body.accounts) {
      if (account.provider === 'kimi-code') expect(account.hidden).toBe(true);
    }
  });

  it('case 8b: per-account Show on dashboard and Show in tray are independent in all four combinations', async () => {
    const ctx = await startE2E();
    const browser = await setupFresh(ctx);
    const id = 'codex:e2e-acct';
    const read = async () => {
      const get = await browser.send('GET', '/api/accounts/visibility');
      expect(get.status).toBe(200);
      const dashboard = await browser.send(
        'GET',
        '/api/accounts/dashboard?platform=mac&refresh=false'
      );
      expect(dashboard.status).toBe(200);
      const settings = (dashboard.body as { settings: Record<string, string[]> }).settings;
      // The dashboard DTO carries exactly what the store saved.
      expect(settings.hiddenAccountIds).toEqual(get.body.hiddenAccountIds);
      expect(settings.trayHiddenAccountIds).toEqual(get.body.trayHiddenAccountIds);
      return get.body as Record<string, string[]>;
    };
    const steps: Array<[string, Record<string, string[]>, boolean, boolean]> = [
      ['shown in both', { hiddenAccountIds: [], trayHiddenAccountIds: [] }, false, false],
      ['hidden only from the dashboard', { hiddenAccountIds: [id] }, true, false],
      ['hidden from both', { trayHiddenAccountIds: [id] }, true, true],
      ['hidden only from the tray', { hiddenAccountIds: [] }, false, true],
      ['shown in both again', { trayHiddenAccountIds: [] }, false, false],
    ];
    for (const [name, body, dashboard, tray] of steps) {
      const put = await browser.send('PUT', '/api/accounts/visibility', body);
      expect([name, put.status]).toEqual([name, 200]);
      const saved = await read();
      expect([
        name,
        saved.hiddenAccountIds.includes(id),
        saved.trayHiddenAccountIds.includes(id),
      ]).toEqual([name, dashboard, tray]);
      expect(saved.hiddenProviders).toEqual([]);
      expect(saved.trayHiddenProviders).toEqual([]);
    }
  });

  it('case 9: API key add, replace and remove never leak the secret', async () => {
    const ctx = await startE2E();
    const browser = await setupFresh(ctx);
    const key = 'zai-TEST-key-0123456789-x7Qa';
    const added = await browser.send('POST', '/api/accounts/add', {
      provider: 'zai',
      key,
      label: 'Work',
    });
    expect(added.status).toBe(201);
    const account = added.body.account as Record<string, unknown>;
    const credential = account.credential as Record<string, unknown>;
    expect(credential).toMatchObject({ kind: 'aac-key', last4: 'x7Qa', storedOn: 'ubuntu' });
    expect(typeof credential.fingerprint).toBe('string');
    expect(String(credential.fingerprint)).toMatch(/^sha256:[0-9a-f]{16}$/);
    expect(JSON.stringify(added.body)).not.toContain(key);
    const id = account.id as string;
    expect(id).toMatch(/^zai:acct:[a-f0-9]{8}$/);

    const replacement = 'zai-NEW-key-9999';
    const replaced = await browser.send('PUT', `/api/accounts/${id}/key`, { key: replacement });
    expect(replaced.status).toBe(200);
    const replacedAccount = replaced.body.account as Record<string, unknown>;
    const replacedCredential = replacedAccount.credential as Record<string, unknown>;
    expect(replacedCredential).toMatchObject({ kind: 'aac-key', last4: '9999' });
    expect(JSON.stringify(replaced.body)).not.toContain(key);
    expect(JSON.stringify(replaced.body)).not.toContain(replacement);

    const prepared = await browser.send('POST', `/api/accounts/${id}/remove`, {});
    expect(prepared.status).toBe(200);
    const confirmation = prepared.body.confirmation as { token: string };
    expect(typeof confirmation.token).toBe('string');
    const removed = await browser.send('POST', `/api/accounts/${id}/remove`, {
      confirmationToken: confirmation.token,
    });
    expect(removed.status).toBe(200);
    expect(removed.body).toMatchObject({ removed: true });
    expect(JSON.stringify(removed.body)).not.toContain(replacement);
  });

  it('case 10: Codex add via device code reaches awaiting state, completes, and cancels', async () => {
    const ctx = await startE2E();
    const browser = await setupFresh(ctx);

    const started = await browser.send('POST', '/api/accounts/add', {
      provider: 'codex',
      profileName: 'e2e-codex-1',
    });
    expect(started.status).toBe(202);
    const job = started.body.job as { id: string };
    expect(job.id).toMatch(/^job_[0-9a-f]{16}$/);

    let seenWaiting = false;
    let seenUrl = '';
    let seenCode: string | null = null;
    for (let attempt = 0; attempt < 40; attempt++) {
      const poll = await browser.send('GET', `/api/accounts/signin-jobs/${job.id}`);
      expect(poll.status).toBe(200);
      const state = poll.body.state as string;
      if (state === 'waiting' || state === 'awaiting_code') {
        seenWaiting = true;
        const verification = poll.body.verification as {
          url: string;
          userCode: string | null;
        } | null;
        expect(verification?.url).toBe('https://auth.openai.com/codex/device');
        expect(verification?.userCode).toBe('ABCD-12345');
        seenUrl = verification?.url ?? '';
        seenCode = verification?.userCode ?? null;
        break;
      }
      if (state === 'succeeded') break;
      expect(['starting', 'waiting', 'awaiting_code', 'verifying']).toContain(state);
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    expect(seenWaiting).toBe(true);
    expect(seenUrl).toBe('https://auth.openai.com/codex/device');
    expect(seenCode).toBe('ABCD-12345');

    let done: Record<string, unknown> | null = null;
    for (let attempt = 0; attempt < 40; attempt++) {
      const poll = await browser.send('GET', `/api/accounts/signin-jobs/${job.id}`);
      if (poll.body.state === 'succeeded') {
        done = poll.body;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    expect(done?.state).toBe('succeeded');
    expect(done?.accountId).toBe('codex:e2e-codex-1');
    const profileAuth = path.join(ctx.ccsDir, 'codex-instances', 'e2e-codex-1', 'auth.json');
    expect(fs.existsSync(profileAuth)).toBe(true);
    expect(fs.existsSync(path.join(ctx.homeDir, '.codex', 'auth.json'))).toBe(false);

    const second = await browser.send('POST', '/api/accounts/add', {
      provider: 'codex',
      profileName: 'e2e-codex-2',
    });
    expect(second.status).toBe(202);
    const job2 = (second.body.job as { id: string }).id;
    for (let attempt = 0; attempt < 40; attempt++) {
      const poll = await browser.send('GET', `/api/accounts/signin-jobs/${job2}`);
      if (poll.body.state === 'waiting' || poll.body.state === 'awaiting_code') break;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    const cancelled = await browser.send('POST', `/api/accounts/signin-jobs/${job2}/cancel`, {});
    expect(cancelled.status).toBe(200);
    expect(['cancelled', 'verifying']).toContain(cancelled.body.state as string);
    let finalState = '';
    for (let attempt = 0; attempt < 40; attempt++) {
      const poll = await browser.send('GET', `/api/accounts/signin-jobs/${job2}`);
      finalState = poll.body.state as string;
      if (finalState === 'cancelled') break;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    expect(finalState).toBe('cancelled');
  });

  it('case 11: sensitive routes need a secure transport; login still works from the LAN', async () => {
    const ctx = await startE2E('0.0.0.0');
    expect(ctx.lanBase).toBe(`http://${LAN_ADDRESS}:${ctx.port}`);
    const browser = await setupFresh(ctx);

    const loopPair = await new Jar(ctx.base, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-loop', platform: 'mac' },
      { origin: '' }
    );
    expect(loopPair.status).toBe(201);

    const lanBase = ctx.lanBase as string;
    let lanReachable = false;
    try {
      const probe = await fetch(`${lanBase}/api/auth/check`);
      lanReachable = probe.status === 200;
      await probe.text().catch(() => undefined);
    } catch {
      lanReachable = false;
    }
    if (!lanReachable) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }

    const lanBrowser = new Jar(lanBase, lanBase);
    const lanLoginFirst = await lanBrowser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(lanLoginFirst.status).toBe(200);

    const lanChange = await lanBrowser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: 'lan-should-fail-123',
    });
    expect(lanChange.status).toBe(403);
    expect(lanChange.body.code).toBe('secure_transport_required');

    const lanPair = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan', platform: 'mac' },
      { origin: '' }
    );
    expect(lanPair.status).toBe(403);
    expect(lanPair.body.code).toBe('secure_transport_required');

    const lanLogin = await new Jar(lanBase, lanBase).send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(lanLogin.status).toBe(200);
  });

  it('case 12: trust off by default: LAN password change and pair refused, login works', async () => {
    const ctx = await startE2E('0.0.0.0');
    await setupFresh(ctx);
    const lanBase = ctx.lanBase as string;
    if (!(await lanReachable(lanBase))) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }

    const lanBrowser = new Jar(lanBase, lanBase);
    const login = await lanBrowser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(login.status).toBe(200);

    const change = await lanBrowser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(403);
    expect(change.body.code).toBe('secure_transport_required');

    const pair = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-off', platform: 'mac' },
      { origin: '' }
    );
    expect(pair.status).toBe(403);
    expect(pair.body.code).toBe('secure_transport_required');

    const check = await lanBrowser.send('GET', '/api/auth/check');
    expect(check.status).toBe(200);
    expect(check.body.trustedLocalNetwork).toBe(false);
    expect(check.body.connection).toMatchObject({ peer: LAN_ADDRESS, trusted: false });
  });

  it('case 13: turning trust on from the LAN is refused; from loopback it works and persists', async () => {
    const ctx = await startE2E('0.0.0.0');
    const browser = await setupFresh(ctx);
    const lanBase = ctx.lanBase as string;
    if (!(await lanReachable(lanBase))) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }

    const lanBrowser = new Jar(lanBase, lanBase);
    const login = await lanBrowser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(login.status).toBe(200);

    const network = await lanBrowser.send('GET', '/api/auth/network');
    expect(network.status).toBe(200);
    expect(network.body).toMatchObject({ trustLocalNetwork: false, canTurnOn: false });

    const refused = await lanBrowser.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect(refused.status).toBe(403);
    expect(refused.body.code).toBe('loopback_required');

    const allowed = await browser.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect(allowed.status).toBe(200);
    expect(allowed.body).toMatchObject({ trustLocalNetwork: true, canTurnOn: true });

    await settleAuthWrites();
    expect(configYamlText(ctx)).toContain('trust_local_network: true');

    const reread = await browser.send('GET', '/api/auth/network');
    expect(reread.status).toBe(200);
    expect(reread.body).toMatchObject({ trustLocalNetwork: true });
  });

  it('case 14: with trust on, password change, pair, Bearer, rotate and key add work from the LAN', async () => {
    const ctx = await startE2E('0.0.0.0');
    const browser = await setupFresh(ctx);
    const lanBase = ctx.lanBase as string;
    if (!(await lanReachable(lanBase))) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }
    const on = await browser.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect(on.status).toBe(200);

    const lanBrowser = new Jar(lanBase, lanBase);
    const login = await lanBrowser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(login.status).toBe(200);

    const check = await lanBrowser.send('GET', '/api/auth/check');
    expect(check.status).toBe(200);
    expect(check.body.trustedLocalNetwork).toBe(true);
    expect(check.body.connection).toMatchObject({ peer: LAN_ADDRESS, trusted: true });

    const pair = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-trusted', platform: 'mac' },
      { origin: '' }
    );
    expect(pair.status).toBe(201);
    const token = pair.body.token as string;
    expect(token).toMatch(/^aacd_[A-Za-z0-9_-]{43}$/);

    const device = new Jar(lanBase, null).withBearer(token);
    const me = await device.send('GET', '/api/auth/devices/me');
    expect(me.status).toBe(200);
    expect(me.body).toMatchObject({ platform: 'mac' });

    const rotate = await device.send('POST', '/api/auth/devices/me/rotate', {});
    expect(rotate.status).toBe(200);
    const rotated = rotate.body.token as string;
    expect(rotated).toMatch(/^aacd_[A-Za-z0-9_-]{43}$/);
    expect(rotated).not.toBe(token);

    const key = 'zai-LAN-trust-key-1234-ab90';
    const added = await lanBrowser.send('POST', '/api/accounts/add', {
      provider: 'zai',
      key,
      label: 'LAN key',
    });
    expect(added.status).toBe(201);
    const credential = (added.body.account as Record<string, unknown>).credential as Record<
      string,
      unknown
    >;
    expect(credential).toMatchObject({ kind: 'aac-key', last4: 'ab90' });
    expect(JSON.stringify(added.body)).not.toContain(key);

    const change = await lanBrowser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(200);
    expect(change.body).toMatchObject({ ok: true });
  });

  it('case 15: turning trust off from a LAN session works; sensitive routes refused again', async () => {
    const ctx = await startE2E('0.0.0.0');
    const browser = await setupFresh(ctx);
    const lanBase = ctx.lanBase as string;
    if (!(await lanReachable(lanBase))) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }
    const on = await browser.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect(on.status).toBe(200);

    const lanBrowser = new Jar(lanBase, lanBase);
    const login = await lanBrowser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(login.status).toBe(200);

    const pairWhileOn = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-before-off', platform: 'mac' },
      { origin: '' }
    );
    expect(pairWhileOn.status).toBe(201);

    const off = await lanBrowser.send('PUT', '/api/auth/network', { trustLocalNetwork: false });
    expect(off.status).toBe(200);
    expect(off.body).toMatchObject({ trustLocalNetwork: false });

    const change = await lanBrowser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(403);
    expect(change.body.code).toBe('secure_transport_required');

    const pair = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-after-off', platform: 'mac' },
      { origin: '' }
    );
    expect(pair.status).toBe(403);
    expect(pair.body.code).toBe('secure_transport_required');

    const check = await lanBrowser.send('GET', '/api/auth/check');
    expect(check.body).toMatchObject({
      trustedLocalNetwork: false,
      connection: { trusted: false },
    });
  });

  it('case 16: a LAN request carrying proxy headers never gains or inherits local-network trust', async () => {
    const ctx = await startE2E('0.0.0.0');
    const browser = await setupFresh(ctx);
    const lanBase = ctx.lanBase as string;
    if (!(await lanReachable(lanBase))) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }

    // Off: spoofing a loopback or public address must not grant trust.
    for (const spoofed of ['127.0.0.1', '203.0.113.9']) {
      const check = await new Jar(lanBase, lanBase).send('GET', '/api/auth/check', undefined, {
        'x-forwarded-for': spoofed,
        'x-forwarded-proto': 'https',
      });
      expect(check.status).toBe(200);
      expect(check.body).toMatchObject({
        trustedLocalNetwork: false,
        connection: { peer: LAN_ADDRESS, trusted: false },
      });
    }
    const spoofedPairOff = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-spoof-off', platform: 'mac' },
      { origin: '', 'x-forwarded-for': '127.0.0.1', 'x-forwarded-proto': 'https' }
    );
    expect(spoofedPairOff.status).toBe(403);
    expect(spoofedPairOff.body.code).toBe('secure_transport_required');

    // On: rule 4 refuses any request that carries a proxy header (R2-BASE review fix 2), so a
    // LAN reverse proxy can never pass its trust on. The peer is still the socket address.
    const on = await browser.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect(on.status).toBe(200);
    for (const spoofed of ['203.0.113.9', '127.0.0.1']) {
      const check = await new Jar(lanBase, lanBase).send('GET', '/api/auth/check', undefined, {
        'x-forwarded-for': spoofed,
      });
      expect(check.status).toBe(200);
      expect(check.body).toMatchObject({
        trustedLocalNetwork: true,
        connection: { peer: LAN_ADDRESS, trusted: false },
      });
    }
    const spoofedPairOn = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      {
        username: USERNAME,
        password: PASSWORD,
        deviceName: 'e2e-lan-spoof-on',
        platform: 'windows',
      },
      { origin: '', 'x-forwarded-for': '203.0.113.9', 'x-forwarded-proto': 'http' }
    );
    expect(spoofedPairOn.status).toBe(403);
    expect(spoofedPairOn.body.code).toBe('secure_transport_required');

    // The same LAN peer without proxy headers is trusted and pairs.
    const plainCheck = await new Jar(lanBase, lanBase).send('GET', '/api/auth/check');
    expect(plainCheck.body).toMatchObject({
      trustedLocalNetwork: true,
      connection: { peer: LAN_ADDRESS, trusted: true },
    });
    const plainPair = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      {
        username: USERNAME,
        password: PASSWORD,
        deviceName: 'e2e-lan-plain-on',
        platform: 'windows',
      },
      { origin: '' }
    );
    expect(plainPair.status).toBe(201);
  });

  it('case 17: a custom trusted_networks list decides the LAN peer, covering VPN subnets', async () => {
    const ctx = await startE2E('0.0.0.0');
    const browser = await setupFresh(ctx);
    const lanBase = ctx.lanBase as string;
    if (!(await lanReachable(lanBase))) {
      console.log(`[e2e] LAN ${LAN_ADDRESS} unreachable; skipping non-loopback assertions`);
      return;
    }

    // A WireGuard-client subnet that excludes the LAN: the LAN peer is not trusted.
    writeDashboardNetwork({ trust_local_network: true, trusted_networks: ['10.6.0.0/24'] });
    const excluded = await new Jar(lanBase, lanBase).send('GET', '/api/auth/check');
    expect(excluded.status).toBe(200);
    expect(excluded.body).toMatchObject({
      trustedLocalNetwork: true,
      connection: { peer: LAN_ADDRESS, trusted: false },
    });
    const pairExcluded = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-excluded', platform: 'mac' },
      { origin: '' }
    );
    expect(pairExcluded.status).toBe(403);
    expect(pairExcluded.body.code).toBe('secure_transport_required');

    const viewExcluded = await browser.send('GET', '/api/auth/network');
    expect(viewExcluded.status).toBe(200);
    expect(viewExcluded.body).toMatchObject({ trustedNetworks: ['10.6.0.0/24'] });

    // Adding the LAN range trusts the peer again, with the VPN subnet still listed.
    writeDashboardNetwork({
      trust_local_network: true,
      trusted_networks: ['10.6.0.0/24', '192.168.0.0/16'],
    });
    const included = await new Jar(lanBase, lanBase).send('GET', '/api/auth/check');
    expect(included.status).toBe(200);
    expect(included.body).toMatchObject({
      trustedLocalNetwork: true,
      connection: { peer: LAN_ADDRESS, trusted: true },
    });
    const pairIncluded = await new Jar(lanBase, null).send(
      'POST',
      '/api/auth/devices/pair',
      { username: USERNAME, password: PASSWORD, deviceName: 'e2e-lan-included', platform: 'mac' },
      { origin: '' }
    );
    expect(pairIncluded.status).toBe(201);

    await settleAuthWrites();
    const saved = configYamlText(ctx);
    expect(saved).toContain('10.6.0.0/24');
    expect(saved).toContain('192.168.0.0/16');
  });
});
