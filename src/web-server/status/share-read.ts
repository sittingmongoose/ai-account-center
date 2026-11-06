/**
 * Opening files on the reports share without trusting what is there. A writer can leave a FIFO
 * where a report belongs: opening one blocks a libuv thread until something writes to it, and a
 * few of those stop every file read in the server. So nothing here opens first and asks later:
 * `O_NONBLOCK` makes the open return at once, `O_NOFOLLOW` refuses a link, and the handle itself is
 * checked to be a regular file before a byte is read.
 */
import fs from 'fs';

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
