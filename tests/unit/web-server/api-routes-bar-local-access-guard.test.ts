/**
 * Security gate for the provider-free native Bar process-identity probe.
 * Remote callers require dashboard authentication; local probes use a nonce
 * proof without reading account data or starting provider collection.
 *
 * The gate lives in the top-level apiRoutes middleware (one choke point), so we
 * exercise it by mounting the real apiRoutes and toggling auth via env, mirroring
 * api-routes-remote-write-guard.test.ts.
 */

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
import {
  BAR_AUTH_NONCE_HEADER,
  BAR_AUTH_TOKEN_HEADER,
  getBarAuthTokenPath,
  getOrCreateBarAuthToken,
  isMatchingBarAuthProof,
} from '../../../src/utils/bar-auth-token';

const BAR_LOCAL_ACCESS_ERROR =
  'CCS Bar endpoints require localhost access when dashboard auth is disabled.';

describe('api-routes /api/bar/* local-access guard', () => {
  let server: Server;
  let baseUrl = '';
  let forcedRemoteAddress = '127.0.0.1';
  let tempHome = '';
  let originalDashboardAuthEnabled: string | undefined;
  let originalCcsHome: string | undefined;
  let originalCcsDir: string | undefined;
  let originalCodexHome: string | undefined;

  beforeAll(async () => {
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      Object.defineProperty(req.socket, 'remoteAddress', {
        value: forcedRemoteAddress,
        configurable: true,
      });
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
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-api-routes-bar-guard-'));
    process.env.CCS_HOME = tempHome;
    delete process.env.CCS_DIR;
    process.env.CODEX_HOME = path.join(tempHome, '.codex');
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    forcedRemoteAddress = '192.168.2.50';
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
    if (originalCcsDir !== undefined) process.env.CCS_DIR = originalCcsDir;
    else delete process.env.CCS_DIR;

    if (tempHome && fs.existsSync(tempHome)) {
      fs.rmSync(tempHome, { recursive: true, force: true });
      tempHome = '';
    }
  });

  it('rejects a non-loopback GET /api/bar/auth when dashboard auth is disabled', async () => {
    const response = await fetch(`${baseUrl}/api/bar/auth`);

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ error: BAR_LOCAL_ACCESS_ERROR });
  });

  it('does not mint a proof for a refused remote probe', async () => {
    const response = await fetch(`${baseUrl}/api/bar/auth`, {
      headers: { [BAR_AUTH_NONCE_HEADER]: '1234567890abcdef1234567890abcdef' },
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ error: BAR_LOCAL_ACCESS_ERROR });
    expect(response.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
    expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);
  });

  it('returns only liveness for a loopback probe without a nonce', async () => {
    forcedRemoteAddress = '127.0.0.1';

    const response = await fetch(`${baseUrl}/api/bar/auth`, {
      headers: { Host: '127.0.0.1' },
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ status: 'ok' });
    expect(response.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
    expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);
  });

  it('binds an allowed loopback proof to its nonce without exposing the file token', async () => {
    forcedRemoteAddress = '127.0.0.1';
    const nonce = '1234567890abcdef1234567890abcdef';
    const response = await fetch(`${baseUrl}/api/bar/auth`, {
      headers: { [BAR_AUTH_NONCE_HEADER]: nonce },
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ status: 'ok' });
    const proof = response.headers.get(BAR_AUTH_TOKEN_HEADER);
    const token = getOrCreateBarAuthToken();
    expect(proof).not.toBe(token);
    expect(isMatchingBarAuthProof(token, nonce, proof || '')).toBe(true);
    expect(isMatchingBarAuthProof(token, 'abcdef1234567890abcdef1234567890', proof || '')).toBe(
      false
    );
  });

  it('rejects a cross-origin local probe before creating a proof', async () => {
    forcedRemoteAddress = '127.0.0.1';
    const response = await fetch(`${baseUrl}/api/bar/auth`, {
      headers: {
        Origin: 'https://unrelated.example',
        [BAR_AUTH_NONCE_HEADER]: '1234567890abcdef1234567890abcdef',
      },
    });
    expect(response.status).toBe(403);
    expect(response.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
    expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);
  });

  it('allows only an authenticated same-origin remote Bar identity probe', async () => {
    // With auth enabled the helper returns true regardless of peer address, so
    // an authenticated remote dashboard keeps working. We log in to get a session
    // cookie, then a remote (non-loopback) GET must pass.
    const password = 'testpassword123';
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    process.env.CCS_DASHBOARD_USERNAME = 'admin';
    process.env.CCS_DASHBOARD_PASSWORD_HASH = await bcrypt.hash(password, 4);

    const authApp = express();
    authApp.use(express.json());
    authApp.use((req, _res, next) => {
      Object.defineProperty(req.socket, 'remoteAddress', {
        value: '203.0.113.7',
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

    try {
      const address = authServer.address();
      if (!address || typeof address === 'string') {
        throw new Error('Unable to resolve auth-enabled test server port');
      }
      const authBaseUrl = `http://127.0.0.1:${address.port}`;

      const loginResponse = await fetch(`${authBaseUrl}/api/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: 'admin', password }),
      });
      const cookie = loginResponse.headers.get('set-cookie');
      expect(loginResponse.status).toBe(200);
      expect(cookie).toBeTruthy();

      const denied = await fetch(`${authBaseUrl}/api/bar/auth`);
      expect(denied.status).toBe(401);
      const crossOrigin = await fetch(`${authBaseUrl}/api/bar/auth`, {
        headers: {
          Cookie: cookie as string,
          Origin: 'https://unrelated.example',
          [BAR_AUTH_NONCE_HEADER]: '1234567890abcdef1234567890abcdef',
        },
      });
      expect(crossOrigin.status).toBe(403);
      expect(crossOrigin.headers.get(BAR_AUTH_TOKEN_HEADER)).toBeNull();
      expect(fs.existsSync(getBarAuthTokenPath())).toBe(false);
      const response = await fetch(`${authBaseUrl}/api/bar/auth`, {
        headers: { Cookie: cookie as string },
      });

      // Not the gate's 403 — auth-enabled bypasses the localhost requirement.
      expect(response.status).toBe(200);
      expect(await response.json()).toEqual({ status: 'ok' });
    } finally {
      await new Promise<void>((resolve) => authServer.close(() => resolve()));
      delete process.env.CCS_DASHBOARD_USERNAME;
      delete process.env.CCS_DASHBOARD_PASSWORD_HASH;
    }
  }, 15000);
});
