import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import {
  ANTIGRAVITY_NATIVE_RELEASED,
  MAX_ANTIGRAVITY_NATIVE_BINARY_BYTES,
  createInstalledAntigravityRuntimeFactory,
  verifyOwnedAntigravityNativePin,
} from '../../../src/antigravity/production-runtime';

// Invented inert bytes only; no actual native executable or user home is read.
const homes: string[] = [];
afterEach(() => {
  for (const directory of homes.splice(0)) fs.rmSync(directory, { recursive: true, force: true });
});

function fixture() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aic-native-pin-fixture-'));
  homes.push(directory);
  fs.chmodSync(directory, 0o700);
  const binary = path.join(directory, 'invented-owned-executable');
  const raw = Buffer.alloc(130 * 1024, 0x5a);
  fs.writeFileSync(binary, raw, { mode: 0o700 });
  const hash = createHash('sha256').update(raw).digest('hex');
  return { directory, binary, raw, hash };
}

function sameMetadata(before: fs.BigIntStats, after: fs.BigIntStats) {
  for (const key of [
    'dev',
    'ino',
    'size',
    'mode',
    'nlink',
    'uid',
    'gid',
    'mtimeNs',
    'ctimeNs',
  ] as const)
    expect(after[key]).toBe(before[key]);
}

function observedReader(action?: (position: number) => void) {
  const descriptors: number[] = [];
  const reads: Array<{ position: number; length: number; bufferLength: number }> = [];
  return {
    descriptors,
    reads,
    reader: (fd: number, buffer: Buffer, offset: number, length: number, position: number) => {
      descriptors.push(fd);
      reads.push({ position, length, bufferLength: buffer.length });
      action?.(position);
      return fs.readSync(fd, buffer, offset, length, position);
    },
    assertClosed: () => {
      expect(descriptors.length).toBeGreaterThan(0);
      for (const descriptor of new Set(descriptors))
        expect(() => fs.fstatSync(descriptor)).toThrow();
    },
  };
}

