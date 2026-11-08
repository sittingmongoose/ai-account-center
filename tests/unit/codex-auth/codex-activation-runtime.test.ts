import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { spawnSync } from 'child_process';
import * as runtimeModule from '../../../src/codex-auth/codex-activation-runtime';
import {
  isDesktop,
  CodexActivationRuntimeError,
  CodexActivationRuntimeDependencies,
  CodexProcessSnapshot,
  createCodexActivationRuntime,
  launchCodexProcess,
  prepareAppServerLog,
  readRolloutLifecycle,
} from '../../../src/codex-auth/codex-activation-runtime';
import type { CodexActivationStopPlan } from '../../../src/codex-auth/codex-activation-confirmation';

const home = '/fixture/.codex';
function processFixture(
  pid: number,
  args: string[],
  extra: Partial<CodexProcessSnapshot> = {}
): CodexProcessSnapshot {
  return {
    pid,
    ppid: 1,
    startTime: String(pid),
    state: 'S',
    args,
    exe: args[0],
    cwd: '/fixture',
    env: { HOME: '/fixture', CODEX_HOME: home },
    ...extra,
  };
}
const daemon = processFixture(10, [
  '/fixture/bin/codex',
  '-c',
  'features.code_mode_host=true',
  'app-server',
  '--listen',
  'unix://',
]);
const desktop = processFixture(20, ['/usr/lib/chatgpt/ChatGPT'], { env: {} });
const bundled = processFixture(21, ['/usr/lib/chatgpt/resources/codex', 'app-server'], {
  ppid: 20,
  env: {
    HOME: '/fixture',
    CODEX_HOME: home,
    DISPLAY: ':8',
    XAUTHORITY: '/fixture/.Xauthority',
    DBUS_SESSION_BUS_ADDRESS: 'unix:path=/run/user/fixture/bus',
    CODEX_APP_TOOLS_PIPE_PATH: '/old/stale/pipe',
    BROWSER_USE_ACCESS_VERIFYING_IDENTITY: 'fake-internal-value',
  },
});
const renderer = processFixture(
  22,
  ['/usr/lib/chatgpt/ChatGPT', '--type=renderer', '--user-data-dir=/fixture/.config/Codex'],
  { ppid: 20 }
);

function harness(initial: CodexProcessSnapshot[]) {
  let current = initial.map((entry) => ({
    ...entry,
    args: [...entry.args],
    env: { ...entry.env },
  }));
  let clock = 0;
  let nextPid = 100;
  const events: string[] = [];
  const launches: CodexProcessSnapshot[] = [];
  let failDesktop = false;
  const removeTree = (pid: number): void => {
    const childPids = current.filter((entry) => entry.ppid === pid).map((entry) => entry.pid);
    for (const child of childPids) removeTree(child);
    current = current.filter((entry) => entry.pid !== pid);
  };
  const deps: CodexActivationRuntimeDependencies = {
    platform: 'linux',
    scan: async () => [...current],
    acquireStartupLock: async () => {
      events.push('lock');
      return async () => {
        events.push('unlock');
      };
    },
    acquireNativeStartupLock: async () => {
      events.push('native-lock');
    },
    releaseNativeStartupLock: async () => {
      events.push('native-unlock');
    },
    assertIdle: async () => {
      events.push('idle');
    },
    signal: async (target, signal) => {
      events.push(`${signal}:${target.pid}`);
      removeTree(target.pid);
    },
    launch: async (target, isDesktop) => {
      events.push(isDesktop ? 'start-desktop' : 'start-daemon');
      launches.push(target);
      if (isDesktop && failDesktop) {
        failDesktop = false;
        throw new CodexActivationRuntimeError('fixture desktop failed');
      }
      const started = { ...target, pid: nextPid++, startTime: String(nextPid) };
      current.push(started);
      if (isDesktop)
        current.push({ ...bundled, pid: nextPid++, ppid: started.pid, startTime: String(nextPid) });
      // The pid it spawned, as a real launch reports it.
      return { pid: started.pid, startTime: started.startTime };
    },
    prepareCli: async () => {
      events.push('prepare-cli');
    },
    launchCli: async (target) => {
      events.push('start-cli');
      const started = { ...target, args: [target.exe], pid: nextPid++, startTime: String(nextPid) };
      launches.push(started);
      current.push(started);
    },
    verifyDaemon: async () => {
      events.push('healthy');
    },
    removeControlSocket: () => {
      events.push('retire-socket');
    },
    sleep: async (ms) => {
      clock += ms;
    },
    now: () => clock,
  };
  return {
    deps,
    events,
    launches,
    current: () => current,
    inject: (entry: CodexProcessSnapshot) => {
      current.push({ ...entry, args: [...entry.args], env: { ...entry.env } });
    },
    failDesktop: () => {
      failDesktop = true;
    },
  };
}

