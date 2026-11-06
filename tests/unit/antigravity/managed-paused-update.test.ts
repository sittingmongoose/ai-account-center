import { afterEach, describe, expect, test } from 'bun:test';
import fs from 'fs';
import net from 'net';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import { spawnSync } from 'child_process';
import {
  createManagedNativeUpdate,
  nativeMatchesReviewedBuild,
  runManagedAntigravityUpdateCommand,
  type ManagedNativeCommand,
} from '../../../src/antigravity/managed-update-command';
import {
  routeManagedAntigravityUpdate,
  updatePausedAntigravity,
} from '../../../src/antigravity/managed-updater';
import { censusAntigravityProcesses } from '../../../src/antigravity/native-process-census';
import { probeRuntimeSocket } from '../../../src/antigravity/runtime-socket-probe';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import { AntigravityError } from '../../../src/antigravity/errors';
import type { AntigravitySwitchDriver, ProcessPlan } from '../../../src/antigravity/types';

const ORIGINAL = Buffer.from('public invented paused native fixture 1.4.12\n');
const REPLACEMENT = Buffer.from('public invented paused native fixture 1.5.2\n');
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

function temporary(prefix: string): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  directories.push(root);
  return root;
}

function sha(bytes: Buffer): string {
  return createHash('sha256').update(bytes).digest('hex');
}

/** Public fixture bytes only. The fake binary is never executed. */
function fixture() {
  const root = temporary('aac-agy-paused-update-');
  const ccsDirectory = path.join(root, 'fixture-state');
  const binDirectory = path.join(root, 'fixture-bin');
  fs.mkdirSync(ccsDirectory, { mode: 0o700 });
  fs.mkdirSync(binDirectory, { mode: 0o700 });
  const nativeBinary = path.join(binDirectory, 'fixture-agy');
  fs.writeFileSync(nativeBinary, ORIGINAL, { mode: 0o700 });
  const calls: string[][] = [];
  let reportedVersion = '1.4.12';
  let updateAction: () => Promise<string> = async () => '';
  const runCommand: ManagedNativeCommand = async (_binary, args, capture) => {
    calls.push([...args]);
    if (args.length === 1 && args[0] === '--version' && capture)
      return `Fixture native ${reportedVersion}\n`;
    if (args.length === 1 && args[0] === 'update' && !capture) return updateAction();
    throw new Error('Unexpected fixture command.');
  };
  const value = {
    root,
    ccsDirectory,
    nativeBinary,
    calls,
    runCommand,
    native: createManagedNativeUpdate({ nativeBinary, ccsDirectory, runCommand }),
    registry: new AntigravityProfileRegistry(ccsDirectory),
    backup: path.join(ccsDirectory, 'antigravity-switching/native-backups', sha(ORIGINAL)),
    setUpdate: (action: () => Promise<string>) => {
      updateAction = action;
    },
    installNewBuild: () => {
      value.setUpdate(async () => {
        const incoming = path.join(binDirectory, 'fixture-incoming');
        fs.writeFileSync(incoming, REPLACEMENT, { mode: 0o700 });
        fs.renameSync(incoming, nativeBinary);
        reportedVersion = '1.5.2';
        return '';
      });
    },
  };
  return value;
}

