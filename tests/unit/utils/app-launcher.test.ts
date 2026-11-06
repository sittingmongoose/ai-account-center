import { describe, expect, it } from 'bun:test';
import { execFileSync, type ChildProcess } from 'child_process';
import {
  buildSystemdRunArgs,
  launchApp,
  sanitizeAppName,
  systemdRunEnvironment,
  transientUnitName,
} from '../../../src/utils/app-launcher';

function fakeChild(outcome: { code?: number | null; spawnError?: Error }) {
  const handlers: Record<string, Array<(arg?: unknown) => void>> = {};
  const child = {
    once: (event: string, cb: (arg?: unknown) => void) => {
      (handlers[event] ??= []).push(cb);
    },
    unref: () => {},
    __emit: (event: string, arg?: unknown) => {
      for (const cb of handlers[event] ?? []) cb(arg);
    },
  };
  queueMicrotask(() => {
    if (outcome.spawnError) child.__emit('error', outcome.spawnError);
    else if (outcome.code !== undefined) child.__emit('exit', outcome.code);
    else child.__emit('spawn');
  });
  return child as unknown as ChildProcess;
}

describe('app-launcher', () => {
  it('builds a transient user-service argv, never a scope', () => {
    const argv = buildSystemdRunArgs(
      {
        app: 'codex-app-server',
        exe: '/usr/bin/codex',
        args: ['app-server'],
        cwd: '/tmp/work',
        env: { HOME: '/home/u', PATH: '/usr/bin', EMPTY: undefined },
        logFile: '/tmp/app-server.log',
      },
      'aac-launch-codex-app-server-123-abcdef'
    );
    expect(argv[0]).toBe('--user');
    expect(argv).toContain('--unit=aac-launch-codex-app-server-123-abcdef');
    expect(argv).toContain('--collect');
    expect(argv).toContain('--working-directory=/tmp/work');
    expect(argv).toContain('--setenv=HOME');
    expect(argv).toContain('--setenv=PATH');
    expect(argv.some((a) => a.startsWith('--setenv=EMPTY'))).toBe(false);
    expect(argv).toContain('--property=KillMode=process');
    expect(argv).toContain('--property=StandardOutput=append:/tmp/app-server.log');
    expect(argv).toContain('--property=StandardError=append:/tmp/app-server.log');
    expect(argv).not.toContain('--scope');
    const sep = argv.indexOf('--');
    expect(sep).toBeGreaterThan(0);
    expect(argv.slice(sep + 1)).toEqual(['/usr/bin/codex', 'app-server']);
  });

  it('discards the output of GUI apps', () => {
    const argv = buildSystemdRunArgs(
      { app: 'codex-desktop', exe: '/usr/lib/chatgpt/ChatGPT', args: [] },
      'aac-launch-codex-desktop-1-x'
    );
    expect(argv).toContain('--property=StandardOutput=null');
    expect(argv).toContain('--property=StandardError=null');
    expect(argv).toContain('--property=KillMode=process');
    expect(argv.slice(argv.indexOf('--') + 1)).toEqual(['/usr/lib/chatgpt/ChatGPT']);
  });

  it('never puts an environment value on the argv and hands it to systemd-run instead', async () => {
    const secrets = { OPENAI_API_KEY: 'sk-fixture-secret', GH_TOKEN: 'ghp-fixture-secret' };
    const calls: Array<{ args: string[]; options: { env?: Record<string, string> } }> = [];
    const saved = process.env.XDG_RUNTIME_DIR;
    process.env.XDG_RUNTIME_DIR = '/run/user/dashboard';
    try {
      await launchApp(
        {
          app: 'codex-app-server',
          exe: '/usr/bin/codex',
          args: ['app-server'],
          env: { ...secrets, XDG_RUNTIME_DIR: '/run/user/stale' },
        },
        {
          platform: 'linux',
          systemdRunPath: '/usr/bin/systemd-run',
          spawn: ((_file: string, args: string[], options: { env?: Record<string, string> }) => {
            calls.push({ args, options });
            return fakeChild({ code: 0 });
          }) as unknown as typeof import('child_process').spawn,
        }
      );
    } finally {
      if (saved === undefined) delete process.env.XDG_RUNTIME_DIR;
      else process.env.XDG_RUNTIME_DIR = saved;
    }
    const argv = calls[0].args.join(' ');
    for (const value of Object.values(secrets)) expect(argv).not.toContain(value);
    expect(calls[0].args).toContain('--setenv=OPENAI_API_KEY');
    expect(calls[0].args).toContain('--setenv=GH_TOKEN');
    expect(calls[0].options.env?.OPENAI_API_KEY).toBe(secrets.OPENAI_API_KEY);
    expect(calls[0].options.env?.GH_TOKEN).toBe(secrets.GH_TOKEN);
    // The dashboard's runtime folder is how systemd-run reaches the user manager.
    expect(calls[0].options.env?.XDG_RUNTIME_DIR).toBe('/run/user/dashboard');
  });

  it('leaves out names systemd refuses instead of failing the launch', () => {
    const spec = {
      app: 'codex-desktop',
      exe: '/usr/lib/chatgpt/ChatGPT',
      args: [],
      env: { KEEP: 'yes', 'BASH_FUNC_ml%%': '() { :; }', 'a.b': '1', '1BAD': '1', NUL: 'a\0b' },
    };
    const setenv = buildSystemdRunArgs(spec, 'unit').filter((a) => a.startsWith('--setenv'));
    expect(setenv).toEqual(['--setenv=KEEP']);
    expect(systemdRunEnvironment(spec, {})).toEqual({ KEEP: 'yes' });
  });

  it('keeps a captured loader setting out of systemd-run and off the argv, and gives it to the app', () => {
    const spec = {
      app: 'codex-desktop',
      exe: '/opt/app',
      args: ['--flag'],
      env: { KEEP: 'yes', LD_PRELOAD: '/secret/preload.so', LD_LIBRARY_PATH: '/secret/lib' },
    };
    const argv = buildSystemdRunArgs(spec, 'unit');
    expect(argv.join(' ')).not.toContain('/secret');
    expect(argv).toContain('--setenv=AAC_LOADER_LD_PRELOAD');
    expect(argv).toContain('--setenv=AAC_LOADER_LD_LIBRARY_PATH');
    expect(argv.some((a) => a === '--setenv=LD_PRELOAD' || a === '--setenv=LD_LIBRARY_PATH')).toBe(
      false
    );
    const command = argv.slice(argv.indexOf('--') + 1);
    expect(command.slice(0, 2)).toEqual(['/bin/sh', '-c']);
    expect(command.slice(3)).toEqual(['sh', '/opt/app', '--flag']);
    const own = systemdRunEnvironment(spec, {}) ?? {};
    expect(own.LD_PRELOAD).toBeUndefined();
    expect(own.LD_LIBRARY_PATH).toBeUndefined();
    expect(own.AAC_LOADER_LD_PRELOAD).toBe('/secret/preload.so');
    // Every $ is escaped, or systemd would expand the value into this shell's argv.
    expect(command[2]).not.toMatch(/(^|[^$])\$([^$]|$)/);
    // The step as systemd hands it to sh: the app sees the values under their own names.
    const seen = execFileSync(
      '/bin/sh',
      [
        '-c',
        command[2].replaceAll('$$', '$'),
        'sh',
        '/bin/sh',
        '-c',
        'echo "$LD_LIBRARY_PATH|$AAC_LOADER_LD_LIBRARY_PATH|$KEEP"',
      ],
      { env: { KEEP: 'yes', AAC_LOADER_LD_LIBRARY_PATH: '/nonexistent/lib' }, encoding: 'utf8' }
    );
    expect(seen.trim()).toBe('/nonexistent/lib||yes');
  });

  it('runs the app directly when no loader setting was captured', () => {
    const argv = buildSystemdRunArgs(
      { app: 'app', exe: '/opt/app', args: [], env: { KEEP: 'yes' } },
      'unit'
    );
    expect(argv.slice(argv.indexOf('--') + 1)).toEqual(['/opt/app']);
  });

  it('lets systemd-run inherit this environment when the spec names none', () => {
    const spec = { app: 'app', exe: '/bin/true', args: [] };
    expect(systemdRunEnvironment(spec, { XDG_RUNTIME_DIR: '/run/user/1' })).toBeUndefined();
  });

  it('sanitizes unit names', () => {
    expect(sanitizeAppName('Codex CLI!')).toBe('codex-cli');
    expect(sanitizeAppName('!!!')).toBe('app');
    expect(transientUnitName('codex-cli', 999, 'abc123')).toBe('aac-launch-codex-cli-999-abc123');
  });

  it('launches on linux through systemd-run and waits for success', async () => {
    const calls: Array<{ file: string; args: string[] }> = [];
    const result = await launchApp(
      { app: 'codex-app-server', exe: '/usr/bin/codex', args: ['app-server'] },
      {
        platform: 'linux',
        systemdRunPath: '/usr/bin/systemd-run',
        pid: 4242,
        randomSuffix: () => 'deadbeef',
        spawn: ((file: string, args: string[]) => {
          calls.push({ file, args });
          return fakeChild({ code: 0 });
        }) as unknown as typeof import('child_process').spawn,
      }
    );
    expect(result).toEqual({
      route: 'systemd-service',
      unit: 'aac-launch-codex-app-server-4242-deadbeef',
    });
    expect(calls).toHaveLength(1);
    expect(calls[0].file).toBe('/usr/bin/systemd-run');
    expect(calls[0].args).toContain('--unit=aac-launch-codex-app-server-4242-deadbeef');
    expect(calls[0].args).not.toContain('--scope');
  });

  it('fails closed when systemd-run is unavailable', async () => {
    let spawned = false;
    await expect(
      launchApp(
        { app: 'codex-app-server', exe: '/usr/bin/codex', args: [] },
        {
          platform: 'linux',
          systemdRunPath: null,
          spawn: (() => {
            spawned = true;
            return fakeChild({ code: 0 });
          }) as unknown as typeof import('child_process').spawn,
        }
      )
    ).rejects.toThrow(/outside the dashboard cgroup/);
    expect(spawned).toBe(false);
  });

  it('fails closed when systemd-run exits non-zero', async () => {
    await expect(
      launchApp(
        { app: 'codex-app-server', exe: '/usr/bin/codex', args: [] },
        {
          platform: 'linux',
          systemdRunPath: '/usr/bin/systemd-run',
          spawn: (() => fakeChild({ code: 1 })) as unknown as typeof import('child_process').spawn,
        }
      )
    ).rejects.toThrow(/systemd-run exited with code 1/);
  });

  it('spawns detached directly on non-linux platforms', async () => {
    const calls: Array<{ file: string; args: string[]; options: Record<string, unknown> }> = [];
    const result = await launchApp(
      { app: 'codex-desktop', exe: '/Applications/ChatGPT.app/x', args: ['a'], cwd: '/tmp' },
      {
        platform: 'darwin',
        spawn: ((file: string, args: string[], options: Record<string, unknown>) => {
          calls.push({ file, args, options });
          return fakeChild({});
        }) as unknown as typeof import('child_process').spawn,
      }
    );
    expect(result).toEqual({ route: 'direct' });
    expect(calls).toHaveLength(1);
    expect(calls[0].file).toBe('/Applications/ChatGPT.app/x');
    expect(calls[0].options['detached']).toBe(true);
    expect(calls[0].options['stdio']).toBe('ignore');
  });
});
