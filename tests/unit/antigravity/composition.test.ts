import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import * as fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import {
  createAntigravityRuntime,
  type AntigravityRuntime,
  type AntigravityRuntimeDependencies,
  type SavedQuotaRequest,
  type SavedQuotaSnapshot,
} from '../../../src/antigravity/runtime-composition';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import { AntigravitySwitchService } from '../../../src/antigravity/switch-service';
import type {
  AntigravitySwitchDriver,
  NativeCredential,
  VerifiedIdentity,
} from '../../../src/antigravity/types';

// This suite imports only the explicitly injected composition root. It never
// imports the default-directory resolver, a platform adapter, or a provider.
const INITIAL_NOW = Date.parse('2026-10-01T17:00:00.000Z');
const FAKE_PRIVATE = 'FIXTURE_ONLY_PRIVATE_NATIVE_BYTES';
const PROFILE_IDS = ['gmail', 'party'] as const;
type FixtureProfileId = (typeof PROFILE_IDS)[number];

function nativeCredential(id: FixtureProfileId, version = 1): NativeCredential {
  return {
    format: 'fake-native-json',
    bytes: Buffer.from(JSON.stringify({ fixtureAccount: id, version, private: FAKE_PRIVATE })),
  };
}

function identity(id: FixtureProfileId, now: number): VerifiedIdentity {
  return {
    email: `${id}@example.com`,
    subject: `fixture-authoritative-subject-${id}`,
    plan: 'Google AI Pro',
    verifiedAt: new Date(now).toISOString(),
    source: 'provider-userinfo',
  };
}

function directorySnapshot(directory: string): Array<{ path: string; hash: string }> {
  const result: Array<{ path: string; hash: string }> = [];
  const visit = (current: string): void => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const filename = path.join(current, entry.name);
      if (entry.isDirectory()) visit(filename);
      else if (entry.isFile()) {
        result.push({
          path: path.relative(directory, filename),
          hash: createHash('sha256').update(fs.readFileSync(filename)).digest('hex'),
        });
      } else throw new Error('Unexpected non-owned fixture filesystem entry.');
    }
  };
  visit(directory);
  return result.sort((left, right) => left.path.localeCompare(right.path));
}

