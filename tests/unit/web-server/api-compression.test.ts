/**
 * /api response compression and the static-asset skip of session and request
 * logging. The middleware's edges run on a bare express app; the mounting, the
 * session skip and the WebSocket upgrade run through the real startServer()
 * stack with a temporary CCS_HOME and a stubbed UI folder.
 */
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import bcrypt from 'bcrypt';
import crypto from 'crypto';
import express from 'express';
import fs from 'fs';
import http from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import zlib from 'zlib';

import { startServer, type ServerInstance } from '../../../src/web-server';
import { apiCompression } from '../../../src/web-server/api-compression';
import { clearRecentLogEntries, getRecentLogEntries } from '../../../src/services/logging';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import { invalidateConfigCache } from '../../../src/config/config-loader-facade';
import { loginRateLimiter } from '../../../src/web-server/middleware/auth-middleware';
import { resetAuthRateLimitsForTests } from '../../../src/web-server/routes/auth-rate-limits';
import { resetDashboardAuthStateForTests } from '../../../src/web-server/services/dashboard-auth-state';
import {
  setAuthClockForTests,
  setPasswordHashCostForTests,
  settleAuthWrites,
} from '../../../src/web-server/services/dashboard-auth-files';
import { resetDeviceStoreForTests } from '../../../src/web-server/services/dashboard-device-store';
import { resetSetupCodeForTests } from '../../../src/web-server/services/dashboard-setup-code';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../../src/web-server/services/account-analytics-service';

interface RawResponse {
  status: number;
  headers: http.IncomingHttpHeaders;
  body: Buffer;
}

/** A raw request: no automatic decompression and no client-side path normalisation. */
function rawRequest(
  port: number,
  method: string,
  route: string,
  headers: Record<string, string> = {},
  body?: string | Buffer
): Promise<RawResponse> {
  return new Promise((resolve, reject) => {
    const request = http.request(
      { host: '127.0.0.1', port, method, path: route, headers, agent: false },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.on('end', () =>
          resolve({
            status: response.statusCode ?? 0,
            headers: response.headers,
            body: Buffer.concat(chunks),
          })
        );
        response.on('error', reject);
      }
    );
    request.on('error', reject);
    if (body !== undefined) request.write(body);
    request.end();
  });
}

const BIG = { text: 'account-window-'.repeat(200) };
const BIG_JSON = JSON.stringify(BIG);
const PREENCODED = zlib.gzipSync(Buffer.from(BIG_JSON));
const HUGE = Buffer.alloc(9 * 1024 * 1024, 0x61);

let unitServer: http.Server | undefined;
let unitPort = 0;

beforeAll(async () => {
  const app = express();
  app.disable('x-powered-by');
  app.use('/api', apiCompression());
  app.get('/api/big', (_req, res) => res.json(BIG));
  app.get('/api/small', (_req, res) => res.json({ ok: true }));
  app.get('/api/status', (_req, res) => res.status(400).json(BIG));
  app.get('/api/empty', (_req, res) => res.status(204).end());
  app.get('/api/text', (_req, res) => res.type('text/plain').send('x'.repeat(4096)));
  app.get('/api/preencoded', (_req, res) => {
    res.setHeader('Content-Type', 'application/json');
    res.setHeader('Content-Encoding', 'gzip');
    res.end(PREENCODED);
  });
  app.get('/api/huge', (_req, res) => {
    res.type('application/json');
    res.write(HUGE);
    res.end();
  });
  unitServer = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => unitServer?.once('listening', () => resolve()));
  unitPort = (unitServer.address() as AddressInfo).port;
});

afterAll(() => {
  unitServer?.close();
  unitServer?.closeAllConnections?.();
});

