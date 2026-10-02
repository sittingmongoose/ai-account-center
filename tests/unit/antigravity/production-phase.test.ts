import { afterEach, expect, test } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AntigravityProfileRegistry,
  AntigravitySwitchService,
  credentialFingerprint,
  identityKey,
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
import { updateManagedAntigravity } from '../../../src/antigravity/managed-updater';

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
  private proofFailed = false;
  approved = true;
  quiesced = true;
  approveCalls = 0;
  approveHook?: (count: number) => boolean;
  completeThrows = false;
  async approveOwnedIdlePlan(): Promise<boolean> {
    this.events.push('approve');
    this.approveCalls++;
    return this.approveHook?.(this.approveCalls) ?? this.approved;
  }
  async revalidateQuiescedStop() {
    this.events.push('quiesced');
    return this.quiesced
      ? {
          hostId: 'ubuntu' as const,
          available: true,
          complete: true,
          busy: false,
          manualActivationInProgress: false,
          sampledAt: new Date(NOW).toISOString(),
        }
      : null;
  }
  async completeActivation() {
    this.events.push('complete');
    if (this.completeThrows) throw new Error('lost commit response');
  }
  async canRestartSupportedNative() {
    return true;
  }

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

test('owned idle auto stop rechecks exact private plan and final quiesced quota before install', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  const phases: string[] = [];
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async (context) => {
      phases.push(context.phase ?? 'unknown');
      if (context.phase === 'before-install') expect(context.quiescedHost?.available).toBe(true);
      return true;
    },
  });
  expect(result.status).toBe('active');
  expect(driver.approveCalls).toBe(2);
  expect(phases).toEqual(['before-stop', 'before-stop', 'before-install']);
  expect(driver.events.indexOf('quiesced')).toBeLessThan(driver.events.indexOf('install'));
  expect(driver.events.indexOf('runtime-proof')).toBeLessThan(driver.events.indexOf('complete'));
});

for (const refusal of [
  'queued-input',
  'changed-generation',
  'wrong-account',
  'wrong-revision',
  'wrong-CID',
  'foreign-PTY',
  'unmanaged-process',
  'unrestorable-args',
]) {
  test(`transaction obeys injected ${refusal} idle refusal before stop/install`, async () => {
    const { driver, service } = await setup();
    driver.plan = runningPlan();
    driver.approved = false;
    const result = await service.activate({
      profileId: 'second',
      hostId: 'ubuntu',
      mode: 'automatic',
      expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
      revalidateAutomatic: async () => true,
    });
    expect(result.status).toBe('busy');
    expect(driver.events).not.toContain('stop');
    expect(driver.events).not.toContain('install');
  });
}

test('idle changes at final locked approval are deferred before stop', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  driver.approveHook = (count) => count === 1;
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => true,
  });
  expect(result.status).toBe('busy');
  expect(driver.events).not.toContain('stop');
});

test('final policy rejection resumes proves and commits previous session before deferred', async () => {
  const { driver, service, registry } = await setup();
  driver.plan = runningPlan();
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async (context) => context.phase !== 'before-install',
  });
  expect(result.status).toBe('deferred');
  expect(driver.events).not.toContain('install');
  expect(driver.events).toContain('restart');
  expect(driver.events).toContain('runtime-proof');
  expect(driver.events).toContain('complete');
  expect(registry.hasRecovery()).toBe(false);
});

test('missing quiesced receipt proof resumes prior through recovery without installing target', async () => {
  const { driver, service } = await setup();
  driver.plan = runningPlan();
  driver.quiesced = false;
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => true,
  });
  expect(result.status).toBe('failed-rolled-back');
  expect(driver.events).not.toContain('install');
  expect(driver.events).toContain('restart');
  expect(driver.events).toContain('complete');
});

test('durable completion failure never releases foreground before exact prior recovery completes', async () => {
  const { driver, service, registry } = await setup();
  driver.plan = runningPlan();
  const actual = registry.completeTransaction.bind(registry);
  let count = 0;
  registry.completeTransaction = (id, at) => {
    if (++count === 1) throw new Error('fixture durable failure');
    actual(id, at);
  };
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => true,
  });
  expect(result.status).toBe('failed-rolled-back');
  expect(driver.events.filter((event) => event === 'complete')).toHaveLength(1);
  expect(driver.events.indexOf('rollback')).toBeLessThan(driver.events.indexOf('complete'));
});

