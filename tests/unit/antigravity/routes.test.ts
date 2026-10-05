import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import express from 'express';
import type { Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { ANTIGRAVITY_AUTO_SWITCH_MESSAGES } from '../../../src/antigravity/auto-switch/monitor';
import type { ActivationResult, ActivateRequest } from '../../../src/antigravity/types';
import type { AntigravityRecoveryResult } from '../../../src/antigravity/switch-service';
import { PrivateStorageError } from '../../../src/antigravity/registry';
import type {
  AntigravityApiDependencies,
  AntigravityAutoSettings,
  AntigravityDashboardAccount,
  AntigravityInventory,
} from '../../../src/antigravity/usage-contract';
import { createAntigravityRouter } from '../../../src/web-server/routes/antigravity-routes';

// Everything below is invented fixture data. No provider, credential store,
// process, installed application, or production dashboard is accessed.
const PRIVATE = 'FIXTURE_PRIVATE_SENTINEL';
const EMAIL = 'party@example.com';
const SAMPLE_TIME = '2026-10-01T17:00:00.000Z';
const TOKEN = 'FixtureConfirmation_1234567890';
const SETTINGS: AntigravityAutoSettings = {
  enabled: false,
  thresholdUsedPercent: 95,
  pollIntervalSeconds: 60,
  maxQuotaAgeSeconds: 300,
  cooldownSeconds: 300,
  selectedHostIds: ['ubuntu'],
  requestedPoolId: null,
};

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

function autoStatus(patch: Partial<AntigravityAutoSettings> = {}): Record<string, unknown> {
  return {
    ...SETTINGS,
    selectedHostIds: [...SETTINGS.selectedHostIds],
    ...patch,
    outcome: 'disabled',
    message: ANTIGRAVITY_AUTO_SWITCH_MESSAGES.disabled,
    activationInProgress: false,
    lastCheckedAt: SAMPLE_TIME,
    lastSwitchedAt: null,
    lastProfileId: null,
    lastHostId: null,
  };
}

function active(): ActivationResult {
  return { status: 'active', profileId: 'party', hostId: 'ubuntu', email: EMAIL };
}

function recovery(): AntigravityRecoveryResult {
  return { status: 'completed', hostId: 'ubuntu', profileId: 'party', email: EMAIL };
}

function busy(): ActivationResult {
  return {
    status: 'confirmation-required',
    profileId: 'party',
    hostId: 'ubuntu',
    email: EMAIL,
    reason: 'running-processes',
    confirmation: {
      token: TOKEN,
      expiresAt: '2099-10-01T13:00:00-04:00',
      profileId: 'party',
      hostId: 'ubuntu',
      email: EMAIL,
      warning: PRIVATE,
      processes: [
        { pid: 12, role: 'cli', label: 'Antigravity CLI' },
        { pid: 13, role: 'desktop', label: 'Antigravity Desktop' },
        { pid: 14, role: 'language-server', label: 'Antigravity language server' },
      ],
    },
  };
}

interface Calls {
  inventory: number;
  accounts: Array<{ refresh: boolean }>;
  activation: ActivateRequest[];
  autoStatus: number;
  settings: Partial<AntigravityAutoSettings>[];
  invalidation: number;
  recovery: number;
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
    calls = {
      inventory: 0,
      accounts: [],
      activation: [],
      autoStatus: 0,
      settings: [],
      invalidation: 0,
      recovery: 0,
    };
    deps = {
      getInventory: async () => {
        calls.inventory++;
        return inventory();
      },
      getAccounts: async (options) => {
        calls.accounts.push(options);
        return [account()];
      },
      activate: async (request) => {
        calls.activation.push(request);
        return active();
      },
      recover: async () => {
        calls.recovery++;
        return recovery();
      },
      getAutoSwitchStatus: () => {
        calls.autoStatus++;
        return autoStatus();
      },
      updateAutoSwitchSettings: (patch) => {
        calls.settings.push(patch);
        return autoStatus(patch);
      },
      invalidateUsage: () => {
        calls.invalidation++;
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
      createAntigravityRouter(
        deps,
        (req) => req.get('origin') === baseUrl && req.get('host') === new URL(baseUrl).host,
        () => {
          clockReads++;
          return now;
        }
      )
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
    expect(calls).toEqual({
      inventory: 0,
      accounts: [],
      activation: [],
      autoStatus: 0,
      settings: [],
      invalidation: 0,
      recovery: 0,
    });
  }

  const endpoints = [
    { path: '/profiles', method: 'GET' },
    { path: '/profiles/quotas', method: 'GET' },
    { path: '/auto-switch', method: 'GET' },
    { path: '/auto-switch', method: 'PUT' },
    { path: '/profiles/party/activate', method: 'POST' },
    { path: '/profiles/party/confirm', method: 'POST' },
  ] as const;

  for (const endpoint of endpoints) {
    test(`${endpoint.method} ${endpoint.path} requires an authenticated session before any dependency`, async () => {
      const response = await request(endpoint.path, {
        method: endpoint.method,
        authenticated: false,
        ...(endpoint.method !== 'GET'
          ? { body: { hostId: 'ubuntu', confirmationToken: TOKEN } }
          : {}),
      });
      expect(response.status).toBe(401);
      expect(response.body).toEqual({ error: 'Authentication required' });
      expectNoEngineCalls();
    });
  }

  for (const endpoint of endpoints.filter((value) => value.method !== 'GET')) {
    for (const origin of [null, 'https://foreign.example', 'null']) {
      test(`${endpoint.method} ${endpoint.path} rejects ${origin ?? 'missing'} origin`, async () => {
        const response = await request(endpoint.path, {
          method: endpoint.method,
          origin,
          body: { hostId: 'ubuntu', confirmationToken: TOKEN },
        });
        expect(response.status).toBe(403);
        expectNoEngineCalls();
      });
    }
    for (const contentType of [null, 'text/plain', 'application/x-www-form-urlencoded']) {
      test(`${endpoint.method} ${endpoint.path} requires application/json, not ${contentType ?? 'missing type'}`, async () => {
        const response = await request(endpoint.path, {
          method: endpoint.method,
          contentType,
          body: { hostId: 'ubuntu', confirmationToken: TOKEN },
        });
        expect(response.status).toBe(415);
        expectNoEngineCalls();
      });
    }
  }

  test('a same-Origin header with a foreign Host is rejected', async () => {
    const response = await request('/profiles/party/activate', {
      method: 'POST',
      host: 'foreign.example',
      body: { hostId: 'ubuntu' },
    });
    expect(response.status).toBe(403);
    expectNoEngineCalls();
  });

  for (const path of [
    '/profiles?refresh=true',
    '/profiles?hostId=mac',
    '/auto-switch?enabled=true',
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
    value.status = 'cached';
    deps.getAccounts = async () => [value];
    const response = await request('/profiles/quotas');
    expect(response.status).toBe(200);
    expect(response.body.accounts[0]).toEqual({
      ...account(),
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

  const invalidActivationBodies: unknown[] = [
    null,
    [],
    {},
    { hostId: 'mac' },
    { hostId: 'windows' },
    { hostId: '../ubuntu' },
    { hostId: 'ubuntu', mode: 'automatic' },
    { hostId: 'ubuntu', expectedActiveIdentityKey: PRIVATE },
    { hostId: 'ubuntu', path: '/fixture/private' },
    { hostId: 'ubuntu', confirmationToken: 'short' },
    { hostId: 'ubuntu', confirmationToken: 'x'.repeat(257) },
    { hostId: 'ubuntu', confirmationToken: 'bad/token-1234567890' },
    { hostId: 'ubuntu', confirmationToken: null },
    { hostId: 'ubuntu', confirmationToken: 1234567890123456 },
  ];
  invalidActivationBodies.forEach((body, index) => {
    test(`invalid activation body ${index + 1} is rejected before the switching engine`, async () => {
      expect((await request('/profiles/party/activate', { method: 'POST', body })).status).toBe(
        400
      );
      expectNoEngineCalls();
    });
  });

  for (const profileId of [
    '..%2Fparty',
    'party%5Cfile',
    'party%252Fsecret',
    '-party',
    'p'.repeat(65),
  ]) {
    test(`invalid profile identifier ${profileId} is rejected without activation`, async () => {
      expect(
        (
          await request(`/profiles/${profileId}/activate`, {
            method: 'POST',
            body: { hostId: 'ubuntu' },
          })
        ).status
      ).toBe(400);
      expectNoEngineCalls();
    });
  }

  test('activation queries cannot pass target paths or alternate modes', async () => {
    expect(
      (
        await request('/profiles/party/activate?mode=automatic', {
          method: 'POST',
          body: { hostId: 'ubuntu' },
        })
      ).status
    ).toBe(400);
    expectNoEngineCalls();
  });

  test('activation without a token succeeds in server-selected manual mode', async () => {
    const response = await request('/profiles/party/activate', {
      method: 'POST',
      body: { hostId: 'ubuntu' },
    });
    expect(response.status).toBe(200);
    expect(response.body).toEqual(active());
    expect(calls.activation).toEqual([{ profileId: 'party', hostId: 'ubuntu', mode: 'manual' }]);
    expect(calls.invalidation).toBe(1);
  });

  test('recover finishes a stuck switch, publishes only public fields and refreshes usage', async () => {
    const response = await request('/recover', { method: 'POST', body: { hostId: 'ubuntu' } });
    expect(response.status).toBe(200);
    expect(response.body).toEqual({
      status: 'completed',
      hostId: 'ubuntu',
      profileId: 'party',
      email: EMAIL,
    });
    expect(calls.recovery).toBe(1);
    expect(calls.invalidation).toBe(1);
  });

  test('recover reports a still-stuck switch without clearing usage', async () => {
    deps.recover = async () => {
      calls.recovery++;
      return { status: 'recovery-required', hostId: 'ubuntu' };
    };
    const response = await request('/recover', { method: 'POST', body: { hostId: 'ubuntu' } });
    expect(response.status).toBe(500);
    expect(response.body).toEqual({ status: 'recovery-required', hostId: 'ubuntu' });
    expect(calls.invalidation).toBe(0);
  });

  test('recover refuses a switch that is still running instead of interfering', async () => {
    deps.recover = async () => {
      throw new PrivateStorageError('busy');
    };
    const response = await request('/recover', { method: 'POST', body: { hostId: 'ubuntu' } });
    expect(response.status).toBe(409);
    expect(calls.invalidation).toBe(0);
  });

  test('recover accepts only an Ubuntu host body', async () => {
    for (const body of [{}, { hostId: 'macos' }, { hostId: 'ubuntu', extra: true }, []]) {
      expect((await request('/recover', { method: 'POST', body })).status).toBe(400);
    }
    expectNoEngineCalls();
  });

  test('confirm requires a token even when an ordinary activation does not', async () => {
    expect(
      (await request('/profiles/party/confirm', { method: 'POST', body: { hostId: 'ubuntu' } }))
        .status
    ).toBe(400);
    expectNoEngineCalls();
  });

  for (const action of ['activate', 'confirm']) {
    test(`${action} passes a valid opaque token to the manual transaction`, async () => {
      const response = await request(`/profiles/party/${action}`, {
        method: 'POST',
        body: { hostId: 'ubuntu', confirmationToken: TOKEN },
      });
      expect(response.status).toBe(200);
      expect(calls.activation).toEqual([
        { profileId: 'party', hostId: 'ubuntu', mode: 'manual', confirmationToken: TOKEN },
      ]);
    });
  }

  test('busy confirmation returns only reviewed PIDs, canonical expiry and fixed public labels', async () => {
    const value = busy();
    Object.assign(value, { args: PRIVATE, rollbackState: PRIVATE });
    Object.assign(value.confirmation!, { nativePath: PRIVATE });
    for (const process of value.confirmation!.processes)
      Object.assign(process, {
        label: PRIVATE,
        args: PRIVATE,
        environment: PRIVATE,
        startTime: PRIVATE,
      });
    deps.activate = async (input) => {
      calls.activation.push(input);
      return value;
    };
    const response = await request('/profiles/party/activate', {
      method: 'POST',
      body: { hostId: 'ubuntu' },
    });
    expect(response.status).toBe(409);
    expect(response.body.confirmation).toEqual({
      token: TOKEN,
      expiresAt: '2099-10-01T17:00:00.000Z',
      profileId: 'party',
      hostId: 'ubuntu',
      email: EMAIL,
      warning:
        'Another Antigravity program is running on Ubuntu. Stop the listed programs, switch accounts, and restore their reviewed sessions?',
      processes: busy().confirmation!.processes,
    });
    expect(Object.keys(response.body).sort()).toEqual([
      'confirmation',
      'email',
      'hostId',
      'profileId',
      'reason',
      'status',
    ]);
    expect(calls.invalidation).toBe(0);
  });

  const resultStatuses = [
    ['already-active', 200, 1],
    ['busy', 409, 0],
    ['stale-confirmation', 409, 0],
    ['invalid-profile', 400, 0],
    ['unsupported-runtime-probe', 409, 0],
    ['deferred', 409, 0],
    ['failed-rolled-back', 500, 1],
    ['recovery-required', 500, 1],
  ] as const;
  for (const [status, httpStatus, invalidations] of resultStatuses) {
    test(`transaction status ${status} maps to HTTP ${httpStatus} without raw details`, async () => {
      deps.activate = async () => ({ ...active(), status });
      const response = await request('/profiles/party/confirm', {
        method: 'POST',
        body: { hostId: 'ubuntu', confirmationToken: TOKEN },
      });
      expect(response.status).toBe(httpStatus);
      expect(response.body.status).toBe(status);
      expect(calls.invalidation).toBe(invalidations);
    });
  }

  test('busy warning and stale confirmation preserve the cached usage reading and retry delay', async () => {
    const cached = { ...account(), status: 'cached' as const };
    const retryAfter = '2099-10-01T17:00:00.000Z';
    let cachedReading: AntigravityDashboardAccount | null = cached;
    let retryUntil: string | null = retryAfter;
    deps.getAccounts = async (options) => {
      calls.accounts.push(options);
      return cachedReading ? [cachedReading] : [];
    };
    deps.invalidateUsage = () => {
      calls.invalidation++;
      cachedReading = null;
      retryUntil = null;
    };
    deps.activate = async (input) => {
      calls.activation.push(input);
      return calls.activation.length === 1 ? busy() : { ...active(), status: 'stale-confirmation' };
    };

    expect(
      (
        await request('/profiles/party/activate', {
          method: 'POST',
          body: { hostId: 'ubuntu' },
        })
      ).status
    ).toBe(409);
    expect(
      (
        await request('/profiles/party/confirm', {
          method: 'POST',
          body: { hostId: 'ubuntu', confirmationToken: TOKEN },
        })
      ).status
    ).toBe(409);

    const usage = await request('/profiles/quotas');
    expect(usage.status).toBe(200);
    expect(usage.body.accounts[0]).toMatchObject({
      status: 'cached',
      fetchedAt: SAMPLE_TIME,
      sampledAt: SAMPLE_TIME,
      windows: cached.windows,
    });
    expect(cachedReading).toBe(cached);
    expect(retryUntil).toBe(retryAfter);
    expect(calls.accounts).toEqual([{ refresh: false }]);
    expect(calls.activation).toHaveLength(2);
    expect(calls.invalidation).toBe(0);
  });

  const invalidResultMutations: Array<[string, (value: ActivationResult) => void]> = [
    [
      'foreign host',
      (value) => {
        (value as any).hostId = 'mac';
      },
    ],
    [
      'different profile',
      (value) => {
        value.profileId = 'gmail';
      },
    ],
    [
      'unknown status',
      (value) => {
        (value as any).status = PRIVATE;
      },
    ],
    [
      'invalid expiry',
      (value) => {
        value.confirmation!.expiresAt = PRIVATE;
      },
    ],
    [
      'invalid token',
      (value) => {
        value.confirmation!.token = 'short';
      },
    ],
    [
      'foreign confirmation',
      (value) => {
        value.confirmation!.profileId = 'gmail';
      },
    ],
    [
      'unreviewed role',
      (value) => {
        (value.confirmation!.processes[0] as any).role = 'shell';
      },
    ],
    [
      'zero PID',
      (value) => {
        value.confirmation!.processes[0].pid = 0;
      },
    ],
    [
      'fractional PID',
      (value) => {
        value.confirmation!.processes[0].pid = 1.5;
      },
    ],
    [
      'too many processes',
      (value) => {
        value.confirmation!.processes = Array.from({ length: 33 }, () => ({
          pid: 1,
          role: 'cli',
          label: 'Antigravity CLI',
        }));
      },
    ],
  ];
  for (const [label, mutate] of invalidResultMutations) {
    test(`malformed adapter activation result: ${label} fails closed`, async () => {
      const value = busy();
      mutate(value);
      deps.activate = async () => value;
      const response = await request('/profiles/party/activate', {
        method: 'POST',
        body: { hostId: 'ubuntu' },
      });
      expect(response.status).toBe(500);
      expect(response.body).toEqual({
        error:
          'Antigravity account activation failed safely. Refresh the account list before retrying.',
      });
      expect(calls.invalidation).toBe(1);
    });
  }

  const invalidPatches: unknown[] = [
    null,
    [],
    {},
    { enabled: 'true' },
    { thresholdUsedPercent: 0 },
    { thresholdUsedPercent: 100 },
    { thresholdUsedPercent: 95.5 },
    { pollIntervalSeconds: 14 },
    { pollIntervalSeconds: 3601 },
    { maxQuotaAgeSeconds: 14 },
    { maxQuotaAgeSeconds: 901 },
    { cooldownSeconds: 59 },
    { cooldownSeconds: 3601 },
    { selectedHostIds: [] },
    { selectedHostIds: ['mac'] },
    { selectedHostIds: ['windows'] },
    { selectedHostIds: ['ubuntu', 'ubuntu'] },
    { requestedPoolId: '' },
    { requestedPoolId: PRIVATE.repeat(8) },
    { enabled: true, token: PRIVATE },
    { enabled: true, mode: 'automatic' },
  ];
  invalidPatches.forEach((body, index) => {
    test(`invalid automatic settings patch ${index + 1} cannot call persistence`, async () => {
      expect((await request('/auto-switch', { method: 'PUT', body })).status).toBe(400);
      expectNoEngineCalls();
    });
  });

  test('settings query keys are rejected before persistence', async () => {
    expect(
      (await request('/auto-switch?hostId=ubuntu', { method: 'PUT', body: { enabled: true } }))
        .status
    ).toBe(400);
    expectNoEngineCalls();
  });

  test('valid sibling-settings boundaries and provider pool identifiers are retained exactly', async () => {
    const patch: AntigravityAutoSettings = {
      enabled: true,
      thresholdUsedPercent: 99,
      pollIntervalSeconds: 15,
      maxQuotaAgeSeconds: 900,
      cooldownSeconds: 60,
      selectedHostIds: ['ubuntu'],
      requestedPoolId: 'models/gemini-2.5:pro@quota',
    };
    const response = await request('/auto-switch', { method: 'PUT', body: patch });
    expect(response.status).toBe(200);
    expect(calls.settings).toEqual([patch]);
    expect(response.body).toMatchObject(patch);
    expect(calls.activation).toEqual([]);
  });

  test('automatic status strips arbitrary messages and private extra fields', async () => {
    const value = {
      ...autoStatus(),
      outcome: 'waiting_idle',
      message: PRIVATE,
      privatePath: PRIVATE,
      identityKey: PRIVATE,
    };
    deps.getAutoSwitchStatus = () => value;
    const response = await request('/auto-switch');
    expect(response.status).toBe(200);
    expect(response.body).toEqual({
      ...autoStatus(),
      outcome: 'waiting_idle',
      message: ANTIGRAVITY_AUTO_SWITCH_MESSAGES.waiting_idle,
    });
  });

  for (const outcome of Object.keys(ANTIGRAVITY_AUTO_SWITCH_MESSAGES)) {
    test(`automatic outcome ${outcome} uses the sibling monitor's fixed public message`, async () => {
      deps.getAutoSwitchStatus = () => ({ ...autoStatus(), outcome, message: PRIVATE });
      const response = await request('/auto-switch');
      expect(response.status).toBe(200);
      expect(response.body.outcome).toBe(outcome);
      expect(response.body.message).toBe(
        ANTIGRAVITY_AUTO_SWITCH_MESSAGES[outcome as keyof typeof ANTIGRAVITY_AUTO_SWITCH_MESSAGES]
      );
    });
  }

  test('unknown automatic outcomes fail to the fixed error message', async () => {
    deps.getAutoSwitchStatus = () => ({ ...autoStatus(), outcome: PRIVATE, message: PRIVATE });
    const response = await request('/auto-switch');
    expect(response.status).toBe(200);
    expect(response.body.outcome).toBe('error');
    expect(response.body.message).toBe(ANTIGRAVITY_AUTO_SWITCH_MESSAGES.error);
  });

  for (const endpoint of endpoints) {
    test(`${endpoint.method} ${endpoint.path} hides dependency exceptions containing private data`, async () => {
      const fail = () => {
        throw new Error(`${PRIVATE} /fixture/private bearer fixture-only`);
      };
      deps.getInventory = async () => fail();
      deps.getAccounts = async () => fail();
      deps.getAutoSwitchStatus = fail;
      deps.updateAutoSwitchSettings = fail;
      deps.activate = async () => fail();
      const body =
        endpoint.path === '/auto-switch'
          ? { enabled: true }
          : { hostId: 'ubuntu', confirmationToken: TOKEN };
      const response = await request(endpoint.path, {
        method: endpoint.method,
        ...(endpoint.method !== 'GET' ? { body } : {}),
      });
      expect(response.status).toBe(500);
      expect(Object.keys(response.body)).toEqual(['error']);
      expect(response.text).not.toContain('/fixture/private');
      expect(response.text).not.toContain('bearer fixture-only');
      expect(calls.invalidation).toBe(endpoint.method === 'POST' ? 1 : 0);
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

  test('incomplete internal automatic settings cannot be published', async () => {
    const value = autoStatus();
    delete value.cooldownSeconds;
    deps.getAutoSwitchStatus = () => value;
    expect((await request('/auto-switch')).status).toBe(500);
  });
});
