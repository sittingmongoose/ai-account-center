import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { execFileSync } from 'child_process';
const BRIDGE_SOURCE = path.resolve(
  import.meta.dir,
  '../../../scripts/app-updates/app_update_codex.cjs'
);
const bridge = require(BRIDGE_SOURCE);
const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});
beforeEach(() =>
  Object.assign(bridge.output, {
    appId: 'codex-cli',
    platform: 'ubuntu',
    status: 'failed',
    previousVersion: null,
    version: null,
    manager: 'native',
    messageCode: 'update_failed',
    updateAttempted: false,
    restartedProcesses: 0,
  })
);
function fixture() {
  let clock = 0;
  let version = '1.0.0';
  let pending = false;
  const events: string[] = [];
  const marks: unknown[] = [];
  let busy = 2;
  return {
    events,
    deps: {
      now: () => clock,
      sleep: async (ms: number) => {
        clock += ms;
        events.push('wait');
      },
      readVersion: async () => version,
      update: async () => {
        version = '2.0.0';
        events.push('update');
      },
      pending: () => pending,
      mark: (closed?: Record<string, boolean>) => {
        pending = true;
        marks.push(closed);
        events.push('mark');
      },
      clear: () => {
        pending = false;
        events.push('clear');
      },
      restartProxies: async () => {
        events.push('proxy-reconnect');
      },
      hasOldProxies: async () => false,
      marked: (): Record<string, unknown> | null => null,
      running: async () => ({ desktop: false, daemon: false }),
      runtime: () => ({
        stop: async () => {
          events.push('stop');
          if (busy-- > 0) throw Object.assign(new Error('private'), { code: 'busy' });
        },
        start: async () => {
          events.push('start');
        },
        dispose: async () => {
          events.push('dispose');
        },
      }),
    },
    marks,
    pending: () => pending,
  };
}
describe('Codex update idle and restart coordinator', () => {
  it('queues busy work, then reconnects proxies under stop/start protection', async () => {
    const value = fixture();
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('updated');
    expect(value.events).toEqual([
      'update',
      'mark',
      'stop',
      'wait',
      'stop',
      'wait',
      'stop',
      'mark',
      'proxy-reconnect',
      'start',
      'clear',
      'dispose',
    ]);
    expect(value.pending()).toBe(false);
  });
  it('retains pending restart after a failure so a later explicit click retries', async () => {
    const value = fixture();
    value.deps.runtime = () => ({
      stop: async () => {},
      start: async () => {
        throw new Error('PRIVATE_SENTINEL');
      },
      dispose: async () => {},
    });
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
  });
  it('does not stop a current package without pending restart', async () => {
    const value = fixture();
    value.deps.update = async () => {
      value.events.push('update');
    };
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('current');
    expect(value.events).toEqual(['update']);
  });
  it('gives up on a Codex that stays busy after about a minute, never fifteen', async () => {
    // The live 2026-10-06 run waited 14.8 minutes here while Mac and Windows queued.
    const value = fixture();
    let stops = 0;
    value.deps.runtime = () => ({
      stop: async () => {
        stops++;
        throw Object.assign(new Error('PRIVATE_SENTINEL'), { code: 'busy' });
      },
      start: async () => {
        value.events.push('start');
      },
      dispose: async () => {},
    });
    const before = value.deps.now();
    const result = await bridge.execute(value.deps);
    const waited = value.deps.now() - before;
    expect(waited).toBeLessThanOrEqual(60_000);
    expect(waited).toBeGreaterThanOrEqual(55_000);
    expect(stops).toBeLessThanOrEqual(14);
    expect(result.status).toBe('action_required');
    expect(result.messageCode).toBe('codex_busy');
    expect(result.version).toBe('2.0.0');
    // Nothing was stopped, so nothing restarts; the marker stays for the next click.
    expect(value.events).not.toContain('start');
    expect(value.pending()).toBe(true);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
  });
  it('honours a shorter idle budget', async () => {
    const value = fixture();
    value.deps.runtime = () => ({
      stop: async () => {
        throw Object.assign(new Error('busy'), { code: 'busy' });
      },
      start: async () => {},
      dispose: async () => {},
    });
    const result = await bridge.execute({ ...value.deps, idleSeconds: 10 });
    expect(value.deps.now()).toBeLessThanOrEqual(10_000);
    expect(result.messageCode).toBe('codex_busy');
  });
  it('reports an unreplaced proxy as a restart failure', async () => {
    const value = fixture();
    value.deps.hasOldProxies = async () => true;
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
  });
});
describe('Codex update runtime resolution', () => {
  function write(file: string, text: string): string {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
    return file;
  }
  /** A flat remote helper folder holding the bundle, and an installed AAC package. */
  function layout() {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-runtime-'));
    directories.push(root);
    const bundle = path.join(root, 'app-updates', 'app_update_codex_runtime.cjs');
    const installed = path.join(root, 'pkg', 'dist', 'codex-auth', 'codex-activation-runtime.js');
    write(installed, "exports.createCodexActivationRuntime = () => 'installed';\n");
    write(
      path.join(root, 'pkg', 'node_modules', 'proper-lockfile', 'index.js'),
      "exports.lock = 'installed-lock';\n"
    );
    return { bundle, installed };
  }

  it('tries the bundle beside the helper first, then the installed packages', () => {
    const helpers = path.dirname(
      require.resolve('../../../scripts/app-updates/app_update_codex.cjs')
    );
    const packages = path.join(os.homedir(), '.local', 'lib', 'node_modules');
    expect(bridge.modules[0]).toBe(path.join(helpers, 'app_update_codex_runtime.cjs'));
    expect(bridge.modules[1]).toBe(
      path.resolve(helpers, '..', '..', 'dist', 'codex-auth', 'codex-activation-runtime.js')
    );
    expect(bridge.modules.slice(2)).toEqual([
      path.join(
        packages,
        '@sittingmongoose',
        'ai-account-center',
        'dist',
        'codex-auth',
        'codex-activation-runtime.js'
      ),
      path.join(
        packages,
        '@kaitranntt',
        'ccs',
        'dist',
        'codex-auth',
        'codex-activation-runtime.js'
      ),
    ]);
  });

  it('loads the bundle and its own lock library when the bundle is present', () => {
    const { bundle, installed } = layout();
    write(
      bundle,
      "exports.createCodexActivationRuntime = () => 'bundle';\nexports.lockfile = { lock: 'bundle-lock' };\n"
    );
    const loaded = bridge.loadRuntime([bundle, installed]);
    expect(loaded.selected).toBe(bundle);
    expect(loaded.createCodexActivationRuntime()).toBe('bundle');
    expect(loaded.lockfile.lock).toBe('bundle-lock');
  });

  it('falls back to the installed package and its node_modules lock library', () => {
    const { bundle, installed } = layout();
    const loaded = bridge.loadRuntime([bundle, installed]);
    expect(loaded.selected).toBe(installed);
    expect(loaded.createCodexActivationRuntime()).toBe('installed');
    expect(loaded.lockfile.lock).toBe('installed-lock');
  });

  it('finds nothing when no runtime exists', () => {
    const { bundle } = layout();
    expect(bridge.loadRuntime([bundle])).toBeNull();
  });
});