describe('Codex Desktop update retry on the real runtime', () => {
  const bridge = require('../../../scripts/app-updates/app_update_codex.cjs');

  function click(
    fake: ReturnType<typeof harness>,
    launch?: CodexActivationRuntimeDependencies['launch']
  ) {
    let clock = 0;
    let installed = '1.0.0';
    let marker: Record<string, unknown> | null = null;
    const deps = {
      now: () => clock,
      sleep: async (ms: number) => {
        clock += ms;
      },
      readVersion: async () => installed,
      update: async () => {
        installed = '2.0.0';
      },
      pending: (expected: string) => marker?.version === expected,
      marked: () => marker,
      markerSnapshot: () => (marker ? JSON.stringify(marker) : null),
      restoreMarker: (text: string) => {
        marker = JSON.parse(text);
      },
      mark: (closed: Record<string, unknown> = {}) => {
        // As the real marker does: the version the bridge holds now.
        marker = { version: bridge.output.version, ...closed };
      },
      clear: () => {
        marker = null;
      },
      hasOldProxies: async () => false,
      runtime: () =>
        createCodexActivationRuntime(home, {
          ...fake.deps,
          launch: (target, isDesktop) =>
            bridge.relaunch(target, isDesktop, launch ?? fake.deps.launch, { inside: () => true }),
        }),
      operation: 'desktop',
    };
    return {
      deps,
      marker: () => marker,
      run: async (extra: object) => {
        bridge.output.restartedProcesses = 0;
        return { ...(await bridge.execute({ ...deps, expectedCandidate: '2.0.0', ...extra })) };
      },
    };
  }

  it('reports restart_not_running, never updated, while the desktop stays closed', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    fake.failDesktop();
    const { run, marker } = click(fake);
    const first = await run({});
    expect(first.status).toBe('restart_failed');
    expect(first.restartedProcesses).toBe(1);
    expect(marker()).toEqual({
      version: '2.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });

    const second = await run({ restartOnly: true });
    expect(second.restartedProcesses).toBe(1);
    expect(second.status).toBe('restart_failed');
    expect(second.messageCode).toBe('restart_not_running');
    expect(marker()).toBeNull();
  });

  const proxyChild = processFixture(
    23,
    ['/usr/lib/chatgpt/resources/codex', 'app-server', 'proxy'],
    { ppid: 20, env: { HOME: '/fixture', CODEX_HOME: home, DISPLAY: ':8' } }
  );

  it('remembers a desktop whose only Codex child was a proxy', async () => {
    const fake = harness([desktop, proxyChild]);
    const { run, marker } = click(fake, async (target, isDesktop) => {
      const own = await fake.deps.launch(target, isDesktop);
      const servers = fake.current().filter((entry) => entry.exe.endsWith('/resources/codex'));
      for (const server of servers) await fake.deps.signal(server, 'SIGKILL');
      return own;
    });
    const first = await run({});
    expect(first.status).toBe('restart_failed');
    expect(marker()).toEqual({
      version: '2.0.0',
      desktop: true,
      daemon: false,
      desktops: 1,
    });

    const second = await run({ restartOnly: true });
    expect(second.status).toBe('restart_failed');
    expect(second.messageCode).toBe('restart_not_running');
  });

  it('records the daemon when stop() closes it and its own recovery fails', async () => {
    const fake = harness([daemon]);
    const { deps, marker } = click(fake, async () => {
      throw new CodexActivationRuntimeError('fixture launch failed');
    });
    const interrupted = {
      ...deps,
      runtime: () =>
        createCodexActivationRuntime(home, {
          ...fake.deps,
          signal: async (target, signal) => {
            await fake.deps.signal(target, signal);
            // A new writer appears once the daemon is gone, so stop() must recover.
            fake.inject(processFixture(30, ['/fixture/bin/codex', 'exec'], {}));
          },
          launch: async () => {
            throw new CodexActivationRuntimeError('fixture launch failed');
          },
        }),
    };
    bridge.output.restartedProcesses = 0;
    const result = {
      ...(await bridge.execute({ ...interrupted, operation: 'cli', expectedCandidate: '2.0.0' })),
    };
    expect(result.status).toBe('restart_failed');
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  const dataDesktop = (pid: number, data: string) =>
    processFixture(pid, ['/usr/lib/chatgpt/ChatGPT', `--user-data-dir=${data}`], {
      env: { HOME: '/fixture', DISPLAY: ':8' },
    });
  const childOf = (pid: number, ppid: number) => ({
    ...bundled,
    pid,
    ppid,
    startTime: String(pid),
  });

  it('names a daemon that appeared just before stop() took its lock', async () => {
    const fake = harness([]);
    const { deps, marker } = click(fake);
    const result = {
      ...(await bridge.execute({
        ...deps,
        operation: 'cli',
        runtime: () =>
          createCodexActivationRuntime(home, {
            ...fake.deps,
            acquireStartupLock: async () => {
              fake.inject(daemon);
              return fake.deps.acquireStartupLock();
            },
            launch: async () => {
              throw new CodexActivationRuntimeError('fixture launch failed');
            },
          }),
      })),
    };
    expect(result.status).toBe('restart_failed');
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  it('owes every desktop it closed: one coming back does not satisfy a retry', async () => {
    const fake = harness([
      dataDesktop(40, '/private/data-a'),
      childOf(41, 40),
      dataDesktop(50, '/private/data-b'),
      childOf(51, 50),
    ]);
    const { run, marker } = click(fake, async (target, isDesktop) => {
      if (target.args.join(' ').includes('/private/data-b')) {
        throw new CodexActivationRuntimeError('fixture launch failed');
      }
      return fake.deps.launch(target, isDesktop);
    });
    const first = await run({});
    expect(first.status).toBe('restart_failed');
    expect(marker()).toEqual({ version: '2.0.0', desktop: true, daemon: false, desktops: 2 });
    expect(JSON.stringify(marker())).not.toContain('/private');

    const second = await run({ restartOnly: true });
    expect(second.status).toBe('restart_failed');
    expect(second.messageCode).toBe('restart_not_running');
    expect(marker()).toBeNull();
  });

  it.each(['electron', 'none', 'node'])(
    'never lets a Node-mode Electron helper stand in for a closed desktop (helper: %s)',
    async (helper) => {
      const exe = '/usr/lib/chatgpt/ChatGPT';
      const fallback = path.join(os.homedir(), '.config', 'Codex');
      const at = (pid: number, args: string[]) =>
        processFixture(pid, args, { env: { HOME: '/fixture', DISPLAY: ':8' }, exe });
      // Under desktop B, a helper owning B's app server: Electron itself in Node mode, or plain node.
      const helperOf = (fake: ReturnType<typeof harness>, mainPid: number, pid: number) => {
        if (helper === 'none') {
          fake.inject(childOf(pid + 1, mainPid));
          return;
        }
        fake.inject(
          processFixture(pid, [helper === 'node' ? '/usr/bin/node' : exe, '/fixture/helper.js'], {
            ppid: mainPid,
            exe: helper === 'node' ? '/usr/bin/node' : exe,
            env: { HOME: '/fixture', DISPLAY: ':8', ELECTRON_RUN_AS_NODE: '1' },
          })
        );
        fake.inject(childOf(pid + 1, pid));
      };
      const fake = harness([at(40, [exe, `--user-data-dir=${fallback}`]), childOf(41, 40)]);
      let nextHelper = 300;
      const { run, marker } = click(fake, async (target, isDesktopLaunch) => {
        // Desktop A, on the default folder named explicitly, never comes back.
        if (isDesktopLaunch && target.args.includes(`--user-data-dir=${fallback}`)) {
          throw new CodexActivationRuntimeError('fixture launch failed');
        }
        const before = new Set(fake.current().map((entry) => entry.pid));
        const own = await fake.deps.launch(target, isDesktopLaunch);
        if (!isDesktopLaunch || !target.args.includes('--user-data-dir=/x/b')) return own;
        // The harness gives a relaunched desktop an app server; B's lives under its helper.
        const added = fake.current().filter((entry) => !before.has(entry.pid));
        const main = added.find((entry) => entry.args.includes('--user-data-dir=/x/b'));
        for (const entry of added.filter((item) => item !== main))
          await fake.deps.signal(entry, 'SIGKILL');
        helperOf(fake, (main as CodexProcessSnapshot).pid, (nextHelper += 10));
        return own;
      });
      expect((await run({})).status).toBe('restart_failed');
      expect(marker()).toMatchObject({ desktops: 1 });

      // Before the retry, desktop B runs, its app server under the helper. A stays closed.
      fake.inject(at(50, [exe, '--user-data-dir=/x/b']));
      helperOf(fake, 50, 52);
      const second = await run({ restartOnly: true, launchOnly: true });
      expect([second.status, second.messageCode]).toEqual([
        'restart_failed',
        'restart_not_running',
      ]);
    }
  );

  it('relaunches each of two closed desktops, and owes the one that does not come back', async () => {
    // Electron rewrote each main cmdline into one string, so `/x/a --b` and `/x/a --c` can't be told
    // from a folder `/x/a` and a flag. Both read as `/x/a`, and each is still its own desktop.
    const rewritten = (pid: number, tail: string) =>
      processFixture(pid, [`/usr/lib/chatgpt/ChatGPT --user-data-dir=/x/a ${tail}`], {
        env: { HOME: '/fixture', DISPLAY: ':8' },
        exe: '/usr/lib/chatgpt/ChatGPT',
      });
    const pair = () => [
      rewritten(40, '--b'),
      childOf(41, 40),
      rewritten(50, '--c'),
      childOf(51, 50),
    ];

    const both = harness(pair());
    const first = click(both);
    expect((await first.run({})).status).toBe('updated');
    expect(both.launches.filter(isDesktop).map((entry) => entry.args)).toEqual([
      ['/usr/lib/chatgpt/ChatGPT', '--user-data-dir=/x/a', '--b'],
      ['/usr/lib/chatgpt/ChatGPT', '--user-data-dir=/x/a', '--c'],
    ]);
    expect(first.marker()).toBeNull();

    // The second relaunch fails: one desktop fewer is running, so it is not updated.
    const fake = harness(pair());
    let failSecond = true;
    const { run, marker } = click(fake, async (target, isDesktop) => {
      if (failSecond && target.args.includes('--c')) {
        throw new CodexActivationRuntimeError('fixture launch failed');
      }
      return fake.deps.launch(target, isDesktop);
    });
    const failed = await run({});
    expect(failed.status).toBe('restart_failed');
    // Closed two, and the one back is still counted until the run ends: any count is a restart owed.
    expect((marker() as { desktops: number }).desktops).toBeGreaterThan(0);

    // A retry finds only the one desktop running, closes and reopens it: one is still missing.
    failSecond = false;
    const second = await run({ restartOnly: true, launchOnly: true });
    expect(second.status).toBe('restart_failed');
    expect(second.messageCode).toBe('restart_not_running');
  });

  describe('a failed stop() over two desktops', () => {
    const rewritten = (pid: number, tail: string) =>
      processFixture(pid, [`/usr/lib/chatgpt/ChatGPT --user-data-dir=/fixture/Profile ${tail}`], {
        env: { HOME: '/fixture', DISPLAY: ':8' },
        exe: '/usr/lib/chatgpt/ChatGPT',
      });
    const pair = () => [
      rewritten(40, '--one'),
      childOf(41, 40),
      rewritten(50, '--two'),
      childOf(51, 50),
    ];
    /** A runtime whose desktop relaunches fail for the flag in `failing`, while it is set. */
    const runtimeFor =
      (
        fake: ReturnType<typeof harness>,
        failing: { flag: string | null },
        overrides: Partial<CodexActivationRuntimeDependencies> = {}
      ) =>
      (hooks = {}) =>
        createCodexActivationRuntime(home, {
          ...fake.deps,
          launch: (target, isDesktop) =>
            bridge.relaunch(
              target,
              isDesktop,
              async (entry, desktop) => {
                if (failing.flag && entry.args.includes(failing.flag)) {
                  throw new CodexActivationRuntimeError('fixture launch failed');
                }
                return fake.deps.launch(entry, desktop);
              },
              { inside: () => true }
            ),
          ...overrides,
          ...hooks,
        });

    it('owes the desktop a failed stop() closed and its recovery did not bring back', async () => {
      const fake = harness(pair());
      const failing = { flag: '--two' as string | null };
      let scanFails = false;
      let faulted = false;
      const runtime = runtimeFor(fake, failing, {
        scan: async () => {
          if (scanFails) {
            scanFails = false;
            throw new CodexActivationRuntimeError('fixture inventory failed');
          }
          return fake.deps.scan();
        },
        signal: async (target, signal) => {
          await fake.deps.signal(target, signal);
          // Once both are gone, one inventory scan fails, so stop() fails and recovers.
          if (!faulted && !fake.current().some(isDesktop)) faulted = scanFails = true;
        },
      });
      const { run, marker } = click(fake);
      expect((await run({ runtime })).status).toBe('restart_failed');
      expect(fake.current().filter(isDesktop)).toHaveLength(1);
      expect((marker() as { desktops: number }).desktops).toBe(1);

      // The next update finds only the recovered desktop, and one is still closed.
      failing.flag = null;
      const second = await run({ runtime });
      expect([second.status, second.messageCode]).toEqual([
        'restart_failed',
        'restart_not_running',
      ]);
    });

    it('recovers the desktop stop() closed, never its running twin', async () => {
      const fake = harness(pair());
      const runtime = runtimeFor(
        fake,
        { flag: null },
        {
          signal: async (target, signal) => {
            if (target.pid === 50) throw new CodexActivationRuntimeError('fixture signal failed');
            await fake.deps.signal(target, signal);
          },
        }
      );
      const { run } = click(fake);
      await run({ runtime });
      expect(fake.launches.filter(isDesktop).map((entry) => entry.args)).toEqual([
        ['/usr/lib/chatgpt/ChatGPT', '--user-data-dir=/fixture/Profile', '--one'],
      ]);
      expect(fake.current().filter(isDesktop)).toHaveLength(2);
    });

    it('owes only the desktop a failed stop() closed, not the one it never reached', async () => {
      const fake = harness(pair());
      const failing = { flag: '--one' as string | null };
      let refuse = true;
      const runtime = runtimeFor(fake, failing, {
        signal: async (target, signal) => {
          // The second desktop refuses its signal, so stop() fails with it still running.
          if (refuse && target.pid === 50) {
            throw new CodexActivationRuntimeError('fixture signal failed');
          }
          await fake.deps.signal(target, signal);
        },
      });
      const { run, marker } = click(fake);
      expect((await run({ runtime })).status).toBe('restart_failed');
      expect(
        fake
          .current()
          .filter(isDesktop)
          .map((entry) => entry.pid)
      ).toEqual([50]);
      expect((marker() as { desktops: number }).desktops).toBe(1);

      refuse = false;
      failing.flag = null;
      const second = await run({ runtime });
      expect([second.status, second.messageCode]).toEqual([
        'restart_failed',
        'restart_not_running',
      ]);
    });
  });

  it('never clears an owed desktop by finding one running on its folder', async () => {
    const fake = harness([dataDesktop(40, '/private/data-a'), childOf(41, 40)]);
    let fail = true;
    const { run, marker } = click(fake, async (target, isDesktopLaunch) => {
      if (fail && isDesktopLaunch) throw new CodexActivationRuntimeError('fixture launch failed');
      return fake.deps.launch(target, isDesktopLaunch);
    });
    expect((await run({})).status).toBe('restart_failed');
    expect(marker()).toEqual({ version: '2.0.0', desktop: true, daemon: false, desktops: 1 });

    // The user reopens it. The retry closes and relaunches that one, and still can't call the earlier
    // restart done: only AAC's own relaunch of the desktop it closed clears it.
    fail = false;
    fake.inject(dataDesktop(60, '/private/data-a'));
    fake.inject(childOf(61, 60));
    const second = await run({ restartOnly: true, launchOnly: true });
    expect([second.status, second.messageCode]).toEqual(['restart_failed', 'restart_not_running']);
    expect(marker()).toBeNull();
  });

  it('never takes another desktop for the one it launched', async () => {
    const fake = harness([dataDesktop(40, '/private/data-a'), childOf(41, 40)]);
    const { run, marker } = click(fake, async (target, isDesktopLaunch) => {
      if (!isDesktopLaunch) return fake.deps.launch(target, isDesktopLaunch);
      // AAC's launch exits at once, and a desktop someone else opened on the same folder comes up.
      fake.inject(dataDesktop(70, '/private/data-a'));
      fake.inject(childOf(71, 70));
      return { pid: 999 };
    });
    expect((await run({})).status).toBe('restart_failed');
    expect(marker()).toEqual({ version: '2.0.0', desktop: true, daemon: false, desktops: 1 });
  });

  const withoutServer = (fake: ReturnType<typeof harness>) =>
    (async (target, isDesktop) => {
      const own = await fake.deps.launch(target, isDesktop);
      if (!isDesktop) return own;
      for (const server of fake.current().filter((entry) => entry.exe.endsWith('/resources/codex')))
        await fake.deps.signal(server, 'SIGKILL');
      return own;
    }) as CodexActivationRuntimeDependencies['launch'];

  /** A click whose stop() retires what it was given, then meets a new writer and must recover. */
  function interruptedClick(
    fake: ReturnType<typeof harness>,
    launch: CodexActivationRuntimeDependencies['launch'],
    signal: CodexActivationRuntimeDependencies['signal']
  ) {
    const { deps, marker } = click(fake);
    return {
      marker,
      run: async () => {
        bridge.output.restartedProcesses = 0;
        return {
          ...(await bridge.execute({
            ...deps,
            operation: 'cli',
            expectedCandidate: '2.0.0',
            runtime: (hooks: Partial<CodexActivationRuntimeDependencies> = {}) =>
              createCodexActivationRuntime(home, {
                ...fake.deps,
                signal,
                launch: (target, isDesktop) =>
                  bridge.relaunch(target, isDesktop, launch, { inside: () => true }),
                ...hooks,
              }),
          })),
        };
      },
    };
  }

  it('owes a desktop a failed stop() retired when its relaunch never got an app server', async () => {
    const fake = harness([desktop, bundled, renderer]);
    const { run, marker } = interruptedClick(fake, withoutServer(fake), async (target, signal) => {
      await fake.deps.signal(target, signal);
      fake.inject(processFixture(30, ['/fixture/bin/codex', 'exec'], {}));
    });
    expect((await run()).status).toBe('restart_failed');
    expect(fake.current().some(isDesktop)).toBe(true);
    expect(marker()).toEqual({
      version: '2.0.0',
      desktop: true,
      daemon: false,
      desktops: 1,
    });
  });

  it('owes a daemon a failed stop() retired when its relaunch fails the readiness check', async () => {
    const fake = harness([daemon]);
    let checks = 0;
    const { deps, marker } = click(fake);
    const result = {
      ...(await bridge.execute({
        ...deps,
        operation: 'cli',
        expectedCandidate: '2.0.0',
        runtime: () =>
          createCodexActivationRuntime(home, {
            ...fake.deps,
            signal: async (target, signal) => {
              await fake.deps.signal(target, signal);
              // A new writer appears once the daemon is gone, so stop() must recover.
              fake.inject(processFixture(30, ['/fixture/bin/codex', 'exec'], {}));
            },
            launch: (target, isDesktop) =>
              bridge.relaunch(target, isDesktop, fake.deps.launch, { inside: () => true }),
            verifyDaemon: async () => {
              checks++;
              throw new CodexActivationRuntimeError('socket not bound');
            },
          }),
      })),
    };
    expect(result.status).toBe('restart_failed');
    // Recovery relaunched it, so a daemon is running, but it never passed the check.
    expect(fake.current().some((entry) => entry.args.includes('--listen'))).toBe(true);
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 });
    expect(checks).toBeGreaterThan(1);
  });

  it('keeps the plan when every scan fails after stop() retired the daemon', async () => {
    const fake = harness([daemon]);
    let broken = false;
    const { deps, marker } = click(fake);
    const result = {
      ...(await bridge.execute({
        ...deps,
        operation: 'cli',
        expectedCandidate: '2.0.0',
        runtime: () =>
          createCodexActivationRuntime(home, {
            ...fake.deps,
            scan: async () => {
              if (broken) throw new CodexActivationRuntimeError('Codex inventory failed.');
              return fake.deps.scan();
            },
            signal: async (target, signal) => {
              await fake.deps.signal(target, signal);
              broken = true;
            },
          }),
      })),
    };
    expect(result.status).toBe('restart_failed');
    expect(fake.current().some((entry) => entry.args.includes('--listen'))).toBe(false);
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 });
  });

  it('does not owe a proxy-only desktop a failed stop() never retired', async () => {
    const fake = harness([desktop, proxyChild]);
    const refuse = async () => {
      throw new CodexActivationRuntimeError('fixture failed');
    };
    const { run, marker } = interruptedClick(fake, refuse, refuse);
    await run();
    // Still running, and not ready: only the fact it was never retired keeps it out of the marker.
    // The plan under the lock found no daemon, so the install intent is settled too.
    expect(fake.current().some(isDesktop)).toBe(true);
    expect(marker()).toBeNull();
  });

  it('keeps what stop() closed when the install fails and recovery fails too', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const { run, marker } = click(fake, async () => {
      throw new CodexActivationRuntimeError('fixture launch failed');
    });
    const failed = {
      update: async () => {
        throw new Error('fixture apt failed');
      },
    };
    const first = await run(failed);
    expect(first.status).toBe('restart_failed');
    expect(marker()).toEqual({
      version: '1.0.0',
      target: '2.0.0',
      desktop: true,
      daemon: true,
      desktops: 1,
    });
    const second = await run({ ...failed, expectedCandidate: '1.0.0', restartOnly: true });
    expect(second.messageCode).toBe('restart_not_running');
  });

  it('keeps the record when an installer replaces the app, fails, and the relaunch fails', async () => {
    const fake = harness([daemon]);
    let installedNow = '1.0.0';
    const { run, marker } = click(fake, async () => {
      throw new CodexActivationRuntimeError('fixture launch failed');
    });
    const partial = {
      readVersion: async () => installedNow,
      update: async () => {
        installedNow = '2.0.0';
        throw new Error('fixture installer failed');
      },
    };
    const first = await run(partial);
    expect(first.status).toBe('restart_failed');
    expect(first.version).toBe('2.0.0');
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 });
    // Both installed and candidate are now 2.0.0: the record still applies, and is still owed.
    const second = await run({ ...partial, restartOnly: true });
    expect(second.messageCode).toBe('restart_not_running');
  });

  /** A retry owing a daemon, whose stop() finds only a proxy that respawns it as the lock is released. */
  function respawnedDaemon(verifyDaemon: CodexActivationRuntimeDependencies['verifyDaemon']) {
    const proxy = processFixture(70, ['/fixture/bin/codex', 'app-server', 'proxy'], {
      env: { HOME: '/fixture', CODEX_HOME: home },
    });
    const fake = harness([proxy]);
    let unlocked = 0;
    const { deps } = click(fake);
    return {
      fake,
      run: async () => {
        bridge.output.restartedProcesses = 0;
        return {
          ...(await bridge.execute({
            ...deps,
            operation: 'cli',
            update: async () => {},
            readVersion: async () => '2.0.0',
            pending: () => true,
            marked: () => ({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 }),
            runtime: () =>
              createCodexActivationRuntime(home, {
                ...fake.deps,
                verifyDaemon,
                releaseNativeStartupLock: async () => {
                  await fake.deps.releaseNativeStartupLock();
                  if (++unlocked === 1) fake.inject(daemon);
                },
              }),
          })),
        };
      },
    };
  }

  it('does not take a respawned daemon for ready without the readiness check', async () => {
    let checks = 0;
    const { run } = respawnedDaemon(async () => {
      checks++;
      throw new CodexActivationRuntimeError('socket not bound');
    });
    const result = await run();
    expect(checks).toBe(1);
    expect(result.messageCode).toBe('restart_not_running');
  });

  /** A retry owing a daemon, where daemon B replaces the one that just passed the bridge's check. */
  function replacedAfterCheck(replacementReady: boolean) {
    const fake = harness([daemon]);
    const checked: number[] = [];
    const { deps, marker } = click(fake);
    const replacement = { ...daemon, pid: 999, startTime: '999' };
    return {
      fake,
      checked,
      marker,
      run: async () => {
        bridge.output.restartedProcesses = 0;
        return {
          ...(await bridge.execute({
            ...deps,
            operation: 'cli',
            update: async () => {},
            readVersion: async () => '2.0.0',
            pending: () => true,
            marked: () => ({ version: '2.0.0', desktop: false, daemon: true, desktops: 0 }),
            runtime: (hooks = {}) =>
              createCodexActivationRuntime(home, {
                ...fake.deps,
                ...hooks,
                launch: (target, isDesktop) =>
                  bridge.relaunch(target, isDesktop, fake.deps.launch, { inside: () => true }),
                verifyDaemon: async (target) => {
                  checked.push(target.pid);
                  if (target.pid === replacement.pid) {
                    if (!replacementReady) throw new CodexActivationRuntimeError('never bound');
                    return;
                  }
                  // The bridge's own check (after start()'s): it passes, then B takes over.
                  if (checked.length === 2) {
                    for (const entry of [...fake.current()])
                      await fake.deps.signal(entry, 'SIGKILL');
                    fake.inject(replacement);
                  }
                },
              }),
          })),
        };
      },
    };
  }

  it('does not credit a replacement daemon with the readiness of the one it replaced', async () => {
    const { run, checked, marker } = replacedAfterCheck(false);
    const result = await run();
    expect(checked).toContain(999);
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(marker()).toBeNull();
  });

  it('accepts a replacement daemon once it passes its own readiness check', async () => {
    const { run, checked } = replacedAfterCheck(true);
    expect((await run()).status).toBe('updated');
    expect(checked[checked.length - 1]).toBe(999);
  });

  it('leaves no marker and touches no daemon when the CLI updater fails having installed nothing', async () => {
    const fake = harness([daemon]);
    const { deps, marker } = click(fake);
    const result = {
      ...(await bridge.execute({
        ...deps,
        operation: 'cli',
        readVersion: async () => '1.0.0',
        update: async () => {
          throw new Error('fixture network error');
        },
        runtime: (hooks = {}) => createCodexActivationRuntime(home, { ...fake.deps, ...hooks }),
      })),
    };
    expect(result.status).toBe('failed');
    expect(result.messageCode).toBe('update_failed');
    expect(marker()).toBeNull();
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
  });

  it('accepts a respawned daemon once the readiness check passes', async () => {
    let checks = 0;
    const { run } = respawnedDaemon(async () => {
      checks++;
    });
    expect((await run()).status).toBe('updated');
    expect(checks).toBe(1);
  });

  it('does not call a desktop whose app server never started a restart', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const { run, marker } = click(fake, async (target, isDesktop) => {
      const own = await fake.deps.launch(target, isDesktop);
      if (!isDesktop) return own;
      // The app starts, its app server does not: what start() and the retry both wait for.
      const servers = fake.current().filter((entry) => entry.exe.endsWith('/resources/codex'));
      for (const server of servers) await fake.deps.signal(server, 'SIGKILL');
      return own;
    });
    const first = await run({});
    expect(first.status).toBe('restart_failed');
    expect(fake.current().some(isDesktop)).toBe(true);

    const second = await run({ restartOnly: true });
    expect(second.status).toBe('restart_failed');
    expect(second.messageCode).toBe('restart_not_running');
    expect(marker()).toBeNull();
  });
});

