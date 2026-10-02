import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Server } from 'http';
import * as activation from '../../../src/codex-auth/activate-codex-profile';
import { authMiddleware } from '../../../src/web-server/middleware/auth-middleware';
import codexRoutes from '../../../src/web-server/routes/codex-routes';
import { getCodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';

let server: Server;
let baseUrl: string;
let tmpDir: string;
let originalCcsDir: string | undefined;
let originalAuthEnabled: string | undefined;

beforeEach(async () => {
  originalCcsDir = process.env.CCS_DIR;
  originalAuthEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-auto-routes-'));
  process.env.CCS_DIR = tmpDir;
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (req.headers['x-test-session']) {
      Object.assign(req, { session: { authenticated: true } });
    }
    if (req.headers['x-test-remote']) {
      Object.defineProperty(req, 'socket', {
        value: { remoteAddress: '203.0.113.42' },
        configurable: true,
      });
    }
    next();
  });
  app.use(authMiddleware);
  app.use('/api/codex', codexRoutes);
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test server port');
  baseUrl = `http://127.0.0.1:${address.port}`;
});

afterEach(async () => {
  getCodexAutoSwitchService().stop();
  await new Promise<void>((resolve) => server.close(() => resolve()));
  mock.restore();
  if (originalCcsDir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = originalCcsDir;
  if (originalAuthEnabled === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = originalAuthEnabled;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

function request(
  method: 'GET' | 'PUT',
  body: unknown = { enabled: true },
  headers: Record<string, string> = {}
): Promise<Response> {
  return fetch(`${baseUrl}/api/codex/profiles/auto-switch`, {
    method,
    headers: { 'Content-Type': 'application/json', 'x-test-session': 'true', ...headers },
    ...(method === 'PUT' ? { body: JSON.stringify(body) } : {}),
  });
}

describe('native Codex automatic switching routes', () => {
  it('requires an authenticated session for reads and writes even when auth is disabled', async () => {
    for (const method of ['GET', 'PUT'] as const) {
      const response = await request(method, { enabled: true }, { 'x-test-session': '' });
      expect(response.status).toBe(401);
    }
    expect(fs.existsSync(path.join(tmpDir, 'codex-auto-switch.json'))).toBe(false);
  });

  it('returns safe disabled defaults without writing config', async () => {
    const response = await request('GET');
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      activationInProgress: false,
    });
    expect(fs.readdirSync(tmpDir)).toEqual([]);
  });

  it('persists enabling and disabling with no synchronous account activation', async () => {
    const activate = spyOn(activation, 'activateCodexProfile');
    const enabled = await request('PUT');
    expect(enabled.status).toBe(200);
    expect(await enabled.json()).toMatchObject({
      enabled: true,
      outcome: 'scheduled',
      activationInProgress: false,
    });
    const read = await request('GET');
    expect(await read.json()).toMatchObject({ enabled: true, outcome: 'scheduled' });
    const disabled = await request('PUT', { enabled: false });
    expect(await disabled.json()).toMatchObject({ enabled: false, outcome: 'disabled' });
    expect(activate).not.toHaveBeenCalled();
    const file = path.join(tmpDir, 'codex-auto-switch.json');
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({
      version: 1,
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
    });
  });

  it.each([
    ['null', null],
    ['array', []],
    ['missing enabled', {}],
    ['string enabled', { enabled: 'true' }],
    ['numeric enabled', { enabled: 1 }],
    ['zero threshold', { enabled: true, thresholdPercent: 0 }],
    ['threshold above range', { thresholdPercent: 100 }],
    ['fractional threshold', { thresholdPercent: 15.5 }],
    ['string threshold', { thresholdPercent: '15' }],
    ['null threshold', { thresholdPercent: null }],
    ['interval', { enabled: true, pollIntervalSeconds: 1 }],
    ['host', { enabled: true, host: 'attacker.example.test' }],
    ['path', { enabled: true, path: '/private/auth.json' }],
    ['Claude provider', { enabled: true, provider: 'claude' }],
    ['command', { enabled: true, command: 'anything' }],
  ])('rejects untrusted config input %s', async (_label, body) => {
    const response = await request('PUT', body);
    expect(response.status).toBe(400);
    expect(fs.existsSync(path.join(tmpDir, 'codex-auto-switch.json'))).toBe(false);
  });

  it('persists threshold-only changes while preserving enabled and enabled-only changes preserve threshold', async () => {
    await request('PUT', { enabled: true });
    const updated = await request('PUT', { thresholdPercent: 15 });
    expect(updated.status).toBe(200);
    expect(await updated.json()).toMatchObject({
      enabled: true,
      thresholdPercent: 15,
      outcome: 'scheduled',
    });
    const disabled = await request('PUT', { enabled: false });
    expect(await disabled.json()).toMatchObject({ enabled: false, thresholdPercent: 15 });
    const read = await request('GET');
    expect(await read.json()).toMatchObject({ enabled: false, thresholdPercent: 15 });
  });

  it.each([1, 99])(
    'accepts integer threshold boundary %s with an explicit enabled update',
    async (thresholdPercent) => {
      const response = await request('PUT', { enabled: true, thresholdPercent });
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({ enabled: true, thresholdPercent });
    }
  );

  it.each([
    { Origin: 'https://attacker.example.test' },
    { Host: 'attacker.example.test', Origin: 'http://127.0.0.1:3000' },
    { Origin: 'null' },
  ])('rejects cross-origin writes before config mutation', async (headers) => {
    const response = await request('PUT', { enabled: true }, headers);
    expect(response.status).toBe(403);
    expect(fs.existsSync(path.join(tmpDir, 'codex-auto-switch.json'))).toBe(false);
  });

  it('requires JSON content type for updates', async () => {
    const response = await request('PUT', { enabled: true }, { 'Content-Type': 'text/plain' });
    expect(response.status).toBe(415);
    expect(fs.readdirSync(tmpDir)).toEqual([]);
  });

  it('supports authenticated LAN access while retaining origin checks', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    const response = await request(
      'PUT',
      { enabled: true },
      { 'x-test-remote': 'true', Origin: baseUrl }
    );
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ enabled: true });
    const rejected = await request(
      'PUT',
      { enabled: false },
      { 'x-test-remote': 'true', Origin: 'https://attacker.example.test' }
    );
    expect(rejected.status).toBe(403);
    const read = await request('GET');
    expect(await read.json()).toMatchObject({ enabled: true });
  });

  it('sanitizes unreadable config and save failures', async () => {
    fs.mkdirSync(path.join(tmpDir, 'codex-auto-switch.json'));
    const read = await request('GET');
    expect(await read.json()).toMatchObject({ enabled: false, outcome: 'error' });
    const response = await request('PUT');
    expect(response.status).toBe(500);
    expect(await response.text()).not.toContain(tmpDir);
    expect(fs.statSync(path.join(tmpDir, 'codex-auto-switch.json')).isDirectory()).toBe(true);
  });

  it('reports the threshold in % used next to % remaining', async () => {
    const response = await request('GET');
    expect(await response.json()).toMatchObject({ thresholdPercent: 5, thresholdUsedPercent: 95 });
  });

  it.each([
    [{ thresholdUsedPercent: 95 }, 5],
    [{ thresholdUsedPercent: 80, enabled: true }, 20],
    [{ thresholdUsedPercent: 1 }, 99],
    [{ thresholdUsedPercent: 99 }, 1],
  ])('stores thresholdUsedPercent %j as thresholdPercent %d', async (body, stored) => {
    const response = await request('PUT', body);
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      thresholdPercent: stored,
      thresholdUsedPercent: 100 - stored,
    });
    const file = JSON.parse(fs.readFileSync(path.join(tmpDir, 'codex-auto-switch.json'), 'utf8'));
    expect(file.thresholdPercent).toBe(stored);
    expect(file).not.toHaveProperty('thresholdUsedPercent');
    expect(await (await request('GET')).json()).toMatchObject({
      thresholdUsedPercent: 100 - stored,
    });
  });

  it('rejects a body with both threshold fields', async () => {
    const response = await request('PUT', { thresholdPercent: 5, thresholdUsedPercent: 95 });
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({
      error: 'Provide thresholdPercent or thresholdUsedPercent, not both.',
    });
    expect(fs.existsSync(path.join(tmpDir, 'codex-auto-switch.json'))).toBe(false);
  });

  it.each([0, 100, 50.5, '95', null, -5])(
    'rejects thresholdUsedPercent %p',
    async (thresholdUsedPercent) => {
      const response = await request('PUT', { thresholdUsedPercent });
      expect(response.status).toBe(400);
      expect(fs.existsSync(path.join(tmpDir, 'codex-auto-switch.json'))).toBe(false);
    }
  );

  it('rejects unknown keys next to thresholdUsedPercent', async () => {
    const response = await request('PUT', {
      thresholdUsedPercent: 90,
      host: 'attacker.example.test',
    });
    expect(response.status).toBe(400);
    expect(fs.existsSync(path.join(tmpDir, 'codex-auto-switch.json'))).toBe(false);
  });
});
