import { afterEach, describe, expect, test } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import {
  createManagedNativeUpdate,
  runManagedAntigravityUpdateCommand,
  type ManagedNativeCommand,
} from '../../../src/antigravity/managed-update-command';
import { updateManagedAntigravity } from '../../../src/antigravity/managed-updater';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import type { AntigravitySwitchDriver, ProcessPlan } from '../../../src/antigravity/types';

const ORIGINAL = Buffer.from('public invented native fixture version 0.4.1\n');
const REPLACEMENT = Buffer.from('public invented native fixture version 0.4.2\n');
const RAW_FAILURE = 'invented-raw-updater-diagnostic-sentinel';
const MAX_OUTPUT_BYTES = 65_536;
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

interface CommandCall {
  binary: string;
  args: string[];
  capture: boolean;
}

/** Public fixture bytes only. The fake binary is never executed. */
function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-native-update-fixture-'));
  directories.push(root);
  const ccsDirectory = path.join(root, 'fixture-state');
  const binDirectory = path.join(root, 'fixture-bin');
  fs.mkdirSync(ccsDirectory, { mode: 0o700 });
  fs.mkdirSync(binDirectory, { mode: 0o700 });
  const nativeBinary = path.join(binDirectory, 'fixture-agy');
  fs.writeFileSync(nativeBinary, ORIGINAL, { mode: 0o700 });
  const calls: CommandCall[] = [];
  let reportedVersion = '0.4.1';
  let updateAction: () => Promise<string> = async () => '';
  const runCommand: ManagedNativeCommand = async (binary, args, capture) => {
    calls.push({ binary, args: [...args], capture });
    if (args.length === 1 && args[0] === '--version' && capture)
      return `Fixture native updater ${reportedVersion}\n`;
    if (args.length === 1 && args[0] === 'update' && !capture) return updateAction();
    throw new Error('Unexpected fixture command.');
  };
  return {
    root,
    ccsDirectory,
    binDirectory,
    nativeBinary,
    calls,
    runCommand,
    native: createManagedNativeUpdate({ nativeBinary, ccsDirectory, runCommand }),
    setVersion: (version: string) => {
      reportedVersion = version;
    },
    setUpdate: (action: () => Promise<string>) => {
      updateAction = action;
    },
    replaceBinary: (bytes = REPLACEMENT) => {
      const incoming = path.join(binDirectory, 'fixture-incoming-binary');
      fs.writeFileSync(incoming, bytes, { mode: 0o700 });
      fs.renameSync(incoming, nativeBinary);
    },
  };
}

function sha(bytes: Buffer): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function backupPath(value: ReturnType<typeof fixture>): string {
  return path.join(value.ccsDirectory, 'antigravity-switching/native-backups', sha(ORIGINAL));
}

function identity(filename: string) {
  const stat = fs.lstatSync(filename);
  return {
    device: stat.dev,
    inode: stat.ino,
    modified: stat.mtimeMs,
    changed: stat.ctimeMs,
    mode: stat.mode,
    uid: stat.uid,
    links: stat.nlink,
    sha: sha(fs.readFileSync(filename)),
  };
}

function idlePlan(): ProcessPlan {
  return { complete: true, processes: [], continuity: null };
}

function residentPlan(): ProcessPlan {
  return {
    complete: true,
    processes: [
      {
        role: 'cli',
        identity: {
          pid: 41414,
          ownerId: 'fixture-owner',
          startTime: 'fixture-birth',
          fingerprint: 'a'.repeat(64),
        },
      },
    ],
    continuity: { fingerprint: 'b'.repeat(64), restorable: true },
  };
}