test('lost completion acknowledgement retains recovery and never stops potentially resumed target', async () => {
  const { driver, service, registry } = await setup();
  driver.plan = runningPlan();
  driver.completeThrows = true;
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async () => true,
  });
  expect(result.status).toBe('recovery-required');
  expect(registry.hasRecovery()).toBe(true);
  expect(driver.events).not.toContain('stop-owned');
  expect(driver.events).not.toContain('rollback');
});

test('failed deferred abort completes no foreground release until prior recovery is durable', async () => {
  const { driver, service, registry } = await setup();
  driver.plan = runningPlan();
  registry.abortTransaction = () => {
    throw new Error('fixture abort failure');
  };
  const result = await service.activate({
    profileId: 'second',
    hostId: 'ubuntu',
    mode: 'automatic',
    expectedActiveIdentityKey: identityKey(fixtureIdentity(driver.current)),
    revalidateAutomatic: async (context) => context.phase !== 'before-install',
  });
  expect(result.status).toBe('failed-rolled-back');
  expect(driver.events.filter((event) => event === 'complete')).toHaveLength(1);
  expect(driver.events.indexOf('stop-owned')).toBeLessThan(driver.events.indexOf('complete'));
});

test('closed installed CLI can update without fake live identity or restarting any session', async () => {
  const { driver, registry } = await setup();
  driver.canProbe = false;
  const events: string[] = [];
  let version = '1.2.14';
  const result = await updateManagedAntigravity({
    registry,
    driver,
    native: {
      version: async () => version,
      backup: async () => {
        events.push('backup');
      },
      update: async () => {
        events.push('update');
        version = '1.2.15';
      },
      rollback: async () => false,
    },
  });
  expect(result).toMatchObject({
    status: 'updated',
    version: '1.2.15',
    restartedProcesses: 0,
    updateAttempted: true,
  });
  expect(events).toEqual(['backup', 'update']);
  expect(driver.events).not.toContain('stop');
  expect(driver.events).not.toContain('runtime-proof');
});

test('closed updater second census refuses newly opened CLI with no update attempt', async () => {
  const { driver, registry } = await setup();
  driver.inspectHook = (count) => (count === 1 ? idlePlan() : runningPlan());
  let attempts = 0;
  const result = await updateManagedAntigravity({
    registry,
    driver,
    native: {
      version: async () => '1.2.14',
      backup: async () => {},
      update: async () => {
        attempts++;
      },
      rollback: async () => false,
    },
  });
  expect(result.updateAttempted).toBe(false);
  expect(attempts).toBe(0);
});

test('no-op managed update relies on static native support while stopped then fresh running proof', async () => {
  const { driver, registry } = await setup();
  driver.plan = runningPlan();
  driver.canProveRuntimeIdentity = async () => driver.plan.processes.length > 0;
  const restart = driver.restartProcesses.bind(driver);
  driver.restartProcesses = async (receipt) => {
    await restart(receipt);
    driver.plan = runningPlan('new-birth');
  };
  const result = await updateManagedAntigravity({
    registry,
    driver,
    native: {
      version: async () => '1.2.14',
      backup: async () => {},
      update: async () => {
        expect(await driver.canProveRuntimeIdentity()).toBe(false);
      },
      rollback: async () => true,
    },
  });
  expect(result.status).toBe('current');
  expect(result.updateAttempted).toBe(true);
  expect(driver.events).toContain('restart');
  expect(driver.events).toContain('runtime-proof');
  expect(await driver.canProveRuntimeIdentity()).toBe(true);
});

test('unsupported changed native pin retains backup/recovery and refuses foreign executable rollback', async () => {
  const { driver, registry } = await setup();
  driver.plan = runningPlan();
  driver.canRestartSupportedNative = async () => false;
  let rollback = 0;
  const result = await updateManagedAntigravity({
    registry,
    driver,
    native: {
      version: async () => '1.2.14',
      backup: async () => {},
      update: async () => {},
      rollback: async () => {
        rollback++;
        return false;
      },
    },
  });
  expect(result.status).toBe('restart_failed');
  expect(result.updateAttempted).toBe(true);
  expect(rollback).toBe(1);
  expect(driver.events).not.toContain('restart');
  expect(registry.hasRecovery()).toBe(true);
});