describe('the /api compression middleware', () => {
  it('encodes a big JSON answer per Accept-Encoding, decodable to the identical bytes', async () => {
    const identity = await rawRequest(unitPort, 'GET', '/api/big');
    expect(identity.status).toBe(200);
    expect(identity.headers['content-encoding']).toBeUndefined();
    expect(identity.body.toString()).toBe(BIG_JSON);
    expect(String(identity.headers.vary)).toContain('Accept-Encoding');

    const br = await rawRequest(unitPort, 'GET', '/api/big', { 'Accept-Encoding': 'br, gzip' });
    expect(br.status).toBe(200);
    expect(br.headers['content-encoding']).toBe('br');
    expect(br.headers['content-length']).toBeUndefined();
    expect(String(br.headers.vary)).toContain('Accept-Encoding');
    expect(br.body.length).toBeLessThan(identity.body.length);
    expect(zlib.brotliDecompressSync(br.body).toString()).toBe(BIG_JSON);

    const gzip = await rawRequest(unitPort, 'GET', '/api/big', {
      'Accept-Encoding': 'br;q=0, gzip',
    });
    expect(gzip.headers['content-encoding']).toBe('gzip');
    expect(zlib.gunzipSync(gzip.body).toString()).toBe(BIG_JSON);
  });

  it('stays identity when the client did not accept a coding', async () => {
    for (const headers of [
      {},
      { 'Accept-Encoding': 'identity' },
      { 'Accept-Encoding': 'br;q=0, gzip;q=0' },
      { 'Accept-Encoding': 'deflate' },
    ]) {
      const response = await rawRequest(unitPort, 'GET', '/api/big', headers);
      expect(response.status).toBe(200);
      expect(response.headers['content-encoding']).toBeUndefined();
      expect(response.body.toString()).toBe(BIG_JSON);
      // The size still varies with Accept-Encoding, so caches need the Vary.
      expect(String(response.headers.vary)).toContain('Accept-Encoding');
    }
  });

  it('leaves a small JSON answer completely untouched', async () => {
    const response = await rawRequest(unitPort, 'GET', '/api/small', {
      'Accept-Encoding': 'br, gzip',
    });
    expect(response.status).toBe(200);
    expect(response.headers['content-encoding']).toBeUndefined();
    expect(response.headers.vary).toBeUndefined();
    expect(response.body.toString()).toBe(JSON.stringify({ ok: true }));
    expect(Number(response.headers['content-length'])).toBe(response.body.length);
  });

  it('keeps res.status, HEAD and the 204 path intact', async () => {
    const failed = await rawRequest(unitPort, 'GET', '/api/status', { 'Accept-Encoding': 'br' });
    expect(failed.status).toBe(400);
    expect(failed.headers['content-encoding']).toBe('br');
    expect(zlib.brotliDecompressSync(failed.body).toString()).toBe(BIG_JSON);

    const head = await rawRequest(unitPort, 'HEAD', '/api/big', { 'Accept-Encoding': 'br' });
    expect(head.status).toBe(200);
    expect(head.body.length).toBe(0);
    expect(head.headers['content-encoding']).toBeUndefined();
    expect(Number(head.headers['content-length'])).toBe(BIG_JSON.length);

    const empty = await rawRequest(unitPort, 'GET', '/api/empty', { 'Accept-Encoding': 'br' });
    expect(empty.status).toBe(204);
    expect(empty.body.length).toBe(0);
    expect(empty.headers['content-encoding']).toBeUndefined();
  });

  it('never encodes a non-JSON or an already encoded answer', async () => {
    const text = await rawRequest(unitPort, 'GET', '/api/text', { 'Accept-Encoding': 'br' });
    expect(text.headers['content-encoding']).toBeUndefined();
    expect(text.body.toString()).toBe('x'.repeat(4096));

    const preencoded = await rawRequest(unitPort, 'GET', '/api/preencoded', {
      'Accept-Encoding': 'br, gzip',
    });
    expect(preencoded.headers['content-encoding']).toBe('gzip');
    expect(preencoded.body.equals(PREENCODED)).toBe(true);
  });

  it('streams an answer above the buffer cap out unencoded', async () => {
    const response = await rawRequest(unitPort, 'GET', '/api/huge', {
      'Accept-Encoding': 'br, gzip',
    });
    expect(response.status).toBe(200);
    expect(response.headers['content-encoding']).toBeUndefined();
    expect(response.body.length).toBe(HUGE.length);
  });
});

const USERNAME = 'aac-perf-admin';
const PASSWORD = '***';
const FIXTURE_ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_SESSION_SECRET',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
] as const;
const RATE_LIMIT_KEYS = ['127.0.0.1', '::ffff:127.0.0.1', '::1'];

