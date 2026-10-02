import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'bun:test';
import bcrypt from 'bcrypt';
import express from 'express';
import type { Server } from 'http';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { apiRoutes } from '../../../src/web-server/routes';
import {
  authMiddleware,
  createSessionMiddleware,
} from '../../../src/web-server/middleware/auth-middleware';
import {
  BAR_AUTH_NONCE_HEADER,
  BAR_AUTH_TOKEN_HEADER,
  getBarAuthTokenPath,
} from '../../../src/utils/bar-auth-token';

const RETIRED_READ_PATHS = [
  '/profiles',
  '/accounts',
  '/cliproxy',
  '/cliproxy/auth',
  '/cliproxy/auth/accounts',
  '/cliproxy/sync',
  '/cliproxy/catalog',
  '/cliproxy/ai-providers',
  '/cliproxy/openai-compat',
  '/cliproxy-server',
  '/channels',
  '/websearch',
  '/browser',
  '/image-analysis',
  '/copilot',
  '/copilot/status',
  '/cursor',
  '/cursor/status',
  '/legacy/cursor',
  '/legacy/cursor/status',
  '/droid',
  '/droid/diagnostics',
  '/codex/diagnostics',
  '/codex/config/raw',
  '/persist',
  '/persist/backups',
  '/claude-extension',
  '/claude-extension/profiles',
  '/file',
  '/files',
  '/global-env',
  '/globalenv',
  '/thinking',
  '/logs',
  '/logs/config',
  '/settings',
  '/settings/default',
  '/config',
  '/shared',
  '/overview',
  '/usage',
  '/bar/summary',
  '/bar/analytics',
] as const;

const FIXTURE_ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
  'CCS_SESSION_SECRET',
] as const;