function driverFixture(plans: ProcessPlan[] = [idlePlan()]) {
  const actions: string[] = [];
  let inspections = 0;
  const forbidden = async (name: string): Promise<never> => {
    actions.push(name);
    throw new Error('Unexpected fixture native/account/process action.');
  };
  const driver: AntigravitySwitchDriver = {
    hostId: 'ubuntu',
    canProveRuntimeIdentity: async () => {
      actions.push('release-gate-false');
      return false;
    },
    inspectProcesses: async () => {
      actions.push('inspect');
      return structuredClone(plans[Math.min(inspections++, plans.length - 1)]);
    },
    readCurrentCredential: () => forbidden('read-current-credential'),
    validateCredential: () => forbidden('validate-credential'),
    stopProcesses: () => forbidden('stop-processes'),
    installCredential: () => forbidden('install-credential'),
    readStoredIdentity: () => forbidden('read-stored-identity'),
    restartProcesses: () => forbidden('restart-processes'),
    proveRuntimeIdentity: () => forbidden('prove-runtime-identity'),
    rollbackCredential: () => forbidden('rollback-credential'),
    stopOwnedRestarts: () => forbidden('stop-owned-restarts'),
  };
  return { driver, actions };
}

function coordinatorResult(): Awaited<ReturnType<typeof updateManagedAntigravity>> {
  return {
    status: 'current',
    previousVersion: '0.4.1',
    version: '0.4.1',
    restartedProcesses: 0,
    updateAttempted: true,
  };
}

function failedCommandResult(attempted: boolean) {
  return {
    appId: 'antigravity-cli',
    platform: 'ubuntu',
    manager: 'native',
    status: 'failed',
    messageCode: 'update_failed',
    restartedProcesses: 0,
    updateAttempted: attempted,
  };
}

