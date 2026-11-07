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
    const add = (pid: number, comm: string, exe: string, args: string[], cwd?: string) => {
      const directory = path.join(proc, String(pid));
      fs.mkdirSync(directory);
      fs.writeFileSync(path.join(directory, 'comm'), `${comm}\n`);
      fs.symlinkSync(exe, path.join(directory, 'exe'));
      fs.writeFileSync(path.join(directory, 'cmdline'), args.map((arg) => `${arg}\0`).join(''));
      if (cwd) fs.symlinkSync(cwd, path.join(directory, 'cwd'));
    };
    // The census requires its own pid in the listing.
    add(7, 'node', '/opt/fixture-runtime/node', ['node', 'managed-update-command.js']);
    const census = (overrides: { home?: string; nativeBinary?: string } = {}) =>
      censusAntigravityProcesses({
        home: overrides.home ?? home,
        nativeBinary: overrides.nativeBinary ?? native,
        procRoot: proc,
        selfPid: 7,
      });
    return { root, home, native, proc, add, census };
  }

  test('no Antigravity process: complete and idle; unrelated, self and pid 1 are ignored', () => {
    const value = procFixture();
    value.add(1, 'agy', value.native, [value.native]);
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

  test('an unlistable /proc, or one without this process, is an incomplete census', () => {
    const value = procFixture();
    fs.rmSync(path.join(value.proc, '7'), { recursive: true });
    expect(value.census()).toEqual({ complete: false, running: 0 });
    fs.rmSync(value.proc, { recursive: true });
    expect(value.census()).toEqual({ complete: false, running: 0 });
  });

  test('a read failure other than the process exiting makes the census incomplete', () => {
    for (const broken of ['comm', 'cmdline', 'exe'] as const) {
      const value = procFixture();
      value.add(500, 'bash', '/usr/bin/bash', ['bash']);
      const field = path.join(value.proc, '500', broken);
      fs.rmSync(field);
      // A directory where a field should be: EISDIR / EINVAL, not ENOENT.
      fs.mkdirSync(field);
      expect(value.census()).toEqual({ complete: false, running: 0 });
    }
  });

  test('a process gone between listing and reading is skipped, not incomplete', () => {
    const value = procFixture();
    value.add(600, 'bash', '/usr/bin/bash', ['bash']);
    fs.rmSync(path.join(value.proc, '600', 'cmdline'));
    expect(value.census()).toEqual({ complete: true, running: 0 });
  });

  test('a long command line is read in part, so its native argument still counts', () => {
    const value = procFixture();
    value.add(700, 'sh', '/usr/bin/dash', ['/bin/sh', value.native, 'x'.repeat(200_000)]);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a symlinked home still matches helpers and runtime processes by their real path', () => {
    const value = procFixture();
    const linked = path.join(value.root, 'linked-home');
    fs.mkdirSync(path.join(value.home, '.gemini/antigravity-cli/bin'), { recursive: true });
    fs.symlinkSync(value.home, linked);
    value.add(800, 'helper', path.join(value.home, '.gemini/antigravity-cli/bin/agentapi'), ['x']);
    expect(value.census({ home: linked })).toEqual({ complete: true, running: 1 });
  });

  test('a dangling ~/.gemini symlink resolves by its target; an unreadable target parent is incomplete', () => {
    // the link's target is known (readlink), so a helper still running from where it pointed is counted
    const dangling = procFixture();
    fs.symlinkSync(path.join(dangling.root, 'gone-gemini'), path.join(dangling.home, '.gemini'));
    dangling.add(840, 'helper', path.join(dangling.root, 'gone-gemini/antigravity-cli/bin/x'), [
      'x',
    ]);
    expect(dangling.census()).toEqual({ complete: true, running: 1 });
    if (process.getuid?.() === 0) return; // root reads mode-000 folders
    const locked = procFixture();
    const parent = path.join(locked.root, 'locked-parent');
    fs.mkdirSync(path.join(parent, 'real-gemini/antigravity-cli'), { recursive: true });
    fs.symlinkSync(path.join(parent, 'real-gemini'), path.join(locked.home, '.gemini'));
    fs.chmodSync(parent, 0o000);
    try {
      expect(locked.census()).toEqual({ complete: false, running: 0 });
    } finally {
      fs.chmodSync(parent, 0o700);
    }
  });

  test('a removed root is rejoined, not widened: a sibling under the real parent is not counted', () => {
    const value = procFixture();
    const real = path.join(value.root, 'real-gemini-sibling');
    fs.mkdirSync(path.join(real, 'other-tool/bin'), { recursive: true });
    fs.symlinkSync(real, path.join(value.home, '.gemini'));
    value.add(850, 'other', path.join(real, 'other-tool/bin/other'), ['other']);
    expect(value.census()).toEqual({ complete: true, running: 0 });
  });

  test('a symlinked ~/.local with the whole runtime tail removed still matches by real path', () => {
    const value = procFixture();
    const realLocal = path.join(value.root, 'real-local');
    // Move ~/.local (with its bin/agy) out and link it back: share/... never existed.
    fs.renameSync(path.join(value.home, '.local'), realLocal);
    fs.symlinkSync(realLocal, path.join(value.home, '.local'));
    value.add(860, 'python3', '/usr/bin/python3.12', [
      path.join(
        realLocal,
        'share/ai-account-center/antigravity-runtime/bundles/x/venv/bin/python3'
      ),
    ]);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('the native matches by its real spelling when named through a linked home', () => {
    const value = procFixture();
    const linked = path.join(value.root, 'linked-home-native');
    fs.symlinkSync(value.home, linked);
    value.add(870, 'sh', '/usr/bin/dash', ['sh', value.native, '--hold']);
    expect(
      value.census({ home: linked, nativeBinary: path.join(linked, '.local/bin/agy') })
    ).toEqual({ complete: true, running: 1 });
  });

  test('a root child named ..cache is inside the root', () => {
    const value = procFixture();
    fs.mkdirSync(path.join(value.home, '.gemini/antigravity-cli/..cache'), { recursive: true });
    value.add(880, 'helper', path.join(value.home, '.gemini/antigravity-cli/..cache/helper'), [
      'x',
    ]);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a relative path argument resolves against the process working folder', () => {
    const value = procFixture();
    const root = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(path.join(root, 'bin'), { recursive: true });
    value.add(890, 'node', '/opt/fixture-runtime/node', ['node', './bin/helper.js'], root);
    // An interpreter's bare script name resolves too (it is its script, whatever its spelling).
    value.add(891, 'node', '/opt/fixture-runtime/node', ['node', 'helper.js'], root);
    expect(value.census()).toEqual({ complete: true, running: 2 });
  });

  test('a relative argument from a removed working folder still resolves inside the root', () => {
    const value = procFixture();
    // ~/.gemini/antigravity-cli was rm -rf'd after `sh ./bin/h.sh` started in it.
    const root = path.join(value.home, '.gemini/antigravity-cli');
    value.add(900, 'sh', '/usr/bin/dash', ['sh', './bin/h.sh'], `${root} (deleted)`);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a live working folder really named "... (deleted)" still resolves inside the root', () => {
    const value = procFixture();
    const real = path.join(value.root, 'data/agy (deleted)');
    fs.mkdirSync(path.join(real, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(value.home, '.gemini'));
    fs.symlinkSync(real, path.join(value.home, '.gemini/antigravity-cli'));
    // The kernel reports the real folder name, which happens to end in " (deleted)".
    value.add(940, 'sh', '/usr/bin/dash', ['sh', './bin/h.sh'], real);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a home really named "home (deleted)" still resolves a relative argument inside it', () => {
    const value = procFixture();
    const home = path.join(value.root, 'home (deleted)');
    fs.mkdirSync(path.join(home, '.gemini/antigravity-cli'), { recursive: true });
    value.add(950, 'sh', '/usr/bin/dash', ['sh', './.gemini/antigravity-cli/h.sh'], home);
    expect(value.census({ home, nativeBinary: path.join(home, '.local/bin/agy') })).toEqual({
      complete: true,
      running: 1,
    });
  });

  test('an executable in a root whose real name ends in " (deleted)" is counted', () => {
    const value = procFixture();
    const real = path.join(value.root, 'data-exe/agy (deleted)');
    fs.mkdirSync(path.join(real, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(value.home, '.gemini'));
    fs.symlinkSync(real, path.join(value.home, '.gemini/antigravity-cli'));
    value.add(960, 'helper', path.join(real, 'bin/unlisted-helper'), ['x']);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a live executable really named "agy (deleted)" is counted by its exact path', () => {
    const value = procFixture();
    const native = path.join(value.root, 'native-bin/agy (deleted)');
    fs.mkdirSync(path.dirname(native), { recursive: true });
    fs.writeFileSync(native, ORIGINAL, { mode: 0o700 });
    // Stripping the suffix alone would look for ".../agy", which is not the native.
    value.add(970, 'renamed', native, ['renamed']);
    expect(value.census({ nativeBinary: native })).toEqual({ complete: true, running: 1 });
  });

  test('each process resolves against its own working folder', () => {
    const value = procFixture();
    const root = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(root, { recursive: true });
    value.add(910, 'sh', '/usr/bin/dash', ['sh', './elsewhere.sh'], value.root);
    value.add(911, 'sh', '/usr/bin/dash', ['sh', './bin/h.sh'], root);
    // A working folder shared across processes would count 0 or 2, never exactly 1.
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a working-folder link that fails other than denied or gone is incomplete', () => {
    const value = procFixture();
    value.add(920, 'sh', '/usr/bin/dash', ['sh', './bin/h.sh']);
    // A regular file where the cwd link should be: readlink gives EINVAL.
    fs.writeFileSync(path.join(value.proc, '920', 'cwd'), 'not a link');
    expect(value.census()).toEqual({ complete: false, running: 0 });
  });

  test('~/.gemini as a regular file walks on through ENOTDIR and stays complete', () => {
    const value = procFixture();
    fs.writeFileSync(path.join(value.home, '.gemini'), 'not a folder');
    value.add(930, 'bash', '/usr/bin/bash', ['bash']);
    expect(value.census()).toEqual({ complete: true, running: 0 });
  });

  test('a symlinked ~/.gemini matches a helper run from its real path', () => {
    const value = procFixture();
    const real = path.join(value.root, 'real-gemini');
    fs.mkdirSync(path.join(real, 'antigravity-cli/bin'), { recursive: true });
    fs.symlinkSync(real, path.join(value.home, '.gemini'));
    value.add(820, 'helper', path.join(real, 'antigravity-cli/bin/unlisted-helper'), ['x']);
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a symlinked ~/.gemini whose root folder was removed still matches by real path', () => {
    const value = procFixture();
    const real = path.join(value.root, 'real-gemini-removed');
    fs.mkdirSync(real);
    fs.symlinkSync(real, path.join(value.home, '.gemini'));
    // ~/.gemini/antigravity-cli is gone; the helper still runs from its old real path.
    value.add(
      830,
      'helper',
      `${path.join(real, 'antigravity-cli/bin/unlisted-helper')} (deleted)`,
      ['x']
    );
    expect(value.census()).toEqual({ complete: true, running: 1 });
  });

  test('a symlinked home matches a helper whose root folder no longer exists', () => {
    const value = procFixture();
    const linked = path.join(value.root, 'linked-home-missing-root');
    fs.symlinkSync(value.home, linked);
    // No ~/.gemini folder at all: the root cannot be realpath'd, only joined.
    value.add(
      810,
      'helper',
      `${path.join(value.home, '.gemini/antigravity-cli/bin/agentapi')} (deleted)`,
      ['x']
    );
    expect(value.census({ home: linked })).toEqual({ complete: true, running: 1 });
  });

  test('unreadable comm or cmdline is incomplete: only the exe link may be denied', () => {
    if (process.getuid?.() === 0) return; // root reads mode-000 files
    for (const field of ['comm', 'cmdline'] as const) {
      const value = procFixture();
      value.add(900, 'bash', '/usr/bin/bash', ['bash']);
      fs.chmodSync(path.join(value.proc, '900', field), 0o000);
      expect(value.census()).toEqual({ complete: false, running: 0 });
    }
  });

  test('an interpreter running a relative script with its working folder denied leaves idle unproven', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(path.join(helperDir, 'bin'), { recursive: true });
    // A non-dumpable helper: `python3 ./bin/standin.py`, its exe and cwd links denied (EACCES).
    value.add(950, 'python3', '/usr/bin/python3', ['python3', './bin/standin.py'], helperDir);
    // An sshd-session title reads like a relative path; it is no interpreter, so its denied links prove nothing.
    value.add(
      951,
      'sshd-session',
      '/usr/lib/openssh/sshd-session',
      ['sshd-session: me@pts/0'],
      '/'
    );
    const denied = new Set([
      path.join(value.proc, '950', 'exe'),
      path.join(value.proc, '950', 'cwd'),
      path.join(value.proc, '951', 'exe'),
      path.join(value.proc, '951', 'cwd'),
    ]);
    const real = fs.readlinkSync;
    const spy = spyOn(fs, 'readlinkSync').mockImplementation(((
      file: fs.PathLike,
      ...rest: unknown[]
    ) => {
      if (denied.has(String(file))) throw Object.assign(new Error('denied'), { code: 'EACCES' });
      return (real as (...a: unknown[]) => string)(file, ...rest);
    }) as typeof fs.readlinkSync);
    try {
      expect(value.census()).toEqual({ complete: false, running: 0 });
      // a sandboxed renderer's `--flag=/path` arguments read like relative paths; it is no interpreter
      value.add(
        952,
        'chrome',
        '/opt/google/chrome/chrome',
        ['/opt/google/chrome/chrome', '--type=renderer', '--user-data-dir=/home/me/.config/x'],
        '/'
      );
      denied.add(path.join(value.proc, '952', 'exe'));
      denied.add(path.join(value.proc, '952', 'cwd'));
      // the same helper with its cwd readable is counted; the sshd-session alone leaves the census complete
      denied.delete(path.join(value.proc, '950', 'cwd'));
      expect(value.census()).toEqual({ complete: true, running: 1 });
      fs.rmSync(path.join(value.proc, '950'), { recursive: true });
      expect(value.census()).toEqual({ complete: true, running: 0 });
      // other interpreters count the same way (lua by its argv0, the process renamed)
      value.add(953, 'helper', '/usr/bin/lua5.4', ['lua5.4', './bin/standin.lua'], helperDir);
      denied.add(path.join(value.proc, '953', 'exe'));
      denied.add(path.join(value.proc, '953', 'cwd'));
      expect(value.census()).toEqual({ complete: false, running: 0 });
    } finally {
      spy.mockRestore();
    }
  });

  test('an interpreter`s script is found after any options and by a bare name', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(helperDir, { recursive: true });
    // readable working folders: the script is placed and counted
    value.add(960, 'python3', '/usr/bin/python3', ['python3', 'helper.py'], helperDir);
    expect(value.census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '960'), { recursive: true });
    value.add(
      961,
      'python3',
      '/usr/bin/python3',
      ['python3', '-I', '-u', '-B', './helper.py'],
      helperDir
    );
    expect(value.census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '961'), { recursive: true });
    // the same bare script outside the roots is not a helper
    value.add(962, 'node', '/usr/bin/node', ['node', 'server.js'], value.root);
    expect(value.census()).toEqual({ complete: true, running: 0 });
    // a non-interpreter's bare words stay unresolved
    value.add(963, 'rg', '/usr/bin/rg', ['rg', 'helper.py'], helperDir);
    expect(value.census()).toEqual({ complete: true, running: 0 });
    // every argument read is looked at: a script after 40 options outside the roots is idle, inside them busy
    const many = (script: string) => [
      'python3',
      '-X',
      'dev',
      ...Array.from({ length: 40 }, (_, i) => `-W${i}`),
      script,
    ];
    value.add(964, 'python3', '/usr/bin/python3', many('helper.py'), value.root);
    expect(value.census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '964'), { recursive: true });
    value.add(
      965,
      'python3',
      '/usr/bin/python3',
      many(path.join(helperDir, 'helper.py')),
      value.root
    );
    expect(value.census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '965'), { recursive: true });
    // an option's value is a script too
    value.add(
      966,
      'node',
      '/usr/bin/node',
      ['node', `--require=${path.join(helperDir, 'hook.js')}`, 'app.js'],
      value.root
    );
    expect(value.census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '966'), { recursive: true });
    // an interpreter working inside a root counts whatever its arguments say
    for (const argv of [
      ['python3', '-m', 'helper'],
      ['python3', '-c', 'import helper'],
      ['python3', '-u'],
    ]) {
      value.add(967, 'python3', '/usr/bin/python3', argv, helperDir);
      expect([argv, value.census()]).toEqual([argv, { complete: true, running: 1 }]);
      fs.rmSync(path.join(value.proc, '967'), { recursive: true });
    }
    // an interpreter with no arguments in a root reads its script from stdin: busy; a bare shell parked there is not
    value.add(968, 'node', '/usr/bin/node', ['node'], helperDir);
    expect(value.census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '968'), { recursive: true });
    value.add(968, 'bash', '/usr/bin/bash', ['bash'], helperDir);
    expect(value.census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '968'), { recursive: true });
    // an empty command line is no evidence
    value.add(968, 'python3', '/usr/bin/python3', [], helperDir);
    expect(value.census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '968'), { recursive: true });
    // a rewritten title still runs the interpreter (by its exe); attached and listed option values are scripts too
    value.add(970, 'helper', '/usr/bin/python3.12', ['helper', 'x.py'], helperDir);
    expect(value.census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '970'), { recursive: true });
    // a percent-encoded file URL and a path through a symlink elsewhere both reach the root
    const link = path.join(value.root, 'link-to-helpers');
    fs.symlinkSync(helperDir, link);
    fs.writeFileSync(path.join(helperDir, 'h.py'), '');
    const encoded = `file://${helperDir.replace('.gemini', '%2Egemini')}/h.mjs`;
    for (const argv of [
      ['node', `--import=${encoded}`, 'app.js'],
      ['node', `--import=${encoded.replace('file:', 'FILE:')}`, 'app.js'],
      ['node', `--path=/a:${encoded}`, 'app.js'],
      ['node', `--path=file:///a:${path.join(helperDir, 'h.mjs')}`, 'app.js'],
      ['python3', path.join(link, 'gone-now.py')],
      ['python3', path.join(link, 'h.py')],
      ['node', `-r${path.join(helperDir, 'hook.js')}`, 'app.js'],
      ['node', `--import=file://${path.join(helperDir, 'h.mjs')}`, 'app.js'],
      ['node', `--path=/a:${helperDir}`, 'app.js'],
      ['python3', 'script.py', `ROOT=${path.join(helperDir, 'x')}`],
    ]) {
      value.add(971, argv[0], `/usr/bin/${argv[0]}`, argv, value.root);
      expect([argv, value.census()]).toEqual([argv, { complete: true, running: 1 }]);
      fs.rmSync(path.join(value.proc, '971'), { recursive: true });
    }
    // a command line cut at the read limit leaves idle unproven
    value.add(
      969,
      'python3',
      '/usr/bin/python3',
      ['python3', '-W', 'x'.repeat(70_000)],
      value.root
    );
    expect(value.census()).toEqual({ complete: false, running: 0 });
  });

  test('lookups never touch a network or automount, and links, chained URLs and the lookup cap hold', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(helperDir, { recursive: true });
    // a fixture mount table: one folder is an automount (autofs) whose name has a space (\040)
    const share = path.join(value.root, 'auto share');
    fs.mkdirSync(share);
    fs.symlinkSync(helperDir, path.join(share, 'to-root'));
    const mountInfo = path.join(value.root, 'mountinfo');
    fs.writeFileSync(
      mountInfo,
      `36 25 0:32 / ${share.replace(/ /g, '\\040')} rw,relatime shared:1 - autofs systemd-1 rw,fd=1\n`
    );
    const census = () =>
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        mountInfo,
      });
    // a local link into the automount is not followed (it would mount a share that may be down)
    const viaShare = path.join(value.root, 'via-share');
    fs.symlinkSync(path.join(share, 'to-root'), viaShare);
    value.add(
      980,
      'python3',
      '/usr/bin/python3',
      ['python3', path.join(viaShare, 'h.py')],
      value.root
    );
    expect(census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '980'), { recursive: true });
    // a dangling link into a root still reaches it, by its target
    const dangling = path.join(value.root, 'dangling');
    fs.symlinkSync(path.join(helperDir, 'removed-dir'), dangling);
    value.add(
      981,
      'python3',
      '/usr/bin/python3',
      ['python3', path.join(dangling, 'x.py')],
      value.root
    );
    expect(census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '981'), { recursive: true });
    // two URLs in one list item, and a URL followed by :more
    const url = `file://${helperDir.replace('.gemini', '%2Egemini')}`;
    for (const argv of [
      ['node', `--path=file:///a:${url}/h.mjs`, 'app.js'],
      ['node', `--path=${url}:/b`, 'app.js'],
    ]) {
      value.add(982, 'node', '/usr/bin/node', argv, value.root);
      expect([argv, census()]).toEqual([argv, { complete: true, running: 1 }]);
      fs.rmSync(path.join(value.proc, '982'), { recursive: true });
    }
    // a long inline script's bare fragments cost no lookups: the census stays complete
    const script = Array.from({ length: 2000 }, (_, i) => `echo step${i}:done,ok`).join('; ');
    value.add(984, 'bash', '/usr/bin/bash', ['bash', '-c', script], value.root);
    expect(census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '984'), { recursive: true });
    // past the lookup cap (thousands of distinct paths) an interpreter's script is unplaced, not idle
    const many = Array.from({ length: 5000 }, (_, i) => `/q/${i}`);
    value.add(983, 'python3', '/usr/bin/python3', ['python3', ...many], value.root);
    expect(census()).toEqual({ complete: false, running: 0 });
    fs.rmSync(path.join(value.proc, '983'), { recursive: true });
    // `..` applies after a link, as the kernel does: /tmp/l/../h.py with /tmp/l -> root/sub is root/h.py
    fs.mkdirSync(path.join(helperDir, 'sub'), { recursive: true });
    const intoSub = path.join(value.root, 'into-sub');
    fs.symlinkSync(path.join(helperDir, 'sub'), intoSub);
    value.add(985, 'python3', '/usr/bin/python3', ['python3', `${intoSub}/../h.py`], value.root);
    expect(census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '985'), { recursive: true });
  });

  test('a link into a root that sits on a network mount still counts, by its spelling', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(helperDir, { recursive: true });
    const mountInfo = path.join(value.root, 'mountinfo');
    fs.writeFileSync(
      mountInfo,
      `40 25 0:40 / ${path.join(value.home, '.gemini')} rw - nfs4 nas:/gemini rw\n` +
        `1 0 0:1 / / rw - virtiofs root rw\n`
    );
    const link = path.join(value.root, 'to-helpers');
    fs.symlinkSync(helperDir, link);
    value.add(990, 'python3', '/usr/bin/python3', ['python3', path.join(link, 'h.py')], value.root);
    expect(
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        mountInfo,
      })
    ).toEqual({ complete: true, running: 1 });
  });

  test('a bare word that is a link in the working folder still reaches a root', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(helperDir, { recursive: true });
    const work = path.join(value.root, 'work');
    fs.mkdirSync(work);
    for (const name of ['h.py', 'h.sh']) {
      fs.writeFileSync(path.join(helperDir, name), '');
      fs.symlinkSync(path.join(helperDir, name), path.join(work, name));
    }
    fs.symlinkSync(helperDir, path.join(work, 'pkg'));
    fs.writeFileSync(path.join(work, 'app.js'), '');
    for (const argv of [
      ['python3', 'h.py'],
      ['python3', '-I', '-u', 'h.py'],
      ['bash', 'h.sh'],
      ['node', 'pkg'],
      ['node', '--require=h.py', 'app.js'],
      ['node', '--import=file:h.py', 'app.js'],
    ]) {
      value.add(991, argv[0], `/usr/bin/${argv[0]}`, argv, work);
      expect([argv, value.census()]).toEqual([argv, { complete: true, running: 1 }]);
      fs.rmSync(path.join(value.proc, '991'), { recursive: true });
    }
    // a plain file there (not a link) is no helper
    value.add(992, 'node', '/usr/bin/node', ['node', 'app.js'], work);
    expect(value.census()).toEqual({ complete: true, running: 0 });
  });

  test('a .. that leaves a network mount resumes link resolution, and a long ./ path stays fast', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(helperDir, { recursive: true });
    const nfs = path.join(value.root, 'nfs');
    fs.mkdirSync(path.join(nfs, 'proj'), { recursive: true });
    fs.symlinkSync(helperDir, path.join(value.root, 'lnk'));
    const mountInfo = path.join(value.root, 'mountinfo');
    fs.writeFileSync(mountInfo, `40 25 0:40 / ${nfs} rw - nfs4 nas:/x rw\n`);
    const census = () =>
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        mountInfo,
      });
    for (const [argv, cwd] of [
      [['python3', '../../lnk/h.py'], path.join(nfs, 'proj')],
      [['python3', `${nfs}/../lnk/h.py`], value.root],
    ] as const) {
      value.add(993, 'python3', '/usr/bin/python3', [...argv], cwd);
      expect([argv, census()]).toEqual([argv, { complete: true, running: 1 }]);
      fs.rmSync(path.join(value.proc, '993'), { recursive: true });
    }
    const dots = `${'./'.repeat(32_000)}x`;
    for (let pid = 994; pid < 999; pid += 1)
      value.add(pid, 'python3', '/usr/bin/python3', ['python3', dots], value.root);
    const started = performance.now();
    census();
    expect(performance.now() - started).toBeLessThan(2000);
  });

  test('a mount resume keeps later links, long arguments and unlistable folders resolve, and work stays bounded', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(path.join(helperDir, 'sub'), { recursive: true });
    fs.writeFileSync(path.join(helperDir, 'h.py'), '');
    const nfs = path.join(value.root, 'nfs');
    fs.mkdirSync(nfs);
    fs.symlinkSync(path.join(helperDir, 'sub'), path.join(value.root, 'lnk'));
    const mountInfo = path.join(value.root, 'mountinfo');
    fs.writeFileSync(mountInfo, `40 25 0:40 / ${nfs} rw - nfs4 nas:/x rw\n`);
    const census = () =>
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        mountInfo,
      });
    // F1: /nfs/../lnk/../h.py opens root/h.py (lnk -> root/sub): the .. after the link applies to the link's target
    value.add(
      1001,
      'python3',
      '/usr/bin/python3',
      ['python3', `${nfs}/../lnk/../h.py`],
      value.root
    );
    expect(census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '1001'), { recursive: true });
    // F2: a 4090-byte argument (./ repeats, then a link into a root) still resolves
    const work = path.join(value.root, 'work');
    fs.mkdirSync(work);
    fs.symlinkSync(path.join(helperDir, 'h.py'), path.join(work, 'h.py'));
    value.add(1002, 'python3', '/usr/bin/python3', ['python3', `${'./'.repeat(2043)}h.py`], work);
    expect(census()).toEqual({ complete: true, running: 1 });
    fs.rmSync(path.join(value.proc, '1002'), { recursive: true });
    // glued short-option values: php -fh.py, ruby -rh.py
    for (const argv of [
      ['php', '-fh.py'],
      ['ruby', '-rh.py'],
    ]) {
      value.add(1003, argv[0], `/usr/bin/${argv[0]}`, argv, work);
      expect([argv, census()]).toEqual([argv, { complete: true, running: 1 }]);
      fs.rmSync(path.join(value.proc, '1003'), { recursive: true });
    }
    // F3: a working folder that can be searched but not listed (--x) still has its links followed
    if (process.getuid?.() !== 0) {
      fs.chmodSync(work, 0o311);
      try {
        value.add(1004, 'python3', '/usr/bin/python3', ['python3', 'h.py'], work);
        expect(census()).toEqual({ complete: true, running: 1 });
        fs.rmSync(path.join(value.proc, '1004'), { recursive: true });
      } finally {
        fs.chmodSync(work, 0o755);
      }
    }
    // F4: a very deep working folder with many relative arguments runs out of the step budget quickly: unplaced
    let deep = value.root;
    for (let i = 0; i < 300; i += 1) deep = path.join(deep, 'd');
    fs.mkdirSync(deep, { recursive: true });
    const args = Array.from({ length: 400 }, (_, i) => `./${i}/x.py`);
    value.add(1005, 'python3', '/usr/bin/python3', ['python3', ...args], deep);
    const started = performance.now();
    expect(census()).toEqual({ complete: false, running: 0 });
    expect(performance.now() - started).toBeLessThan(3000);
  });

  test('a denied script path is unknown, a loop or a prose mention is not, and start-up never touches a mount', () => {
    const value = procFixture();
    const helperDir = path.join(value.home, '.gemini/antigravity-cli');
    fs.mkdirSync(helperDir, { recursive: true });
    // a link loop: the kernel can't open the path either (ELOOP), so nothing runs from it
    fs.symlinkSync(path.join(value.root, 'loop-b'), path.join(value.root, 'loop-a'));
    fs.symlinkSync(path.join(value.root, 'loop-a'), path.join(value.root, 'loop-b'));
    value.add(
      1010,
      'python3',
      '/usr/bin/python3',
      ['python3', path.join(value.root, 'loop-a', 'h.py')],
      value.root
    );
    expect(value.census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '1010'), { recursive: true });
    // a NUL byte (file:///tmp/a%00b.py) names no openable path
    value.add(
      1012,
      'node',
      '/usr/bin/node',
      ['node', '--import=file:///tmp/a%00b.py', 'app.js'],
      value.root
    );
    expect(value.census()).toEqual({ complete: true, running: 0 });
    fs.rmSync(path.join(value.proc, '1012'), { recursive: true });
    // a denied folder on the way (not root, which reads mode-000 folders)
    if (process.getuid?.() !== 0) {
      const locked = path.join(value.root, 'locked');
      fs.mkdirSync(path.join(locked, 'in'), { recursive: true });
      // the same, under a folder whose name has a space
      const spaced = path.join(value.root, 'My Scripts');
      const spacedLocked = path.join(spaced, 'locked');
      fs.mkdirSync(path.join(spacedLocked, 'in'), { recursive: true });
      fs.symlinkSync(locked, path.join(value.root, 'to-locked'));
      fs.chmodSync(locked, 0o000);
      fs.chmodSync(spacedLocked, 0o000);
      const census = (argv: string[], cwd = value.root) => {
        value.add(1011, argv[0], `/usr/bin/${argv[0]}`, argv, cwd);
        try {
          return value.census();
        } finally {
          fs.rmSync(path.join(value.proc, '1011'), { recursive: true });
        }
      };
      try {
        const unknown = { complete: false, running: 0 };
        const idle = { complete: true, running: 0 };
        expect(census(['python3', path.join(locked, 'in', 'h.py')])).toEqual(unknown);
        // A script is never prose, whatever spaces its path, its own name or its working folder has; a later
        // argument is prose only when the space comes at or after the denied part.
        for (const [argv, cwd] of [
          [['python3', path.join(spacedLocked, 'in', 'h.py')]],
          [['python3', path.join(locked, 'in', 'my helper.py')]],
          [['python3', '-u', path.join(locked, 'in', 'my helper.py')]],
          [['python3', 'locked/in/h.py'], spaced],
          [['node', 'app.js', path.join(spacedLocked, 'in', 'h.js')]],
          // past a link, the rest is no longer the argument as written: not prose
          [['node', 'app.js', path.join(value.root, 'to-locked', 'a b')]],
          // the script after an option's separate value or a subcommand, with or without an extension
          [['python3', '-W', 'ignore', path.join(locked, 'in', 'my helper.py')]],
          [['python3', '-W', 'ignore', path.join(locked, 'in', 'my helper')]],
          [['node', '--require', './hook.js', path.join(locked, 'in', 'my helper.js')]],
          [['node', '--require', './hook.js', path.join(locked, 'in', 'my helper')]],
          [['node', '--import', 'tsx', path.join(spacedLocked, 'in', 'h b.js')]],
          [['bun', 'run', path.join(locked, 'in', 'my helper.ts')]],
          [['bun', 'run', path.join(locked, 'in', 'my helper')]],
          [['deno', 'run', path.join(locked, 'in', 'my helper')]],
          [['perl', '-I', 'lib', path.join(locked, 'in', 'my helper')]],
          [['ruby', '-r', 'json', path.join(locked, 'in', 'my helper')]],
          [['bash', '-o', 'pipefail', path.join(locked, 'in', 'my helper')]],
          // clustered short options, and options no list knows, with a script that has no extension
          [['python3', '-uW', 'ignore', path.join(locked, 'in', 'my helper')]],
          [['bash', '-eo', 'pipefail', path.join(locked, 'in', 'my helper')]],
          [['bash', '-euo', 'pipefail', path.join(locked, 'in', 'my helper')]],
          [['perl', '-wI', 'lib', path.join(locked, 'in', 'my helper')]],
          [['ruby', '-wr', 'json', path.join(locked, 'in', 'my helper')]],
          [['node', '--disable-warning', 'X', path.join(locked, 'in', 'my helper')]],
          [['node', '--watch-path', './src', path.join(locked, 'in', 'my helper')]],
          [['bun', '--filter', 'pkg', 'run', path.join(locked, 'in', 'my helper')]],
          [['bun', 'x', 'tsx', path.join(locked, 'in', 'my helper')]],
          [['deno', 'run', '--ext', 'ts', path.join(locked, 'in', 'my helper')]],
          [['julia', '--threads', 'auto', path.join(locked, 'in', 'my helper')]],
          [['tclsh', '-encoding', 'utf-8', path.join(locked, 'in', 'my helper')]],
          [['fish', '-C', 'source ~/x/y', path.join(locked, 'in', 'my helper')]],
          // a file an interpreter runs, by its extension, is never prose wherever it sits
          [['node', 'launcher.js', path.join(locked, 'in', 'my helper.js')]],
          [['node', 'app.js', `--require=${path.join(locked, 'in', 'my hook.mjs')}`]],
        ] as [string[], string?][])
          expect([argv, census(argv, cwd)]).toEqual([argv, unknown]);
        // a prose prompt that starts with the denied folder is no script: idle stays provable
        for (const argv of [
          ['node', 'app.js', `${locked}/in and/or later`],
          ['python3', '-u', 'app.py', `${locked}/in and/or later`],
          ['python3', '-W', 'ignore', 'app.py', `${locked}/in and/or later`],
          ['bun', 'run', 'app.ts', `${locked}/in and/or later`],
          ['python3', '-OO', 'app.py', `${locked}/in and/or later`],
          ['node', '--enable-source-maps', '--no-warnings', 'cli.js', `${locked}/in and/or later`],
          ['deno', 'run', '--lock', 'app.ts', `${locked}/in and/or later`],
          ['deno', 'run', '--allow-read', 'app.ts', `${locked}/in and/or later`],
          ['bash', '-eu', 'run.sh', `${locked}/in and/or later`],
          ['node', 'app.js', `${spacedLocked}/in/notes are here`],
          ['node', 'app.js', `--prompt=${locked}/in and/or later`],
        ])
          expect([argv, census(argv)]).toEqual([argv, idle]);
      } finally {
        fs.chmodSync(locked, 0o755);
        fs.chmodSync(spacedLocked, 0o755);
      }
    }
    // start-up: a home whose .gemini is an automount resolves by spelling, with no lookup under it
    const mountInfo = path.join(value.root, 'mountinfo');
    fs.writeFileSync(
      mountInfo,
      `50 25 0:50 / ${path.join(value.home, '.gemini')} rw - autofs systemd-1 rw\n`
    );
    const lstat = spyOn(fs, 'lstatSync');
    try {
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        mountInfo,
      });
      const underMount = lstat.mock.calls.filter(([file]) =>
        String(file).startsWith(path.join(value.home, '.gemini') + path.sep)
      );
      expect(underMount).toEqual([]);
    } finally {
      lstat.mockRestore();
    }
  });

  test('a long prompt with a slash stays idle, and an unreadable mount table is incomplete', () => {
    const value = procFixture();
    const prompt = `${'review the change carefully '.repeat(12)}and/or file a bug`;
    for (const argv of [
      ['bash', '-c', `set -e; true && ${'x'.repeat(300)}/etc/hostname`],
      ['node', 'app.js', prompt],
      ['node', 'app.js', `--prompt=${'y'.repeat(300)}/x`],
    ]) {
      value.add(1020, argv[0], `/usr/bin/${argv[0]}`, argv, value.root);
      expect([argv, value.census()]).toEqual([argv, { complete: true, running: 0 }]);
      fs.rmSync(path.join(value.proc, '1020'), { recursive: true });
    }
    expect(
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        mountInfo: path.join(value.root, 'no-such-mountinfo'),
      })
    ).toEqual({ complete: false, running: 0 });
  });

  test('the real /proc census is complete despite non-dumpable same-user processes', () => {
    // Read-only; any native path works. systemd --user, ssh-agent and sshd-session
    // deny their exe link, which must not make the census incomplete forever.
    const root = temporary('aac-agy-real-proc-');
    const census = censusAntigravityProcesses({
      home: root,
      nativeBinary: path.join(root, '.local/bin/agy'),
    });
    expect(census.complete).toBe(true);
  });

  test('another user`s processes are not counted', () => {
    const value = procFixture();
    value.add(400, 'agy', value.native, [value.native]);
    expect(
      censusAntigravityProcesses({
        home: value.home,
        nativeBinary: value.native,
        procRoot: value.proc,
        selfPid: 7,
        uid: (process.getuid?.() ?? 0) + 1,
      })
    ).toEqual({ complete: true, running: 0 });
  });
});

