import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import * as fsSync from 'fs';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import {
  readUsageCache,
  writeUsageCache,
} from '../../../src/web-server/services/usage-failover-cache';

const scopes: string[] = [];
afterEach(async () => {
  await Promise.all(
    scopes.splice(0).map((scope) => fs.rm(scope, { recursive: true, force: true }))
  );
});
async function fixture() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-failover-cache-'));
  scopes.push(root);
  return { root, file: path.join(root, 'account-usage-failover', `${'a'.repeat(64)}.json`) };
}

const key = (index: number) => index.toString(16).padStart(64, '0');
const observation = (digest: string) => ({
  version: 1,
  key: digest,
  identity: null,
  primary: null,
  saved: null,
  nextAttempt: 0,
});
async function populate(root: string, count: number): Promise<void> {
  await fs.mkdir(root, { recursive: true, mode: 0o700 });
  const now = Date.now();
  await Promise.all(
    Array.from({ length: count }, async (_, offset) => {
      const index = offset + 1;
      const file = path.join(root, `${key(index)}.json`);
      await fs.writeFile(file, JSON.stringify(observation(key(index))), { mode: 0o600 });
      // The newly written current file is intentionally older than every fixture
      // so the test proves its explicit retention, independent of timestamp order.
      const stamp = new Date(now + 3_600_000 + index * 1000);
      await fs.utimes(file, stamp, stamp);
    })
  );
}

