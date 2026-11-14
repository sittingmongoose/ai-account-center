import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import express from 'express';
import type { Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import type {
  AntigravityApiDependencies,
  AntigravityDashboardAccount,
  AntigravityInventory,
} from '../../../src/antigravity/usage-contract';
import { createAntigravityRouter } from '../../../src/web-server/routes/antigravity-routes';

// Everything below is invented fixture data. No provider, credential store,
// process, installed application, or production dashboard is accessed.
const PRIVATE = 'FIXTURE_PRIVATE_SENTINEL';
const EMAIL = 'party@example.com';
const SAMPLE_TIME = '2026-10-01T17:00:00.000Z';
function inventory(): AntigravityInventory {
  return {
    schemaVersion: 1,
    hostId: 'ubuntu',
    profiles: ['gmail', 'party'].map((id) => ({
      id,
      email: `${id}@example.com`,
      plan: 'Google AI Pro',
      available: true,
      selected: id === 'gmail',
      runtimeVerified: id === 'gmail',
      verifiedAt: SAMPLE_TIME,
      hostId: 'ubuntu',
    })),
  };
}

function account(): AntigravityDashboardAccount {
  return {
    id: 'antigravity:profile:party',
    provider: 'antigravity',
    providerLabel: 'Antigravity',
    label: EMAIL,
    email: EMAIL,
    plan: 'Google AI Pro',
    platform: 'ubuntu',
    source: 'Antigravity saved login on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: SAMPLE_TIME,
    sampledAt: SAMPLE_TIME,
    isActive: false,
    windows: [
      {
        key: 'model-pool',
        label: 'Model pool',
        usedPercent: 15.1234,
        remainingPercent: 84.8766,
        resetAt: '2026-10-02T17:00:00.000Z',
        windowMinutes: null,
        used: null,
        limit: null,
        unit: null,
        kind: 'rate_limit',
        poolId: 'model-pool',
        modelIds: ['model-one'],
      },
    ],
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: 'party',
      antigravityHostIds: ['ubuntu'],
    },
  };
}

interface Calls {
  inventory: number;
  accounts: Array<{ refresh: boolean }>;
}

interface ResponseFixture {
  status: number;
  body: any;
  text: string;
}

interface RequestOptions {
  method?: 'GET' | 'POST' | 'PUT';
  authenticated?: boolean;
  origin?: string | null;
  contentType?: string | null;
  host?: string;
  body?: unknown;
}

