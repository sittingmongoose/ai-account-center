import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'bun:test';
import bcrypt from 'bcrypt';
import express from 'express';
import type { Server } from 'http';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { apiRoutes } from '../../../src/web-server/routes';
import {
  authMiddleware,
  createSessionMiddleware,
} from '../../../src/web-server/middleware/auth-middleware';

describe('api-routes remote write guard', () => {
  let server: Server;
  let baseUrl = '';
  let forcedRemoteAddress = '127.0.0.1';
  let tempHome = '';
  let localSettingsSession = false;
  let originalDashboardAuthEnabled: string | undefined;
  let originalCcsHome: string | undefined;
  let originalCcsDir: string | undefined;
  let originalCodexHome: string | undefined;
  let originalDashboardUsername: string | undefined;
  let originalDashboardPasswordHash: string | undefined;
  let originalSessionSecret: string | undefined;

  beforeAll(async () => {
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      Object.defineProperty(req.socket, 'remoteAddress', {
        value: forcedRemoteAddress,
        configurable: true,
      });
      if (localSettingsSession) {
        Object.assign(req, { session: { authenticated: true } });
      }
      next();
    });
    app.use('/api', apiRoutes);

    await new Promise<void>((resolve, reject) => {
      server = app.listen(0, '127.0.0.1');
      server.once('error', reject);
      server.once('listening', () => resolve());
    });

    const address = server.address();
    if (!address || typeof address === 'string') {
      throw new Error('Unable to resolve test server port');
    }
    baseUrl = `http://127.0.0.1:${address.port}`;
  });

  afterAll(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  beforeEach(() => {
    originalDashboardAuthEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;
    originalCcsHome = process.env.CCS_HOME;
    originalCcsDir = process.env.CCS_DIR;
    originalCodexHome = process.env.CODEX_HOME;
    originalDashboardUsername = process.env.CCS_DASHBOARD_USERNAME;
    originalDashboardPasswordHash = process.env.CCS_DASHBOARD_PASSWORD_HASH;
    originalSessionSecret = process.env.CCS_SESSION_SECRET;
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-api-routes-remote-write-guard-'));
    process.env.CCS_HOME = tempHome;
    process.env.CCS_DIR = path.join(tempHome, '.ccs');
    process.env.CODEX_HOME = path.join(tempHome, '.codex');
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    process.env.CCS_SESSION_SECRET = 'isolated-api-write-guard-session-secret';
    localSettingsSession = false;
    forcedRemoteAddress = '10.10.0.24';
  });

  afterEach(() => {
    if (originalDashboardAuthEnabled !== undefined) {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = originalDashboardAuthEnabled;
    } else {
      delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
    }

    if (originalCcsHome !== undefined) {
      process.env.CCS_HOME = originalCcsHome;
    } else {
      delete process.env.CCS_HOME;
    }

    if (originalCodexHome !== undefined) {
      process.env.CODEX_HOME = originalCodexHome;
    } else {
      delete process.env.CODEX_HOME;
    }

    for (const [name, value] of [
      ['CCS_DIR', originalCcsDir],
      ['CCS_DASHBOARD_USERNAME', originalDashboardUsername],
      ['CCS_DASHBOARD_PASSWORD_HASH', originalDashboardPasswordHash],
      ['CCS_SESSION_SECRET', originalSessionSecret],
    ] as const) {
      if (value !== undefined) process.env[name] = value;
      else delete process.env[name];
    }

    if (tempHome && fs.existsSync(tempHome)) {
      fs.rmSync(tempHome, { recursive: true, force: true });
      tempHome = '';
    }
  });

  it('allows remote read-only GET requests when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/health`);

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ status: 'ok' });
  });

  it('returns 404 for retired remote Codex config and diagnostics reads', async () => {
    for (const route of ['/config/raw', '/diagnostics']) {
      const response = await fetch(`${baseUrl}/api/codex${route}`);
      expect(response.status).toBe(404);
    }
  });

  it('protects remote saved Codex profile identity when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/codex/profiles`);
    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error:
        'Codex auth profiles endpoint requires localhost access when dashboard auth is disabled.',
    });
  });

  it('blocks remote Codex activation before its handler when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/codex/profiles/guard-test/activate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
  });

  it('blocks remote settings writes even with the dashboard origin when auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Origin: baseUrl },
      body: JSON.stringify({ refreshIntervalSeconds: 120 }),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
  });

  it('blocks remote PUT requests when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ refreshIntervalSeconds: 120 }),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
  });

  it('rejects invalid local refresh intervals at the account settings API boundary', async () => {
    forcedRemoteAddress = '127.0.0.1';
    localSettingsSession = true;

    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Origin: baseUrl },
      body: JSON.stringify({ refreshIntervalSeconds: 29 }),
    });

    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({
      error: 'Choose a whole number of seconds from 30 to 3600.',
    });
  });

  it('rejects cross-origin account settings writes from a local client', async () => {
    forcedRemoteAddress = '127.0.0.1';
    localSettingsSession = true;

    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Origin: 'https://untrusted.example' },
      body: JSON.stringify({ refreshIntervalSeconds: 120 }),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
    expect(fs.existsSync(path.join(tempHome, '.ccs', 'account-refresh-settings.json'))).toBe(false);
  });

  it('rejects account settings writes with a non-loopback Host even from a local client', async () => {
    forcedRemoteAddress = '127.0.0.1';
    localSettingsSession = true;

    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Host: 'untrusted.example' },
      body: JSON.stringify({ refreshIntervalSeconds: 120 }),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
    expect(fs.existsSync(path.join(tempHome, '.ccs', 'account-refresh-settings.json'))).toBe(false);
  });

  it('blocks remote PATCH requests when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
  });

  it('blocks remote DELETE requests before route dispatch when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/accounts/settings`, {
      method: 'DELETE',
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      error: 'Remote dashboard writes require localhost access when dashboard auth is disabled.',
    });
  });

  it('allows remote writes again when dashboard auth is enabled', async () => {
    const password = 'testpassword123';
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    process.env.CCS_DASHBOARD_USERNAME = 'admin';
    process.env.CCS_DASHBOARD_PASSWORD_HASH = await bcrypt.hash(password, 4);

    const authApp = express();
    authApp.use(express.json());
    authApp.use((req, _res, next) => {
      Object.defineProperty(req.socket, 'remoteAddress', {
        value: forcedRemoteAddress,
        configurable: true,
      });
      next();
    });
    authApp.use(createSessionMiddleware());
    authApp.use(authMiddleware);
    authApp.use('/api', apiRoutes);

    const authServer = await new Promise<Server>((resolve, reject) => {
      const instance = authApp.listen(0, '127.0.0.1');
      instance.once('error', reject);
      instance.once('listening', () => resolve(instance));
    });

    const address = authServer.address();
    if (!address || typeof address === 'string') {
      throw new Error('Unable to resolve auth-enabled test server port');
    }
    const authBaseUrl = `http://127.0.0.1:${address.port}`;

    try {
      const anonymousResponse = await fetch(`${authBaseUrl}/api/accounts/settings`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json', Origin: authBaseUrl },
        body: JSON.stringify({ refreshIntervalSeconds: 120 }),
      });
      expect(anonymousResponse.status).toBe(401);
      expect(await anonymousResponse.json()).toEqual({ error: 'Authentication required' });

      const loginResponse = await fetch(`${authBaseUrl}/api/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          username: 'admin',
          password,
        }),
      });
      const cookie = loginResponse.headers.get('set-cookie');

      expect(loginResponse.status).toBe(200);
      expect(cookie).toBeTruthy();

      const missingOriginResponse = await fetch(`${authBaseUrl}/api/accounts/settings`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json', Cookie: cookie as string },
        body: JSON.stringify({ refreshIntervalSeconds: 120 }),
      });
      expect(missingOriginResponse.status).toBe(403);
      expect(await missingOriginResponse.json()).toEqual({
        error: 'Settings require the dashboard origin.',
      });

      const settingsResponse = await fetch(`${authBaseUrl}/api/accounts/settings`, {
        method: 'PUT',
        headers: {
          'Content-Type': 'application/json',
          Cookie: cookie as string,
          Origin: authBaseUrl,
        },
        body: JSON.stringify({ refreshIntervalSeconds: 120 }),
      });
      expect(settingsResponse.status).toBe(200);
      expect(await settingsResponse.json()).toEqual({ refreshIntervalSeconds: 120 });
      expect(settingsResponse.headers.get('cache-control')).toBe('no-store');
      const savedSettings = await fetch(`${authBaseUrl}/api/accounts/settings`, {
        headers: { Cookie: cookie as string },
      });
      expect(savedSettings.status).toBe(200);
      expect(await savedSettings.json()).toEqual({ refreshIntervalSeconds: 120 });
    } finally {
      await new Promise<void>((resolve) => authServer.close(() => resolve()));
    }
  }, 15000);
});
