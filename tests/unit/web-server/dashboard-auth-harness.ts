/**
 * Shared fixture for the dashboard sign-in and paired-device tests
 * (CONTRACT-auth-devices section 12). It builds the startServer() middleware
 * order (body parser, request log, session, global guard, /api router with its
 * own guard, JSON 404) on a loopback port, in a temporary CCS_HOME, with a
 * controllable peer: remote address, Host header and `socket.encrypted`.
 * Nothing reads or writes the real ~/.ccs; bcrypt runs at cost 4.
 */
import bcrypt from 'bcrypt';
import express from 'express';
import * as fs from 'fs';
import type { Server } from 'http';
import type { AddressInfo } from 'net';
import * as os from 'os';
import * as path from 'path';

import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import { invalidateConfigCache } from '../../../src/config/config-loader-facade';
import {
  authMiddleware,
  createSessionMiddleware,
  isApiRequestPath,
  loginRateLimiter,
  resetLoginRateLimitForTests,
} from '../../../src/web-server/middleware/auth-middleware';
import { requestLoggingMiddleware } from '../../../src/web-server/middleware/request-logging-middleware';
import {
  setLocalNetworkTrustResolver,
  setTrustedProxyAddressesResolver,
  setTrustedProxyResolver,
} from '../../../src/web-server/middleware/secure-transport';
import { setThisComputerAddressesForTests } from '../../../src/web-server/services/dashboard-tls-config';
import { configureDashboardTransport } from '../../../src/web-server/dashboard-auth-runtime';
import { apiRoutes } from '../../../src/web-server/routes';
import { resetAuthRateLimitsForTests } from '../../../src/web-server/routes/auth-rate-limits';
import { resetDashboardAuthStateForTests } from '../../../src/web-server/services/dashboard-auth-state';
import {
  setAuthClockForTests,
  setPasswordHashCostForTests,
  settleAuthWrites,
} from '../../../src/web-server/services/dashboard-auth-files';
import { resetDeviceStoreForTests } from '../../../src/web-server/services/dashboard-device-store';
import { resetSetupCodeForTests } from '../../../src/web-server/services/dashboard-setup-code';

export const USERNAME = 'aac-test-admin';
export const PASSWORD = 'first-fixture-password';
export const LAN_PEER = '192.0.2.21';
export const LAN_HOST = '192.0.2.10:3000';
/** The dashboard computer's own addresses as the proxy-address check sees them in the fixture. */
export const THIS_COMPUTER = ['127.0.0.1', '::1', '192.168.1.10'];

const FIXTURE_ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
  'CCS_SESSION_SECRET',
] as const;

export interface Peer {
  address: string;
  /** Overrides the Host header the server sees; null keeps the real one (127.0.0.1:port). */
  host: string | null;
  encrypted: boolean;
  /** Extra request headers set server-side (e.g. X-Forwarded-Proto). */
  headers: Record<string, string>;
}

export type AuthMode = 'config' | 'env' | 'setup' | 'off';

export interface HarnessOptions {
  mode?: AuthMode;
  /** Routers mounted in front of /api (e.g. a fixture Antigravity router). */
  before?: (app: express.Express) => void;
  dashboardTls?: Record<string, unknown>;
  /** `dashboard_network` in the fixture config.yaml (rule 4, off by default). */
  dashboardNetwork?: Record<string, unknown>;
  logging?: boolean;
}

export interface Harness {
  baseUrl: string;
  home: string;
  ccsDir: string;
  peer: Peer;
  /** The Origin a browser on the current peer's Host would send. */
  origin(): string;
  close(): Promise<void>;
}

let original: Record<string, string | undefined> = {};

export async function hashPassword(password: string): Promise<string> {
  return bcrypt.hash(password, 4);
}

export function configYaml(harness: Harness): string {
  return path.join(harness.ccsDir, 'config.yaml');
}

