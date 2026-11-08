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
/** The daemon a fake readiness check vouches for, and a fake scan finds. */
const DAEMON = { pid: 1, startTime: '1' };
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
  let stored: Record<string, unknown> | null = null;
  const restored: string[] = [];
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
        stored = { version: '1.0.0', ...closed };
        marks.push(closed);
        events.push('mark');
      },
      clear: () => {
        pending = false;
        stored = null;
        events.push('clear');
      },
      restartProxies: async () => {
        events.push('proxy-reconnect');
      },
      hasOldProxies: async () => false,
      marked: (): Record<string, unknown> | null => stored,
      markerSnapshot: (): string | null => (stored ? JSON.stringify(stored) : null),
      restoreMarker: (text: string) => {
        stored = JSON.parse(text);
        restored.push(text);
      },
      runtime: () => ({
        stopPlan: () => ({ daemon: false, desktops: 0 }),
        retiredPlan: async () => ({ daemon: false, desktops: 0 }),
        restartState: async () => ({ daemon: false, desktops: 0, proxy: false }),
        verifyRunningDaemon: async () => DAEMON,
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
    restored,
    pending: () => pending,
  };
}
describe('Codex update idle and restart coordinator', () => {
  it('queues busy work, then reconnects proxies under stop/start protection', async () => {
    const value = fixture();
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('updated');
    expect(value.events).toEqual([
      'mark',
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
    // The install intent, then the marker from before it put back: there was none.
    expect(value.events).toEqual(['mark', 'update', 'clear']);
    expect(value.pending()).toBe(false);
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
    // Written when stop() returned, then cleared: the failed install owes nothing with all back.
    expect(value.events).toContain('clear');
    expect(value.pending()).toBe(false);
  });
  it('can recover a pending restart without requiring another version change', async () => {
    const value = fixture();
    value.deps.mark();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '1.0.0', daemon: true }),
      runtime: () => ({
        ...value.deps.runtime(),
        restartState: async () => ({ daemon: true, daemonIdentity: DAEMON, proxy: false }),
      }),
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

  it('reads the transient unit a process runs in from its cgroup', () => {
    const unit = 'aac-launch-codex-desktop-1-ab.service';
    expect(
      bridge.cgroupUnit(`0::/user.slice/user-1000.slice/user@1000.service/app.slice/${unit}\n`)
    ).toBe(unit);
    expect(bridge.cgroupUnit('0::/user.slice/user-1000.slice/session-3.scope\n')).toBeUndefined();
    expect(bridge.cgroupUnit('')).toBeUndefined();
  });

  it('hands back what it started, so start() can know its own process', async () => {
    // Outside the cgroup: whatever the launcher reports, such as its transient unit.
    const outside = await bridge.relaunch(target, true, async () => ({ unit: 'aac-launch-x' }), {
      inside: () => true,
    });
    expect(outside).toEqual({ unit: 'aac-launch-x' });
    // A direct start: the pid it spawned.
    const direct = await bridge.relaunch(target, true, async () => {}, {
      spawn: () => ({
        pid: 4242,
        once: (name: string, done: () => void) => {
          if (name === 'spawn') done();
        },
        unref: () => {},
      }),
      inside: () => false,
      startTimeOf: (pid: number) => (pid === 4242 ? '777' : undefined),
    });
    // With its start time, so a later process given the same pid is never taken for it.
    expect(direct).toEqual({ pid: 4242, startTime: '777' });
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
  type Plan = { daemon?: boolean; desktops?: number };
  /** What runs after start(). */
  type Live = {
    daemon?: boolean;
    proxy?: boolean;
    daemonIdentity?: { pid: number; startTime: string } | null;
    /** How many of the desktops this run closed start() relaunched and saw come up. */
    desktops?: number;
  };
  const state = ({ desktops: _desktops, ...live }: Live = {}) => ({
    daemon: false,
    proxy: false,
    daemonIdentity: live.daemon ? DAEMON : null,
    ...live,
  });
  const plan = (value: Plan = {}) => ({ daemon: false, desktops: 0, ...value });
  /** The runtime a click meets: what its stop() plans to close, and what runs after start(). */
  const runtime = (planned: Plan = {}, after: Live = {}) => ({
    stop: async () => {},
    start: async () => {},
    dispose: async () => {},
    stopPlan: () => plan(planned),
    retiredPlan: async () => ({
      daemon: planned.daemon ?? false,
      desktops: Math.max(0, (planned.desktops ?? 0) - (after.desktops ?? 0)),
    }),
    restartState: async () => state(after),
    verifyRunningDaemon: async () => DAEMON,
  });
  function retry(marker: Record<string, unknown> | null, after: Live = {}, planned: Plan = {}) {
    const value = fixture();
    if (marker) value.deps.mark();
    value.marks.length = 0;
    return {
      value,
      deps: {
        ...value.deps,
        update: async () => {},
        pending: () => marker !== null,
        marked: () => (marker ? { version: '1.0.0', ...marker } : null),
        operation: 'cli',
        runtime: () => runtime(planned, after),
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
    const { value, deps } = retry({ daemon: true });
    const result = await bridge.execute(deps);
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(value.pending()).toBe(false);
  });

  it('stays updated once what the marker names is running again', async () => {
    const { value, deps } = retry({ daemon: true }, { daemon: true });
    expect((await bridge.execute(deps)).status).toBe('updated');
    expect(value.pending()).toBe(false);
  });

  it('never applies to a first update, where no marker existed', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => runtime(),
    });
    expect(result.status).toBe('updated');
  });

  it('never clears a desktop an earlier run owes, whatever runs now', async () => {
    // Only AAC's own relaunch of a desktop, seen coming up, clears it; that run's launcher is gone.
    for (const marker of [
      { desktop: true, desktops: 2 },
      { desktop: true, desktops: ['a'.repeat(32), 'b'.repeat(32)] },
      { desktop: true },
    ]) {
      for (const desktops of [0, 1, 2]) {
        const { value, deps } = retry(marker, { desktops }, { desktops });
        const result = await bridge.execute(desktopRetry(deps));
        expect([result.status, result.messageCode]).toEqual([
          'restart_failed',
          'restart_not_running',
        ]);
        // Said once, then cleared: nothing here could ever finish it.
        expect(value.pending()).toBe(false);
      }
    }
  });

  it('owes the desktops a run closed even when the daemon it closed is back', async () => {
    const marker = { desktop: true, daemon: true, desktops: 1 };
    for (const after of [
      { desktops: 1 },
      { daemon: true },
      { daemon: true, desktops: 1 },
    ] as Live[]) {
      expect((await bridge.execute(retry(marker, after).deps)).status).toBe('restart_failed');
    }
  });

  it('holds a user-reopened desktop to the daemon the marker also names', async () => {
    const { deps } = retry({ daemon: true }, { desktops: 1 }, { desktops: 1 });
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
  });

  it('counts a desktop marker from before these fields as owing some desktop', async () => {
    const { deps } = retry({}, { daemon: true });
    expect((await bridge.execute(desktopRetry(deps))).messageCode).toBe('restart_not_running');
    const back = retry({}, { desktops: 1 }, { desktops: 1 });
    expect((await bridge.execute(desktopRetry(back.deps))).messageCode).toBe('restart_not_running');
  });

  it('reads a marker that names a desktop but no count as owing one', async () => {
    for (const marker of [
      { desktop: true, desktops: ['/private/path', 7] },
      { desktop: true, desktops: -1 },
    ]) {
      const { deps } = retry(marker);
      expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
      const back = retry(marker, { desktops: 1 }, { desktops: 1 });
      expect((await bridge.execute(back.deps)).messageCode).toBe('restart_not_running');
    }
  });

  it('does not count a CLI marker that names nothing as a retry', async () => {
    const { value, deps } = retry({});
    const result = await bridge.execute(deps);
    expect(result.status).toBe('updated');
    expect(value.pending()).toBe(false);
  });

  it('does not demand what the marker says nothing closed', async () => {
    const { deps } = retry({ desktop: false, daemon: false, desktops: 0 });
    expect((await bridge.execute(desktopRetry(deps))).status).toBe('updated');
  });

  it('keeps a real upgrade with a stale marker that names nothing clean', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '0.9.0', desktop: false, daemon: false, desktops: 0 }),
      operation: 'cli',
      runtime: () => runtime(),
    });
    expect(result.status).toBe('updated');
  });

  it('owes what a marker names whatever version is installed now', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '0.9.0', daemon: true }),
      operation: 'cli',
      runtime: () => runtime(),
    });
    expect(result.messageCode).toBe('restart_not_running');
  });

  it('does not call a retry current when the update changed nothing', async () => {
    const { deps } = retry({ version: '0.9.0', daemon: true });
    deps.pending = () => false;
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
    const back = retry({ version: '0.9.0', daemon: true }, { daemon: true });
    back.deps.pending = () => false;
    expect((await bridge.execute(back.deps)).status).toBe('updated');
  });

  it('reports a failed retry as a failed restart at any version', async () => {
    const { deps } = retry({ version: '0.9.0', daemon: true });
    deps.pending = () => false;
    deps.runtime = () => ({
      ...runtime(),
      start: async () => {
        throw new Error('private');
      },
    });
    expect((await bridge.execute(deps)).status).toBe('restart_failed');
  });

  it('writes what was closed only after stop(), from the runtime plan', async () => {
    const first = fixture();
    let atStop: unknown[] = [];
    await bridge.execute({
      ...first.deps,
      operation: 'cli',
      runtime: () => ({
        ...runtime({ desktops: 2 }, { desktops: 2 }),
        stop: async () => {
          atStop = [...first.marks];
        },
      }),
    });
    expect(atStop).toEqual([
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: false, daemon: true, desktops: 0 },
    ]);
    expect(first.marks).toEqual([
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: true, daemon: false, desktops: 2 },
    ]);
  });

  it('keeps what an earlier run closed across a retry', async () => {
    const { value, deps } = retry(
      { desktop: true, desktops: 1 },
      {},
      { daemon: true, desktops: 1 }
    );
    await bridge.execute(deps);
    // The intent before the updater, naming the daemon, the same on to stop(), then what stop() closed.
    expect(value.marks[0]).toEqual({ desktop: true, daemon: true, desktops: 1 });
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: true,
      desktops: 2,
    });
  });

  it('leaves a busy run owing the daemon it could not check', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      idleSeconds: 1,
      operation: 'cli',
      runtime: () => ({
        ...runtime({ daemon: true, desktops: 1 }),
        stop: async () => {
          throw Object.assign(new Error('private'), { code: 'busy' });
        },
      }),
    });
    expect(result.messageCode).toBe('codex_busy');
    // stop() never ran its plan, so the install intent stands: the next click checks the daemon.
    expect(value.marks).toEqual([
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: false, daemon: true, desktops: 0 },
    ]);
  });

  it('writes the marker as soon as stop() returns, before a desktop install', async () => {
    const value = fixture();
    let atUpdate: unknown[] = [];
    await bridge.execute({
      ...value.deps,
      update: async () => {
        atUpdate = [...value.marks];
        await value.deps.update();
      },
      operation: 'desktop',
      runtime: () => runtime({ daemon: true, desktops: 1 }, { daemon: true, desktops: 1 }),
    });
    expect(atUpdate).toEqual([{ desktop: true, daemon: true, desktops: 1 }]);
  });

  const failedInstall = (value: ReturnType<typeof fixture>, planned: Plan, after: Live) => ({
    ...value.deps,
    update: async () => {
      throw new Error('private');
    },
    operation: 'desktop',
    runtime: () => runtime(planned, after),
  });

  it('keeps what a failed install left closed in the marker', async () => {
    const value = fixture();
    const result = await bridge.execute(failedInstall(value, { desktops: 1 }, {}));
    expect(result.status).toBe('restart_failed');
    expect(value.marks).toEqual([{ desktop: true, daemon: false, desktops: 1 }]);
    expect(value.pending()).toBe(true);
  });

  it('owes nothing after a failed install once everything is back', async () => {
    const value = fixture();
    const result = await bridge.execute(failedInstall(value, { desktops: 1 }, { desktops: 1 }));
    expect(result.status).toBe('failed');
    expect(result.messageCode).toBe('update_failed');
    expect(value.pending()).toBe(false);
  });

  it('keeps what an earlier run owed after a failed install', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...failedInstall(value, { desktops: 1 }, { desktops: 1 }),
      marked: () => ({ version: '1.0.0', desktop: true, desktops: 1 }),
    });
    expect(result.status).not.toBe('updated');
    expect(value.pending()).toBe(true);
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: false,
      desktops: 2,
    });
  });

  /** An installer that puts 2.0.0 in place and then exits unsuccessfully. */
  function partialInstall(_value: ReturnType<typeof fixture>) {
    let installed = '1.0.0';
    return {
      readVersion: async () => installed,
      update: async () => {
        installed = '2.0.0';
        throw new Error('private');
      },
      expectedCandidate: '2.0.0',
      operation: 'desktop',
    };
  }

  it('records the install it is about to attempt before attempting it', async () => {
    const value = fixture();
    let atUpdate: unknown[] = [];
    await bridge.execute({
      ...value.deps,
      update: async () => {
        atUpdate = [...value.marks];
        throw new Error('private');
      },
      expectedCandidate: '2.0.0',
      operation: 'desktop',
      runtime: () => runtime({ daemon: true }),
    });
    expect(atUpdate).toEqual([{ desktop: false, daemon: true, desktops: 0, target: '2.0.0' }]);
  });

  it('moves the record to what an installer that failed really left installed', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      ...partialInstall(value),
      runtime: () => runtime({ daemon: true }),
    });
    expect(result.status).toBe('restart_failed');
    expect(result.version).toBe('2.0.0');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
    expect(value.pending()).toBe(true);
  });

  it('reports a failed install, not a failed restart, when it replaced the app and all came back', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      ...partialInstall(value),
      runtime: () => runtime({ daemon: true }, { daemon: true }),
    });
    expect(result.status).toBe('failed');
    expect(result.messageCode).toBe('update_failed');
    expect(value.pending()).toBe(false);
  });

  it('carries a CLI retry to the version an updater that failed left installed', async () => {
    const value = fixture();
    let installed = '1.0.0';
    const result = await bridge.execute({
      ...value.deps,
      readVersion: async () => installed,
      update: async () => {
        installed = '2.0.0';
        throw new Error('private');
      },
      pending: () => true,
      marked: () => ({ version: '1.0.0', daemon: true }),
      operation: 'cli',
    });
    expect(result.status).toBe('restart_failed');
    expect(result.version).toBe('2.0.0');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
  });

  it('leaves a first CLI update that failed and changed nothing as a failed update', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      update: async () => {
        throw new Error('private');
      },
      operation: 'cli',
    });
    expect(result.status).toBe('failed');
    // The intent, then the marker from before it put back exactly: there was none.
    expect(value.marks).toEqual([{ desktop: false, daemon: true, desktops: 0 }]);
    expect(value.deps.marked()).toBeNull();
  });

  /** A first CLI update that puts 2.0.0 in place and then exits unsuccessfully, with no marker. */
  function partialCli(value: ReturnType<typeof fixture>, daemon: boolean) {
    let installed = '1.0.0';
    return {
      ...value.deps,
      readVersion: async () => installed,
      update: async () => {
        installed = '2.0.0';
        throw new Error('private');
      },
      operation: 'cli',
      runtime: () => runtime({}, { daemon }),
    };
  }

  it('owes a restart to a daemon a partial first CLI update left on the old binary', async () => {
    const value = fixture();
    const result = await bridge.execute(partialCli(value, true));
    expect(result.status).toBe('restart_failed');
    expect(result.version).toBe('2.0.0');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
  });

  it('owes nothing after a partial first CLI update with no daemon running', async () => {
    const value = fixture();
    const result = await bridge.execute(partialCli(value, false));
    expect(result.status).toBe('failed');
    expect(result.version).toBe('2.0.0');
    expect(value.marks).toEqual([{ desktop: false, daemon: true, desktops: 0 }]);
    expect(value.deps.marked()).toBeNull();
  });

  it('takes a failed scan to mean a daemon after a partial first CLI update', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...partialCli(value, false),
      runtime: () => ({
        ...runtime(),
        restartState: async () => {
          throw new Error('Codex inventory failed.');
        },
      }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
  });

  it('takes a runtime that cannot be created to mean a daemon after a partial first CLI update', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...partialCli(value, false),
      runtime: () => {
        throw new Error('private');
      },
    });
    expect(result.status).toBe('restart_failed');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
  });

  it('takes an older runtime to have a daemon after a partial first CLI update', async () => {
    const value = fixture();
    await bridge.execute({
      ...partialCli(value, false),
      runtime: () => ({ stop: async () => {}, start: async () => {} }),
    });
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
  });

  it('never writes an empty version over the record', async () => {
    const value = fixture();
    let reads = 0;
    const result = await bridge.execute({
      ...value.deps,
      readVersion: async () => (reads++ === 0 ? '1.0.0' : ''),
      update: async () => {},
      expectedCandidate: '2.0.0',
      operation: 'desktop',
      runtime: () => runtime({ daemon: true }),
    });
    expect(result.status).toBe('restart_failed');
    // Only the record from when stop() returned: the version read came back empty.
    expect(value.marks).toEqual([{ desktop: false, daemon: true, desktops: 0, target: '2.0.0' }]);
  });

  it('holds an owed daemon to the readiness check, not its arguments', async () => {
    const { value, deps } = retry({ daemon: true }, { daemon: true });
    deps.runtime = () => ({
      ...runtime({}, { daemon: true }),
      verifyRunningDaemon: async () => null,
    });
    const result = await bridge.execute(deps);
    expect(result.messageCode).toBe('restart_not_running');
    expect(value.pending()).toBe(false);
  });

  it('judges the daemon on a fresh scan taken after the readiness check', async () => {
    for (const [afterCheck, status] of [
      [{ daemon: false }, 'restart_failed'],
      [{ daemon: true }, 'updated'],
    ] as Array<[Live, string]>) {
      const { deps } = retry({ daemon: true }, { daemon: true });
      let checked = false;
      deps.runtime = () => ({
        ...runtime({}, {}),
        restartState: async () => state(checked ? afterCheck : { daemon: true }),
        verifyRunningDaemon: async () => {
          checked = true;
          return DAEMON;
        },
      });
      expect((await bridge.execute(deps)).status).toBe(status);
    }
  });

  it('checks a first update on a fresh scan after the proxy check, and keeps the marker', async () => {
    // A desktop start() saw come up is back; the daemon must still be ready after the proxy check.
    for (const [after, status] of [
      [{ daemon: false, desktops: 1 }, 'restart_failed'],
      [{ daemon: true, desktops: 1 }, 'updated'],
    ] as Array<[Live, string]>) {
      const value = fixture();
      let proxiesChecked = false;
      const result = await bridge.execute({
        ...value.deps,
        operation: 'cli',
        hasOldProxies: async () => {
          proxiesChecked = true;
          return false;
        },
        runtime: () => ({
          ...runtime({ daemon: true, desktops: 1 }, { desktops: 1 }),
          restartState: async () => state(proxiesChecked ? after : { daemon: true, desktops: 1 }),
        }),
      });
      expect(result.status).toBe(status);
      expect(value.pending()).toBe(status !== 'updated');
    }
  });

  it('owes a first update the desktops start() did not see come up, and only those', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => runtime({ daemon: true, desktops: 2 }, { daemon: true, desktops: 1 }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.deps.marked()).toEqual({
      version: '1.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('holds a first update to the readiness check of the daemon it restarted', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...runtime({ daemon: true }, { daemon: true }),
        verifyRunningDaemon: async () => null,
      }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
  });

  it('leaves a first update on an older runtime that cannot be scanned as updated', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({ stop: async () => {}, start: async () => {} }),
    });
    expect(result.status).toBe('updated');
  });

  it('checks a daemon that replaced the verified one before trusting it', async () => {
    for (const [replacementReady, status] of [
      [false, 'restart_failed'],
      [true, 'updated'],
    ] as Array<[boolean, string]>) {
      const { deps } = retry({ daemon: true }, { daemon: true });
      const checked: unknown[] = [];
      let running = { pid: 1, startTime: '1' };
      deps.runtime = () => ({
        ...runtime({}, {}),
        restartState: async () => state({ daemon: true, daemonIdentity: running }),
        verifyRunningDaemon: async () => {
          const target = running;
          checked.push(target);
          if (target.pid === 2) return replacementReady ? target : null;
          running = { pid: 2, startTime: '2' };
          return target;
        },
      });
      expect((await bridge.execute(deps)).status).toBe(status);
      expect(checked).toEqual([
        { pid: 1, startTime: '1' },
        { pid: 2, startTime: '2' },
      ]);
    }
  });

  it('gives up on a daemon that keeps being replaced', async () => {
    const { deps } = retry({ daemon: true }, { daemon: true });
    let pid = 1;
    deps.runtime = () => ({
      ...runtime({}, {}),
      restartState: async () =>
        state({ daemon: true, daemonIdentity: { pid, startTime: String(pid) } }),
      verifyRunningDaemon: async () => {
        const target = { pid, startTime: String(pid) };
        pid++;
        return target;
      },
    });
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
    expect(pid).toBe(3);
  });

  it('writes the restart intent before the CLI updater runs', async () => {
    const value = fixture();
    let atUpdate: unknown;
    await bridge.execute({
      ...value.deps,
      update: async () => {
        atUpdate = value.deps.marked();
      },
      operation: 'cli',
      runtime: () => runtime({}, { daemon: true }),
    });
    expect(atUpdate).toEqual({ version: '1.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  it('puts the earlier marker back exactly when the updater turns out a no-op', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      update: async () => {},
      operation: 'cli',
      runtime: () => runtime({}, { daemon: true }),
    });
    expect(result.status).toBe('current');
    expect(value.deps.marked()).toBeNull();
    const earlier = JSON.stringify({ version: '0.9.0', target: '1.0.0' });
    const other = fixture();
    const kept = await bridge.execute({
      ...other.deps,
      markerSnapshot: () => earlier,
      update: async () => {},
      operation: 'cli',
      runtime: () => runtime({}, { daemon: true }),
    });
    expect(kept.status).toBe('current');
    expect(other.restored).toEqual([earlier]);
  });

  /** A CLI click with a daemon running, whose updater fails having installed nothing. */
  const failedUpdater = (value: ReturnType<typeof fixture>, extra: object = {}) => ({
    ...value.deps,
    update: async () => {
      throw new Error('network error');
    },
    operation: 'cli',
    runtime: () => runtime({}, { daemon: true }),
    ...extra,
  });

  it('reports a failed updater that installed nothing as a failed update, owing nothing', async () => {
    const value = fixture();
    const result = await bridge.execute(failedUpdater(value));
    expect(result.status).toBe('failed');
    expect(result.messageCode).toBe('update_failed');
    expect(value.deps.marked()).toBeNull();
  });

  it('puts the earlier marker back exactly after a failed updater that installed nothing', async () => {
    const value = fixture();
    const earlier = JSON.stringify({ version: '0.9.0', target: '1.0.0' });
    const result = await bridge.execute(failedUpdater(value, { markerSnapshot: () => earlier }));
    expect(result.status).toBe('failed');
    expect(value.restored).toEqual([earlier]);
  });

  it('owes nothing when the expected version was not installed and nothing changed', async () => {
    const value = fixture();
    const result = await bridge.execute(
      failedUpdater(value, { update: async () => {}, expectedCandidate: '2.0.0' })
    );
    expect(result.status).toBe('failed');
    expect(result.messageCode).toBe('update_failed');
    expect(value.deps.marked()).toBeNull();
  });

  it('keeps the daemon owed when the updater installed a version other than expected', async () => {
    const value = fixture();
    let installed = '1.0.0';
    const result = await bridge.execute(
      failedUpdater(value, {
        readVersion: async () => installed,
        update: async () => {
          installed = '1.5.0';
        },
        expectedCandidate: '2.0.0',
      })
    );
    expect(result.status).toBe('restart_failed');
    expect(value.deps.marked()?.daemon).toBe(true);
  });

  it('keeps the intent when the installed version cannot be read after a failed updater', async () => {
    const value = fixture();
    let reads = 0;
    await bridge.execute(
      failedUpdater(value, {
        readVersion: async () => {
          if (reads++ === 0) return '1.0.0';
          throw new Error('private');
        },
      })
    );
    expect(value.deps.marked()?.daemon).toBe(true);
  });

  it('keeps what a retry owes when its updater fails', async () => {
    const { value, deps } = retry({ daemon: true }, { daemon: true });
    const result = await bridge.execute({
      ...deps,
      // An owed restart from another version: only the marker's own obligation makes this a retry.
      marked: () => ({ version: '0.9.0', daemon: true }),
      pending: () => false,
      update: async () => {
        throw new Error('network error');
      },
    });
    expect(result.status).toBe('restart_failed');
    expect(value.restored).toEqual([]);
    expect(value.pending()).toBe(true);
  });

  it('names the daemon before the updater even when none runs yet', async () => {
    // A proxy can start one on the old binary while the updater runs, and stop() alone can say.
    const value = fixture();
    let atUpdate: unknown = 'unset';
    const result = await bridge.execute({
      ...value.deps,
      update: async () => {
        atUpdate = value.deps.marked();
      },
      operation: 'cli',
      runtime: () => runtime({}, {}),
    });
    expect(atUpdate).toMatchObject({ daemon: true });
    // A verified no-op puts the earlier marker back exactly: here, none.
    expect(result.status).toBe('current');
    expect(value.deps.marked()).toBeNull();
  });

  it('settles the intent on the stop plan once an update replaced the app', async () => {
    const value = fixture();
    const marks: unknown[] = [];
    const result = await bridge.execute({
      ...value.deps,
      mark: (fields?: Record<string, boolean>) => {
        marks.push(fields);
        value.deps.mark(fields);
      },
      operation: 'cli',
      runtime: () => runtime({}, {}),
    });
    // Named before the updater, then what stop() found under its lock: no daemon, nothing owed.
    expect(marks[0]).toMatchObject({ daemon: true });
    expect(marks[marks.length - 1]).toEqual({ desktop: false, daemon: false, desktops: 0 });
    expect(result.status).toBe('updated');
    expect(value.deps.marked()).toBeNull();
  });

  it('takes a failed scan before the updater to mean a daemon', async () => {
    const value = fixture();
    let atUpdate: unknown;
    await bridge.execute({
      ...value.deps,
      update: async () => {
        atUpdate = value.deps.marked();
      },
      operation: 'cli',
      runtime: () => ({
        ...runtime(),
        restartState: async () => {
          throw new Error('Codex inventory failed.');
        },
      }),
    });
    expect(atUpdate).toEqual({ version: '1.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  it('keeps the intent on the marker until stop() hands over its plan', async () => {
    const value = fixture();
    let atStop: unknown;
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...runtime({ daemon: true }, { daemon: true }),
        stop: async () => {
          atStop = value.deps.marked();
        },
      }),
    });
    expect(atStop).toEqual({ version: '1.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  it('does not run the readiness check when no daemon is owed', async () => {
    const value = fixture();
    let checks = 0;
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...runtime({ desktops: 1 }, { desktops: 1 }),
        verifyRunningDaemon: async () => {
          checks++;
          return null;
        },
      }),
    });
    expect(result.status).toBe('updated');
    expect(checks).toBe(0);
  });

  it('does not clear a failed install over a daemon that fails the readiness check', async () => {
    const value = fixture();
    await bridge.execute({
      ...failedInstall(value, { daemon: true }, { daemon: true }),
      runtime: () => ({
        ...runtime({ daemon: true }, { daemon: true }),
        verifyRunningDaemon: async () => null,
      }),
    });
    expect(value.pending()).toBe(true);
  });

  it('fails closed when a runtime has no readiness check', async () => {
    const { deps } = retry({ daemon: true }, { daemon: true });
    deps.runtime = () => {
      const { verifyRunningDaemon: _dropped, ...older } = runtime({}, { daemon: true });
      return older as ReturnType<typeof runtime>;
    };
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
  });

  const olderRuntime = () => ({
    stop: async () => {},
    start: async () => {
      throw new Error('private');
    },
    dispose: async () => {},
  });

  it('fails closed on an older runtime that reports no plan', async () => {
    const value = fixture();
    const result = await bridge.execute({ ...value.deps, operation: 'cli', runtime: olderRuntime });
    expect(result.status).toBe('restart_failed');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('reads the next click on an older runtime as restart_not_running', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      update: async () => {},
      pending: () => true,
      marked: () => ({ version: '1.0.0', desktop: true, daemon: true, desktops: 0 }),
      operation: 'cli',
      runtime: () => ({ ...olderRuntime(), start: async () => {} }),
    });
    expect(result.messageCode).toBe('restart_not_running');
  });

  it('fails closed when a stop() on an older runtime fails', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...olderRuntime(),
        stop: async () => {
          throw new Error('private');
        },
      }),
    });
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('gives a live proxy a bounded while to bring the daemon back', async () => {
    const { value, deps } = retry({ daemon: true });
    let checks = 0;
    deps.runtime = () => ({
      ...runtime(),
      restartState: async () => {
        checks++;
        return state({ daemon: checks > 3, proxy: true });
      },
    });
    expect((await bridge.execute(deps)).status).toBe('updated');
    expect(value.events).toContain('wait');
  });

  it('stops waiting for a proxy at the bound and reports the daemon closed', async () => {
    const { value, deps } = retry({ daemon: true }, { proxy: true });
    const result = await bridge.execute({ ...deps, settleMs: 1000 });
    expect(result.messageCode).toBe('restart_not_running');
    expect(value.events.filter((event) => event === 'wait').length).toBe(4);
  });

  it('does not wait with no proxy to bring the daemon back', async () => {
    const { value, deps } = retry({ daemon: true });
    await bridge.execute(deps);
    expect(value.events).not.toContain('wait');
  });

  /** A stop() that fails: `retired` is what it closed, and `after` what its recovery brought back. */
  const failingStop = (planned: Plan, after: Live, retired: Plan = planned) => ({
    ...runtime(planned, after),
    retiredPlan: async () => ({
      daemon: retired.daemon ?? false,
      desktops: Math.max(0, (retired.desktops ?? 0) - (after.desktops ?? 0)),
    }),
    stop: async () => {
      throw new Error('private');
    },
  });

  it('records what a failed stop() retired and did not bring back to ready', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ daemon: true, desktops: 2 }, { desktops: 1 }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('does not owe a planned desktop a failed stop() never reached', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ desktops: 1 }, {}, {}),
    });
    // Not the desktop; the install intent stands, since stop() failed before it made its plan.
    expect(value.marks).toEqual([
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: false, daemon: true, desktops: 0 },
    ]);
  });

  it('owes a daemon a failed stop() retired when its replacement fails the readiness check', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...failingStop({ daemon: true }, { daemon: true }),
        verifyRunningDaemon: async () => null,
      }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: false,
      daemon: true,
      desktops: 0,
    });
  });

  it('names the retired daemon before the readiness check, in case this helper is killed', async () => {
    const value = fixture();
    let atCheck: unknown;
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...failingStop({ daemon: true }, { daemon: true }),
        verifyRunningDaemon: async () => {
          atCheck = value.marks[value.marks.length - 1];
          return DAEMON;
        },
      }),
    });
    expect(atCheck).toEqual({ desktop: false, daemon: true, desktops: 0 });
  });

  it('puts the earlier marker back when the retired daemon passes the readiness check', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ daemon: true }, { daemon: true }),
    });
    // The earlier marker is the intent written before the updater: the daemon still runs the old
    // binary, so it stays owed.
    expect(value.restored).toEqual([
      JSON.stringify({ version: '1.0.0', desktop: false, daemon: true, desktops: 0 }),
    ]);
    expect(value.deps.marked()?.daemon).toBe(true);
  });

  it('removes the marker it added when there was none and the daemon passes', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      expectedCandidate: '2.0.0',
      operation: 'desktop',
      runtime: () => failingStop({ daemon: true }, { daemon: true }),
    });
    expect(value.deps.marked()).toBeNull();
    expect(result.status).toBe('failed');
  });

  it('puts an earlier marker back exactly as it was stored', async () => {
    const value = fixture();
    const earlier = JSON.stringify({ version: '0.5.0', target: '0.9.0' }, null, 2);
    const result = await bridge.execute({
      ...value.deps,
      markerSnapshot: () => earlier,
      // As the real one reads: the restored marker is for another version.
      pending: (expected: string) => value.deps.marked()?.version === expected,
      expectedCandidate: '2.0.0',
      operation: 'desktop',
      runtime: () => failingStop({ daemon: true }, { daemon: true }),
    });
    expect(value.restored).toEqual([earlier]);
    expect(result.status).toBe('failed');
  });

  it('puts back what an earlier run owed when the retired daemon passes', async () => {
    const { value, deps } = retry({ desktop: true, desktops: 1 }, { daemon: true, desktops: 1 });
    deps.runtime = () => failingStop({ daemon: true }, { daemon: true, desktops: 1 });
    await bridge.execute(deps);
    expect(value.restored.map((text) => JSON.parse(text))).toEqual([
      // With the intent written before the updater, the running daemon is owed as well.
      { version: '1.0.0', desktop: true, daemon: true, desktops: 1 },
    ]);
  });

  const scanFails = async () => {
    throw new Error('Codex inventory failed.');
  };

  /** A runtime that hands over its plan the way the real one does, then fails during recovery. */
  const handsOver =
    (value: ReturnType<typeof fixture>, planned: Plan, after: Live, seen: unknown[]) =>
    (hooks: { beforeStop?: (plan: unknown) => unknown } = {}) => ({
      ...failingStop(planned, after),
      stop: async () => {
        await hooks.beforeStop?.(plan(planned));
        // What a helper killed from here on would leave on disk.
        seen.push(value.deps.marked());
        throw new Error('private');
      },
    });

  it('records the stop plan before stop() closes anything', async () => {
    const value = fixture();
    const seen: unknown[] = [];
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: handsOver(value, { daemon: true, desktops: 1 }, {}, seen),
    });
    expect(seen).toEqual([{ version: '1.0.0', desktop: true, daemon: true, desktops: 1 }]);
  });

  it('records the desktop install target with the stop plan', async () => {
    const value = fixture();
    const seen: unknown[] = [];
    await bridge.execute({
      ...value.deps,
      expectedCandidate: '2.0.0',
      operation: 'desktop',
      runtime: handsOver(value, { desktops: 1 }, {}, seen),
    });
    expect(seen).toEqual([
      { version: '1.0.0', desktop: true, daemon: false, desktops: 1, target: '2.0.0' },
    ]);
  });

  it('puts the earlier marker back once recovery from a failed stop() is verified', async () => {
    const value = fixture();
    const seen: unknown[] = [];
    const earlier = JSON.stringify({ version: '0.5.0', target: '0.9.0' });
    await bridge.execute({
      ...value.deps,
      // As the real one reads: what is on disk now, which the stop plan has overwritten by the end.
      markerSnapshot: () => (value.marks.length ? JSON.stringify(value.deps.marked()) : earlier),
      pending: (expected: string) => value.deps.marked()?.version === expected,
      expectedCandidate: '2.0.0',
      operation: 'desktop',
      runtime: handsOver(value, { desktops: 1 }, { desktops: 1 }, seen),
    });
    expect(value.restored).toEqual([earlier]);
  });

  it('settles the install intent on a stop plan that names nothing', async () => {
    const value = fixture();
    const seen: unknown[] = [];
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: handsOver(value, {}, {}, seen),
    });
    // The plan under the lock found no daemon, so the marker from before the intent is back: none.
    expect(seen).toEqual([null]);
  });

  it('falls back to the plan when a scan fails after a failed stop()', async () => {
    for (const broken of ['retiredPlan', 'restartState', 'verifyRunningDaemon']) {
      const value = fixture();
      const result = await bridge.execute({
        ...value.deps,
        operation: 'cli',
        runtime: () => ({
          ...failingStop({ daemon: true, desktops: 1 }, { daemon: true, desktops: 1 }),
          [broken]: scanFails,
        }),
      });
      expect(result.status).toBe('restart_failed');
      expect(value.marks[value.marks.length - 1]).toEqual({
        desktop: true,
        daemon: true,
        desktops: 1,
      });
    }
  });

  it('adds nothing when a scan fails and stop() had made no plan', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({ ...failingStop({}, {}), retiredPlan: scanFails }),
    });
    expect(value.marks).toEqual([
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: false, daemon: true, desktops: 0 },
    ]);
  });

  it('does not run the readiness check when a failed stop() retired no daemon', async () => {
    const value = fixture();
    let checks = 0;
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...failingStop({ daemon: true }, { daemon: true }, {}),
        verifyRunningDaemon: async () => {
          checks++;
          return null;
        },
      }),
    });
    expect(checks).toBe(0);
    // Only the intent from before the updater: nothing retired, nothing more recorded.
    expect(
      value.marks.every(
        (mark) =>
          JSON.stringify(mark) === JSON.stringify({ desktop: false, daemon: true, desktops: 0 })
      )
    ).toBe(true);
  });

  it('adds nothing when a failed stop() recovered what it closed', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ daemon: true, desktops: 1 }, { daemon: true, desktops: 1 }),
    });
    // The earlier marker is the intent written before the updater: the daemon still runs the old
    // binary, so it stays owed.
    expect(value.restored).toEqual([
      JSON.stringify({ version: '1.0.0', desktop: false, daemon: true, desktops: 0 }),
    ]);
    expect(value.deps.marked()?.daemon).toBe(true);
  });

  it('adds nothing when a failed stop() never made a plan', async () => {
    const value = fixture();
    await bridge.execute({ ...value.deps, operation: 'cli', runtime: () => failingStop({}, {}) });
    // stop() failed before it made its plan, so the install intent stands.
    expect(value.marks).toEqual([
      { desktop: false, daemon: true, desktops: 0 },
      { desktop: false, daemon: true, desktops: 0 },
    ]);
  });

  it('keeps what an earlier run closed when a failed stop() loses something more', async () => {
    const { value, deps } = retry({ desktop: true, desktops: 1 });
    deps.runtime = () => failingStop({ daemon: true }, {});
    await bridge.execute(deps);
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: true,
      desktops: 1,
    });
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
exports.createCodexActivationRuntime = (home, overrides) => {
  const read = async () => {
    const seen = await overrides.scan();
    const desktop = seen.some((row) => row.exe === '/usr/lib/chatgpt/ChatGPT');
    const daemon = seen.find((row) => row.args.includes('--listen'));
    return {
      daemon: Boolean(daemon),
      daemonIdentity: daemon ? { pid: daemon.pid, startTime: daemon.startTime } : null,
      desktops: desktop ? 1 : 0,
    };
  };
  let plan = { daemon: false, desktops: 0 };
  return {
    stopPlan: () => plan,
    retiredPlan: async () => {
      if (process.env.FIXTURE_STOP === 'fail-scan') await overrides.scan();
      // After a failed stop(): what it closed and its recovery did not bring back. A replaced daemon's
      // recovery brought the desktop back.
      if (procs.length && process.env.FIXTURE_STOP !== 'fail-replaced') return { daemon: false, desktops: 0 };
      return process.env.FIXTURE_STOP === 'fail-replaced' ? { ...plan, desktops: 0 } : plan;
    },
    verifyRunningDaemon: async () => {
      if (process.env.FIXTURE_AT_VERIFY) fs.copyFileSync(process.env.FIXTURE_MARKER, process.env.FIXTURE_AT_VERIFY);
      return process.env.FIXTURE_VERIFY === 'fail' ? null : (await read()).daemonIdentity;
    },
    restartState: async () => ({ ...(await read()), proxy: false }),
    stop: async () => {
      if (process.env.FIXTURE_AT_STOP) fs.copyFileSync(process.env.FIXTURE_MARKER, process.env.FIXTURE_AT_STOP);
      if (process.env.FIXTURE_KILL) {
        await overrides.beforeStop?.(await read());
        process.kill(process.pid, 'SIGKILL');
      }
      plan = await read();
      if (process.env.FIXTURE_STOP === 'fail-scan') fs.readdirSync = () => { throw Object.assign(new Error('scan'), { code: 'EIO' }); };
      if (!['fail-recovered', 'fail-replaced'].includes(process.env.FIXTURE_STOP) && !process.env.FIXTURE_KEEP) procs = [];
      if (process.env.FIXTURE_STOP) throw new Error('private');
    },
    start: async () => {
      if (process.env.FIXTURE_START === 'fail') throw new Error('private');
      if (process.env.FIXTURE_START !== 'launch') return;
      await overrides.launch({ exe: '/old/codex', args: ['codex', 'app-server'], cwd: '/', env: { KEEP: 'yes' } }, false,
        async (target) => { fs.writeFileSync(process.env.FIXTURE_RECORD, JSON.stringify(target)); });
    },
  };
};
`;
  function run(
    start: 'launch' | 'fail' | 'idle',
    options: {
      marker?: object | null;
      next?: string;
      installed?: string;
      stop?: string;
      keep?: boolean;
      verify?: string;
      kill?: boolean;
      procs?: (root: string) => object[];
    } = {}
  ) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-bridge-'));
    directories.push(root);
    const script = path.join(root, 'bridge', 'app_update_codex.cjs');
    fs.mkdirSync(path.dirname(script));
    fs.copyFileSync(BRIDGE_SOURCE, script);
    fs.writeFileSync(path.join(root, 'bridge', 'app_update_codex_runtime.cjs'), RUNTIME);
    const codex = path.join(root, '.local/bin/codex');
    fs.mkdirSync(path.dirname(codex), { recursive: true });
    fs.writeFileSync(path.join(root, 'version'), `${options.installed ?? '2.0.0'}\n`);
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
    const atVerify = path.join(root, 'marker-at-verify.json');
    const invoke = () =>
      execFileSync('node', [script, '--operation', 'cli'], {
        env: {
          PATH: process.env.PATH,
          HOME: root,
          FIXTURE_RECORD: record,
          FIXTURE_START: start,
          FIXTURE_STOP: options.stop ?? '',
          FIXTURE_KEEP: options.keep ? '1' : '',
          FIXTURE_KILL: options.kill ? '1' : '',
          FIXTURE_VERIFY: options.verify ?? '',
          FIXTURE_AT_VERIFY: atVerify,
          FIXTURE_MARKER: marker,
          FIXTURE_AT_STOP: atStop,
          FIXTURE_PROCS: JSON.stringify(options.procs?.(root) ?? []),
        },
        encoding: 'utf8',
      });
    let stdout = '{}';
    try {
      stdout = invoke();
    } catch (error) {
      if (!options.kill) throw error;
    }
    return { result: JSON.parse(stdout), record, marker, codex, atStop, atVerify };
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

  it('writes the running daemon before the update, and what stop() closed after it', () => {
    const { result, marker, atStop } = run('fail', {
      marker: null,
      next: '3.0.0',
      procs: running,
    });
    expect(result.status).toBe('restart_failed');
    // The intent written before the updater ran: the daemon on the old binary.
    expect(read(atStop)).toEqual({ version: '3.0.0', desktop: false, daemon: true, desktops: 0 });
    expect(read(marker)).toEqual({
      version: '3.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('records only what was running on the real marker', () => {
    const { marker } = run('fail', { marker: null, next: '3.0.0' });
    expect(read(marker)).toEqual({ version: '3.0.0', desktop: false, daemon: false, desktops: 0 });
  });

  it('keeps what an earlier run closed on the real marker across a failed retry', () => {
    const { marker } = run('fail', { marker: { version: '2.0.0', desktop: true } });
    expect(read(marker)).toEqual({ version: '2.0.0', desktop: true, daemon: false, desktops: 1 });
  });

  it('records on the real marker what a failed stop() could not bring back', () => {
    const { result, marker } = run('idle', {
      marker: null,
      next: '3.0.0',
      stop: 'fail',
      procs: running,
    });
    expect(result.status).toBe('restart_failed');
    expect(read(marker)).toEqual({
      version: '3.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('leaves the update intent on the real marker when a failed stop() recovered', () => {
    const { marker } = run('idle', {
      marker: null,
      next: '3.0.0',
      stop: 'fail-recovered',
      procs: running,
    });
    // Recovery brought the old daemon back, still on the old binary: it is still owed.
    expect(read(marker)).toEqual({ version: '3.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  it('reads a marker the Python path wrote for a prerelease as a retry', () => {
    const { result, marker } = run('idle', {
      installed: '2.0.0-alpha.1',
      marker: { version: '2.0.0-alpha.1', daemon: true },
    });
    expect(result.messageCode).toBe('restart_not_running');
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('reads a prerelease marker naming nothing as a restart owed, not as current', () => {
    const { result, marker } = run('idle', {
      installed: '2.0.0-alpha.1',
      marker: { version: '2.0.0-alpha.1' },
    });
    expect(result.status).toBe('updated');
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('finds a real marker by its target after an interrupted install', () => {
    const { result, marker } = run('idle', {
      marker: { version: '1.0.0', target: '2.0.0', daemon: true },
    });
    expect(result.messageCode).toBe('restart_not_running');
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('still owes what a marker names after a helper was killed mid-update', () => {
    const { result, marker } = run('idle', { marker: { version: '1.0.0', daemon: true } });
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('has the real marker naming the daemon during the readiness check, and restores it', () => {
    const { marker, atVerify, result } = run('idle', {
      marker: null,
      next: '3.0.0',
      stop: 'fail-replaced',
      procs: running,
    });
    expect(read(atVerify).daemon).toBe(true);
    // Put back as it was before stop(): the update intent naming the daemon.
    expect(read(marker)).toEqual({ version: '3.0.0', desktop: false, daemon: true, desktops: 0 });
    expect(result.status).toBe('restart_failed');
  });

  it('keeps the daemon on the real marker when the replacement fails the check', () => {
    const { marker } = run('idle', {
      marker: null,
      next: '3.0.0',
      stop: 'fail-replaced',
      verify: 'fail',
      procs: running,
    });
    expect(read(marker).daemon).toBe(true);
  });

  it('keeps the plan on the real marker when the scan fails after a failed stop()', () => {
    const { result, marker } = run('idle', {
      marker: null,
      next: '3.0.0',
      stop: 'fail-scan',
      procs: running,
    });
    expect(result.status).toBe('restart_failed');
    expect(read(marker)).toEqual({
      version: '3.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('has the stop plan on the real marker when the helper is killed inside stop()', () => {
    const { marker } = run('idle', {
      marker: null,
      next: '3.0.0',
      kill: true,
      procs: running,
    });
    expect(read(marker)).toEqual({
      version: '3.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });
  });

  it('does not turn a marker that names nothing into a red row', () => {
    const { result, marker } = run('idle', { marker: { version: '2.0.0' } });
    expect(result.status).toBe('updated');
    expect(fs.existsSync(marker)).toBe(false);
  });
});

describe('Putting a marker back', () => {
  it('writes the stored bytes back exactly, privately and in one step', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-marker-'));
    directories.push(root);
    const file = path.join(root, 'state', 'codex-desktop-pending-restart.json');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    // As the Python path writes one: its own spacing, a prerelease version and a target.
    const earlier = '{\n  "version": "0.50.0-alpha.1",\n  "target": "0.51.0"\n}\n';
    fs.writeFileSync(file, earlier, { mode: 0o600 });
    const snapshot = bridge.markerSnapshot(file);
    expect(snapshot).toBe(earlier);
    fs.writeFileSync(file, JSON.stringify({ version: '2.0.0', daemon: true }));
    bridge.restoreMarker(snapshot, file);
    expect(fs.readFileSync(file, 'utf8')).toBe(earlier);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(fs.readdirSync(path.dirname(file))).toEqual(['codex-desktop-pending-restart.json']);
  });

  it('reads no snapshot from a missing marker', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-marker-'));
    directories.push(root);
    expect(bridge.markerSnapshot(path.join(root, 'none.json'))).toBeNull();
  });
});
