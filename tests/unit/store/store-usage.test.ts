import { loadStoreTestModule } from '../../support/store-handle';
/**
 * The history store with the real native module (dist/native, built by `bun run store:build`).
 * Skipped, and said so, when this computer has no module. Every store lives in a temporary
 * folder; never the real home.
 */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as zlib from 'zlib';
import { haveNativeStoreServer, startNativeStoreServer } from '../../support/native-store-server';
import { nativeModuleName } from '../../../src/store/native-loader';
import { AacStore } from '../../../src/store/store-service';
import { buildRollupExports } from '../../../src/store/usage-exports';
import { backfillUsageHistory } from '../../../src/store/usage-backfill';
import { storeObservationFromAccount } from '../../../src/store/usage-observations';
import {
  appendAccountAnalyticsSnapshot,
  FileAccountAnalyticsHistoryStore,
  type AccountAnalyticsHistoryData,
} from '../../../src/web-server/services/account-analytics-history';
import { handleStoreCommand } from '../../../src/commands/store-command';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';

const MODULE = path.resolve(__dirname, '../../../dist/native', nativeModuleName());
const loaded = loadStoreTestModule({ env: {}, file: MODULE });
const haveModule = loaded.module !== null;
if (!haveModule)
  console.warn(`[!] ${MODULE} is not built: the store tests are skipped (bun run store:build).`);

// These full-history fixtures check completeness, with extra I/O time for the socket handle.
const LARGE_FIXTURE_TIMEOUT_MS = process.env.AAC_TEST_STORE_HANDLE === 'socket' ? 180_000 : 60_000;
const MIN = 60_000;
const HOUR = 3_600_000;
const START = Date.parse('2026-10-01T10:00:00Z');

