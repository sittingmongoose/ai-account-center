import { describe, expect, it } from 'bun:test';
import type { ChildProcess } from 'child_process';
import {
  buildSystemdRunArgs,
  launchApp,
  sanitizeAppName,
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
    expect(argv).toContain('--setenv=HOME=/home/u');
    expect(argv).toContain('--setenv=PATH=/usr/bin');
    expect(argv.some((a) => a.startsWith('--setenv=EMPTY='))).toBe(false);
    expect(argv).toContain('--property=StandardOutput=append:/tmp/app-server.log');
    expect(argv).toContain('--property=StandardError=append:/tmp/app-server.log');
    expect(argv).not.toContain('--scope');
    const sep = argv.indexOf('--');
    expect(sep).toBeGreaterThan(0);
    expect(argv.slice(sep + 1)).toEqual(['/usr/bin/codex', 'app-server']);
  });

  it('omits log properties for GUI apps', () => {
    const argv = buildSystemdRunArgs(
      { app: 'codex-desktop', exe: '/usr/lib/chatgpt/ChatGPT', args: [] },
      'aac-launch-codex-desktop-1-x'
    );
    expect(argv.some((a) => a.startsWith('--property='))).toBe(false);
    expect(argv.slice(argv.indexOf('--') + 1)).toEqual(['/usr/lib/chatgpt/ChatGPT']);
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
