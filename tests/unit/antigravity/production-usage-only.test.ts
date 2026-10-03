import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import {
  ANTIGRAVITY_NATIVE_RELEASED,
  createInstalledAntigravityRuntimeFactory,
} from '../../../src/antigravity/production-runtime';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import type {
  AntigravityRuntime,
  SavedQuotaRequest,
  SavedQuotaSnapshot,
} from '../../../src/antigravity/runtime-composition';
import * as quotaTransport from '../../../src/antigravity/quota-worker-transport';
import * as nativeTransport from '../../../src/antigravity/native-credential-transport';
import * as nativeBridge from '../../../src/antigravity/ubuntu-runtime-bridge';
import * as nativeDriver from '../../../src/antigravity/ubuntu-driver';

// All identities, saved credentials and quota data below are invented. The only
// filesystem access is the disposable fixture; no worker/native process runs.
const PRIVATE = 'FIXTURE_ONLY_USAGE_NATIVE_SENTINEL';
const NOW = '2026-10-02T04:30:00.000Z';
const POOL_ID = 'fixture-provider-gemini-pool';
const PROFILES = ['gmail', 'party'] as const;
const NOW_MS = Date.parse(NOW);

function sample(request: SavedQuotaRequest): SavedQuotaSnapshot {
  const usedPercent = request.profileId === 'gmail' ? 35 : 15;
  const resetAt =
    request.profileId === 'gmail' ? '2026-10-02T06:30:00.000Z' : '2026-10-02T07:30:00.000Z';
  return {
    profileId: request.profileId,
    email: request.email,
    plan: request.identity.plan,
    identityKey: request.identityKey,
    credentialRevision: request.credentialRevision,
    source: 'native-consumer',
    identityVerified: true,
    identityValidation: 'verified',
    status: 'fresh',
    fetchedAt: NOW,
    sampledAt: NOW,
    windows: [
      {
        key: 'fixture-gemini-quota',
        label: 'Fixture Gemini model quota',
        kind: 'rate_limit',
        usedPercent,
        remainingPercent: 100 - usedPercent,
        resetAt,
        poolId: POOL_ID,
        poolIdSource: 'provider-id',
        modelIds: ['fixture-gemini-model'],
      },
    ],
    pools: [
      {
        id: POOL_ID,
        idSource: 'provider-id',
        eligibility: 'reported-quota',
        complete: true,
        windows: [
          {
            key: 'fixture-gemini-quota',
            kind: 'rate_limit',
            remainingPercent: 100 - usedPercent,
            resetAt,
          },
        ],
      },
    ],
  };
}

function snapshot(directory: string): string[] {
  const result: string[] = [];
  function visit(current: string): void {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const file = path.join(current, entry.name);
      if (entry.isDirectory()) visit(file);
      else
        result.push(
          path.relative(directory, file) +
            ':' +
            createHash('sha256').update(fs.readFileSync(file)).digest('hex')
        );
    }
  }
  visit(directory);
  return result.sort();
}

