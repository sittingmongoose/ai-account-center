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
  fs.mkdirSync(path.join(home, 'ui', 'app'), { recursive: true });
  fs.writeFileSync(path.join(home, 'ui', 'app', 'index.html'), '<main></main>');
  resetDashboardAuthStateForTests();
  resetDeviceStoreForTests();
  resetSetupCodeForTests();
  invalidateConfigCache();
  stubs = [];
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

const rustPublicBinary = process.env.AAC_REVIEW_RUST_HTTP;
const legacyDashboard = process.env.AAC_REVIEW_LEGACY_DASHBOARD;

async function startRustAuth() {
  const rust = Bun.spawn([rustPublicBinary!, '--public'], {
    env: {
      ...process.env,
      AAC_RUST_PUBLIC_LISTEN: '127.0.0.1:0',
      AAC_RUST_COMPOSE_UPSTREAM: '127.0.0.1:1',
      AAC_RUST_COMPOSE_PROOF: 'a'.repeat(64),
    },
    stdout: 'pipe',
    stderr: 'inherit',
  });
  const reader = rust.stdout.pipeThrough(new TextDecoderStream()).getReader();
  try {
    let stdout = '';
    for (;;) {
      const { value, done } = await reader.read();
      if (done) throw new Error('Rust public listener failed to start.');
      stdout += value;
      const address = /aac-http public listening on (127\.0\.0\.1:\d+)/.exec(stdout)?.[1];
      if (address) return { rust, origin: 'http://' + address };
    }
  } catch (error) {
    rust.kill('SIGTERM');
    await rust.exited;
    throw error;
  } finally {
    reader.releaseLock();
  }
}

async function signInRust(origin: string): Promise<string> {
  const login = await fetch(origin + '/api/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin },
    body: JSON.stringify({ username: USERNAME, password: PASSWORD }),
  });
  expect(login.status).toBe(200);
  const cookie = login.headers.get('set-cookie')?.split(';')[0] ?? '';
  expect(cookie).toStartWith('connect.sid=');
  return cookie;
}

it.skipIf(!rustPublicBinary)(
  'refuses TS rollback while the Rust listener holds a signed-in session',
  async () => {
    await writeConfig('configured');
    const { rust, origin } = await startRustAuth();
    try {
      const cookie = await signInRust(origin);
      const rollback = startServer({
        port: 0,
        host: '127.0.0.1',
        staticDir: path.join(home, 'ui'),
        rustService: 'off',
      }).then((started) => {
        instance = started;
      });
      await expect(rollback).rejects.toThrow('Another dashboard holds the saved sign-ins.');

      const logout = await fetch(origin + '/api/auth/logout', {
        method: 'POST',
        headers: { 'content-type': 'application/json', origin, cookie },
        body: '{}',
      });
      expect(logout.status).toBe(200);
      const persisted = fs.readFileSync(path.join(home, '.ccs', 'auth', 'sessions.json'), 'utf8');
      expect(JSON.parse(persisted).sessions).toEqual([]);
    } finally {
      rust.kill('SIGTERM');
      await rust.exited;
    }
  }
);