/**
 * The installed bridge runs under Node, so the probe runs there too: Bun reports
 * every failed unix-socket connect as ENOENT, which would hide the very error
 * codes these cases distinguish.
 */
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

  test('a process starting while stillPaused re-hashes the native answers in_use', async () => {
    const value = fixture();
    value.installNewBuild();
    let started = false;
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      census: () => (started ? { complete: true, running: 1 } : idle()),
      stillPaused: async () => {
        started = true; // agy starts during the slow re-hash
        return true;
      },
    });
    expect(result.status).toBe('in_use');
    expect(value.calls).toEqual([['--version']]);
  });

  test('nothing is awaited between the final census and the update command', async () => {
    const value = fixture();
    let fresh = false;
    let freshAtUpdate: boolean | null = null;
    value.setUpdate(async () => {
      freshAtUpdate = fresh;
      return '';
    });
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      census: () => {
        fresh = true;
        queueMicrotask(() => {
          fresh = false;
        });
        return idle();
      },
      stillPaused: async () => true,
    });
    expect(result.status).toBe('current');
    expect(freshAtUpdate).toBe(true);
  });

  test('nothing is awaited before the update even through the command wrapper', async () => {
    const value = fixture();
    let fresh = false;
    let freshAtUpdate: boolean | null = null;
    const result = await runManagedAntigravityUpdateCommand({
      nativeBinary: value.nativeBinary,
      ccsDirectory: value.ccsDirectory,
      runCommand: async (binary, args, capture) => {
        if (args[0] === 'update') freshAtUpdate = fresh;
        return value.runCommand(binary, args, capture);
      },
      execute: (native) =>
        updatePausedAntigravity({
          registry: value.registry,
          native,
          census: () => {
            fresh = true;
            queueMicrotask(() => {
              fresh = false;
            });
            return idle();
          },
          stillPaused: async () => true,
        }),
    });
    expect(result).toMatchObject({ status: 'current', updateAttempted: true });
    expect(freshAtUpdate).toBe(true);
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
      census: idle,
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
          census: idle,
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
      // The first census and the recovery check refuse before any backup.
      if (variant !== 'runtime-back') expect(fs.existsSync(value.backup)).toBe(false);
    }
  });

  test('an incomplete final census after the backup never updates', async () => {
    const value = fixture();
    const answers = [idle(), { complete: false, running: 0 }];
    const result = await updatePausedAntigravity({
      registry: value.registry,
      native: value.native,
      census: () => answers.shift() ?? idle(),
      stillPaused: async () => true,
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
    expect(result).toMatchObject({ status: 'failed', messageCode: 'runtime_paused_blocked' });
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
          nativeReview: () => 'unreviewed',
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

  test('a failed paused result never falls through to the coordinator', async () => {
    const value = fixture();
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
          census: () => ({ complete: false, running: 0 }),
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
          nativeReview: () => 'unreviewed',
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
    ['a listening runtime on an unreviewed build', 'unreviewed', 'listening'],
    ['an unclear runtime socket on an unreviewed build', 'unreviewed', 'unknown'],
    ['a reviewed build with a stopped runtime', 'reviewed', 'stopped'],
    ['an unreadable native with a stopped runtime', 'unknown', 'stopped'],
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
            nativeReview: () => reviewed,
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
      nativeReview: () => 'unreviewed',
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
      census: idle,
    });
    expect(order).toEqual(['socket']);
  });
});

