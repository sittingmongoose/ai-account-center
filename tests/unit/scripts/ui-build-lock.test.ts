import { afterEach, describe, expect, it } from 'bun:test';
import { spawn, spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';

const {
  runWithUiBuildLock,
  isStale,
  lockPath,
  wasmTargetDir,
  waitSeconds,
  hasFlockTool,
  BUSY_MESSAGE,
  FLOCK_BUSY_EXIT,
  STALE_AFTER_MS,
} = require('../../../scripts/ui-build-lock.js');
const { buildUi, commandRunner } = require('../../../scripts/build-ui.js');

const LOCK_MODULE = path.resolve(__dirname, '../../../scripts/ui-build-lock.js');
const dirs: string[] = [];
const children: Array<ReturnType<typeof spawn>> = [];

function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-ui-lock-'));
  dirs.push(dir);
  return dir;
}

/** A fake clock: sleeping moves time forward, so waits finish instantly. */
function fakeClock() {
  let time = 1_000_000;
  return { now: () => time, sleep: (ms: number) => (time += ms) };
}

function deadPid(): number {
  const child = spawnSync(process.execPath, ['-e', 'process.stdout.write(String(process.pid))'], {
    encoding: 'utf8',
  });
  return Number(child.stdout);
}

/** Starts another process that holds the lock (through a fake build) until it is killed. */
function holder(file: string, useFlock: boolean): Promise<ReturnType<typeof spawn>> {
  const script = `
    const { runWithUiBuildLock } = require(${JSON.stringify(LOCK_MODULE)});
    const { spawnSync } = require('child_process');
    runWithUiBuildLock('sh', ['-c', 'echo held; sleep 30'], {}, {
      lockFile: ${JSON.stringify(file)},
      useFlock: ${useFlock},
      waitS: 0,
      run: (command, args) => {
        const result = spawnSync(command, args, { stdio: 'inherit' });
        if (result.status !== 0) {
          const error = new Error('failed');
          error.status = result.status;
          throw error;
        }
        return '';
      },
    });
  `;
  const child = spawn(process.execPath, ['-e', script], {
    stdio: ['ignore', 'pipe', 'inherit'],
    detached: true,
  });
  children.push(child);
  return new Promise((resolve, reject) => {
    child.stdout!.on('data', (data) => {
      if (String(data).includes('held')) resolve(child);
    });
    child.on('exit', (code) => reject(new Error(`holder exited early (${code})`)));
  });
}

/** Kills the holder and its fake build (the whole process group). */
function killHolder(child: ReturnType<typeof spawn>) {
  try {
    process.kill(-child.pid!, 'SIGKILL');
  } catch {
    // Already gone.
  }
}

function exited(child: ReturnType<typeof spawn>): Promise<void> {
  return new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) resolve();
    else child.on('exit', () => resolve());
  });
}

