import { randomUUID } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';

const MAX_BYTES = 256 * 1024;
function owner(stat: fs.Stats): boolean {
  return (
    process.platform === 'win32' || (stat.uid === process.getuid?.() && (stat.mode & 0o077) === 0)
  );
}
function directory(stat: fs.Stats): boolean {
  return stat.isDirectory() && !stat.isSymbolicLink() && owner(stat);
}
function regular(stat: fs.Stats): boolean {
  return (
    stat.isFile() &&
    !stat.isSymbolicLink() &&
    stat.nlink === 1 &&
    stat.size <= MAX_BYTES &&
    owner(stat)
  );
}
function sameFile(a: fs.Stats, b: fs.Stats): boolean {
  return a.ino === b.ino && a.dev === b.dev;
}

/** Private observation data is bounded and cannot follow links or another owner's cache. */
export function readUsageCache(file: string): unknown {
  let descriptor: number | undefined;
  try {
    const before = fs.lstatSync(path.dirname(file));
    if (!directory(before)) return null;
    const link = fs.lstatSync(file);
    if (!regular(link)) return null;
    descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    const stat = fs.fstatSync(descriptor);
    if (!regular(stat) || !sameFile(link, stat)) return null;
    const buffer = Buffer.alloc(MAX_BYTES + 1);
    const length = fs.readSync(descriptor, buffer, 0, buffer.length, 0);
    const after = fs.lstatSync(path.dirname(file));
    if (length > MAX_BYTES || !directory(after) || !sameFile(before, after)) return null;
    return JSON.parse(buffer.subarray(0, length).toString('utf8'));
  } catch {
    return null;
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
}

export function writeUsageCache(file: string, value: unknown): void {
  const temporary = `${file}.${randomUUID()}.tmp`;
  const safeDestination = () => {
    try {
      return regular(fs.lstatSync(file));
    } catch (error) {
      return (error as NodeJS.ErrnoException).code === 'ENOENT';
    }
  };
  try {
    const payload = JSON.stringify(value);
    if (Buffer.byteLength(payload) > MAX_BYTES) return;
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const before = fs.lstatSync(path.dirname(file));
    if (!directory(before) || !safeDestination()) return;
    fs.writeFileSync(temporary, payload, { mode: 0o600, flag: 'wx' });
    const after = fs.lstatSync(path.dirname(file));
    if (!directory(after) || !sameFile(before, after) || !safeDestination()) return;
    fs.renameSync(temporary, file);
  } catch {
    // A read-only disk does not discard the verified in-memory observation.
  } finally {
    try {
      fs.unlinkSync(temporary);
    } catch {
      // Renamed or never created.
    }
  }
}
