/**
 * One writer for the answer store: the process that records answers holds `<store>/answers.lock` for its lifetime,
 * so no other process appends to `answers.jsonl` (a failed write's cut-back would otherwise remove a record another
 * process had just acknowledged) or creates or repairs the answer key at the same time. Within the process every
 * write is synchronous, so its own instances never interleave.
 *
 * The lock names its holder (pid, host, time). A lock is taken over when its holder is gone: a process on this host
 * that no longer runs, a lock written before this computer last started, or one with no readable holder after
 * STALE_UNREADABLE_MS (a writer stopped between creating and filling it). A holder on another host is never judged
 * gone, since its processes can't be seen from here. A take-over moves the stale lock aside and checks it moved the
 * one it judged; a live lock moved by mistake is put back.
 */
import { randomBytes } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { ConfigError } from '../../errors/error-types';

export const WRITER_LOCK_FILE = 'answers.lock';
const STALE_UNREADABLE_MS = 10_000;
/** Clock slack when comparing a lock's time with this computer's start. */
const BOOT_SLACK_MS = 60_000;

export class WriterBusyError extends ConfigError {
  constructor(readonly holder: string) {
    super(`Another AI Account Center process (${holder}) records answers from this folder.`);
    this.name = 'WriterBusyError';
  }
}

interface Holder {
  pid: number;
  host: string;
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
      ? { pid: value.pid as number, host: value.host }
      : null;
  } catch {
    return null;
  }
}

function running(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM: the process runs under another user
    return errorCode(error) === 'EPERM';
  }
}

function stale(holder: Holder | null, mtimeMs: number): boolean {
  if (holder === null) return Date.now() - mtimeMs > STALE_UNREADABLE_MS;
  if (holder.host !== os.hostname()) return false;
  const bootedAt = Date.now() - os.uptime() * 1000;
  return mtimeMs < bootedAt - BOOT_SLACK_MS || !running(holder.pid);
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
  try {
    if (readText(aside) !== judged) {
      try {
        fs.linkSync(aside, file);
      } catch {
        /* another process took the name meanwhile; it holds the lock now */
      }
    }
  } finally {
    fs.rmSync(aside, { force: true });
  }
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
  if (mine !== undefined && readText(file) === mine) return;
  held.delete(file);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const text = JSON.stringify({ pid: process.pid, host: os.hostname(), at: Date.now() });
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
