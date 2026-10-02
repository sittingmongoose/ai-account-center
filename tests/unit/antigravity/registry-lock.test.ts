import { afterEach, expect, spyOn, test } from 'bun:test';
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { AntigravityProfileRegistry, PrivateStorageError } from '../../../src/antigravity';

const STALE_AFTER_MS = 11 * 60_000;
const temporary: string[] = [];
afterEach(() => {
  for (const directory of temporary.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

function setup(): { profiles: string; registry: AntigravityProfileRegistry } {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-lock-'));
  temporary.push(directory);
  const ccs = path.join(directory, '.ccs');
  const registry = new AntigravityProfileRegistry(ccs);
  return { profiles: path.join(ccs, 'antigravity-profiles'), registry };
}

function plantLock(
  profiles: string,
  holder: { pid: number; startedAt: string } | null,
  ageMs = 0
): string {
  const lock = path.join(profiles, '.transaction-lock');
  fs.mkdirSync(lock, { mode: 0o700 });
  if (holder)
    fs.writeFileSync(path.join(lock, 'holder.json'), JSON.stringify(holder), {
      flag: 'wx',
      mode: 0o600,
    });
  if (ageMs) {
    const aged = new Date(Date.now() - ageMs);
    fs.utimesSync(lock, aged, aged);
  }
  return lock;
}

/** A disposable reaped child pid; the fixture asserts ESRCH before use. */
async function deadPid(): Promise<number> {
  const child = spawn(process.execPath, ['--version'], { stdio: 'ignore' });
  const pid = child.pid;
  await new Promise<void>((resolve, reject) => {
    child.once('exit', () => resolve());
    child.once('error', reject);
  });
  if (pid === undefined) throw new Error('fixture could not spawn a disposable process');
  return pid;
}

/** bun's mockRestore clears call history; capture the one-line reasons first. */
async function withCapturedWarnings(run: () => Promise<unknown>): Promise<string[]> {
  const warn = spyOn(console, 'warn').mockImplementation(() => {});
  try {
    await run();
    return warn.mock.calls.map((call) => String(call[0]));
  } finally {
    warn.mockRestore();
  }
}

test('a lock whose recorded holder pid is dead is taken over with a one-line reason', async () => {
  const { profiles, registry } = setup();
  const pid = await deadPid();
  expect(() => process.kill(pid, 0)).toThrow();
  const lock = plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  let ran = false;
  const warnings = await withCapturedWarnings(() =>
    registry.withLock(async () => {
      ran = true;
    })
  );
  expect(ran).toBe(true);
  expect(warnings.length).toBe(1);
  expect(warnings[0]).toContain(`holder pid ${pid} is dead`);
  expect(warnings[0]).not.toContain('\n');
  expect(fs.existsSync(lock)).toBe(false);
  expect(fs.readdirSync(profiles).filter((name) => name.includes('.stale-'))).toEqual([]);
});

test('a lock older than the bounded timeout is taken over even with a live holder pid', async () => {
  const { profiles, registry } = setup();
  const lock = plantLock(profiles, {
    pid: process.pid,
    startedAt: new Date(Date.now() - STALE_AFTER_MS).toISOString(),
  });
  let ran = false;
  const warnings = await withCapturedWarnings(() =>
    registry.withLock(async () => {
      ran = true;
    })
  );
  expect(ran).toBe(true);
  expect(warnings.length).toBe(1);
  expect(warnings[0]).toContain('held the lock for');
  expect(fs.existsSync(lock)).toBe(false);
});

test('a holder-less legacy lock ages out on its directory timestamp alone', async () => {
  const { profiles, registry } = setup();
  const lock = plantLock(profiles, null, STALE_AFTER_MS);
  let ran = false;
  const warnings = await withCapturedWarnings(() =>
    registry.withLock(async () => {
      ran = true;
    })
  );
  expect(ran).toBe(true);
  expect(warnings.length).toBe(1);
  expect(warnings[0]).toContain('without a holder record');
  expect(fs.existsSync(lock)).toBe(false);
  expect(fs.readdirSync(profiles).filter((name) => name.includes('.stale-'))).toEqual([]);
});

test('a live, recent holder is respected and its lock folder stays untouched', async () => {
  const { profiles, registry } = setup();
  const lock = plantLock(profiles, { pid: process.pid, startedAt: new Date().toISOString() });
  await expect(registry.withLock(async () => undefined)).rejects.toMatchObject({ code: 'busy' });
  expect(fs.readdirSync(lock)).toEqual(['holder.json']);
  const holder = JSON.parse(fs.readFileSync(path.join(lock, 'holder.json'), 'utf8'));
  expect(holder.pid).toBe(process.pid);
  expect(fs.readdirSync(profiles).filter((name) => name.includes('.stale-'))).toEqual([]);
});

test('a recent holder-less lock stays busy until it ages out', async () => {
  const { profiles, registry } = setup();
  const lock = plantLock(profiles, null);
  await expect(registry.withLock(async () => undefined)).rejects.toMatchObject({ code: 'busy' });
  expect(fs.existsSync(lock)).toBe(true);
  expect(fs.readdirSync(lock)).toEqual([]);
});

test('two concurrent acquirers of one stale lock never both win', async () => {
  const { profiles } = setup();
  const pid = await deadPid();
  expect(() => process.kill(pid, 0)).toThrow();
  plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  const ccs = path.dirname(profiles);
  const first = new AntigravityProfileRegistry(ccs);
  const second = new AntigravityProfileRegistry(ccs);
  let wins = 0;
  const warn = spyOn(console, 'warn').mockImplementation(() => {});
  let results: Array<PromiseSettledResult<void>>;
  try {
    results = await Promise.allSettled([
      first.withLock(async () => {
        wins++;
      }),
      second.withLock(async () => {
        wins++;
      }),
    ]);
  } finally {
    warn.mockRestore();
  }
  expect(wins).toBe(1);
  expect(results.filter((entry) => entry.status === 'fulfilled').length).toBe(1);
  const rejected = results.find(
    (entry) => entry.status === 'rejected'
  ) as PromiseRejectedResult | undefined;
  expect(rejected?.reason).toBeInstanceOf(PrivateStorageError);
  expect((rejected?.reason as PrivateStorageError).code).toBe('busy');
  expect(fs.existsSync(path.join(profiles, '.transaction-lock'))).toBe(false);
  expect(fs.readdirSync(profiles).filter((name) => name.includes('.stale-'))).toEqual([]);
});

test('a displaced lock with foreign contents is preserved across a takeover', async () => {
  const { profiles, registry } = setup();
  const pid = await deadPid();
  const lock = plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  fs.writeFileSync(path.join(lock, 'foreign-marker'), 'review me', { mode: 0o600 });
  const warn = spyOn(console, 'warn').mockImplementation(() => {});
  try {
    await registry.withLock(async () => undefined);
  } finally {
    warn.mockRestore();
  }
  const displaced = fs
    .readdirSync(profiles)
    .filter((name) => name.startsWith('.transaction-lock.stale-'));
  expect(displaced.length).toBe(1);
  expect(fs.readdirSync(path.join(profiles, displaced[0])).sort()).toEqual([
    'foreign-marker',
    'holder.json',
  ]);
  expect(fs.readFileSync(path.join(profiles, displaced[0], 'foreign-marker'), 'utf8')).toBe(
    'review me'
  );
  expect(fs.existsSync(lock)).toBe(false);
});