describe('Codex desktop version verification', () => {
  it('does not report a successful installer with unchanged bits as updated', async () => {
    const value = fixture();
    value.deps.update = async () => {};
    const result = await bridge.execute({
      ...value.deps,
      operation: 'desktop',
      expectedCandidate: '2.0.0',
    });
    expect(result.status).toBe('failed');
    expect(value.events).not.toContain('mark');
  });
  it('can recover a pending restart without requiring another version change', async () => {
    const value = fixture();
    value.deps.mark();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '1.0.0', desktop: true }),
      running: async () => ({ desktop: true, daemon: false }),
      operation: 'desktop',
      expectedCandidate: '1.0.0',
      restartOnly: true,
    });
    expect(result.status).toBe('updated');
    expect(value.events).not.toContain('update');
    expect(value.pending()).toBe(false);
  });
  it('rejects a changed version that is not the requested candidate', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'desktop',
      expectedCandidate: '3.0.0',
    });
    expect(result.status).not.toBe('updated');
  });
});

describe('Codex relaunch after an update', () => {
  const dashboard =
    '0::/user.slice/user-1000.slice/user@1000.service/app.slice/ccs-dashboard.service\n';
  const target = {
    exe: '/old/codex',
    args: ['codex', 'app-server', 'daemon'],
    cwd: '/work',
    env: { KEEP: 'yes', CCS_SESSION_SECRET: 'hidden', CCS_DASHBOARD_USERNAME: 'hidden' },
  };
  function spawned() {
    const calls: unknown[][] = [];
    return {
      calls,
      spawn: (...args: unknown[]) => {
        calls.push(args);
        return {
          once: (name: string, done: () => void) => {
            if (name === 'spawn') done();
          },
          unref: () => {},
        };
      },
    };
  }
  beforeEach(() => {
    bridge.output.restartedProcesses = 0;
  });

  it('never spawns inside the dashboard cgroup, so the app is not charged to the dashboard', async () => {
    const { calls, spawn } = spawned();
    const outside: unknown[][] = [];
    await bridge.relaunch(
      target,
      false,
      async (...args: unknown[]) => {
        outside.push(args);
      },
      { spawn, inside: () => bridge.insideServiceCgroup(() => dashboard), executable: '/new/codex' }
    );
    expect(calls).toEqual([]);
    expect(outside).toEqual([
      [
        {
          exe: '/new/codex',
          args: target.args,
          cwd: '/work',
          env: { KEEP: 'yes' },
        },
        false,
      ],
    ]);
    expect(bridge.output.restartedProcesses).toBe(1);
  });

  it('reports a failed outside launch instead of falling back into the cgroup', async () => {
    const { calls, spawn } = spawned();
    await expect(
      bridge.relaunch(
        target,
        true,
        async () => {
          throw new Error('no systemd-run');
        },
        { spawn, inside: () => true }
      )
    ).rejects.toThrow('no systemd-run');
    expect(calls).toEqual([]);
    expect(bridge.output.restartedProcesses).toBe(0);
  });

  it('starts directly from an ssh session, where there is no dashboard cgroup to leave', async () => {
    const { calls, spawn } = spawned();
    await bridge.relaunch(target, false, async () => {}, {
      spawn,
      inside: () => false,
      executable: '/new/codex',
    });
    expect(calls.length).toBe(1);
    expect(calls[0][0]).toBe('/new/codex');
    expect((calls[0][2] as { env: object }).env).toEqual({ KEEP: 'yes' });
    expect(bridge.output.restartedProcesses).toBe(1);
  });

  it('fails closed when an older runtime offers no launcher inside the dashboard', async () => {
    const { calls, spawn } = spawned();
    await expect(
      bridge.relaunch(target, false, undefined, { spawn, inside: () => true })
    ).rejects.toThrow('Launch failed.');
    expect(calls).toEqual([]);
    expect(bridge.output.restartedProcesses).toBe(0);
  });

  it('hands the runtime launcher through the update runtime factory', async () => {
    const { calls, spawn } = spawned();
    const outside: unknown[][] = [];
    let overrides: {
      launch: (t: unknown, d: boolean, o: (...args: unknown[]) => Promise<void>) => Promise<void>;
      scan: unknown;
    } | null = null;
    const created = bridge.createUpdateRuntime(
      (codexHome: string, given: NonNullable<typeof overrides>) => {
        overrides = given;
        return codexHome;
      },
      { spawn, inside: () => true, executable: '/new/codex' }
    );
    expect(created.endsWith('.codex')).toBe(true);
    await overrides!.launch(target, false, async (...args: unknown[]) => {
      outside.push(args);
    });
    expect(calls).toEqual([]);
    expect(outside.length).toBe(1);
    expect((outside[0][0] as { exe: string }).exe).toBe('/new/codex');
    expect(typeof overrides!.scan).toBe('function');
  });

  it('tells a user service from a session, a slice and an unreadable file', () => {
    expect(bridge.insideServiceCgroup(() => dashboard)).toBe(true);
    for (const nested of ['/sub', '/sub.scope', '/a/b.scope'])
      expect(bridge.insideServiceCgroup(() => `${dashboard.trim()}${nested}\n`)).toBe(true);
    expect(
      bridge.insideServiceCgroup(
        () => '0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.slice/y.service/z\n'
      )
    ).toBe(true);
    for (const cgroup of [
      '0::/user.slice/user-1000.slice/user@1000.service/init.scope\n',
      '0::/user.slice/user-1000.slice/session-3.scope\n',
      '0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-agents.slice\n',
      '0::/system.slice/ssh.service\n',
    ])
      expect(bridge.insideServiceCgroup(() => cgroup)).toBe(false);
    expect(
      bridge.insideServiceCgroup(() => {
        throw new Error('no proc');
      })
    ).toBe(false);
  });
});

