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
 *   when it was written in an earlier boot or its pid now belongs to a process that started later (a reused number).
 *   Nothing is judged by the clock:
 *   a stalled holder or a clock correction never loses its lock;
 * - from another host (a replaced container on the same volume), whose processes can't be seen from here, when no one
 *   has refreshed it for UNREFRESHED_MS;
 * - with no readable holder, after STALE_UNREADABLE_MS (a writer stopped between creating and filling it).
 * Take-overs happen one at a time, under `<lock>.taking` (see takeOver), and a process that gains the lock reloads
 * the store before it judges anything. Every write checks the lock still names this process. Known limits: outside Linux, a crashed holder's pid
 * reused by a long-lived process keeps the lock until that process ends (deleting `answers.lock` frees it at once);
 * two processes clearing the same ended taker's `.taking` at one moment can let two take-overs overlap.
 */
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
  start: string | null;
}

/** When a process started, in clock ticks since boot, on Linux (so a reused pid is told apart); null elsewhere. */
function processStart(pid: number): string | null {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    // the fields after the name (which may hold spaces and parentheses) start with the state, field 3
    return stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19] ?? null;
  } catch {
    return null;
  }
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
          start: typeof value.start === 'string' ? value.start : null,
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
  if (!running(holder.pid)) return true;
  // the same number, but a process that started later: the holder ended and its pid was reused
  const start = processStart(holder.pid);
  return start !== null && holder.start !== null && start !== holder.start;
}

/** Creates `file` holding `text`, only if nothing has that name; false when something does. */
function createExclusive(file: string, text: string): boolean {
  let fd: number;
  try {
    fd = fs.openSync(file, 'wx', 0o600);
  } catch (error) {
    if (errorCode(error) === 'EEXIST') return false;
    throw error;
  }
  try {
    fs.writeFileSync(fd, text);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
  return true;
}

/** This process as a lock names it: pid, host, Linux boot id and start time. */
function selfText(): string {
  return JSON.stringify({
    pid: process.pid,
    host: os.hostname(),
    boot: bootId(),
    start: processStart(process.pid),
    at: Date.now(),
  });
}

/** Whether a holder is this very process: its pid on this host, in this boot, started when this one did. */
function isSelf(holder: Holder | null): boolean {
  return (
    holder !== null &&
    holder.pid === process.pid &&
    holder.host === os.hostname() &&
    (holder.boot === null || holder.boot === bootId()) &&
    (holder.start === null || holder.start === processStart(process.pid))
  );
}

/** The text and time of `file`, or null when it is gone. */
function readLock(file: string): { text: string; mtimeMs: number } | null {
  try {
    const text = readText(file);
    return text === null ? null : { text, mtimeMs: fs.statSync(file).mtimeMs };
  } catch (error) {
    if (errorCode(error) === 'ENOENT') return null;
    throw error;
  }
}

/**
 * Removes the stale lock `judged`, one take-over at a time: under `<lock>.taking`, created exclusively, no other
 * process removes or replaces the lock, and plain acquisition only fills an empty name, so the lock is removed only
 * if it is still the one judged. A taker that ended mid-way leaves its `.taking` behind, which is judged like a lock.
 * Returns false when another live process is taking over (the store is busy for now).
 */
function takeOver(file: string, judged: string): boolean {
  const taking = `${file}.taking`;
  const mine = selfText();
  if (!createExclusive(taking, mine)) {
    const other = readLock(taking);
    if (other === null) return true;
    if (!stale(parseHolder(other.text), other.mtimeMs)) return false;
    // a taker that ended mid-way: its marker goes, if it is still the one judged (a known limit: two processes
    // clearing the same ended taker at the same moment)
    if (readText(taking) === other.text) fs.rmSync(taking, { force: true });
    return true;
  }
  try {
    if (readText(file) === judged) fs.rmSync(file, { force: true });
  } finally {
    if (readText(taking) === mine) fs.rmSync(taking, { force: true });
  }
  return true;
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
 * Makes sure this process holds the answer store's writer lock in `dir`, taking it if it is free or stale. Returns
 * true when it was not held a moment ago (whatever this process remembered of the store may be stale), false when it
 * still was. Throws WriterBusyError when another live process holds it or is taking it over.
 */
export function holdWriterLock(dir: string): boolean {
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
    return false;
  }
  held.delete(file);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const text = selfText();
    if (createExclusive(file, text)) {
      held.set(file, text);
      releaseAtExit();
      return true;
    }
    const found = readLock(file);
    if (found === null) continue;
    const holder = parseHolder(found.text);
    // another instance in this very process wrote it (a restart within one process, or tests): it is ours
    if (isSelf(holder)) {
      held.set(file, found.text);
      releaseAtExit();
      return true;
    }
    if (!stale(holder, found.mtimeMs) || !takeOver(file, found.text))
      throw new WriterBusyError(holder ? `pid ${holder.pid} on ${holder.host}` : 'a process');
  }
  throw new WriterBusyError('a process');
}
