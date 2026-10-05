import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  isDesktop,
  CodexActivationRuntimeError,
  CodexActivationRuntimeDependencies,
  CodexProcessSnapshot,
  createCodexActivationRuntime,
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
