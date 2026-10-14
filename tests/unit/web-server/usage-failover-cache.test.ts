import { afterEach, describe, expect, it } from 'bun:test';
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
});
