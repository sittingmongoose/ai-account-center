import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Server } from 'http';
import * as activation from '../../../src/codex-auth/activate-codex-profile';
import * as dashboard from '../../../src/codex-auth/codex-auth-dashboard-service';
import { authMiddleware } from '../../../src/web-server/middleware/auth-middleware';
import codexRoutes from '../../../src/web-server/routes/codex-routes';

let server: Server;
let baseUrl: string;
let tmpDir: string;
let originalCcsHome: string | undefined;
let originalAuthEnabled: string | undefined;

beforeEach(async () => {
  originalCcsHome = process.env.CCS_HOME;
  originalAuthEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activate-routes-'));
  process.env.CCS_HOME = tmpDir;
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';

  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (req.headers['x-test-remote']) {
      Object.defineProperty(req, 'socket', {
        value: { remoteAddress: '203.0.113.42' },
        configurable: true,
      });
    }
    if (req.headers['x-test-session']) {
      Object.assign(req, { session: { authenticated: true } });
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
  await new Promise<void>((resolve) => server.close(() => resolve()));
  mock.restore();
  if (originalCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = originalCcsHome;
  if (originalAuthEnabled === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = originalAuthEnabled;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

function post(headers: Record<string, string> = {}): Promise<Response> {
  return fetch(`${baseUrl}/api/codex/profiles/work/activate`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: '{}',
  });
}

function stubActivation() {
  return spyOn(activation, 'activateCodexProfile').mockResolvedValue({
    name: 'work',
    email: 'work@example.test',
    plan: 'pro',
    codexHome: '/fake/.codex',
    previousEmail: 'personal@example.test',
    // The route whitelists identity fields even if a dependency grows new fields.
    tokens: 'MUST_NOT_APPEAR_IN_RESPONSE',
  } as activation.CodexActivationResult);
}

describe('POST /api/codex/profiles/:name/activate', () => {
  it('uses shared activation, invalidates summary, and returns only display-safe fields', async () => {
    const activate = stubActivation();
    const invalidate = spyOn(dashboard, 'invalidateCodexAuthProfilesCache');
    const response = await post();
    const body = await response.json();
    expect(response.status).toBe(200);
    expect(activate).toHaveBeenCalledWith('work');
    expect(invalidate).toHaveBeenCalledTimes(1);
    expect(body).toEqual({
      success: true,
      name: 'work',
      email: 'work@example.test',
      plan: 'pro',
      codexHome: '/fake/.codex',
      previousEmail: 'personal@example.test',
    });
    expect(JSON.stringify(body)).not.toContain('MUST_NOT_APPEAR_IN_RESPONSE');
  });

  it.each(['busy', 'invalid_profile', 'invalid_codex_home', 'restart_failed'] as const)(
    'maps %s to a safe response without raw error details',
    async (code) => {
      spyOn(activation, 'activateCodexProfile').mockRejectedValue(
        new activation.CodexActivationError(code, 'MUST_NOT_APPEAR_IN_RESPONSE')
      );
      const response = await post();
      const body = (await response.json()) as { code: string; error: string };
      expect(response.status).toBe(code === 'busy' ? 409 : code === 'restart_failed' ? 500 : 400);
      expect(body.code).toBe(code);
      expect(body.error).not.toContain('MUST_NOT_APPEAR_IN_RESPONSE');
    }
  );

  it('sanitizes unexpected errors and refreshes summaries after a failed restart', async () => {
    spyOn(activation, 'activateCodexProfile').mockRejectedValue(
      new Error('MUST_NOT_APPEAR_IN_RESPONSE')
    );
    const invalidate = spyOn(dashboard, 'invalidateCodexAuthProfilesCache');
    const response = await post();
    expect(response.status).toBe(500);
    expect(await response.text()).not.toContain('MUST_NOT_APPEAR_IN_RESPONSE');
    expect(invalidate).toHaveBeenCalledTimes(1);
  });

  it.each([
    { 'x-test-remote': 'true' },
    { Origin: 'https://attacker.example.test' },
    { Host: 'attacker.example.test' },
  ])('rejects nonlocal clients or origins before activation', async (headers) => {
    const activate = stubActivation();
    const response = await post(headers);
    expect(response.status).toBe(403);
    expect(activate).not.toHaveBeenCalled();
  });

  it('rejects writes without a JSON content type', async () => {
    const activate = stubActivation();
    const response = await post({ 'Content-Type': 'text/plain' });
    expect(response.status).toBe(415);
    expect(activate).not.toHaveBeenCalled();
  });

  it('preserves required dashboard authentication', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    const activate = stubActivation();
    const response = await post();
    expect(response.status).toBe(401);
    expect(activate).not.toHaveBeenCalled();
  });

  it('allows an authenticated LAN client and still rejects cross-origin writes', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    const activate = stubActivation();
    const rejected = await post({
      'x-test-session': 'true',
      'x-test-remote': 'true',
      Origin: 'https://attacker.example.test',
    });
    expect(rejected.status).toBe(403);
    expect(activate).not.toHaveBeenCalled();
    const accepted = await post({ 'x-test-session': 'true', 'x-test-remote': 'true' });
    expect(accepted.status).toBe(200);
    expect(activate).toHaveBeenCalledTimes(1);
  });
});