/**
 * The installed entry point with a temporary home: a real descriptor, the real
 * /proc census, the real socket probe and a fixture shell script as agy. The
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
      '  --hold) sleep 30 ;;',
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

  test('a running fixture agy is in_use; nothing is backed up or updated', async () => {
    const value = installedHome();
    const child = spawn(value.native, ['--hold'], { stdio: 'ignore' });
    children.push(child);
    for (let i = 0; i < 100; i++) {
      try {
        if (fs.readFileSync(`/proc/${child.pid}/comm`, 'utf8').trim() === 'agy') break;
      } catch {
        /* not started yet */
      }
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    const result = await runInstalledManagedAntigravityUpdate(value.home);
    expect(result).toEqual({
      appId: 'antigravity-cli',
      platform: 'ubuntu',
      manager: 'native',
      status: 'action_required',
      messageCode: 'in_use',
      previousVersion: '1.4.12',
      version: '1.4.12',
      updateAttempted: false,
      restartedProcesses: 0,
    });
    expect(fs.existsSync(value.log)).toBe(false);
    expect(fs.existsSync(value.backups)).toBe(false);
  });

  async function holdUntilVisible(argv: string[], marker: string) {
    const child = spawn(argv[0], argv.slice(1), { stdio: 'ignore' });
    children.push(child);
    for (let i = 0; i < 100; i++) {
      try {
        if (fs.readFileSync(`/proc/${child.pid}/cmdline`, 'utf8').includes(marker)) return;
      } catch {
        /* not started yet */
      }
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    throw new Error('fixture process never appeared');
  }

  const IN_USE = {
    appId: 'antigravity-cli',
    platform: 'ubuntu',
    manager: 'native',
    status: 'action_required',
    messageCode: 'in_use',
    previousVersion: '1.4.12',
    version: '1.4.12',
    updateAttempted: false,
    restartedProcesses: 0,
  };

  test('the native run through /bin/sh is in_use (matched by its argument, not its name)', async () => {
    const value = installedHome();
    await holdUntilVisible(['/bin/sh', value.native, '--hold'], value.native);
    expect(await runInstalledManagedAntigravityUpdate(value.home)).toEqual(IN_USE);
    expect(fs.existsSync(value.log)).toBe(false);
  });

  test('a script run from the managed runtime folder is in_use', async () => {
    const value = installedHome();
    const bin = path.join(value.home, '.local/share/ai-account-center/antigravity-runtime/bin');
    fs.mkdirSync(bin, { recursive: true, mode: 0o700 });
    const script = path.join(bin, 'fixture-hold.sh');
    fs.writeFileSync(script, 'sleep 30\n:\n', { mode: 0o700 });
    await holdUntilVisible(['/bin/sh', script], script);
    expect(await runInstalledManagedAntigravityUpdate(value.home)).toEqual(IN_USE);
    expect(fs.existsSync(value.log)).toBe(false);
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

  // The real census also sees this machine's own processes: it cannot be idle
  // while a real Antigravity runs here (the in_use cases above still run).
  const hostCensus = censusAntigravityProcesses({
    home: path.join(os.tmpdir(), 'aac-agy-no-such-home'),
    nativeBinary: path.join(os.tmpdir(), 'aac-agy-no-such-home/.local/bin/agy'),
  });
  test.skipIf(!hostCensus.complete || hostCensus.running > 0)(
    'with nothing running the paused path backs up and updates under the lock' +
      ' (skipped while a real Antigravity process runs on this host)',
    async () => {
      const value = installedHome();
      const result = await runInstalledManagedAntigravityUpdate(value.home);
      expect(result).toEqual({
        appId: 'antigravity-cli',
        platform: 'ubuntu',
        manager: 'native',
        status: 'updated',
        messageCode: 'updated',
        previousVersion: '1.4.12',
        version: '1.5.2',
        updateAttempted: true,
        restartedProcesses: 0,
      });
      expect(fs.readFileSync(value.log, 'utf8').trim()).toBe('locked');
      expect(sha(fs.readFileSync(path.join(value.backups, value.originalSha)))).toBe(
        value.originalSha
      );
    }
  );
});
