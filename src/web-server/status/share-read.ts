/**
 * Opening files on the reports share without trusting what is there. A writer can leave a FIFO
 * where a report belongs: opening one blocks a libuv thread until something writes to it, and a
 * few of those stop every file read in the server. So nothing here opens first and asks later:
 * `O_NONBLOCK` makes the open return at once, `O_NOFOLLOW` refuses a link, and the handle itself is
 * checked to be a regular file before a byte is read.
 */
import fs from 'fs';
import path from 'path';

const OPEN_FLAGS =
  fs.constants.O_RDONLY | (fs.constants.O_NONBLOCK ?? 0) | (fs.constants.O_NOFOLLOW ?? 0);
const CHUNK_BYTES = 64 * 1024;

/** The path names something other than a regular file. Not a transient share error. */
export class NotRegularFileError extends Error {
  readonly code = 'ENOTREG';
  constructor() {
    super('not a regular file');
    this.name = 'NotRegularFileError';
  }
}

/** A handle on a regular file with its size; the caller closes it. */
export async function openRegularFile(
  file: string
): Promise<{ handle: fs.promises.FileHandle; stat: fs.Stats }> {
  const handle = await fs.promises.open(file, OPEN_FLAGS);
  try {
    const stat = await handle.stat();
    if (!stat.isFile()) throw new NotRegularFileError();
    return { handle, stat };
  } catch (error) {
    await handle.close().catch(() => undefined);
    throw error;
  }
}

/**
 * Whether an opened file really lies under `folder`, asked of the handle. `O_NOFOLLOW` guards only
 * the last path segment, so a parent folder swapped for a link after a `realpath` check would still
 * open a file elsewhere. Linux names what the descriptor actually holds. Without `/proc` (macOS,
 * Windows) every segment of `file` (the checked real path) is looked at again: no links, and the
 * last must be the opened file. That leaves one race there: a folder swapped out for the open and
 * back again before this look.
 */
export async function openedFileIsUnder(
  handle: fs.promises.FileHandle,
  stat: fs.Stats,
  folder: string,
  file: string,
  fdDir = '/proc/self/fd'
): Promise<boolean> {
  let opened: string;
  try {
    opened = await fs.promises.readlink(path.join(fdDir, String(handle.fd)));
  } catch {
    return walkFindsFile(stat, folder, file);
  }
  return opened.startsWith(folder + path.sep);
}

async function walkFindsFile(stat: fs.Stats, folder: string, file: string): Promise<boolean> {
  try {
    let current = folder;
    let seen = await fs.promises.lstat(current);
    if (seen.isSymbolicLink()) return false;
    for (const segment of path.relative(folder, file).split(path.sep)) {
      current = path.join(current, segment);
      seen = await fs.promises.lstat(current);
      if (seen.isSymbolicLink()) return false;
    }
    return seen.dev === stat.dev && seen.ino === stat.ino;
  } catch {
    return false;
  }
}

/**
 * The file's bytes, at most `capBytes + 1` of them: a file that grew past its cap after the stat
 * comes back over the cap, so the caller's own check refuses it without holding the whole file.
 */
export async function readRegularFile(file: string, capBytes: number): Promise<Buffer> {
  const { handle } = await openRegularFile(file);
  try {
    const limit = capBytes + 1;
    const chunks: Buffer[] = [];
    let total = 0;
    while (total < limit) {
      const chunk = Buffer.allocUnsafe(Math.min(CHUNK_BYTES, limit - total));
      const { bytesRead } = await handle.read(chunk, 0, chunk.length, total);
      if (bytesRead === 0) break;
      chunks.push(chunk.subarray(0, bytesRead));
      total += bytesRead;
    }
    return Buffer.concat(chunks, total);
  } finally {
    await handle.close().catch(() => undefined);
  }
}
