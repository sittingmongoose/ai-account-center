import { afterEach, expect, test } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AntigravityConfirmationStore,
  AntigravityProfileRegistry,
  AntigravitySwitchService,
  credentialFingerprint,
  identityKey,
  PrivateStorageError,
} from '../../../src/antigravity';
import type {
  AntigravitySwitchDriver,
  InstallReceipt,
  NativeCredential,
  ProcessPlan,
  RuntimeProof,
  StopReceipt,
  VerifiedIdentity,
} from '../../../src/antigravity';

const NOW = Date.parse('2026-10-01T18:00:00Z');
const temporary: string[] = [];
afterEach(() => {
  for (const directory of temporary.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

function credential(email: string, revision = 'original'): NativeCredential {
  return {
    format: 'fixture-native-v1',
    bytes: Buffer.from(
      JSON.stringify({
        email,
        revision,
        secret: `fixture-only-secret-${email}-${revision}`,
      })
    ),
  };
}

function fixtureIdentity(
  value: NativeCredential,
  source: VerifiedIdentity['source'] = 'provider-userinfo'
): VerifiedIdentity {
  const { email } = JSON.parse(value.bytes.toString('utf8'));
  return {
    email,
    subject: `fixture-subject:${email}`,
    plan: 'fixture-plan',
    verifiedAt: new Date(NOW).toISOString(),
    source,
  };
}

function runningPlan(birth = 'fixture-birth-1', checkpoint = 'b'.repeat(64)): ProcessPlan {
  return {
    complete: true,
    continuity: { fingerprint: checkpoint, restorable: true },
    processes: [
      {
        role: 'cli',
        identity: {
          pid: 41001,
          startTime: birth,
          ownerId: 'fixture-owner',
          fingerprint: 'a'.repeat(64),
        },
      },
    ],
  };
}

function idlePlan(): ProcessPlan {
  return { complete: true, processes: [], continuity: null };
}

class FixtureDriver implements AntigravitySwitchDriver {
  hostId = 'ubuntu' as const;
  current = credential('first@example.com');
  plan = idlePlan();
  events: string[] = [];
  canProbe = true;
  restoreSession = true;
  wrongStoredIdentity = false;
  wrongRuntimeIdentity = false;
  runtimeSource: VerifiedIdentity['source'] = 'native-runtime';
  stopThrows = false;
  partialStop = false;
  rollbackAllowed = true;
  installThrowsAfterWrite = false;
  afterStopCredential?: NativeCredential;
  inspectHook?: (count: number) => ProcessPlan;
  inspections = 0;
  runtimeHook?: (expected: VerifiedIdentity) => void;
  approveOwnedIdlePlan?: AntigravitySwitchDriver['approveOwnedIdlePlan'];
  private proofFailed = false;

  async canProveRuntimeIdentity(): Promise<boolean> {
    this.events.push('capability');
    return this.canProbe;
  }
  async readCurrentCredential(): Promise<NativeCredential> {
    this.events.push('read');
    return this.current;
  }
  async validateCredential(value: NativeCredential): Promise<VerifiedIdentity> {
    this.events.push('validate');
    return fixtureIdentity(value);
  }
  async inspectProcesses(): Promise<ProcessPlan> {
    this.events.push('census');
    this.inspections++;
    return this.inspectHook?.(this.inspections) ?? this.plan;
  }
  async stopProcesses(plan: ProcessPlan): Promise<StopReceipt> {
    this.events.push('stop');
    if (this.stopThrows) throw new Error('SECRET driver output should never escape');
    this.plan = idlePlan();
    if (this.afterStopCredential) this.current = this.afterStopCredential;
    return {
      stopped: this.partialStop ? [] : plan.processes.map((entry) => entry.identity),
      restartState: { fixture: true },
      complete: !this.partialStop,
    };
  }
  async installCredential(value: NativeCredential, expected: string): Promise<InstallReceipt> {
    this.events.push('install');
    if (credentialFingerprint(this.current) !== expected)
      throw new Error('Foreign credential changed');
    this.current = value;
    if (this.installThrowsAfterWrite) throw new Error('SECRET after partial write');
    return {
      installedFingerprint: credentialFingerprint(value),
      rollbackState: { fixture: true },
    };
  }
  async readStoredIdentity(): Promise<VerifiedIdentity> {
    this.events.push('stored-identity');
    return fixtureIdentity(
      this.wrongStoredIdentity ? credential('wrong@example.com') : this.current
    );
  }
  async restartProcesses(_receipt: StopReceipt): Promise<void> {
    this.events.push('restart');
  }
  async proveRuntimeIdentity(expected: VerifiedIdentity): Promise<RuntimeProof> {
    this.events.push('runtime-proof');
    this.runtimeHook?.(expected);
    const wrong = this.wrongRuntimeIdentity && !this.proofFailed;
    this.proofFailed = wrong;
    return {
      identity: fixtureIdentity(
        wrong ? credential('wrong@example.com') : this.current,
        this.runtimeSource
      ),
      credentialFingerprint: credentialFingerprint(this.current),
      runtimeStarted: true,
      sessionRestored: this.restoreSession,
    };
  }
  async rollbackCredential(_receipt: InstallReceipt, previous: NativeCredential): Promise<boolean> {
    this.events.push('rollback');
    if (this.rollbackAllowed) this.current = previous;
    return this.rollbackAllowed;
  }
  async stopOwnedRestarts(): Promise<void> {
    this.events.push('stop-owned');
  }
}

async function setup(): Promise<{
  directory: string;
  registry: AntigravityProfileRegistry;
  driver: FixtureDriver;
  service: AntigravitySwitchService;
}> {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-transaction-'));
  temporary.push(directory);
  const registry = new AntigravityProfileRegistry(path.join(directory, '.ccs'));
  const driver = new FixtureDriver();
  const service = new AntigravitySwitchService({
    registry,
    driver,
    now: () => NOW,
  });
  await service.importNativeProfile('first', driver.current);
  await service.importNativeProfile('second', credential('second@example.com'));
  driver.events = [];
  return { directory, registry, driver, service };
}

test('idle manual switch proves running identity, preserves current backup and exposes only safe fields', async () => {
  const { registry, driver, service } = await setup();
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(response.status).toBe('active');
  expect(response.email).toBe('second@example.com');
  expect(driver.events).not.toContain('stop');
  expect(driver.events).toContain('runtime-proof');
  expect(
    registry
      .readCredential('first', 'ubuntu')
      .credential.bytes.equals(credential('first@example.com').bytes)
  ).toBe(true);
  expect(service.listProfiles().find((profile) => profile.id === 'second')?.hosts[0].active).toBe(
    true
  );
  expect(JSON.stringify(response)).not.toContain('fixture-only-secret');
});

test('running manual switch first requires review without stopping or writing', async () => {
  const { directory, driver, service } = await setup();
  driver.plan = runningPlan();
  const before = fs.readdirSync(path.join(directory, '.ccs', 'antigravity-profiles'));
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(response.status).toBe('confirmation-required');
  expect(response.confirmation?.processes).toEqual([
    { pid: 41001, role: 'cli', label: 'Antigravity CLI' },
  ]);
  expect(driver.events).not.toContain('install');
  expect(driver.events).not.toContain('stop');
  expect(fs.readdirSync(path.join(directory, '.ccs', 'antigravity-profiles'))).toEqual(before);
  expect(JSON.stringify(response)).not.toMatch(
    /startTime|ownerId|fingerprint|fixture-only-secret|restartState/
  );
});

test('confirmed switch checkpoints and resumes exact session; native shutdown refresh is backed up', async () => {
  const { registry, driver, service } = await setup();
  driver.plan = runningPlan();
  driver.afterStopCredential = credential('first@example.com', 'shutdown-refresh');
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
    confirmationToken: review.confirmation!.token,
  });
  expect(response.status).toBe('active');
  expect(driver.events.indexOf('stop')).toBeLessThan(driver.events.indexOf('install'));
  expect(driver.events.indexOf('install')).toBeLessThan(driver.events.indexOf('restart'));
  expect(
    registry
      .readCredential('first', 'ubuntu')
      .credential.bytes.equals(driver.afterStopCredential.bytes)
  ).toBe(true);
});

test.each(['pid-birth', 'checkpoint', 'new-writer'] as const)(
  'confirmation rejects changed %s and is consumed once',
  async (change) => {
    const { driver, service } = await setup();
    driver.plan = runningPlan();
    const review = await service.activate({
      profileId: 'second',
      hostId: 'ubuntu',
      mode: 'manual',
    });
    if (change === 'pid-birth') driver.plan = runningPlan('fixture-birth-2');
    if (change === 'checkpoint') driver.plan = runningPlan('fixture-birth-1', 'c'.repeat(64));
    if (change === 'new-writer') driver.plan = idlePlan();
    const request = {
      profileId: 'second',
      hostId: 'ubuntu' as const,
      mode: 'manual' as const,
      confirmationToken: review.confirmation!.token,
    };
    expect((await service.activate(request)).status).toBe('stale-confirmation');
    driver.plan = runningPlan();
    expect((await service.activate(request)).status).toBe('stale-confirmation');
    expect(driver.events).not.toContain('stop');
    expect(driver.events).not.toContain('install');
  }
);

test('native/runtime identity verification must be supported before any action', async () => {
  const { driver, service } = await setup();
  driver.canProbe = false;
  driver.plan = runningPlan();
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('unsupported-runtime-probe');
  expect(driver.events).not.toContain('stop');
  expect(driver.events).not.toContain('install');
});

test('nonrestorable exact session or unknown writers cannot receive a stop confirmation', async () => {
  const { driver, service } = await setup();
  driver.plan = {
    ...runningPlan(),
    continuity: { fingerprint: 'c'.repeat(64), restorable: false },
  };
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('busy');
  driver.plan = { ...idlePlan(), complete: false };
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).reason
  ).toBe('unreviewed-processes');
});

