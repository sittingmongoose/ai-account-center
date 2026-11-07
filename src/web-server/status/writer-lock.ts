/**
 * One writer for the answer store: the process that records answers holds `<store>/answers.lock` for its lifetime,
 * so a second dashboard on the same folder records nothing (no doubled history, rings or share writes) and no two
 * processes create or repair the answer key at the same time. Within the process every write is synchronous, so its
 * own instances never interleave. The store itself is append-only (answers.ts), so even two writers at once, should
 * this lock ever be taken by mistake, can't remove or damage each other's records.
 *
 * The lock names its holder (pid, host, Linux boot id, time), and the holder refreshes its time stamp on every write
 * and every retry pass (each minute). A lock is taken over only when its holder is gone:
 * - on this host, when its pid no longer runs (a zombie, ended but not yet reaped, counts as ended), or, on Linux,
 *   when it was written in an earlier boot (its pid belongs to another process now). Nothing is judged by the clock:
 *   a stalled holder or a clock correction never loses its lock;
 * - from another host (a replaced container on the same volume), whose processes can't be seen from here, when no one
 *   has refreshed it for UNREFRESHED_MS;
 * - with no readable holder, after STALE_UNREADABLE_MS (a writer stopped between creating and filling it).
 * A take-over moves the stale lock aside and checks it moved the one it judged; a live lock moved by mistake is put
 * back. Every write checks the lock still names this process. Known limits: outside Linux, a crashed holder's pid
 * reused by a long-lived process keeps the lock until that process ends (deleting `answers.lock` frees it at once);
 * three processes racing on one stale lock can, for a moment, leave two that each believe they hold it.
 */
import { randomBytes } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { ConfigError } from '../../errors/error-types';

export const WRITER_LOCK_FILE = 'answers.lock';
const STALE_UNREADABLE_MS = 10_000;
/** Another host's lock its holder has not refreshed for this long is gone (it refreshes it each minute). */
const UNREFRESHED_MS = 10 * 60 * 1000;

export class WriterBusyError extends ConfigError {
  constructor(readonly holder: string) {
    super(`Another AI Account Center process (${holder}) records answers from this folder.`);
    this.name = 'WriterBusyError';
  }
}

interface Holder {
  pid: number;
  host: string;
  boot: string | null;
}

/** This boot's id on Linux (it changes at every start, whatever the clock says); null elsewhere. */
function bootId(): string | null {
  try {
    return fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8').trim() || null;
  } catch {
    return null;
  }
}

/** Lock files this process holds, with the text it wrote into each. */
const held = new Map<string, string>();
let releaseRegistered = false;

function errorCode(error: unknown): string | undefined {
  return (error as NodeJS.ErrnoException | null)?.code;
}

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8');
  } catch (error) {
    if (errorCode(error) === 'ENOENT') return null;
    throw error;
  }
}

function parseHolder(text: string): Holder | null {
  try {
    const value = JSON.parse(text) as Partial<Holder>;
    return Number.isSafeInteger(value.pid) && typeof value.host === 'string'
      ? {
          pid: value.pid as number,
          host: value.host,
          boot: typeof value.boot === 'string' ? value.boot : null,
        }
      : null;
  } catch {
    return null;
  }
}

function running(pid: number): boolean {
  try {
    process.kill(pid, 0);
  } catch (error) {
    // EPERM: the process runs under another user
    return errorCode(error) === 'EPERM';
  }
  // a zombie (ended, not yet reaped by its parent) still answers kill(0): on Linux its state says so
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    const state = stat.charAt(stat.lastIndexOf(')') + 2);
    if (state === 'Z' || state === 'X') return false;
  } catch {
    /* no /proc: kill(0) is all there is */
  }
  return true;
}

function stale(holder: Holder | null, mtimeMs: number): boolean {
  if (holder === null) return Date.now() - mtimeMs > STALE_UNREADABLE_MS;
  if (holder.host !== os.hostname()) return Date.now() - mtimeMs > UNREFRESHED_MS;
  const boot = bootId();
  if (boot !== null && holder.boot !== null && holder.boot !== boot) return true;
  return !running(holder.pid);
}

/** Moves a stale lock aside, and puts it back if what moved was not the lock judged stale. */
function takeOver(file: string, judged: string): void {
  const aside = `${file}.${process.pid}.${randomBytes(6).toString('hex')}.stale`;
  try {
    fs.renameSync(file, aside);
  } catch (error) {
    if (errorCode(error) === 'ENOENT') return;
    throw error;
  }
  if (readText(aside) !== judged) {
    try {
      fs.linkSync(aside, file);
    } catch (error) {
      // EEXIST: another process took the name meanwhile and holds the lock now. Without hard links, the moved lock
      // goes back by name while the name is still free.
      if (errorCode(error) !== 'EEXIST' && !fs.existsSync(file)) {
        try {
          fs.renameSync(aside, file);
          return;
        } catch {
          /* its holder takes it again on its next write */
        }
      }
    }
  }
  fs.rmSync(aside, { force: true });
}

function releaseAtExit(): void {
  if (releaseRegistered) return;
  releaseRegistered = true;
  process.once('exit', () => {
    for (const [file, text] of held) {
      try {
        if (fs.readFileSync(file, 'utf8') === text) fs.unlinkSync(file);
      } catch {
        /* best effort: a lock left behind is taken over once this process is gone */
      }
    }
  });
}

/**
 * Makes sure this process holds the answer store's writer lock in `dir`, taking it if it is free or stale. Throws
 * WriterBusyError when another live process holds it.
 */
export function holdWriterLock(dir: string): void {
  const file = path.join(dir, WRITER_LOCK_FILE);
  const mine = held.get(file);
  if (mine !== undefined && readText(file) === mine) {
    // still ours: refresh its time stamp, so it never reads as left behind
    try {
      const now = new Date();
      fs.utimesSync(file, now, now);
    } catch {
      /* best effort */
    }
    return;
  }
  held.delete(file);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const text = JSON.stringify({
      pid: process.pid,
      host: os.hostname(),
      boot: bootId(),
      at: Date.now(),
    });
    try {
      const fd = fs.openSync(file, 'wx', 0o600);
      try {
        fs.writeFileSync(fd, text);
        fs.fsyncSync(fd);
      } finally {
        fs.closeSync(fd);
      }
      held.set(file, text);
      releaseAtExit();
      return;
    } catch (error) {
      if (errorCode(error) !== 'EEXIST') throw error;
    }
    let found: string | null;
    let mtimeMs: number;
    try {
      found = readText(file);
      mtimeMs = fs.statSync(file).mtimeMs;
    } catch (error) {
      if (errorCode(error) === 'ENOENT') continue;
      throw error;
    }
    if (found === null) continue;
    const holder = parseHolder(found);
    // another instance in this process wrote it (a restart within one process, or tests): it is ours
    if (holder && holder.pid === process.pid && holder.host === os.hostname()) {
      held.set(file, found);
      releaseAtExit();
      return;
    }
    if (!stale(holder, mtimeMs))
      throw new WriterBusyError(holder ? `pid ${holder.pid} on ${holder.host}` : 'a process');
    takeOver(file, found);
  }
  throw new WriterBusyError('a process');
}
