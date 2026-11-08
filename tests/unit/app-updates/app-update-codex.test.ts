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
    // A CLI click keeps no record: it updates, then stops and starts under the same protection.
    expect(value.events).toEqual([
      'update',
      'stop',
      'wait',
      'stop',
      'wait',
      'stop',
      'proxy-reconnect',
      'start',
      'dispose',
    ]);
    expect(value.pending()).toBe(false);
  });
  it('says the restart failed, keeping no record, when start() fails', async () => {
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
    expect(value.pending()).toBe(false);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
  });
  it('does not stop a current package', async () => {
    const value = fixture();
    value.deps.update = async () => {
      value.events.push('update');
    };
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('current');
    expect(value.events).toEqual(['update']);
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
    // Nothing was stopped, so nothing restarts, and no record is kept for a later click.
    expect(value.events).not.toContain('start');
    expect(value.pending()).toBe(false);
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
    expect(value.pending()).toBe(false);
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

describe('Restart obligations', () => {
  beforeEach(() => {
    bridge.output.restartedProcesses = 0;
  });
  type Plan = { daemon?: boolean; desktops?: number };
  /** What runs after start(): the daemon, a proxy, and how many of the desktops it closed came back. */
  type Live = { daemon?: boolean; proxy?: boolean; desktops?: number };
  const runtime = (planned: Plan = {}, after: Live = {}, events: string[] = []) => ({
    stop: async () => {
      events.push('stop');
    },
    start: async () => {
      events.push('start');
    },
    dispose: async () => {},
    stopPlan: () => ({ daemon: false, desktops: 0, ...planned }),
    retiredPlan: async () => ({
      daemon: planned.daemon ?? false,
      desktops: Math.max(0, (planned.desktops ?? 0) - (after.desktops ?? 0)),
    }),
    restartState: async () => ({
      daemon: after.daemon ?? false,
      proxy: after.proxy ?? false,
      daemonIdentity: after.daemon ? DAEMON : null,
    }),
    verifyRunningDaemon: async () => DAEMON,
  });

  describe('the Codex CLI: each click stands alone', () => {
    const cli = (value: ReturnType<typeof fixture>, extra: object = {}) => ({
      ...value.deps,
      operation: 'cli',
      ...extra,
    });

    it('restarts the daemon on the new version and reports updated', async () => {
      const value = fixture();
      const events: string[] = [];
      const result = await bridge.execute(
        cli(value, { runtime: () => runtime({ daemon: true }, { daemon: true }, events) })
      );
      expect([result.status, result.previousVersion, result.version]).toEqual([
        'updated',
        '1.0.0',
        '2.0.0',
      ]);
      expect(events).toEqual(['stop', 'start']);
      expect(value.marks).toEqual([]);
    });

    it('says the restart failed when the daemon does not come up, and keeps no record', async () => {
      const value = fixture();
      const result = await bridge.execute(
        cli(value, { runtime: () => runtime({ daemon: true }, {}) })
      );
      expect([result.status, result.messageCode]).toEqual(['restart_failed', 'restart_failed']);
      expect(value.marks).toEqual([]);
      expect(value.deps.marked()).toBeNull();
    });

    it('says failed with the version installed now when the updater fails, and stops nothing', async () => {
      for (const [left, shown] of [
        [null, '1.0.0'],
        ['codex-cli 1.5.0', '1.5.0'],
      ] as const) {
        const value = fixture();
        const events: string[] = [];
        let installed = 'codex-cli 1.0.0';
        const result = await bridge.execute(
          cli(value, {
            readVersion: async () => installed,
            update: async () => {
              if (left) installed = left;
              throw new Error('network error');
            },
            runtime: () => runtime({ daemon: true }, { daemon: true }, events),
          })
        );
        expect([result.status, result.messageCode, result.version]).toEqual([
          'failed',
          'update_failed',
          shown,
        ]);
        expect(events).toEqual([]);
        expect(value.marks).toEqual([]);
      }
    });

    it('says current and stops nothing when the version did not change', async () => {
      const value = fixture();
      const events: string[] = [];
      const result = await bridge.execute(
        cli(value, {
          update: async () => {},
          runtime: () => runtime({ daemon: true }, { daemon: true }, events),
        })
      );
      expect(result.status).toBe('current');
      expect(events).toEqual([]);
      expect(value.marks).toEqual([]);
    });

    it.each([
      { from: '0.50.0-alpha.1', to: '0.50.0', status: 'updated' },
      { from: '0.50.0', to: '0.50.0-alpha.1', status: 'updated' },
      { from: '0.50.0-alpha.1', to: '0.50.0-alpha.1', status: 'current' },
    ])('judges a change on the whole version: $from -> $to', async ({ from, to, status }) => {
      const value = fixture();
      let installed = from;
      const result = await bridge.execute(
        cli(value, {
          readVersion: async () => `codex-cli ${installed}`,
          update: async () => {
            installed = to;
          },
          runtime: () => runtime({ daemon: true }, { daemon: true }),
        })
      );
      expect(result.status).toBe(status);
    });

    it('carries nothing over from a marker an older version left', async () => {
      const value = fixture();
      value.deps.restoreMarker(
        JSON.stringify({
          version: '1.0.0',
          desktop: false,
          daemon: true,
          desktops: 0,
          install: { earlier: null, changed: true },
        })
      );
      const events: string[] = [];
      const result = await bridge.execute(
        cli(value, { update: async () => {}, runtime: () => runtime({ daemon: true }, {}, events) })
      );
      expect(result.status).toBe('current');
      expect(events).toEqual([]);
    });

    it('reports Codex busy and changes nothing while it stays busy', async () => {
      const value = fixture();
      const result = await bridge.execute(
        cli(value, {
          idleSeconds: 1,
          runtime: () => ({
            ...runtime({ daemon: true }),
            stop: async () => {
              throw Object.assign(new Error('private'), { code: 'busy' });
            },
          }),
        })
      );
      expect([result.status, result.messageCode]).toEqual(['action_required', 'codex_busy']);
      expect(value.marks).toEqual([]);
    });

    it('says the restart failed when stop() fails after the update', async () => {
      const value = fixture();
      const result = await bridge.execute(
        cli(value, {
          runtime: () => ({
            ...runtime({ daemon: true }, { daemon: true }),
            stop: async () => {
              throw new Error('private');
            },
          }),
        })
      );
      expect([result.status, result.version]).toEqual(['restart_failed', '2.0.0']);
      expect(value.marks).toEqual([]);
    });

    it('takes the daemon to be ready only when it is the one the readiness check vouched for', async () => {
      for (const [identity, status] of [
        [DAEMON, 'updated'],
        [{ pid: 2, startTime: '2' }, 'restart_failed'],
      ] as const) {
        const value = fixture();
        const result = await bridge.execute(
          cli(value, {
            runtime: () => ({
              ...runtime({ daemon: true }, { daemon: true }),
              restartState: async () => ({ daemon: true, proxy: false, daemonIdentity: identity }),
            }),
          })
        );
        expect(result.status).toBe(status);
      }
    });

    it('gives a daemon a live proxy brings back a short while', async () => {
      const value = fixture();
      let scans = 0;
      const result = await bridge.execute(
        cli(value, {
          runtime: () => ({
            ...runtime({ daemon: true }, { daemon: true }),
            restartState: async () =>
              scans++ < 2
                ? { daemon: false, proxy: true, daemonIdentity: null }
                : { daemon: true, proxy: true, daemonIdentity: DAEMON },
          }),
        })
      );
      expect(result.status).toBe('updated');
      expect(value.events).toContain('wait');
    });
  });

  describe('Codex Desktop: a desktop it closed is owed until AAC relaunches it', () => {
    const desktop = (value: ReturnType<typeof fixture>, extra: object = {}) => ({
      ...value.deps,
      operation: 'desktop',
      expectedCandidate: '2.0.0',
      ...extra,
    });

    it('updates, relaunches every desktop it closed, and clears the marker', async () => {
      const value = fixture();
      const result = await bridge.execute(
        desktop(value, { runtime: () => runtime({ desktops: 1 }, { desktops: 1 }) })
      );
      expect(result.status).toBe('updated');
      expect(value.deps.marked()).toBeNull();
    });

    it('records the desktops before stop() closes anything, with the install target', async () => {
      const value = fixture();
      let seen: unknown = null;
      await bridge.execute(
        desktop(value, {
          runtime: (hooks: { beforeStop?: (plan: Plan) => void } = {}) => ({
            ...runtime({ desktops: 1 }, { desktops: 1 }),
            stop: async () => {
              hooks.beforeStop?.({ daemon: true, desktops: 1 });
              seen = value.deps.marked();
            },
          }),
        })
      );
      // Desktops only: the daemon is never carried to a later click.
      expect(seen).toEqual({ version: '1.0.0', desktop: true, desktops: 1, target: '2.0.0' });
    });

    it('owes the desktops start() did not see come up, and only those', async () => {
      const value = fixture();
      const result = await bridge.execute(
        desktop(value, { runtime: () => runtime({ desktops: 2 }, { desktops: 1 }) })
      );
      expect(result.status).toBe('restart_failed');
      expect(value.deps.marked()).toEqual({ version: '1.0.0', desktop: true, desktops: 1 });
    });

    it('never clears desktops an earlier run owes, whatever runs now, and says so once', async () => {
      for (const marker of [{ desktop: true, desktops: 2 }, { desktop: true }, {}]) {
        const value = fixture();
        value.deps.restoreMarker(JSON.stringify({ version: '1.0.0', ...marker }));
        const result = await bridge.execute(
          desktop(value, {
            expectedCandidate: '1.0.0',
            restartOnly: true,
            runtime: () => runtime({ desktops: 1 }, { desktops: 1 }),
          })
        );
        expect([result.status, result.messageCode]).toEqual([
          'restart_failed',
          'restart_not_running',
        ]);
        expect(value.deps.marked()).toBeNull();
      }
    });

    it('owes nothing after a failed install once everything is back', async () => {
      const value = fixture();
      const result = await bridge.execute(
        desktop(value, {
          update: async () => {
            throw new Error('apt failed');
          },
          runtime: () => runtime({ desktops: 1 }, { desktops: 1 }),
        })
      );
      expect(result.status).toBe('failed');
      expect(value.deps.marked()).toBeNull();
    });

    it('keeps the desktops a failed install left closed', async () => {
      const value = fixture();
      await bridge.execute(
        desktop(value, {
          update: async () => {
            throw new Error('apt failed');
          },
          runtime: () => runtime({ desktops: 1 }, {}),
        })
      );
      expect(value.deps.marked()).toEqual({
        version: '1.0.0',
        desktop: true,
        desktops: 1,
        target: '2.0.0',
      });
    });

    it('records the desktops a failed stop() closed and did not bring back', async () => {
      const value = fixture();
      const result = await bridge.execute(
        desktop(value, {
          runtime: () => ({
            ...runtime({ desktops: 2 }, { desktops: 1 }),
            stop: async () => {
              throw new Error('private');
            },
          }),
        })
      );
      expect(result.status).not.toBe('updated');
      expect(value.deps.marked()).toEqual({
        version: '1.0.0',
        desktop: true,
        desktops: 1,
        target: '2.0.0',
      });
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
      if (process.env.FIXTURE_AT_VERIFY && fs.existsSync(process.env.FIXTURE_MARKER)) fs.copyFileSync(process.env.FIXTURE_MARKER, process.env.FIXTURE_AT_VERIFY);
      return process.env.FIXTURE_VERIFY === 'fail' ? null : (await read()).daemonIdentity;
    },
    restartState: async () => ({ ...(await read()), proxy: false }),
    stop: async () => {
      if (process.env.FIXTURE_AT_STOP && fs.existsSync(process.env.FIXTURE_MARKER)) fs.copyFileSync(process.env.FIXTURE_MARKER, process.env.FIXTURE_AT_STOP);
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

  it('starts the updated executable through the runtime launcher, and keeps no CLI marker', () => {
    const { result, record, marker, codex } = run('launch', {
      marker: null,
      next: '3.0.0',
      procs: running,
    });
    // The stand-in never brings a daemon up, so the restart is said to have failed.
    expect(result.status).toBe('restart_failed');
    expect(result.restartedProcesses).toBe(1);
    const target = read(record);
    expect(target.exe).toBe(codex);
    expect(target.env).toEqual({ KEEP: 'yes' });
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('calls an unchanged CLI current and starts nothing', () => {
    const { result, record, marker } = run('launch', { marker: null, procs: running });
    expect(result.status).toBe('current');
    expect(fs.existsSync(record)).toBe(false);
    expect(fs.existsSync(marker)).toBe(false);
  });

  it('leaves a marker an older version wrote alone and carries nothing from it', () => {
    const old = { version: '2.0.0', desktop: false, daemon: true, desktops: 0 };
    const { result, marker } = run('launch', { marker: old, procs: running });
    expect(result.status).toBe('current');
    expect(read(marker)).toEqual(old);
  });
});