test('automatic switching never stops a running process or consumes manual permission', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  let callbacks = 0;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => {
      callbacks++;
      return true;
    },
  });
  expect(response.status).toBe('busy');
  expect(callbacks).toBe(0);
  expect(driver.events).not.toContain('stop');
  expect(driver.events).not.toContain('install');
});

test('automatic activation rejects missing or stale decision identity and callback', async () => {
  const { driver, service } = await setup();
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'automatic',
      })
    ).status
  ).toBe('deferred');
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'automatic',
        expectedActiveIdentityKey: 'wrong',
        revalidateAutomatic: async () => true,
      })
    ).reason
  ).toBe('active-identity-changed');
  expect(driver.events).not.toContain('install');
});

test('automatic policy is revalidated immediately before install and late disable cancels without an active claim', async () => {
  const { registry, driver, service } = await setup();
  let checks = 0;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => ++checks < 3,
  });
  expect(response.status).toBe('deferred');
  expect(checks).toBe(3);
  expect(driver.events).not.toContain('install');
  expect(registry.hasRecovery()).toBe(false);
  expect(service.listProfiles().some((profile) => profile.hosts[0].active)).toBe(false);
});

test('automatic idle switch rechecks settings under lock and does not terminate anything', async () => {
  const { driver, service } = await setup();
  let checks = 0;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => {
      checks++;
      driver.events.push('policy');
      return true;
    },
  });
  expect(response.status).toBe('active');
  expect(checks).toBe(3);
  expect(driver.events[driver.events.indexOf('install') - 1]).toBe('policy');
  expect(driver.events).not.toContain('stop');
});