describe('private failover cache storage', () => {
  it('round trips bounded observations without leaving temporary files', async () => {
    const h = await fixture();
    writeUsageCache(h.file, { version: 1, reading: 'fixture' });
    expect(readUsageCache(h.file)).toEqual({ version: 1, reading: 'fixture' });
    expect(await fs.readdir(path.dirname(h.file))).toHaveLength(1);
  });
  it('rejects oversized reads and writes', async () => {
    const h = await fixture();
    writeUsageCache(h.file, { value: 'x'.repeat(300_000) });
    expect(readUsageCache(h.file)).toBeNull();
    await fs.mkdir(path.dirname(h.file), { recursive: true, mode: 0o700 });
    await fs.writeFile(h.file, JSON.stringify({ value: 'x'.repeat(300_000) }), { mode: 0o600 });
    expect(readUsageCache(h.file)).toBeNull();
  });
  it('neither reads nor replaces a multiply linked file', async () => {
    const h = await fixture();
    writeUsageCache(h.file, { original: true });
    const linked = path.join(h.root, 'linked.json');
    await fs.link(h.file, linked);
    expect(readUsageCache(h.file)).toBeNull();
    writeUsageCache(h.file, { replaced: true });
    expect(JSON.parse(await fs.readFile(h.file, 'utf8'))).toEqual({ original: true });
  });
  it('rejects a linked cache directory', async () => {
    const h = await fixture();
    const destination = path.join(h.root, 'other');
    await fs.mkdir(destination, { mode: 0o700 });
    await fs.symlink(
      destination,
      path.dirname(h.file),
      process.platform === 'win32' ? 'junction' : 'dir'
    );
    writeUsageCache(h.file, { rejected: true });
    expect(readUsageCache(h.file)).toBeNull();
    expect(await fs.readdir(destination)).toHaveLength(0);
  });

  it('retains only the newest 256 observations while always preserving the current write', async () => {
    const h = await fixture();
    const root = path.dirname(h.file);
    await populate(root, 270);
    writeUsageCache(h.file, observation(path.basename(h.file, '.json')));
    const remaining = await fs.readdir(root);
    expect(remaining).toHaveLength(256);
    expect(remaining).toContain(path.basename(h.file));
    expect(remaining).toContain(`${key(270)}.json`);
    expect(remaining).toContain(`${key(16)}.json`);
    expect(remaining).not.toContain(`${key(15)}.json`);
    expect(remaining).not.toContain(`${key(1)}.json`);
  });

  it('does not prune after an oversized write was refused', async () => {
    const h = await fixture();
    await populate(path.dirname(h.file), 260);
    writeUsageCache(h.file, { oversized: 'x'.repeat(300_000) });
    expect(await fs.readdir(path.dirname(h.file))).toHaveLength(260);
  });

  it('preserves cooldowns, unknown formats, oversized records, linked files and linked paths', async () => {
    const h = await fixture();
    const root = path.dirname(h.file);
    await populate(root, 260);
    const preserved = new Map([
      ['notes.json', JSON.stringify({ private: 'unrelated' })],
      [`cooldown-${key(900)}.json`, JSON.stringify({ version: 1, key: key(900), until: 1 })],
      [`${key(901)}.json`, JSON.stringify({ version: 2, key: key(901) })],
      [`${key(902)}.json`, '{broken-json'],
      [`${key(903)}.json`, JSON.stringify({ ...observation(key(903)), unknown: true })],
      [
        `${key(904)}.json`,
        JSON.stringify({ ...observation(key(904)), saved: 'x'.repeat(300_000) }),
      ],
    ]);
    for (const [name, value] of preserved)
      await fs.writeFile(path.join(root, name), value, { mode: 0o600 });
    const outside = path.join(h.root, 'other-private-file.json');
    await fs.writeFile(outside, JSON.stringify(observation(key(905))), { mode: 0o600 });
    const linked = path.join(root, `${key(905)}.json`);
    await fs.link(outside, linked);
    const otherDirectory = path.join(h.root, 'other-directory');
    await fs.mkdir(otherDirectory, { mode: 0o700 });
    const sentinel = path.join(otherDirectory, 'sentinel.json');
    await fs.writeFile(sentinel, 'preserved', { mode: 0o600 });
    const directoryLink = path.join(root, `${key(906)}.json`);
    await fs.symlink(
      otherDirectory,
      directoryLink,
      process.platform === 'win32' ? 'junction' : 'dir'
    );
    writeUsageCache(h.file, observation(path.basename(h.file, '.json')));
    expect(await fs.readdir(root)).toHaveLength(256 + preserved.size + 2);
    for (const [name, value] of preserved)
      expect(await fs.readFile(path.join(root, name), 'utf8')).toBe(value);
    expect(await fs.readFile(linked, 'utf8')).toBe(await fs.readFile(outside, 'utf8'));
    expect((await fs.lstat(directoryLink)).isSymbolicLink()).toBe(true);
    expect(await fs.readFile(sentinel, 'utf8')).toBe('preserved');
  });

  it('does not prune a replacement directory after the successful write changes parent inode', async () => {
    const h = await fixture();
    const root = path.dirname(h.file);
    const foreign = path.join(h.root, 'replacement');
    const original = path.join(h.root, 'original');
    await populate(root, 260);
    await populate(foreign, 260);
    const rename = fsSync.renameSync;
    const hook = spyOn(fsSync, 'renameSync').mockImplementation((from, to) => {
      rename(from, to);
      if (to === h.file) {
        rename(root, original);
        rename(foreign, root);
      }
    });
    try {
      writeUsageCache(h.file, observation(path.basename(h.file, '.json')));
    } finally {
      hook.mockRestore();
    }
    expect(await fs.readdir(root)).toHaveLength(260);
    expect(await fs.readdir(original)).toHaveLength(261);
    expect(await fs.readFile(path.join(root, `${key(1)}.json`), 'utf8')).toBe(
      JSON.stringify(observation(key(1)))
    );
  });

  it.skipIf(process.platform === 'win32')(
    'leaves nonprivate files outside the observation retention count',
    async () => {
      const h = await fixture();
      const root = path.dirname(h.file);
      await populate(root, 260);
      const exposed = path.join(root, `${key(999)}.json`);
      await fs.writeFile(exposed, JSON.stringify(observation(key(999))), { mode: 0o644 });
      writeUsageCache(h.file, observation(path.basename(h.file, '.json')));
      expect(await fs.readdir(root)).toHaveLength(257);
      expect(await fs.readFile(exposed, 'utf8')).toBe(JSON.stringify(observation(key(999))));
    }
  );
});
