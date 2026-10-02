import { randomBytes } from 'crypto';
import { constants, type Stats } from 'fs';
import fs from 'fs/promises';
import path from 'path';

/**
 * Small private JSON stores under the CCS directory
 * (CONTRACT-registry-lifecycle section 1, rule 6):
 * - readers refuse a symlink, a non-regular file, a file over its size cap or
 *   one with any group or other permission bit, and report it as `invalid`,
 *   never as empty;
 * - writers run under one in-process mutex per file and replace the file
 *   atomically: a temporary file in the same directory, mode 0600, then
 *   rename. Missing directories are created at 0700.
 */
export type PrivateJsonRead =
  | { state: 'absent' }
  | { state: 'invalid' }
  | { state: 'ok'; value: unknown; contents: string };

const locks = new Map<string, Promise<void>>();

/** Run `task` after every earlier task for the same file has settled. */
export function withPrivateFileLock<T>(file: string, task: () => Promise<T>): Promise<T> {
  const key = path.resolve(file);
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

export async function readPrivateJsonFile(
  file: string,
  maxBytes: number
): Promise<PrivateJsonRead> {
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

/** Atomic replacement; call inside `withPrivateFileLock` for the same file. */
export async function writePrivateJsonFile(file: string, value: unknown): Promise<void> {
  const directory = path.dirname(file);
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  const temporary = path.join(
    directory,
    `.${path.basename(file)}.${randomBytes(8).toString('hex')}.tmp`
  );
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    handle = await fs.open(temporary, 'wx', 0o600);
    await handle.writeFile(`${JSON.stringify(value, null, 2)}\n`, 'utf8');
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = undefined;
    await fs.rename(temporary, file);
  } finally {
    await handle?.close().catch(() => undefined);
    await fs.rm(temporary, { force: true }).catch(() => undefined);
  }
}