describe('What the runtime reports for a retry', () => {
  const state = (entries: CodexProcessSnapshot[]) => {
    const fake = harness(entries);
    return createCodexActivationRuntime(home, fake.deps).restartState();
  };

  it.each([
    { name: 'an app server on this home under it', children: 'server', ready: true },
    { name: 'one under a utility child', children: 'helper', ready: true },
    { name: 'no app server', children: 'none', ready: false },
    { name: 'an app server on another home', children: 'other-home', ready: false },
    { name: 'only a proxy', children: 'proxy', ready: false },
  ])('counts a relaunched desktop back only with $name', async ({ children, ready }) => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      launch: async (target, isDesktopLaunch) => {
        const own = await fake.deps.launch(target, isDesktopLaunch);
        if (!isDesktopLaunch || !own) return own;
        const pid = (own as { pid: number }).pid;
        // Replace the harness's app server with the children this case gives the launched desktop.
        for (const entry of fake.current().filter((item) => item.ppid === pid)) {
          await fake.deps.signal(entry, 'SIGKILL');
        }
        const server = (id: number, ppid: number, extra: Partial<CodexProcessSnapshot> = {}) =>
          fake.inject({ ...bundled, pid: id, ppid, startTime: String(id), ...extra });
        if (children === 'server') server(500, pid);
        if (children === 'helper') {
          fake.inject(
            processFixture(501, ['/usr/lib/chatgpt/ChatGPT', '--type=utility'], { ppid: pid })
          );
          server(502, 501);
        }
        if (children === 'other-home')
          server(503, pid, { env: { ...bundled.env, CODEX_HOME: '/t3/.codex' } });
        if (children === 'proxy') {
          server(504, pid, { args: ['/usr/lib/chatgpt/resources/codex', 'app-server', 'proxy'] });
        }
        return own;
      },
    });
    await runtime.stop();
    if (ready) await runtime.start();
    else await expect(runtime.start()).rejects.toThrow();
  });

  it('counts only the shared daemon on this home as the daemon', async () => {
    const run = async (args: string[], env: Record<string, string> = {}) =>
      (
        await state([
          processFixture(20, ['/fixture/bin/codex', ...args], {
            env: { CODEX_HOME: home, ...env },
          }),
        ])
      ).daemon;
    expect(await run(['app-server', '--listen', 'unix://'])).toBe(true);
    expect(await run(['app-server', '--listen', 'unix://'], { CODEX_HOME: '/t3/.codex' })).toBe(
      false
    );
    expect(await run(['app-server'])).toBe(false);
    expect(await run(['app-server', 'proxy', '--listen', 'unix://'])).toBe(false);
    // A desktop's own app server is not the shared daemon.
    const main = processFixture(10, ['/usr/lib/chatgpt/ChatGPT', '--user-data-dir=/data/a']);
    const server = processFixture(11, ['/usr/lib/chatgpt/resources/codex', 'app-server'], {
      ppid: 10,
      env: { CODEX_HOME: home },
    });
    expect((await state([main, server])).daemon).toBe(false);
  });

  it('names a proxy on this home', async () => {
    const proxy = (env: Record<string, string>) =>
      processFixture(20, ['/fixture/bin/codex', 'app-server', 'proxy'], { env });
    expect((await state([proxy({ CODEX_HOME: home })])).proxy).toBe(true);
    expect((await state([proxy({ CODEX_HOME: '/t3/.codex' })])).proxy).toBe(false);
  });

  it('reports the plan stop() made under its lock, and none before it', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    expect(runtime.stopPlan()).toEqual({ daemon: false, desktops: 0 });
    await runtime.stop();
    expect(runtime.stopPlan()).toEqual({
      daemon: true,
      desktops: 1,
    });
  });

  it('reports as retired only planned processes no longer running', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      signal: async (target, signal) => {
        // The desktop goes, the daemon refuses: stop() fails partway.
        if (target.pid === daemon.pid)
          throw new CodexActivationRuntimeError('fixture signal failed');
        await fake.deps.signal(target, signal);
      },
    });
    expect(await runtime.retiredPlan()).toEqual({ daemon: false, desktops: 0 });
    await expect(runtime.stop()).rejects.toThrow();
    const retired = await runtime.retiredPlan();
    expect(retired.daemon).toBe(false);
    // Recovery relaunched the desktop under a new pid; the planned one is still gone.
    expect(retired.desktops).toBe(1);
  });

  it('runs the readiness check on a running daemon, and refuses with none', async () => {
    const checked: number[] = [];
    const fake = harness([daemon]);
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      verifyDaemon: async (target) => {
        checked.push(target.pid);
      },
    });
    expect(await runtime.verifyRunningDaemon()).toEqual({
      pid: daemon.pid,
      startTime: daemon.startTime,
    });
    expect((await runtime.restartState()).daemonIdentity).toEqual({
      pid: daemon.pid,
      startTime: daemon.startTime,
    });
    expect(checked).toEqual([daemon.pid]);
    const none = createCodexActivationRuntime(home, harness([]).deps);
    expect(await none.verifyRunningDaemon()).toBeNull();
    expect((await none.restartState()).daemonIdentity).toBeNull();
    const failing = createCodexActivationRuntime(home, {
      ...fake.deps,
      verifyDaemon: async () => {
        throw new CodexActivationRuntimeError('socket not bound');
      },
    });
    expect(await failing.verifyRunningDaemon()).toBeNull();
  });

  it('hands its plan to beforeStop under the lock, before anything is signalled', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const plans: unknown[] = [];
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      beforeStop: async (plan) => {
        plans.push(plan);
        fake.events.push('intent');
      },
    });
    await runtime.stop();
    expect(plans).toEqual([{ daemon: true, desktops: 1 }]);
    const intent = fake.events.indexOf('intent');
    expect(intent).toBeGreaterThan(fake.events.indexOf('lock'));
    expect(intent).toBeLessThan(fake.events.findIndex((event) => event.startsWith('SIGTERM')));
  });

  it('closes nothing when its plan cannot be handed over', async () => {
    const fake = harness([daemon]);
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      beforeStop: async () => {
        throw new Error('marker write failed');
      },
    });
    await expect(runtime.stop()).rejects.toThrow();
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
    expect(fake.current()).toHaveLength(1);
  });

  it('reports no plan when stop() refused before closing anything', async () => {
    const fake = harness([daemon]);
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      assertIdle: async () => {
        throw new CodexActivationRuntimeError('working', 'busy');
      },
    });
    await expect(runtime.stop()).rejects.toThrow('working');
    expect(runtime.stopPlan()).toEqual({ daemon: false, desktops: 0 });
  });
});