it.skipIf(!rustPublicBinary || !legacyDashboard)(
  'keeps a Rust sign-out after the deployed pre-cut listener starts at hour one',
  async () => {
    await writeConfig('configured');
    const ownerFile = path.join(home, '.ccs', 'auth', 'sessions.owner');
    const sessionsFile = path.join(home, '.ccs', 'auth', 'sessions.json');
    const legacyStore = path.join(
      legacyDashboard!,
      'src/web-server/middleware/dashboard-session-store.ts'
    );
    const source = fs.readFileSync(legacyStore, 'utf8');
    // Reuse the deployed predicate and its helpers. The legacy listener below also
    // runs this source as part of the entire previous checkout.
    const predicate = source.match(
      /private async (claimIsStale\(held: string\): Promise<boolean> \{[\s\S]*?\n  \})/
    )?.[1];
    const bootMinute = source.match(/function bootMinute\(\)[\s\S]*?\n\}/)?.[0];
    const pidAlive = source.match(/function pidAlive\(pid: number\)[\s\S]*?\n\}/)?.[0];
    const constants = source.match(/const OWNER_BEAT_MS = .*;\nconst OWNER_STALE_MS = .*;/)?.[0];
    expect([predicate, bootMinute, pidAlive, constants].every(Boolean)).toBe(true);
    const js = new Bun.Transpiler({ loader: 'ts' }).transformSync(
      `${constants}
${bootMinute}
${pidAlive}
async function ${predicate}`
    );
    const claimIsStale = new Function('fs', 'os', js + '\nreturn claimIsStale;')(
      fs.promises,
      os
    ).bind({ ownerFile }) as (held: string) => Promise<boolean>;
    const { startServer: startLegacy } = await import(
      path.join(legacyDashboard!, 'src/web-server/index.ts')
    );
    const { closeDashboardSessions: closeLegacy } = await import(
      path.join(legacyDashboard!, 'src/web-server/middleware/auth-middleware.ts')
    );
    const legacyAnalytics = await import(
      path.join(legacyDashboard!, 'src/web-server/agent-usage/usage-sample-events.ts')
    );
    stubs.push(
      spyOn(legacyAnalytics, 'startNativeUsageSamples').mockImplementation(() => () => {})
    );
    const options = {
      port: 0,
      host: '127.0.0.1',
      staticDir: path.join(home, 'ui'),
      rustService: 'off' as const,
    };
    const { rust, origin } = await startRustAuth();
    let legacy: typeof instance;
    let stopped = false;
    try {
      const cookie = await signInRust(origin);
      const held = fs.readFileSync(ownerFile, 'utf8');
      expect(await claimIsStale(held)).toBe(false);
      // Simulate an owner that started 59 minutes ago. Keep it fresh for Rust,
      // then advance the rollback listener by two more minutes past that start.
      const birth = new Date(Date.now() - 59 * 60_000);
      fs.utimesSync(ownerFile, birth, birth);
      const clockOffset = 2 * 60_000;
      const realNow = Date.now.bind(Date);
      const realUptime = os.uptime.bind(os);
      stubs.push(
        spyOn(Date, 'now').mockImplementation(() => realNow() + clockOffset),
        spyOn(os, 'uptime').mockImplementation(() => realUptime() + clockOffset / 1000)
      );
      expect(await claimIsStale(held)).toBe(true);

      const deadline = realNow() + 18_000;
      while (fs.statSync(ownerFile).mtimeMs <= birth.getTime() && realNow() < deadline)
        await Bun.sleep(100);
      const staleAtStartup = await claimIsStale(held);
      legacy = await startLegacy(options);
      const legacyAddress = legacy.server.address() as AddressInfo;
      const legacySession = await fetch(`http://127.0.0.1:${legacyAddress.port}/api/auth/check`, {
        headers: { cookie },
      });
      const overlapped = (await legacySession.json()).authenticated;
      const logout = await fetch(origin + '/api/auth/logout', {
        method: 'POST',
        headers: { 'content-type': 'application/json', origin, cookie },
        body: '{}',
      });
      expect(logout.status).toBe(200);
      expect(JSON.parse(fs.readFileSync(sessionsFile, 'utf8')).sessions).toEqual([]);
      rust.kill('SIGTERM');
      await rust.exited;
      stopped = true;
      // The previous writer's close saves anything it loaded during the overlap.
      await closeLegacy();
      legacy.cleanup();
      legacy.server.closeAllConnections();
      await new Promise<void>((resolve) => legacy!.server.close(() => resolve()));
      legacy = undefined;
      const restored = JSON.parse(fs.readFileSync(sessionsFile, 'utf8')).sessions;

      instance = await startServer(options);
      const address = instance.server.address() as AddressInfo;
      const after = await fetch(`http://127.0.0.1:${address.port}/api/auth/check`, {
        headers: { cookie },
      });
      expect({
        staleAtStartup,
        overlapped,
        restored: restored.length,
        after: (await after.json()).authenticated,
      }).toEqual({ staleAtStartup: false, overlapped: false, restored: 0, after: false });
    } finally {
      if (!stopped) {
        rust.kill('SIGTERM');
        await rust.exited;
      }
      if (legacy) {
        await closeLegacy();
        legacy.cleanup();
        legacy.server.closeAllConnections();
        await new Promise<void>((resolve) => legacy!.server.close(() => resolve()));
      }
    }
  },
  30_000
);

it.skipIf(!rustPublicBinary)(
  'refuses admission and shutdown persistence after losing its claim',
  async () => {
    await writeConfig('configured');
    const { rust, origin } = await startRustAuth();
    const ownerFile = path.join(home, '.ccs', 'auth', 'sessions.owner');
    const sessionsFile = path.join(home, '.ccs', 'auth', 'sessions.json');
    let admission = 0;
    try {
      const cookie = await signInRust(origin);
      fs.renameSync(ownerFile, ownerFile + '.previous');
      fs.writeFileSync(ownerFile, 'new owner', { mode: 0o600 });
      fs.writeFileSync(sessionsFile, '{"version":1,"sessions":[]}\n');
      admission = (await fetch(origin + '/api/auth/session', { headers: { cookie } })).status;
    } finally {
      rust.kill('SIGTERM');
      await rust.exited;
    }
    expect(JSON.parse(fs.readFileSync(sessionsFile, 'utf8')).sessions).toEqual([]);
    expect(fs.readFileSync(ownerFile, 'utf8')).toBe('new owner');
    expect(admission).toBe(500);
  }
);