describe('retained account dashboard API surface', () => {
  let server: Server | undefined;
  let baseUrl = '';
  let tempHome = '';
  let forcedRemoteAddress = '127.0.0.1';
  let originalEnvironment: Record<string, string | undefined> = {};
  const username = 'surface-test-admin';
  const password = 'isolated-surface-test-password';
  let passwordHash = '';

  beforeAll(async () => {
    passwordHash = await bcrypt.hash(password, 4);
  });

  beforeEach(async () => {
    originalEnvironment = Object.fromEntries(
      FIXTURE_ENVIRONMENT.map((name) => [name, process.env[name]])
    );
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-api-product-surface-'));
    process.env.CCS_HOME = tempHome;
    process.env.CCS_DIR = path.join(tempHome, '.ccs');
    process.env.CODEX_HOME = path.join(tempHome, '.codex');
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    process.env.CCS_DASHBOARD_USERNAME = username;
    process.env.CCS_DASHBOARD_PASSWORD_HASH = passwordHash;
    process.env.CCS_SESSION_SECRET = 'isolated-api-product-surface-session-secret';
    forcedRemoteAddress = '127.0.0.1';

    // Mount the real router and production authentication order without starting
    // the server's quota collectors, analytics sampler, or automatic switching.
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      Object.defineProperty(req.socket, 'remoteAddress', {
        value: forcedRemoteAddress,
        configurable: true,
      });
      next();
    });
    app.use(createSessionMiddleware());
    app.use(authMiddleware);
    app.use('/api', apiRoutes);
    server = await new Promise<Server>((resolve, reject) => {
      const listener = app.listen(0, '127.0.0.1');
      listener.once('error', reject);
      listener.once('listening', () => resolve(listener));
    });
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('Unable to resolve API test port');
    baseUrl = `http://127.0.0.1:${address.port}`;
  });

  afterEach(async () => {
    try {
      if (server) {
        await new Promise<void>((resolve) => server!.close(() => resolve()));
        server = undefined;
      }
    } finally {
      for (const name of FIXTURE_ENVIRONMENT) {
        const value = originalEnvironment[name];
        if (value !== undefined) process.env[name] = value;
        else delete process.env[name];
      }
      if (tempHome) fs.rmSync(tempHome, { recursive: true, force: true });
      tempHome = '';
    }
  });

  it.each(RETIRED_READ_PATHS)('returns 404 for retired GET /api%s', async (route) => {
    const response = await fetch(`${baseUrl}/api${route}`, { redirect: 'manual' });
    expect(response.status).toBe(404);
    expect(await response.text()).toContain(`Cannot GET /api${route}`);
  });

  it('returns process health without exposing the retired system health report', async () => {
    const response = await fetch(`${baseUrl}/api/health`);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ status: 'ok' });
  });

  it('keeps lightweight health public when remote dashboard authentication is enabled', async () => {
    forcedRemoteAddress = '198.51.100.42';
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    const response = await fetch(`${baseUrl}/api/health`);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ status: 'ok' });
  });

  it('does not expose the retired health repair action', async () => {
    const response = await fetch(`${baseUrl}/api/health/fix/retired-test-check`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
    });
    expect(response.status).toBe(404);
    expect(await response.text()).toContain('Cannot POST /api/health/fix/retired-test-check');
  });

  it('keeps account settings protected even when dashboard auth is disabled locally', async () => {
    const response = await fetch(`${baseUrl}/api/accounts/settings`);
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: 'Authentication required' });
    expect(response.headers.get('cache-control')).toBe('no-store');
  });

  it.each([
    '/accounts/dashboard',
    '/accounts/settings',
    '/accounts/analytics',
    '/accounts/visibility',
    '/app-updates/status',
    '/bar/auth',
  ])('protects remote GET /api%s before its handler runs', async (route) => {
    forcedRemoteAddress = '198.51.100.42';
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    const response = await fetch(`${baseUrl}/api${route}`, {
      headers: { [BAR_AUTH_NONCE_HEADER]: '1234567890abcdef1234567890abcdef' },
    });
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: 'Authentication required' });
    expect(response.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
    expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);
  });

  it('keeps remote Bar identity probes local-only when dashboard auth is disabled', async () => {
    forcedRemoteAddress = '198.51.100.42';
    const response = await fetch(`${baseUrl}/api/bar/auth`, {
      headers: { [BAR_AUTH_NONCE_HEADER]: '1234567890abcdef1234567890abcdef' },
    });
    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'CCS Bar endpoints require localhost access when dashboard auth is disabled.',
    });
    expect(response.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
    expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);
  });

  it.each([
    { method: 'PUT', route: '/codex/config/raw' },
    { method: 'PATCH', route: '/codex/config/patch' },
  ])(
    'returns 404 for retired local $method /api$route without changing native config',
    async ({ method, route }) => {
      const response = await fetch(`${baseUrl}/api${route}`, {
        method,
        headers: { 'Content-Type': 'application/json', Origin: baseUrl },
        body: JSON.stringify({ rawText: 'model = "fixture-only"', kind: 'features' }),
      });
      expect(response.status).toBe(404);
      expect(fs.existsSync(path.join(tempHome, '.codex', 'config.toml'))).toBe(false);
    }
  );

  it('serves retained account settings through a genuine authenticated remote session', async () => {
    forcedRemoteAddress = '198.51.100.42';
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    const login = await fetch(`${baseUrl}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    });
    expect(login.status).toBe(200);
    expect(await login.json()).toEqual({ success: true, username });
    const cookie = login.headers.get('set-cookie');
    expect(cookie).toBeTruthy();

    const retiredProfiles = await fetch(`${baseUrl}/api/profiles`, {
      headers: { Cookie: cookie as string },
    });
    expect(retiredProfiles.status).toBe(404);
    expect(await retiredProfiles.text()).toContain('Cannot GET /api/profiles');

    const retiredDefault = await fetch(`${baseUrl}/api/accounts/default`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Cookie: cookie as string,
        Origin: baseUrl,
      },
      body: JSON.stringify({ name: 'retired-test-account' }),
    });
    expect(retiredDefault.status).toBe(404);
    expect(await retiredDefault.text()).toContain('Cannot POST /api/accounts/default');

    for (const route of ['/bar/summary', '/bar/analytics']) {
      const retiredBar = await fetch(`${baseUrl}/api${route}`, {
        headers: {
          Cookie: cookie as string,
          [BAR_AUTH_NONCE_HEADER]: '1234567890abcdef1234567890abcdef',
        },
      });
      expect(retiredBar.status).toBe(404);
      expect(retiredBar.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
    }
    expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);

    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      headers: { Cookie: cookie as string },
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ refreshIntervalSeconds: 60 });
    expect(response.headers.get('cache-control')).toBe('no-store');

    const check = await fetch(`${baseUrl}/api/auth/check`, {
      headers: { Cookie: cookie as string },
    });
    expect(check.status).toBe(200);
    expect(await check.json()).toMatchObject({
      authEnabled: true,
      authConfigured: true,
      authenticated: true,
      isLocalAccess: false,
      username,
    });

    const logout = await fetch(`${baseUrl}/api/auth/logout`, {
      method: 'POST',
      headers: { Cookie: cookie as string },
    });
    expect(logout.status).toBe(200);
    expect(await logout.json()).toEqual({ success: true });
    const loggedOut = await fetch(`${baseUrl}/api/accounts/settings`, {
      headers: { Cookie: cookie as string },
    });
    expect(loggedOut.status).toBe(401);
    expect(await loggedOut.json()).toEqual({ error: 'Authentication required' });
  });
});
