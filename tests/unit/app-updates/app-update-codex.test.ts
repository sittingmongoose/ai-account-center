import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { execFileSync } from 'child_process';
import { createHmac } from 'crypto';
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
      runtime: () => ({
        stopPlan: () => ({ daemon: false, desktops: [] }),
        retiredPlan: async () => ({ daemon: false, desktops: [] }),
        restartState: async () => ({ daemon: false, desktops: [], proxy: false }),
        verifyRunningDaemon: async () => true,
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
    // Written when stop() returned, then cleared: the failed install owes nothing with all back.
    expect(value.events).toContain('clear');
    expect(value.pending()).toBe(false);
  });
  it('can recover a pending restart without requiring another version change', async () => {
    const value = fixture();
    value.deps.mark();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '1.0.0', desktop: true }),
      runtime: () => ({
        ...value.deps.runtime(),
        restartState: async () => ({ daemon: false, desktops: ['a'.repeat(32)], proxy: false }),
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
  const A = 'a'.repeat(32);
  const B = 'b'.repeat(32);
  type Plan = { daemon?: boolean; desktops?: string[] };
  type Live = Plan & { proxy?: boolean };
  const state = (live: Live = {}) => ({ daemon: false, desktops: [], proxy: false, ...live });
  const plan = (value: Plan = {}) => ({ daemon: false, desktops: [], ...value });
  /** The runtime a click meets: what its stop() plans to close, and what runs after start(). */
  const runtime = (planned: Plan = {}, after: Live = {}) => ({
    stop: async () => {},
    start: async () => {},
    dispose: async () => {},
    stopPlan: () => plan(planned),
    retiredPlan: async () => plan(planned),
    restartState: async () => state(after),
    verifyRunningDaemon: async () => true,
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

  it('requires every desktop the marker names, not just one', async () => {
    const marker = { desktop: true, desktops: [A, B] };
    for (const [desktops, status] of [
      [[A], 'restart_failed'],
      [[B], 'restart_failed'],
      [[], 'restart_failed'],
      [[A, B], 'updated'],
    ] as Array<[string[], string]>) {
      expect((await bridge.execute(desktopRetry(retry(marker, { desktops }).deps))).status).toBe(
        status
      );
    }
  });

  it('requires the daemon and the desktops when a run closed both', async () => {
    const marker = { desktop: true, daemon: true, desktops: [A] };
    for (const [after, status] of [
      [{ desktops: [A] }, 'restart_failed'],
      [{ daemon: true }, 'restart_failed'],
      [{ daemon: true, desktops: [A] }, 'updated'],
    ] as Array<[Live, string]>) {
      expect((await bridge.execute(retry(marker, after).deps)).status).toBe(status);
    }
  });

  it('holds a user-reopened desktop to the daemon the marker also names', async () => {
    const { deps } = retry({ daemon: true }, { desktops: [A] }, { desktops: [A] });
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
  });

  it('counts a desktop marker from before these fields as owing some desktop', async () => {
    const { deps } = retry({}, { daemon: true });
    expect((await bridge.execute(desktopRetry(deps))).messageCode).toBe('restart_not_running');
    const back = retry({}, { desktops: [A] });
    expect((await bridge.execute(desktopRetry(back.deps))).status).toBe('updated');
  });

  it('reads a marker that names a desktop but no valid instance as owing some desktop', async () => {
    const marker = { desktop: true, desktops: ['/private/path', 7] };
    const { deps } = retry(marker);
    expect((await bridge.execute(deps)).messageCode).toBe('restart_not_running');
    const back = retry(marker, { desktops: [A] });
    expect((await bridge.execute(back.deps)).status).toBe('updated');
  });

  it('does not count a CLI marker that names nothing as a retry', async () => {
    const { value, deps } = retry({});
    const result = await bridge.execute(deps);
    expect(result.status).toBe('updated');
    expect(value.pending()).toBe(false);
  });

  it('does not demand what the marker says nothing closed', async () => {
    const { deps } = retry({ desktop: false, daemon: false, desktops: [] });
    expect((await bridge.execute(desktopRetry(deps))).status).toBe('updated');
  });

  it('keeps a real upgrade with a stale marker that names nothing clean', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      marked: () => ({ version: '0.9.0', desktop: false, daemon: false, desktops: [] }),
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
        ...runtime({ daemon: true, desktops: [A, B] }),
        stop: async () => {
          atStop = [...first.marks];
        },
      }),
    });
    expect(atStop).toEqual([undefined]);
    expect(first.marks).toEqual([undefined, { desktop: true, daemon: true, desktops: [A, B] }]);
  });

  it('keeps what an earlier run closed across a retry', async () => {
    const { value, deps } = retry(
      { desktop: true, desktops: [A] },
      {},
      { daemon: true, desktops: [B] }
    );
    await bridge.execute(deps);
    expect(value.marks).toEqual([
      { desktop: true, daemon: false, desktops: [A] },
      { desktop: true, daemon: true, desktops: [A, B] },
    ]);
  });

  it('leaves a busy run with a marker that names nothing', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      idleSeconds: 1,
      operation: 'cli',
      runtime: () => ({
        ...runtime({ daemon: true, desktops: [A] }),
        stop: async () => {
          throw Object.assign(new Error('private'), { code: 'busy' });
        },
      }),
    });
    expect(result.messageCode).toBe('codex_busy');
    expect(value.marks).toEqual([undefined]);
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
      runtime: () => runtime({ daemon: true, desktops: [A] }, { daemon: true, desktops: [A] }),
    });
    expect(atUpdate).toEqual([{ desktop: true, daemon: true, desktops: [A] }]);
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
    const result = await bridge.execute(failedInstall(value, { desktops: [A] }, {}));
    expect(result.status).toBe('restart_failed');
    expect(value.marks).toEqual([{ desktop: true, daemon: false, desktops: [A] }]);
    expect(value.pending()).toBe(true);
  });

  it('owes nothing after a failed install once everything is back', async () => {
    const value = fixture();
    const result = await bridge.execute(failedInstall(value, { desktops: [A] }, { desktops: [A] }));
    expect(result.status).toBe('failed');
    expect(result.messageCode).toBe('update_failed');
    expect(value.pending()).toBe(false);
  });

  it('keeps what an earlier run owed after a failed install', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...failedInstall(value, { desktops: [A] }, { desktops: [A] }),
      marked: () => ({ version: '1.0.0', desktop: true, desktops: [B] }),
    });
    expect(result.status).not.toBe('updated');
    expect(value.pending()).toBe(true);
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: false,
      desktops: [B, A],
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
    expect(atUpdate).toEqual([{ desktop: false, daemon: true, desktops: [], target: '2.0.0' }]);
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
      desktops: [],
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
      desktops: [],
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
    expect(value.marks).toEqual([]);
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
    expect(value.marks).toEqual([{ desktop: false, daemon: true, desktops: [] }]);
  });

  it('owes nothing after a partial first CLI update with no daemon running', async () => {
    const value = fixture();
    const result = await bridge.execute(partialCli(value, false));
    expect(result.status).toBe('failed');
    expect(result.version).toBe('2.0.0');
    expect(value.marks).toEqual([]);
  });

  it('takes an older runtime to have a daemon after a partial first CLI update', async () => {
    const value = fixture();
    await bridge.execute({
      ...partialCli(value, false),
      runtime: () => ({ stop: async () => {}, start: async () => {} }),
    });
    expect(value.marks).toEqual([{ desktop: false, daemon: true, desktops: [] }]);
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
    expect(value.marks).toEqual([{ desktop: false, daemon: true, desktops: [], target: '2.0.0' }]);
  });

  it('holds an owed daemon to the readiness check, not its arguments', async () => {
    const { value, deps } = retry({ daemon: true }, { daemon: true });
    deps.runtime = () => ({
      ...runtime({}, { daemon: true }),
      verifyRunningDaemon: async () => false,
    });
    const result = await bridge.execute(deps);
    expect(result.messageCode).toBe('restart_not_running');
    expect(value.pending()).toBe(false);
  });

  it('does not run the readiness check when no daemon is owed', async () => {
    const { deps } = retry({ desktop: true, desktops: [A] }, { desktops: [A], daemon: true });
    let checks = 0;
    deps.runtime = () => ({
      ...runtime({}, { desktops: [A], daemon: true }),
      verifyRunningDaemon: async () => {
        checks++;
        return false;
      },
    });
    expect((await bridge.execute(deps)).status).toBe('updated');
    expect(checks).toBe(0);
  });

  it('does not clear a failed install over a daemon that fails the readiness check', async () => {
    const value = fixture();
    await bridge.execute({
      ...failedInstall(value, { daemon: true }, { daemon: true }),
      runtime: () => ({
        ...runtime({ daemon: true }, { daemon: true }),
        verifyRunningDaemon: async () => false,
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
      desktops: [],
    });
  });

  it('reads the next click on an older runtime as restart_not_running', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      update: async () => {},
      pending: () => true,
      marked: () => ({ version: '1.0.0', desktop: true, daemon: true, desktops: [] }),
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
      desktops: [],
    });
  });

  it('meets a salt problem before stopping anything', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      prepareSeal: () => {
        throw new Error('private');
      },
      operation: 'desktop',
    });
    expect(value.events).not.toContain('stop');
    expect(value.events).not.toContain('update');
    expect(result.status).toBe('failed');
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

  const failingStop = (planned: Plan, after: Live, retired: Plan = planned) => ({
    ...runtime(planned, after),
    retiredPlan: async () => plan(retired),
    stop: async () => {
      throw new Error('private');
    },
  });

  it('records what a failed stop() retired and did not bring back to ready', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ daemon: true, desktops: [A, B] }, { desktops: [B] }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.marks).toEqual([undefined, { desktop: true, daemon: true, desktops: [A] }]);
  });

  it('does not owe a planned desktop a failed stop() never reached', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ desktops: [A] }, {}, {}),
    });
    expect(value.marks).toEqual([undefined]);
  });

  it('owes a daemon a failed stop() retired when its replacement fails the readiness check', async () => {
    const value = fixture();
    const result = await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => ({
        ...failingStop({ daemon: true }, { daemon: true }),
        verifyRunningDaemon: async () => false,
      }),
    });
    expect(result.status).toBe('restart_failed');
    expect(value.marks).toEqual([undefined, { desktop: false, daemon: true, desktops: [] }]);
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
          return false;
        },
      }),
    });
    expect(checks).toBe(0);
    expect(value.marks).toEqual([undefined]);
  });

  it('adds nothing when a failed stop() recovered what it closed', async () => {
    const value = fixture();
    await bridge.execute({
      ...value.deps,
      operation: 'cli',
      runtime: () => failingStop({ daemon: true, desktops: [A] }, { daemon: true, desktops: [A] }),
    });
    expect(value.marks).toEqual([undefined]);
  });

  it('adds nothing when a failed stop() never made a plan', async () => {
    const value = fixture();
    await bridge.execute({ ...value.deps, operation: 'cli', runtime: () => failingStop({}, {}) });
    expect(value.marks).toEqual([undefined]);
  });

  it('keeps what an earlier run closed when a failed stop() loses something more', async () => {
    const { value, deps } = retry({ desktop: true, desktops: [A] });
    deps.runtime = () => failingStop({ daemon: true }, {});
    await bridge.execute(deps);
    expect(value.marks[value.marks.length - 1]).toEqual({
      desktop: true,
      daemon: true,
      desktops: [A],
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
  const INSTANCE = 'a'.repeat(32);
  const read = async () => {
    const seen = await overrides.scan();
    const desktop = seen.some((row) => row.exe === '/usr/lib/chatgpt/ChatGPT');
    return { daemon: seen.some((row) => row.args.includes('--listen')), desktops: desktop ? [INSTANCE] : [] };
  };
  let plan = { daemon: false, desktops: [] };
  return {
    stopPlan: () => plan,
    retiredPlan: async () => (procs.length ? { daemon: false, desktops: [] } : plan),
    verifyRunningDaemon: async () => true,
    restartState: async () => ({ ...(await read()), proxy: false }),
    stop: async () => {
      if (process.env.FIXTURE_AT_STOP) fs.copyFileSync(process.env.FIXTURE_MARKER, process.env.FIXTURE_AT_STOP);
      fs.writeFileSync(process.env.FIXTURE_RECORD + '.salt', String(fs.lstatSync(require('path').join(require('path').dirname(process.env.FIXTURE_MARKER), 'codex-desktop-instance.salt')).isFile()));
      plan = await read();
      if (process.env.FIXTURE_STOP !== 'fail-recovered' && !process.env.FIXTURE_KEEP) procs = [];
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
      salt?: string;
      plantSalt?: (file: string) => void;
      keep?: boolean;
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
    const salt = path.join(path.dirname(marker), 'codex-desktop-instance.salt');
    if (options.salt) fs.writeFileSync(salt, options.salt, { mode: 0o600 });
    options.plantSalt?.(salt);
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
        FIXTURE_STOP: options.stop ?? '',
        FIXTURE_KEEP: options.keep ? '1' : '',
        FIXTURE_MARKER: marker,
        FIXTURE_AT_STOP: atStop,
        FIXTURE_PROCS: JSON.stringify(options.procs?.(root) ?? []),
      },
      encoding: 'utf8',
    });
    return { result: JSON.parse(stdout), record, marker, codex, atStop, salt };
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
  const INSTANCE = 'a'.repeat(32);
  const keyed = (saltValue: string) =>
    createHmac('sha256', saltValue).update(INSTANCE).digest('hex').slice(0, 32);
  const keyedBy = (file: string) => keyed(fs.readFileSync(file, 'utf8').trim());

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
    const { result, marker, atStop, salt } = run('fail', {
      marker: null,
      next: '3.0.0',
      procs: running,
    });
    expect(result.status).toBe('restart_failed');
    expect(read(atStop)).toEqual({ version: '3.0.0' });
    expect(read(marker)).toEqual({
      version: '3.0.0',
      desktop: true,
      daemon: true,
      desktops: [keyedBy(salt)],
    });
  });

  it('records only what was running on the real marker', () => {
    const { marker } = run('fail', { marker: null, next: '3.0.0' });
    expect(read(marker)).toEqual({ version: '3.0.0', desktop: false, daemon: false, desktops: [] });
  });

  it('keeps what an earlier run closed on the real marker across a failed retry', () => {
    const { marker } = run('fail', { marker: { version: '2.0.0', desktop: true } });
    expect(read(marker)).toEqual({ version: '2.0.0', desktop: true, daemon: false, desktops: [] });
  });

  it('records on the real marker what a failed stop() could not bring back', () => {
    const { result, marker, salt } = run('idle', {
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
      desktops: [keyedBy(salt)],
    });
  });

  it('leaves the real marker naming nothing when a failed stop() recovered', () => {
    const { marker } = run('idle', {
      marker: null,
      next: '3.0.0',
      stop: 'fail-recovered',
      procs: running,
    });
    expect(read(marker)).toEqual({ version: '3.0.0' });
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

  it('keeps the marker hash keyed with a private salt beside it', () => {
    const { marker, salt } = run('fail', { marker: null, next: '3.0.0', procs: running });
    expect(fs.statSync(salt).mode & 0o777).toBe(0o600);
    expect(fs.readFileSync(salt, 'utf8')).toMatch(/^[0-9a-f]{64}$/);
    // The runtime's own unsalted name, which a guessed path could be tested against, is not stored.
    expect(read(marker).desktops).not.toContain(INSTANCE);
  });

  it('matches a retry through the salt, and not through a different one', () => {
    const first = 'c'.repeat(64);
    const owed = { version: '2.0.0', desktop: true, desktops: [keyed(first)] };
    const same = run('idle', { marker: owed, salt: first, keep: true, procs: running });
    expect(same.result.status).toBe('updated');
    const other = run('idle', { marker: owed, salt: 'd'.repeat(64), keep: true, procs: running });
    expect(other.result.messageCode).toBe('restart_not_running');
  });

  it('has a usable salt in place before stop() runs', () => {
    const { record } = run('idle', { marker: null, next: '3.0.0', procs: running });
    expect(fs.readFileSync(`${record}.salt`, 'utf8')).toBe('true');
  });

  it('replaces a directory at the salt path and carries on', () => {
    const { result, salt, marker } = run('fail', {
      marker: null,
      next: '3.0.0',
      procs: running,
      plantSalt: (file) => {
        fs.mkdirSync(path.join(file, 'nested'), { recursive: true });
      },
    });
    expect(result.status).toBe('restart_failed');
    expect(fs.statSync(salt).isFile()).toBe(true);
    expect(read(marker).desktops).toEqual([keyedBy(salt)]);
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

  it('does not turn a marker that names nothing into a red row', () => {
    const { result, marker } = run('idle', { marker: { version: '2.0.0' } });
    expect(result.status).toBe('updated');
    expect(fs.existsSync(marker)).toBe(false);
  });
});

describe('The per-home desktop salt', () => {
  const folder = () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-salt-'));
    directories.push(root);
    return root;
  };
  const file = (root: string) => path.join(root, 'state', 'codex-desktop-instance.salt');

  it('is created private, then stays the same', () => {
    const root = folder();
    const directory = path.dirname(file(root));
    const value = bridge.desktopSalt(directory);
    expect(value).toMatch(/^[0-9a-f]{64}$/);
    expect(fs.statSync(file(root)).mode & 0o777).toBe(0o600);
    expect(bridge.desktopSalt(directory)).toBe(value);
  });

  it('differs between homes, and so does the same desktop under it', () => {
    const one = bridge.desktopSalt(path.dirname(file(folder())));
    const two = bridge.desktopSalt(path.dirname(file(folder())));
    expect(one).not.toBe(two);
    expect(bridge.sealInstance('a'.repeat(32), one)).not.toBe(
      bridge.sealInstance('a'.repeat(32), two)
    );
    expect(bridge.sealInstance('a'.repeat(32), one)).toMatch(/^[0-9a-f]{32}$/);
  });

  it('replaces a salt that is readable by others, malformed, or a link', () => {
    for (const plant of [
      (target: string) => fs.writeFileSync(target, 'e'.repeat(64), { mode: 0o644 }),
      (target: string) => fs.writeFileSync(target, 'short', { mode: 0o600 }),
      (target: string, root: string) => {
        const elsewhere = path.join(root, 'elsewhere');
        fs.writeFileSync(elsewhere, 'e'.repeat(64), { mode: 0o600 });
        fs.symlinkSync(elsewhere, target);
      },
      (target: string) => {
        fs.mkdirSync(path.join(target, 'nested'), { recursive: true });
      },
      (target: string) => {
        fs.writeFileSync(target, 'e'.repeat(64), { mode: 0o600 });
        fs.chmodSync(target, 0o000);
      },
    ]) {
      const root = folder();
      fs.mkdirSync(path.dirname(file(root)));
      plant(file(root), root);
      const value = bridge.desktopSalt(path.dirname(file(root)));
      expect(value).toMatch(/^[0-9a-f]{64}$/);
      expect(value).not.toBe('e'.repeat(64));
      expect(fs.lstatSync(file(root)).isSymbolicLink()).toBe(false);
      expect(fs.statSync(file(root)).mode & 0o777).toBe(0o600);
    }
  });
});
