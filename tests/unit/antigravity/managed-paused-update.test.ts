import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import * as managedUpdater from '../../../src/antigravity/managed-updater';
import fs from 'fs';
import net from 'net';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import { execFile, spawn, spawnSync, type ChildProcess } from 'child_process';
import {
  createManagedNativeUpdate,
  nativeReviewState,
  runInstalledManagedAntigravityUpdate,
  runManagedAntigravityUpdateCommand,
  type ManagedNativeCommand,
} from '../../../src/antigravity/managed-update-command';
import {
  routeManagedAntigravityUpdate,
  updatePausedAntigravity,
} from '../../../src/antigravity/managed-updater';
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

function probeUnderNode(socketPath: string): Promise<string> {
  const module = path.resolve(import.meta.dir, '../../../src/antigravity/runtime-socket-probe.ts');
  const script = `const m = await import(${JSON.stringify(module)});
process.stdout.write(await m.probeRuntimeSocket(${JSON.stringify(socketPath)}));`;
  return new Promise((resolve, reject) => {
    execFile(
      'node',
      ['--experimental-strip-types', '--no-warnings', '--input-type=module', '-e', script],
      { timeout: 20_000 },
      (error, stdout) => (error ? reject(error) : resolve(stdout))
    );
  });
}

describe('runtime socket probe (under Node)', () => {
  test('absent folder or socket is stopped; a listener is listening; a leftover socket file is stopped', async () => {
    const root = temporary('aac-agy-sock-');
    const runtime = path.join(root, 'antigravity-runtime');
    const socketPath = path.join(runtime, 'control.sock');
    expect(await probeUnderNode(socketPath)).toBe('stopped');
    fs.mkdirSync(runtime, { mode: 0o700 });
    expect(await probeUnderNode(socketPath)).toBe('stopped');
    const server = net.createServer((socket) => socket.destroy());
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    try {
      expect(await probeUnderNode(socketPath)).toBe('listening');
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
    expect(await probeUnderNode(leftover)).toBe('stopped');
    const plain = path.join(runtime, 'plain-file');
    fs.writeFileSync(plain, 'not a socket');
    expect(await probeUnderNode(plain)).toBe('unknown');
  });

  test('a connect refused for permission (EACCES) is unknown, never stopped', async () => {
    if (process.getuid?.() === 0) return; // root ignores socket permissions
    const root = temporary('aac-agy-sock-eacces-');
    const socketPath = path.join(root, 'control.sock');
    const server = net.createServer((socket) => socket.destroy());
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    try {
      fs.chmodSync(socketPath, 0o000);
      expect(await probeUnderNode(socketPath)).toBe('unknown');
      // Under Bun the same refusal reads as ENOENT; a present file still keeps it unknown.
      expect(await probeRuntimeSocket(socketPath)).toBe('unknown');
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });
});

describe('managed Antigravity update while switching is paused', () => {
  test('nothing running: backs up the native, runs agy update and reports updated without the runtime', async () => {
    const value = fixture();
    value.installNewBuild();
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      stillPaused: async () => true,
    });
    expect(result).toEqual({
      status: 'updated',
      previousVersion: '1.4.12',
      version: '1.5.2',
      restartedProcesses: 0,
      updateAttempted: true,
    });
    expect(value.calls).toEqual([['--version'], ['update'], ['--version']]);
    expect(fs.readFileSync(value.backup)).toEqual(ORIGINAL);
    expect((fs.statSync(value.backup).mode & 0o777).toString(8)).toBe('600');
  });

  test('the final recheck follows backup and directly invokes the update', async () => {
    const value = fixture();
    const order: string[] = [];
    const native = {
      ...value.native,
      backup: async () => {
        order.push('backup');
        await value.native.backup();
      },
      update: () => {
        order.push('update');
        return value.native.update();
      },
    };
    await updatePausedAntigravity({
      registry: value.registry,
      native,
      stillPaused: () => {
        order.push('recheck');
        // Queue a sentinel after the await continuation. An extra await before
        // native.update would let it run before the update invocation.
        return Promise.resolve(true).then((value) => {
          queueMicrotask(() => queueMicrotask(() => order.push('later')));
          return value;
        });
      },
    });
    expect(order.slice(0, 3)).toEqual(['backup', 'recheck', 'update']);
  });

  test('agy update runs while the account lock is held', async () => {
    const value = fixture();
    const lock = path.join(value.ccsDirectory, 'antigravity-profiles/.transaction-lock');
    let locked: boolean | null = null;
    value.setUpdate(async () => {
      locked = fs.existsSync(lock);
      return '';
    });
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      stillPaused: async () => true,
    });
    expect(result.status).toBe('current');
    expect(locked).toBe(true);
    expect(fs.existsSync(lock)).toBe(false);
  });

  test('a failed backup never runs agy update and is a blocked paused failure', async () => {
    const value = fixture();
    const backups = path.join(value.ccsDirectory, 'antigravity-switching/native-backups');
    fs.mkdirSync(backups, { recursive: true, mode: 0o755 });
    fs.chmodSync(backups, 0o755);
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) =>
        updatePausedAntigravity({
          registry: value.registry,
          native,
          stillPaused: async () => true,
        }),
    });
    expect(result).toMatchObject({
      status: 'failed',
      messageCode: 'runtime_paused_blocked',
      updateAttempted: false,
    });
    expect(value.calls).toEqual([['--version']]);
    expect(fs.existsSync(value.backup)).toBe(false);
  });

  test('a pending recovery or a runtime that came back never updates', async () => {
    for (const variant of ['recovery', 'runtime-back'] as const) {
      const value = fixture();
      if (variant === 'recovery') value.registry.hasRecovery = () => true;
      const result = await updatePausedAntigravity({
        registry: value.registry,
        native: value.native,
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
      // Recovery refuses before any backup.
      if (variant !== 'runtime-back') expect(fs.existsSync(value.backup)).toBe(false);
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
          stillPaused: async () => true,
        }),
    });
    expect(result).toMatchObject({ status: 'failed', messageCode: 'runtime_paused_blocked' });
    expect(result.updateAttempted).toBe(false);
  });
});

