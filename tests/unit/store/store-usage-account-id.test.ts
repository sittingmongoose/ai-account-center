/**
 * Scoreboard v2 recheck 2, H1: a usage account id may be exactly `<provider>:<email address>`
 * (a Claude account without a profile id is `claude:<email>`) and nothing near it. The shared
 * table (`tests/fixtures/usage-account-id-cases.json`, also run by `native/aac-store`) goes
 * through the real producer (`storeObservationFromAccount`) and the real native module; every
 * store file's bytes are checked after sealing, reopening and a rebuild from the logs. Synthetic
 * values only: every refused id holds `synthetic-pass`, no accepted one does. Skipped, and said
 * so, without the module. Never the real home.
 */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as zlib from 'zlib';
import { loadNativeStore, nativeModuleName } from '../../../src/store/native-loader';
import { AacStore } from '../../../src/store/store-service';
import { storeObservationFromAccount } from '../../../src/store/usage-observations';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import cases from '../../fixtures/usage-account-id-cases.json';

const MODULE = path.resolve(__dirname, '../../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: MODULE });
const haveModule = loaded.module !== null;
if (!haveModule)
  console.warn(
    `[!] ${MODULE} is not built: the account id tests are skipped (bun run store:build).`
  );

const HOUR = 3_600_000;
const START = Date.parse('2026-10-01T10:00:00Z');
const REFUSED = 'synthetic-pass';
const table = cases as Array<{ name: string; id: string; accepted: boolean }>;

let sandbox = '';
let previousHome: string | undefined;
beforeAll(() => {
  previousHome = process.env.CCS_HOME;
  sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-account-id-'));
  process.env.CCS_HOME = sandbox;
});
afterAll(() => {
  if (previousHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previousHome;
  fs.rmSync(sandbox, { recursive: true, force: true });
});

async function openStore(dir: string): Promise<AacStore> {
  if (!loaded.module) throw new Error('no module');
  const handle = new loaded.module.NativeStore();
  await handle.call('open', JSON.stringify({ dir, retentionDays: 90 }));
  return new AacStore(handle, dir);
}

/** Files under `dir` holding `needle`, compressed segments decoded. */
function holding(dir: string, needle: string): string[] {
  const hits: string[] = [];
  const walk = (at: string) => {
    for (const entry of fs.readdirSync(at, { withFileTypes: true })) {
      const file = path.join(at, entry.name);
      if (entry.isDirectory()) walk(file);
      else {
        let bytes = fs.readFileSync(file);
        if (file.endsWith('.zst')) bytes = zlib.zstdDecompressSync(bytes);
        if (bytes.includes(Buffer.from(needle))) hits.push(file);
      }
    }
  };
  walk(dir);
  return hits;
}

function account(id: string, now: number): DashboardAccount {
  return {
    id,
    provider: 'claude',
    providerLabel: 'Claude',
    label: 'Synthetic',
    email: null,
    plan: 'max',
    platform: 'ubuntu',
    source: 'Claude quota',
    status: 'ok',
    message: null,
    fetchedAt: new Date(now).toISOString(),
    sampledAt: new Date(now).toISOString(),
    isActive: false,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
    windows: [
      {
        key: 'five_hour',
        label: '5 hour',
        usedPercent: 10,
        remainingPercent: 90,
        resetAt: new Date(now + 5 * HOUR).toISOString(),
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
    ],
  } as unknown as DashboardAccount;
}

describe('the shared account id table', () => {
  it('is the table both languages run, with one needle for every refused id', () => {
    expect(table.length).toBeGreaterThanOrEqual(40);
    expect(new Set(table.map((c) => c.name)).size).toBe(table.length);
    for (const c of table) expect(c.id.includes(REFUSED)).toBe(!c.accepted);
    // The reviewer's witness is in it, refused.
    expect(table).toContainEqual({
      name: 'reviewer witness: two @ ending at localhost',
      id: 'claude:synthetic-pass@example.com@localhost',
      accepted: false,
    });
  });
});

describe.skipIf(!haveModule)('usage account ids through the native module', () => {
  it('keeps exactly a provider and an address and refuses every near miss', async () => {
    const dir = path.join(sandbox, 'store');
    let store = await openStore(dir);
    for (const [n, c] of table.entries()) {
      const now = START + n * 1000;
      const observation = storeObservationFromAccount(account(c.id, now), now, 90 * 24 * HOUR);
      expect(observation?.account.account_id).toBe(c.id);
      const result = await store.record([observation!]);
      expect([c.name, result.sampled, result.rejected]).toEqual([
        c.name,
        c.accepted ? 1 : 0,
        c.accepted ? 0 : 1,
      ]);
    }
    // Sealed (maintenance seals and compresses the logs), closed, reopened, rebuilt.
    await store.maintain({ nowMs: START + 2 * 24 * HOUR });
    await store.close();
    expect(holding(dir, REFUSED)).toEqual([]);
    expect(holding(dir, 'person.name+tag@mail.example.co.uk').length).toBeGreaterThan(0);
    store = await openStore(dir);
    await store.close();
    fs.rmSync(path.join(dir, 'aac.redb'));
    store = await openStore(dir);
    await store.close();
    expect(holding(dir, REFUSED)).toEqual([]);
    expect(holding(dir, 'person.name+tag@mail.example.co.uk').length).toBeGreaterThan(0);
  });
});
