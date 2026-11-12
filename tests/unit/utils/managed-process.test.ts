import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  runManagedCommand,
  processBrokerStats,
  startProcessBroker,
  stopProcessBroker,
} from '../../../src/utils/managed-process';

const roots: string[] = [];
afterEach(async () => {
  await stopProcessBroker();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});
const options = { timeoutMs: 2000, maxBytes: 65536, env: { PATH: '/usr/bin:/bin' } };
const python = (source: string, extra: Partial<typeof options> = {}) =>
  runManagedCommand('/usr/bin/python3', ['-I', '-c', source], { ...options, ...extra });

describe('small command parent', () => {
  it('accepts an account-collection burst without refusing valid concurrent commands', async () => {
    const results = await Promise.all(
      Array.from({ length: 20 }, (_, i) => python(`import time;time.sleep(.1);print(${i})`))
    );
    expect(
      results.every((result, i) => result.code === 0 && result.stdout.trim() === String(i))
    ).toBe(true);
  });
  it('preserves argv, stdin, env and cwd without a shell; stderr stays private', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-'));
    roots.push(root);
    const result = await runManagedCommand(
      '/usr/bin/python3',
      [
        '-I',
        '-c',
        'import os,sys,json;sys.stderr.write("fixture-private-stderr");print(json.dumps([sys.argv[1],sys.stdin.read(),os.getcwd(),os.environ.get("FIXTURE")]))',
        '$(false);literal',
      ],
      { ...options, cwd: root, input: 'fixture-private-stdin', env: { FIXTURE: 'fixture-env' } }
    );
    expect(result.failed).toBe(false);
    expect(JSON.parse(result.stdout)).toEqual([
      '$(false);literal',
      'fixture-private-stdin',
      root,
      'fixture-env',
    ]);
    expect(JSON.stringify(result)).not.toContain('fixture-private-stderr');
  });

  it('deduplicates only identical concurrent reads, and runs again after settlement', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-dedupe-'));
    roots.push(root);
    const file = path.join(root, 'count');
    const args = [
      '-I',
      '-c',
      'import sys,time;open(sys.argv[1],"a").write("x");time.sleep(.05);print("ok")',
      file,
    ];
    const invoke = () =>
      runManagedCommand('/usr/bin/python3', args, { ...options, coalesce: true });
    const results = await Promise.all(Array.from({ length: 12 }, invoke));
    expect(results.every((r) => r.stdout === 'ok\n')).toBe(true);
    expect(fs.readFileSync(file, 'utf8')).toBe('x');
    await invoke();
    expect(fs.readFileSync(file, 'utf8')).toBe('xx');
    expect(processBrokerStats().pending).toBe(0);
    expect(processBrokerStats().coalesced).toBe(0);
  });

  it('settles outstanding work on helper failure and restarts for the next request', async () => {
    const child = await startProcessBroker();
    const before = processBrokerStats().launches;
    const outstanding = python('import time;time.sleep(10)');
    await new Promise((resolve) => setTimeout(resolve, 100));
    child.kill('SIGKILL');
    expect((await outstanding).failed).toBe(true);
    expect((await python('print("restarted")')).stdout).toBe('restarted\n');
    expect(processBrokerStats().launches).toBe(before + 1);
  });

  it('kills a command that ignores SIGTERM, bounds output and reports missing programs', async () => {
    const result = await python(
      'import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(10)',
      { timeoutMs: 150, ...{ graceMs: 20 } }
    );
    expect(result.failed).toBe(true);
    expect(result.timedOut).toBe(true);
    const gracefulTimeout = await python(
      'import signal,sys,time;signal.signal(signal.SIGTERM,lambda *_: sys.exit(0));time.sleep(10)',
      { timeoutMs: 150 }
    );
    expect(gracefulTimeout.timedOut).toBe(true);
    expect(gracefulTimeout.code).not.toBe(0);
    expect((await python('print("x"*100000)', { maxBytes: 100 })).failed).toBe(true);
    const missing = await runManagedCommand('/fixture/no-such-command', [], options);
    expect(missing.failed).toBe(true);
    expect(missing.missing).toBe(true);
    expect(processBrokerStats().pending).toBe(0);
  });
});