describe('Pending-marker retry', () => {
  beforeEach(() => {
    bridge.output.restartedProcesses = 0;
  });
  const idleRuntime = () => ({
    stop: async () => {},
    start: async () => {},
    dispose: async () => {},
  });
  type Live = { desktop: boolean; daemon: boolean };
  const nothing: Live = { desktop: false, daemon: false };
  /** A second click: the marker as given, and what is running before stop() and after start(). */
  function retry(marker: Record<string, unknown> | null, after: Live, before: Live = nothing) {
    const value = fixture();
    if (marker) value.deps.mark();
    value.marks.length = 0;
    let stopped = false;
    return {
      value,
      deps: {
        ...value.deps,
        update: async () => {},
        marked: () => (marker ? { version: '1.0.0', ...marker } : null),
        running: async () => (stopped ? after : before),
        operation: 'cli',
        runtime: () => ({
          ...idleRuntime(),
          stop: async () => {},
          start: async () => {
            stopped = true;
          },
        }),
      },
    };
  }
  const desktopRetry = (deps: object) => ({
    ...deps,
    operation: 'desktop',
    expectedCandidate: '1.0.0',
    restartOnly: true,
  });

  it('reports restart_not_running when the daemon it closed stays closed', async () => {
    const { value, deps } = retry({ daemon: true }, nothing);
    const result = await bridge.execute(deps);
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(value.pending()).toBe(false);
  });

  it('stays updated once what the marker names is running again', async () => {
    const { value, deps } = retry({ daemon: true }, { desktop: false, daemon: true });
    expect((await bridge.execute(deps)).status).toBe('updated');
    expect(value.pending()).toBe(false);
  });

  it('never applies to a first update, where no marker existed', async () => {
    const value = fixture();
    const result = await bridge.execute({ ...value.deps, operation: 'cli', runtime: idleRuntime });
    expect(result.status).toBe('updated');
  });

  it('requires the daemon and the desktop when a run closed both', async () => {
    const both = { desktop: true, daemon: true };
    for (const [after, status] of [
      [{ desktop: true, daemon: false }, 'restart_failed'],
      [{ desktop: false, daemon: true }, 'restart_failed'],
      [both, 'updated'],
    ] as Array<[Live, string]>) {
      expect((await bridge.execute(retry(both, after).deps)).status).toBe(status);
    }
  });

  it('holds a user-reopened desktop to the daemon the marker also names', async () => {
    const { deps } = retry(
      { daemon: true },
      { desktop: true, daemon: false },
      { desktop: true, daemon: false }
    );
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
  });

  it('holds a desktop retry to the desktop, even with the daemon back', async () => {
    const { deps } = retry({ desktop: true, daemon: true }, { desktop: false, daemon: true });
    expect((await bridge.execute(desktopRetry(deps))).messageCode).toBe('restart_not_running');
  });

  it('counts a desktop marker from before these fields as a closed desktop', async () => {
    const { deps } = retry({}, { desktop: false, daemon: true });
    expect((await bridge.execute(desktopRetry(deps))).messageCode).toBe('restart_not_running');
  });

  it('does not count a CLI marker that names nothing as a retry', async () => {
    const { value, deps } = retry({}, nothing);
    const result = await bridge.execute(deps);
    expect(result.status).toBe('updated');
    expect(value.pending()).toBe(false);
  });

  it('does not demand what the marker says nothing closed', async () => {
    const { deps } = retry({ desktop: false, daemon: false }, nothing);
    expect((await bridge.execute(desktopRetry(deps))).status).toBe('updated');
  });

  it('keeps a real upgrade with a stale marker clean', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '0.9.0', desktop: true, daemon: true }),
      operation: 'cli',
      runtime: idleRuntime,
    });
    expect(result.status).toBe('updated');
  });

  it('writes what was closed only after stop(), and keeps it across a retry', async () => {
    const first = fixture();
    let atStop: unknown[] = [];
    await bridge.execute({
      ...first.deps,
      running: async () => ({ desktop: true, daemon: true }),
      operation: 'cli',
      runtime: () => ({
        ...idleRuntime(),
        stop: async () => {
          atStop = [...first.marks];
        },
      }),
    });
    expect(atStop).toEqual([undefined]);
    expect(first.marks).toEqual([undefined, { desktop: true, daemon: true }]);

    const { value, deps } = retry({ desktop: true }, nothing, { desktop: false, daemon: true });
    await bridge.execute(deps);
    expect(value.marks).toEqual([
      { desktop: true, daemon: false },
      { desktop: true, daemon: true },
    ]);
  });

  it('leaves a busy run with a marker that names nothing', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      idleSeconds: 1,
      running: async () => ({ desktop: true, daemon: true }),
      operation: 'cli',
      runtime: () => ({
        ...idleRuntime(),
        stop: async () => {
          throw Object.assign(new Error('private'), { code: 'busy' });
        },
      }),
    });
    expect(result.messageCode).toBe('codex_busy');
    expect(value.marks).toEqual([undefined]);
  });
});

