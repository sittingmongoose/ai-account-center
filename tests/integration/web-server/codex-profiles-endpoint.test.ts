/**
 * Integration tests for GET /api/codex/profiles endpoint.
 *
 * Covers:
 * - localhost GET -> 200 + correct shape
 * - non-localhost (mocked remote IP) -> 403 (H6 localhost guard)
 * - empty registry -> {active: null, default: null, profiles: []} (no 404)
 * - response contains no token substrings
 */

import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as http from 'http';
import * as os from 'os';
import * as path from 'path';
import express from 'express';

let tmpDir: string;
let ccsDir: string;
let server: http.Server | null = null;
let port: number;

// Helpers ------------------------------------------------------------------

function buildToken(payload: Record<string, unknown>): string {
  const header = Buffer.from(JSON.stringify({ alg: 'RS256', typ: 'JWT' })).toString('base64url');
  const body = Buffer.from(JSON.stringify(payload)).toString('base64url');
  return `${header}.${body}.fakesig`;
}

function writeAuthJson(profileDir: string, payload: Record<string, unknown>): void {
  const authJson = {
    tokens: {
      id_token: buildToken(payload),
      access_token: 'MUST_NOT_APPEAR_IN_RESPONSE',
      refresh_token: 'MUST_NOT_APPEAR_IN_RESPONSE',
    },
  };
  fs.writeFileSync(path.join(profileDir, 'auth.json'), JSON.stringify(authJson), {
    mode: 0o600,
  });
}

async function startApp(): Promise<void> {
  // Invalidate cache before each test
  const svc = await import('../../../src/codex-auth/codex-auth-dashboard-service');
  svc.invalidateCodexAuthProfilesCache();

  const codexRouter = (await import('../../../src/web-server/routes/codex-routes')).default;

  const app = express();
  app.use(express.json());
  app.use('/api/codex', codexRouter);

  await new Promise<void>((resolve) => {
    server = app.listen(0, '127.0.0.1', () => resolve());
  });
  port = (server!.address() as { port: number }).port;
}

async function stopApp(): Promise<void> {
  if (server) {
    await new Promise<void>((resolve, reject) => {
      server!.close((err) => (err ? reject(err) : resolve()));
    });
    server = null;
  }
}

async function get(urlPath: string): Promise<{ status: number; body: unknown }> {
  const res = await fetch(`http://127.0.0.1:${port}${urlPath}`);
  const body = await res.json();
  return { status: res.status, body };
}

// Setup / teardown ---------------------------------------------------------

beforeEach(async () => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-int-test-'));
  process.env.CCS_HOME = tmpDir;
  // getCcsDir() returns path.join(CCS_HOME, '.ccs')
  ccsDir = path.join(tmpDir, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  delete process.env.CODEX_HOME;
  delete process.env.CCS_CODEX_PROFILE;
  delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  const nativeQuota = await import('../../../src/web-server/usage/native-quota-collector');
  nativeQuota.resetNativeQuotaState();

  await startApp();
});

describe('GET /api/codex/profiles/quotas', () => {
  it('returns an empty saved-profile quota list without a registry', async () => {
    const { status, body } = await get('/api/codex/profiles/quotas');
    expect(status).toBe(200);
    expect(body).toEqual({ profiles: [] });
  });

  it('blocks non-local dashboard quota reads when auth is disabled', async () => {
    const response = await fetch(`http://127.0.0.1:${port}/api/codex/profiles/quotas`, {
      headers: { Host: '192.0.2.20:3000' },
    });
    expect(response.status).toBe(403);
  });

  it('returns a sanitized error for an unreadable registry', async () => {
    fs.writeFileSync(path.join(ccsDir, 'codex-profiles.yaml'), '{ invalid: yaml: [');
    const { status, body } = await get('/api/codex/profiles/quotas');
    expect(status).toBe(500);
    expect(body).toEqual({ error: 'Codex profile usage could not be read safely.' });
    expect(JSON.stringify(body)).not.toContain(ccsDir);
  });

  it('returns native profile usage without exposing or changing saved authentication', async () => {
    const profileDir = path.join(ccsDir, 'codex-instances', 'gmail');
    fs.mkdirSync(profileDir, { recursive: true });
    const savedAuth = JSON.stringify({
      tokens: {
        access_token: 'FAKE_SECRET_ACCESS_MUST_STAY_PRIVATE',
        account_id: 'fake-workspace-gmail',
        id_token: buildToken({ email: 'gmail@example.com' }),
        refresh_token: 'FAKE_SECRET_REFRESH_MUST_STAY_PRIVATE',
      },
    });
    fs.writeFileSync(path.join(profileDir, 'auth.json'), savedAuth);
    fs.writeFileSync(
      path.join(ccsDir, 'codex-profiles.yaml'),
      'version: "1.0"\ndefault: gmail\nprofiles:\n  gmail:\n    type: codex\n    created: "2026-01-01T00:00:00Z"\n    last_used: null\n'
    );
    const originalFetch = global.fetch;
    let quotaCalls = 0;
    try {
      global.fetch = (async (input, init) => {
        if (String(input).startsWith('https://chatgpt.com/backend-api/wham/usage')) {
          quotaCalls += 1;
          const headers = new Headers(init?.headers);
          expect(headers.get('Authorization')).toBe('Bearer FAKE_SECRET_ACCESS_MUST_STAY_PRIVATE');
          expect(headers.get('ChatGPT-Account-Id')).toBe('fake-workspace-gmail');
          return new Response(
            JSON.stringify({
              plan_type: 'pro',
              rate_limit: {
                primary_window: { used_percent: 15, reset_after_seconds: 1000 },
                secondary_window: { used_percent: 45, reset_after_seconds: 86400 },
              },
            })
          );
        }
        return originalFetch(input, init);
      }) as typeof fetch;
      const { status, body } = await get('/api/codex/profiles/quotas');
      const result = body as {
        profiles: Array<{
          profileName: string;
          status: string;
          windows: Array<{ usedPercent: number }>;
        }>;
      };
      expect(status).toBe(200);
      expect(result.profiles[0].profileName).toBe('gmail');
      expect(result.profiles[0].status).toBe('available');
      expect(result.profiles[0].windows.map((window) => window.usedPercent)).toEqual([15, 45]);
      expect(quotaCalls).toBe(1);
      expect(JSON.stringify(body)).not.toContain('FAKE_SECRET');
      expect(JSON.stringify(body)).not.toContain('fake-workspace');
      expect(fs.readFileSync(path.join(profileDir, 'auth.json'), 'utf8')).toBe(savedAuth);
    } finally {
      global.fetch = originalFetch;
    }
  });
});

