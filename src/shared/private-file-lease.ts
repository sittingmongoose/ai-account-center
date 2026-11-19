import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const binaries = new Map<string, string>();
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
export function claimFileLease(root: string, file: string): () => void {
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
    return () => fs.rmdirSync(file);
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
    return () => fs.closeSync(fd);
  } finally {
    if (!locked) fs.closeSync(fd);
  }
}
