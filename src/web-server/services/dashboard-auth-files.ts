import { constants, closeSync, fstatSync, lstatSync, openSync, readSync, type Stats } from 'fs';
import fs from 'fs/promises';
import path from 'path';
import { getCcsDir } from '../../config/config-loader-facade';
import { ConfigError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';
import { writePrivateJsonFile } from './private-json-store';

/**
 * Files behind dashboard sign-in (CONTRACT-auth-devices sections 2, 3, 4, 5
 * and 11): `~/.ccs/auth/` (0700) holds `state.json` (the session epoch),
 * `devices.json` (paired trays, token hashes only) and `setup-code`, each 0600
 * and written atomically.
 *
 * Every auth write (these files and `dashboard_auth` in config.yaml) runs
 * through one in-process async gate, so two writers never interleave.
 * Reads are synchronous and small, so the request guard stays synchronous.
 */
const logger = createLogger('dashboard-auth');

export const AUTH_FOLDER_NAME = 'auth';
export const MAX_AUTH_FILE_BYTES = 256 * 1024;

export function authDirectory(): string {
  return path.join(getCcsDir(), AUTH_FOLDER_NAME);
}

export function authFile(name: 'state.json' | 'devices.json' | 'setup-code'): string {
  return path.join(authDirectory(), name);
}

let gate: Promise<void> = Promise.resolve();

/** Run `task` after every earlier auth write has settled (rule 7: one gate for all auth writes). */
export function withAuthWriteGate<T>(task: () => Promise<T> | T): Promise<T> {
  const result = gate.then(task);
  gate = result.then(
    () => undefined,
    () => undefined
  );
  return result;
}

/** Resolves once every auth write queued so far has settled (shutdown and tests). */
export function settleAuthWrites(): Promise<void> {
  return withAuthWriteGate(() => undefined);
}

/** The folder at 0700, created if missing and tightened if looser (this process owns it). */
export async function ensureAuthDirectory(): Promise<string> {
  const directory = authDirectory();
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  if (process.platform !== 'win32') {
    const stat = await fs.lstat(directory);
    if (!stat.isDirectory()) throw new ConfigError('The dashboard auth folder is not a directory.');
    if ((stat.mode & 0o077) !== 0) await fs.chmod(directory, 0o700);
  }
  return directory;
}

/** Atomic 0600 JSON write inside the auth folder; call inside `withAuthWriteGate`. */
export async function writeAuthJson(file: string, value: unknown): Promise<void> {
  await ensureAuthDirectory();
  await writePrivateJsonFile(file, value);
}

export type AuthFileRead =
  | { state: 'absent' }
  | { state: 'invalid' }
  | { state: 'ok'; value: unknown; stamp: string };

function isPrivate(stat: Stats): boolean {
  return process.platform === 'win32' || (stat.mode & 0o077) === 0;
}

const warned = new Set<string>();

function warnOnce(file: string, reason: string): void {
  const key = `${file}\0${reason}`;
  if (warned.has(key)) return;
  warned.add(key);
  try {
    logger.warn('auth.store.refused', 'A dashboard auth file was refused', {
      file: path.basename(file),
      reason,
    });
  } catch {
    /* Logging is best effort. */
  }
}

/** A change stamp: inode, size and modification time, so a cache can tell when the file moved. */
export function authFileStamp(file: string): string | null {
  try {
    const stat = lstatSync(file);
    return `${stat.ino}:${stat.size}:${stat.mtimeMs}`;
  } catch {
    return null;
  }
}

/**
 * Synchronous private read: a symlink, a non-regular file, a file over the cap
 * or one with any group or other permission bit is `invalid` (and warned about
 * once), never empty. A folder others can write is refused too.
 */
export function readAuthJsonSync(file: string): AuthFileRead {
  if (process.platform !== 'win32') {
    try {
      const folder = lstatSync(path.dirname(file));
      if (!folder.isDirectory() || (folder.mode & 0o022) !== 0) {
        warnOnce(file, 'folder');
        return { state: 'invalid' };
      }
    } catch (error) {
      return (error as NodeJS.ErrnoException).code === 'ENOENT'
        ? { state: 'absent' }
        : { state: 'invalid' };
    }
  }
  let descriptor: number | undefined;
  try {
    const link = lstatSync(file);
    if (link.isSymbolicLink() || !link.isFile()) {
      warnOnce(file, 'not-a-file');
      return { state: 'invalid' };
    }
    const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
    descriptor = openSync(file, constants.O_RDONLY | noFollow);
    const stat = fstatSync(descriptor);
    if (!stat.isFile() || stat.size > MAX_AUTH_FILE_BYTES) {
      warnOnce(file, 'size');
      return { state: 'invalid' };
    }
    if (!isPrivate(stat)) {
      warnOnce(file, 'permissions');
      return { state: 'invalid' };
    }
    const buffer = Buffer.alloc(stat.size);
    const bytesRead = stat.size > 0 ? readSync(descriptor, buffer, 0, stat.size, 0) : 0;
    const value = JSON.parse(buffer.subarray(0, bytesRead).toString('utf8')) as unknown;
    return { state: 'ok', value, stamp: `${stat.ino}:${stat.size}:${stat.mtimeMs}` };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return { state: 'absent' };
    warnOnce(file, 'unreadable');
    return { state: 'invalid' };
  } finally {
    if (descriptor !== undefined) {
      try {
        closeSync(descriptor);
      } catch {
        /* Already closed. */
      }
    }
  }
}

/* ------------------------------------------------------------------------ */
/* Test seams: the clock and the bcrypt cost (contract section 12).          */
/* ------------------------------------------------------------------------ */

let clock: () => number = () => Date.now();

export function authNow(): number {
  return clock();
}

/** Tests only: replace the clock; pass null to restore the real one. */
export function setAuthClockForTests(next: (() => number) | null): void {
  clock = next ?? (() => Date.now());
}

/** bcrypt cost for new hashes (contract 3.7: cost 12; tests may lower it). */
let hashCost = 12;

export function passwordHashCost(): number {
  return hashCost;
}

/** Tests only: a lower bcrypt cost for speed; pass null to restore 12. */
export function setPasswordHashCostForTests(cost: number | null): void {
  hashCost = cost ?? 12;
}

export function isoTime(milliseconds: number): string {
  return new Date(milliseconds).toISOString();
}
