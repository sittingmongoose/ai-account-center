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
      mark: (desktop?: boolean) => {
        pending = true;
        marks.push(desktop);
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
      markedDesktop: (): boolean | undefined => undefined,
      desktopRunning: async () => false,
      serverRunning: async () => true,
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
      desktopRunning: async () => true,
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

describe('Pending-marker retry that finds nothing to restart', () => {
  beforeEach(() => {
    bridge.output.restartedProcesses = 0;
  });
  const idleRuntime = () => ({
    stop: async () => {},
    start: async () => {},
    dispose: async () => {},
  });
  function retry(server: boolean, desktop = false, marked?: boolean) {
    const value = fixture();
    value.deps.mark();
    value.marks.length = 0;
    return {
      value,
      deps: {
        ...value.deps,
        update: async () => {},
        serverRunning: async () => server,
        desktopRunning: async () => desktop,
        markedDesktop: () => marked,
        operation: 'cli',
        runtime: idleRuntime,
      },
    };
  }

  it('does not report a restart when it stopped and started nothing', async () => {
    const { value, deps } = retry(false);
    const result = await bridge.execute(deps);
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(value.pending()).toBe(false);
  });

  it('stays updated when a retry really restarted something', async () => {
    const { value, deps } = retry(false);
    deps.runtime = () => ({
      ...idleRuntime(),
      start: async () => {
        bridge.output.restartedProcesses++;
      },
    });
    expect((await bridge.execute(deps)).status).toBe('updated');
    expect(value.pending()).toBe(false);
  });

  it('stays updated when the daemon is already running on the new version', async () => {
    const { deps } = retry(true);
    expect((await bridge.execute(deps)).status).toBe('updated');
  });

  it('never applies to a first update, where no marker existed', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      serverRunning: async () => false,
      operation: 'cli',
      runtime: idleRuntime,
    });
    expect(result.status).toBe('updated');
  });

  it('holds a CLI retry to a desktop its marker says was closed', async () => {
    const { deps } = retry(true, false, true);
    deps.runtime = () => ({
      ...idleRuntime(),
      start: async () => {
        bridge.output.restartedProcesses++;
      },
    });
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
  });

  it('does not hold a CLI retry to a desktop when the marker names none', async () => {
    const { deps } = retry(true, false, undefined);
    expect((await bridge.execute(deps)).status).toBe('updated');
  });

  it('holds a desktop retry to the desktop, even after restarting the daemon', async () => {
    for (const marked of [true, undefined]) {
      const { deps } = retry(true, false, marked);
      bridge.output.restartedProcesses = 0;
      deps.runtime = () => ({
        ...idleRuntime(),
        start: async () => {
          bridge.output.restartedProcesses++;
        },
      });
      const result = await bridge.execute({
        ...deps,
        operation: 'desktop',
        expectedCandidate: '1.0.0',
        restartOnly: true,
      });
      expect(result.messageCode).toBe('restart_not_running');
    }
  });

  it('does not demand a desktop the marker says was never open', async () => {
    const { deps } = retry(true, false, false);
    const result = await bridge.execute({
      ...deps,
      operation: 'desktop',
      expectedCandidate: '1.0.0',
      restartOnly: true,
    });
    expect(result.status).toBe('updated');
  });

  it('records in the marker whether a desktop was open, and keeps it across a retry', async () => {
    const first = fixture();
    await bridge.execute({ ...first.deps, desktopRunning: async () => true, operation: 'cli' });
    expect(first.marks).toEqual([true]);
    const { value, deps } = retry(false, false, true);
    await bridge.execute({
      ...deps,
      operation: 'desktop',
      expectedCandidate: '1.0.0',
      restartOnly: true,
    });
    expect(value.marks).toEqual([true]);
  });
});

