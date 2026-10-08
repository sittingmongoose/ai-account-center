import * as fs from 'fs';
import * as path from 'path';
import { randomBytes } from 'crypto';

/** Fixed message: callers write credentials, so no cause or path detail is attached. */
export class CodexAtomicWriteError extends Error {
  constructor() {
    super('Could not atomically save the file.');
    this.name = 'CodexAtomicWriteError';
  }
}

/**
 * Replace a private file in one step: a new 0600 temporary file in the same
 * folder ('wx', so a planted file or link is never followed), fsync, rename,
 * then fsync of the folder. Readers see the old or the new file, never a part.
 */
export function replaceFileAtomically(filePath: string, content: Buffer | string): void {
  const temporary = `${filePath}.tmp.${process.pid}.${randomBytes(8).toString('hex')}`;
  let fd: number | undefined;
  try {
    fd = fs.openSync(temporary, 'wx', 0o600);
    fs.writeFileSync(fd, content);
    fs.fsyncSync(fd);
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(temporary, filePath);
    // Windows cannot open a folder for fsync; the file itself was synced.
    if (process.platform !== 'win32') {
      const directoryFd = fs.openSync(path.dirname(filePath), 'r');
      try {
        fs.fsyncSync(directoryFd);
      } finally {
        fs.closeSync(directoryFd);
      }
    }
  } catch {
    throw new CodexAtomicWriteError();
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
    try {
      fs.unlinkSync(temporary);
    } catch {
      // Successful rename leaves no temporary file.
    }
  }
}