/** A driver whose every call is recorded; the paused path must touch none of it. */
function recordingDriver(
  plan: ProcessPlan | Error = new AntigravityError('antigravity-runtime-unavailable')
) {
  const actions: string[] = [];
  const forbidden = async (name: string): Promise<never> => {
    actions.push(name);
    throw new Error('Unexpected fixture driver action.');
  };
  const driver: AntigravitySwitchDriver = {
    hostId: 'ubuntu',
    canProveRuntimeIdentity: async () => {
      actions.push('capability');
      return false;
    },
    inspectProcesses: async () => {
      actions.push('inspect');
      if (plan instanceof Error) throw plan;
      return structuredClone(plan);
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

const idle = () => ({ complete: true, running: 0 });

describe('runtime-free Antigravity process census', () => {
  function procFixture() {
    const root = temporary('aac-agy-proc-');
    const home = path.join(root, 'home');
    const native = path.join(home, '.local/bin/agy');
    fs.mkdirSync(path.dirname(native), { recursive: true });
    fs.writeFileSync(native, ORIGINAL, { mode: 0o700 });
    const proc = path.join(root, 'proc');
    fs.mkdirSync(proc);
    const add = (pid: number, comm: string, exe: string, args: string[]) => {
      const directory = path.join(proc, String(pid));
      fs.mkdirSync(directory);
      fs.writeFileSync(path.join(directory, 'comm'), `${comm}\n`);
      fs.symlinkSync(exe, path.join(directory, 'exe'));
      fs.writeFileSync(path.join(directory, 'cmdline'), args.map((arg) => `${arg}\0`).join(''));
    };
    const census = () =>
      censusAntigravityProcesses({ home, nativeBinary: native, procRoot: proc, selfPid: 7 });
    return { home, native, proc, add, census };
  }

  test('no Antigravity process: complete and idle; unrelated, self and pid 1 are ignored', () => {
    const value = procFixture();
    value.add(1, 'agy', value.native, [value.native]);
    value.add(7, 'node', '/opt/fixture-runtime/node', ['node', 'managed-update-command.js']);
    value.add(200, 'bash', '/usr/bin/bash', ['bash']);
    fs.mkdirSync(path.join(value.proc, 'self'));
    expect(value.census()).toEqual({ complete: true, running: 0 });
  });

  test('counts the native by name, by a replaced (deleted) exe, helpers and the managed runtime', () => {
    const value = procFixture();
    value.add(300, 'agy', '/elsewhere/agy', ['agy']);
    value.add(301, 'renamed', `${value.native} (deleted)`, ['x']);
    value.add(302, 'helper', path.join(value.home, '.gemini/antigravity-cli/bin/agentapi'), ['x']);
    const bundle = path.join(
      value.home,
      '.local/share/ai-account-center/antigravity-runtime/bundles/fixture'
    );
    value.add(303, 'python3', '/usr/bin/python3.12', [
      path.join(bundle, 'venv/bin/python3'),
      '-I',
      path.join(bundle, 'managed_launcher.py'),
    ]);
    value.add(304, 'sh', '/usr/bin/dash', [
      '/bin/sh',
      path.join(value.home, '.local/share/ai-account-center/antigravity-runtime/bin/agy'),
    ]);
    value.add(305, 'antigravity', '/opt/Antigravity/antigravity', ['antigravity']);
    expect(value.census()).toEqual({ complete: true, running: 6 });
  });

  test('an unlistable /proc is an incomplete census', () => {
    const value = procFixture();
    fs.rmSync(value.proc, { recursive: true });
    expect(value.census()).toEqual({ complete: false, running: 0 });
  });

  test('another user`s processes are not counted', () => {
    const value = procFixture();
    value.add(400, 'agy', value.native, [value.native]);
    expect(
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        uid: (process.getuid?.() ?? 0) + 1,
      })
    ).toEqual({ complete: true, running: 0 });
  });
});

describe('runtime socket probe', () => {
  test('absent folder or socket is stopped; a listener is listening; a leftover socket file is stopped', async () => {
    const root = temporary('aac-agy-sock-');
    const runtime = path.join(root, 'antigravity-runtime');
    const socketPath = path.join(runtime, 'control.sock');
    expect(await probeRuntimeSocket(socketPath)).toBe('stopped');
    fs.mkdirSync(runtime, { mode: 0o700 });
    expect(await probeRuntimeSocket(socketPath)).toBe('stopped');
    const server = net.createServer((socket) => socket.destroy());
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    try {
      expect(await probeRuntimeSocket(socketPath)).toBe('listening');
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    // A runtime that exited leaves its bound socket file with no listener.
    const leftover = path.join(runtime, 'leftover.sock');
    const bound = spawnSync('python3', [
      '-I',
      '-c',
      'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])',
      leftover,
    ]);
    expect(bound.status).toBe(0);
    expect(fs.lstatSync(leftover).isSocket()).toBe(true);
    expect(await probeRuntimeSocket(leftover)).toBe('stopped');
    const plain = path.join(runtime, 'plain-file');
    fs.writeFileSync(plain, 'not a socket');
    expect(await probeRuntimeSocket(plain)).toBe('unknown');
  });
});

describe('managed Antigravity update while switching is paused', () => {
  test('nothing running: backs up the native, runs agy update and reports updated without the runtime', async () => {
    const value = fixture();
    value.installNewBuild();
    const census: string[] = [];
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      census: () => {
        census.push('census');
        return idle();
      },
      stillPaused: async () => true,
    });
    expect(result).toEqual({
      status: 'updated',
      previousVersion: '1.4.12',
      version: '1.5.2',
      restartedProcesses: 0,
      updateAttempted: true,
    });
    expect(census).toEqual(['census', 'census']);
    expect(value.calls).toEqual([['--version'], ['update'], ['--version']]);
    expect(fs.readFileSync(value.backup)).toEqual(ORIGINAL);
    expect((fs.statSync(value.backup).mode & 0o777).toString(8)).toBe('600');
  });

  test('a running Antigravity process answers in_use before any backup or update', async () => {
    const value = fixture();
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      census: () => ({ complete: true, running: 1 }),
      stillPaused: async () => true,
    });
    expect(result).toEqual({
      status: 'in_use',
      previousVersion: '1.4.12',
      version: '1.4.12',
      restartedProcesses: 0,
      updateAttempted: false,
    });
    expect(value.calls).toEqual([['--version']]);
    expect(fs.existsSync(value.backup)).toBe(false);
  });

  test('a process appearing after the backup still answers in_use without an update', async () => {
    const value = fixture();
    const answers = [idle(), { complete: true, running: 1 }];
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      census: () => answers.shift() ?? idle(),
      stillPaused: async () => true,
    });
    expect(result.status).toBe('in_use');
    expect(value.calls).toEqual([['--version']]);
  });

  test('an incomplete census, a pending recovery or a runtime that came back never updates', async () => {
    for (const variant of ['incomplete', 'recovery', 'runtime-back'] as const) {
      const value = fixture();
      if (variant === 'recovery') value.registry.hasRecovery = () => true;
      const result = await updatePausedAntigravity({
        registry: value.registry,
        native: value.native,
        census: () => (variant === 'incomplete' ? { complete: false, running: 0 } : idle()),
        stillPaused: async () => variant !== 'runtime-back',
      });
      expect(result).toEqual({
        status: 'failed',
        previousVersion: '1.4.12',
        version: '1.4.12',
        restartedProcesses: 0,
        updateAttempted: false,
        runtimePaused: true,
      });
      expect(value.calls).toEqual([['--version']]);
    }
  });

  test('a failed agy update is a runtime_paused failure that records its attempt', async () => {
    const value = fixture();
    value.setUpdate(async () => {
      throw new Error('fixture updater failure');
    });
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) =>
        updatePausedAntigravity({
          registry: value.registry,
          native,
          census: idle,
          stillPaused: async () => true,
        }),
    });
    expect(result).toEqual({
      appId: 'antigravity-cli',
      platform: 'ubuntu',
      manager: 'native',
      status: 'failed',
      messageCode: 'runtime_paused',
      previousVersion: '1.4.12',
      version: '1.4.12',
      updateAttempted: true,
      restartedProcesses: 0,
    });
  });

  test('an unreadable version on the paused path still names the paused runtime', async () => {
    const value = fixture();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: async () => {
        throw new Error('fixture version failure');
      },
      execute: (native) =>
        updatePausedAntigravity({
          registry: value.registry,
          native,
          census: idle,
          stillPaused: async () => true,
        }),
    });
    expect(result).toMatchObject({ status: 'failed', messageCode: 'runtime_paused' });
    expect(result.updateAttempted).toBe(false);
  });
});

