import { afterEach, expect, spyOn, test } from 'bun:test';
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { AntigravityProfileRegistry, PrivateStorageError } from '../../../src/antigravity';
import { currentBootId, processStartTime } from '../../../src/antigravity/holder-record';

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

interface PlantedHolder {
  pid: number;
  startedAt: string;
  processStart?: string | null;
  bootId?: string | null;
}

/** Lock, claim and staging folders left in the profiles folder. */
function leftovers(profiles: string): string[] {
  return fs.readdirSync(profiles).filter((name) => name.startsWith('.transaction-lock'));
}

function plantLock(
  profiles: string,
  holder: PlantedHolder | null,
  ageMs = 0,
  name = '.transaction-lock'
): string {
  const lock = path.join(profiles, name);
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

test('a live holder whose heartbeat stopped past the timeout is taken over', async () => {
  const { profiles, registry } = setup();
  const lock = plantLock(profiles, {
    pid: process.pid,
    startedAt: new Date(Date.now() - STALE_AFTER_MS).toISOString(),
  });
  const silent = new Date(Date.now() - STALE_AFTER_MS);
  fs.utimesSync(path.join(lock, 'holder.json'), silent, silent);
  let ran = false;
  const warnings = await withCapturedWarnings(() =>
    registry.withLock(async () => {
      ran = true;
    })
  );
  expect(ran).toBe(true);
  expect(warnings.length).toBe(1);
  expect(warnings[0]).toContain('sent no heartbeat for');
  expect(fs.existsSync(lock)).toBe(false);
});

test('a live holder that started long ago but beats recently is never taken over', async () => {
  const { profiles, registry } = setup();
  const lock = plantLock(profiles, {
    pid: process.pid,
    startedAt: new Date(Date.now() - 3 * STALE_AFTER_MS).toISOString(),
  });
  await expect(registry.withLock(async () => undefined)).rejects.toMatchObject({ code: 'busy' });
  expect(fs.readdirSync(lock)).toEqual(['holder.json']);
  expect(registry.lockHeldLive()).toBe(true);
});

test.if(process.platform === 'linux')(
  'the lock record names the holder process start time and boot',
  async () => {
    const { profiles, registry } = setup();
    let record: Record<string, unknown> = {};
    await registry.withLock(async () => {
      record = JSON.parse(
        fs.readFileSync(path.join(profiles, '.transaction-lock', 'holder.json'), 'utf8')
      );
    });
    expect(record.pid).toBe(process.pid);
    expect(record.processStart).toBe(processStartTime(process.pid));
    expect(record.bootId).toBe(currentBootId());
    expect(typeof record.processStart).toBe('string');
    expect(leftovers(profiles)).toEqual([]);
  }
);

test.if(process.platform === 'linux')(
  'a holder whose pid now runs another process is taken over at once',
  async () => {
    const { profiles, registry } = setup();
    const lock = plantLock(profiles, {
      pid: process.pid,
      startedAt: new Date().toISOString(),
      processStart: '1',
      bootId: currentBootId(),
    });
    const warnings = await withCapturedWarnings(() => registry.withLock(async () => undefined));
    expect(warnings.length).toBe(1);
    expect(warnings[0]).toContain(`holder pid ${process.pid} now belongs to another process`);
    expect(fs.existsSync(lock)).toBe(false);
  }
);

test.if(process.platform === 'linux')(
  'a holder from an earlier boot is taken over at once',
  async () => {
    const { profiles, registry } = setup();
    plantLock(profiles, {
      pid: process.pid,
      startedAt: new Date().toISOString(),
      processStart: processStartTime(process.pid) as string,
      bootId: '00000000-0000-4000-8000-000000000000',
    });
    const warnings = await withCapturedWarnings(() => registry.withLock(async () => undefined));
    expect(warnings.length).toBe(1);
    expect(warnings[0]).toContain('ran before this boot');
  }
);

test('lockHeldLive counts a live holder and never an abandoned lock', async () => {
  const { profiles, registry } = setup();
  expect(registry.lockHeldLive()).toBe(false);
  const lock = plantLock(profiles, { pid: process.pid, startedAt: new Date().toISOString() });
  expect(registry.lockHeldLive()).toBe(true);
  fs.rmSync(lock, { recursive: true });
  plantLock(profiles, { pid: await deadPid(), startedAt: new Date().toISOString() });
  expect(registry.lockHeldLive()).toBe(false);
  fs.rmSync(lock, { recursive: true });
  plantLock(profiles, null);
  expect(registry.lockHeldLive()).toBe(true);
  fs.rmSync(lock, { recursive: true });
  plantLock(profiles, null, STALE_AFTER_MS);
  expect(registry.lockHeldLive()).toBe(false);
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
  const rejected = results.find((entry) => entry.status === 'rejected') as
    | PromiseRejectedResult
    | undefined;
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

test('a lock that changed under the claim is put back, never kept aside', async () => {
  const { profiles, registry } = setup();
  const pid = await deadPid();
  const lock = plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  const original = fs.renameSync;
  let raced = false;
  let rival: fs.Stats | null = null;
  // The lock changes between the check under the claim and the rename (only a
  // judged-stale holder that was in fact alive and released could do this).
  const rename = spyOn(fs, 'renameSync').mockImplementation(((
    from: fs.PathLike,
    to: fs.PathLike
  ) => {
    if (!raced && String(from) === lock) {
      raced = true;
      original(lock, `${lock}.rival-aside`);
      fs.mkdirSync(lock, { mode: 0o700 });
      fs.writeFileSync(
        path.join(lock, 'holder.json'),
        JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString() }),
        { flag: 'wx', mode: 0o600 }
      );
      rival = fs.lstatSync(lock);
    }
    return original(from, to);
  }) as typeof fs.renameSync);
  let ran = false;
  try {
    await expect(
      registry.withLock(async () => {
        ran = true;
      })
    ).rejects.toMatchObject({ code: 'busy' });
  } finally {
    rename.mockRestore();
  }
  expect(raced).toBe(true);
  expect(ran).toBe(false);
  const current = fs.lstatSync(lock);
  expect(rival).not.toBeNull();
  expect(current.ino).toBe((rival as unknown as fs.Stats).ino);
  expect(JSON.parse(fs.readFileSync(path.join(lock, 'holder.json'), 'utf8')).pid).toBe(process.pid);
  expect(fs.readdirSync(profiles).filter((name) => name.includes('.stale-'))).toEqual([]);
});

test("three racers: a takeover that lost the race never frees or moves the winner's lock", async () => {
  const { profiles } = setup();
  const pid = await deadPid();
  const lock = plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  const ccs = path.dirname(profiles);
  const [a, b, c, d] = [0, 1, 2, 3].map(() => new AntigravityProfileRegistry(ccs));
  const claim = `${lock}.takeover`;
  const original = fs.renameSync;
  let raced = false;
  let nested = false;
  let bLock: fs.Stats | null = null;
  let bHolding = false;
  let movedWhileBHeld = 0;
  let release: () => void = () => undefined;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let bDone: Promise<void> = Promise.resolve();
  const rename = spyOn(fs, 'renameSync').mockImplementation(((
    from: fs.PathLike,
    to: fs.PathLike
  ) => {
    if (bHolding && String(from) === lock) movedWhileBHeld += 1;
    // A judged the lock stale; just before A takes the claim, B takes the
    // same stale lock over completely and starts its operation.
    if (!raced && !nested && String(to) === claim) {
      raced = true;
      nested = true;
      try {
        bDone = b.withLock(async () => {
          bLock = fs.lstatSync(lock);
          bHolding = true;
          await gate;
          bHolding = false;
        });
      } finally {
        nested = false;
      }
    }
    return original(from, to);
  }) as typeof fs.renameSync);
  const warn = spyOn(console, 'warn').mockImplementation(() => {});
  try {
    await expect(a.withLock(async () => undefined)).rejects.toMatchObject({ code: 'busy' });
    expect(raced).toBe(true);
    expect(bHolding).toBe(true);
    // C, a plain acquirer, finds B's lock held, and the lock is still B's.
    await expect(c.withLock(async () => undefined)).rejects.toMatchObject({ code: 'busy' });
    const current = fs.lstatSync(lock);
    expect(bLock).not.toBeNull();
    expect(current.ino).toBe((bLock as unknown as fs.Stats).ino);
    expect(movedWhileBHeld).toBe(0);
    expect(JSON.parse(fs.readFileSync(path.join(lock, 'holder.json'), 'utf8')).pid).toBe(
      process.pid
    );
    release();
    await bDone;
  } finally {
    rename.mockRestore();
    warn.mockRestore();
  }
  expect(fs.existsSync(lock)).toBe(false);
  let ran = false;
  await d.withLock(async () => {
    ran = true;
  });
  expect(ran).toBe(true);
  expect(leftovers(profiles)).toEqual([]);
});

test('a takeover waits while another takeover holds the claim', async () => {
  const { profiles, registry } = setup();
  const pid = await deadPid();
  const lock = plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  const claim = plantLock(
    profiles,
    { pid: process.pid, startedAt: new Date().toISOString() },
    0,
    '.transaction-lock.takeover'
  );
  await expect(registry.withLock(async () => undefined)).rejects.toMatchObject({ code: 'busy' });
  expect(fs.readdirSync(lock)).toEqual(['holder.json']);
  expect(fs.readdirSync(claim)).toEqual(['holder.json']);
  expect(leftovers(profiles).sort()).toEqual(['.transaction-lock', '.transaction-lock.takeover']);
});

test('an abandoned takeover claim is cleared and the stale lock is then recovered', async () => {
  const { profiles, registry } = setup();
  const pid = await deadPid();
  const lock = plantLock(profiles, { pid, startedAt: new Date().toISOString() });
  plantLock(
    profiles,
    { pid, startedAt: new Date().toISOString() },
    0,
    '.transaction-lock.takeover'
  );
  let ran = false;
  const warnings = await withCapturedWarnings(() =>
    registry.withLock(async () => {
      ran = true;
    })
  );
  expect(ran).toBe(true);
  expect(warnings.length).toBe(2);
  expect(warnings[0]).toContain('cleared an abandoned lock takeover claim');
  expect(warnings[1]).toContain('recovered a stale transaction lock');
  expect(fs.existsSync(lock)).toBe(false);
  expect(leftovers(profiles)).toEqual([]);
});

test('a read-only registry never creates a folder and never locks', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-lock-'));
  temporary.push(directory);
  const ccs = path.join(directory, '.ccs');
  expect(AntigravityProfileRegistry.openExisting(ccs)).toBeNull();
  fs.mkdirSync(path.join(ccs, 'antigravity-profiles'), { recursive: true, mode: 0o700 });
  fs.chmodSync(ccs, 0o700);
  const registry = AntigravityProfileRegistry.openExisting(ccs);
  expect(registry).not.toBeNull();
  expect(registry!.listProfiles()).toEqual([]);
  expect(registry!.lockHeldLive()).toBe(false);
  expect(fs.existsSync(path.join(ccs, 'antigravity-instances'))).toBe(false);
  await expect(registry!.withLock(async () => undefined)).rejects.toMatchObject({
    code: 'unsafe',
  });
  expect(fs.readdirSync(path.join(ccs, 'antigravity-profiles'))).toEqual([]);
});

test('a holder refreshes its own record while it holds the lock', async () => {
  const { profiles } = setup();
  const registry = new AntigravityProfileRegistry(path.dirname(profiles), { heartbeatMs: 10 });
  const holder = path.join(profiles, '.transaction-lock', 'holder.json');
  let silent = 0;
  let refreshed = 0;
  await registry.withLock(async () => {
    const old = new Date(Date.now() - STALE_AFTER_MS);
    fs.utimesSync(holder, old, old);
    silent = fs.statSync(holder).mtimeMs;
    await new Promise((resolve) => setTimeout(resolve, 80));
    refreshed = fs.statSync(holder).mtimeMs;
    expect(registry.lockHeldLive()).toBe(true);
  });
  expect(refreshed - silent).toBeGreaterThan(STALE_AFTER_MS - 60_000);
  expect(leftovers(profiles)).toEqual([]);
});
