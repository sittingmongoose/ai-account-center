import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  CodexActivationRuntimeError,
  CodexActivationRuntimeDependencies,
  CodexProcessSnapshot,
  createCodexActivationRuntime,
  readRolloutLifecycle,
} from '../../../src/codex-auth/codex-activation-runtime';

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
      'native-unlock',
      'start-daemon',
      'healthy',
      'start-desktop',
      'native-unlock',
      'unlock',
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

  it('refuses active CLI work without signalling any writer', async () => {
    const fake = harness([
      daemon,
      processFixture(30, ['/fixture/bin/codex', 'exec', 'fake-prompt']),
    ]);
    await expect(createCodexActivationRuntime(home, fake.deps).stop()).rejects.toMatchObject({
      code: 'busy',
    });
    expect(fake.events).toEqual(['lock', 'native-unlock', 'unlock']);
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
    expect(fake.events.at(-1)).toBe('unlock');
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

const temporaryDirectories: string[] = [];
afterEach(() => {
  for (const directory of temporaryDirectories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
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
