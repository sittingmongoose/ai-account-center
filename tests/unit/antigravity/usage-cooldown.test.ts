import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AntigravityProfileRegistry,
  credentialFingerprint,
  identityKey,
} from '../../../src/antigravity/registry';
import {
  createAntigravityRuntime,
  type AntigravityRuntime,
  type SavedQuotaRequest,
  type SavedQuotaSnapshot,
} from '../../../src/antigravity/runtime-composition';
import { defaultAntigravityAutoSwitchState } from '../../../src/antigravity/auto-switch/settings';
import type { AntigravitySwitchDriver } from '../../../src/antigravity/types';
import { readIdentityUsageCooldown } from '../../../src/web-server/services/usage-failover-cooldown';
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import type {
  UsageCandidate,
  UsageFailoverTarget,
} from '../../../src/web-server/services/usage-failover-contract';

const STAMP = '2026-10-01T17:00:00.000Z';
const scopes: string[] = [];
const runtimes: AntigravityRuntime[] = [];
afterEach(() => {
  for (const runtime of runtimes.splice(0)) runtime.stop();
  for (const scope of scopes.splice(0)) fs.rmSync(scope, { recursive: true, force: true });
});

function fixture() {
  const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-cooldown-'));
  scopes.push(scope);
  fs.chmodSync(scope, 0o700);
  const profiles = ['alice', 'bob'].map((id) => {
    const credential = { format: 'fixture-json', bytes: Buffer.from(`PRIVATE-FIXTURE-${id}`) };
    const identity = {
      email: `${id}@example.test`,
      subject: `fixture-authoritative-${id}`,
      plan: 'Pro',
      verifiedAt: STAMP,
      source: 'provider-userinfo' as const,
    };
    const directory = path.join(scope, 'antigravity-instances', id, 'ubuntu');
    fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
    fs.writeFileSync(
      path.join(directory, `credential-${credentialFingerprint(credential)}.bin`),
      credential.bytes,
      { mode: 0o600 }
    );
    return {
      id,
      email: identity.email,
      plan: identity.plan,
      identityKey: identityKey(identity),
      credential: {
        format: credential.format,
        fingerprint: credentialFingerprint(credential),
        identity,
        savedAt: STAMP,
      },
    };
  });
  fs.mkdirSync(path.join(scope, 'antigravity-profiles'), { mode: 0o700 });
  fs.writeFileSync(
    path.join(scope, 'antigravity-profiles/registry-000000000001.json'),
    JSON.stringify({ version: 1, revision: 1, active: null, transaction: null, profiles }),
    { mode: 0o600 }
  );
  const registry = new AntigravityProfileRegistry(scope, { readOnly: true });
  let now = Date.parse(STAMP);
  let state = defaultAntigravityAutoSwitchState();
  let callback: (() => void) | null = null;
  const quotaCalls: string[] = [];
  const forbidden = async (): Promise<never> => {
    throw new Error('Native mutation forbidden');
  };
  const driver: AntigravitySwitchDriver = {
    hostId: 'ubuntu',
    canProveRuntimeIdentity: async () => true,
    readCurrentCredential: async () => registry.readCredential('alice', 'ubuntu').credential,
    validateCredential: async () => profiles[0].credential.identity,
    inspectProcesses: forbidden,
    stopProcesses: forbidden,
    installCredential: forbidden,
    readStoredIdentity: forbidden,
    restartProcesses: forbidden,
    proveRuntimeIdentity: forbidden,
    rollbackCredential: forbidden,
    stopOwnedRestarts: forbidden,
  };
  const sample = (request: SavedQuotaRequest): SavedQuotaSnapshot => ({
    profileId: request.profileId,
    identityKey: request.identityKey,
    credentialRevision: request.credentialRevision,
    email: request.email,
    plan: request.identity.plan,
    status: 'fresh',
    fetchedAt: new Date(now).toISOString(),
    sampledAt: new Date(now).toISOString(),
    windows: [
      {
        key: 'model-pool',
        label: 'Model quota',
        remainingPercent: 80,
        resetAt: new Date(now + 3600_000).toISOString(),
        poolId: 'model-pool',
        poolIdSource: 'provider-id',
        modelIds: ['fixture-model'],
      },
    ],
    source: 'native-consumer',
    identityVerified: true,
    identityValidation: 'verified',
    pools: [
      {
        id: 'model-pool',
        idSource: 'provider-id',
        eligibility: 'reported-quota',
        complete: true,
        windows: [
          {
            key: 'model-pool',
            kind: 'rate_limit',
            remainingPercent: 80,
            resetAt: new Date(now + 3600_000).toISOString(),
          },
        ],
      },
    ],
  });
  let collect = async (request: SavedQuotaRequest) => sample(request);
  const construct = () => {
    const runtime = createAntigravityRuntime({
      registry,
      ccsDirectory: scope,
      driver,
      now: () => now,
      store: {
        read: () => structuredClone(state),
        write: (next) => {
          state = structuredClone(next);
        },
      },
      collectQuota: async (request) => {
        quotaCalls.push(request.profileId);
        return collect(request);
      },
      observeHost: async () => ({
        hostId: 'ubuntu',
        available: true,
        complete: true,
        busy: false,
        manualActivationInProgress: false,
        sampledAt: new Date(now).toISOString(),
      }),
      setTimer: (next) => {
        callback = next;
        return next;
      },
      clearTimer: () => {
        callback = null;
      },
    });
    runtimes.push(runtime);
    return runtime;
  };
  const candidate: UsageCandidate = {
    locator: 'a'.repeat(64),
    revision: 'b'.repeat(64),
    selectors: ['antigravity-profile:alice'],
    identity: { kind: 'antigravity-subject', key: profiles[0].identityKey },
    email: profiles[0].email,
    plan: 'Pro',
  };
  const host = { host: 'mac' as const, platform: 'mac' as const, sshHost: 'fixture-mac' };
  const target: UsageFailoverTarget = {
    accountId: 'antigravity:profile:alice',
    provider: 'antigravity',
    ccsDir: scope,
    binding: 'fixture-saved-credential',
    primarySources: [{ ...host, selector: 'antigravity-profile:alice' }],
    expectedEmail: candidate.email,
    expectedIdentity: candidate.identity,
  };
  return {
    scope,
    candidate,
    host,
    target,
    quotaCalls,
    construct,
    sample,
    now: () => now,
    advance: (milliseconds: number) => {
      now += milliseconds;
    },
    setCollect: (next: typeof collect) => {
      collect = next;
    },
    rotateAliceCredential: () => {
      const credential = { format: 'fixture-json', bytes: Buffer.from('PRIVATE-ROTATED-FIXTURE') };
      profiles[0].credential.fingerprint = credentialFingerprint(credential);
      fs.writeFileSync(
        path.join(
          scope,
          'antigravity-instances/alice/ubuntu',
          `credential-${credentialFingerprint(credential)}.bin`
        ),
        credential.bytes,
        { mode: 0o600 }
      );
      fs.writeFileSync(
        path.join(scope, 'antigravity-profiles/registry-000000000002.json'),
        JSON.stringify({ version: 1, revision: 2, active: null, transaction: null, profiles }),
        { mode: 0o600 }
      );
    },
    tick: async () => {
      callback?.();
      await new Promise<void>((resolve) => setImmediate(resolve));
    },
  };
}