export async function startAuthHarness(options: HarnessOptions = {}): Promise<Harness> {
  const mode = options.mode ?? 'config';
  original = Object.fromEntries(FIXTURE_ENVIRONMENT.map((name) => [name, process.env[name]]));
  for (const name of FIXTURE_ENVIRONMENT) delete process.env[name];
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-auth-devices-'));
  const ccsDir = path.join(home, '.ccs');
  process.env.CCS_HOME = home;
  process.env.CCS_DIR = ccsDir;
  process.env.CODEX_HOME = path.join(home, '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(home, '.claude');
  process.env.CCS_SESSION_SECRET = 'isolated-auth-devices-session-secret';
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  resetDashboardAuthStateForTests();
  resetDeviceStoreForTests();
  resetSetupCodeForTests();
  setAuthClockForTests(null);
  setPasswordHashCostForTests(4);
  setThisComputerAddressesForTests(THIS_COMPUTER);
  invalidateConfigCache();

  const config = createEmptyUnifiedConfig();
  if (options.logging) {
    config.logging = { ...config.logging, enabled: true, level: 'debug', redact: false };
  }
  if (options.dashboardTls) {
    (config as unknown as Record<string, unknown>).dashboard_tls = options.dashboardTls;
  }
  if (options.dashboardNetwork) {
    (config as unknown as Record<string, unknown>).dashboard_network = options.dashboardNetwork;
  }
  const hash = await hashPassword(PASSWORD);
  if (mode === 'config') {
    config.dashboard_auth = {
      enabled: true,
      username: USERNAME,
      password_hash: hash,
      session_timeout_hours: 24,
    };
  } else if (mode === 'setup') {
    config.dashboard_auth = {
      enabled: true,
      username: '',
      password_hash: '',
      session_timeout_hours: 12,
    };
  } else if (mode === 'env') {
    config.dashboard_auth = {
      enabled: true,
      username: 'file-user',
      password_hash: hash,
      session_timeout_hours: 24,
    };
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    process.env.CCS_DASHBOARD_USERNAME = USERNAME;
    process.env.CCS_DASHBOARD_PASSWORD_HASH = hash;
  }
  saveUnifiedConfig(config);
  invalidateConfigCache();

  const peer: Peer = { address: '127.0.0.1', host: null, encrypted: false, headers: {} };
  const app = express();
  app.set('case sensitive routing', true);
  configureDashboardTransport(app);
  app.use((req, _res, next) => {
    Object.defineProperty(req.socket, 'remoteAddress', {
      value: peer.address,
      configurable: true,
    });
    Object.defineProperty(req.socket, 'encrypted', { value: peer.encrypted, configurable: true });
    if (peer.host !== null) req.headers.host = peer.host;
    for (const [name, value] of Object.entries(peer.headers)) req.headers[name] = value;
    next();
  });
  app.use(express.json());
  app.use(requestLoggingMiddleware);
  app.use(createSessionMiddleware());
  app.use(authMiddleware);
  options.before?.(app);
  app.use('/api', apiRoutes);
  app.use((req, res, next) => {
    if (!isApiRequestPath(req.path)) return next();
    res.status(404).json({ error: 'API endpoint was not found.' });
  });

  const server = await new Promise<Server>((resolve, reject) => {
    const listener = app.listen(0, '127.0.0.1');
    listener.once('error', reject);
    listener.once('listening', () => resolve(listener));
  });
  const baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  for (const key of [peer.address, `::ffff:${peer.address}`, LAN_PEER, '::1']) {
    loginRateLimiter.resetKey(key);
  }
  await resetLoginRateLimitForTests();
  await resetAuthRateLimitsForTests();

  return {
    baseUrl,
    home,
    ccsDir,
    peer,
    origin: () => (peer.host ? `http://${peer.host}` : baseUrl),
    async close() {
      // Background last-seen writes land before the fixture folder goes away.
      await settleAuthWrites();
      await new Promise<void>((resolve) => {
        server.close(() => resolve());
        server.closeAllConnections?.();
      });
      for (const key of ['127.0.0.1', '::ffff:127.0.0.1', LAN_PEER, '::1']) {
        loginRateLimiter.resetKey(key);
      }
      await resetLoginRateLimitForTests();
      await resetAuthRateLimitsForTests();
      await settleAuthWrites();
      setAuthClockForTests(null);
      setPasswordHashCostForTests(null);
      setTrustedProxyResolver(() => null);
      setTrustedProxyAddressesResolver(null);
      setLocalNetworkTrustResolver(null);
      setThisComputerAddressesForTests(null);
      resetDashboardAuthStateForTests();
      resetDeviceStoreForTests();
      resetSetupCodeForTests();
      for (const name of FIXTURE_ENVIRONMENT) {
        const value = original[name];
        if (value === undefined) delete process.env[name];
        else process.env[name] = value;
      }
      invalidateConfigCache();
      fs.rmSync(home, { recursive: true, force: true });
    },
  };
}

/** A browser or tray: keeps its own cookie and sends JSON. */
export class Client {
  cookie = '';
  constructor(
    private readonly harness: Harness,
    private readonly bearer: string | null = null
  ) {}

  withBearer(token: string | null): Client {
    const client = new Client(this.harness, token);
    client.cookie = this.cookie;
    return client;
  }

  async send(
    method: string,
    route: string,
    body?: unknown,
    headers: Record<string, string> = {}
  ): Promise<{ status: number; body: Record<string, unknown>; headers: Headers; text: string }> {
    const requestHeaders: Record<string, string> = { ...headers };
    if (body !== undefined && !('content-type' in requestHeaders)) {
      requestHeaders['content-type'] = 'application/json';
    }
    if (method !== 'GET' && !('origin' in requestHeaders)) {
      requestHeaders.origin = this.harness.origin();
    }
    if (requestHeaders.origin === '') delete requestHeaders.origin;
    if (this.cookie) requestHeaders.cookie = this.cookie;
    if (this.bearer !== null) requestHeaders.authorization = `Bearer ${this.bearer}`;
    const response = await fetch(`${this.harness.baseUrl}${route}`, {
      method,
      headers: requestHeaders,
      ...(body === undefined
        ? {}
        : { body: typeof body === 'string' ? body : JSON.stringify(body) }),
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

  async login(password = PASSWORD, username = USERNAME) {
    return this.send('POST', '/api/auth/login', { username, password });
  }
}

export async function pairTray(
  harness: Harness,
  overrides: Record<string, unknown> = {}
): Promise<{ status: number; body: Record<string, unknown> }> {
  return new Client(harness).send(
    'POST',
    '/api/auth/devices/pair',
    {
      username: USERNAME,
      password: PASSWORD,
      deviceName: 'fixture-mac',
      platform: 'mac',
      ...overrides,
    },
    { origin: '' }
  );
}

export function readDevicesFile(harness: Harness): { mode: number; text: string } {
  const file = path.join(harness.ccsDir, 'auth', 'devices.json');
  return { mode: fs.statSync(file).mode & 0o777, text: fs.readFileSync(file, 'utf8') };
}