describe('Antigravity HTTP controls on an owned loopback fixture', () => {
  let server: Server;
  let baseUrl: string;
  let calls: Calls;
  let deps: AntigravityApiDependencies;
  let now: number;
  let clockReads: number;

  beforeEach(async () => {
    // The fixture reading is current unless a test moves the clock past its reset.
    now = Date.parse(SAMPLE_TIME);
    clockReads = 0;
    calls = { inventory: 0, accounts: [] };
    deps = {
      getInventory: async () => {
        calls.inventory++;
        return inventory();
      },
      getAccounts: async (options) => {
        calls.accounts.push(options);
        return [account()];
      },
    };
    const app = express();
    // Let primitive JSON reach the router so its own fail-closed shape checks
    // are exercised rather than Express's earlier strict-parser rejection.
    app.use(express.json({ strict: false }));
    app.use((req, _res, next) => {
      Object.assign(req, {
        session: {
          authenticated: req.get('x-test-session') === 'fixture-session',
        },
      });
      next();
    });
    app.use(
      '/api/antigravity',
      createAntigravityRouter(deps, () => {
        clockReads++;
        return now;
      })
    );
    server = await new Promise<Server>((resolve) => {
      const fixtureServer = app.listen(0, '127.0.0.1', () => resolve(fixtureServer));
    });
    baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  });

  afterEach(async () => {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
      server.closeAllConnections();
    });
  });

  async function request(path: string, options: RequestOptions = {}): Promise<ResponseFixture> {
    const headers: Record<string, string> = {};
    if (options.authenticated !== false) headers['x-test-session'] = 'fixture-session';
    if (options.origin !== null) headers.origin = options.origin ?? baseUrl;
    if (options.contentType !== null)
      headers['content-type'] = options.contentType ?? 'application/json';
    if (options.host !== undefined) headers.host = options.host;
    const response = await fetch(`${baseUrl}/api/antigravity${path}`, {
      method: options.method ?? 'GET',
      headers,
      ...(options.body !== undefined ? { body: JSON.stringify(options.body) } : {}),
    });
    const text = await response.text();
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(text).not.toContain(PRIVATE);
    return { status: response.status, body: JSON.parse(text), text };
  }

  function expectNoEngineCalls(): void {
    expect(calls).toEqual({ inventory: 0, accounts: [] });
  }

  const endpoints = [
    { path: '/profiles', method: 'GET' },
    { path: '/profiles/quotas', method: 'GET' },
  ] as const;

  for (const endpoint of endpoints) {
    test(`${endpoint.method} ${endpoint.path} requires an authenticated session before any dependency`, async () => {
      const response = await request(endpoint.path, {
        method: endpoint.method,
        authenticated: false,
      });
      expect(response.status).toBe(401);
      expect(response.body).toEqual({ error: 'Authentication required' });
      expectNoEngineCalls();
    });
  }

  for (const path of [
    '/profiles?refresh=true',
    '/profiles?hostId=mac',
    '/profiles/quotas?refresh=1',
    '/profiles/quotas?refresh=',
    '/profiles/quotas?refresh=true&refresh=false',
    '/profiles/quotas?token=not-accepted',
  ]) {
    test(`rejects unknown or malformed query ${path}`, async () => {
      expect((await request(path)).status).toBe(400);
      expectNoEngineCalls();
    });
  }

  test('inventory exposes only rebuilt public identities and selected status', async () => {
    const value = inventory();
    Object.assign(value, { privatePath: PRIVATE });
    Object.assign(value.profiles[0], {
      identityKey: PRIVATE,
      credentialRevision: PRIVATE,
      path: PRIVATE,
    });
    deps.getInventory = async () => {
      calls.inventory++;
      return value;
    };
    const response = await request('/profiles');
    expect(response.status).toBe(200);
    expect(response.body).toEqual(inventory());
    expect(calls.inventory).toBe(1);
  });

  test('inventory publishes the paused update state but scrubs a hostile version', async () => {
    const value = inventory();
    value.nativeUpdatePaused = {
      installedVersion: '1.2.17',
      privatePath: PRIVATE,
    } as unknown as { installedVersion: string };
    deps.getInventory = async () => value;
    const response = await request('/profiles');
    expect(response.status).toBe(200);
    expect(response.body.nativeUpdatePaused).toEqual({ installedVersion: '1.2.17' });
    const hostile = inventory();
    hostile.nativeUpdatePaused = { installedVersion: '1.2.17; id' };
    deps.getInventory = async () => hostile;
    const scrubbed = await request('/profiles');
    expect(scrubbed.status).toBe(200);
    expect(scrubbed.body.nativeUpdatePaused).toEqual({ installedVersion: null });
  });

  test('inventory publishes a runtime service problem but scrubs hostile fields', async () => {
    const value = inventory();
    value.runtimeServiceProblem = {
      reason: 'missing-python-module',
      module: 'pyte',
      python: '3.14',
      builtFor: '3.13',
      exitStatus: null,
      privatePath: PRIVATE,
    } as unknown as NonNullable<typeof value.runtimeServiceProblem>;
    deps.getInventory = async () => value;
    const response = await request('/profiles');
    expect(response.status).toBe(200);
    expect(response.body.runtimeServiceProblem).toEqual({
      reason: 'missing-python-module',
      module: 'pyte',
      python: '3.14',
      builtFor: '3.13',
      exitStatus: null,
    });
    expect(JSON.stringify(response.body)).not.toContain(PRIVATE);
    const hostile = inventory();
    hostile.runtimeServiceProblem = {
      reason: 'service-failed',
      module: 'pyte; id',
      python: '3.14\n',
      builtFor: null,
      exitStatus: 999,
    };
    deps.getInventory = async () => hostile;
    const scrubbed = await request('/profiles');
    expect(scrubbed.body.runtimeServiceProblem).toEqual({
      reason: 'service-failed',
      module: null,
      python: null,
      builtFor: null,
      exitStatus: null,
    });
    const unknown = inventory();
    unknown.runtimeServiceProblem = { reason: 'not-running' } as never;
    deps.getInventory = async () => unknown;
    expect((await request('/profiles')).body).not.toHaveProperty('runtimeServiceProblem');
  });

  for (const refresh of [undefined, 'false', 'true']) {
    test(`quotas pass only explicit refresh=${refresh ?? 'default false'} to the dependency`, async () => {
      const response = await request(
        `/profiles/quotas${refresh === undefined ? '' : `?refresh=${refresh}`}`
      );
      expect(response.status).toBe(200);
      expect(calls.accounts).toEqual([{ refresh: refresh === 'true' }]);
      expect(response.body.accounts[0].windows[0].usedPercent).toBe(15.1234);
    });
  }

  test('quota output strips private raw/source/capability fields and rebuilds nested windows', async () => {
    const value = account();
    Object.assign(value, {
      identityKey: PRIVATE,
      credentialRevision: PRIVATE,
      raw: PRIVATE,
      source: PRIVATE,
      message: PRIVATE,
    });
    Object.assign(value.capabilities, {
      nativePath: PRIVATE,
      codexProfile: PRIVATE,
      claudePlatforms: ['mac'],
    });
    Object.assign(value.windows[0], { raw: PRIVATE, token: PRIVATE, resetPath: PRIVATE });
    value.antigravityPlan = { class: 'pro', summary: PRIVATE, models: [PRIVATE] };
    value.status = 'cached';
    deps.getAccounts = async () => [value];
    const response = await request('/profiles/quotas');
    expect(response.status).toBe(200);
    expect(response.body.accounts[0]).toEqual({
      ...account(),
      antigravityPlan: {
        class: 'pro',
        quotaPolicy: 'five-hour-weekly',
        summary:
          'Refreshes every 5 hours up to a weekly limit. Gemini 4 Hydrogen shares the Gemini Models limits (same reset and remaining); Gemini 4 Argon has its own weekly limit.',
        models: [
          'Gemini 3.8 Flash',
          'Gemini 3.7 Flash',
          'Gemini 3.6 Flash',
          'Gemini 3.1 Pro',
          'Gemini 4 Argon',
          'Gemini 4 Hydrogen',
          'Claude Sonnet 5.5',
          'Claude Opus 5.5',
          'Claude Sonnet 4.6',
          'Claude Opus 4.6',
          'GPT-OSS-120B',
        ],
        thirdPartyModels: true,
        creditsOverage: true,
      },
      status: 'cached',
      message: 'Showing the last successful Antigravity usage reading.',
    });
  });

  test('quota output marks a window whose reset passed after its reading, keeping the reading', async () => {
    now = Date.parse('2026-10-02T17:00:00.000Z');
    const response = await request('/profiles/quotas');
    expect(response.status).toBe(200);
    expect(response.body.accounts[0].windows[0]).toEqual({
      ...account().windows[0],
      resetPassed: true,
    });
    now = Date.parse('2026-10-02T16:59:59.999Z');
    const before = await request('/profiles/quotas');
    expect(before.body.accounts[0].windows[0]).toEqual(account().windows[0]);
  });

  test('quota output judges every account in one response against one clock reading', async () => {
    const second = account();
    second.id = 'antigravity:profile:gmail';
    second.capabilities.antigravityProfileId = 'gmail';
    deps.getAccounts = async () => [account(), second];
    const response = await request('/profiles/quotas');
    expect(response.status).toBe(200);
    expect(response.body.accounts).toHaveLength(2);
    expect(clockReads).toBe(1);
  });

  test('quota output filters malformed windows without inventing an hourly interval', async () => {
    const value = account();
    value.windows = [
      ...value.windows,
      { key: '../private', usedPercent: 80 },
      { key: 'not-quota', usedPercent: -1, remainingPercent: 101 },
      {
        key: 'remaining-only',
        remainingPercent: 75,
        label: 'Bearer fixture-only',
        modelIds: ['model-one', '../path', 'model-one'],
        resetAt: SAMPLE_TIME,
      },
    ] as AntigravityDashboardAccount['windows'];
    deps.getAccounts = async () => [value];
    const response = await request('/profiles/quotas');
    expect(response.status).toBe(200);
    expect(response.body.accounts[0].windows).toHaveLength(2);
    expect(response.body.accounts[0].windows[1]).toMatchObject({
      key: 'remaining-only',
      label: 'Model quota',
      usedPercent: 25,
      remainingPercent: 75,
      modelIds: ['model-one'],
      resetAt: SAMPLE_TIME,
      windowMinutes: null,
    });
  });

  for (const endpoint of endpoints) {
    test(`${endpoint.method} ${endpoint.path} hides dependency exceptions containing private data`, async () => {
      const fail = () => {
        throw new Error(`${PRIVATE} /fixture/private bearer fixture-only`);
      };
      deps.getInventory = async () => fail();
      deps.getAccounts = async () => fail();
      const response = await request(endpoint.path, { method: endpoint.method });
      expect(response.status).toBe(500);
      expect(Object.keys(response.body)).toEqual(['error']);
      expect(response.text).not.toContain('/fixture/private');
      expect(response.text).not.toContain('bearer fixture-only');
    });
  }

  test('invalid internal inventory identities cannot be published', async () => {
    const value = inventory();
    value.profiles[1].id = value.profiles[0].id;
    deps.getInventory = async () => value;
    expect((await request('/profiles')).status).toBe(500);
  });

  test('a quota adapter cannot publish an account for another profile or provider', async () => {
    const value = account();
    value.id = 'antigravity:profile:gmail';
    deps.getAccounts = async () => [value];
    expect((await request('/profiles/quotas')).status).toBe(500);
  });
});