afterEach(async () => {
  for (const child of children.splice(0)) {
    killHolder(child);
    await exited(child);
  }
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

describe('UI build lane lock (owner-file fallback)', () => {
  it('holds the lock while the build runs and releases it afterwards', () => {
    const file = path.join(tempDir(), 'nested', 'ui-build.lock');
    let heldDuringRun = false;
    const result = runWithUiBuildLock(
      'wasm-pack',
      ['build'],
      { cwd: '/x' },
      {
        lockFile: file,
        useFlock: false,
        run: (command: string, args: string[], options: { cwd: string }) => {
          expect([command, args, options.cwd]).toEqual(['wasm-pack', ['build'], '/x']);
          heldDuringRun = JSON.parse(fs.readFileSync(file, 'utf8')).pid === process.pid;
          return 'built';
        },
      }
    );
    expect(result).toBe('built');
    expect(heldDuringRun).toBe(true);
    expect(fs.existsSync(file)).toBe(false);
  });

  it('releases the lock when the build fails', () => {
    const file = path.join(tempDir(), 'ui-build.lock');
    expect(() =>
      runWithUiBuildLock(
        'wasm-pack',
        [],
        {},
        {
          lockFile: file,
          useFlock: false,
          run: () => {
            throw new Error('wasm-pack failed with exit 1.');
          },
        }
      )
    ).toThrow('wasm-pack failed');
    expect(fs.existsSync(file)).toBe(false);
  });

  it('gives up with the clear busy message after the wait, logging once a minute', () => {
    const file = path.join(tempDir(), 'ui-build.lock');
    const clock = fakeClock();
    const busy = JSON.stringify({ pid: process.pid, acquiredAt: clock.now(), startTicks: null });
    fs.writeFileSync(file, busy);
    const lines: string[] = [];
    let ran = false;
    expect(() =>
      runWithUiBuildLock(
        'wasm-pack',
        [],
        {},
        {
          lockFile: file,
          useFlock: false,
          waitS: 150,
          now: clock.now,
          sleep: clock.sleep,
          log: (line: string) => lines.push(line),
          run: () => (ran = true),
        }
      )
    ).toThrow(BUSY_MESSAGE);
    expect(ran).toBe(false);
    expect(lines.length).toBe(2);
    expect(lines[0]).toContain('another release wasm build is running');
    // Someone else's lock is never removed.
    expect(fs.readFileSync(file, 'utf8')).toBe(busy);
  });

  it('clears a lock whose holder died and takes it', () => {
    const file = path.join(tempDir(), 'ui-build.lock');
    fs.writeFileSync(file, JSON.stringify({ pid: deadPid(), acquiredAt: Date.now() }));
    let ran = false;
    runWithUiBuildLock(
      'wasm-pack',
      [],
      {},
      {
        lockFile: file,
        useFlock: false,
        waitS: 0,
        run: () => (ran = true),
      }
    );
    expect(ran).toBe(true);
    expect(fs.existsSync(file)).toBe(false);
    expect(fs.readdirSync(path.dirname(file))).toEqual([]);
  });

  it('treats unreadable, too old or reused-PID owner records as stale', () => {
    const now = Date.now();
    const alive = () => true;
    const ticks = () => '500';
    expect(isStale('not json', now, alive, ticks)).toBe(true);
    expect(isStale(JSON.stringify({ pid: 0, acquiredAt: now }), now, alive, ticks)).toBe(true);
    expect(
      isStale(JSON.stringify({ pid: 42, acquiredAt: now - STALE_AFTER_MS - 1 }), now, alive, ticks)
    ).toBe(true);
    expect(
      isStale(JSON.stringify({ pid: 42, acquiredAt: now, startTicks: '499' }), now, alive, ticks)
    ).toBe(true);
    expect(
      isStale(JSON.stringify({ pid: 42, acquiredAt: now, startTicks: '500' }), now, alive, ticks)
    ).toBe(false);
    expect(isStale(JSON.stringify({ pid: 42, acquiredAt: now }), now, () => false, ticks)).toBe(
      true
    );
  });

  it('makes a second process fail fast while the first holds the lane, then frees it on death', async () => {
    const file = path.join(tempDir(), 'ui-build.lock');
    const first = await holder(file, false);
    expect(() =>
      runWithUiBuildLock(
        'wasm-pack',
        [],
        {},
        {
          lockFile: file,
          useFlock: false,
          waitS: 0,
          run: () => '',
        }
      )
    ).toThrow(BUSY_MESSAGE);
    killHolder(first);
    await exited(first);
    let ran = false;
    runWithUiBuildLock(
      'wasm-pack',
      [],
      {},
      {
        lockFile: file,
        useFlock: false,
        waitS: 0,
        run: () => (ran = true),
      }
    );
    expect(ran).toBe(true);
  });
});

describe('UI build lane lock (flock)', () => {
  it('retries while flock reports the lane busy, then runs the build under flock', () => {
    const file = path.join(tempDir(), 'ui-build.lock');
    const clock = fakeClock();
    const calls: string[][] = [];
    const result = runWithUiBuildLock(
      'wasm-pack',
      ['build', '--release'],
      {},
      {
        lockFile: file,
        useFlock: true,
        now: clock.now,
        sleep: clock.sleep,
        run: (command: string, args: string[]) => {
          calls.push([command, ...args]);
          if (calls.length < 3) throw Object.assign(new Error('busy'), { status: FLOCK_BUSY_EXIT });
          return 'built';
        },
      }
    );
    expect(result).toBe('built');
    expect(calls.at(-1)).toEqual([
      'flock',
      '-n',
      '-E',
      String(FLOCK_BUSY_EXIT),
      file,
      'wasm-pack',
      'build',
      '--release',
    ]);
  });

  it('passes a real build failure straight through', () => {
    const file = path.join(tempDir(), 'ui-build.lock');
    expect(() =>
      runWithUiBuildLock(
        'wasm-pack',
        [],
        {},
        {
          lockFile: file,
          useFlock: true,
          run: () => {
            throw Object.assign(new Error('wasm-pack failed with exit 1.'), { status: 1 });
          },
        }
      )
    ).toThrow('wasm-pack failed');
  });

  it.skipIf(!hasFlockTool())(
    'blocks a concurrent real flock build and frees the lane when the holder dies',
    async () => {
      const file = path.join(tempDir(), 'ui-build.lock');
      const first = await holder(file, true);
      expect(() =>
        runWithUiBuildLock(
          'true',
          [],
          { capture: true },
          {
            lockFile: file,
            useFlock: true,
            waitS: 0,
            run: commandRunner,
          }
        )
      ).toThrow(BUSY_MESSAGE);
      killHolder(first);
      await exited(first);
      const deadline = Date.now() + 5000;
      let ok = false;
      while (!ok && Date.now() < deadline) {
        try {
          runWithUiBuildLock(
            'true',
            [],
            { capture: true },
            {
              lockFile: file,
              useFlock: true,
              waitS: 0,
              run: commandRunner,
            }
          );
          ok = true;
        } catch {
          await new Promise((resolve) => setTimeout(resolve, 100));
        }
      }
      expect(ok).toBe(true);
    }
  );
});

describe('UI build defaults', () => {
  it('keeps the lock and the shared wasm target in the user cache, never in the worktree', () => {
    expect(lockPath({ XDG_CACHE_HOME: '/c' })).toBe(
      path.join('/c', 'ai-account-center', 'ui-build.lock')
    );
    expect(wasmTargetDir({ XDG_CACHE_HOME: '/c' })).toBe(
      path.join('/c', 'ai-account-center', 'cargo-target-wasm')
    );
    expect(wasmTargetDir({})).toBe(
      path.join(os.homedir(), '.cache', 'ai-account-center', 'cargo-target-wasm')
    );
    expect(wasmTargetDir({ XDG_CACHE_HOME: '/c', CARGO_TARGET_DIR: '/explicit' })).toBe(
      '/explicit'
    );
  });

  it('waits 600 seconds unless AAC_UI_BUILD_LOCK_WAIT_S says otherwise', () => {
    expect(waitSeconds({})).toBe(600);
    expect(waitSeconds({ AAC_UI_BUILD_LOCK_WAIT_S: '30' })).toBe(30);
    expect(waitSeconds({ AAC_UI_BUILD_LOCK_WAIT_S: 'soon' })).toBe(600);
  });

  it('gives wasm-pack the shared target by default and an explicit CARGO_TARGET_DIR otherwise', () => {
    const cache = tempDir();
    const seen: Array<string | undefined> = [];
    const run = (command: string, _args: string[], options: { env: Record<string, string> }) => {
      if (command.includes('rustc')) return 'rustc 1.98.0 (fixture)';
      if (command.includes('rustup')) return 'wasm32-unknown-unknown\n';
      if (command.includes('wasm-pack') && _args[0] === 'build')
        seen.push(options.env.CARGO_TARGET_DIR);
      // Stop right after the build call; the output steps are covered by build-ui tests.
      if (_args[0] === 'build') throw new Error('stop');
      return '';
    };
    const repoRoot = path.resolve(__dirname, '../../..');
    const lock = { useFlock: false, lockFile: path.join(cache, 'ui-build.lock') };
    expect(() => buildUi({ repoRoot, run, lock, env: { XDG_CACHE_HOME: cache } })).toThrow('stop');
    expect(() =>
      buildUi({ repoRoot, run, lock, env: { XDG_CACHE_HOME: cache, CARGO_TARGET_DIR: '/deploy' } })
    ).toThrow('stop');
    expect(seen).toEqual([path.join(cache, 'ai-account-center', 'cargo-target-wasm'), '/deploy']);
  });
});