describe("A helper killed during stop()'s own recovery", () => {
  const repo = path.resolve(import.meta.dir, '../../..');
  const roots: string[] = [];
  afterEach(() => {
    for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
  });
  const isolated = (root: string) => ({
    PATH: process.env.PATH ?? '',
    HOME: root,
    USERPROFILE: root,
    CCS_HOME: path.join(root, 'ccs'),
    XDG_CONFIG_HOME: path.join(root, '.config'),
    XDG_DATA_HOME: path.join(root, '.local/share'),
    XDG_STATE_HOME: path.join(root, '.local/state'),
    XDG_CACHE_HOME: path.join(root, '.cache'),
    CODEX_HOME: path.join(root, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(root, '.claude'),
    PYTHONDONTWRITEBYTECODE: '1',
  });

  /** The next Python updater call on what a killed helper left, with no daemon reported running. */
  const nextClick = (root: string) => {
    return spawnSync(
      'python3',
      [
        '-c',
        [
          'import json, pathlib, sys, time',
          'from unittest import mock',
          "sys.path.insert(0, 'scripts/app-updates')",
          'import app_update_common as common, app_updates as updater',
          'root = pathlib.Path(sys.argv[1])',
          "(root / '.local/bin').mkdir(parents=True, exist_ok=True)",
          "(root / '.local/bin/codex').write_text('')",
          "install = common.Install('codex-cli', 'ubuntu', pathlib.Path('/fixture/codex'), '2.0.0', 'native')",
          "answer = {'appId': 'codex-cli', 'platform': 'ubuntu', 'status': 'restart_failed', 'previousVersion': '2.0.0', 'version': '2.0.0', 'manager': 'native', 'messageCode': 'restart_not_running', 'updateAttempted': True, 'restartedProcesses': 0, 'forcedStops': 0}",
          "patches = [mock.patch.object(pathlib.Path, 'home', return_value=root), mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'family', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([], [])), mock.patch.object(updater, 'cli_instances', return_value=([], [], [])), mock.patch.object(updater, 'check_terminal'), mock.patch.object(updater, 'detect_cli', return_value=install)]",
          '[patch.start() for patch in patches]',
          "with mock.patch.object(updater, 'command', return_value=json.dumps(answer)) as run:",
          '    value = updater.update_cli(install, time.monotonic() + 600)',
          "bridged = any('app_update_codex.cjs' in ' '.join(str(arg) for arg in call.args[0]) for call in run.call_args_list)",
          "print(json.dumps({'bridged': bridged, 'status': value['status'], 'marker': (root / '.ccs/app-updates/codex-cli-pending-restart.json').exists()}))",
        ].join('\n'),
        root,
      ],
      { cwd: repo, env: isolated(root), encoding: 'utf8', timeout: 60_000 }
    );
  };

  it('leaves a marker that sends the next click to the bridge, not to current', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-killed-recovery-'));
    roots.push(root);
    const child = spawnSync(
      process.execPath,
      ['run', path.join(repo, 'tests/unit/codex-auth/fixtures/killed-recovery.ts')],
      { cwd: repo, env: isolated(root), encoding: 'utf8', timeout: 60_000 }
    );
    expect(child.signal).toBe('SIGKILL');
    const marker = path.join(root, '.ccs/app-updates/codex-cli-pending-restart.json');
    expect(JSON.parse(fs.readFileSync(marker, 'utf8'))).toEqual({
      version: '2.0.0',
      desktop: false,
      daemon: true,
      desktops: 0,
    });

    // The next updater call, once the failed replacement has exited: the real Python routing.
    const next = nextClick(root);
    expect(next.status).toBe(0);
    const routed = JSON.parse(next.stdout.trim());
    expect(routed.bridged).toBe(true);
    expect(routed.status).not.toBe('current');
    expect(routed.marker).toBe(true);
  });

  it('keeps what a failed stop() owes when killed during its readiness check', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-killed-standing-'));
    roots.push(root);
    const env = isolated(root);
    const fixture = path.join(repo, 'tests/unit/codex-auth/fixtures/killed-standing-total.ts');
    const spawn = (mode: string) =>
      spawnSync(process.execPath, ['run', fixture, mode], {
        cwd: repo,
        env,
        encoding: 'utf8',
        timeout: 60_000,
      });
    expect(spawn('kill').signal).toBe('SIGKILL');
    const marker = JSON.parse(
      fs.readFileSync(
        path.join(root, '.ccs/app-updates/codex-desktop-pending-restart.json'),
        'utf8'
      )
    );
    // The daemon, and the desktop stop() closed and could not bring back; not the one it never reached.
    expect(marker.daemon).toBe(true);
    expect(marker.desktops).toBe(1);

    // The next update finds only the other desktop and the daemon running: the closed one is not back.
    const retry = spawn('retry');
    expect(retry.status).toBe(0);
    expect(JSON.parse(retry.stdout.trim().split('\n').pop() as string)).toEqual({
      status: 'restart_failed',
      messageCode: 'restart_not_running',
      desktops: 1,
    });
  });

  it('leaves a marker naming the old daemon when killed while the updater runs', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-killed-install-'));
    roots.push(root);
    const child = spawnSync(
      process.execPath,
      ['run', path.join(repo, 'tests/unit/codex-auth/fixtures/killed-install.ts')],
      { cwd: repo, env: isolated(root), encoding: 'utf8', timeout: 60_000 }
    );
    expect(child.signal).toBe('SIGKILL');
    const marker = path.join(root, '.ccs/app-updates/codex-cli-pending-restart.json');
    expect(JSON.parse(fs.readFileSync(marker, 'utf8'))).toEqual({
      version: '1.0.0',
      desktop: false,
      daemon: true,
      desktops: 0,
    });
    const next = nextClick(root);
    expect(next.status).toBe(0);
    const routed = JSON.parse(next.stdout.trim());
    expect(routed.bridged).toBe(true);
    expect(routed.status).not.toBe('current');
  });

  it('owes a daemon that arrived on the old binary while the updater ran, when killed then', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-killed-arrival-'));
    roots.push(root);
    const env = { ...isolated(root), AAC_FIXTURE_ARRIVAL: '1' };
    const fixture = path.join(repo, 'tests/unit/codex-auth/fixtures/killed-install.ts');
    const spawn = (...args: string[]) =>
      spawnSync(process.execPath, ['run', fixture, ...args], {
        cwd: repo,
        env,
        encoding: 'utf8',
        timeout: 60_000,
      });
    // No daemon ran when the update began, only a proxy; one started during the install.
    expect(spawn().signal).toBe('SIGKILL');
    const marker = path.join(root, '.ccs/app-updates/codex-cli-pending-restart.json');
    expect(JSON.parse(fs.readFileSync(marker, 'utf8'))).toMatchObject({ daemon: true });
    // The real Python routing sends the next click to the bridge.
    const routed = JSON.parse(nextClick(root).stdout.trim());
    expect(routed.bridged).toBe(true);
    // That bridge restarts the daemon still running 1.0.0 instead of calling 2.0.0 current.
    const retry = JSON.parse(spawn('retry').stdout.trim().split('\n').pop() as string);
    expect(retry.status).toBe('updated');
    expect(retry.signals).toBeGreaterThan(0);
    expect(retry.marker).toBeNull();
  });

  it('restarts a daemon that arrived while the updater ran, when nothing is killed', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-arrival-'));
    roots.push(root);
    const child = spawnSync(
      process.execPath,
      ['run', path.join(repo, 'tests/unit/codex-auth/fixtures/killed-install.ts')],
      {
        cwd: repo,
        env: { ...isolated(root), AAC_FIXTURE_ARRIVAL: '1', AAC_FIXTURE_NO_KILL: '1' },
        encoding: 'utf8',
        timeout: 60_000,
      }
    );
    const result = JSON.parse(child.stdout.trim().split('\n').pop() as string);
    expect(result).toMatchObject({ status: 'updated', marker: null });
    expect(result.signals).toBeGreaterThan(0);
  });
});

