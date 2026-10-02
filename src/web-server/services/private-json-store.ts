import { randomBytes } from 'crypto';
import { constants, realpathSync, type Stats } from 'fs';
import fs from 'fs/promises';
import path from 'path';
import { ConfigError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';

/**
 * Small private JSON stores under the CCS directory
 * (CONTRACT-registry-lifecycle section 1, rule 6):
 * - readers refuse a symlink, a non-regular file, a file over its size cap or
 *   one with any group or other permission bit, and report it as `invalid`,
 *   never as empty;
 * - the folder is checked on every read and write: a folder that is not a
 *   directory or that others can write is refused (`invalid` on read, an error
 *   on write). A group-writable or symlinked folder is accepted with one fixed
 *   warning, because Ubuntu's private-group umask (002) makes `~/.ccs` 0775 on
 *   ordinary installs and refusing it would hide every account;
 * - writers run under one in-process mutex per file, keyed on the real path of
 *   its folder, and replace the file atomically: a temporary file in the same
 *   directory, mode 0600, fsync, rename, then a best-effort fsync of the
 *   folder. Missing directories are created at 0700.
 */
export type PrivateJsonRead =
  | { state: 'absent' }
  | { state: 'invalid' }
  | { state: 'ok'; value: unknown; contents: string };

const logger = createLogger('private-json-store');
const locks = new Map<string, Promise<void>>();
const warned = new Set<string>();

/**
 * The lock key: the real path of the nearest existing folder plus the rest,
 * so the same file reached through a symlinked CCS_HOME shares one mutex.
 */
export function privateFileLockKey(file: string): string {
  let directory = path.dirname(path.resolve(file));
  const rest = [path.basename(file)];
  for (;;) {
    try {
      return path.join(realpathSync(directory), ...rest);
    } catch {
      const parent = path.dirname(directory);
      if (parent === directory) return path.resolve(file);
      rest.unshift(path.basename(directory));
      directory = parent;
    }
  }
}

/** Run `task` after every earlier task for the same file has settled. */
export function withPrivateFileLock<T>(file: string, task: () => Promise<T>): Promise<T> {
  const key = privateFileLockKey(file);
  const previous = locks.get(key) ?? Promise.resolve();
  const result = previous.then(task);
  const tail = result.then(
    () => undefined,
    () => undefined
  );
  locks.set(key, tail);
  void tail.then(() => {
    if (locks.get(key) === tail) locks.delete(key);
  });
  return result;
}

function isPrivate(stat: Stats): boolean {
  // Windows reports synthetic permission bits; ownership is enforced by the profile ACL there.
  return process.platform === 'win32' || (stat.mode & 0o077) === 0;
}

/** One fixed line per folder and kind; the folder path is never logged (rule 7). */
function warnOnce(directory: string, kind: 'group-writable' | 'symlink'): void {
  const key = `${kind}\0${directory}`;
  if (warned.has(key)) return;
  warned.add(key);
  try {
    logger.warn(
      'private-store.folder',
      kind === 'symlink'
        ? 'A private account store folder is a symbolic link; its target is checked instead.'
        : 'A private account store folder is writable by its group; set it to 0700.'
    );
  } catch {
    /* Logging is best effort. */
  }
}

/**
 * The store folder: `absent`, `unsafe` (not a directory, or writable by
 * others) or `ok`. Windows has no POSIX bits; the profile ACL applies there.
 */
async function checkStoreFolder(directory: string): Promise<'ok' | 'absent' | 'unsafe'> {
  if (process.platform === 'win32') return 'ok';
  let stat: Stats;
  try {
    stat = await fs.lstat(directory);
    if (stat.isSymbolicLink()) {
      warnOnce(directory, 'symlink');
      stat = await fs.stat(directory);
    }
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT' ? 'absent' : 'unsafe';
  }
  if (!stat.isDirectory() || (stat.mode & 0o002) !== 0) return 'unsafe';
  if ((stat.mode & 0o020) !== 0) warnOnce(directory, 'group-writable');
  return 'ok';
}

export async function readPrivateJsonFile(
  file: string,
  maxBytes: number
): Promise<PrivateJsonRead> {
  if ((await checkStoreFolder(path.dirname(file))) === 'unsafe') return { state: 'invalid' };
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    const link = await fs.lstat(file);
    if (link.isSymbolicLink() || !link.isFile()) return { state: 'invalid' };
    const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
    handle = await fs.open(file, constants.O_RDONLY | noFollow);
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > maxBytes || !isPrivate(stat)) return { state: 'invalid' };
    const buffer = Buffer.alloc(maxBytes + 1);
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
    if (bytesRead > maxBytes) return { state: 'invalid' };
    const contents = buffer.subarray(0, bytesRead).toString('utf8');
    return { state: 'ok', value: JSON.parse(contents) as unknown, contents };
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT'
      ? { state: 'absent' }
      : { state: 'invalid' };
  } finally {
    await handle?.close().catch(() => undefined);
  }
}

/** Best effort: makes the rename durable where the platform can sync a folder. */
async function syncFolder(directory: string): Promise<void> {
  if (process.platform === 'win32') return;
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    handle = await fs.open(directory, constants.O_RDONLY);
    await handle.sync();
  } catch {
    /* Some file systems cannot sync a folder; the file itself was synced. */
  } finally {
    await handle?.close().catch(() => undefined);
  }
}

/** Atomic replacement; call inside `withPrivateFileLock` for the same file. */
export async function writePrivateJsonFile(file: string, value: unknown): Promise<void> {
  await writePrivateTextFile(file, `${JSON.stringify(value, null, 2)}\n`);
}

/**
 * Atomic 0600 replacement of a small private text file: a new temporary file
 * with a random name (`wx`, so a planted file or symlink is never followed),
 * fsync, rename, then a best-effort fsync of the folder.
 */
export async function writePrivateTextFile(file: string, contents: string): Promise<void> {
  const directory = path.dirname(file);
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  if ((await checkStoreFolder(directory)) !== 'ok') {
    throw new ConfigError('The private store folder is not safe to write.');
  }
  const temporary = path.join(
    directory,
    `.${path.basename(file)}.${randomBytes(8).toString('hex')}.tmp`
  );
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    handle = await fs.open(temporary, 'wx', 0o600);
    await handle.writeFile(contents, 'utf8');
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = undefined;
    await fs.rename(temporary, file);
    await syncFolder(directory);
  } finally {
    await handle?.close().catch(() => undefined);
    await fs.rm(temporary, { force: true }).catch(() => undefined);
  }
}