let sandbox = '';
let previousHome: string | undefined;
beforeAll(() => {
  previousHome = process.env.CCS_HOME;
  sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-usage-'));
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

/** Two accounts; the first burns a point every 7 minutes, then resets at START + 90 min. */
function accountsAt(now: number): DashboardAccount[] {
  const minute = Math.round((now - START) / MIN);
  const reset = START + 90 * MIN;
  const used = now < reset ? 20 + Math.floor(minute / 7) : 2 + Math.floor((minute - 90) / 7);
  const base = (id: string, windows: DashboardAccount['windows']): DashboardAccount => ({
    id,
    provider: 'codex',
    providerLabel: 'Codex',
    label: id.split(':')[1],
    email: null,
    plan: 'pro',
    platform: 'ubuntu',
    source: 'Codex quota',
    status: 'ok',
    message: null,
    fetchedAt: new Date(now).toISOString(),
    sampledAt: new Date(now).toISOString(),
    isActive: id === 'codex:first',
    capabilities: { codexProfile: id.split(':')[1], claudeProfileId: null, claudePlatforms: [] },
    windows,
  });
  return [
    base('codex:first', [
      {
        key: 'five_hour',
        label: '5 hour',
        usedPercent: used,
        remainingPercent: 100 - used,
        resetAt: new Date(now < reset ? reset : START + 390 * MIN).toISOString(),
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
      {
        key: 'seven_day',
        label: 'week',
        usedPercent: 40,
        remainingPercent: 60,
        resetAt: '2026-10-05T00:00:00Z',
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
      },
    ]),
    base('codex:second', [
      {
        key: 'credits',
        label: 'Credits',
        kind: 'balance',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
        remaining: 500 - Math.floor(minute / 10) * 3,
        unit: 'credits',
      },
    ]),
  ];
}

describe.skipIf(!haveModule)('history store with the native module', () => {
  it('answers ranges across UTC days from the packed day values, and after a rebuild', async () => {
    const dir = path.join(fs.mkdtempSync(path.join(sandbox, 'days-')), 'store');
    let store = await openStore(dir);
    const END = START + 20 * HOUR; // from 10:00 to 06:00 the next day
    const observations = [];
    for (let t = START; t <= END; t += MIN)
      for (const account of accountsAt(t)) {
        const observation = storeObservationFromAccount(account, t, 90 * 24 * HOUR);
        if (observation) observations.push(observation);
      }
    await store.record(observations as never);
    const raw = async (fromMs: number, toMs: number) => {
      const answer = await store.seriesPoints({
        identities: undefined,
        windowKey: 'five_hour',
        fromMs,
        toMs,
        resolution: 'raw',
      } as never);
      return answer.series.flatMap((series) => series.points) as number[][];
    };
    const midnight = Date.parse('2026-10-02T00:00:00Z');
    const all = await raw(START, END);
    expect(all[0][0]).toBe(START);
    expect(all.at(-1)?.[0]).toBeGreaterThan(midnight);
    // One hour across midnight is exactly the matching part of the whole list.
    const across = await raw(midnight - 30 * MIN, midnight + 30 * MIN);
    expect(across.length).toBeGreaterThan(2);
    expect(across).toEqual(
      all.filter((p) => p[0] >= midnight - 30 * MIN && p[0] <= midnight + 30 * MIN)
    );
    // Every reading is counted, with the open span of each series among them.
    const counts = await store.counts();
    expect(counts.series).toBe(3);
    expect(counts.reading).toBeGreaterThanOrEqual(all.length);
    expect(counts.reading_by_time).toBeUndefined();
    const hours = await store.seriesPoints({
      windowKey: 'five_hour',
      fromMs: START,
      toMs: END,
      resolution: 'hour',
    } as never);
    // 10:00 through 05:00: the 06:00 sample changes nothing, so its hour has no rollup yet.
    expect(hours.series[0].points.length).toBe(20);
    // Redb is only a projection: the same answers from the logs alone.
    const at = await store.usageAt(midnight);
    await store.close();
    fs.rmSync(path.join(dir, 'aac.redb'));
    store = await openStore(dir);
    try {
      expect(await raw(START, END)).toEqual(all);
      expect(await store.usageAt(midnight)).toEqual(at);
      expect((await store.counts()).reading).toBe(counts.reading);
    } finally {
      await store.close();
    }
  });

  it(
    'the burn route finds every series, however many hourly points they hold',
    async () => {
      const dir = fs.mkdtempSync(path.join(sandbox, 'burn-'));
      const store = await openStore(path.join(dir, 'store'));
      // Keep the first observation well inside the route's rolling 90-day window.
      // Anchor the end one hour back to exercise a rollover even on a fast run.
      const end = (Math.floor(Date.now() / HOUR) - 1) * HOUR;
      const hours = 90 * 24 - 12;
      const start = end - hours * HOUR;
      const resetAt = new Date(end).toISOString();
      let server: Awaited<ReturnType<typeof startNativeStoreServer>> | undefined;
      try {
        // Keep more than 20,000 hourly points across 11 series to cross the 20,000-point cap.
        // Usage changes daily. Hourly sightings between changes still produce rollups
        // without projecting a changed quota reading every hour.
        const [first] = accountsAt(START);
        for (let account = 0; account < 11; account++) {
          const observations = [];
          for (let t = start; t < end; t += HOUR) {
            const used = Math.floor((t - start) / (24 * HOUR));
            const observation = storeObservationFromAccount(
              {
                ...first,
                id: `codex:burn${account}`,
                label: `burn${account}`,
                fetchedAt: new Date(t).toISOString(),
                sampledAt: new Date(t).toISOString(),
                windows: [
                  { ...first.windows[0], usedPercent: used, remainingPercent: 100 - used, resetAt },
                ],
              } as DashboardAccount,
              t,
              90 * 24 * HOUR
            );
            if (observation) observations.push(observation);
          }
          await store.record(observations as never);
        }
        const capped = await store.seriesPoints({
          fromMs: start,
          toMs: end - 1,
          resolution: 'hour',
        });
        expect(capped.truncated).toBe(true);
        expect(capped.series.length).toBeLessThan(11);
        const exports = await buildRollupExports(store, [], [], end - 1);
        expect(exports.hourly.series).toHaveLength(11);
        expect(exports.hourly.series.reduce((n, s) => n + s.points.length, 0)).toBe(11 * hours);
        expect(exports.hourly.series.every((s) => s.points.length === hours)).toBe(true);
        if (!haveNativeStoreServer) {
          console.warn('[SKIP] burn HTTP replay requires a built native dashboard launcher');
          return;
        }
        await store.close();
        server = await startNativeStoreServer(dir);
        const response = await server.send('/api/usage/burn?span=90d', {
          headers: { cookie: server.cookie },
        });
        expect(response.status).toBe(200);
        const body = (await response.json()) as {
          at: string;
          series: Array<{ accountId: string; spans: Array<{ span: string; consumed: number }> }>;
        };
        expect(body.series.map((s) => s.accountId).sort()).toEqual(
          Array.from({ length: 11 }, (_, i) => `codex:burn${i}`).sort()
        );
        const serverNow = Date.parse(body.at);
        expect(serverNow).toBeGreaterThan(end);
        expect(serverNow - 90 * 24 * HOUR).toBeLessThanOrEqual(start);
        for (const series of body.series) {
          expect(series.spans).toEqual([expect.objectContaining({ span: '90d', consumed: 89 })]);
        }
      } finally {
        await server?.stop();
        await store.close();
      }
    },
    LARGE_FIXTURE_TIMEOUT_MS
  );

  it('later CLI checks continue to report known segment corruption', async () => {
    const home = fs.mkdtempSync(path.join(sandbox, 'check-corruption-'));
    const previous = process.env.CCS_HOME;
    process.env.CCS_HOME = home;
    try {
      const dir = path.join(home, '.ccs', 'store');
      const store = await openStore(dir);
      await store.record([storeObservationFromAccount(accountsAt(START)[0], START, HOUR)] as never);
      await store.close();
      const log = path.join(dir, 'log', 'readings');
      const segment = fs.readdirSync(log).find((name) => name.endsWith('.seglog'))!;
      const file = path.join(log, segment);
      const bytes = fs.readFileSync(file);
      bytes[Math.floor(bytes.length / 2)] ^= 1;
      fs.writeFileSync(file, bytes);
      for (let pass = 0; pass < 2; pass++) {
        const out: string[] = [];
        await handleStoreCommand(['check'], {
          out: (text) => out.push(text),
          err: () => undefined,
          env: { AAC_STORE_MODULE: MODULE },
        });
        expect(out.join('')).toContain('integrity finding(s)');
        expect(out.join('')).not.toContain('[OK]');
      }
    } finally {
      if (previous === undefined) delete process.env.CCS_HOME;
      else process.env.CCS_HOME = previous;
    }
  });

  it('imports the JSON history and every readable archive file, counting the rest', async () => {
    const home = fs.mkdtempSync(path.join(sandbox, 'backfill-'));
    const base = path.join(home, 'account-analytics');
    const archive = path.join(base, 'archive');
    fs.mkdirSync(path.join(archive, 'nested'), { recursive: true });
    // Older history in the archive, newer in the main file.
    let old: AccountAnalyticsHistoryData | null = null;
    for (let minute = 0; minute < 60; minute += 5)
      old = appendAccountAnalyticsSnapshot(
        old,
        accountsAt(START + minute * MIN),
        START + minute * MIN
      );
    let current: AccountAnalyticsHistoryData | null = null;
    for (let minute = 60; minute < 120; minute += 5)
      current = appendAccountAnalyticsSnapshot(
        current,
        accountsAt(START + minute * MIN),
        START + minute * MIN
      );
    await new FileAccountAnalyticsHistoryStore(home).write(current as AccountAnalyticsHistoryData);
    const records = (old as AccountAnalyticsHistoryData).records;
    fs.writeFileSync(path.join(archive, 'quota-history-2026-10-21.json'), JSON.stringify(old));
    fs.writeFileSync(
      path.join(archive, 'part.jsonl'),
      [
        JSON.stringify(records[0]),
        '{not json',
        JSON.stringify({ identity: 'bad' }),
        JSON.stringify(records[1]),
      ].join('\n')
    );
    fs.writeFileSync(
      path.join(archive, 'nested', 'old.json.gz'),
      zlib.gzipSync(JSON.stringify(records.slice(2, 4)))
    );
    fs.writeFileSync(path.join(archive, 'garbage.bin'), Buffer.from([0, 255, 1, 2, 3]));
    fs.writeFileSync(path.join(archive, 'broken.json.gz'), Buffer.from('not gzip'));
    fs.writeFileSync(path.join(archive, 'empty.json'), '');
    const store = await openStore(path.join(home, 'store'));
    try {
      const report = await backfillUsageHistory(store, home);
      expect(report.filesUnreadable).toBe(2); // garbage.bin and broken.json.gz
      expect(report.files).toBe(5); // 4 readable archive files and the main file
      expect(report.recordsSkipped).toBe(2); // the two bad JSON lines
      expect(report.rejected).toBe(0);
      expect(report.sampled).toBeGreaterThan(0);
      expect(report.duplicates).toBeGreaterThan(0); // the archive's overlapping copies
      const all = await store.historyRecords({
        fromMs: START,
        toMs: START + 2 * HOUR,
        nowMs: START + 2 * HOUR,
        thinning: { kind: 'none' },
      });
      const times = all.map((r) => Date.parse(r.sampledAt));
      expect(Math.min(...times)).toBe(START);
      expect(Math.max(...times)).toBe(START + 115 * MIN);
      // A second run imports nothing again, and the sources are untouched.
      const before = fs.readFileSync(path.join(base, 'quota-history-v1.json'));
      const again = await backfillUsageHistory(store, home);
      expect(again.filesAlreadyImported).toBe(5);
      expect(again.records).toBe(0);
      expect(fs.readFileSync(path.join(base, 'quota-history-v1.json'))).toEqual(before);
    } finally {
      await store.close();
    }
  });
});

describe.skipIf(!haveModule)('a store from an unreleased test build', () => {
  it('is refused with a clear message, left as it is, and the CLI says so', async () => {
    // The CLI opens <CCS_HOME>/.ccs/store: give it a test build's files (logs, no release marker).
    const home = fs.mkdtempSync(path.join(sandbox, 'unreleased-'));
    const previous = process.env.CCS_HOME;
    process.env.CCS_HOME = home;
    const dir = path.join(home, '.ccs', 'store');
    fs.mkdirSync(path.join(dir, 'log', 'readings'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'log', 'instance-id'), '0'.repeat(32));
    try {
      if (!loaded.module) throw new Error('no module');
      const handle = new loaded.module.NativeStore();
      await expect(handle.call('open', JSON.stringify({ dir }))).rejects.toThrow(
        'unreleased_store: This store was written by an unreleased test build; move it aside and AAC will start a new one.'
      );
      const err: string[] = [];
      const code = await handleStoreCommand(['status'], {
        out: () => undefined,
        err: (t: string) => err.push(t),
        env: { AAC_STORE_MODULE: MODULE },
      });
      expect(code).toBe(1);
      expect(err.join('')).toContain(
        'This store was written by an unreleased test build; move it aside and AAC will start a new one.'
      );
      // Nothing was deleted or marked.
      expect(fs.existsSync(path.join(dir, 'release'))).toBe(false);
      expect(fs.readFileSync(path.join(dir, 'log', 'instance-id'), 'utf8')).toBe('0'.repeat(32));
    } finally {
      if (previous === undefined) delete process.env.CCS_HOME;
      else process.env.CCS_HOME = previous;
    }
  });
});
