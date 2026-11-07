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
import express from 'express';
import type { Server } from 'http';
import { loadNativeStore, nativeModuleName } from '../../../src/store/native-loader';
import { AacStore } from '../../../src/store/store-service';
import { backfillUsageHistory } from '../../../src/store/usage-backfill';
import { storeObservationFromAccount } from '../../../src/store/usage-observations';
import {
  AccountAnalyticsService,
  type AccountAnalyticsSampleEvent,
  onAccountAnalyticsSample,
} from '../../../src/web-server/services/account-analytics-service';
import {
  appendAccountAnalyticsSnapshot,
  FileAccountAnalyticsHistoryStore,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsHistoryStore,
} from '../../../src/web-server/services/account-analytics-history';
import { usageHistory } from '../../../src/web-server/agent-usage/agent-usage-history';
import { projectAgentUsage } from '../../../src/web-server/agent-usage/agent-usage-projection';
import { buildSnapshotFiles } from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import { createUsageRouter } from '../../../src/web-server/routes/store-routes';
import type {
  AccountDashboard,
  DashboardAccount,
} from '../../../src/web-server/services/account-dashboard-types';
import type { AccountAnalyticsActivity } from '../../../src/web-server/services/account-analytics-types';

const MODULE = path.resolve(__dirname, '../../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: MODULE });
const haveModule = loaded.module !== null;
if (!haveModule)
  console.warn(`[!] ${MODULE} is not built: the store tests are skipped (bun run store:build).`);

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

const noActivity = {
  status: 'unavailable',
  scope: 'multi-host-cli',
  timezone: 'UTC',
  accountAttribution: 'unavailable',
  costBasis: 'estimated-api-equivalent',
  fetchedAt: null,
  message: 'none',
  totals: null,
  providers: [],
  byDay: [],
  byHour: [],
  models: [],
} as unknown as AccountAnalyticsActivity;

function dashboardAt(now: number): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: new Date(now).toISOString(),
    accounts: accountsAt(now),
    settings: { refreshIntervalSeconds: 60 },
  } as unknown as AccountDashboard;
}

function memory(): AccountAnalyticsHistoryStore & { data: AccountAnalyticsHistoryData | null } {
  return {
    data: null,
    async read() {
      return this.data;
    },
    async write(data) {
      this.data = structuredClone(data);
    },
  };
}

/**
 * A day and more of three accounts across every case the analytics route draws: five-hour resets,
 * an error hour without windows, a cached window, a stretch with no samples at all, and a credit
 * balance. Long enough to cross the JSON history's thinning steps (every record in the last hour,
 * five-minute buckets to a day, hourly beyond) and the 24-hour range's start.
 */
const RICH_MINUTES = 28 * 60;
const richSkipped = (minute: number) => minute >= 15 * 60 && minute < 15 * 60 + 40;
function richAccountsAt(now: number): DashboardAccount[] {
  const minute = Math.round((now - START) / MIN);
  const period = Math.floor(minute / 300);
  const used = Math.min(100, 3 + Math.floor((minute % 300) / 6));
  const [first, second] = accountsAt(START);
  const at = new Date(now).toISOString();
  const five = {
    ...first.windows[0],
    usedPercent: used,
    remainingPercent: 100 - used,
    resetAt: new Date(START + (period + 1) * 300 * MIN).toISOString(),
  };
  const cachedHour = minute >= 10 * 60 && minute < 11 * 60;
  const week = {
    ...first.windows[1],
    usedPercent: 40 + Math.floor(minute / 240),
    remainingPercent: 60 - Math.floor(minute / 240),
    ...(cachedHour
      ? { status: 'cached', sampledAt: new Date(START + 10 * 60 * MIN - 5 * MIN).toISOString() }
      : {}),
  };
  const failing = minute >= 5 * 60 && minute < 6 * 60;
  const credits = { ...second.windows[0], remaining: 500 - Math.floor(minute / 30) * 2 };
  const third = {
    ...first,
    id: 'codex:third',
    label: 'third',
    isActive: false,
    windows: [{ ...five, usedPercent: 10 + (period % 3), remainingPercent: 90 - (period % 3) }],
  };
  return [
    { ...first, fetchedAt: at, sampledAt: at, windows: [five, week] },
    failing
      ? {
          ...second,
          status: 'error',
          message: 'refresh failed',
          fetchedAt: at,
          sampledAt: at,
          windows: [],
        }
      : { ...second, fetchedAt: at, sampledAt: at, windows: [credits] },
    { ...third, fetchedAt: at, sampledAt: at },
  ] as unknown as DashboardAccount[];
}