describe('Codex process launch', () => {
  const roots: string[] = [];
  const tempHome = (): string => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-launch-'));
    roots.push(root);
    return root;
  };
  afterEach(() => {
    for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
  });
  const launched = async (codexHome: string, desktop: boolean) => {
    const specs: Array<{ app: string; logFile?: string }> = [];
    await launchCodexProcess(codexHome, daemon, desktop, async (spec) => {
      specs.push(spec);
      return { route: 'direct' };
    });
    return specs[0];
  };

  it('creates the missing control folder and a private log before starting the daemon', async () => {
    const codexHome = tempHome();
    const spec = await launched(codexHome, false);
    const log = path.join(codexHome, 'app-server-control', 'app-server.log');
    expect(spec.logFile).toBe(log);
    expect(fs.statSync(log).mode & 0o777).toBe(0o600);
  });

  it('gives a desktop app no log file', async () => {
    const codexHome = tempHome();
    const spec = await launched(codexHome, true);
    expect(spec.logFile).toBeUndefined();
    expect(fs.existsSync(path.join(codexHome, 'app-server-control'))).toBe(false);
  });

  it('rotates the log past its cap at launch and keeps a smaller one', () => {
    const log = path.join(tempHome(), 'control', 'app-server.log');
    prepareAppServerLog(log);
    fs.writeFileSync(log, 'small');
    prepareAppServerLog(log);
    expect(fs.readFileSync(log, 'utf8')).toBe('small');
    expect(fs.existsSync(`${log}.1`)).toBe(false);
    fs.writeFileSync(log, Buffer.alloc(5 * 1024 * 1024 + 1));
    prepareAppServerLog(log);
    expect(fs.statSync(log).size).toBe(0);
    expect(fs.statSync(log).mode & 0o777).toBe(0o600);
    expect(fs.statSync(`${log}.1`).size).toBe(5 * 1024 * 1024 + 1);
  });

  it('reports a failed launch without its detail', async () => {
    await expect(
      launchCodexProcess(tempHome(), daemon, false, async () => {
        throw new Error('private detail');
      })
    ).rejects.toThrow('Could not launch Codex.');
  });
});