let tempRoot = '';
let staticDir = '';
let instance: ServerInstance | undefined;
let port = 0;
let originalEnvironment: Record<string, string | undefined> = {};

async function stop(): Promise<void> {
  if (!instance) return;
  instance.cleanup();
  const server = instance.server;
  instance = undefined;
  server.closeAllConnections?.();
  await new Promise<void>((resolve) => server.close(() => resolve()));
}

beforeEach(async () => {
  originalEnvironment = Object.fromEntries(
    FIXTURE_ENVIRONMENT.map((name) => [name, process.env[name]])
  );
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-api-compression-'));
  process.env.CCS_HOME = tempRoot;
  process.env.CCS_DIR = path.join(tempRoot, '.ccs');
  process.env.CODEX_HOME = path.join(tempRoot, '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(tempRoot, '.claude');
  process.env.CCS_SESSION_SECRET = 'api-compression-fixture-secret';
  for (const name of [
    'CCS_DASHBOARD_AUTH_ENABLED',
    'CCS_DASHBOARD_USERNAME',
    'CCS_DASHBOARD_PASSWORD_HASH',
  ])
    delete process.env[name];
  fs.mkdirSync(process.env.CCS_DIR, { recursive: true, mode: 0o700 });
  resetDashboardAuthStateForTests();
  resetDeviceStoreForTests();
  resetSetupCodeForTests();
  setAuthClockForTests(null);
  setPasswordHashCostForTests(4);
  invalidateConfigCache();
  const config = createEmptyUnifiedConfig();
  config.dashboard_auth = {
    enabled: true,
    username: USERNAME,
    password_hash: await bcrypt.hash(PASSWORD, 4),
    session_timeout_hours: 24,
  };
  saveUnifiedConfig(config);
  invalidateConfigCache();

  staticDir = path.join(tempRoot, 'ui');
  fs.mkdirSync(staticDir, { recursive: true });
  fs.writeFileSync(
    path.join(staticDir, 'index.html'),
    '<!doctype html><canvas id="slint-dashboard"></canvas>'
  );
  fs.writeFileSync(path.join(staticDir, 'bridge.js'), 'export default 1;\n'.repeat(200));

  spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {});
  spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {});
  instance = await startServer({ port: 0, host: '127.0.0.1', staticDir });
  port = (instance.server.address() as AddressInfo).port;
  for (const key of RATE_LIMIT_KEYS) loginRateLimiter.resetKey(key);
  await resetAuthRateLimitsForTests();
});