describe('installed Antigravity saved-profile usage without native descriptor', () => {
  let fixture: string;
  let ccs: string;
  let runtime: AntigravityRuntime | null;
  let registry: AntigravityProfileRegistry;
  let restore: Array<() => void>;
  let nativeCalls: string[];
  let quotaCalls: string[];
  let callbacks: Map<number, () => void>;
  let nextTimer: number;

  beforeEach(() => {
    fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-usage-only-'));
    ccs = path.join(fixture, '.ccs');
    runtime = null;
    registry = new AntigravityProfileRegistry(ccs);
    nativeCalls = [];
    quotaCalls = [];
    callbacks = new Map();
    nextTimer = 0;
    restore = [];
    const forbidden = (name: string) => () => {
      nativeCalls.push(name);
      throw new Error('Unexpected fixture native component construction.');
    };
    for (const spy of [
      spyOn(quotaTransport, 'createAntigravityQuotaWorker').mockImplementation(
        forbidden('quota-worker')
      ),
      spyOn(nativeTransport, 'createUbuntuNativeCredentialStore').mockImplementation(
        forbidden('native-store')
      ),
      spyOn(nativeBridge, 'createUbuntuRuntimeBridge').mockImplementation(
        forbidden('native-bridge')
      ),
      spyOn(nativeDriver, 'createUbuntuAntigravityDriver').mockImplementation(
        forbidden('native-driver')
      ),
    ])
      restore.push(() => spy.mockRestore());
  });

  afterEach(() => {
    runtime?.stop();
    for (const reset of restore) reset();
    fs.rmSync(fixture, { recursive: true, force: true });
  });

  async function saveProfiles(): Promise<void> {
    await registry.withLock(async () => {
      for (const id of PROFILES) {
        registry.saveCredential(
          id,
          'ubuntu',
          {
            format: 'fixture-native-json',
            bytes: Buffer.from(JSON.stringify({ profile: id, private: PRIVATE })),
          },
          {
            email: `${id}@example.test`,
            subject: `fixture-authoritative-${id}`,
            source: 'provider-userinfo',
            plan: 'Fixture Google plan',
            verifiedAt: NOW,
          },
          NOW_MS
        );
      }
    });
  }

  function factory(
    collect = async (request: SavedQuotaRequest): Promise<SavedQuotaSnapshot> => {
      quotaCalls.push(request.profileId);
      expect(request.credential.bytes.toString()).toContain(PRIVATE);
      return sample(request);
    }
  ) {
    return createInstalledAntigravityRuntimeFactory({
      home: fixture,
      collectQuota: collect,
      now: () => NOW_MS,
      setTimer: (callback) => {
        const id = ++nextTimer;
        callbacks.set(id, callback);
        return id;
      },
      clearTimer: (id) => {
        callbacks.delete(id as number);
      },
    });
  }

  function noNativeInstallation(): void {
    expect(fs.existsSync(path.join(ccs, 'antigravity-switching/runtime-installation.json'))).toBe(
      false
    );
    expect(fs.existsSync(path.join(ccs, 'antigravity-runtime'))).toBe(false);
    expect(nativeCalls).toEqual([]);
    expect(ANTIGRAVITY_NATIVE_RELEASED).toBe(true);
  }

  test('two saved profiles expose isolated quota/reset/pool samples with activation disabled', async () => {
    await saveProfiles();
    const before = snapshot(ccs);
    runtime = factory()(ccs);
    expect(runtime).not.toBeNull();
    expect(runtime!.hasProfiles()).toBe(true);
    const inventory = await runtime!.getInventory();
    expect(inventory.activationSupported).toBe(false);
    expect(
      inventory.profiles.map((profile) => [
        profile.id,
        profile.email,
        profile.selected,
        profile.runtimeVerified,
      ])
    ).toEqual([
      ['gmail', 'gmail@example.test', false, false],
      ['party', 'party@example.test', false, false],
    ]);
    const accounts = await runtime!.getAccounts({ refresh: true });
    expect(accounts.map((account) => account.id)).toEqual([
      'antigravity:profile:gmail',
      'antigravity:profile:party',
    ]);
    expect(accounts.map((account) => account.email)).toEqual([
      'gmail@example.test',
      'party@example.test',
    ]);
    expect(accounts.map((account) => account.status)).toEqual(['ok', 'ok']);
    expect(accounts.map((account) => account.windows[0].usedPercent)).toEqual([35, 15]);
    expect(accounts.map((account) => account.windows[0].resetAt)).toEqual([
      '2026-10-02T06:30:00.000Z',
      '2026-10-02T07:30:00.000Z',
    ]);
    expect(
      accounts.every((account) => account.sampledAt === NOW && account.fetchedAt === NOW)
    ).toBe(true);
    expect(
      accounts.every(
        (account) =>
          account.windows[0].poolId === POOL_ID && account.windows[0].poolIdSource === 'provider-id'
      )
    ).toBe(true);
    expect(
      accounts.every(
        (account) =>
          account.capabilities.antigravityCanActivate === false && account.isActive === false
      )
    ).toBe(true);
    expect(quotaCalls).toEqual(['gmail', 'party']);
    const output = JSON.stringify({ inventory, accounts });
    for (const value of [
      PRIVATE,
      'identityKey',
      'credentialRevision',
      'credentialFingerprint',
      'fixture-authoritative-',
    ])
      expect(output).not.toContain(value);
    accounts[0].windows[0].modelIds!.push('mutated-public-copy');
    expect(runtime!.cachedAccounts()[0].windows[0].modelIds).toEqual(['fixture-gemini-model']);
    expect(snapshot(ccs)).toEqual(before);
    noNativeInstallation();
  });

  test('manual and enabled automatic switching cannot touch native auth or processes', async () => {
    await saveProfiles();
    await registry.withLock(async () => {
      registry.beginTransaction('party', 'gmail', NOW_MS);
      registry.completeTransaction('gmail', NOW);
    });
    const beforeProfiles = snapshot(path.join(ccs, 'antigravity-profiles'));
    const beforeCredentials = snapshot(path.join(ccs, 'antigravity-instances'));
    runtime = factory()(ccs);
    expect(
      (await runtime!.activate({ profileId: 'party', hostId: 'ubuntu', mode: 'manual' })).status
    ).toBe('unsupported-runtime-probe');
    await expect(
      runtime!.activate({ profileId: 'party', hostId: 'ubuntu', mode: 'automatic' })
    ).rejects.toThrow('Invalid manual activation request.');
    expect(await runtime!.readSelectedProfileId()).toBeNull();
    runtime!.updateAutoSwitchSettings({
      enabled: true,
      requestedPoolId: POOL_ID,
      thresholdUsedPercent: 95,
    });
    runtime!.start();
    const callback = callbacks.values().next().value;
    expect(typeof callback).toBe('function');
    callbacks.clear();
    callback!();
    await new Promise<void>((resolve) => setImmediate(resolve));
    const status = runtime!.getAutoSwitchStatus();
    expect(status.enabled).toBe(true);
    expect(status.outcome).toBe('setup_required');
    expect(status.activationInProgress).toBe(false);
    expect(status.lastSwitchedAt).toBeUndefined();
    expect(snapshot(path.join(ccs, 'antigravity-profiles'))).toEqual(beforeProfiles);
    expect(snapshot(path.join(ccs, 'antigravity-instances'))).toEqual(beforeCredentials);
    noNativeInstallation();
  });

  test('a mismatched saved-account quota cannot leak or become another profile sample', async () => {
    await saveProfiles();
    runtime = factory(async (request) => ({
      ...sample(request),
      email: request.profileId === 'party' ? 'other@example.test' : request.email,
    }))(ccs);
    const accounts = await runtime!.getAccounts({ refresh: true });
    expect(accounts[0].status).toBe('ok');
    expect(accounts[1].email).toBe('party@example.test');
    expect(accounts[1].status).toBe('needs_sign_in');
    expect(accounts[1].windows).toEqual([]);
    expect(JSON.stringify(accounts)).not.toContain(PRIVATE);
    noNativeInstallation();
  });

  test('the default usage-only path selects the packaged quota worker without native adapters', async () => {
    await saveProfiles();
    const worker = spyOn(quotaTransport, 'createAntigravityQuotaWorker').mockImplementation(() => ({
      collectQuota: async (request) => {
        quotaCalls.push(request.profileId);
        return sample(request);
      },
      validateCredential: async () => {
        throw new Error('Fixture usage must not validate or adopt native credentials.');
      },
    }));
    runtime = createInstalledAntigravityRuntimeFactory({ home: fixture, now: () => NOW_MS })(ccs);
    expect(
      (await runtime!.getAccounts({ refresh: true })).map((account) => account.status)
    ).toEqual(['ok', 'ok']);
    expect(worker).toHaveBeenCalledTimes(1);
    expect(quotaCalls).toEqual(['gmail', 'party']);
    noNativeInstallation();
  });

  test('missing or empty saved inventory preserves the legacy fallback without starting workers', () => {
    expect(factory()(ccs)).toBeNull();
    const empty = path.join(fixture, 'empty-ccs');
    fs.mkdirSync(empty, { mode: 0o700 });
    expect(factory()(empty)).toBeNull();
    expect(fs.readdirSync(empty)).toEqual([]);
    expect(quotaCalls).toEqual([]);
    noNativeInstallation();
  });
});
