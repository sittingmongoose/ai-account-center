import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const binaries = new Map<string, string>();
const writers = new Map<string, { binary: string; committed: () => void }>();

export function registerFileWriter(
  root: string,
  binary: string,
  committed: () => void
): () => void {
  const writer = { binary: path.resolve(binary), committed };
  writers.set(root, writer);
  return () => {
    if (writers.get(root) === writer) writers.delete(root);
  };
}

export interface FileLease {
  close(): void;
  descriptor?: number;
}

/** Return false only when no native writer was selected or the executable could not launch. */
export function commitWithFileLease(
  root: string,
  id: string,
  lease: FileLease,
  bytes: string
): boolean {
  const writer = writers.get(root);
  if (!writer || lease.descriptor === undefined) return false;
  const result = spawnSync(writer.binary, ['--registered-write', id, root], {
    input: bytes,
    stdio: ['pipe', 'ignore', lease.descriptor],
    timeout: 5000,
    windowsHide: true,
  });
  if (result.status === 0) {
    writer.committed();
    return true;
  }
  if (result.error && (result.error as NodeJS.ErrnoException).code === 'ENOENT') return false;
  throw new Error('The private file could not be saved safely.');
}
const defaultBinary = path.resolve(__dirname, '../native/aac-http');

export function rememberFileLeaseBinary(root: string, binary: string): void {
  binaries.set(path.resolve(root), path.resolve(binary));
}

function executable(file: string): boolean {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

/**
 * Take the same flock as aac-files. The helper locks the descriptor inherited
 * on stdin. The kernel keeps the lock with the open file description, including
 * after the helper exits, until this process closes its descriptor.
 */
export function claimFileLease(root: string, file: string): FileLease {
  const binary =
    binaries.get(path.resolve(root)) ?? process.env.AAC_RUST_SERVICE_BINARY ?? defaultBinary;
  const helper = executable(binary) ? binary : undefined;
  const flock =
    process.platform === 'linux' ? ['/usr/bin/flock', '/bin/flock'].find(executable) : undefined;
  if (!helper && !flock) {
    // A fresh legacy installation can reserve the lock name by mkdir. Rust's
    // open_lock refuses a directory there. Keep an existing inode for flock
    // readers that might already hold it.
    fs.mkdirSync(file, { mode: 0o700 });
    return { close: () => fs.rmdirSync(file) };
  }

  const { O_RDWR, O_CREAT, O_NOFOLLOW, O_NONBLOCK } = fs.constants;
  const fd = fs.openSync(file, O_RDWR | O_CREAT | O_NOFOLLOW | O_NONBLOCK, 0o600);
  let locked = false;
  try {
    const metadata = fs.fstatSync(fd);
    const current = fs.lstatSync(file);
    if (
      !metadata.isFile() ||
      metadata.ino !== current.ino ||
      metadata.dev !== current.dev ||
      (process.getuid && metadata.uid !== process.getuid()) ||
      (metadata.mode & 0o077) !== 0
    )
      throw new Error('Private file lease could not be acquired.');
    const options = { stdio: [fd, 'ignore', 'ignore'] as const, timeout: 500, windowsHide: true };
    const open = helper
      ? spawnSync(helper, ['--file-lease'], { ...options, stdio: [...options.stdio] })
      : null;
    locked =
      open?.status === 0 ||
      (!!flock &&
        open?.status !== 73 &&
        spawnSync(flock, ['-n', '0'], { ...options, stdio: [...options.stdio] }).status === 0);
    if (!locked) throw new Error('Private file lease could not be acquired.');
    const lease: FileLease = {
      descriptor: fd,
      close: () => {
        delete lease.descriptor;
        fs.closeSync(fd);
      },
    };
    return lease;
  } finally {
    if (!locked) fs.closeSync(fd);
  }
}