describe('managed update routing', () => {
  test('unreviewed build with a stopped runtime takes the paused path and never asks the driver', async () => {
    const value = fixture();
    value.installNewBuild();
    const { driver, actions } = recordingDriver();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) =>
        routeManagedAntigravityUpdate({
          registry: value.registry,
          driver,
          native,
          idleOnly: true,
          nativeReviewed: () => false,
          runtimeSocket: async () => 'stopped',
          census: idle,
        }),
    });
    expect(result).toMatchObject({
      status: 'updated',
      messageCode: 'updated',
      previousVersion: '1.4.12',
      version: '1.5.2',
      updateAttempted: true,
    });
    expect(actions).toEqual([]);
    expect(fs.readFileSync(value.backup)).toEqual(ORIGINAL);
  });

  test('a running process on the paused path is action_required / in_use', async () => {
    const value = fixture();
    const { driver, actions } = recordingDriver();
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) =>
        routeManagedAntigravityUpdate({
          registry: value.registry,
          driver,
          native,
          nativeReviewed: () => false,
          runtimeSocket: async () => 'stopped',
          census: () => ({ complete: true, running: 2 }),
        }),
    });
    expect(result).toMatchObject({
      status: 'action_required',
      messageCode: 'in_use',
      updateAttempted: false,
    });
    expect(actions).toEqual([]);
    expect(value.calls).toEqual([['--version']]);
  });

  for (const [label, reviewed, socket] of [
    ['a listening runtime on an unreviewed build', false, 'listening'],
    ['an unclear runtime socket on an unreviewed build', false, 'unknown'],
    ['a reviewed build with a stopped runtime', true, 'stopped'],
  ] as const)
    test(`${label} keeps the coordinator; a down runtime is runtime_unavailable`, async () => {
      const value = fixture();
      const { driver, actions } = recordingDriver();
      let censused = false;
      const result = await runManagedAntigravityUpdateCommand({
        nativeBinary: value.nativeBinary,
        ccsDirectory: value.ccsDirectory,
        runCommand: value.runCommand,
        execute: (native) =>
          routeManagedAntigravityUpdate({
            registry: value.registry,
            driver,
            native,
            idleOnly: true,
            nativeReviewed: () => reviewed,
            runtimeSocket: async () => socket,
            census: () => {
              censused = true;
              return idle();
            },
          }),
      });
      expect(result).toEqual({
        appId: 'antigravity-cli',
        platform: 'ubuntu',
        manager: 'native',
        status: 'failed',
        messageCode: 'runtime_unavailable',
        updateAttempted: false,
        restartedProcesses: 0,
      });
      expect(actions).toEqual(['inspect']);
      expect(censused).toBe(false);
      expect(value.calls).toEqual([['--version']]);
      expect(fs.existsSync(value.backup)).toBe(false);
    });

  test('a live runtime still runs the coordinator idle-only path unchanged', async () => {
    const value = fixture();
    const { driver, actions } = recordingDriver({
      complete: true,
      processes: [],
      continuity: null,
    });
    value.installNewBuild();
    const result = await routeManagedAntigravityUpdate({
      registry: value.registry,
      driver,
      native: value.native,
      idleOnly: true,
      nativeReviewed: () => false,
      runtimeSocket: async () => 'listening',
      census: () => {
        throw new Error('the runtime-free census ran beside a live runtime');
      },
    });
    expect(result.status).toBe('updated');
    expect(result.runtimePaused).toBeUndefined();
    expect(actions).toEqual(['inspect', 'inspect']);
  });
});

describe('reviewed-build check', () => {
  function release(root: string, entries: Array<{ nativeVersion: string; nativeSha256: string }>) {
    const file = path.join(root, 'release.json');
    fs.writeFileSync(file, JSON.stringify({ reviewedNatives: entries }));
    return file;
  }

  test('matches a reviewed hash, refuses an unreviewed one, and treats an unusable list as reviewed', () => {
    const value = fixture();
    const reviewed = release(value.root, [{ nativeVersion: '1.3.5', nativeSha256: sha(ORIGINAL) }]);
    expect(nativeMatchesReviewedBuild(value.nativeBinary, reviewed)).toBe(true);
    const other = release(value.root, [{ nativeVersion: '1.3.5', nativeSha256: sha(REPLACEMENT) }]);
    expect(nativeMatchesReviewedBuild(value.nativeBinary, other)).toBe(false);
    const broken = path.join(value.root, 'broken.json');
    fs.writeFileSync(broken, '{');
    expect(nativeMatchesReviewedBuild(value.nativeBinary, broken)).toBe(true);
  });
});