describe('What counts as running', () => {
  const home = os.homedir();
  const codexHome = path.join(home, '.codex');
  const desktop = '/usr/lib/chatgpt/ChatGPT';
  const item = (
    pid: number,
    ppid: number,
    exe: string,
    args: string[],
    env: Record<string, string> = {}
  ) => ({ pid, ppid, exe, args, env });
  const server = (ppid: number, env: Record<string, string> = { HOME: home }) =>
    item(11, ppid, '/usr/lib/chatgpt/resources/codex', ['codex', 'app-server'], env);

  it('counts a desktop only with a live app server on this home under it', () => {
    const main = item(10, 1, desktop, [desktop]);
    const ready = (...rest: object[]) => bridge.desktopReady([main, ...rest], codexHome);
    expect(ready(server(10))).toBe(true);
    // A helper between them still counts as under the main process.
    expect(ready(item(12, 10, desktop, [desktop, '--type=utility']), server(12))).toBe(true);
    expect(ready()).toBe(false);
    expect(ready(server(99))).toBe(false);
    expect(ready(server(10, { CODEX_HOME: '/t3/shadow/.codex' }))).toBe(false);
    expect(
      ready(
        item(11, 10, '/usr/lib/chatgpt/resources/codex', ['codex', 'app-server', 'proxy'], {
          HOME: home,
        })
      )
    ).toBe(false);
  });

  it('does not take a renderer for the main process', () => {
    const renderer = item(10, 1, desktop, [`${desktop} --type=zygote`]);
    expect(bridge.desktopReady([renderer, server(10)], codexHome)).toBe(false);
  });

  it('counts only the shared daemon on this home as the daemon', () => {
    const daemon = (args: string[], env: Record<string, string>) =>
      bridge.daemonRunning([item(20, 1, '/fixture/bin/codex', args, env)], codexHome);
    const listen = ['codex', 'app-server', '--listen', 'unix://'];
    expect(daemon(listen, { HOME: home })).toBe(true);
    expect(daemon(listen, { CODEX_HOME: codexHome })).toBe(true);
    expect(daemon(listen, { CODEX_HOME: '/t3/shadow/.codex' })).toBe(false);
    expect(daemon(['codex', 'app-server'], { HOME: home })).toBe(false);
    expect(daemon(['codex', 'app-server', 'proxy', '--listen', 'unix://'], { HOME: home })).toBe(
      false
    );
    expect(daemon(['codex', 'exec'], { HOME: home })).toBe(false);
    expect(bridge.daemonRunning([server(10)], codexHome)).toBe(false);
  });
});