describe('Codex activation process lifecycle', () => {
  it('stops desktop and every server before the auth install, preserving display and daemon command', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    fake.events.push('install-auth');
    await runtime.start();
    expect(fake.events).toEqual([
      'lock',
      'idle',
      'SIGTERM:20',
      'SIGTERM:10',
      'retire-socket',
      'install-auth',
      'retire-socket',
      'native-unlock',
      'unlock',
      'start-daemon',
      'healthy',
      'start-desktop',
      'native-unlock',
    ]);
    expect(fake.launches[0].args).toEqual(daemon.args);
    expect(fake.launches[0].env).toEqual(daemon.env);
    const app = fake.launches[1];
    expect(app.args).toEqual([
      '/usr/lib/chatgpt/ChatGPT',
      '--user-data-dir=/fixture/.config/Codex',
    ]);
    expect(app.env.DISPLAY).toBe(':8');
    expect(app.env.XAUTHORITY).toBe('/fixture/.Xauthority');
    expect(app.env.CODEX_APP_TOOLS_PIPE_PATH).toBeUndefined();
    expect(app.env.BROWSER_USE_ACCESS_VERIFYING_IDENTITY).toBeUndefined();
  });

  it('offers its default launcher to a launch override, so an updater can stay outside the cgroup', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const offered: unknown[] = [];
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      launch: async (target, isDesktop, outside) => {
        offered.push(outside);
        return fake.deps.launch(target, isDesktop);
      },
    });
    await runtime.stop();
    await runtime.start();
    expect(offered.length).toBe(2);
    expect(offered.every((value) => typeof value === 'function')).toBe(true);
  });

  it('retires a daemon an SSH proxy respawned during the swap before starting the new one', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    fake.inject({ ...daemon, pid: 50, startTime: '50' });
    fake.events.push('install-auth');
    await runtime.start();
    const install = fake.events.indexOf('install-auth');
    expect(fake.events.indexOf('SIGTERM:50')).toBeGreaterThan(install);
    expect(fake.events.indexOf('SIGTERM:50')).toBeLessThan(fake.events.indexOf('start-daemon'));
    expect(fake.current().some((entry) => entry.pid === 50)).toBe(false);
    // The daemon only binds its socket once both startup locks are free.
    expect(fake.events.indexOf('unlock')).toBeLessThan(fake.events.indexOf('start-daemon'));
  });

  it('refuses active CLI work without signalling any writer', async () => {
    const fake = harness([
      daemon,
      processFixture(30, ['/fixture/bin/codex', 'exec', 'fake-prompt']),
    ]);
    await expect(createCodexActivationRuntime(home, fake.deps).stop()).rejects.toMatchObject({
      code: 'busy',
    });
    expect(fake.events).toEqual(['lock', 'prepare-cli', 'native-unlock', 'unlock']);
  });

  it('lets an idle daemon, proxy and desktop-hosted servers switch without treating presence as work', async () => {
    const proxy = processFixture(30, ['/fixture/bin/codex', 'app-server', 'proxy']);
    const execServer = processFixture(
      23,
      [
        '/usr/lib/chatgpt/resources/codex',
        'exec-server',
        '--remote',
        'https://fixture.invalid/api',
      ],
      { ppid: 20, env: { HOME: '/fixture' } }
    );
    const fake = harness([daemon, proxy, desktop, bundled, execServer, renderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    expect(fake.events).toEqual(['lock', 'idle', 'SIGTERM:20', 'SIGTERM:10', 'retire-socket']);
    // The proxy is not an auth writer: it survives the swap untouched.
    expect(fake.current().some((entry) => entry.pid === 30)).toBe(true);
  });

  it('refuses an orphaned exec-server with a reviewable plan instead of silently killing it', async () => {
    const orphan = processFixture(23, [
      '/usr/lib/chatgpt/resources/codex',
      'exec-server',
      '--remote',
      'https://fixture.invalid/api',
    ]);
    const fake = harness([daemon, orphan]);
    const error: unknown = await createCodexActivationRuntime(home, fake.deps)
      .stop()
      .catch((cause: unknown) => cause);
    expect(error).toBeInstanceOf(CodexActivationRuntimeError);
    expect((error as CodexActivationRuntimeError).code).toBe('busy');
    expect((error as CodexActivationRuntimeError).stopPlan).toBeDefined();
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
  });

  it('refuses actual active thread state, including waiting-for-user tasks, without stopping processes', async () => {
    const fake = harness([daemon]);
    fake.deps.assertIdle = async () => {
      throw new CodexActivationRuntimeError('Codex work is running.', 'busy');
    };
    await expect(createCodexActivationRuntime(home, fake.deps).stop()).rejects.toMatchObject({
      code: 'busy',
    });
    expect(fake.current()).toEqual([daemon]);
    expect(fake.events).toEqual(['lock', 'native-unlock', 'unlock']);
  });

  it('quiesces the partially restarted daemon before rollback and retains the original desktop launcher', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    fake.failDesktop();
    await expect(runtime.start()).rejects.toMatchObject({ code: 'restart_failed' });
    expect(fake.current().some((entry) => entry.args.includes('app-server'))).toBe(true);
    await runtime.stop();
    expect(fake.current()).toHaveLength(0);
    fake.events.push('restore-auth');
    await runtime.start();
    expect(fake.events.indexOf('native-lock')).toBeLessThan(fake.events.indexOf('restore-auth'));
    expect(fake.events.filter((event) => event === 'start-desktop')).toHaveLength(2);
    expect(fake.launches[3].args).toEqual([
      '/usr/lib/chatgpt/ChatGPT',
      '--user-data-dir=/fixture/.config/Codex',
    ]);
    expect(fake.events.lastIndexOf('unlock')).toBeLessThan(fake.events.lastIndexOf('start-daemon'));
  });

  it('does not let an unrelated home daemon suppress the shared daemon restart', async () => {
    const unrelated = processFixture(
      40,
      ['/elsewhere/codex', 'app-server', '--listen', 'unix://'],
      { env: { CODEX_HOME: '/other/.codex' } }
    );
    const fake = harness([daemon, unrelated]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    expect(fake.current()).toEqual([unrelated]);
    await runtime.start();
    expect(fake.events).toContain('start-daemon');
    expect(fake.events).not.toContain('SIGTERM:40');
  });

  it('allows SSH transport proxies to remain and treats zombies as exited', async () => {
    const proxy = processFixture(50, ['/fixture/bin/codex', 'app-server', 'proxy']);
    const zombie = { ...daemon, state: 'Z' };
    const fake = harness([proxy, zombie]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
    expect(fake.events).not.toContain('start-daemon');
  });

  it('recognizes a replaced codex executable as a writer', async () => {
    const replaced = { ...daemon, exe: `${daemon.exe} (deleted)` };
    const fake = harness([replaced]);
    await createCodexActivationRuntime(home, fake.deps).stop();
    expect(fake.events).toContain('SIGTERM:10');
  });

  it('uses SIGKILL only after graceful exit has timed out', async () => {
    const fake = harness([daemon]);
    fake.deps.signal = async (target, signal) => {
      fake.events.push(`${signal}:${target.pid}`);
      if (signal === 'SIGKILL') target.state = 'Z';
    };
    await createCodexActivationRuntime(home, fake.deps).stop();
    expect(fake.events.filter((event) => event.startsWith('SIG'))).toEqual([
      'SIGTERM:10',
      'SIGTERM:10',
      'SIGKILL:10',
    ]);
    expect(fake.deps.now()).toBeGreaterThanOrEqual(12000);
  });

  it('releases the startup lock if shutdown and original-process recovery both fail', async () => {
    const fake = harness([daemon]);
    fake.deps.signal = async () => {
      throw new CodexActivationRuntimeError('fixture stop failed');
    };
    fake.deps.verifyDaemon = async () => {
      throw new CodexActivationRuntimeError('fixture recovery failed');
    };
    await expect(createCodexActivationRuntime(home, fake.deps).stop()).rejects.toMatchObject({
      code: 'restart_failed',
    });
    expect(fake.events.at(-1)).toBe('unlock');
  });

  it('dispose releases an exhausted rollback lock without launching processes', async () => {
    const fake = harness([daemon]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.dispose?.();
    expect(fake.events.at(-1)).toBe('unlock');
    expect(fake.launches).toHaveLength(0);
  });
});

describe('explicit confirmed process interruption', () => {
  async function warning(fake: ReturnType<typeof harness>): Promise<CodexActivationStopPlan> {
    try {
      await createCodexActivationRuntime(home, fake.deps).stop();
    } catch (error) {
      expect(error).toBeInstanceOf(CodexActivationRuntimeError);
      const plan = (error as CodexActivationRuntimeError).stopPlan;
      expect(plan).toBeDefined();
      return plan!;
    }
    throw new Error('Expected a confirmation warning');
  }

  it('does not stop anything on warning/cancellation and exposes only named safe processes', async () => {
    const cli = processFixture(30, ['/fixture/bin/codex', 'exec', 'private-prompt']);
    const fake = harness([daemon, cli]);
    const plan = await warning(fake);
    expect(fake.current()).toHaveLength(2);
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
    expect(plan.processes).toEqual([
      { label: 'Shared Codex server', pid: 10, role: 'daemon' },
      { label: 'Codex CLI', pid: 30, role: 'cli' },
    ]);
    expect(JSON.stringify(plan)).not.toContain('private-prompt');
  });

  it('stops approved work, then restarts both server and a fresh idle CLI without its original prompt', async () => {
    const fake = harness([
      daemon,
      processFixture(30, ['/fixture/bin/codex', 'exec', 'private-prompt']),
    ]);
    const plan = await warning(fake);
    fake.deps.assertIdle = async () => {
      throw new Error('Confirmed work need not be idle');
    };
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop(plan);
    expect(fake.current()).toHaveLength(0);
    fake.events.push('install-auth');
    await runtime.start();
    expect(fake.events).toContain('SIGTERM:10');
    expect(fake.events).toContain('SIGTERM:30');
    expect(fake.events.indexOf('start-cli')).toBeGreaterThan(fake.events.indexOf('install-auth'));
    expect(fake.launches.at(-1)?.args).toEqual(['/fixture/bin/codex']);
    expect(fake.launches.at(-1)?.env).toEqual({ HOME: '/fixture', CODEX_HOME: home });
  });

  it.each(['pid-reuse', 'exec-change', 'new-child', 'new-writer', 'exited'] as const)(
    'rejects a %s scope change before signalling any writer',
    async (kind) => {
      const cli = processFixture(30, ['/fixture/bin/codex']);
      const fake = harness([daemon, cli]);
      const plan = await warning(fake);
      if (kind === 'pid-reuse') fake.current()[1].startTime = 'reused';
      if (kind === 'exec-change') fake.current()[1].args.push('new-command');
      if (kind === 'new-child') fake.inject(processFixture(31, ['/bin/worker'], { ppid: 30 }));
      if (kind === 'new-writer') fake.inject(processFixture(32, ['/fixture/bin/codex']));
      if (kind === 'exited') fake.current()[1].state = 'Z';
      await expect(createCodexActivationRuntime(home, fake.deps).stop(plan)).rejects.toMatchObject({
        code: 'confirmation_stale',
      });
      expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
    }
  );

  it('rejects a process spawned during terminal preflight rather than enlarging the stop set', async () => {
    const fake = harness([daemon, processFixture(30, ['/fixture/bin/codex'])]);
    const plan = await warning(fake);
    fake.deps.prepareCli = async () => {
      fake.inject(processFixture(32, ['/fixture/bin/codex']));
    };
    await expect(createCodexActivationRuntime(home, fake.deps).stop(plan)).rejects.toMatchObject({
      code: 'confirmation_stale',
    });
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
  });

  it('offers an active daemon by name without signalling it until explicitly confirmed', async () => {
    const fake = harness([daemon]);
    fake.deps.assertIdle = async () => {
      throw new CodexActivationRuntimeError('active task', 'busy');
    };
    const plan = await warning(fake);
    expect(plan.processes).toEqual([{ label: 'Shared Codex server', pid: 10, role: 'daemon' }]);
    await createCodexActivationRuntime(home, fake.deps).stop(plan);
    expect(fake.events).toContain('SIGTERM:10');
  });

  it('handles the native browser stdio regression by restarting its daemon owner, never detaching old stdio arguments', async () => {
    const node = processFixture(35, ['/usr/lib/chatgpt/resources/cua_node/bin/node_repl'], {
      ppid: 10,
    });
    const helper = processFixture(
      36,
      ['/usr/lib/chatgpt/resources/codex', 'app-server', '--listen', 'stdio://'],
      {
        ppid: 35,
      }
    );
    const fake = harness([daemon, node, helper]);
    fake.deps.assertIdle = async () => {
      throw new CodexActivationRuntimeError('active task', 'busy');
    };
    const plan = await warning(fake);
    expect(plan.processes).toContainEqual({
      label: 'Codex browser automation helper',
      pid: 36,
      role: 'automation',
    });
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop(plan);
    await runtime.start();
    expect(fake.launches).toHaveLength(1);
    expect(fake.launches[0].args).toEqual(daemon.args);
    expect(fake.current().some((entry) => entry.pid === 36)).toBe(false);
    expect(fake.events).not.toContain('start-cli');
  });

  it('allows the known idle native browser helper during normal activation after the daemon confirms all work is idle', async () => {
    const node = processFixture(35, ['/usr/lib/chatgpt/resources/cua_node/bin/node_repl'], {
      ppid: 10,
    });
    const helper = processFixture(
      36,
      ['/usr/lib/chatgpt/resources/codex', 'app-server', '--listen', 'stdio://'],
      { ppid: 35 }
    );
    const fake = harness([daemon, node, helper]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.events.indexOf('idle')).toBeLessThan(fake.events.indexOf('SIGTERM:10'));
    expect(fake.launches).toHaveLength(1);
    expect(fake.current().some((entry) => entry.pid === 36)).toBe(false);
  });

  it('reopens a CLI nested under the approved daemon rather than silently losing the child program', async () => {
    const nested = processFixture(30, ['/fixture/bin/codex', 'exec', 'private-prompt'], {
      ppid: 10,
    });
    const fake = harness([daemon, nested]);
    const plan = await warning(fake);
    expect(plan.processes).toContainEqual({ pid: 30, label: 'Codex CLI', role: 'cli' });
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop(plan);
    await runtime.start();
    expect(fake.events).toContain('start-cli');
    expect(fake.launches.at(-1)?.args).toEqual(['/fixture/bin/codex']);
  });

  it('refuses an unowned stdio server because its original pipes cannot be safely reconstructed', async () => {
    const fake = harness([
      processFixture(36, ['/fixture/bin/codex', 'app-server', '--listen', 'stdio://']),
    ]);
    await expect(createCodexActivationRuntime(home, fake.deps).stop()).rejects.toMatchObject({
      code: 'busy',
      stopPlan: undefined,
    });
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
  });

  it('fails terminal preflight without stopping a process or issuing an unusable offer', async () => {
    const fake = harness([processFixture(30, ['/fixture/bin/codex'])]);
    fake.deps.prepareCli = async () => {
      throw new CodexActivationRuntimeError('No terminal');
    };
    await expect(createCodexActivationRuntime(home, fake.deps).stop()).rejects.toMatchObject({
      code: 'restart_failed',
      stopPlan: undefined,
    });
    expect(fake.events.some((event) => event.startsWith('SIG'))).toBe(false);
  });

  it('retains a partially relaunched CLI start identity so rollback stops only that instance and can reopen it', async () => {
    const fake = harness([processFixture(30, ['/fixture/bin/codex', 'exec', 'private-prompt'])]);
    const plan = await warning(fake);
    const launch = fake.deps.launchCli;
    let attempt = 0;
    fake.deps.launchCli = async (target) => {
      await launch(target);
      if (++attempt === 1) throw new CodexActivationRuntimeError('Terminal readiness failed');
    };
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop(plan);
    await expect(runtime.start()).rejects.toMatchObject({ code: 'restart_failed' });
    const partial = fake.current()[0];
    expect(partial).toBeDefined();
    await runtime.stop();
    expect(fake.events).toContain(`SIGTERM:${partial.pid}`);
    await runtime.start();
    expect(fake.current()).toHaveLength(1);
    expect(fake.current()[0].args).toEqual(['/fixture/bin/codex']);
    expect(fake.current()[0].pid).not.toBe(partial.pid);
  });
});

const temporaryDirectories: string[] = [];
afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

/**
 * A small reference for base::CommandLine's switch parsing on Linux, written apart from the
 * runtime's port: each argument after argv[0] loses ASCII whitespace (0x09-0x0D and space) at both
 * ends, as TrimWhitespaceASCII does; everything before the first `--` is read; a switch is `--` or
 * `-` and at least one more character; its name runs to the first `=`, its value is the rest; the
 * last one wins.
 */
function referenceSwitches(argv: string[]): [string, string][] {
  const ascii = (code: number): boolean => code === 32 || (code >= 9 && code <= 13);
  const trim = (arg: string): string => {
    let start = 0;
    let end = arg.length;
    while (start < end && ascii(arg.charCodeAt(start))) start++;
    while (end > start && ascii(arg.charCodeAt(end - 1))) end--;
    return arg.slice(start, end);
  };
  const rest = argv.slice(1).map(trim);
  const end = rest.indexOf('--');
  const switches: Record<string, string> = Object.create(null);
  for (const arg of end < 0 ? rest : rest.slice(0, end)) {
    const match = /^(?:--|-)(?!$)([^=]*)(?:=([\s\S]*))?$/.exec(arg);
    if (match) switches[match[1]] = match[2] ?? '';
  }
  return Object.entries(switches).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
}

/** A small seeded generator, so a failing case can be replayed. */
function seeded(seed: number): () => number {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let value = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    value = (value + Math.imul(value ^ (value >>> 7), 61 | value)) ^ value;
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

describe('an activation rollback after start()', () => {
  // The caller's rollback: stop() again, which closes the desktops start() reopened, then start().
  const mainAt = (pid: number, folder: string) =>
    processFixture(pid, ['/usr/lib/chatgpt/ChatGPT', `--user-data-dir=/fixture/${folder}`], {
      env: { HOME: '/fixture', DISPLAY: ':8' },
      exe: '/usr/lib/chatgpt/ChatGPT',
    });
  const serverOf = (pid: number, ppid: number) => ({
    ...bundled,
    pid,
    ppid,
    startTime: String(pid),
  });

  it.each([
    { name: 'after a step that failed past a good start()', desktops: 1, failSecond: false },
    { name: 'after a start() that reopened one of two', desktops: 2, failSecond: true },
  ])('reopens every desktop it closed again, $name', async ({ desktops, failSecond }) => {
    const initial = Array.from({ length: desktops }, (_, index) => [
      mainAt(40 + index * 10, `profile-${index}`),
      serverOf(41 + index * 10, 40 + index * 10),
    ]).flat();
    const fake = harness(initial);
    let launches = 0;
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      launch: async (target, isDesktopLaunch) => {
        if (failSecond && ++launches === 2) {
          throw new CodexActivationRuntimeError('fixture second desktop launch failed');
        }
        return fake.deps.launch(target, isDesktopLaunch);
      },
    });
    await runtime.stop();
    if (failSecond) await expect(runtime.start()).rejects.toThrow();
    else await runtime.start();

    await runtime.stop();
    // Each desktop is closed again, including one start() reopened, so each is owed again.
    expect(fake.current().filter(isDesktop)).toHaveLength(0);
    expect((await runtime.retiredPlan()).desktops).toBe(desktops);
    await runtime.start();
    expect(fake.current().filter(isDesktop)).toHaveLength(desktops);
    expect((await runtime.retiredPlan()).desktops).toBe(0);
  });

  it('reopens a desktop whose reopened process exited before the rollback', async () => {
    const fake = harness([mainAt(40, 'profile-0'), serverOf(41, 40)]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    // The desktop start() reopened exits on its own, so the rollback's stop() has nothing to close.
    for (const entry of fake.current().filter(isDesktop)) await fake.deps.signal(entry, 'SIGKILL');
    await runtime.stop();
    await runtime.start();
    expect(fake.current().filter(isDesktop)).toHaveLength(1);
  });
});

describe('desktop main process detection', () => {
  it('reads a descendant bare user-data-dir as the default folder, never the next word', async () => {
    const splitRenderer = {
      ...renderer,
      args: [renderer.exe, '--type=renderer', '--user-data-dir', '/fixture/custom-profile'],
    };
    const fake = harness([daemon, desktop, bundled, splitRenderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.launches[1].args).toEqual([
      '/usr/lib/chatgpt/ChatGPT',
      `--user-data-dir=${path.join(os.homedir(), '.config', 'Codex')}`,
    ]);
  });

  it.each([
    { parentArgs: ['--user-data-dir=/fixture/parent-profile'] },
    { parentArgs: ['--user-data-dir', '/fixture/parent-profile'] },
  ])('preserves an explicit parent user-data-dir override %j', async ({ parentArgs }) => {
    const parent = { ...desktop, args: [desktop.exe, ...parentArgs] };
    const fake = harness([daemon, parent, bundled, renderer]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.launches[1].args).toEqual(parent.args);
  });

  it('does not pair a missing descendant flag value with another child command', async () => {
    const missing = { ...renderer, args: [renderer.exe, '--type=renderer', '--user-data-dir'] };
    const sibling = { ...renderer, pid: 23, args: [renderer.exe, '--type=utility'] };
    const fake = harness([daemon, desktop, bundled, missing, sibling]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.launches[1].args).toEqual([
      '/usr/lib/chatgpt/ChatGPT',
      `--user-data-dir=${path.join(os.homedir(), '.config', 'Codex')}`,
    ]);
  });

  describe('switch parsing against a reference for base::CommandLine', () => {
    const exe = '/usr/lib/chatgpt/ChatGPT';
    const names = [
      'user-data-dir',
      'type',
      'class',
      'user-data-dirx',
      'user-data',
      'User-Data-Dir',
      '',
    ];
    const values = ['/x/a', '/x/a b', '/x/a\tb', '', '/x/=b', '/x/a --b', '/x/a -c', 'rel/x'];
    const prefixes = ['--', '-', '---', ''];
    /** One argv element: switches in every form, spaces, tabs, `=`, empty values and terminators. */
    const element = (random: () => number): string => {
      const pick = <T>(items: T[]): T => items[Math.floor(random() * items.length)];
      switch (Math.floor(random() * 9)) {
        case 0:
        case 1:
        case 2:
          return `${pick(prefixes)}${pick(names)}=${pick(values)}`;
        case 3:
          return `${pick(prefixes)}${pick(names)}`;
        case 4:
          return `${pick(prefixes)}${pick(names)}${pick([' ', '\t'])}${pick(values)}`;
        case 5:
          return pick(['--', '-', '', '-- c', '--\t']);
        case 6:
          return pick(values);
        default:
          return `--user-data-dir=${pick(values)}`;
      }
    };
    // Whitespace around an element, including a no-break space, which ASCII trimming keeps.
    const pads = ['', '', '', ' ', '\t', '\n', '\v', '\f', '\r', '  ', '\u00a0'];
    const padded = (random: () => number): string =>
      `${pads[Math.floor(random() * pads.length)]}${element(random)}${pads[Math.floor(random() * pads.length)]}`;
    const generate = (random: () => number): string[] =>
      Array.from({ length: Math.floor(random() * 8) }, () => padded(random));
    const sorted = (switches: Map<string, string>): [string, string][] =>
      [...switches.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));

    it('reads every generated argv exactly as the reference does', () => {
      const random = seeded(20261107);
      for (let run = 0; run < 5000; run++) {
        const argv = [exe, ...generate(random)];
        expect({ argv, switches: sorted(runtimeModule.chromiumSwitches(argv)) }).toEqual({
          argv,
          switches: referenceSwitches(argv),
        });
      }
    });

    it('relaunches a rewritten cmdline as the argv it was joined from, when that argv had no spaces', async () => {
      const random = seeded(11);
      const pick = <T>(items: T[]): T => items[Math.floor(random() * items.length)];
      let checked = 0;
      for (let run = 0; run < 3000 && checked < 400; run++) {
        // Elements without whitespace; a plain word only after a bare switch or the terminator,
        // where the joined string still says where it belongs.
        const argv = [exe];
        let ended = false;
        for (let index = Math.floor(random() * 7); index > 0; index--) {
          if (ended) {
            argv.push(pick(['/x/p', '--user-data-dir=/x/c', '--type=renderer', '--']));
            continue;
          }
          const kind = Math.floor(random() * 4);
          const name = pick(['user-data-dir', 'type', 'class']);
          if (kind === 0) argv.push(`--${name}=${pick(['/x/a', '', '/x/=b', 'rel/x'])}`);
          else if (kind === 1) {
            argv.push(`--${name}`);
            if (random() < 0.5) argv.push('/x/p');
          } else if (kind === 2) {
            argv.push('--');
            ended = true;
          } else argv.push(`--user-data-dir=${pick(['/x/a', '/x/b'])}`);
        }
        const joined = argv.join(' ');
        if (!/\s--/.test(joined)) continue;
        checked++;
        const switches = new Map(referenceSwitches(argv));
        const main = processFixture(20, [joined], { cwd: '/fixture', env: { DISPLAY: ':8' }, exe });
        expect({ joined, desktop: isDesktop(main) }).toEqual({
          joined,
          desktop: !switches.get('type'),
        });
        if (switches.get('type')) continue;
        // The relaunch carries the switches the argv had, plus the default folder when it named none.
        const fake = harness([daemon, main, bundled]);
        const runtime = createCodexActivationRuntime(home, fake.deps);
        await runtime.stop();
        await runtime.start();
        const [relaunched] = fake.launches.filter(isDesktop);
        if (!switches.has('user-data-dir')) {
          switches.set('user-data-dir', path.join(os.homedir(), '.config', 'Codex'));
        }
        expect({
          joined,
          switches: sorted(runtimeModule.chromiumSwitches(relaunched.args)),
        }).toEqual({
          joined,
          switches: [...switches.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
        });
      }
      expect(checked).toBe(400);
    });
  });

  it('reads --type as Chromium does: none after the terminator', () => {
    const exe = '/usr/lib/chatgpt/ChatGPT';
    const main = (args: string[]) => processFixture(20, args, { env: { DISPLAY: ':8' }, exe });
    // A `--type=` after the terminator is a plain argument: this is still the main process.
    expect(isDesktop(main([exe, '--', '--type=renderer']))).toBe(true);
    expect(isDesktop(main([exe, '-- ', '--type=renderer']))).toBe(true);
    expect(isDesktop(main([exe, '--type=renderer']))).toBe(false);
    expect(isDesktop(main([exe, '--type=']))).toBe(true);
  });

  it('relaunches with the data folder before the switch terminator', async () => {
    const plain = processFixture(20, ['/usr/lib/chatgpt/ChatGPT', '--', '/fixture/file'], {
      env: {},
      exe: '/usr/lib/chatgpt/ChatGPT',
    });
    const fake = harness([daemon, plain, bundled]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.launches.filter(isDesktop).map((entry) => entry.args)).toEqual([
      [
        '/usr/lib/chatgpt/ChatGPT',
        `--user-data-dir=${path.join(os.homedir(), '.config', 'Codex')}`,
        '--',
        '/fixture/file',
      ],
    ]);
    // A rewritten cmdline's terminator and the words after it relaunch as their own arguments.
    const rewritten = processFixture(
      20,
      ['/usr/lib/chatgpt/ChatGPT --user-data-dir=/x/b -- /fixture/file'],
      { env: {}, exe: '/usr/lib/chatgpt/ChatGPT' }
    );
    const again = harness([daemon, rewritten, bundled]);
    const second = createCodexActivationRuntime(home, again.deps);
    await second.stop();
    await second.start();
    expect(again.launches.filter(isDesktop).map((entry) => entry.args)).toEqual([
      ['/usr/lib/chatgpt/ChatGPT', '--user-data-dir=/x/b', '--', '/fixture/file'],
    ]);
  });

  it('never counts Electron in Node mode as a desktop, yet lets it make its desktop ready', async () => {
    const exe = '/usr/lib/chatgpt/ChatGPT';
    const helper = (env: Record<string, string>) =>
      processFixture(22, [exe, '/fixture/helper.js'], {
        ppid: 20,
        exe,
        env: { DISPLAY: ':8', ...env },
      });
    expect(isDesktop(helper({ ELECTRON_RUN_AS_NODE: '1' }))).toBe(false);
    expect(isDesktop(helper({ ELECTRON_RUN_AS_NODE: '0' }))).toBe(false);
    // Electron runs as Node only when the variable is set and not empty.
    expect(isDesktop(helper({ ELECTRON_RUN_AS_NODE: '' }))).toBe(true);
    // A relaunched desktop whose app server runs under a Node-mode helper is back.
    const fake = harness([daemon, desktop, bundled]);
    const runtime = createCodexActivationRuntime(home, {
      ...fake.deps,
      launch: async (target, isDesktopLaunch) => {
        const own = await fake.deps.launch(target, isDesktopLaunch);
        if (!isDesktopLaunch || !own) return own;
        const pid = (own as { pid: number }).pid;
        for (const entry of fake.current().filter((item) => item.ppid === pid)) {
          await fake.deps.signal(entry, 'SIGKILL');
        }
        fake.inject({ ...helper({ ELECTRON_RUN_AS_NODE: '1' }), pid: 600, ppid: pid });
        fake.inject({ ...bundled, pid: 601, ppid: 600, startTime: '601' });
        return own;
      },
    });
    await runtime.stop();
    await runtime.start();
    expect(fake.current().filter(isDesktop)).toHaveLength(1);
  });

  it('treats only the Electron main process as the desktop, including rewritten zygote cmdlines', () => {
    const zygote = processFixture(
      23,
      ['/usr/lib/chatgpt/ChatGPT --type=zygote --no-zygote-sandbox --crashpad-handler-pid=19'],
      { ppid: 20, exe: '/usr/lib/chatgpt/ChatGPT' }
    );
    expect(isDesktop(desktop)).toBe(true);
    expect(isDesktop(renderer)).toBe(false);
    expect(isDesktop(zygote)).toBe(false);
    expect(isDesktop(bundled)).toBe(false);
  });

  it.each([
    {
      name: 'its own unit',
      token: { unit: 'aac-launch-codex-desktop-1-ab' },
      unit: 'aac-launch-codex-desktop-1-ab.service',
      ready: true,
    },
    {
      name: 'another unit',
      token: { unit: 'aac-launch-codex-desktop-1-ab' },
      unit: 'aac-launch-codex-desktop-2-cd.service',
      ready: false,
    },
    { name: 'its own pid', token: 'pid', unit: undefined, ready: true },
    { name: 'a process under its pid', token: 'parent', unit: undefined, ready: true },
    // The pid came back, but a different process has it now: never the one AAC started.
    { name: 'its pid reused by another process', token: 'reused', unit: undefined, ready: false },
    { name: 'a pid with no start time', token: 'bare-pid', unit: undefined, ready: false },
    { name: 'no identity', token: undefined, unit: undefined, ready: false },
  ])(
    'counts a relaunched desktop back only by what its launch started: $name',
    async ({ token, unit, ready }) => {
      const fake = harness([daemon, desktop, bundled, renderer]);
      const runtime = createCodexActivationRuntime(home, {
        ...fake.deps,
        launch: async (target, isDesktopLaunch) => {
          const own = await fake.deps.launch(target, isDesktopLaunch);
          if (!isDesktopLaunch) return own;
          const started = fake.current().find((entry) => isDesktop(entry) && entry.pid >= 100);
          if (started && unit) Object.assign(started, { unit });
          if (token === 'parent' && started) {
            // The launch spawned a wrapper, and the desktop main runs under it.
            fake.inject(processFixture(700, ['/bin/sh', '-c', 'exec app'], { exe: '/bin/sh' }));
            started.ppid = 700;
            return { pid: 700, startTime: '700' };
          }
          const pid = (own as { pid: number }).pid;
          if (token === 'reused') return { pid, startTime: 'an-earlier-process' };
          if (token === 'bare-pid') return { pid };
          return token === 'pid' ? own : token;
        },
      });
      await runtime.stop();
      if (ready) await runtime.start();
      else await expect(runtime.start()).rejects.toThrow();
      expect((await runtime.retiredPlan()).desktops).toBe(ready ? 0 : 1);
    }
  );

  it('reads the transient unit a process runs in from its cgroup', () => {
    const service =
      '0::/user.slice/user-1000.slice/user@1000.service/app.slice/aac-launch-codex-desktop-1-ab.service\n';
    expect(runtimeModule.systemdUnitOf(service)).toBe('aac-launch-codex-desktop-1-ab.service');
    expect(
      runtimeModule.systemdUnitOf('0::/user.slice/user-1000.slice/session-3.scope\n')
    ).toBeUndefined();
    expect(runtimeModule.systemdUnitOf('')).toBeUndefined();
  });

  it('relaunches a desktop whose own cmdline Electron rewrote, keeping its user data dir', async () => {
    const rewritten = processFixture(
      20,
      ['/usr/lib/chatgpt/ChatGPT --user-data-dir=/fixture/.config/Codex'],
      { env: {}, exe: '/usr/lib/chatgpt/ChatGPT' }
    );
    const zygote = processFixture(
      22,
      ['/usr/lib/chatgpt/ChatGPT --type=zygote --user-data-dir=/fixture/.config/Codex'],
      { ppid: 20, exe: '/usr/lib/chatgpt/ChatGPT' }
    );
    expect(isDesktop(rewritten)).toBe(true);
    const fake = harness([daemon, rewritten, bundled, zygote]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    expect(fake.launches[1].args).toEqual([
      '/usr/lib/chatgpt/ChatGPT',
      '--user-data-dir=/fixture/.config/Codex',
    ]);
  });

  it('relaunches a rewritten flag and the word after it as two arguments', async () => {
    const rewritten = processFixture(
      20,
      [
        '/usr/lib/chatgpt/ChatGPT --user-data-dir /fixture/Work One --class Codex --enable-features=X',
      ],
      { env: {}, exe: '/usr/lib/chatgpt/ChatGPT' }
    );
    const fake = harness([daemon, rewritten, bundled]);
    const runtime = createCodexActivationRuntime(home, fake.deps);
    await runtime.stop();
    await runtime.start();
    // As typed, so Chromium reads them as it did: a bare switch, which is the default folder.
    expect(fake.launches.filter(isDesktop).map((entry) => entry.args)).toEqual([
      [
        '/usr/lib/chatgpt/ChatGPT',
        '--user-data-dir',
        '/fixture/Work One',
        '--class',
        'Codex',
        '--enable-features=X',
      ],
    ]);
  });
});

describe('desktop rollout lifecycle guard', () => {
  function rollout(events: string[]): string {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-runtime-rollout-'));
    temporaryDirectories.push(directory);
    const file = path.join(directory, 'fake-rollout.jsonl');
    fs.writeFileSync(
      file,
      events
        .map((type) =>
          JSON.stringify({ type: 'event_msg', payload: { type, turn_id: 'fixture-turn' } })
        )
        .join('\n') + '\n'
    );
    return file;
  }
  it('blocks a newer task start even after an older completion', () => {
    expect(readRolloutLifecycle(rollout(['task_started', 'task_complete', 'task_started']))).toBe(
      'busy'
    );
  });
  it('recognizes completion and real turn_aborted terminal events', () => {
    expect(readRolloutLifecycle(rollout(['task_started', 'task_complete']))).toBe('idle');
    expect(readRolloutLifecycle(rollout(['task_started', 'turn_aborted']))).toBe('idle');
  });
  it('does not infer idle from a file with unknown events', () => {
    expect(readRolloutLifecycle(rollout(['token_count']))).toBe('unknown');
  });
  it('finds lifecycle events across reverse-read chunk boundaries without persisting task text', () => {
    const file = rollout(['task_started']);
    fs.appendFileSync(
      file,
      JSON.stringify({ type: 'response_item', payload: { text: 'fixture'.repeat(20000) } }) + '\n'
    );
    expect(readRolloutLifecycle(file)).toBe('busy');
  });
});