it.skipIf(!rustPublicBinary || process.platform !== 'linux')(
  'reclaims an interrupted session takeover when its kernel lease is free',
  async () => {
    await writeConfig('configured');
    const { rust } = await startRustAuth();
    rust.kill('SIGKILL');
    await rust.exited;
    const auth = path.join(home, '.ccs', 'auth');
    const gate = path.join(auth, 'sessions.owner.lock');
    const takeover = path.join(auth, 'sessions.owner.takeover.lock');
    fs.writeFileSync(takeover, '', { mode: 0o600 });
    const leaseHolder = Bun.spawn(
      ['flock', '-x', '-F', takeover, 'sh', '-c', 'echo locked; exec sleep 60'],
      {
        stdout: 'pipe',
      }
    );
    const reader = leaseHolder.stdout.getReader();
    try {
      expect(new TextDecoder().decode((await reader.read()).value)).toContain('locked');
      fs.mkdirSync(gate, { mode: 0o1700 });
      await expect(startRustAuth()).rejects.toThrow('Rust public listener failed to start.');
    } finally {
      reader.releaseLock();
      leaseHolder.kill('SIGKILL');
      await leaseHolder.exited;
    }
    const recovered = await startRustAuth();
    recovered.rust.kill('SIGTERM');
    await recovered.rust.exited;
    expect(fs.existsSync(gate)).toBe(false);
  }
);

it.skipIf(!rustPublicBinary || process.platform !== 'linux')(
  'reclaims only an old empty legacy session takeover directory',
  async () => {
    await writeConfig('configured');
    const { rust } = await startRustAuth();
    rust.kill('SIGKILL');
    await rust.exited;
    const gate = path.join(home, '.ccs', 'auth', 'sessions.owner.lock');
    fs.mkdirSync(gate);
    await expect(startRustAuth()).rejects.toThrow('Rust public listener failed to start.');
    const old = new Date(Date.now() - 61_000);
    fs.writeFileSync(path.join(gate, 'occupied'), '');
    fs.utimesSync(gate, old, old);
    await expect(startRustAuth()).rejects.toThrow('Rust public listener failed to start.');
    fs.unlinkSync(path.join(gate, 'occupied'));
    fs.utimesSync(gate, old, old);
    const recovered = await startRustAuth();
    recovered.rust.kill('SIGTERM');
    await recovered.rust.exited;
    expect(fs.existsSync(gate)).toBe(false);
  }
);

it.skipIf(!rustPublicBinary || process.platform !== 'linux')(
  'saves settings after the legacy empty marker loses its last writer',
  async () => {
    await writeConfig('configured');
    const { rust, origin } = await startRustAuth();
    const marker = path.join(home, '.ccs', 'config.yaml.lock');
    try {
      const cookie = await signInRust(origin);
      const save = () =>
        fetch(origin + '/api/auth/session-lifetime', {
          method: 'PUT',
          headers: { 'content-type': 'application/json', origin, cookie },
          body: '{"days":30}',
        });
      const legacyFd = fs.openSync(marker, 'wx', 0o600);
      try {
        expect((await save()).status).toBe(500);
        const old = new Date(Date.now() - 6_000);
        fs.futimesSync(legacyFd, old, old);
        expect((await save()).status).toBe(500);
        expect(fs.statSync(marker).size).toBe(0);
      } finally {
        fs.closeSync(legacyFd);
      }
      const saved = await save();
      expect(saved.status).toBe(200);
      expect((await saved.json()).days).toBe(30);
      expect(fs.existsSync(marker)).toBe(false);
    } finally {
      rust.kill('SIGTERM');
      await rust.exited;
    }
  }
);

describe('in-process HTTPS listener (dashboard_tls.https_listener)', () => {
  it('serves the same app over TLS, where secureTransport is true', async () => {
    const files = makeCertificate(0o600);
    if (!files) return; // openssl unavailable: nothing to check here.
    const port = await freePort();
    await writeConfig('configured', {
      https_listener: { enabled: true, port, cert_path: files.cert, key_path: files.key },
    });
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    expect(instance.httpsAddress).toMatchObject({ address: '127.0.0.1', port });
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
    expect(instance.httpsAddress).toBeNull();
    const plain = (instance.server.address() as AddressInfo).port;
    const check = await fetch(`http://127.0.0.1:${plain}/api/auth/check`);
    expect(check.status).toBe(200);
  });

  it('is off by default', async () => {
    await writeConfig('configured');
    instance = await startServer({ port: 0, host: '127.0.0.1', staticDir: path.join(home, 'ui') });
    expect(instance.httpsServer).toBeNull();
    expect(instance.httpsAddress).toBeNull();
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
    expect(await other.json()).toEqual({
      error: 'Invalid JSON in request body',
      code: 'invalid_json',
    });
    const big = await fetch(`${base}/api/health`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ padding: 'x'.repeat(200 * 1024) }),
    });
    expect(big.status).toBe(413);
    expect(await big.json()).toEqual({
      error: 'Request body is too large.',
      code: 'body_too_large',
    });

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
