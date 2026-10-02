/**
 * The startServer() wiring of CONTRACT-auth-devices 2a, 4 and 6: the optional
 * in-process HTTPS listener (a real TLS socket, so `socket.encrypted` is true),
 * a key readable by others keeping it off, the first-run setup code printed on
 * stdout, and /ws refusing device tokens and signed-out browsers. Temporary
 * CCS_HOME only; the certificate is made with openssl in the fixture folder.
 * Slow bucket: it spawns openssl and starts the real server.
 */
import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import bcrypt from 'bcrypt';
import { spawnSync } from 'child_process';
import * as fs from 'fs';
import https from 'https';
import * as net from 'net';
import type { AddressInfo } from 'net';
import * as os from 'os';
import * as path from 'path';

import { startServer } from '../../../src/web-server';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import { invalidateConfigCache } from '../../../src/config/config-loader-facade';
import { loginRateLimiter } from '../../../src/web-server/middleware/auth-middleware';
import { resetAuthRateLimitsForTests } from '../../../src/web-server/routes/auth-rate-limits';
import { setTrustedProxyResolver } from '../../../src/web-server/middleware/secure-transport';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../../src/web-server/services/account-analytics-service';
import { resetDashboardAuthStateForTests } from '../../../src/web-server/services/dashboard-auth-state';
import { resetDeviceStoreForTests } from '../../../src/web-server/services/dashboard-device-store';
import { resetSetupCodeForTests } from '../../../src/web-server/services/dashboard-setup-code';
import { settleAuthWrites } from '../../../src/web-server/services/dashboard-auth-files';

const ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
  'CCS_SESSION_SECRET',
] as const;
const USERNAME = 'runtime-admin';
const PASSWORD = 'runtime-fixture-password';

let original: Record<string, string | undefined> = {};
let home = '';
let instance: Awaited<ReturnType<typeof startServer>> | undefined;
let stubs: Array<{ mockRestore: () => void }> = [];

beforeEach(() => {
  original = Object.fromEntries(ENVIRONMENT.map((name) => [name, process.env[name]]));
  for (const name of ENVIRONMENT) delete process.env[name];
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-auth-runtime-'));
  process.env.CCS_HOME = home;
  process.env.CCS_DIR = path.join(home, '.ccs');
  process.env.CODEX_HOME = path.join(home, '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(home, '.claude');
  process.env.CCS_SESSION_SECRET = 'isolated-auth-runtime-session-secret';
  fs.mkdirSync(path.join(home, '.ccs'), { recursive: true, mode: 0o700 });
  fs.mkdirSync(path.join(home, 'ui'));
  fs.writeFileSync(path.join(home, 'ui', 'index.html'), '<canvas></canvas>');
  resetDashboardAuthStateForTests();
  resetDeviceStoreForTests();
  resetSetupCodeForTests();
  invalidateConfigCache();
  stubs = [
    spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {}),
    spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {}),
  ];
});

afterEach(async () => {
  if (instance) {
    const { server, httpsServer } = instance;
    for (const client of instance.wss.clients) client.terminate();
    instance.cleanup();
    const closed = new Promise<void>((resolve) => server.close(() => resolve()));
    server.closeAllConnections();
    // Under bun, a socket upgraded to /ws keeps close() pending; bound the wait.
    await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 500))]);
    if (httpsServer?.listening)
      await new Promise<void>((resolve) => httpsServer.close(() => resolve()));
  }
  instance = undefined;
  await settleAuthWrites();
  for (const stub of stubs) stub.mockRestore();
  setTrustedProxyResolver(() => null);
  for (const key of ['127.0.0.1', '::ffff:127.0.0.1']) loginRateLimiter.resetKey(key);
  await resetAuthRateLimitsForTests();
  for (const name of ENVIRONMENT) {
    if (original[name] === undefined) delete process.env[name];
    else process.env[name] = original[name];
  }
  invalidateConfigCache();
  resetDashboardAuthStateForTests();
  resetDeviceStoreForTests();
  resetSetupCodeForTests();
  fs.rmSync(home, { recursive: true, force: true });
});

async function writeConfig(
  auth: 'configured' | 'setup',
  dashboardTls?: Record<string, unknown>
): Promise<void> {
  const config = createEmptyUnifiedConfig();
  config.dashboard_auth =
    auth === 'configured'
      ? {
          enabled: true,
          username: USERNAME,
          password_hash: await bcrypt.hash(PASSWORD, 4),
          session_timeout_hours: 24,
        }
      : { enabled: true, username: '', password_hash: '', session_timeout_hours: 24 };
  if (dashboardTls) (config as unknown as Record<string, unknown>).dashboard_tls = dashboardTls;
  saveUnifiedConfig(config);
  invalidateConfigCache();
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => resolve(port));
    });
  });
}