describe('Antigravity explicitly injected runtime composition', () => {
  let fixtureDirectory: string;
  let privateCcsDirectory: string;
  let registry: AntigravityProfileRegistry;
  let driver: AntigravitySwitchDriver;
  let importer: AntigravitySwitchService;
  let runtime: AntigravityRuntime | undefined;
  let now: number;
  let supported: boolean;
  let selected: FixtureProfileId;
  let driverCalls: string[];
  let quotaCalls: SavedQuotaRequest[];
  let collectQuota: AntigravityRuntimeDependencies['collectQuota'];

  beforeEach(() => {
    fixtureDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-composition-fixture-'));
    privateCcsDirectory = path.join(fixtureDirectory, '.ccs');
    registry = new AntigravityProfileRegistry(privateCcsDirectory);
    now = INITIAL_NOW;
    supported = false;
    selected = 'gmail';
    driverCalls = [];
    quotaCalls = [];
    const forbiddenNativeAction = async (name: string): Promise<never> => {
      driverCalls.push(name);
      throw new Error(`Unexpected fixture-only native action: ${name}`);
    };
    driver = {
      hostId: 'ubuntu',
      canProveRuntimeIdentity: async () => {
        driverCalls.push('canProveRuntimeIdentity');
        return supported;
      },
      readCurrentCredential: async () => {
        driverCalls.push('readCurrentCredential');
        return nativeCredential(selected);
      },
      validateCredential: async (credential) => {
        driverCalls.push('validateCredential');
        if (credential.format !== 'fake-native-json') throw new Error('Fixture format mismatch.');
        const parsed = JSON.parse(credential.bytes.toString('utf8'));
        if (!PROFILE_IDS.includes(parsed.fixtureAccount) || parsed.private !== FAKE_PRIVATE)
          throw new Error('Only invented fixture bytes may be validated.');
        return identity(parsed.fixtureAccount, now);
      },
      inspectProcesses: async () => forbiddenNativeAction('inspectProcesses'),
      stopProcesses: async () => forbiddenNativeAction('stopProcesses'),
      installCredential: async () => forbiddenNativeAction('installCredential'),
      readStoredIdentity: async () => forbiddenNativeAction('readStoredIdentity'),
      restartProcesses: async () => forbiddenNativeAction('restartProcesses'),
      proveRuntimeIdentity: async () => forbiddenNativeAction('proveRuntimeIdentity'),
      rollbackCredential: async () => forbiddenNativeAction('rollbackCredential'),
      stopOwnedRestarts: async () => forbiddenNativeAction('stopOwnedRestarts'),
    };
    importer = new AntigravitySwitchService({ registry, driver, now: () => now });
    collectQuota = async (request) => sample(request);
  });

  afterEach(() => {
    fs.rmSync(fixtureDirectory, { recursive: true, force: true });
    runtime = undefined;
  });

  function sample(request: SavedQuotaRequest, remainingPercent = 80): SavedQuotaSnapshot {
    const resetAt = new Date(now + 3600_000).toISOString();
    return {
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
          label: 'Model pool',
          remainingPercent,
          resetAt,
          poolId: 'model-pool',
          poolIdSource: 'provider-id',
          modelIds: ['model-one'],
        },
      ],
      source: 'native-consumer',
      identityVerified: true,
      identityValidation: 'verified',
    };
  }

  function construct(extra: Partial<AntigravityRuntimeDependencies> = {}): AntigravityRuntime {
    runtime = createAntigravityRuntime({
      registry,
      driver,
      now: () => now,
      collectQuota: async (request) => {
        quotaCalls.push(request);
        return collectQuota(request);
      },
      ...extra,
    });
    return runtime;
  }

  async function importFixtures(): Promise<void> {
    for (const id of PROFILE_IDS) await importer.importNativeProfile(id, nativeCredential(id));
    expect(driverCalls).toEqual(['validateCredential', 'validateCredential']);
    driverCalls = [];
  }

  function expectNoNativeActions(): void {
    const actionNames = [
      'inspectProcesses',
      'stopProcesses',
      'installCredential',
      'readStoredIdentity',
      'restartProcesses',
      'proveRuntimeIdentity',
      'rollbackCredential',
      'stopOwnedRestarts',
    ];
    expect(driverCalls.filter((name) => actionNames.includes(name))).toEqual([]);
  }

  test('construction has no quota, driver, timer, settings, or import side effects', () => {
    const before = directorySnapshot(privateCcsDirectory);
    const instance = construct();
    expect(directorySnapshot(privateCcsDirectory)).toEqual(before);
    expect(driverCalls).toEqual([]);
    expect(quotaCalls).toEqual([]);
    expect(instance.hasProfiles()).toBe(false);
    expect(registry.listProfiles()).toEqual([]);
  });

  test('explicit imports persist two verified fixture profiles without replacing native login', async () => {
    await importFixtures();
    const reopened = new AntigravityProfileRegistry(privateCcsDirectory);
    expect(reopened.listProfiles().map((profile) => profile.id)).toEqual(PROFILE_IDS);
    expect(
      reopened
        .readCredential('party', 'ubuntu')
        .credential.bytes.equals(nativeCredential('party').bytes)
    ).toBe(true);
    const instance = construct();
    expect(instance.hasProfiles()).toBe(true);
    expect(selected).toBe('gmail');
    expect(driverCalls).toEqual([]);
    expectNoNativeActions();
  });

  test('getAccounts samples both saved credentials independently and never activates an inactive account', async () => {
    await importFixtures();
    const before = directorySnapshot(privateCcsDirectory);
    const instance = construct();
    const accounts = await instance.getAccounts({ refresh: false });
    expect(accounts.map((account) => account.id)).toEqual([
      'antigravity:profile:gmail',
      'antigravity:profile:party',
    ]);
    expect(accounts.map((account) => account.status)).toEqual(['ok', 'ok']);
    expect(accounts.map((account) => account.isActive)).toEqual([true, false]);
    expect(quotaCalls.map((request) => request.profileId)).toEqual(PROFILE_IDS);
    for (const request of quotaCalls) {
      const saved = registry.readCredential(request.profileId, 'ubuntu');
      expect(request.credential.bytes.equals(saved.credential.bytes)).toBe(true);
      expect(request.identity.subject).toBe(`fixture-authoritative-subject-${request.profileId}`);
      expect(request.credentialRevision).toBe(saved.credentialRevision);
    }
    expect(directorySnapshot(privateCcsDirectory)).toEqual(before);
    expect(selected).toBe('gmail');
    expectNoNativeActions();
  });

  test('public inventory, accounts, cached rows and status omit subjects, native bytes and revision keys', async () => {
    await importFixtures();
    collectQuota = async (request) => {
      const result = sample(request);
      Object.assign(result, {
        raw: FAKE_PRIVATE,
        privatePath: '/fixture-only/private',
        subject: request.identity.subject,
      });
      Object.assign(result.windows[0] as object, {
        raw: FAKE_PRIVATE,
        credentialRevision: request.credentialRevision,
      });
      return result;
    };
    const instance = construct();
    const publicValue = {
      inventory: await instance.getInventory(),
      accounts: await instance.getAccounts({ refresh: false }),
      cached: instance.cachedAccounts(),
    };
    const encoded = JSON.stringify(publicValue);
    for (const forbidden of [
      'identityKey',
      'credentialRevision',
      'subject',
      'credential',
      'privatePath',
      'raw',
      FAKE_PRIVATE,
      '/fixture-only/private',
    ])
      expect(encoded).not.toContain(forbidden);
    for (const request of quotaCalls) {
      expect(encoded).not.toContain(request.identityKey);
      expect(encoded).not.toContain(request.credentialRevision);
      expect(encoded).not.toContain(request.identity.subject);
      expect(encoded).not.toContain(request.credential.bytes.toString('utf8'));
    }
    expectNoNativeActions();
  });

  test('a paused native update ships in the inventory while quotas stay readable', async () => {
    await importFixtures();
    const instance = construct({
      readNativeUpdatePaused: async () => ({ installedVersion: '1.2.17' }),
    });
    const inventory = await instance.getInventory();
    expect(inventory.nativeUpdatePaused).toEqual({ installedVersion: '1.2.17' });
    const accounts = await instance.getAccounts({ refresh: false });
    expect(accounts.map((account) => account.status)).toEqual(['ok', 'ok']);
    expectNoNativeActions();
  });

  test('a failing or hostile paused probe leaves the inventory unpaused', async () => {
    await importFixtures();
    const failing = construct({
      readNativeUpdatePaused: async () => {
        throw new Error(FAKE_PRIVATE);
      },
    });
    expect(await failing.getInventory()).not.toHaveProperty('nativeUpdatePaused');
    const hostile = construct({
      readNativeUpdatePaused: async () => ({ installedVersion: '1.2.17; id' }),
    });
    expect((await hostile.getInventory()).nativeUpdatePaused).toEqual({
      installedVersion: null,
    });
    const absent = construct();
    expect(await absent.getInventory()).not.toHaveProperty('nativeUpdatePaused');
    expectNoNativeActions();
  });

  test('a runtime service that cannot start ships in the inventory, scrubbed, while quotas stay readable', async () => {
    await importFixtures();
    driver.canProveRuntimeIdentity = async () => false;
    const instance = construct({
      readRuntimeServiceProblem: async () => ({
        reason: 'missing-python-module',
        module: 'pyte',
        python: '3.14',
        builtFor: '3.13',
        exitStatus: null,
      }),
    });
    const inventory = await instance.getInventory();
    expect(inventory.runtimeServiceProblem).toEqual({
      reason: 'missing-python-module',
      module: 'pyte',
      python: '3.14',
      builtFor: '3.13',
      exitStatus: null,
    });
    const accounts = await instance.getAccounts({ refresh: false });
    expect(accounts.map((account) => account.status)).toEqual(['ok', 'ok']);
    const hostile = construct({
      readRuntimeServiceProblem: async () =>
        ({
          reason: 'missing-python-module',
          module: `pyte ${FAKE_PRIVATE}`,
          python: '3.14',
          builtFor: '3.13',
          exitStatus: null,
          privatePath: FAKE_PRIVATE,
        }) as never,
    });
    const scrubbed = await hostile.getInventory();
    expect(scrubbed.runtimeServiceProblem?.module).toBeNull();
    expect(JSON.stringify(scrubbed)).not.toContain(FAKE_PRIVATE);
    const failing = construct({
      readRuntimeServiceProblem: async () => {
        throw new Error(FAKE_PRIVATE);
      },
    });
    expect(await failing.getInventory()).not.toHaveProperty('runtimeServiceProblem');
    const unknownReason = construct({
      readRuntimeServiceProblem: async () => ({ reason: 'not-running' }) as never,
    });
    expect(await unknownReason.getInventory()).not.toHaveProperty('runtimeServiceProblem');
    expect(await construct().getInventory()).not.toHaveProperty('runtimeServiceProblem');
    expectNoNativeActions();
  });

  test('account-bound identity mismatch clears the affected previous good quota instead of reusing it', async () => {
    await importFixtures();
    const instance = construct();
    expect(
      (await instance.getAccounts({ refresh: false })).map((account) => account.status)
    ).toEqual(['ok', 'ok']);
    now += 5001;
    collectQuota = async (request) => ({
      ...sample(request),
      ...(request.profileId === 'party' ? { email: 'different@example.com' } : {}),
    });
    const accounts = await instance.getAccounts({ refresh: true });
    expect(accounts.find((account) => account.email === 'party@example.com')).toMatchObject({
      status: 'needs_sign_in',
      windows: [],
    });
    expect(accounts.find((account) => account.email === 'gmail@example.com')?.status).toBe('ok');
    expect(
      instance.cachedAccounts().find((account) => account.email === 'party@example.com')?.windows
    ).toEqual([]);
    expectNoNativeActions();
  });

  test('a credential revision race discards in-flight and cached old quota without installing either account', async () => {
    await importFixtures();
    const instance = construct();
    await instance.getAccounts({ refresh: false });
    now += 5001;
    let changed = false;
    collectQuota = async (request) => {
      if (request.profileId === 'party' && !changed) {
        changed = true;
        await importer.importNativeProfile('party', nativeCredential('party', 2));
      }
      return sample(request);
    };
    const accounts = await instance.getAccounts({ refresh: true });
    const party = accounts.find((account) => account.email === 'party@example.com');
    expect(changed).toBe(true);
    expect(party?.status).not.toBe('ok');
    expect(party?.windows).toEqual([]);
    expect(
      instance.cachedAccounts().find((account) => account.email === 'party@example.com')?.windows
    ).toEqual([]);
    expect(
      registry
        .readCredential('party', 'ubuntu')
        .credential.bytes.equals(nativeCredential('party', 2).bytes)
    ).toBe(true);
    expect(selected).toBe('gmail');
    expectNoNativeActions();
  });

  test('cached rows hold the last sample across a same-identity credential revision change', async () => {
    await importFixtures();
    supported = true;
    const instance = construct();
    const initial = await instance.getAccounts({ refresh: false });
    expect(initial.find((account) => account.email === 'party@example.com')).toMatchObject({
      status: 'ok',
    });
    const oldRevision = registry.readCredential('party', 'ubuntu').credentialRevision;
    await importer.importNativeProfile('party', nativeCredential('party', 2));
    expect(registry.readCredential('party', 'ubuntu').credentialRevision).not.toBe(oldRevision);
    const beforeCalls = [...driverCalls];
    const beforeQuotaCount = quotaCalls.length;
    const beforeFiles = directorySnapshot(privateCcsDirectory);

    const cached = instance.cachedAccounts();
    const party = cached.find((account) => account.email === 'party@example.com');
    // Same identity, new revision: the row holds the last sample as cached
    // instead of going dark while the new sign-in is read.
    expect(party).toMatchObject({
      status: 'cached',
      message: 'Showing the last saved sample while the new sign-in is read.',
    });
    expect(party?.windows).toHaveLength(1);
    expect(party?.fetchedAt).not.toBeNull();
    expect(party?.sampledAt).not.toBeNull();
    expect(cached.find((account) => account.email === 'gmail@example.com')?.windows).toHaveLength(
      1
    );
    expect(driverCalls).toEqual(beforeCalls);
    expect(quotaCalls).toHaveLength(beforeQuotaCount);
    expect(directorySnapshot(privateCcsDirectory)).toEqual(beforeFiles);
    expectNoNativeActions();
  });

  for (const scenario of ['missing bundle', 'foreign replacement under old revision'] as const) {
    test(`cached rows drop unreadable saved credentials: ${scenario}`, async () => {
      await importFixtures();
      supported = true;
      const instance = construct();
      await instance.getAccounts({ refresh: false });
      const revision = registry.readCredential('party', 'ubuntu').credentialRevision;
      const credentialFile = path.join(
        privateCcsDirectory,
        'antigravity-instances',
        'party',
        'ubuntu',
        `credential-${revision}.bin`
      );
      if (scenario === 'missing bundle') fs.unlinkSync(credentialFile);
      else {
        const replacement = `${credentialFile}.fixture-replacement`;
        fs.writeFileSync(replacement, nativeCredential('party', 2).bytes, { mode: 0o600 });
        fs.renameSync(replacement, credentialFile);
      }
      const beforeCalls = [...driverCalls];
      const beforeQuotaCount = quotaCalls.length;
      const beforeFiles = directorySnapshot(privateCcsDirectory);

      const cached = instance.cachedAccounts();
      expect(cached.find((account) => account.email === 'party@example.com')).toMatchObject({
        status: 'needs_sign_in',
        windows: [],
        fetchedAt: null,
        sampledAt: null,
        capabilities: { antigravityHostIds: [] },
      });
      expect(cached.find((account) => account.email === 'gmail@example.com')?.windows).toHaveLength(
        1
      );
      expect(driverCalls).toEqual(beforeCalls);
      expect(quotaCalls).toHaveLength(beforeQuotaCount);
      expect(directorySnapshot(privateCcsDirectory)).toEqual(beforeFiles);
      expectNoNativeActions();
    });
  }

  test('synchronous cached rows make no native or collector call and keep no unverified selected-account claim', async () => {
    await importFixtures();
    const instance = construct();
    const accounts = await instance.getAccounts({ refresh: false });
    expect(accounts[0].isActive).toBe(true);
    const beforeCalls = [...driverCalls];
    const beforeQuotaCount = quotaCalls.length;
    const cached = instance.cachedAccounts();
    expect(cached.map((account) => account.windows)).toEqual(
      accounts.map((account) => account.windows)
    );
    expect(cached.map((account) => account.isActive)).toEqual([false, false]);
    expect(driverCalls).toEqual(beforeCalls);
    expect(quotaCalls).toHaveLength(beforeQuotaCount);
    expect(await instance.readSelectedProfileId()).toBe('gmail');
    selected = 'party';
    expect(await instance.readSelectedProfileId()).toBe('party');
    expectNoNativeActions();
  });

  test('cached account copies cannot mutate future public windows or model IDs', async () => {
    await importFixtures();
    const instance = construct();
    await instance.getAccounts({ refresh: false });
    const first = instance.cachedAccounts();
    first[0].windows[0].usedPercent = 100;
    first[0].windows[0].modelIds!.push('foreign-model');
    first[0].capabilities.antigravityHostIds.length = 0;
    const second = instance.cachedAccounts();
    expect(second[0].windows[0].usedPercent).toBe(20);
    expect(second[0].windows[0].modelIds).toEqual(['model-one']);
    expect(second[0].capabilities.antigravityHostIds).toEqual(['ubuntu']);
  });

  test('explicit cache invalidation clears usage without native calls or implicit recollection', async () => {
    await importFixtures();
    const instance = construct();
    await instance.getAccounts({ refresh: false });
    const beforeCalls = [...driverCalls];
    const beforeQuotaCount = quotaCalls.length;
    instance.invalidateUsage?.();
    expect(instance.cachedAccounts().map((account) => account.windows)).toEqual([[], []]);
    expect(driverCalls).toEqual(beforeCalls);
    expect(quotaCalls).toHaveLength(beforeQuotaCount);
  });

  test('construction rejects a non-Ubuntu driver without running it', () => {
    driver.hostId = 'mac' as AntigravitySwitchDriver['hostId'];
    expect(() => construct()).toThrow('Antigravity switching targets the Ubuntu CLI.');
    expect(driverCalls).toEqual([]);
    expect(quotaCalls).toEqual([]);
  });
});