describe('What counts as running', () => {
  const home = os.homedir();
  const item = (exe: string, args: string[], env: Record<string, string> = {}) => ({
    exe,
    args,
    env,
  });
  it('counts only a Codex Desktop main process as the desktop', () => {
    const desktop = '/usr/lib/chatgpt/ChatGPT';
    expect(bridge.desktopRunning([item(desktop, [desktop])])).toBe(true);
    expect(bridge.desktopRunning([item(desktop, [desktop, '--type=renderer'])])).toBe(false);
    expect(bridge.desktopRunning([item(desktop, [`${desktop} --type=zygote`])])).toBe(false);
    expect(bridge.desktopRunning([item('/fixture/bin/codex', ['codex', 'app-server'])])).toBe(
      false
    );
  });
  it('counts only an app server on this Codex home', () => {
    const codexHome = path.join(home, '.codex');
    const server = (args: string[], env: Record<string, string>) =>
      bridge.serverRunning([item('/fixture/bin/codex', args, env)], codexHome);
    expect(server(['codex', 'app-server', '--listen', 'unix://'], { HOME: home })).toBe(true);
    expect(server(['codex', 'app-server'], { CODEX_HOME: codexHome })).toBe(true);
    expect(server(['codex', 'app-server', 'proxy'], { HOME: home })).toBe(false);
    expect(server(['codex', 'app-server'], { CODEX_HOME: '/t3/shadow/.codex' })).toBe(false);
    expect(server(['codex', 'exec'], { HOME: home })).toBe(false);
    expect(
      bridge.serverRunning(
        [item('/usr/bin/node', ['node', 'app-server'], { HOME: home })],
        codexHome
      )
    ).toBe(false);
  });
});

describe('The bridge run as a program, with a stand-in runtime and codex', () => {
  const RUNTIME = `
const fs = require('fs');
const realDir = fs.readdirSync, realRead = fs.readFileSync;
// Never scans this machine's processes, and reads as running inside the dashboard's service.
fs.readdirSync = (dir, ...rest) => dir === '/proc' ? [] : realDir(dir, ...rest);
fs.readFileSync = (file, ...rest) => file === '/proc/self/cgroup'
  ? '0::/user.slice/user-1000.slice/user@1000.service/app.slice/ccs-dashboard.service\\n'
  : realRead(file, ...rest);
exports.lockfile = { lock: async () => async () => {} };
exports.createCodexActivationRuntime = (home, overrides) => ({
  stop: async () => {},
  start: async () => {
    if (process.env.FIXTURE_LAUNCH !== '1') return;
    await overrides.launch({ exe: '/old/codex', args: ['codex', 'app-server'], cwd: '/', env: { KEEP: 'yes' } }, false,
      async (target) => { fs.writeFileSync(process.env.FIXTURE_RECORD, JSON.stringify(target)); });
  },
});
`;
  function run(launch: boolean) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-bridge-'));
    directories.push(root);
    const script = path.join(root, 'bridge', 'app_update_codex.cjs');
    fs.mkdirSync(path.dirname(script));
    fs.copyFileSync(BRIDGE_SOURCE, script);
    fs.writeFileSync(path.join(root, 'bridge', 'app_update_codex_runtime.cjs'), RUNTIME);
    const codex = path.join(root, '.local/bin/codex');
    fs.mkdirSync(path.dirname(codex), { recursive: true });
    fs.writeFileSync(codex, '#!/bin/sh\necho codex-cli 2.0.0\n', { mode: 0o755 });
    fs.mkdirSync(path.join(root, '.codex'));
    const marker = path.join(root, '.ccs/app-updates/codex-cli-pending-restart.json');
    fs.mkdirSync(path.dirname(marker), { recursive: true });
    fs.writeFileSync(marker, JSON.stringify({ version: '2.0.0' }));
    const record = path.join(root, 'launched.json');
    const stdout = execFileSync('node', [script, '--operation', 'cli'], {
      env: {
        PATH: process.env.PATH,
        HOME: root,
        FIXTURE_RECORD: record,
        FIXTURE_LAUNCH: launch ? '1' : '0',
      },
      encoding: 'utf8',
    });
    return { result: JSON.parse(stdout), record, marker, codex };
  }

  it('starts the updated executable through the runtime launcher', () => {
    const { result, record, marker, codex } = run(true);
    expect(result.status).toBe('updated');
    expect(result.restartedProcesses).toBe(1);
    const target = JSON.parse(fs.readFileSync(record, 'utf8'));
    expect(target.exe).toBe(codex);
    expect(target.env).toEqual({ KEEP: 'yes' });
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('asks the real process check before calling a retry a restart', () => {
    const { result, record, marker } = run(false);
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(fs.existsSync(record)).toBe(false);
    expect(fs.existsSync(marker)).toBe(false);
  });
});