function makeCertificate(keyMode: number): { cert: string; key: string } | null {
  const folder = path.join(home, 'tls');
  fs.mkdirSync(folder, { mode: 0o700 });
  const cert = path.join(folder, 'dashboard.crt');
  const key = path.join(folder, 'dashboard.key');
  const made = spawnSync(
    'openssl',
    [
      'req',
      '-x509',
      '-newkey',
      'ec',
      '-pkeyopt',
      'ec_paramgen_curve:prime256v1',
      '-nodes',
      '-days',
      '1',
      '-subj',
      '/CN=localhost',
      '-keyout',
      key,
      '-out',
      cert,
    ],
    { stdio: 'ignore', timeout: 20_000 }
  );
  if (made.status !== 0) return null;
  fs.chmodSync(key, keyMode);
  return { cert, key };
}

function httpsGet(
  port: number,
  route: string,
  host: string
): Promise<{ status: number; body: unknown }> {
  return new Promise((resolve, reject) => {
    const request = https.request(
      {
        host: '127.0.0.1',
        port,
        path: route,
        method: 'GET',
        rejectUnauthorized: false,
        headers: { host },
      },
      (response) => {
        let text = '';
        response.on('data', (chunk) => (text += chunk));
        response.on('end', () =>
          resolve({ status: response.statusCode ?? 0, body: JSON.parse(text) })
        );
      }
    );
    request.on('error', reject);
    request.end();
  });
}

describe('in-process HTTPS listener (dashboard_tls.https_listener)', () => {
  it('serves the same app over TLS, where secureTransport is true', async () => {
    const files = makeCertificate(0o600);
    if (!files) return; // openssl unavailable: nothing to check here.
    const port = await freePort();
    await writeConfig('configured', {
      https_listener: { enabled: true, port, cert_path: files.cert, key_path: files.key },
    });
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    expect(instance.httpsServer).toBeTruthy();
    const response = await httpsGet(port, '/api/auth/check', '192.0.2.10:3443');
    expect(response.status).toBe(200);
    expect(response.body).toMatchObject({ secureTransport: true, accessMode: 'login' });
  });

  it('stays off when the key is readable by others, and plain HTTP still works', async () => {
    const files = makeCertificate(0o644);
    if (!files) return;
    const port = await freePort();
    await writeConfig('configured', {
      https_listener: { enabled: true, port, cert_path: files.cert, key_path: files.key },
    });
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    expect(instance.httpsServer).toBeNull();
    const plain = (instance.server.address() as AddressInfo).port;
    const check = await fetch(`http://127.0.0.1:${plain}/api/auth/check`);
    expect(check.status).toBe(200);
  });

  it('is off by default', async () => {
    await writeConfig('configured');
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    expect(instance.httpsServer).toBeNull();
  });
});

describe('first-run setup code at startup', () => {
  /** Starts the server with stdout seen as a terminal or not, and returns what it printed. */
  async function startCapturingStdout(interactive: boolean): Promise<string[]> {
    const printed: string[] = [];
    const descriptor = Object.getOwnPropertyDescriptor(process.stdout, 'isTTY');
    Object.defineProperty(process.stdout, 'isTTY', { value: interactive, configurable: true });
    const write = spyOn(process.stdout, 'write').mockImplementation(((chunk: unknown) => {
      printed.push(String(chunk));
      return true;
    }) as never);
    try {
      instance = await startServer({
        port: 0,
        host: '127.0.0.1',
        staticDir: path.join(home, 'ui'),
      });
      await settleAuthWrites();
    } finally {
      write.mockRestore();
      if (descriptor) Object.defineProperty(process.stdout, 'isTTY', descriptor);
      else delete (process.stdout as { isTTY?: boolean }).isTTY;
    }
    return printed;
  }

  it('prints the code on a terminal once and writes it to ~/.ccs/auth/setup-code', async () => {
    await writeConfig('setup');
    const printed = await startCapturingStdout(true);
    const file = path.join(home, '.ccs', 'auth', 'setup-code');
    const code = fs.readFileSync(file, 'utf8').trim();
    expect(code).toMatch(/^[2-9A-HJ-NP-Z]{4}-[2-9A-HJ-NP-Z]{4}$/);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(printed.filter((line) => line.includes(code)).length).toBe(1);
  });

  it('prints only where the code is when stdout is a log file, not the code', async () => {
    await writeConfig('setup');
    const printed = await startCapturingStdout(false);
    const code = fs.readFileSync(path.join(home, '.ccs', 'auth', 'setup-code'), 'utf8').trim();
    const text = printed.join('');
    expect(text).not.toContain(code);
    expect(text).not.toContain(code.replace('-', ''));
    expect(text).toContain('setup-code');
  });

  it('makes no code for a configured server', async () => {
    await writeConfig('configured');
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    await settleAuthWrites();
    expect(fs.existsSync(path.join(home, '.ccs', 'auth', 'setup-code'))).toBe(false);
  });
});