test('a new writer after review invalidates confirmation before stopping or writing', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  driver.inspectHook = (count) => (count === 2 ? runningPlan() : runningPlan('new-birth'));
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
    confirmationToken: review.confirmation!.token,
  });
  expect(response.status).toBe('stale-confirmation');
  expect(driver.events).not.toContain('stop');
});

test('wrong stored identity rolls back exact credential and never claims the target active', async () => {
  const { registry, driver, service } = await setup();
  driver.wrongStoredIdentity = true;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(response.status).toBe('failed-rolled-back');
  expect(fixtureIdentity(driver.current).email).toBe('first@example.com');
  expect(registry.hasRecovery()).toBe(false);
  expect(service.listProfiles().find((profile) => profile.id === 'second')?.hosts[0].active).toBe(
    false
  );
});

test('a keyring/file identity alone cannot satisfy runtime proof', async () => {
  const { registry, driver, service } = await setup();
  driver.runtimeSource = 'provider-userinfo';
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(response.status).toBe('recovery-required');
  expect(registry.hasRecovery()).toBe(true);
});

test('failed exact session restoration is recovery-required even if credential rollback succeeds', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  driver.restoreSession = false;
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
        confirmationToken: review.confirmation!.token,
      })
    ).status
  ).toBe('recovery-required');
});

