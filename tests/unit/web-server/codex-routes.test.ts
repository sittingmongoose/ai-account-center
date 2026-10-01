import { afterAll, beforeAll, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Server } from 'http';

const fixtureEnvKeys = ['CODEX_HOME', 'CCS_HOME', 'CCS_DIR', 'CCS_DASHBOARD_AUTH_ENABLED'] as const;
const retiredEndpoints = [
  { method: 'GET', route: '/diagnostics' },
  { method: 'GET', route: '/config/raw' },
  { method: 'PUT', route: '/config/raw' },
  { method: 'PATCH', route: '/config/patch' },
] as const;

let server: Server;
let baseUrl = '';
let tempDir = '';
let codexHome = '';
let originalEnv: Record<string, string | undefined> = {};

beforeAll(async () => {
  originalEnv = Object.fromEntries(fixtureEnvKeys.map((key) => [key, process.env[key]]));
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-routes-test-'));
  codexHome = path.join(tempDir, '.codex');
  process.env.CODEX_HOME = codexHome;
  process.env.CCS_HOME = tempDir;
  process.env.CCS_DIR = path.join(tempDir, '.ccs');
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';

  const codexRoutesModule = await import('../../../src/web-server/routes/codex-routes');
  const app = express();
  app.use(express.json());
  app.use('/api/codex', codexRoutesModule.default);

  server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const address = server.address();
  if (!address || typeof address === 'string')
    throw new Error('Unable to resolve test server port');
  baseUrl = `http://127.0.0.1:${address.port}`;
});

beforeEach(() => {
  fs.rmSync(codexHome, { recursive: true, force: true });
  fs.mkdirSync(codexHome, { recursive: true });
});

afterAll(async () => {
  if (server) await new Promise<void>((resolve) => server.close(() => resolve()));
  for (const key of fixtureEnvKeys) {
    if (originalEnv[key] === undefined) delete process.env[key];
    else process.env[key] = originalEnv[key];
  }
  fs.rmSync(tempDir, { recursive: true, force: true });
});

async function requestRetiredEndpoint(endpoint: (typeof retiredEndpoints)[number]) {
  return fetch(`${baseUrl}/api/codex${endpoint.route}`, {
    method: endpoint.method,
    headers: { 'Content-Type': 'application/json', Origin: baseUrl },
    ...(endpoint.method === 'GET'
      ? {}
      : {
          body: JSON.stringify({
            rawText: 'model = "attempted-editor-change"',
            kind: 'feature',
            feature: 'multi_agent',
            enabled: true,
          }),
        }),
  });
}

describe('retired Codex editor HTTP contract', () => {
  it.each(retiredEndpoints)(
    'returns 404 for $method $route without creating native config',
    async (endpoint) => {
      const response = await requestRetiredEndpoint(endpoint);
      expect(response.status).toBe(404);
      expect(fs.existsSync(path.join(codexHome, 'config.toml'))).toBe(false);
    }
  );

  it.each(retiredEndpoints)(
    'returns 404 for $method $route and leaves an existing native config unchanged',
    async (endpoint) => {
      const configPath = path.join(codexHome, 'config.toml');
      const original = Buffer.from(
        'model = "fixture-private-model"\n[features]\nmulti_agent = false\n'
      );
      fs.writeFileSync(configPath, original);
      const before = fs.statSync(configPath);

      const response = await requestRetiredEndpoint(endpoint);
      expect(response.status).toBe(404);
      expect(fs.readFileSync(configPath)).toEqual(original);
      expect(fs.statSync(configPath).mtimeMs).toBe(before.mtimeMs);
    }
  );
});

describe('Codex profile read failure privacy', () => {
  it('returns a fixed 500 response without exposing a dependency error or its private details', async () => {
    const dashboard = await import('../../../src/codex-auth/codex-auth-dashboard-service');
    const canary = 'Bearer PRIVATE_CANARY_TOKEN at /private/canary-home/.codex/auth.json';
    const summary = spyOn(dashboard, 'getCodexAuthProfilesSummary').mockRejectedValue(
      new Error(canary)
    );
    try {
      const response = await fetch(`${baseUrl}/api/codex/profiles`);
      expect(response.status).toBe(500);
      expect(response.headers.get('content-type')).toContain('application/json');
      const body = await response.text();
      expect(JSON.parse(body)).toEqual({ error: 'Codex profiles could not be read.' });
      expect(body).not.toContain('PRIVATE_CANARY_TOKEN');
      expect(body).not.toContain('/private/canary-home');
      expect(body).not.toContain('auth.json');
      expect(summary).toHaveBeenCalledTimes(1);
    } finally {
      summary.mockRestore();
    }
  });
});
