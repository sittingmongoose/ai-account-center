import { AntigravityError } from '../errors';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { randomBytes } from 'node:crypto';
import {
  defaultAntigravityAutoSwitchState,
  validateAntigravityAutoSwitchStoredState,
} from './settings';
import type { AntigravityAutoSwitchStore, AntigravityAutoSwitchStoredState } from './types';

/**
 * Explicit own private directory, never a config-directory resolver or process HOME.
 * Windows controllers access the Ubuntu API; this file store is Ubuntu-only.
 */
export class AntigravityAutoSwitchFileStore implements AntigravityAutoSwitchStore {
  readonly file: string;
  private readonly directory: string;

  constructor(directory: string) {
    if (!path.isAbsolute(directory))
      throw new AntigravityError('Antigravity switching state directory must be absolute.');
    this.directory = path.resolve(directory);
    this.file = path.join(this.directory, 'antigravity-auto-switch.json');
  }

  private openDirectory(create: boolean): number | null {
    if (process.platform !== 'linux')
      throw new AntigravityError('Antigravity automatic switching state is managed on Ubuntu.');
    let stat: fs.Stats;
    try {
      stat = fs.lstatSync(this.directory);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      if (!create) return null;
      // Parent must already exist; do not traverse/create unknown ancestor trees.
      if (fs.realpathSync(path.dirname(this.directory)) !== path.dirname(this.directory))
        throw new AntigravityError('Antigravity switching parent path is not canonical.');
      fs.mkdirSync(this.directory, { mode: 0o700 });
      stat = fs.lstatSync(this.directory);
    }
    if (
      !stat.isDirectory() ||
      stat.isSymbolicLink() ||
      (stat.mode & 0o077) !== 0 ||
      (typeof process.getuid === 'function' && stat.uid !== process.getuid())
    )
      throw new AntigravityError('Antigravity switching state directory is not private.');
    if (fs.realpathSync(this.directory) !== this.directory)
      throw new AntigravityError('Antigravity switching state path is not canonical.');
    const descriptor = fs.openSync(
      this.directory,
      fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW
    );
    const actual = fs.fstatSync(descriptor);
    if (
      actual.dev !== stat.dev ||
      actual.ino !== stat.ino ||
      !actual.isDirectory() ||
      (actual.mode & 0o077) !== 0 ||
      (typeof process.getuid === 'function' && actual.uid !== process.getuid())
    ) {
      fs.closeSync(descriptor);
      throw new AntigravityError('Antigravity switching state directory changed during open.');
    }
    return descriptor;
  }

  private assertFile(file: string): fs.Stats | null {
    let stat: fs.Stats;
    try {
      stat = fs.lstatSync(file);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
      throw error;
    }
    if (
      !stat.isFile() ||
      stat.isSymbolicLink() ||
      stat.nlink !== 1 ||
      (stat.mode & 0o077) !== 0 ||
      stat.size > 16_384 ||
      (typeof process.getuid === 'function' && stat.uid !== process.getuid())
    )
      throw new AntigravityError('Antigravity switching settings file is not private.');
    return stat;
  }

  read(): AntigravityAutoSwitchStoredState {
    const directoryDescriptor = this.openDirectory(false);
    if (directoryDescriptor === null) return defaultAntigravityAutoSwitchState();
    try {
      const file = `/proc/self/fd/${directoryDescriptor}/antigravity-auto-switch.json`;
      const expected = this.assertFile(file);
      if (!expected) return defaultAntigravityAutoSwitchState();
      const descriptor = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
      try {
        const actual = fs.fstatSync(descriptor);
        if (!sameFile(expected, actual))
          throw new AntigravityError('Antigravity switching settings changed during read.');
        const content = fs.readFileSync(descriptor, 'utf8');
        if (!sameFile(actual, fs.fstatSync(descriptor)) || Buffer.byteLength(content) > 16_384)
          throw new AntigravityError('Antigravity switching settings changed during read.');
        return validateAntigravityAutoSwitchStoredState(JSON.parse(content));
      } finally {
        fs.closeSync(descriptor);
      }
    } finally {
      fs.closeSync(directoryDescriptor);
    }
  }

  write(value: AntigravityAutoSwitchStoredState): void {
    const state = validateAntigravityAutoSwitchStoredState(value);
    const directoryDescriptor = this.openDirectory(true);
    if (directoryDescriptor === null)
      throw new AntigravityError('Antigravity switching settings directory is unavailable.');
    const directoryIdentity = fs.fstatSync(directoryDescriptor);
    const anchor = `/proc/self/fd/${directoryDescriptor}`;
    const file = `${anchor}/antigravity-auto-switch.json`;
    const temporary = `${anchor}/.antigravity-auto-switch.${randomBytes(12).toString('hex')}.tmp`;
    let createdTemporary = false;
    try {
      const previous = this.assertFile(file);
      const descriptor = fs.openSync(
        temporary,
        fs.constants.O_WRONLY |
          fs.constants.O_CREAT |
          fs.constants.O_EXCL |
          fs.constants.O_NOFOLLOW,
        0o600
      );
      createdTemporary = true;
      try {
        fs.writeFileSync(descriptor, `${JSON.stringify(state, null, 2)}\n`, 'utf8');
        fs.fsyncSync(descriptor);
      } finally {
        fs.closeSync(descriptor);
      }
      const currentDirectory = fs.lstatSync(this.directory);
      if (
        !currentDirectory.isDirectory() ||
        currentDirectory.isSymbolicLink() ||
        currentDirectory.dev !== directoryIdentity.dev ||
        currentDirectory.ino !== directoryIdentity.ino ||
        (currentDirectory.mode & 0o077) !== 0 ||
        (typeof process.getuid === 'function' && currentDirectory.uid !== process.getuid())
      ) {
        throw new AntigravityError('Antigravity switching state directory changed during write.');
      }
      const latest = this.assertFile(file);
      if (
        (previous === null) !== (latest === null) ||
        (previous && latest && !sameFile(previous, latest))
      ) {
        throw new AntigravityError('Antigravity switching settings changed during write.');
      }
      // Both paths are anchored to the opened owned directory, so a replacement
      // of its pathname can never redirect the write/cleanup into a foreign tree.
      fs.renameSync(temporary, file);
      createdTemporary = false;
      fs.fsyncSync(directoryDescriptor);
    } finally {
      try {
        if (createdTemporary) {
          try {
            fs.unlinkSync(temporary);
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
          }
        }
      } finally {
        fs.closeSync(directoryDescriptor);
      }
    }
  }
}

function sameFile(left: fs.Stats, right: fs.Stats): boolean {
  return (
    left.dev === right.dev &&
    left.ino === right.ino &&
    left.size === right.size &&
    left.mtimeMs === right.mtimeMs &&
    left.ctimeMs === right.ctimeMs &&
    left.mode === right.mode &&
    left.nlink === right.nlink &&
    left.uid === right.uid
  );
}