test('foreign credential replacement is preserved and blocks subsequent switches', async () => {
  const { registry, driver, service } = await setup();
  driver.wrongRuntimeIdentity = true;
  driver.rollbackAllowed = false;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(response.status).toBe('recovery-required');
  expect(response.reason).toBe('foreign-replacement');
  expect(fixtureIdentity(driver.current).email).toBe('second@example.com');
  expect(registry.hasRecovery()).toBe(true);
  const before = driver.events.length;
  expect(
    (
      await service.activate({
        profileId: 'first',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('recovery-required');
  expect(driver.events.length).toBe(before);
});

test('a native install throwing after write does not invent a rollback receipt or report success', async () => {
  const { registry, driver, service } = await setup();
  driver.installThrowsAfterWrite = true;
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('recovery-required');
  expect(driver.events).not.toContain('rollback');
  expect(registry.hasRecovery()).toBe(true);
});

test('unknown partial stop is recovery-required; private driver errors do not escape', async () => {
  const { registry, driver, service } = await setup();
  driver.plan = runningPlan();
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  driver.stopThrows = true;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
    confirmationToken: review.confirmation!.token,
  });
  expect(response.status).toBe('recovery-required');
  expect(JSON.stringify(response)).not.toContain('SECRET');
  expect(driver.events).not.toContain('install');
  expect(registry.hasRecovery()).toBe(true);
});

test('same selected account requires runtime proof but never installs stale saved credentials', async () => {
  const { driver, service } = await setup();
  driver.current = credential('first@example.com', 'fresh-live');
  expect(
    (
      await service.activate({
        profileId: 'first',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('already-active');
  expect(driver.events).not.toContain('install');
  expect(fixtureIdentity(driver.current).email).toBe('first@example.com');
});

test('inventory distinguishes selected native credential from unproven running account', async () => {
  const { service } = await setup();
  const first = (await service.readInventory()).find((profile) => profile.id === 'first')!;
  expect(first.hosts[0].selected).toBe(true);
  expect(first.hosts[0].active).toBe(false);
  expect(first.hosts[0].verification).toBe('stored-only');
});

test('host scope is strictly Ubuntu; caller-supplied unsupported host performs no driver action', async () => {
  const { driver, service } = await setup();
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'macos' as 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('invalid-profile');
  expect(driver.events).toEqual([]);
});

test('cross-process lock excludes another registry instance and is never silently broken', async () => {
  const { directory, registry, service } = await setup();
  const other = new AntigravityProfileRegistry(path.join(directory, '.ccs'));
  await registry.withLock(async () => {
    await expect(other.withLock(async () => undefined)).rejects.toMatchObject({
      code: 'busy',
    });
    expect(
      (
        await service.activate({
          profileId: 'second',
          hostId: 'ubuntu',
          mode: 'manual',
        })
      ).reason
    ).toBe('activation-running');
  });
});

test('identity deduplication prevents a second profile ID or an existing profile from changing account', async () => {
  const { service } = await setup();
  await expect(
    service.importNativeProfile('duplicate', credential('FIRST@example.com'))
  ).rejects.toBeInstanceOf(PrivateStorageError);
  await expect(
    service.importNativeProfile('first', credential('third@example.com'))
  ).rejects.toBeInstanceOf(PrivateStorageError);
  expect(service.listProfiles()).toHaveLength(2);
});

test('profile IDs cannot traverse private storage and immutable files remain 600 with 700 directories', async () => {
  const { directory, service } = await setup();
  await expect(
    service.importNativeProfile('../escape', credential('third@example.com'))
  ).rejects.toBeInstanceOf(PrivateStorageError);
  const root = path.join(directory, '.ccs');
  for (const entry of [
    'antigravity-profiles',
    'antigravity-instances',
    'antigravity-instances/first',
    'antigravity-instances/first/ubuntu',
  ]) {
    expect(fs.statSync(path.join(root, entry)).mode & 0o777).toBe(0o700);
  }
  const filename = fs.readdirSync(path.join(root, 'antigravity-instances/first/ubuntu'))[0];
  expect(
    fs.statSync(path.join(root, 'antigravity-instances/first/ubuntu', filename)).mode & 0o777
  ).toBe(0o600);
});

test('symlinked registry ancestors and foreign credential/hardlink replacements fail closed', async () => {
  const { directory, registry } = await setup();
  const foreign = path.join(directory, 'foreign');
  fs.mkdirSync(foreign, { mode: 0o700 });
  fs.symlinkSync(foreign, path.join(directory, 'unsafe'));
  expect(() => new AntigravityProfileRegistry(path.join(directory, 'unsafe', 'nested'))).toThrow();
  const nativeDirectory = path.join(directory, '.ccs', 'antigravity-instances', 'first', 'ubuntu');
  const filename = path.join(nativeDirectory, fs.readdirSync(nativeDirectory)[0]);
  fs.linkSync(filename, path.join(foreign, 'foreign-hardlink'));
  expect(() => registry.readCredential('first', 'ubuntu')).toThrow();
  expect(fs.existsSync(path.join(foreign, 'foreign-hardlink'))).toBe(true);
});

test('corrupt newest registry is not treated as empty and no auth/account files are rewritten', async () => {
  const { directory, service } = await setup();
  const registryDirectory = path.join(directory, '.ccs', 'antigravity-profiles');
  fs.writeFileSync(path.join(registryDirectory, 'registry-000000000003.json'), '{corrupt fixture', {
    mode: 0o600,
  });
  const before = fs.readdirSync(registryDirectory);
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('invalid-profile');
  expect(fs.readdirSync(registryDirectory)).toEqual(before);
});

test('confirmation expiry and wrong target credential are one-shot and never authorize a later retry', async () => {
  const store = new AntigravityConfirmationStore();
  const binding = {
    profileId: 'second',
    currentIdentityKey: 'first',
    targetIdentityKey: 'second',
    credentialFingerprint: 'd'.repeat(64),
    planFingerprint: 'e'.repeat(64),
  };
  const expired = store.issue(binding, 'second@example.com', runningPlan(), NOW);
  expect(store.consume(expired.token, binding, NOW + 60_000)).toBe(false);
  expect(store.consume(expired.token, binding, NOW + 1000)).toBe(false);
  const changed = store.issue(binding, 'second@example.com', runningPlan(), NOW);
  expect(
    store.consume(changed.token, { ...binding, credentialFingerprint: 'f'.repeat(64) }, NOW)
  ).toBe(false);
  expect(store.consume(changed.token, binding, NOW)).toBe(false);
});

test('target credential refreshed after review requires fresh process approval', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  await service.importNativeProfile('second', credential('second@example.com', 'refreshed-target'));
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
        confirmationToken: review.confirmation!.token,
      })
    ).status
  ).toBe('stale-confirmation');
  expect(driver.events).not.toContain('stop');
  expect(driver.events).not.toContain('install');
});

test('unverified or stale provider identity never writes metadata or touches a program', async () => {
  const { directory, driver, service } = await setup();
  const registryDirectory = path.join(directory, '.ccs', 'antigravity-profiles');
  const before = fs.readdirSync(registryDirectory);
  driver.validateCredential = async (value) => ({
    ...fixtureIdentity(value),
    verifiedAt: new Date(NOW - 600_000).toISOString(),
  });
  expect(
    (
      await service.activate({
        profileId: 'second',
        hostId: 'ubuntu',
        mode: 'manual',
      })
    ).status
  ).toBe('invalid-profile');
  expect(fs.readdirSync(registryDirectory)).toEqual(before);
  expect(driver.events).not.toContain('stop');
  expect(driver.events).not.toContain('install');
});

test('foreign replacement of the lock directory is preserved during release', async () => {
  const { directory, registry } = await setup();
  const lock = path.join(directory, '.ccs', 'antigravity-profiles', '.transaction-lock');
  const displaced = `${lock}.displaced-fixture`;
  await expect(
    registry.withLock(async () => {
      fs.renameSync(lock, displaced);
      fs.mkdirSync(lock, { mode: 0o700 });
      fs.writeFileSync(path.join(lock, 'foreign-marker'), 'owned by foreign fixture', {
        mode: 0o600,
      });
    })
  ).rejects.toBeInstanceOf(PrivateStorageError);
  expect(fs.readFileSync(path.join(lock, 'foreign-marker'), 'utf8')).toBe(
    'owned by foreign fixture'
  );
  expect(fs.existsSync(displaced)).toBe(true);
});

test('replacement of the immutable credential directory is not adopted as owned storage', async () => {
  const { directory, registry } = await setup();
  const instances = path.join(directory, '.ccs', 'antigravity-instances');
  fs.renameSync(instances, `${instances}.displaced-fixture`);
  fs.mkdirSync(instances, { mode: 0o700 });
  expect(() => registry.readCredential('first', 'ubuntu')).toThrow();
  expect(fs.readdirSync(instances)).toEqual([]);
});

test('partial reviewed stop cannot proceed to install or return active', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  driver.partialStop = true;
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
    confirmationToken: review.confirmation!.token,
  });
  expect(response.status).toBe('failed-rolled-back');
  expect(driver.events).not.toContain('install');
});

