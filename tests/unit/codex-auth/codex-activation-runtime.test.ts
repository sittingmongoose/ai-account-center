import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { spawnSync } from 'child_process';
import {
  isDesktop,
  CodexActivationRuntimeError,
  CodexActivationRuntimeDependencies,
  CodexProcessSnapshot,
  codexDesktopInstance,
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
      desktops: [codexDesktopInstance('/fixture/.config/Codex')],
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
      await fake.deps.launch(target, isDesktop);
      const servers = fake.current().filter((entry) => entry.exe.endsWith('/resources/codex'));
      for (const server of servers) await fake.deps.signal(server, 'SIGKILL');
    });
    const first = await run({});
    expect(first.status).toBe('restart_failed');
    expect(marker()).toEqual({
      version: '2.0.0',
      desktop: true,
      daemon: false,
      desktops: [codexDesktopInstance('')],
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
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: [] });
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
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: [] });
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
      await fake.deps.launch(target, isDesktop);
    });
    const first = await run({});
    expect(first.status).toBe('restart_failed');
    const a = codexDesktopInstance('/private/data-a');
    const b = codexDesktopInstance('/private/data-b');
    expect(marker()).toEqual({ version: '2.0.0', desktop: true, daemon: false, desktops: [a, b] });
    expect(JSON.stringify(marker())).not.toContain('/private');

    const second = await run({ restartOnly: true });
    expect(second.status).toBe('restart_failed');
    expect(second.messageCode).toBe('restart_not_running');
    expect(marker()).toBeNull();
  });

  it('is updated once every desktop it closed is back', async () => {
    const fake = harness([
      dataDesktop(40, '/private/data-a'),
      childOf(41, 40),
      dataDesktop(50, '/private/data-b'),
      childOf(51, 50),
    ]);
    let failB = true;
    const { run, marker } = click(fake, async (target, isDesktop) => {
      if (failB && target.args.join(' ').includes('/private/data-b')) {
        throw new CodexActivationRuntimeError('fixture launch failed');
      }
      await fake.deps.launch(target, isDesktop);
    });
    await run({});
    // The user reopens the missing one by hand before the next click.
    failB = false;
    fake.inject(dataDesktop(60, '/private/data-b'));
    fake.inject(childOf(61, 60));
    const second = await run({ restartOnly: true });
    expect(second.status).toBe('updated');
    expect(marker()).toBeNull();
  });

  const withoutServer = (fake: ReturnType<typeof harness>) =>
    (async (target, isDesktop) => {
      await fake.deps.launch(target, isDesktop);
      if (!isDesktop) return;
      for (const server of fake.current().filter((entry) => entry.exe.endsWith('/resources/codex')))
        await fake.deps.signal(server, 'SIGKILL');
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
            runtime: () =>
              createCodexActivationRuntime(home, {
                ...fake.deps,
                signal,
                launch: (target, isDesktop) =>
                  bridge.relaunch(target, isDesktop, launch, { inside: () => true }),
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
      desktops: [codexDesktopInstance('/fixture/.config/Codex')],
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
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: [] });
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
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: [] });
  });

  it('does not owe a proxy-only desktop a failed stop() never retired', async () => {
    const fake = harness([desktop, proxyChild]);
    const refuse = async () => {
      throw new CodexActivationRuntimeError('fixture failed');
    };
    const { run, marker } = interruptedClick(fake, refuse, refuse);
    await run();
    // Still running, and not ready: only the fact it was never retired keeps it out of the marker.
    expect(fake.current().some(isDesktop)).toBe(true);
    expect((await createCodexActivationRuntime(home, fake.deps).restartState()).desktops).toEqual(
      []
    );
    expect(marker()).toEqual({ version: '2.0.0' });
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
      desktops: [codexDesktopInstance('/fixture/.config/Codex')],
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
    expect(marker()).toEqual({ version: '2.0.0', desktop: false, daemon: true, desktops: [] });
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
            marked: () => ({ version: '2.0.0', desktop: false, daemon: true, desktops: [] }),
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

  it('judges the owed desktop after the readiness check, not before it', async () => {
    const fake = harness([daemon, desktop, bundled, renderer]);
    let checks = 0;
    const { deps, marker } = click(fake);
    const owed = {
      version: '2.0.0',
      desktop: true,
      daemon: true,
      desktops: [codexDesktopInstance('/fixture/.config/Codex')],
    };
    const result = {
      ...(await bridge.execute({
        ...deps,
        operation: 'cli',
        update: async () => {},
        readVersion: async () => '2.0.0',
        pending: () => true,
        marked: () => owed,
        runtime: () =>
          createCodexActivationRuntime(home, {
            ...fake.deps,
            launch: (target, isDesktop) =>
              bridge.relaunch(target, isDesktop, fake.deps.launch, { inside: () => true }),
            verifyDaemon: async () => {
              // The retry's own check comes after start()'s: the desktop exits while it waits.
              if (++checks === 2) {
                for (const entry of fake
                  .current()
                  .filter((item) => !item.args.includes('--listen')))
                  await fake.deps.signal(entry, 'SIGKILL');
              }
            },
          }),
      })),
    };
    expect(checks).toBe(2);
    expect(fake.current().some(isDesktop)).toBe(false);
    expect(result.status).toBe('restart_failed');
    expect(result.messageCode).toBe('restart_not_running');
    expect(marker()).toBeNull();
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
      await fake.deps.launch(target, isDesktop);
      if (!isDesktop) return;
      // The app starts, its app server does not: what start() and the retry both wait for.
      const servers = fake.current().filter((entry) => entry.exe.endsWith('/resources/codex'));
      for (const server of servers) await fake.deps.signal(server, 'SIGKILL');
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
  const server = (ppid: number, env: Record<string, string> = { CODEX_HOME: home }) =>
    processFixture(11, ['/usr/lib/chatgpt/resources/codex', 'app-server'], { ppid, env });
  const main = processFixture(10, ['/usr/lib/chatgpt/ChatGPT', '--user-data-dir=/data/a']);
  const a = codexDesktopInstance('/data/a');
  const helper = processFixture(12, ['/usr/lib/chatgpt/ChatGPT', '--type=utility'], { ppid: 10 });

  it('counts a desktop as ready only with a live app server on this home under it', async () => {
    expect((await state([main, server(10)])).desktops).toEqual([a]);
    expect((await state([main, helper, server(12)])).desktops).toEqual([a]);
    expect((await state([main])).desktops).toEqual([]);
    expect((await state([main, server(99)])).desktops).toEqual([]);
    expect((await state([main, server(10, { CODEX_HOME: '/t3/.codex' })])).desktops).toEqual([]);
    const proxy = processFixture(13, ['/usr/lib/chatgpt/resources/codex', 'app-server', 'proxy'], {
      ppid: 10,
    });
    expect((await state([main, proxy])).desktops).toEqual([]);
  });

  it('does not take a renderer for the main process', async () => {
    const renderer = processFixture(10, ['/usr/lib/chatgpt/ChatGPT --type=zygote']);
    const seen = await state([renderer, server(10)]);
    expect(seen.desktops).toEqual([]);
  });

  it('names the default data folder the same way with or without the flag', () => {
    const explicit = path.join(os.homedir(), '.config', 'Codex');
    expect(codexDesktopInstance('')).toBe(codexDesktopInstance(explicit));
    expect(codexDesktopInstance('/data/a')).not.toBe(codexDesktopInstance('/data/b'));
    expect(codexDesktopInstance('/data/a')).not.toContain('data');
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
    expect((await state([main, server(10)])).daemon).toBe(false);
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
    expect(runtime.stopPlan()).toEqual({ daemon: false, desktops: [] });
    await runtime.stop();
    expect(runtime.stopPlan()).toEqual({
      daemon: true,
      desktops: [codexDesktopInstance('/fixture/.config/Codex')],
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
    expect(await runtime.retiredPlan()).toEqual({ daemon: false, desktops: [] });
    await expect(runtime.stop()).rejects.toThrow();
    const retired = await runtime.retiredPlan();
    expect(retired.daemon).toBe(false);
    // Recovery relaunched the desktop under a new pid; the planned one is still gone.
    expect(retired.desktops).toEqual([codexDesktopInstance('/fixture/.config/Codex')]);
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
    expect(await runtime.verifyRunningDaemon()).toBe(true);
    expect(checked).toEqual([daemon.pid]);
    const none = createCodexActivationRuntime(home, harness([]).deps);
    expect(await none.verifyRunningDaemon()).toBe(false);
    const failing = createCodexActivationRuntime(home, {
      ...fake.deps,
      verifyDaemon: async () => {
        throw new CodexActivationRuntimeError('socket not bound');
      },
    });
    expect(await failing.verifyRunningDaemon()).toBe(false);
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
    expect(plans).toEqual([
      { daemon: true, desktops: [codexDesktopInstance('/fixture/.config/Codex')] },
    ]);
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
    expect(runtime.stopPlan()).toEqual({ daemon: false, desktops: [] });
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
      desktops: [],
    });

    // The next updater call, once the failed replacement has exited: the real Python routing.
    const next = spawnSync(
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
    expect(next.status).toBe(0);
    const routed = JSON.parse(next.stdout.trim());
    expect(routed.bridged).toBe(true);
    expect(routed.status).not.toBe('current');
    expect(routed.marker).toBe(true);
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
        await fake.deps.launch(target, isDesktop);
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
describe('desktop main process detection', () => {
  it('recovers a split descendant user-data-dir without falling back to another profile', async () => {
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
      '--user-data-dir=/fixture/custom-profile',
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
