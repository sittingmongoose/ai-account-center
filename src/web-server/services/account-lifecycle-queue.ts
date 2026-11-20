/**
 * FIFO in the compatibility front door. Work already in the queue keeps its
 * turn across writer adoption. Rust takes the same workflow lease as the
 * native provider actor while the effect runs here.
 */
import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import fs from 'node:fs/promises';
import path from 'node:path';
import { ConfigError } from '../../errors/error-types';
import { activeFileWriterBinary, canonicalLeaseRoot } from '../../shared/private-file-lease';
import { withPrivateFileLock } from './private-json-store';

export function serialized<T>(root: string, name: string, task: () => Promise<T>): Promise<T> {
  const canonical = canonicalLeaseRoot(root);
  const leaf = name.replace(/^key:/, 'key-');
  if (!/^(claude-purge|key-[a-z0-9-]{1,32})$/.test(leaf))
    return Promise.reject(new ConfigError('Invalid lifecycle queue.'));
  const directory = path.join(canonical, 'run');
  const marker = path.join(directory, 'lifecycle-' + leaf + '.lock');
  return withPrivateFileLock(marker, async () => {
    const binary = activeFileWriterBinary(canonical);
    if (!binary) return task();

    const { O_RDWR, O_CREAT, O_NOFOLLOW, O_NONBLOCK } = constants;
    try {
      await fs.mkdir(directory, { mode: 0o700 });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
    }
    const run = await fs.lstat(directory);
    if (!run.isDirectory() || (run.mode & 0o777) !== 0o700 || run.uid !== process.getuid?.())
      throw new ConfigError('Invalid lifecycle queue directory.');
    const lease = await fs.open(marker, O_RDWR | O_CREAT | O_NOFOLLOW | O_NONBLOCK, 0o600);
    try {
      await new Promise<void>((resolve, reject) => {
        const child = spawn(binary, ['--lifecycle-queue', canonical, name], {
          windowsHide: true,
          stdio: [lease.fd, 'ignore', 'ignore'],
        });
        const failed = () => reject(new ConfigError('The lifecycle queue could not be acquired.'));
        child.once('error', failed);
        child.once('exit', (code) => (code === 0 ? resolve() : failed()));
      });
      return await task();
    } finally {
      await lease.close();
    }
  });
}

export function keyQueue(provider: string): string {
  return 'key:' + provider;
}