describe('native and alternate Antigravity identity cooldown', () => {
  it('keeps a bound in-flight 429 hold when the same account credential rotates before the response', async () => {
    const h = fixture();
    h.setCollect(async (request) => {
      if (request.profileId !== 'alice') return h.sample(request);
      h.rotateAliceCredential();
      return {
        ...h.sample(request),
        status: 'rate_limited',
        windows: [],
        pools: [],
        retryAfterSeconds: 120,
      };
    });
    const [result] = await h.construct().getAccounts({ refresh: true });
    expect(result.windows).toEqual([]);
    expect(result.status).not.toBe('ok');
    expect(readIdentityUsageCooldown(h.scope, 'antigravity', h.candidate.identity, h.now())).toBe(
      h.now() + 120_000
    );
    h.setCollect(async (request) => h.sample(request));
    await h.construct().getAccounts({ refresh: true });
    expect(h.quotaCalls).toEqual(['alice', 'bob', 'bob']);
  });

  it('does not publish a cooldown for a response that does not bind the original request', async () => {
    const invalid: Array<Partial<SavedQuotaSnapshot>> = [
      { profileId: 'bob' },
      { identityKey: 'f'.repeat(64) },
      { credentialRevision: 'f'.repeat(64) },
      { source: 'other-source' as SavedQuotaSnapshot['source'] },
      { email: 'other@example.test' },
      { identityValidation: 'mismatch' },
      { identityValidation: 'needs_sign_in' },
    ];
    for (const mismatch of invalid) {
      const h = fixture();
      h.setCollect(async (request) =>
        request.profileId === 'alice'
          ? {
              ...h.sample(request),
              status: 'rate_limited',
              windows: [],
              pools: [],
              retryAfterSeconds: 120,
              ...mismatch,
            }
          : h.sample(request)
      );
      const [result] = await h.construct().getAccounts({ refresh: true });
      expect(result.windows).toEqual([]);
      expect(readIdentityUsageCooldown(h.scope, 'antigravity', h.candidate.identity, h.now())).toBe(
        0
      );
    }
  });

  it('native 429 suppresses alternate quota calls and survives runtime reconstruction', async () => {
    const h = fixture();
    h.setCollect(async (request) =>
      request.profileId === 'alice'
        ? {
            ...h.sample(request),
            status: 'rate_limited',
            windows: [],
            pools: [],
            retryAfterSeconds: 120,
          }
        : h.sample(request)
    );
    const native = h.construct();
    const accounts = await native.getAccounts({ refresh: true });
    expect(readIdentityUsageCooldown(h.scope, 'antigravity', h.candidate.identity, h.now())).toBe(
      h.now() + 120_000
    );
    const requests: string[] = [];
    const alternate = new UsageFailoverService({
      now: h.now,
      hosts: async () => [h.host],
      transport: async (_, request) => {
        requests.push(request.action);
        if (request.action === 'collect') throw new Error('Quota call forbidden during cooldown');
        return { version: 1, action: 'inventory', candidates: [h.candidate] };
      },
    });
    const row: DashboardAccount = { ...accounts[0], windows: [] };
    await alternate.recover(h.target, row);
    expect(requests).not.toContain('collect');
    expect(JSON.stringify(accounts)).not.toContain('retryAfterSeconds');
    expect(JSON.stringify(accounts)).not.toContain(h.candidate.identity.key);
    h.advance(119_000);
    await h.construct().getAccounts({ refresh: true });
    expect(h.quotaCalls).toEqual(['alice', 'bob', 'bob']);
  });

  it('alternate 429 gates native usage and the monitor until the shared deadline', async () => {
    const h = fixture();
    const native = h.construct();
    const alternate = new UsageFailoverService({
      now: h.now,
      hosts: async () => [h.host],
      transport: async (_, request) =>
        request.action === 'inventory'
          ? { version: 1, action: 'inventory', candidates: [h.candidate] }
          : {
              version: 1,
              action: 'collect',
              candidate: h.candidate,
              sample: null,
              failureCode: 'rate_limited',
              retryAfterSeconds: 120,
            },
    });
    const row: DashboardAccount = { ...native.cachedAccounts()[0], windows: [] };
    await alternate.collect(h.target, row);
    await native.getAccounts({ refresh: true });
    expect(h.quotaCalls).toEqual(['bob']);
    native.updateAutoSwitchSettings({ enabled: true, requestedPoolId: 'model-pool' });
    native.start();
    await h.tick();
    expect(h.quotaCalls).toEqual(['bob', 'bob']);
    native.stop();
    h.advance(121_000);
    native.invalidateUsage?.();
    await native.getAccounts({ refresh: true });
    expect(h.quotaCalls).toEqual(['bob', 'bob', 'alice', 'bob']);
  });
});