afterEach(async () => {
  await stopApp();
  delete process.env.CCS_HOME;
  delete process.env.CODEX_HOME;
  delete process.env.CCS_CODEX_PROFILE;
  mock.restore();
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

// Tests --------------------------------------------------------------------

describe('GET /api/codex/profiles', () => {
  it('returns 200 with empty shape when registry does not exist', async () => {
    const { status, body } = await get('/api/codex/profiles');

    expect(status).toBe(200);
    const b = body as Record<string, unknown>;
    expect(b.active).toBeNull();
    expect(b.default).toBeNull();
    expect(Array.isArray(b.profiles)).toBe(true);
    expect((b.profiles as unknown[]).length).toBe(0);
  });

  it('returns 500 when registry YAML is malformed', async () => {
    const registryPath = path.join(ccsDir, 'codex-profiles.yaml');
    fs.writeFileSync(registryPath, '{ invalid: yaml: [', { mode: 0o600 });

    const svc = await import('../../../src/codex-auth/codex-auth-dashboard-service');
    svc.invalidateCodexAuthProfilesCache();

    const { status, body } = await get('/api/codex/profiles');

    expect(status).toBe(500);
    expect((body as { error?: string }).error).toBe('Codex profiles could not be read.');
  });

  it('returns 500 when a malformed registry appears after an empty response was cached', async () => {
    const first = await get('/api/codex/profiles');
    expect(first.status).toBe(200);

    const registryPath = path.join(ccsDir, 'codex-profiles.yaml');
    fs.writeFileSync(registryPath, '{ invalid: yaml: [', { mode: 0o600 });
    const future = new Date(Date.now() + 10_000);
    fs.utimesSync(registryPath, future, future);

    const { status, body } = await get('/api/codex/profiles');

    expect(status).toBe(500);
    expect((body as { error?: string }).error).toBe('Codex profiles could not be read.');
  });

  it('returns a sanitized 500 when registry stat fails', async () => {
    const registryPath = path.join(ccsDir, 'codex-profiles.yaml');
    const rawMessage = `EACCES: permission denied, stat '${registryPath}'`;
    const realStatSync = fs.statSync;
    spyOn(fs, 'statSync').mockImplementation((target) => {
      if (target === registryPath) {
        const err = new Error(rawMessage) as NodeJS.ErrnoException;
        err.code = 'EACCES';
        throw err;
      }
      return realStatSync(target);
    });

    const { status, body } = await get('/api/codex/profiles');
    const error = (body as { error?: string }).error ?? '';

    expect(status).toBe(500);
    expect(error).toBe('Codex profiles could not be read.');
    expect(error).not.toContain(registryPath);
    expect(error).not.toContain('EACCES');
  });

  it('returns 200 with decoded email and plan for a valid profile', async () => {
    const instancesDir = path.join(ccsDir, 'codex-instances');
    const workDir = path.join(instancesDir, 'work');
    fs.mkdirSync(workDir, { recursive: true });

    writeAuthJson(workDir, {
      email: 'work@example.com',
      'https://api.openai.com/auth': {
        chatgpt_plan_type: 'pro',
        chatgpt_account_id: 'acct-work',
      },
    });

    const registryPath = path.join(ccsDir, 'codex-profiles.yaml');
    fs.writeFileSync(
      registryPath,
      `version: "1.0"\ndefault: work\nprofiles:\n  work:\n    type: codex\n    created: "2026-01-01T00:00:00Z"\n    last_used: "2026-05-17T05:45:00Z"\n`,
      { mode: 0o600 }
    );

    // Invalidate cache so the new files are read
    const svc = await import('../../../src/codex-auth/codex-auth-dashboard-service');
    svc.invalidateCodexAuthProfilesCache();

    const { status, body } = await get('/api/codex/profiles');
    const b = body as Record<string, unknown>;

    expect(status).toBe(200);
    const profiles = b.profiles as Array<Record<string, unknown>>;
    expect(profiles.length).toBe(1);
    const profile = profiles[0];
    expect(profile?.name).toBe('work');
    expect(profile?.email).toBe('work@example.com');
    expect(profile?.plan).toBe('pro');
    expect(profile?.authValid).toBe(true);
    const active = b.active as Record<string, unknown>;
    expect(active?.source).toBe('default');
    expect(active?.name).toBe('work');
  });

  it('returns 403 when requireLocalAccessWhenAuthDisabled guard rejects non-localhost', async () => {
    // The guard checks req.socket.remoteAddress. Since the server binds to
    // 127.0.0.1 and the test client connects to 127.0.0.1, the built-in fetch
    // will always be loopback. We test the guard directly via a separate
    // Express app that injects a non-loopback remote address.
    const { requireLocalAccessWhenAuthDisabled } = await import(
      '../../../src/web-server/middleware/auth-middleware'
    );
    const { isDashboardAuthEnabled } = await import('../../../src/config/config-loader-facade');

    if (!isDashboardAuthEnabled()) {
      let guardResult: boolean | undefined;
      let responseStatus: number | undefined;

      const testApp = express();
      testApp.get('/test', (req, res) => {
        // Spoof a non-localhost remote address
        Object.defineProperty(req, 'socket', {
          value: { remoteAddress: '203.0.113.42' },
          writable: true,
          configurable: true,
        });
        guardResult = requireLocalAccessWhenAuthDisabled(req, res, 'localhost only');
        if (guardResult) {
          responseStatus = 200;
          res.json({ ok: true });
        } else {
          responseStatus = 403;
        }
      });

      const testServer = await new Promise<http.Server>((resolve) => {
        const s = testApp.listen(0, '127.0.0.1', () => resolve(s));
      });
      const testPort = (testServer.address() as { port: number }).port;

      const res = await fetch(`http://127.0.0.1:${testPort}/test`);
      await new Promise<void>((resolve) => testServer.close(() => resolve()));

      expect(res.status).toBe(403);
      expect(guardResult).toBe(false);
    }
  });

  it('returns 403 for loopback remote addresses when host/origin indicate non-local origin', async () => {
    const { requireLocalAccessWhenAuthDisabled } = await import(
      '../../../src/web-server/middleware/auth-middleware'
    );
    const { isDashboardAuthEnabled } = await import('../../../src/config/config-loader-facade');

    if (!isDashboardAuthEnabled()) {
      let guardResult: boolean | undefined;

      const testApp = express();
      testApp.get('/test', (req, res) => {
        Object.defineProperty(req, 'socket', {
          value: { remoteAddress: '127.0.0.1' },
          writable: true,
          configurable: true,
        });
        req.headers.host = 'attacker.example.test';
        req.headers.origin = 'http://attacker.example.test';

        guardResult = requireLocalAccessWhenAuthDisabled(req, res, 'localhost only');
        if (guardResult) {
          res.json({ ok: true });
        }
      });

      const testServer = await new Promise<http.Server>((resolve) => {
        const s = testApp.listen(0, '127.0.0.1', () => resolve(s));
      });
      const testPort = (testServer.address() as { port: number }).port;

      const res = await fetch(`http://127.0.0.1:${testPort}/test`);
      await new Promise<void>((resolve) => testServer.close(() => resolve()));

      expect(res.status).toBe(403);
      expect(guardResult).toBe(false);
    }
  });

  it('response body contains no token substrings for a valid profile', async () => {
    const instancesDir = path.join(ccsDir, 'codex-instances');
    const workDir = path.join(instancesDir, 'work');
    fs.mkdirSync(workDir, { recursive: true });

    writeAuthJson(workDir, {
      email: 'secure@example.com',
      'https://api.openai.com/auth': {
        chatgpt_plan_type: 'pro',
        chatgpt_account_id: 'acct-secure',
      },
    });

    const registryPath = path.join(ccsDir, 'codex-profiles.yaml');
    fs.writeFileSync(
      registryPath,
      `version: "1.0"\ndefault: work\nprofiles:\n  work:\n    type: codex\n    created: "2026-01-01T00:00:00Z"\n    last_used: null\n`,
      { mode: 0o600 }
    );

    // Invalidate cache so the new files are read
    const svc = await import('../../../src/codex-auth/codex-auth-dashboard-service');
    svc.invalidateCodexAuthProfilesCache();

    const { status, body } = await get('/api/codex/profiles');
    expect(status).toBe(200);

    const bodyStr = JSON.stringify(body);
    expect(bodyStr).not.toContain('id_token');
    expect(bodyStr).not.toContain('access_token');
    expect(bodyStr).not.toContain('refresh_token');
    expect(bodyStr).not.toContain('MUST_NOT_APPEAR_IN_RESPONSE');
  });
});