test('a broker that cannot honour the stop is never offered as stop-and-switch', async () => {
  const { directory, driver, service } = await setup();
  driver.plan = runningPlan();
  const approvals: Array<{
    plan: ProcessPlan;
    expected: VerifiedIdentity;
    fingerprint: string;
  }> = [];
  driver.approveOwnedIdlePlan = async (plan, expected, fingerprint) => {
    approvals.push({ plan, expected, fingerprint });
    return false;
  };
  const registryDirectory = path.join(directory, '.ccs', 'antigravity-profiles');
  const before = fs.readdirSync(registryDirectory);
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(response.status).toBe('busy');
  expect(response.reason).toBe('running-processes');
  expect(response.confirmation).toBeUndefined();
  expect(approvals.length).toBe(1);
  expect(approvals[0].plan).toEqual(runningPlan());
  expect(approvals[0].expected.email).toBe('first@example.com');
  expect(approvals[0].fingerprint).toBe(credentialFingerprint(driver.current));
  expect(driver.events).not.toContain('stop');
  expect(driver.events).not.toContain('install');
  expect(fs.readdirSync(registryDirectory)).toEqual(before);
});

test('a broker that can honour the stop still offers review and completes the confirmed switch', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  let approvals = 0;
  driver.approveOwnedIdlePlan = async () => {
    approvals++;
    return true;
  };
  const review = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
  });
  expect(review.status).toBe('confirmation-required');
  expect(review.confirmation?.processes).toEqual([
    { pid: 41001, role: 'cli', label: 'Antigravity CLI' },
  ]);
  expect(approvals).toBe(1);
  const response = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'manual',
    confirmationToken: review.confirmation!.token,
  });
  expect(response.status).toBe('active');
  // One probe at offer time plus the re-approval under the confirmed switch.
  expect(approvals).toBe(2);
  expect(driver.events).toContain('stop');
});