describe('/ws and dashboard sign-in', () => {
  /** The status line of a raw upgrade request (Bun's ws client has no unexpected-response). */
  function upgradeStatus(port: number, headers: Record<string, string>): Promise<number> {
    return new Promise((resolve) => {
      const socket = net.connect(port, '127.0.0.1');
      let text = '';
      socket.on('connect', () => {
        const lines = [
          'GET /ws HTTP/1.1',
          `Host: 127.0.0.1:${port}`,
          'Upgrade: websocket',
          'Connection: Upgrade',
          'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==',
          'Sec-WebSocket-Version: 13',
          ...Object.entries(headers).map(([name, value]) => `${name}: ${value}`),
        ];
        socket.write(`${lines.join('\r\n')}\r\n\r\n`);
      });
      socket.on('data', (chunk) => {
        text += chunk.toString('latin1');
        const match = /^HTTP\/1\.1 (\d{3})/.exec(text);
        if (match) {
          socket.destroy();
          resolve(Number(match[1]));
        }
      });
      socket.on('error', () => resolve(-1));
      socket.on('close', () => resolve(-1));
    });
  }

  it('refuses a device token and a browser signed out by another one', async () => {
    await writeConfig('configured');
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    const port = (instance.server.address() as AddressInfo).port;
    const base = `http://127.0.0.1:${port}`;
    const login = async () => {
      const response = await fetch(`${base}/api/auth/login`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ username: USERNAME, password: PASSWORD }),
      });
      expect(response.status).toBe(200);
      return /connect\.sid=[^;]*/.exec(response.headers.get('set-cookie') ?? '')?.[0] ?? '';
    };
    const first = await login();
    const second = await login();
    expect(await upgradeStatus(port, { cookie: second, origin: base })).toBe(101);
    expect(
      await upgradeStatus(port, {
        cookie: second,
        origin: base,
        authorization: `Bearer aacd_${'A'.repeat(43)}`,
      })
      // Refused (403 in auth-middleware.test.ts); Bun may drop the refusal's status line.
    ).not.toBe(101);
    const revoke = await fetch(`${base}/api/auth/sessions/revoke-others`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', cookie: first, origin: base },
      body: '{}',
    });
    expect(revoke.status).toBe(200);
    expect(await upgradeStatus(port, { cookie: second, origin: base })).not.toBe(101);
    // The browser that revoked the others still connects with its new cookie.
    const renewed = /connect\.sid=[^;]*/.exec(revoke.headers.get('set-cookie') ?? '')?.[0] ?? '';
    expect(await upgradeStatus(port, { cookie: renewed, origin: base })).toBe(101);
  });
});

describe('/api/auth error answers from the server', () => {
  it('carry a code and no-store under /api/auth only', async () => {
    await writeConfig('configured');
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    const base = `http://127.0.0.1:${(instance.server.address() as AddressInfo).port}`;
    const malformed = (route: string) =>
      fetch(`${base}${route}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: '{"username":',
      });
    const auth = await malformed('/api/auth/login');
    expect(auth.status).toBe(400);
    expect(auth.headers.get('cache-control')).toBe('no-store');
    expect(await auth.json()).toEqual({
      error: 'Invalid JSON in request body',
      code: 'invalid_json',
    });
    const other = await malformed('/api/health');
    expect(other.status).toBe(400);
    expect(await other.json()).toEqual({ error: 'Invalid JSON in request body' });

    const login = await fetch(`${base}/api/auth/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: USERNAME, password: PASSWORD }),
    });
    const cookie = /connect\.sid=[^;]*/.exec(login.headers.get('set-cookie') ?? '')?.[0] ?? '';
    const unknownAuth = await fetch(`${base}/api/auth/no-such-route`, { headers: { cookie } });
    expect(unknownAuth.status).toBe(404);
    expect(unknownAuth.headers.get('cache-control')).toBe('no-store');
    expect(await unknownAuth.json()).toEqual({
      error: 'API endpoint was not found.',
      code: 'not_found',
    });
    const unknownApi = await fetch(`${base}/api/no-such-route`, { headers: { cookie } });
    expect(unknownApi.status).toBe(404);
    expect(await unknownApi.json()).toEqual({ error: 'API endpoint was not found.' });
  });
});
