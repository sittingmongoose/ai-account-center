import { afterEach, expect, test } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
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
import { AntigravityError } from '../../../src/antigravity/errors';

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
  proveUnavailable = false;
  installThrows = false;
  validateHook?: (value: NativeCredential) => VerifiedIdentity;
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
    return this.validateHook?.(value) ?? fixtureIdentity(value);
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
    if (this.installThrows) throw new Error('SECRET install output should never escape');
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
    // The live broker raises exactly this when no CLI session exists to
    // prove from (the idle path): no owned probe is ever started.
    if (this.proveUnavailable) throw new AntigravityError('antigravity-runtime-unavailable');
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
  async completeActivation(expected: VerifiedIdentity): Promise<void> {
    this.events.push(`complete:${expected.email}`);
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

async function stuckSwitch(live: NativeCredential) {
  const result = await setup();
  result.driver.current = live;
  result.driver.events = [];
  await result.registry.withLock(async () => {
    result.registry.beginTransaction('second', 'first', NOW);
    result.registry.markRecovery();
  });
  return result;
}

function savedRevisions(registry: AntigravityProfileRegistry): string {
  return ['first', 'second']
    .map((id) => registry.readCredential(id, 'ubuntu').credentialRevision)
    .join('|');
}

test('recover completes the stuck switch when the live login is the target', async () => {
  const { registry, driver, service } = await stuckSwitch(credential('second@example.com'));
  driver.proveUnavailable = true;
  const before = savedRevisions(registry);
  const response = await service.recover();
  expect(response.status).toBe('completed');
  expect(response.profileId).toBe('second');
  expect(response.email).toBe('second@example.com');
  expect(registry.hasRecovery()).toBe(false);
  expect(service.listProfiles().find((profile) => profile.id === 'second')?.hosts[0].active).toBe(
    true
  );
  expect(savedRevisions(registry)).toBe(before);
  expect(JSON.stringify(response)).not.toContain('fixture-only-secret');
});

test('recover aborts the stuck switch when the live login is the previous account', async () => {
  const { registry, service } = await stuckSwitch(credential('first@example.com'));
  const before = savedRevisions(registry);
  const response = await service.recover();
  expect(response.status).toBe('aborted');
  expect(response.profileId).toBe('first');
  expect(registry.hasRecovery()).toBe(false);
  // Abort clears only the stuck transaction; the active record is untouched.
  expect(service.listProfiles().every((profile) => profile.hosts[0].active === false)).toBe(true);
  expect(savedRevisions(registry)).toBe(before);
});

test('recover restores the previous saved revision when the live login is unknown', async () => {
  const { registry, driver, service } = await stuckSwitch(credential('unknown@example.com'));
  const before = savedRevisions(registry);
  const response = await service.recover();
  expect(response.status).toBe('restored-previous');
  expect(response.profileId).toBe('first');
  expect(registry.hasRecovery()).toBe(false);
  expect(
    driver.current.bytes.equals(registry.readCredential('first', 'ubuntu').credential.bytes)
  ).toBe(true);
  expect(savedRevisions(registry)).toBe(before);
});

test('recover stays stuck when the previous saved revision no longer verifies', async () => {
  const { registry, driver, service } = await stuckSwitch(credential('unknown@example.com'));
  const saved = registry.readCredential('first', 'ubuntu').credential;
  driver.validateHook = (value) =>
    value.bytes.equals(saved.bytes)
      ? { ...fixtureIdentity(value), subject: 'fixture-subject:changed' }
      : fixtureIdentity(value);
  const response = await service.recover();
  expect(response.status).toBe('recovery-required');
  expect(registry.hasRecovery()).toBe(true);
  expect(fixtureIdentity(driver.current).email).toBe('unknown@example.com');
});

test('recover stays stuck when the restore write fails', async () => {
  const { registry, driver, service } = await stuckSwitch(credential('unknown@example.com'));
  driver.installThrows = true;
  const response = await service.recover();
  expect(response.status).toBe('recovery-required');
  expect(registry.hasRecovery()).toBe(true);
  expect(JSON.stringify(response)).not.toContain('SECRET');
});

test('recover is a no-op when no switch is stuck', async () => {
  const { registry, driver, service } = await setup();
  driver.proveUnavailable = true;
  const response = await service.recover();
  expect(response.status).toBe('no-recovery-pending');
  expect(registry.hasRecovery()).toBe(false);
  expect(driver.events).toEqual([]);
});

test('inventory distinguishes selected native credential from unproven running account', async () => {
  const { service } = await setup();
  const first = (await service.readInventory()).find((profile) => profile.id === 'first')!;
  expect(first.hosts[0].selected).toBe(true);
  expect(first.hosts[0].active).toBe(false);
  expect(first.hosts[0].verification).toBe('stored-only');
});

test('cross-process lock excludes another registry instance and is never silently broken', async () => {
  const { directory, registry } = await setup();
  const other = new AntigravityProfileRegistry(path.join(directory, '.ccs'));
  await registry.withLock(async () => {
    await expect(other.withLock(async () => undefined)).rejects.toMatchObject({
      code: 'busy',
    });
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
  await expect(
    service.importNativeProfile('third', credential('third@example.com'))
  ).rejects.toThrow();
  expect(fs.readdirSync(registryDirectory)).toEqual(before);
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
