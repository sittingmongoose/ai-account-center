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
import { withDeadline } from '../../../src/utils/settle-deadline';

const roots: string[] = [];
afterEach(async () => {
  await stopProcessBroker();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});
const options = { timeoutMs: 2000, maxBytes: 65536, env: { PATH: '/usr/bin:/bin' } };
const python = (source: string, extra: Partial<typeof options> = {}) =>
  runManagedCommand('/usr/bin/python3', ['-I', '-c', source], { ...options, ...extra });

async function hangOwnedBroker() {
  const child = await startProcessBroker();
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-hung-'));
  roots.push(root);
  const marker = path.join(root, 'command-started');
  const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
  const work = python(
    `import time;open(${JSON.stringify(marker)},'w').write('started');time.sleep(10)`,
    { timeoutMs: 500, ...{ graceMs: 20 } }
  );
  for (let attempt = 0; attempt < 100 && !fs.existsSync(marker); attempt++)
    await new Promise((resolve) => setTimeout(resolve, 10));
  expect(fs.existsSync(marker)).toBe(true);
  // Only this fixture's returned process handle is ever stopped or signalled.
  expect(child.kill('SIGSTOP')).toBe(true);
  return { child, exited, work };
}

describe.skipIf(process.platform === 'win32')('small command parent', () => {
  it('forces a lost unresponsive broker to exit without harming its replacement', async () => {
    const { child, exited, work } = await hangOwnedBroker();
    try {
      expect((await work).failed).toBe(true);
      expect(processBrokerStats().pid).toBeNull();
      expect(processBrokerStats().retired).toBe(1);
      const replacement = await startProcessBroker();
      expect(replacement.pid).not.toBe(child.pid);
      await withDeadline(exited, 2000);
      expect(child.signalCode).toBe('SIGKILL');
      expect(processBrokerStats().retired).toBe(0);
      expect((await python('print("replacement-alive")')).stdout).toBe('replacement-alive\n');
      expect(processBrokerStats().pid).toBe(replacement.pid!);
    } finally {
      child.kill('SIGKILL');
      await exited;
    }
  });

  it('shutdown waits for a broker already retired by the request watchdog', async () => {
    const { child, exited, work } = await hangOwnedBroker();
    let didExit = false;
    void exited.then(() => {
      didExit = true;
    });
    try {
      expect((await work).failed).toBe(true);
      expect(processBrokerStats().pid).toBeNull();
      expect(processBrokerStats().retired).toBe(1);
      const replacement = await startProcessBroker();
      let replacementExited = false;
      replacement.once('exit', () => {
        replacementExited = true;
      });
      const shutdown = stopProcessBroker();
      expect(stopProcessBroker()).toBe(shutdown);
      await shutdown;
      expect(didExit).toBe(true);
      expect(replacementExited).toBe(true);
      expect(child.signalCode).toBe('SIGKILL');
      expect(processBrokerStats().retired).toBe(0);
    } finally {
      child.kill('SIGKILL');
      await exited;
    }
  });
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
