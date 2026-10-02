/**
 * The orphan-key sweep (startup and daily): key files that no registry entry
 * names and that are older than an hour are deleted; named, young and
 * unreadable-registry cases keep everything.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { LocalKeyStore } from '../../../src/web-server/services/account-key-store';
import {
  ORPHAN_KEY_AGE_MS,
  sweepOrphanKeys,
} from '../../../src/web-server/services/account-key-sweep';

let root: string;
let ccsDir: string;
const NOW = Date.parse('2026-10-02T12:00:00Z');

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-key-sweep-'));
  ccsDir = path.join(root, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

function registry(accounts: unknown[]): void {
  fs.writeFileSync(
    path.join(ccsDir, 'account-usage-accounts.json'),
    JSON.stringify({ version: 2, accounts }),
    { mode: 0o600 }
  );
}

function keyEntry(provider: string, keyId: string) {
  return {
    id: `${provider}:acct:${keyId}`,
    provider,
    platform: 'ubuntu',
    sshHost: null,
    label: null,
    credential: { kind: 'aac-key', keyId },
    createdAt: null,
    createdBy: 'dashboard',
  };
}

async function key(provider: 'zai' | 'kimi-code', keyId: string, ageMs: number): Promise<void> {
  const store = new LocalKeyStore(ccsDir);
  await store.put(provider, keyId, `${provider}-secret-${keyId}`);
  const at = (NOW - ageMs) / 1000;
  fs.utimesSync(path.join(ccsDir, 'account-usage', 'keys', `${provider}-${keyId}.json`), at, at);
}

function keyFiles(): string[] {
  return fs.readdirSync(path.join(ccsDir, 'account-usage', 'keys')).sort();
}

describe('sweepOrphanKeys', () => {
  it('deletes only old key files that no registry entry names', async () => {
    registry([keyEntry('zai', 'aaaaaaaa')]);
    const old = ORPHAN_KEY_AGE_MS + 60_000;
    await key('zai', 'aaaaaaaa', old);
    await key('zai', 'bbbbbbbb', old);
    await key('zai', 'cccccccc', ORPHAN_KEY_AGE_MS - 60_000);
    await key('kimi-code', 'dddddddd', old);
    expect(await sweepOrphanKeys(ccsDir, () => NOW)).toBe(2);
    expect(keyFiles()).toEqual(['zai-aaaaaaaa.json', 'zai-cccccccc.json']);
    expect(await sweepOrphanKeys(ccsDir, () => NOW)).toBe(0);
  });

  it('deletes nothing without a readable version 2 registry', async () => {
    await key('zai', 'bbbbbbbb', ORPHAN_KEY_AGE_MS * 2);
    expect(await sweepOrphanKeys(ccsDir, () => NOW)).toBe(0);
    fs.writeFileSync(path.join(ccsDir, 'account-usage-accounts.json'), '{ broken');
    expect(await sweepOrphanKeys(ccsDir, () => NOW)).toBe(0);
    expect(keyFiles()).toEqual(['zai-bbbbbbbb.json']);
  });
});