describe('managed Antigravity native updater: public files and injected commands only', () => {
  test('construction performs no filesystem or command action', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-update-construction-fixture-'));
    directories.push(root);
    const calls: CommandCall[] = [];
    const native = createManagedNativeUpdate({
      nativeBinary: path.join(root, 'nonexistent-native'),
      ccsDirectory: path.join(root, 'nonexistent-state'),
      runCommand: async (binary, args, capture) => {
        calls.push({ binary, args, capture });
        return '';
      },
    });
    expect(Object.keys(native)).toEqual(['version', 'backup', 'update', 'rollback']);
    expect(fs.readdirSync(root)).toEqual([]);
    expect(calls).toEqual([]);
  });

  for (const relative of ['nativeBinary', 'ccsDirectory'] as const) {
    test(`rejects a relative ${relative} without command execution`, () => {
      const value = fixture();
      expect(() =>
        createManagedNativeUpdate({
          nativeBinary: value.nativeBinary,
          ccsDirectory: value.ccsDirectory,
          [relative]: 'relative-fixture-path',
          runCommand: async () => {
            throw new Error('Must not execute.');
          },
        })
      ).toThrow('update-native-unsafe');
      expect(value.calls).toEqual([]);
    });
  }

  test('only the fixed version and official update arguments reach the injected runner', async () => {
    const value = fixture();
    expect(await value.native.version()).toBe('0.4.1');
    await value.native.backup();
    await value.native.update();
    expect(value.calls).toEqual([
      { binary: value.nativeBinary, args: ['--version'], capture: true },
      { binary: value.nativeBinary, args: ['update'], capture: false },
    ]);
  });

  test('backup preserves exact public bytes in a private content-addressed immutable file', async () => {
    const value = fixture();
    const before = identity(value.nativeBinary);
    await value.native.backup();
    const saved = backupPath(value);
    const savedIdentity = identity(saved);
    expect(fs.readFileSync(saved)).toEqual(ORIGINAL);
    expect(fs.lstatSync(saved).mode & 0o777).toBe(0o600);
    for (const directory of [
      value.ccsDirectory,
      path.join(value.ccsDirectory, 'antigravity-switching'),
      path.dirname(saved),
    ])
      expect(fs.lstatSync(directory).mode & 0o777).toBe(0o700);
    await value.native.backup();
    expect(identity(saved)).toEqual(savedIdentity);
    expect(identity(value.nativeBinary)).toEqual(before);
    expect(fs.readdirSync(path.dirname(saved))).toEqual([sha(ORIGINAL)]);
    expect(value.calls).toEqual([]);
  });

  test('a corrupt existing backup is refused and preserved without being overwritten', async () => {
    const value = fixture();
    await value.native.backup();
    const saved = backupPath(value);
    fs.writeFileSync(saved, REPLACEMENT);
    const foreign = identity(saved);
    await expect(value.native.backup()).rejects.toThrow('update-backup-changed');
    expect(identity(saved)).toEqual(foreign);
    expect(fs.readFileSync(saved)).toEqual(REPLACEMENT);
    expect(value.calls).toEqual([]);
  });

  for (const component of ['state', 'switching', 'backups'] as const) {
    test(`rejects a symlink ${component} ancestor before backup publication`, async () => {
      const value = fixture();
      const outside = path.join(value.root, 'fixture-foreign-directory');
      fs.mkdirSync(outside, { mode: 0o700 });
      if (component === 'state') {
        fs.rmdirSync(value.ccsDirectory);
        fs.symlinkSync(outside, value.ccsDirectory);
      } else if (component === 'switching') {
        fs.symlinkSync(outside, path.join(value.ccsDirectory, 'antigravity-switching'));
      } else {
        fs.mkdirSync(path.join(value.ccsDirectory, 'antigravity-switching'), { mode: 0o700 });
        fs.symlinkSync(
          outside,
          path.join(value.ccsDirectory, 'antigravity-switching/native-backups')
        );
      }
      await expect(value.native.backup()).rejects.toThrow('update-backup-unsafe');
      expect(fs.readdirSync(outside)).toEqual([]);
      expect(fs.readFileSync(value.nativeBinary)).toEqual(ORIGINAL);
      expect(value.calls).toEqual([]);
    });
  }

  for (const component of ['state', 'switching', 'backups'] as const) {
    test(`rejects a nonprivate ${component} directory without changing its mode`, async () => {
      const value = fixture();
      const switching = path.join(value.ccsDirectory, 'antigravity-switching');
      const backups = path.join(switching, 'native-backups');
      fs.mkdirSync(switching, { mode: 0o700 });
      fs.mkdirSync(backups, { mode: 0o700 });
      const target =
        component === 'state'
          ? value.ccsDirectory
          : component === 'switching'
            ? switching
            : backups;
      fs.chmodSync(target, 0o755);
      await expect(value.native.backup()).rejects.toThrow('update-backup-unsafe');
      expect(fs.lstatSync(target).mode & 0o777).toBe(0o755);
      expect(fs.readdirSync(backups)).toEqual([]);
    });
  }

  test('rollback before a backup and an attempted update refuses without filesystem changes', async () => {
    const value = fixture();
    const before = identity(value.nativeBinary);
    expect(await value.native.rollback()).toBe(false);
    await value.native.backup();
    expect(await value.native.rollback()).toBe(false);
    expect(identity(value.nativeBinary)).toEqual(before);
    expect(value.calls).toEqual([]);
  });

  test('an exact no-op rollback is read-only and preserves executable and backup inode identities', async () => {
    const value = fixture();
    const before = identity(value.nativeBinary);
    await value.native.backup();
    const savedIdentity = identity(backupPath(value));
    await value.native.update();
    expect(await value.native.rollback()).toBe(true);
    expect(await value.native.rollback()).toBe(true);
    expect(identity(value.nativeBinary)).toEqual(before);
    expect(identity(backupPath(value))).toEqual(savedIdentity);
    expect(fs.readdirSync(value.binDirectory)).toEqual(['fixture-agy']);
  });

  for (const bytes of [REPLACEMENT, ORIGINAL]) {
    test(`a foreign ${bytes.equals(ORIGINAL) ? 'same-byte' : 'changed-byte'} rename during update is never owned or overwritten by rollback`, async () => {
      const value = fixture();
      await value.native.backup();
      const original = identity(value.nativeBinary);
      const saved = identity(backupPath(value));
      value.setUpdate(async () => {
        value.replaceBinary(bytes);
        return '';
      });
      await value.native.update();
      const foreign = identity(value.nativeBinary);
      expect(foreign.inode).not.toBe(original.inode);
      expect(await value.native.rollback()).toBe(false);
      expect(await value.native.rollback()).toBe(false);
      expect(identity(value.nativeBinary)).toEqual(foreign);
      expect(fs.readFileSync(value.nativeBinary)).toEqual(bytes);
      expect(identity(backupPath(value))).toEqual(saved);
      expect(fs.readdirSync(value.binDirectory)).toEqual(['fixture-agy']);
    });
  }

  test('an in-place changed executable is preserved because no publication ownership receipt exists', async () => {
    const value = fixture();
    await value.native.backup();
    value.setUpdate(async () => {
      fs.writeFileSync(value.nativeBinary, REPLACEMENT);
      return '';
    });
    await value.native.update();
    const changed = identity(value.nativeBinary);
    expect(await value.native.rollback()).toBe(false);
    expect(identity(value.nativeBinary)).toEqual(changed);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
  });

  test('a replacement after a no-op update is preserved by rollback', async () => {
    const value = fixture();
    await value.native.backup();
    await value.native.update();
    value.replaceBinary();
    const foreign = identity(value.nativeBinary);
    expect(await value.native.rollback()).toBe(false);
    expect(identity(value.nativeBinary)).toEqual(foreign);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
  });

  test('a mode-only foreign change is refused without restoring permissions', async () => {
    const value = fixture();
    await value.native.backup();
    await value.native.update();
    fs.chmodSync(value.nativeBinary, 0o500);
    const changed = identity(value.nativeBinary);
    expect(await value.native.rollback()).toBe(false);
    expect(identity(value.nativeBinary)).toEqual(changed);
    expect(fs.lstatSync(value.nativeBinary).mode & 0o777).toBe(0o500);
  });

  test('a symlink foreign executable is left untouched and rollback returns false', async () => {
    const value = fixture();
    await value.native.backup();
    await value.native.update();
    const foreign = path.join(value.binDirectory, 'fixture-foreign-executable');
    fs.writeFileSync(foreign, REPLACEMENT, { mode: 0o700 });
    fs.unlinkSync(value.nativeBinary);
    fs.symlinkSync(foreign, value.nativeBinary);
    const targetIdentity = identity(foreign);
    expect(await value.native.rollback()).toBe(false);
    expect(fs.lstatSync(value.nativeBinary).isSymbolicLink()).toBe(true);
    expect(fs.readlinkSync(value.nativeBinary)).toBe(foreign);
    expect(identity(foreign)).toEqual(targetIdentity);
  });

  test('a command throw after replacing the executable is sanitized and cannot grant rollback ownership', async () => {
    const value = fixture();
    await value.native.backup();
    value.setUpdate(async () => {
      value.replaceBinary();
      throw new Error(RAW_FAILURE);
    });
    await expect(value.native.update()).rejects.toThrow('update-native-failed');
    const foreign = identity(value.nativeBinary);
    expect(await value.native.rollback()).toBe(false);
    expect(identity(value.nativeBinary)).toEqual(foreign);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
  });

  test('a command throw after no executable change permits only read-only no-op rollback', async () => {
    const value = fixture();
    await value.native.backup();
    const before = identity(value.nativeBinary);
    value.setUpdate(async () => {
      throw new Error(RAW_FAILURE);
    });
    await expect(value.native.update()).rejects.toThrow('update-native-failed');
    expect(await value.native.rollback()).toBe(true);
    expect(identity(value.nativeBinary)).toEqual(before);
  });

  for (const version of ['0.4.1', '0.4.2-rc.1']) {
    test(`accepts the bounded version ${version}`, async () => {
      const value = fixture();
      value.setVersion(version);
      expect(await value.native.version()).toBe(version);
    });
  }

  for (const output of ['no version supplied', '', `${'1'.repeat(129)}.2.3`]) {
    test(`invalid or overlong version output is refused (${output.length} bytes)`, async () => {
      const value = fixture();
      const native = createManagedNativeUpdate({
        nativeBinary: value.nativeBinary,
        ccsDirectory: value.ccsDirectory,
        runCommand: async () => output,
      });
      await expect(native.version()).rejects.toThrow('update-version-unavailable');
    });
  }

  test('a version response at the 65KB boundary remains bounded and returns only its version', async () => {
    const value = fixture();
    const native = createManagedNativeUpdate({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: async () => '0.4.1\n'.padEnd(MAX_OUTPUT_BYTES, ' '),
    });
    expect(await native.version()).toBe('0.4.1');
  });

  for (const output of ['f'.repeat(MAX_OUTPUT_BYTES + 1), 'é'.repeat(MAX_OUTPUT_BYTES / 2 + 1)]) {
    test(`rejects command output over 65KB by UTF-8 bytes (${Buffer.byteLength(output)} bytes)`, async () => {
      const value = fixture();
      const native = createManagedNativeUpdate({
        nativeBinary: value.nativeBinary,
        ccsDirectory: value.ccsDirectory,
        runCommand: async () => output,
      });
      await expect(native.version()).rejects.toThrow('update-output-unsafe');
      await native.backup();
      await expect(native.update()).rejects.toThrow('update-output-unsafe');
      expect(await native.rollback()).toBe(true);
    });
  }

  test('raw injected command errors never become version diagnostics', async () => {
    const value = fixture();
    const native = createManagedNativeUpdate({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: async () => {
        throw new Error(RAW_FAILURE);
      },
    });
    const error: unknown = await native.version().catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toBe('update-native-failed');
    expect(String(error)).not.toContain(RAW_FAILURE);
  });

  test('an ordinary idle update can run with the runtime release gate false and records its real attempt', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture();
    value.setUpdate(async () => {
      value.replaceBinary();
      value.setVersion('0.4.2');
      return '';
    });
    const result = await updateManagedAntigravity({
      registry: new AntigravityProfileRegistry(value.ccsDirectory),
      driver,
      native: value.native,
    });
    expect(result).toEqual({
      status: 'updated',
      previousVersion: '0.4.1',
      version: '0.4.2',
      restartedProcesses: 0,
      updateAttempted: true,
    });
    expect(actions).toEqual(['inspect', 'inspect']);
    expect(value.calls).toEqual([
      { binary: value.nativeBinary, args: ['--version'], capture: true },
      { binary: value.nativeBinary, args: ['update'], capture: false },
      { binary: value.nativeBinary, args: ['--version'], capture: true },
    ]);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
    expect(await value.native.rollback()).toBe(false);
  });

  test('an ordinary no-op update is current and still records that the update command ran', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture();
    const result = await updateManagedAntigravity({
      registry: new AntigravityProfileRegistry(value.ccsDirectory),
      driver,
      native: value.native,
    });
    expect(result).toEqual({
      status: 'current',
      previousVersion: '0.4.1',
      version: '0.4.1',
      restartedProcesses: 0,
      updateAttempted: true,
    });
    expect(actions).toEqual(['inspect', 'inspect']);
    expect(value.calls.filter((call) => call.args[0] === 'update')).toHaveLength(1);
  });

  test('a resident process with the release gate false prevents the actual update attempt', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture([residentPlan()]);
    const result = await updateManagedAntigravity({
      registry: new AntigravityProfileRegistry(value.ccsDirectory),
      driver,
      native: value.native,
    });
    expect(result).toEqual({
      status: 'failed',
      previousVersion: '0.4.1',
      version: '0.4.1',
      restartedProcesses: 0,
      updateAttempted: false,
    });
    expect(actions).toEqual(['inspect', 'release-gate-false']);
    expect(value.calls).toEqual([
      { binary: value.nativeBinary, args: ['--version'], capture: true },
    ]);
    expect(fs.existsSync(backupPath(value))).toBe(false);
  });

  test('an incomplete census prevents an update attempt without touching credentials or processes', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture([{ ...idlePlan(), complete: false }]);
    const result = await updateManagedAntigravity({
      registry: new AntigravityProfileRegistry(value.ccsDirectory),
      driver,
      native: value.native,
    });
    expect(result.updateAttempted).toBe(false);
    expect(result.status).toBe('failed');
    expect(actions).toEqual(['inspect']);
    expect(value.calls.filter((call) => call.args[0] === 'update')).toEqual([]);
    expect(fs.existsSync(backupPath(value))).toBe(false);
  });

  test('a newly appeared resident process after backup prevents the update attempt', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture([idlePlan(), residentPlan()]);
    const result = await updateManagedAntigravity({
      registry: new AntigravityProfileRegistry(value.ccsDirectory),
      driver,
      native: value.native,
    });
    expect(result.updateAttempted).toBe(false);
    expect(result.status).toBe('failed');
    expect(actions).toEqual(['inspect', 'inspect']);
    expect(value.calls.filter((call) => call.args[0] === 'update')).toEqual([]);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
  });

  test('a failed ordinary command after replacing the executable records attempted=true and preserves recovery evidence', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture();
    value.setUpdate(async () => {
      value.replaceBinary();
      throw new Error(RAW_FAILURE);
    });
    const result = await updateManagedAntigravity({
      registry: new AntigravityProfileRegistry(value.ccsDirectory),
      driver,
      native: value.native,
    });
    expect(result).toEqual({
      status: 'failed',
      previousVersion: '0.4.1',
      version: '0.4.1',
      restartedProcesses: 0,
      updateAttempted: true,
    });
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
    expect(actions).toEqual(['inspect', 'inspect']);
    expect(fs.readFileSync(value.nativeBinary)).toEqual(REPLACEMENT);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
    expect(await value.native.rollback()).toBe(false);
  });

  test('a coordinator lock cleanup failure after the native update preserves the real attempt in the fixed public result', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture();
    const registry = new AntigravityProfileRegistry(value.ccsDirectory);
    const held = registry.withLock.bind(registry);
    registry.withLock = async (operation) => {
      try {
        return await held(operation);
      } finally {
        throw new Error(RAW_FAILURE);
      }
    };
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) => updateManagedAntigravity({ registry, driver, native }),
    });
    expect(result).toEqual(failedCommandResult(true));
    expect(value.calls.map((call) => call.args)).toEqual([
      ['--version'],
      ['update'],
      ['--version'],
    ]);
    expect(actions).toEqual(['inspect', 'inspect']);
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
    expect(JSON.stringify(result)).not.toContain(value.nativeBinary);
    expect(fs.readFileSync(backupPath(value))).toEqual(ORIGINAL);
  });

  test('a coordinator lock cleanup failure before an update retains attempt=false', async () => {
    const value = fixture();
    const { driver, actions } = driverFixture([{ ...idlePlan(), complete: false }]);
    const registry = new AntigravityProfileRegistry(value.ccsDirectory);
    const held = registry.withLock.bind(registry);
    registry.withLock = async (operation) => {
      try {
        return await held(operation);
      } finally {
        throw new Error(RAW_FAILURE);
      }
    };
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) => updateManagedAntigravity({ registry, driver, native }),
    });
    expect(result).toEqual(failedCommandResult(false));
    expect(value.calls.map((call) => call.args)).toEqual([['--version']]);
    expect(actions).toEqual(['inspect']);
    expect(fs.existsSync(backupPath(value))).toBe(false);
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
  });

  test('an injected coordinator failure before any command has a fixed safe nonattempted result', async () => {
    const value = fixture();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: async () => {
        throw new Error(RAW_FAILURE);
      },
    });
    expect(result).toEqual(failedCommandResult(false));
    expect(value.calls).toEqual([]);
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
    expect(fs.existsSync(backupPath(value))).toBe(false);
  });

  test('a raw version command failure before update is not counted as an update attempt', async () => {
    const value = fixture();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: async () => {
        throw new Error(RAW_FAILURE);
      },
      execute: async (native) => {
        await native.version();
        return coordinatorResult();
      },
    });
    expect(result).toEqual(failedCommandResult(false));
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
    expect(fs.existsSync(backupPath(value))).toBe(false);
  });

  test('a synchronous runner failure on fixed update is counted before it throws', async () => {
    const value = fixture();
    let updateCalls = 0;
    const before = identity(value.nativeBinary);
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: (binary, args, capture) => {
        if (args.length === 1 && args[0] === 'update' && !capture) {
          updateCalls++;
          throw new Error(RAW_FAILURE);
        }
        return value.runCommand(binary, args, capture);
      },
      execute: async (native) => {
        await native.backup();
        await native.update();
        return coordinatorResult();
      },
    });
    expect(result).toEqual(failedCommandResult(true));
    expect(updateCalls).toBe(1);
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
    expect(identity(value.nativeBinary)).toEqual(before);
  });

  test('public success projection preserves real attempt and omits private coordinator extras', async () => {
    const value = fixture();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: async (native) => {
        await native.backup();
        await native.update();
        return {
          ...coordinatorResult(),
          updateAttempted: false,
          privateError: RAW_FAILURE,
          privatePath: value.nativeBinary,
          privateCredential: RAW_FAILURE,
        };
      },
    });
    expect(result).toEqual({
      appId: 'antigravity-cli',
      platform: 'ubuntu',
      manager: 'native',
      status: 'current',
      messageCode: 'current',
      previousVersion: '0.4.1',
      version: '0.4.1',
      updateAttempted: true,
      restartedProcesses: 0,
    });
    expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
    expect(JSON.stringify(result)).not.toContain(value.nativeBinary);
    expect(result).not.toHaveProperty('privateCredential');
  });

  test('a coordinator result cannot invent an update attempt without the fixed native invocation', async () => {
    const value = fixture();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: async () => ({ ...coordinatorResult(), status: 'failed', updateAttempted: true }),
    });
    expect(result.updateAttempted).toBe(false);
    expect(value.calls).toEqual([]);
  });

  test('unsafe coordinator status, version or process counts fail closed after a counted update', async () => {
    const value = fixture();
    for (const changes of [
      { status: RAW_FAILURE },
      { version: RAW_FAILURE },
      { previousVersion: value.nativeBinary },
      { restartedProcesses: -1 },
      { restartedProcesses: 129 },
    ]) {
      const result = await runManagedAntigravityUpdateCommand({
        nativeBinary: value.nativeBinary,
        ccsDirectory: value.ccsDirectory,
        runCommand: value.runCommand,
        execute: async (native) => {
          await native.backup();
          await native.update();
          return { ...coordinatorResult(), ...changes } as Awaited<
            ReturnType<typeof updateManagedAntigravity>
          >;
        },
      });
      expect(result).toEqual(failedCommandResult(true));
      expect(JSON.stringify(result)).not.toContain(RAW_FAILURE);
      expect(JSON.stringify(result)).not.toContain(value.nativeBinary);
    }
  });

  test('an unsafe native path fails before the injected operation or command', async () => {
    const value = fixture();
    let coordinatorCalls = 0;
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: 'relative-fixture-path',
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: async () => {
        coordinatorCalls++;
        return coordinatorResult();
      },
    });
    expect(result).toEqual(failedCommandResult(false));
    expect(coordinatorCalls).toBe(0);
    expect(value.calls).toEqual([]);
  });
});