describe('The bridge run as a program, with a stand-in runtime and codex', () => {
  // A stand-in /proc: never reads this machine's processes, and reads as inside the dashboard.
  const RUNTIME = `
const fs = require('fs');
const real = { readdirSync: fs.readdirSync, readFileSync: fs.readFileSync, statSync: fs.statSync, readlinkSync: fs.readlinkSync };
let procs = JSON.parse(process.env.FIXTURE_PROCS || '[]');
const proc = (file) => {
  const match = /^\\/proc\\/(\\d+)(?:\\/(\\w+))?$/.exec(String(file));
  return match ? { item: procs.find((row) => String(row.pid) === match[1]), part: match[2] } : null;
};
const gone = () => Object.assign(new Error('gone'), { code: 'ENOENT' });
fs.readdirSync = (dir, ...rest) => dir === '/proc' ? procs.map((row) => String(row.pid)) : real.readdirSync(dir, ...rest);
fs.statSync = (file, ...rest) => {
  const found = proc(file);
  if (!found) return real.statSync(file, ...rest);
  if (!found.item) throw gone();
  return { uid: process.getuid() };
};
fs.readlinkSync = (file, ...rest) => {
  const found = proc(file);
  if (!found) return real.readlinkSync(file, ...rest);
  if (!found.item) throw gone();
  return found.part === 'exe' ? found.item.exe : '/';
};
fs.readFileSync = (file, ...rest) => {
  if (file === '/proc/self/cgroup') return '0::/user.slice/user-1000.slice/user@1000.service/app.slice/ccs-dashboard.service\\n';
  const found = proc(file);
  if (!found) return real.readFileSync(file, ...rest);
  if (!found.item) throw gone();
  const { pid, ppid, args, env } = found.item;
  if (found.part === 'stat') return pid + ' (x) S ' + ppid + ' ' + Array(20).fill('1').join(' ');
  if (found.part === 'cmdline') return args.join('\\0') + '\\0';
  return Object.entries(env).map(([key, value]) => key + '=' + value).join('\\0');
};
exports.lockfile = { lock: async () => async () => {} };
exports.createCodexActivationRuntime = (home, overrides) => ({
  stop: async () => {
    if (process.env.FIXTURE_AT_STOP) fs.copyFileSync(process.env.FIXTURE_MARKER, process.env.FIXTURE_AT_STOP);
    procs = [];
  },
  start: async () => {
    if (process.env.FIXTURE_START === 'fail') throw new Error('private');
    if (process.env.FIXTURE_START !== 'launch') return;
    await overrides.launch({ exe: '/old/codex', args: ['codex', 'app-server'], cwd: '/', env: { KEEP: 'yes' } }, false,
      async (target) => { fs.writeFileSync(process.env.FIXTURE_RECORD, JSON.stringify(target)); });
  },
});
`;
  function run(
    start: 'launch' | 'fail' | 'idle',
    options: { marker?: object | null; next?: string; procs?: (root: string) => object[] } = {}
  ) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-bridge-'));
    directories.push(root);
    const script = path.join(root, 'bridge', 'app_update_codex.cjs');
    fs.mkdirSync(path.dirname(script));
    fs.copyFileSync(BRIDGE_SOURCE, script);
    fs.writeFileSync(path.join(root, 'bridge', 'app_update_codex_runtime.cjs'), RUNTIME);
    const codex = path.join(root, '.local/bin/codex');
    fs.mkdirSync(path.dirname(codex), { recursive: true });
    fs.writeFileSync(path.join(root, 'version'), '2.0.0\n');
    const update = options.next ? `echo ${options.next} > "$HOME/version"` : ':';
    fs.writeFileSync(
      codex,
      `#!/bin/sh\nif [ "$1" = update ]; then ${update}; exit 0; fi\necho "codex-cli $(cat "$HOME/version")"\n`,
      { mode: 0o755 }
    );
    fs.mkdirSync(path.join(root, '.codex'));
    const marker = path.join(root, '.ccs/app-updates/codex-cli-pending-restart.json');
    fs.mkdirSync(path.dirname(marker), { recursive: true });
    if (options.marker !== null)
      fs.writeFileSync(marker, JSON.stringify(options.marker ?? { version: '2.0.0' }));
    const record = path.join(root, 'launched.json');
    const atStop = path.join(root, 'marker-at-stop.json');
    const stdout = execFileSync('node', [script, '--operation', 'cli'], {
      env: {
        PATH: process.env.PATH,
        HOME: root,
        FIXTURE_RECORD: record,
        FIXTURE_START: start,
        FIXTURE_MARKER: marker,
        FIXTURE_AT_STOP: atStop,
        FIXTURE_PROCS: JSON.stringify(options.procs?.(root) ?? []),
      },
      encoding: 'utf8',
    });
    return { result: JSON.parse(stdout), record, marker, codex, atStop };
  }
  const running = (root: string) => [
    {
      pid: 101,
      ppid: 1,
      exe: path.join(root, '.local/bin/codex'),
      args: ['codex', 'app-server', '--listen', 'unix://'],
      env: { HOME: root },
    },
    { pid: 102, ppid: 1, exe: '/usr/lib/chatgpt/ChatGPT', args: ['ChatGPT'], env: { HOME: root } },
    {
      pid: 103,
      ppid: 102,
      exe: '/usr/lib/chatgpt/resources/codex',
      args: ['codex', 'app-server'],
      env: { HOME: root },
    },
  ];
  const read = (file: string) => JSON.parse(fs.readFileSync(file, 'utf8'));

  it('starts the updated executable through the runtime launcher', () => {
    const { result, record, marker, codex } = run('launch', {
      marker: { version: '2.0.0', daemon: true },
      procs: running,
    });
    expect(result.status).toBe('restart_failed');
    expect(result.restartedProcesses).toBe(1);
    const target = read(record);
    expect(target.exe).toBe(codex);
    expect(target.env).toEqual({ KEEP: 'yes' });
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('asks the real process check before calling a retry a restart', () => {
    const { result, record, marker } = run('idle', { marker: { version: '2.0.0', daemon: true } });
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(fs.existsSync(record)).toBe(false);
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('writes what stop() closed to the real marker, and nothing before stop()', () => {
    const { result, marker, atStop } = run('fail', {
      marker: null,
      next: '3.0.0',
      procs: running,
    });
    expect(result.status).toBe('restart_failed');
    expect(read(atStop)).toEqual({ version: '3.0.0' });
    expect(read(marker)).toEqual({ version: '3.0.0', desktop: true, daemon: true });
  });

  it('records only what was running on the real marker', () => {
    const { marker } = run('fail', { marker: null, next: '3.0.0' });
    expect(read(marker)).toEqual({ version: '3.0.0', desktop: false, daemon: false });
  });

  it('keeps what an earlier run closed on the real marker across a failed retry', () => {
    const { marker } = run('fail', { marker: { version: '2.0.0', desktop: true } });
    expect(read(marker)).toEqual({ version: '2.0.0', desktop: true, daemon: false });
  });

  it('does not turn a marker that names nothing into a red row', () => {
    const { result, marker } = run('idle', { marker: { version: '2.0.0' } });
    expect(result.status).toBe('updated');
    expect(fs.existsSync(marker)).toBe(false);
  });
});