describe('managed update routing', () => {
  test('a real listener bound during the final native hash blocks the update', async () => {
    const value = fixture();
    const { driver } = recordingDriver();
    const socket = path.join(value.root, 'control.sock');
    const release = path.join(value.root, 'release.json');
    fs.writeFileSync(
      release,
      JSON.stringify({
        reviewedNatives: [{ nativeVersion: '9.9.9', nativeSha256: sha(REPLACEMENT) }],
      })
    );
    const server = net.createServer((client) => client.destroy());
    let hashes = 0;
    try {
      const result = await routeManagedAntigravityUpdate({
        registry: value.registry,
        native: value.native,
        driver,
        nativeReview: () => {
          if (++hashes === 2) server.listen(socket);
          return nativeReviewState(value.nativeBinary, release);
        },
        runtimeSocket: () => probeRuntimeSocket(socket),
      });
      expect(result).toMatchObject({
        status: 'failed',
        runtimePaused: true,
        updateAttempted: false,
      });
      expect(value.calls).toEqual([['--version']]);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });

  for (const variant of [
    'native-replaced',
    'same-bytes-replaced',
    'socket-during-hash',
    'unclear-during-hash',
    'recovery-during-probe',
  ] as const)
    test(`final boundary refuses ${variant}`, async () => {
      const value = fixture();
      const { driver, actions } = recordingDriver();
      let hashes = 0;
      let socketState: 'stopped' | 'listening' | 'unknown' = 'stopped';
      let recovery = false;
      let probes = 0;
      value.registry.hasRecovery = () => recovery;
      const result = await routeManagedAntigravityUpdate({
        registry: value.registry,
        driver,
        native: {
          ...value.native,
          backup: async () => {
            await value.native.backup();
            if (variant.endsWith('replaced')) {
              const incoming = path.join(value.root, 'incoming');
              fs.writeFileSync(
                incoming,
                variant === 'same-bytes-replaced' ? ORIGINAL : REPLACEMENT,
                { mode: 0o700 }
              );
              fs.renameSync(incoming, value.nativeBinary);
            }
          },
        },
        nativeReview: () => {
          hashes++;
          if (hashes === 2 && variant === 'socket-during-hash') socketState = 'listening';
          if (hashes === 2 && variant === 'unclear-during-hash') socketState = 'unknown';
          return 'unreviewed';
        },
        runtimeSocket: async () => {
          probes++;
          if (probes === 3 && variant === 'recovery-during-probe') recovery = true;
          return socketState;
        },
      });
      expect(result).toMatchObject({
        status: 'failed',
        runtimePaused: true,
        updateAttempted: false,
      });
      expect(value.calls).toEqual([['--version']]);
      expect(actions).toEqual([]);
    });

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
          nativeReview: () => 'unreviewed',
          runtimeSocket: async () => 'stopped',
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

  test('a failed paused result never falls through to the coordinator', async () => {
    const value = fixture();
    value.registry.hasRecovery = () => true;
    const { driver, actions } = recordingDriver({
      complete: true,
      processes: [],
      continuity: null,
    });
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: value.runCommand,
      execute: (native) =>
        routeManagedAntigravityUpdate({
          registry: value.registry,
          driver,
          native,
          nativeReview: () => 'unreviewed',
          runtimeSocket: async () => 'stopped',
        }),
    });
    expect(result).toMatchObject({
      status: 'failed',
      messageCode: 'runtime_paused_blocked',
      updateAttempted: false,
    });
    expect(actions).toEqual([]);
    expect(value.calls).toEqual([['--version']]);
  });

  for (const [label, reviewed, socket] of [
    ['a listening runtime on an unreviewed build', 'unreviewed', 'listening'],
    ['an unclear runtime socket on an unreviewed build', 'unreviewed', 'unknown'],
    ['a reviewed build with a stopped runtime', 'reviewed', 'stopped'],
    ['an unreadable native with a stopped runtime', 'unknown', 'stopped'],
  ] as const)
    test(`${label} keeps the coordinator; a down runtime is runtime_unavailable`, async () => {
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
            idleOnly: true,
            nativeReview: () => reviewed,
            runtimeSocket: async () => socket,
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
      expect(value.calls).toEqual([['--version']]);
      expect(fs.existsSync(value.backup)).toBe(false);
    });

  for (const change of ['runtime-started', 'build-reviewed'] as const)
    test(`final recheck blocks when ${change} after backup`, async () => {
      const value = fixture();
      const { driver, actions } = recordingDriver();
      let backedUp = false;
      const order: string[] = [];
      const result = await routeManagedAntigravityUpdate({
        registry: value.registry,
        driver,
        native: {
          ...value.native,
          backup: async () => {
            await value.native.backup();
            backedUp = true;
            order.push('backup');
          },
        },
        runtimeSocket: async () => {
          order.push('socket');
          return backedUp && change === 'runtime-started' ? 'listening' : 'stopped';
        },
        nativeReview: () => {
          order.push('review');
          return backedUp && change === 'build-reviewed' ? 'reviewed' : 'unreviewed';
        },
      });
      expect(result).toMatchObject({
        status: 'failed',
        runtimePaused: true,
        updateAttempted: false,
      });
      expect(order).toEqual(
        change === 'runtime-started'
          ? ['socket', 'review', 'backup', 'socket']
          : ['socket', 'review', 'backup', 'socket', 'review']
      );
      expect(value.calls).toEqual([['--version']]);
      expect(actions).toEqual([]);
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
      nativeReview: () => 'unreviewed',
      runtimeSocket: async () => 'listening',
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

  test('reviewed, unreviewed, and unknown when the list or the native cannot be read', () => {
    const value = fixture();
    const reviewed = release(value.root, [{ nativeVersion: '1.3.5', nativeSha256: sha(ORIGINAL) }]);
    expect(nativeReviewState(value.nativeBinary, reviewed)).toBe('reviewed');
    const other = release(value.root, [{ nativeVersion: '1.3.5', nativeSha256: sha(REPLACEMENT) }]);
    expect(nativeReviewState(value.nativeBinary, other)).toBe('unreviewed');
    const broken = path.join(value.root, 'broken.json');
    fs.writeFileSync(broken, '{');
    expect(nativeReviewState(value.nativeBinary, broken)).toBe('unknown');
    expect(nativeReviewState(path.join(value.root, 'missing-agy'), other)).toBe('unknown');
  });

  test('routing probes the socket first and never hashes beside a live runtime', async () => {
    const value = fixture();
    const order: string[] = [];
    const { driver } = recordingDriver({ complete: true, processes: [], continuity: null });
    await routeManagedAntigravityUpdate({
      registry: value.registry,
      driver,
      native: value.native,
      idleOnly: true,
      nativeReview: () => {
        order.push('hash');
        return 'unreviewed';
      },
      runtimeSocket: async () => {
        order.push('socket');
        return 'listening';
      },
    });
    expect(order).toEqual(['socket']);
  });
});

/**
 * The installed entry point with a temporary home: a real descriptor, the
 * real socket probe and a fixture shell script as agy. The
 * script is not Antigravity; it only prints a version and replaces itself.
 */
describe('installed managed update wiring (temporary home)', () => {
  const children: ChildProcess[] = [];
  const servers: net.Server[] = [];
  afterEach(async () => {
    for (const child of children.splice(0)) child.kill('SIGKILL');
    for (const server of servers.splice(0))
      await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  function installedHome() {
    const home = temporary('aac-agy-installed-');
    const ccs = path.join(home, '.ccs');
    const switching = path.join(ccs, 'antigravity-switching');
    fs.mkdirSync(switching, { recursive: true, mode: 0o700 });
    fs.chmodSync(ccs, 0o700);
    fs.chmodSync(switching, 0o700);
    const native = path.join(home, '.local/bin/agy');
    fs.mkdirSync(path.dirname(native), { recursive: true, mode: 0o700 });
    const script = [
      '#!/bin/sh',
      'home=$(dirname "$(dirname "$(dirname "$0")")")',
      'case "$1" in',
      '  --version) echo "Fixture agy 1.4.12" ;;',
      '  --hold) echo ready > "$home/hold-ready"; read fixture ;;',
      '  update)',
      '    if [ -e "$home/.ccs/antigravity-profiles/.transaction-lock" ]; then echo locked; else echo unlocked; fi > "$home/update-log"',
      '    sed "s/1[.]4[.]12/1.5.2/" "$0" > "$0.new" && chmod 700 "$0.new" && mv "$0.new" "$0" ;;',
      'esac',
      '',
    ].join('\n');
    fs.writeFileSync(native, script, { mode: 0o700 });
    fs.writeFileSync(
      path.join(switching, 'runtime-installation.json'),
      JSON.stringify({
        schemaVersion: 1,
        bundleDirectory: path.join(
          home,
          '.local/share/ai-account-center/antigravity-runtime/bundles',
          'c'.repeat(64)
        ),
        nativeBinary: native,
        nativeSha256: 'b'.repeat(64),
        socketPath: path.join(ccs, 'antigravity-runtime/control.sock'),
      }),
      { mode: 0o600 }
    );
    return {
      home,
      ccs,
      native,
      log: path.join(home, 'update-log'),
      backups: path.join(switching, 'native-backups'),
      originalSha: sha(fs.readFileSync(native)),
    };
  }

  test('a running fixture agy keeps running while the paused update proceeds', async () => {
    const value = installedHome();
    const child = spawn(value.native, ['--hold'], { stdio: ['pipe', 'ignore', 'ignore'] });
    children.push(child);
    await new Promise<void>((resolve, reject) => {
      child.once('spawn', resolve);
      child.once('error', reject);
    });
    for (let i = 0; i < 100 && !fs.existsSync(path.join(value.home, 'hold-ready')); i++)
      await new Promise((resolve) => setTimeout(resolve, 10));
    expect(fs.existsSync(path.join(value.home, 'hold-ready'))).toBe(true);
    const result = await runInstalledManagedAntigravityUpdate(value.home);
    expect(result).toMatchObject({
      status: 'updated',
      messageCode: 'updated',
      updateAttempted: true,
      previousVersion: '1.4.12',
      version: '1.5.2',
      restartedProcesses: 0,
    });
    expect(child.exitCode).toBeNull();
    expect(child.signalCode).toBeNull();
    expect(process.kill(child.pid!, 0)).toBe(true);
    expect(fs.readFileSync(value.log, 'utf8').trim()).toBe('locked');
    expect(sha(fs.readFileSync(path.join(value.backups, value.originalSha)))).toBe(
      value.originalSha
    );
  });

  test('a release file that reviews the installed build keeps the coordinator', async () => {
    const value = installedHome();
    const release = path.join(value.home, 'fixture-release.json');
    fs.writeFileSync(
      release,
      JSON.stringify({
        reviewedNatives: [{ nativeVersion: '1.4.12', nativeSha256: value.originalSha }],
      })
    );
    // No socket at all: only the reviewed verdict keeps it off the paused path.
    expect(
      await runInstalledManagedAntigravityUpdate(value.home, { releaseFile: release })
    ).toEqual({
      appId: 'antigravity-cli',
      platform: 'ubuntu',
      manager: 'native',
      status: 'failed',
      messageCode: 'runtime_unavailable',
      updateAttempted: false,
      restartedProcesses: 0,
    });
    expect(fs.existsSync(value.log)).toBe(false);
    expect(fs.existsSync(value.backups)).toBe(false);
  });

  test('the installed entry passes idleOnly through: true, false, and true by default', async () => {
    const value = installedHome();
    const seen: Array<boolean | undefined> = [];
    const spy = spyOn(managedUpdater, 'routeManagedAntigravityUpdate').mockImplementation(
      async (deps) => {
        seen.push(deps.idleOnly);
        return {
          status: 'current',
          previousVersion: '1.4.12',
          version: '1.4.12',
          restartedProcesses: 0,
          updateAttempted: false,
        };
      }
    );
    try {
      await runInstalledManagedAntigravityUpdate(value.home, { idleOnly: true });
      await runInstalledManagedAntigravityUpdate(value.home, { idleOnly: false });
      await runInstalledManagedAntigravityUpdate(value.home);
    } finally {
      spy.mockRestore();
    }
    expect(seen).toEqual([true, false, true]);
    expect(fs.existsSync(value.log)).toBe(false);
  });

  test('a listening runtime socket keeps the coordinator, which reports runtime_unavailable', async () => {
    const value = installedHome();
    const runtime = path.join(value.ccs, 'antigravity-runtime');
    fs.mkdirSync(runtime, { mode: 0o700 });
    const socketPath = path.join(runtime, 'control.sock');
    const server = net.createServer((socket) => socket.destroy());
    servers.push(server);
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    fs.chmodSync(socketPath, 0o600);
    const result = await runInstalledManagedAntigravityUpdate(value.home);
    expect(result).toEqual({
      appId: 'antigravity-cli',
      platform: 'ubuntu',
      manager: 'native',
      status: 'failed',
      messageCode: 'runtime_unavailable',
      updateAttempted: false,
      restartedProcesses: 0,
    });
    expect(fs.existsSync(value.log)).toBe(false);
    expect(fs.existsSync(value.backups)).toBe(false);
  });
});
