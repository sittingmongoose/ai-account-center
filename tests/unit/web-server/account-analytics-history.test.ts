import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  accountAnalyticsIdentity,
  analyticsWindow,
  appendAccountAnalyticsSnapshot,
  FileAccountAnalyticsHistoryStore,
} from '../../../src/web-server/services/account-analytics-history';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';

const NOW = Date.parse('2026-10-01T16:00:00Z');
const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});
function directory(): string {
  const result = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-analytics-history-'));
  directories.push(result);
  return result;
}
function account(): DashboardAccount {
  return {
    id: 'codex:first',
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'first',
    email: 'first@example.com',
    plan: 'pro',
    platform: 'ubuntu',
    source: 'Codex saved login on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: new Date(NOW).toISOString(),
    sampledAt: new Date(NOW).toISOString(),
    isActive: true,
    capabilities: { codexProfile: 'first', claudeProfileId: null, claudePlatforms: [] },
    windows: [
      {
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 120.5,
        remainingPercent: 0,
        used: -2.5,
        limit: 100,
        unit: 'credits',
        remaining: -22.5,
        resetAt: null,
        expiresAt: null,
        windowMinutes: 10080,
      },
    ],
  };
}

describe('private bounded quota history', () => {
  it('writes atomic private normalized data and excludes arbitrary auth/helper fields', async () => {
    const root = directory();
    const store = new FileAccountAnalyticsHistoryStore(root);
    expect(await store.read()).toBeNull();
    const current = account();
    Object.assign(current.windows[0], { access_token: 'secret-token-sentinel' });
    const history = appendAccountAnalyticsSnapshot(null, [current], NOW);
    await store.write(history);
    const file = path.join(root, 'account-analytics', 'quota-history-v1.json');
    expect(fs.statSync(path.dirname(file)).mode & 0o777).toBe(0o700);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(fs.readFileSync(file, 'utf8')).not.toContain('secret-token-sentinel');
    const reread = await store.read();
    expect(reread?.records[0].windows[0].usedPercent).toBe(120.5);
    expect(reread?.records[0].windows[0].used).toBe(-2.5);
    expect(reread?.records[0].windows[0].remaining).toBe(-22.5);
    expect(fs.readdirSync(path.dirname(file))).toEqual(['quota-history-v1.json']);
  });

  it('rejects unknown/corrupt history schemas without changing their bytes', async () => {
    const root = directory();
    const location = path.join(root, 'account-analytics');
    fs.mkdirSync(location, { mode: 0o700 });
    const file = path.join(location, 'quota-history-v1.json');
    for (const payload of ['{broken-json', JSON.stringify({ schemaVersion: 2, records: [] })]) {
      fs.writeFileSync(file, payload, { mode: 0o600 });
      await expect(new FileAccountAnalyticsHistoryStore(root).read()).rejects.toBeDefined();
      expect(fs.readFileSync(file, 'utf8')).toBe(payload);
    }
  });

  it('refuses symlink and hardlink destinations and leaves their target intact', async () => {
    const root = directory();
    const location = path.join(root, 'account-analytics');
    fs.mkdirSync(location, { mode: 0o700 });
    const victim = path.join(root, 'victim');
    fs.writeFileSync(victim, 'untouched', { mode: 0o600 });
    const file = path.join(location, 'quota-history-v1.json');
    const store = new FileAccountAnalyticsHistoryStore(root);
    for (const kind of ['symlink', 'hardlink']) {
      if (kind === 'symlink') fs.symlinkSync(victim, file);
      else fs.linkSync(victim, file);
      await expect(store.read()).rejects.toBeDefined();
      await expect(
        store.write(appendAccountAnalyticsSnapshot(null, [account()], NOW))
      ).rejects.toBeDefined();
      expect(fs.readFileSync(victim, 'utf8')).toBe('untouched');
      fs.unlinkSync(file);
    }
  });

  it('refuses linked history directories and oversized files before parsing', async () => {
    const root = directory();
    const target = path.join(root, 'target');
    fs.mkdirSync(target);
    fs.symlinkSync(target, path.join(root, 'account-analytics'));
    await expect(new FileAccountAnalyticsHistoryStore(root).read()).rejects.toBeDefined();
    await expect(
      new FileAccountAnalyticsHistoryStore(root).write(
        appendAccountAnalyticsSnapshot(null, [account()], NOW)
      )
    ).rejects.toBeDefined();
    fs.unlinkSync(path.join(root, 'account-analytics'));
    fs.mkdirSync(path.join(root, 'account-analytics'));
    const file = path.join(root, 'account-analytics', 'quota-history-v1.json');
    fs.writeFileSync(file, '');
    fs.truncateSync(file, 32 * 1024 * 1024 + 1);
    await expect(new FileAccountAnalyticsHistoryStore(root).read()).rejects.toBeDefined();
  });

  it('deduplicates a cached timestamp across computers while retaining actual switches and balance changes', () => {
    const current = account();
    const first = appendAccountAnalyticsSnapshot(null, [current], NOW);
    const duplicate = appendAccountAnalyticsSnapshot(first, [current], NOW + 60_000);
    expect(duplicate.records).toHaveLength(1);
    current.isActive = false;
    const switched = appendAccountAnalyticsSnapshot(duplicate, [current], NOW + 120_000);
    expect(switched.records).toHaveLength(2);
    current.windows[0].remaining = -30;
    expect(appendAccountAnalyticsSnapshot(switched, [current], NOW + 180_000).records).toHaveLength(
      3
    );
  });

  it('expires old observations, skips future and timestamp-less readings, and preserves missing percentages', () => {
    const old = account();
    old.sampledAt = new Date(NOW - 31 * 86_400_000).toISOString();
    const initial = appendAccountAnalyticsSnapshot(null, [old], NOW - 31 * 86_400_000);
    const current = account();
    current.sampledAt = new Date(NOW + 60_000).toISOString();
    expect(appendAccountAnalyticsSnapshot(initial, [current], NOW).records).toEqual([]);
    current.sampledAt = null;
    current.fetchedAt = null;
    expect(appendAccountAnalyticsSnapshot(null, [current], NOW).records).toEqual([]);
    current.sampledAt = new Date(NOW).toISOString();
    current.windows[0].usedPercent = null;
    current.windows[0].remainingPercent = null;
    const missing = appendAccountAnalyticsSnapshot(null, [current], NOW);
    expect(missing.records[0].windows[0].usedPercent).toBeNull();
    expect(missing.records[0].windows[0].resetAt).toBeNull();
  });

  it('does not manufacture a measured zero for a provider without a usable sample', () => {
    const current = account();
    current.status = 'unavailable';
    current.windows = [];
    expect(appendAccountAnalyticsSnapshot(null, [current], NOW).records).toEqual([]);
  });

  it('hashes exact identity independent of launch platform or email casing and separates changed accounts', () => {
    const first = account();
    const second = { ...first, platform: 'windows' as const, email: 'FIRST@example.com' };
    expect(accountAnalyticsIdentity(first)).toBe(accountAnalyticsIdentity(second));
    expect(accountAnalyticsIdentity(first)).not.toBe(
      accountAnalyticsIdentity({ ...first, email: 'other@example.com' })
    );
    expect(accountAnalyticsIdentity(first)).not.toBe(
      accountAnalyticsIdentity({ ...first, provider: 'claude' })
    );
  });

  it('validates nonfinite numeric values without converting them to zero', () => {
    const safe = analyticsWindow({
      ...account().windows[0],
      usedPercent: NaN,
      used: Infinity,
      remaining: -Infinity,
    });
    expect(safe?.usedPercent).toBeNull();
    expect(safe?.used).toBeNull();
    expect(safe?.remaining).toBeNull();
  });

  it('persists cached optional-window provenance through a private cold read without losing precision', async () => {
    const current = account();
    const originalSample = new Date(NOW - 3_600_000).toISOString();
    current.windows.push({
      ...current.windows[0],
      key: 'prepaid_balance',
      kind: 'balance',
      status: 'cached',
      sampledAt: originalSample,
      used: 12.123456,
      remaining: -4.123456,
      resetAt: '2026-10-02T01:02:03Z',
      expiresAt: '2026-10-20T04:05:06Z',
    });
    const root = directory();
    await new FileAccountAnalyticsHistoryStore(root).write(
      appendAccountAnalyticsSnapshot(null, [current], NOW)
    );
    const cold = await new FileAccountAnalyticsHistoryStore(root).read();
    expect(cold?.records[0].sampledAt).toBe(new Date(NOW).toISOString());
    expect(cold?.records[0].windows[0].status).toBeUndefined();
    expect(cold?.records[0].windows[0].sampledAt).toBeUndefined();
    expect(cold?.records[0].windows[1]).toMatchObject({
      status: 'cached',
      sampledAt: originalSample,
      used: 12.123456,
      remaining: -4.123456,
      resetAt: '2026-10-02T01:02:03.000Z',
      expiresAt: '2026-10-20T04:05:06.000Z',
    });
  });

  it('never converts missing, invalid, future or expired cached-window times into a fresh sample', () => {
    const fresh = account();
    const retained = { ...fresh.windows[0], key: 'prepaid', status: 'cached' as const };
    expect(analyticsWindow(retained)).toBeNull();
    expect(analyticsWindow({ ...retained, sampledAt: 'not-a-date' })).toBeNull();
    expect(analyticsWindow({ ...retained, sampledAt: true })).toBeNull();
    fresh.windows.push(
      { ...retained, sampledAt: new Date(NOW + 60_000).toISOString() },
      { ...retained, sampledAt: new Date(NOW - 31 * 86_400_000).toISOString() }
    );
    const observed = appendAccountAnalyticsSnapshot(null, [fresh], NOW);
    expect(observed.records).toHaveLength(1);
    expect(observed.records[0].windows).toHaveLength(1);
    expect(observed.records[0].windows[0].usedPercent).toBe(120.5);
    expect(observed.records[0].sampledAt).toBe(new Date(NOW).toISOString());
  });

  it('retains a full month of hourly observations for the fourteen configured accounts within its storage bounds', () => {
    const rows = Array.from({ length: 14 }, (_, index) => ({
      ...account(),
      id: `codex:fixture-${index}`,
      email: `fixture-${index}@example.com`,
    }));
    const history = {
      schemaVersion: 1 as const,
      collectedSince: new Date(NOW - 30 * 86_400_000).toISOString(),
      records: Array.from({ length: 30 * 24 }, (_, hour) =>
        rows.map((row) => ({
          identity: accountAnalyticsIdentity(row),
          accountId: row.id,
          sampledAt: new Date(NOW - (30 * 24 - hour) * 3_600_000).toISOString(),
          observedAt: new Date(NOW - (30 * 24 - hour) * 3_600_000).toISOString(),
          source: row.source,
          status: row.status,
          platform: row.platform,
          isActive: false,
          windows: row.windows,
        }))
      ).flat(),
    };
    const retained = appendAccountAnalyticsSnapshot(history, rows, NOW);
    expect(retained.records.length).toBeLessThan(24_000);
    expect(Date.parse(retained.records[0].sampledAt)).toBeLessThanOrEqual(NOW - 29 * 86_400_000);
    expect(new Set(retained.records.map((record) => record.identity)).size).toBe(14);
    expect(Buffer.byteLength(JSON.stringify(retained))).toBeLessThan(32 * 1024 * 1024);
  });
});
