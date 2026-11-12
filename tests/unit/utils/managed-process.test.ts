import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  runManagedCommand,
  type ManagedCommandOptions,
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
const python = (source: string, extra: Partial<ManagedCommandOptions> = {}) =>
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
  // Caller expiry cancels its command first; a missing broker acknowledgement retires it.
  const lostBroker = async () => {
    for (let attempt = 0; attempt < 200 && processBrokerStats().pid === child.pid; attempt++)
      await new Promise((resolve) => setTimeout(resolve, 10));
  };
  return {
    child,
    exited,
    work: work.then(async (result) => {
      await lostBroker();
      return result;
    }),
  };
}

describe.skipIf(process.platform === 'win32')('small command parent', () => {
  it('refuses an already expired command before starting a broker', async () => {
    const before = processBrokerStats().launches;
    expect((await python('print("too-late")', { deadlineAt: Date.now() - 1 })).timedOut).toBe(true);
    expect(processBrokerStats().launches).toBe(before);
  });

  it('never submits a command whose caller expires during broker startup', async () => {
    const starting = startProcessBroker();
    const pid = processBrokerStats().pid!;
    process.kill(pid, 'SIGSTOP');
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-startup-'));
    roots.push(root);
    const marker = path.join(root, 'late-command');
    try {
      const result = await python(`open(${JSON.stringify(marker)},'w').write('late')`, {
        deadlineAt: Date.now() + 100,
      });
      expect(result.timedOut).toBe(true);
    } finally {
      process.kill(pid, 'SIGCONT');
    }
    await starting;
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(fs.existsSync(marker)).toBe(false);
    expect(processBrokerStats().pending).toBe(0);
  });

  it('refuses expired IPC-queued work instead of spawning then cancelling it', async () => {
    const child = await startProcessBroker();
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-queued-'));
    roots.push(root);
    const marker = path.join(root, 'late-command');
    child.kill('SIGSTOP');
    try {
      expect(
        (
          await python(`open(${JSON.stringify(marker)},'w').write('late')`, {
            deadlineAt: Date.now() + 100,
          })
        ).timedOut
      ).toBe(true);
    } finally {
      child.kill('SIGCONT');
    }
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(fs.existsSync(marker)).toBe(false);
    expect(processBrokerStats().pending).toBe(0);
  });

  it('keeps a shared command alive for unexpired waiters without extending its original cap', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-waiters-'));
    roots.push(root);
    const marker = path.join(root, 'runs');
    const source = `import time;open(${JSON.stringify(marker)},'a').write('x');time.sleep(.3);print('ok')`;
    const early = python(source, { coalesce: true, deadlineAt: Date.now() + 150 });
    const later = python(source, { coalesce: true, deadlineAt: Date.now() + 1200 });
    expect((await early).timedOut).toBe(true);
    expect((await later).stdout).toBe('ok\n');
    expect(fs.readFileSync(marker, 'utf8')).toBe('x');

    const hanging = `import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(10)`;
    const first = python(hanging, { coalesce: true, timeoutMs: 400, graceMs: 0 });
    await new Promise((resolve) => setTimeout(resolve, 150));
    const began = Date.now();
    const joined = python(hanging, { coalesce: true, timeoutMs: 400, graceMs: 0 });
    expect((await first).timedOut).toBe(true);
    expect((await joined).timedOut).toBe(true);
    expect(Date.now() - began).toBeLessThan(350);
  });

  it('rejects an expired shared waiter when a queued result beats its overdue timer', async () => {
    await startProcessBroker();
    for (let attempt = 0; attempt < 3; attempt++) {
      const began = Date.now();
      const source = 'import time;time.sleep(.1);print("fresh")';
      const early = python(source, { coalesce: true, deadlineAt: began + 300 });
      const later = python(source, { coalesce: true, deadlineAt: began + 1200 });
      const stalled = new Promise<void>((resolve) =>
        setTimeout(() => {
          // Only the fixture parent stalls. The real broker completes its command meanwhile.
          Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 500);
          resolve();
        }, 50)
      );
      const [earlyResult, laterResult] = await Promise.all([early, later]);
      await stalled;
      expect(Date.now() - began).toBeGreaterThanOrEqual(500);
      expect(earlyResult).toEqual({
        code: null,
        stdout: '',
        timedOut: true,
        missing: false,
        failed: true,
      });
      expect(laterResult).toEqual({
        code: 0,
        stdout: 'fresh\n',
        timedOut: false,
        missing: false,
        failed: false,
      });
      expect(processBrokerStats().pending).toBe(0);
      expect(processBrokerStats().coalesced).toBe(0);
    }
  });

  it('kills the shared owned process group when its last waiter expires', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-command-cancel-'));
    roots.push(root);
    const marker = path.join(root, 'pids');
    const source = [
      'import os,signal,time,json',
      'signal.signal(signal.SIGTERM,signal.SIG_IGN)',
      'child=os.fork()',
      `if child: open(${JSON.stringify(marker)},'w').write(json.dumps([os.getpid(),child]))`,
      'time.sleep(10)',
    ].join('\n');
    const first = python(source, { coalesce: true, deadlineAt: Date.now() + 250 });
    const last = python(source, { coalesce: true, deadlineAt: Date.now() + 600 });
    expect((await first).timedOut).toBe(true);
    const pids: number[] = JSON.parse(fs.readFileSync(marker, 'utf8'));
    for (const pid of pids) expect(() => process.kill(pid, 0)).not.toThrow();
    expect((await last).timedOut).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 100));
    for (const pid of pids) {
      if (process.platform === 'linux' && fs.existsSync(`/proc/${pid}/stat`))
        expect(fs.readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1].startsWith('Z ')).toBe(
          true
        );
      else expect(() => process.kill(pid, 0)).toThrow();
    }
    expect((await python('print("usable")')).stdout).toBe('usable\n');
    expect(processBrokerStats().coalesced).toBe(0);
  });

  it('bounds the number of retained waiters on one shared command', async () => {
    const results = await Promise.all(
      Array.from({ length: 70 }, () =>
        python('import time;time.sleep(.1);print("ok")', { coalesce: true })
      )
    );
    expect(results.filter((result) => result.stdout === 'ok\n')).toHaveLength(64);
    expect(results.filter((result) => result.failed)).toHaveLength(6);
    expect(processBrokerStats().coalesced).toBe(0);
  });

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