describe('read-only descriptor-based production native executable pin', () => {
  test('release is open and factory construction performs no filesystem proof', () => {
    const f = fixture();
    const lstat = spyOn(fs, 'lstatSync');
    const open = spyOn(fs, 'openSync');
    try {
      expect(ANTIGRAVITY_NATIVE_RELEASED).toBe(true);
      expect(typeof createInstalledAntigravityRuntimeFactory({ home: f.directory })).toBe(
        'function'
      );
      expect(lstat).not.toHaveBeenCalled();
      expect(open).not.toHaveBeenCalled();
    } finally {
      lstat.mockRestore();
      open.mockRestore();
    }
  });

  test('valid exact-owned executable is hashed in bounded chunks without changing metadata', () => {
    const f = fixture();
    const before = fs.lstatSync(f.binary, { bigint: true });
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(true);
    expect(observed.reads.length).toBeGreaterThan(2);
    expect(
      observed.reads.every((read) => read.length <= 64 * 1024 && read.bufferLength <= 64 * 1024)
    ).toBe(true);
    expect(observed.reads.at(-1)?.position).toBe(f.raw.length);
    expect(observed.reads.at(-1)?.length).toBe(1);
    sameMetadata(before, fs.lstatSync(f.binary, { bigint: true }));
    expect(fs.readFileSync(f.binary)).toEqual(f.raw);
    observed.assertClosed();
  });

  test('wrong hash is false and never modifies the owned executable', () => {
    const f = fixture();
    const before = fs.lstatSync(f.binary, { bigint: true });
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(f.binary, '0'.repeat(64), observed.reader)).toBe(false);
    sameMetadata(before, fs.lstatSync(f.binary, { bigint: true }));
    expect(fs.readFileSync(f.binary)).toEqual(f.raw);
    observed.assertClosed();
  });

  test.each(['invalid', 'A'.repeat(64), '0'.repeat(63)])(
    'malformed hash %s never opens or reads a leaf',
    (hash) => {
      const f = fixture();
      const open = spyOn(fs, 'openSync');
      const observed = observedReader();
      try {
        expect(verifyOwnedAntigravityNativePin(f.binary, hash, observed.reader)).toBe(false);
        expect(open).not.toHaveBeenCalled();
        expect(observed.reads).toEqual([]);
      } finally {
        open.mockRestore();
      }
    }
  );

  test('symlink refuses without reading or changing its foreign target', () => {
    const f = fixture();
    const link = path.join(f.directory, 'fixture-symbolic-link');
    fs.symlinkSync(f.binary, link);
    const before = fs.lstatSync(f.binary, { bigint: true });
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(link, f.hash, observed.reader)).toBe(false);
    expect(observed.reads).toEqual([]);
    sameMetadata(before, fs.lstatSync(f.binary, { bigint: true }));
    expect(fs.readFileSync(f.binary)).toEqual(f.raw);
    expect(fs.lstatSync(link).isSymbolicLink()).toBe(true);
  });

  test('hard-linked executable is refused before reading either name', () => {
    const f = fixture();
    const link = path.join(f.directory, 'fixture-hard-link');
    fs.linkSync(f.binary, link);
    const before = fs.lstatSync(f.binary, { bigint: true });
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    expect(verifyOwnedAntigravityNativePin(link, f.hash, observed.reader)).toBe(false);
    expect(observed.reads).toEqual([]);
    sameMetadata(before, fs.lstatSync(f.binary, { bigint: true }));
    expect(fs.lstatSync(link).nlink).toBe(2);
  });

  test.each([0o600, 0o644, 0o641, 0o601])(
    'mode %s without owner execute is refused before reading',
    (mode) => {
      const f = fixture();
      fs.chmodSync(f.binary, mode);
      const before = fs.lstatSync(f.binary, { bigint: true });
      const observed = observedReader();
      expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
      expect(observed.reads).toEqual([]);
      sameMetadata(before, fs.lstatSync(f.binary, { bigint: true }));
    }
  );

  test.each([0o720, 0o702, 0o777])('unsafe writable mode %s is refused before reading', (mode) => {
    const f = fixture();
    fs.chmodSync(f.binary, mode);
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    expect(observed.reads).toEqual([]);
    expect(fs.statSync(f.binary).mode & 0o777).toBe(mode);
  });

  test('exact current UID is required without changing actual fixture ownership', () => {
    const f = fixture();
    const uid = process.getuid!();
    const currentUid = spyOn(process, 'getuid').mockReturnValue(uid + 1);
    const observed = observedReader();
    try {
      expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
      expect(observed.reads).toEqual([]);
      expect(fs.statSync(f.binary).uid).toBe(uid);
    } finally {
      currentUid.mockRestore();
    }
  });

  test('empty executable is refused before reading', () => {
    const f = fixture();
    fs.truncateSync(f.binary, 0);
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    expect(observed.reads).toEqual([]);
    expect(fs.statSync(f.binary).size).toBe(0);
  });

  test('sparse executable over256MiB is refused before allocation or read', () => {
    const f = fixture();
    fs.truncateSync(f.binary, MAX_ANTIGRAVITY_NATIVE_BINARY_BYTES + 1);
    const observed = observedReader();
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    expect(observed.reads).toEqual([]);
    expect(fs.statSync(f.binary).size).toBe(MAX_ANTIGRAVITY_NATIVE_BINARY_BYTES + 1);
  });

  test('NOFOLLOW open refuses a symlink swapped after initial lstat without foreign writes', () => {
    const f = fixture();
    const foreign = path.join(f.directory, 'fixture-foreign-executable');
    fs.writeFileSync(foreign, f.raw, { mode: 0o500 });
    const before = fs.lstatSync(foreign, { bigint: true });
    const actualOpen = fs.openSync;
    const observed = observedReader();
    let flagsSeen = 0;
    const open = spyOn(fs, 'openSync').mockImplementation((file, flags, mode) => {
      if (file === f.binary) {
        flagsSeen = flags as number;
        fs.unlinkSync(f.binary);
        fs.symlinkSync(foreign, f.binary);
      }
      return actualOpen(file, flags, mode);
    });
    try {
      expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
      expect(flagsSeen & fs.constants.O_NOFOLLOW).toBe(fs.constants.O_NOFOLLOW);
      expect(flagsSeen & fs.constants.O_NONBLOCK).toBe(fs.constants.O_NONBLOCK);
      expect(observed.reads).toEqual([]);
      sameMetadata(before, fs.lstatSync(foreign, { bigint: true }));
      expect(fs.readFileSync(foreign)).toEqual(f.raw);
    } finally {
      open.mockRestore();
    }
  });

  test('truncation injected at the read boundary is refused and never repaired', () => {
    const f = fixture();
    let changed = false;
    const observed = observedReader(() => {
      if (!changed) {
        changed = true;
        fs.truncateSync(f.binary, 2);
      }
    });
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    expect(fs.readFileSync(f.binary)).toEqual(f.raw.subarray(0, 2));
    observed.assertClosed();
  });

  test('same bytes and restored mode cannot hide changed ctime during the read', () => {
    const f = fixture();
    const before = fs.lstatSync(f.binary, { bigint: true });
    let changed = false;
    const observed = observedReader(() => {
      if (!changed) {
        changed = true;
        fs.chmodSync(f.binary, 0o500);
        fs.chmodSync(f.binary, 0o700);
      }
    });
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    const after = fs.lstatSync(f.binary, { bigint: true });
    expect(after.mode).toBe(before.mode);
    expect(after.mtimeNs).toBe(before.mtimeNs);
    expect(after.ctimeNs).not.toBe(before.ctimeNs);
    expect(fs.readFileSync(f.binary)).toEqual(f.raw);
    observed.assertClosed();
  });

  test('foreign same-hash replacement during descriptor read remains intact and refuses proof', () => {
    const f = fixture();
    const foreign = path.join(f.directory, 'fixture-same-hash-foreign');
    fs.writeFileSync(foreign, f.raw, { mode: 0o500 });
    const before = fs.lstatSync(foreign, { bigint: true });
    let changed = false;
    const observed = observedReader(() => {
      if (!changed) {
        changed = true;
        fs.renameSync(foreign, f.binary);
      }
    });
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    const after = fs.lstatSync(f.binary, { bigint: true });
    expect(after.ino).toBe(before.ino);
    expect(after.mode).toBe(before.mode);
    expect(after.mtimeNs).toBe(before.mtimeNs);
    expect(fs.readFileSync(f.binary)).toEqual(f.raw);
    observed.assertClosed();
  });

  test('growth at the explicit EOF boundary refuses a matching prefix hash', () => {
    const f = fixture();
    let changed = false;
    const observed = observedReader((position) => {
      if (!changed && position === f.raw.length) {
        changed = true;
        fs.appendFileSync(f.binary, Buffer.from([0x42]));
      }
    });
    expect(verifyOwnedAntigravityNativePin(f.binary, f.hash, observed.reader)).toBe(false);
    expect(changed).toBe(true);
    expect(fs.readFileSync(f.binary)).toEqual(Buffer.concat([f.raw, Buffer.from([0x42])]));
    observed.assertClosed();
  });

  test('private reader exception is reduced to false and descriptor is closed', () => {
    const f = fixture();
    let descriptor = -1;
    expect(
      verifyOwnedAntigravityNativePin(f.binary, f.hash, (fd) => {
        descriptor = fd;
        throw new Error('INVENTED_PRIVATE_READER_SENTINEL');
      })
    ).toBe(false);
    expect(() => fs.fstatSync(descriptor)).toThrow();
    expect(fs.readFileSync(f.binary)).toEqual(f.raw);
  });
});