describe.skipIf(!haveModule)('history store with the native module', () => {
  it('serves the analytics route and burn rates as the JSON history did', async () => {
    const dir = fs.mkdtempSync(path.join(sandbox, 'equal-'));
    const store = await openStore(path.join(dir, 'store'));
    try {
      let clock = START;
      const jsonService = new AccountAnalyticsService({
        getDashboard: async () => dashboardAt(clock),
        getActivity: async () => ({ ...noActivity, coverage: null }) as never,
        createHistoryStore: () => memory(),
        now: () => clock,
        scope: () => path.join(dir, 'json'),
      });
      const storeService = new AccountAnalyticsService({
        getDashboard: async () => dashboardAt(clock),
        getActivity: async () => ({ ...noActivity, coverage: null }) as never,
        usageStore: async () => store,
        now: () => clock,
        scope: () => path.join(dir, 'store-scope'),
      });
      const events: AccountAnalyticsSampleEvent[] = [];
      const stop = onAccountAnalyticsSample((event) => {
        events.push(event);
      });
      for (let minute = 0; minute <= 180; minute++) {
        clock = START + minute * MIN;
        await jsonService.sample();
        await storeService.sample();
      }
      stop();
      const query = { platform: 'mac', range: '24h', provider: 'all', account: 'all' } as const;
      const fromJson = await jsonService.get({ ...query, tz: 'UTC' });
      const fromStore = await storeService.get({ ...query, tz: 'UTC' });
      expect(fromStore.history.status).toBe('ok');
      expect(fromStore.history.retentionDays).toBe(90);
      for (const account of fromJson.accounts) {
        const other = fromStore.accounts.find((candidate) => candidate.id === account.id);
        expect(other).toBeDefined();
        for (const window of account.windows) {
          const twin = other?.windows.find((candidate) => candidate.key === window.key);
          const value = (p: { usedPercent: number | null; remaining: number | null }) =>
            p.usedPercent ?? p.remaining;
          // The same measured value in every bucket both show, and the same latest reading.
          const bucket = fromJson.range.bucketMinutes * MIN;
          const byBucket = new Map(
            window.points.map((p) => [Math.floor(Date.parse(p.sampledAt) / bucket), value(p)])
          );
          for (const point of twin?.points ?? []) {
            const key = Math.floor(Date.parse(point.sampledAt) / bucket);
            if (byBucket.has(key)) expect(value(point)).toBe(byBucket.get(key) as number);
          }
          expect(value(twin?.points.at(-1) as never)).toBe(value(window.points.at(-1) as never));
          expect((twin?.points.length ?? 0) > 0).toBe(true);
        }
      }
      // Burn rates from the store-backed history equal the JSON history's for every span.
      const jsonEvent = events[events.length - 2];
      const storeEvent = events[events.length - 1];
      const rows = storeEvent.accounts;
      const projected = projectAgentUsage(rows, { now: clock, includeHidden: true }).accounts;
      const a = usageHistory(rows, projected, jsonEvent.history, clock);
      const b = usageHistory(rows, projected, storeEvent.history, clock);
      for (const [id, rates] of a.rates) {
        for (const [key, rate] of rates) {
          const other = b.rates.get(id)?.get(key);
          expect({ key, ...other }).toEqual({ key, ...rate });
        }
      }
      // The snapshot carries the 7- and 30-day burn only with the store.
      expect(storeEvent.longBurn?.size).toBeGreaterThan(0);
      const files = buildSnapshotFiles(storeEvent, 60);
      const burn = files.snapshot.accounts
        .flatMap((acc) => acc.windows)
        .find((w) => w.key === 'five_hour')?.burn;
      expect(burn && 'coverage7Days' in burn).toBe(true);
      expect(burn?.last7Days).toBeNull(); // three hours of readings cover under half of 7 days
      const jsonFiles = buildSnapshotFiles(jsonEvent, 60);
      const jsonBurn = jsonFiles.snapshot.accounts
        .flatMap((acc) => acc.windows)
        .find((w) => w.key === 'five_hour')?.burn;
      expect(jsonBurn && 'last7Days' in jsonBurn).toBe(false);
      // Reset periods and consumption across the reset.
      const status = await store.status();
      expect(status.state).toBe('ready');
    } finally {
      await store.close();
    }
  });

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

  it('draws every analytics point the JSON history draws, across thinning, gaps and failures', async () => {
    const dir = fs.mkdtempSync(path.join(sandbox, 'parity-'));
    const store = await openStore(path.join(dir, 'store'));
    try {
      let clock = START;
      const dashboard = async () =>
        ({
          schemaVersion: 1,
          updatedAt: new Date(clock).toISOString(),
          accounts: richAccountsAt(clock),
          settings: { refreshIntervalSeconds: 60 },
        }) as unknown as AccountDashboard;
      const activity = async () => ({ ...noActivity, coverage: null }) as never;
      const jsonService = new AccountAnalyticsService({
        getDashboard: dashboard,
        getActivity: activity,
        createHistoryStore: () => memory(),
        now: () => clock,
        scope: () => path.join(dir, 'json'),
      });
      const storeService = new AccountAnalyticsService({
        getDashboard: dashboard,
        getActivity: activity,
        usageStore: async () => store,
        now: () => clock,
        scope: () => path.join(dir, 'store-scope'),
      });
      for (let minute = 0; minute <= RICH_MINUTES; minute += 2) {
        if (richSkipped(minute)) continue;
        clock = START + minute * MIN;
        await jsonService.sample();
        await storeService.sample();
      }
      // The store rebuilds records on the JSON history's grid inside each observed span, not at
      // each sample's own minute, so points are compared by analytics bucket: the same buckets in
      // the same order (gaps included), with every field equal and both times inside the bucket.
      const shape = (result: Awaited<ReturnType<typeof jsonService.get>>) => {
        const bucket = result.range.bucketMinutes * MIN;
        const slot = (iso: string) => Math.floor(Date.parse(iso) / bucket);
        return result.accounts.map((account) => ({
          id: account.id,
          windows: account.windows.map((window) => ({
            key: window.key,
            points: window.points.map(({ sampledAt, observedAt, ...fields }) => ({
              bucket: slot(sampledAt),
              observedBucket: slot(observedAt),
              ...fields,
            })),
          })),
        }));
      };
      for (const range of ['24h', '7d'] as const) {
        const query = {
          platform: 'mac',
          range,
          provider: 'all',
          account: 'all',
          tz: 'UTC',
        } as const;
        const fromJson = await jsonService.get(query);
        const fromStore = await storeService.get(query);
        // The store keeps every reading, so its history may start before the thinned file's.
        const { availableFrom, ...jsonRange } = fromJson.range;
        const { availableFrom: storeFrom, ...storeRange } = fromStore.range;
        expect(storeRange).toEqual(jsonRange);
        expect(Date.parse(storeFrom as string)).toBeLessThanOrEqual(
          Date.parse(availableFrom as string)
        );
        const expected = shape(fromJson);
        // The fixture really has what it claims: many points, and the cached window among them.
        const points = expected.flatMap((a) => a.windows.flatMap((w) => w.points));
        expect(points.length).toBeGreaterThan(range === '24h' ? 300 : 100);
        if (range === '7d') expect(points.some((p) => p.status === 'cached')).toBe(true);
        // Every account, window and point: membership, order, values and gaps.
        expect(shape(fromStore)).toEqual(expected);
      }
    } finally {
      await store.close();
    }
  }, 60_000);

  it('the burn route finds every series, however many hourly points they hold', async () => {
    const dir = fs.mkdtempSync(path.join(sandbox, 'burn-'));
    const store = await openStore(path.join(dir, 'store'));
    const end = START + 90 * 24 * HOUR;
    let server: Server | null = null;
    try {
      // 11 series with a reading every hour for 90 days: 23,760 hourly points, over 20,000.
      const [first] = accountsAt(START);
      for (let account = 0; account < 11; account++) {
        const observations = [];
        for (let t = START; t < end; t += HOUR) {
          const used = Math.floor((t - START) / HOUR) % 90;
          const observation = storeObservationFromAccount(
            {
              ...first,
              id: `codex:burn${account}`,
              label: `burn${account}`,
              fetchedAt: new Date(t).toISOString(),
              sampledAt: new Date(t).toISOString(),
              windows: [{ ...first.windows[0], usedPercent: used, remainingPercent: 100 - used }],
            } as DashboardAccount,
            t,
            90 * 24 * HOUR
          );
          if (observation) observations.push(observation);
        }
        await store.record(observations as never);
      }
      const app = express();
      app.use((req, _res, next) => {
        Object.assign(req, { auth: { kind: 'device' } });
        next();
      });
      app.use('/api/usage', createUsageRouter({ store: () => store, now: () => end }));
      const listening = await new Promise<Server>((resolve) => {
        const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
      });
      server = listening;
      const address = listening.address();
      if (!address || typeof address === 'string') throw new Error('No test listener');
      const response = await fetch(`http://127.0.0.1:${address.port}/api/usage/burn?span=90d`);
      expect(response.status).toBe(200);
      const body = (await response.json()) as { series: Array<{ accountId: string }> };
      expect(body.series.map((s) => s.accountId).sort()).toEqual(
        Array.from({ length: 11 }, (_, i) => `codex:burn${i}`).sort()
      );
    } finally {
      const open = server as Server | null;
      if (open) await new Promise<void>((resolve) => open.close(() => resolve()));
      await store.close();
    }
  }, 60_000);

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

describe('the dashboard without the native module', () => {
  it('keeps the JSON history path when no store is available', async () => {
    const dir = fs.mkdtempSync(path.join(sandbox, 'nostore-'));
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboardAt(START),
      getActivity: async () => ({ ...noActivity, coverage: null }) as never,
      usageStore: async () => null,
      createHistoryStore: () => memory(),
      now: () => START,
      scope: () => dir,
    });
    const result = await service.get({
      platform: 'mac',
      range: '24h',
      provider: 'all',
      account: 'all',
      tz: 'UTC',
    });
    expect(result.history.retentionDays).toBe(30);
    expect(result.accounts.length).toBe(2);
  });
});