afterEach(async () => {
  try {
    await settleAuthWrites();
    await stop();
  } finally {
    for (const key of RATE_LIMIT_KEYS) loginRateLimiter.resetKey(key);
    await resetAuthRateLimitsForTests();
    await settleAuthWrites();
    setAuthClockForTests(null);
    setPasswordHashCostForTests(null);
    resetDashboardAuthStateForTests();
    resetDeviceStoreForTests();
    resetSetupCodeForTests();
    invalidateConfigCache();
    mock.restore();
    for (const name of FIXTURE_ENVIRONMENT) {
      const value = originalEnvironment[name];
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

/** A signed-in browser: returns the connect.sid cookie pair. */
async function signIn(): Promise<string> {
  const response = await rawRequest(
    port,
    'POST',
    '/api/auth/login',
    {
      'Content-Type': 'application/json',
      Origin: `http://127.0.0.1:${port}`,
    },
    JSON.stringify({ username: USERNAME, password: PASSWORD })
  );
  expect(response.status).toBe(200);
  const match = /connect\.sid=[^;]+/.exec(String(response.headers['set-cookie'] ?? ''));
  expect(match).not.toBeNull();
  return match![0];
}

describe('startServer: compression, session skip and the upgrade', () => {
  it('compresses a signed-in /api JSON answer to the identical bytes', async () => {
    const cookie = await signIn();
    const identity = await rawRequest(port, 'GET', '/api/accounts/dashboard', { Cookie: cookie });
    expect(identity.status).toBe(200);
    expect(String(identity.headers['content-type'])).toContain('application/json');

    const br = await rawRequest(port, 'GET', '/api/accounts/dashboard', {
      Cookie: cookie,
      'Accept-Encoding': 'br, gzip',
    });
    expect(br.status).toBe(200);
    expect(br.headers['content-encoding']).toBe('br');
    expect(String(br.headers.vary)).toContain('Accept-Encoding');
    expect(br.body.length).toBeLessThan(identity.body.length);
    // The payload carries this run's updatedAt timestamp; everything else must match.
    const stable = (text: string): string => text.replace(/"updatedAt":"[^"]*"/g, '"updatedAt":""');
    expect(stable(zlib.brotliDecompressSync(br.body).toString())).toBe(
      stable(identity.body.toString())
    );
  });

  it('revalidates a small /api answer with 304 and answers HEAD without a body', async () => {
    const first = await rawRequest(port, 'GET', '/api/health');
    expect(first.status).toBe(200);
    const etag = String(first.headers.etag);
    expect(etag).toBeTruthy();
    const revalidated = await rawRequest(port, 'GET', '/api/health', { 'If-None-Match': etag });
    expect(revalidated.status).toBe(304);
    expect(revalidated.body.length).toBe(0);
    expect(revalidated.headers['content-encoding']).toBeUndefined();
    const head = await rawRequest(port, 'HEAD', '/api/health');
    expect(head.status).toBe(200);
    expect(head.body.length).toBe(0);
  });

  it('keeps the rolling session cookie off static assets but on pages and API', async () => {
    const cookie = await signIn();
    const api = await rawRequest(port, 'GET', '/api/accounts/dashboard', { Cookie: cookie });
    expect(api.status).toBe(200);
    expect(api.headers['set-cookie']).toBeDefined();

    const page = await rawRequest(port, 'GET', '/', { Cookie: cookie });
    expect(page.status).toBe(200);
    expect(page.headers['set-cookie']).toBeDefined();

    for (const method of ['GET', 'HEAD']) {
      const asset = await rawRequest(port, method, '/bridge.js', { Cookie: cookie });
      expect(asset.status).toBe(200);
      expect(asset.headers['set-cookie']).toBeUndefined();
    }
  });

  it('logs page and API requests but not static asset hits', async () => {
    clearRecentLogEntries();
    const asset = await rawRequest(port, 'GET', '/bridge.js');
    expect(asset.status).toBe(200);
    expect(asset.headers['x-ccs-request-id']).toBeUndefined();
    const api = await rawRequest(port, 'GET', '/api/health');
    expect(api.headers['x-ccs-request-id']).toBeDefined();
    const page = await rawRequest(port, 'GET', '/');
    expect(page.headers['x-ccs-request-id']).toBeDefined();

    const logged = getRecentLogEntries()
      .filter((entry) => entry.event === 'request.completed')
      .map((entry) => entry.context?.path)
      .filter((path) => path === '/bridge.js' || path === '/api/health');
    expect(logged).toEqual(['/api/health']);
  });

  it('revalidates a static asset with 304 and never sends x-powered-by', async () => {
    const first = await rawRequest(port, 'GET', '/bridge.js');
    expect(first.status).toBe(200);
    const revalidated = await rawRequest(port, 'GET', '/bridge.js', {
      'If-None-Match': String(first.headers.etag),
    });
    expect(revalidated.status).toBe(304);
    expect(revalidated.body.length).toBe(0);
    for (const route of ['/api/health', '/bridge.js', '/']) {
      const response = await rawRequest(port, 'GET', route);
      expect(response.headers['x-powered-by']).toBeUndefined();
    }
  });

  it('still upgrades /ws for a signed-in browser', async () => {
    const cookie = await signIn();
    const upgraded = await new Promise<boolean>((resolve) => {
      const request = http.request({
        host: '127.0.0.1',
        port,
        path: '/ws',
        agent: false,
        headers: {
          Connection: 'Upgrade',
          Upgrade: 'websocket',
          'Sec-WebSocket-Key': crypto.randomBytes(16).toString('base64'),
          'Sec-WebSocket-Version': '13',
          Origin: `http://127.0.0.1:${port}`,
          Cookie: cookie,
        },
      });
      request.on('upgrade', (_response, socket) => {
        socket.destroy();
        resolve(true);
      });
      request.on('response', () => resolve(false));
      request.on('error', () => resolve(false));
      request.end();
    });
    expect(upgraded).toBe(true);
  });
});
