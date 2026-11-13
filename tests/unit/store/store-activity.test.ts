/**
 * Analytics usage history in the store (docs/store.md section 12), with the real native module
 * (dist/native, built by `bun run store:build`). Skipped, and said so, when this computer has no
 * module. Every store lives in a temporary folder; never the real home.
 */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { loadNativeStore, nativeModuleName } from '../../../src/store/native-loader';
import { AacStore } from '../../../src/store/store-service';
import { ActivityIngestor, rowsReport } from '../../../src/store/activity-history';
import {
  AccountAnalyticsActivityService,
  type AccountAnalyticsActivityRequest,
} from '../../../src/web-server/services/account-analytics-activity';
import { AccountAnalyticsService } from '../../../src/web-server/services/account-analytics-service';
import { aggregateRows } from '../../../src/web-server/usage/account-activity-collector';
import type { CompactEntry } from '../../../src/web-server/usage/types';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsActivityProvider,
  AccountAnalyticsQuery,
} from '../../../src/web-server/services/account-analytics-types';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import type { AnalyticsRemoteSourceState } from '../../../src/web-server/services/analytics-remote-sources';

const MODULE = path.resolve(__dirname, '../../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: MODULE });
if (!loaded.module)
  console.warn(`[!] ${MODULE} is not built: the activity history tests are skipped (bun run store:build).`);
const maybe = loaded.module ? describe : describe.skip;

const H = 3_600_000;
const DAY = 86_400_000;
const NOW = Date.parse('2026-11-13T16:30:00Z');

let sandbox = '';
let previousHome: string | undefined;
beforeAll(() => {
  previousHome = process.env.CCS_HOME;
  sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-activity-'));
  process.env.CCS_HOME = sandbox;
});
afterAll(() => {
  if (previousHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previousHome;
  fs.rmSync(sandbox, { recursive: true, force: true });
});

async function openStore(name: string): Promise<AacStore> {
  const dir = path.join(sandbox, name, 'store');
  fs.mkdirSync(path.dirname(dir), { recursive: true });
  const handle = new loaded.module!.NativeStore();
  await handle.call('open', JSON.stringify({ dir, retentionDays: 90 }));
  return new AacStore(handle, dir);
}

interface Use {
  at: number;
  session: string;
  model: string;
  input: number;
  route?: string;
}

/** One read's worker result from raw usage records, through the collectors' own aggregation. */
function worker(tool: AccountAnalyticsActivityProvider, uses: Use[], complete = true): UsageWorkerResult {
  const rows: CompactEntry[] = uses.map((u) => ({
    entry: {
      inputTokens: u.input,
      outputTokens: 10,
      cacheCreationTokens: 0,
      cacheReadTokens: 100,
      model: u.model,
      sessionId: u.session,
      timestamp: new Date(u.at).toISOString(),
      projectPath: '',
      target: tool,
      ...(u.route !== undefined ? { provider: u.route } : {}),
    },
    events: 1,
  }));
  return {
    daily: [],
    monthly: [],
    ...aggregateRows(rows, `${tool}-native`, tool),
    eventCount: rows.length,
    scan: { complete, completedFiles: 1, totalFiles: 1, skippedLines: 0, failedFiles: 0, readBytes: 1 },
  } as UsageWorkerResult;
}

const QUERY: AccountAnalyticsQuery = { platform: 'mac', range: '7d', provider: 'all', account: 'all', tz: 'UTC' };

async function settle(service: AccountAnalyticsActivityService, from: number, to: number): Promise<AccountAnalyticsActivity> {
  let result = await service.get(QUERY, from, to, { tz: 'UTC' });
  for (let i = 0; i < 200 && (result.refreshing || result.status === 'loading'); i++) {
    await new Promise((resolve) => setTimeout(resolve, 10));
    result = await service.get(QUERY, from, to, { tz: 'UTC' });
  }
  await service.historyIdle();
  return result;
}

function activityService(
  store: AacStore | null,
  local: Partial<Record<AccountAnalyticsActivityProvider, () => UsageWorkerResult>>,
  remote: () => { results: Array<{ tool: 'codex' | 'claude'; data: UsageWorkerResult }>; states: AnalyticsRemoteSourceState[] }
): AccountAnalyticsActivityService {
  const requests: AccountAnalyticsActivityRequest[] = Object.keys(local).map((tool) => ({
    provider: tool as AccountAnalyticsActivityProvider,
    request: tool === 'codex' ? { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture' } : { kind: 'claude', projectsDir: '/fixture' },
  }));
  return new AccountAnalyticsActivityService({
    scope: () => sandbox,
    now: () => NOW,
    requests: () => requests,
    loadWorker: async (request) => (local[request.kind as AccountAnalyticsActivityProvider] as () => UsageWorkerResult)(),
    remote: async () => remote(),
    ...(store ? { historyStore: async () => store } : {}),
  });
}

const sessions = {
  a: '0f0e0d0c-aaaa-4bbb-8ccc-000000000001',
  b: '0f0e0d0c-aaaa-4bbb-8ccc-000000000002',
  c: '0f0e0d0c-aaaa-4bbb-8ccc-000000000003',
};

function fixture() {
  const claude = worker('claude', [
    { at: NOW - 2 * DAY - 3 * H, session: sessions.a, model: 'claude-opus-5-5', input: 1000 },
    { at: NOW - 2 * DAY - 3 * H + 60_000, session: sessions.a, model: 'claude-opus-5-5', input: 500 },
    { at: NOW - 3 * H, session: sessions.b, model: 'claude-sonnet-5-5', input: 700 },
  ]);
  const codex = worker('codex', [{ at: NOW - DAY - 5 * H, session: sessions.c, model: 'gpt-6', input: 2000 }]);
  const mac = { ...worker('codex', [{ at: NOW - 4 * DAY, session: sessions.c, model: 'gpt-6', input: 300 }]), host: 'mac' };
  return {
    local: { claude: () => claude, codex: () => codex },
    remote: () => ({
      results: [{ tool: 'codex' as const, data: mac }],
      states: [
        { tool: 'codex' as const, host: 'mac' as const, state: 'ok' as const, lastScanAt: new Date(NOW - 60_000).toISOString(), rowCount: 1, detail: null },
      ],
    }),
  };
}

maybe('analytics usage history', () => {
  it('answers from the store with the same numbers as the in-memory projection, and a re-read never adds', async () => {
    const store = await openStore('equal');
    const f = fixture();
    const memory = await settle(activityService(null, f.local, f.remote), NOW - 7 * DAY, NOW);
    const service = activityService(store, f.local, f.remote);
    await settle(service, NOW - 7 * DAY, NOW);
    const stored = await settle(service, NOW - 7 * DAY, NOW);
    expect(stored.totals).toEqual(memory.totals);
    expect(stored.byDay).toEqual(memory.byDay);
    expect(stored.byHour).toEqual(memory.byHour);
    expect(stored.models).toEqual(memory.models);
    expect(stored.providers.map((p) => [p.provider, p.totals, p.usageEvents])).toEqual(
      memory.providers.map((p) => [p.provider, p.totals, p.usageEvents])
    );
    expect(stored.sessions?.total).toBe(3);
    const before = await store.activityCounts();
    // the same logs read again, by a manual refresh
    await service.get({ ...QUERY, refresh: true }, NOW - 7 * DAY, NOW, { tz: 'UTC' });
    const again = await settle(service, NOW - 7 * DAY, NOW);
    expect(again.totals).toEqual(memory.totals);
    expect(await store.activityCounts()).toEqual(before);
    await store.close();
  });

  it('skips days it already sent unchanged, and a re-read replaces rather than adds', async () => {
    const store = await openStore('ingestor');
    const ingestor = new ActivityIngestor(async () => store);
    const report = (input: number) =>
      rowsReport({
        computer: 'ubuntu',
        tool: 'codex',
        data: [worker('codex', [{ at: NOW - 3 * DAY, session: sessions.c, model: 'gpt-6', input }, { at: NOW - H, session: sessions.c, model: 'gpt-6', input: 5 }])],
        kind: 'backfill',
        readAt: NOW,
        complete: false,
      });
    await ingestor.write([report(100)]);
    const sent: unknown[] = [];
    const spy = new ActivityIngestor(async () => ({
      ...store,
      activityIngest: async (reports: unknown[]) => {
        sent.push(...reports);
        return store.activityIngest(reports as never);
      },
    }) as unknown as AacStore);
    await spy.write([report(100)]);
    await spy.write([report(100)]);
    const second = sent[1] as { cells: unknown[]; sameDays?: number[] };
    expect(second.cells).toHaveLength(0);
    expect(second.sameDays?.length).toBe(2);
    await ingestor.write([report(150)]);
    const range = await store.activityRange({ fromMs: NOW - 7 * DAY, toMs: NOW });
    expect(range.rows.reduce((n, r) => n + r[5], 0)).toBe(155);
    await store.close();
  });

  it('keeps history past the 31-day window: 90D, All and a custom range across months read the store', async () => {
    const store = await openStore('long');
    const old = Date.parse('2026-08-20T10:00:00Z');
    const result = await store.activityIngest([
      rowsReport({
        computer: 'ubuntu',
        tool: 'codex',
        data: [worker('codex', [{ at: old, session: sessions.c, model: 'gpt-6', input: 4000 }, { at: NOW - 40 * DAY, session: sessions.c, model: 'gpt-6', input: 60 }])],
        kind: 'backfill',
        readAt: NOW,
        complete: true,
      }),
    ]);
    expect(result.refused).toEqual([]);
    const activity = activityService(store, {}, () => ({ results: [], states: [] }));
    const analytics = (now = NOW) =>
      new AccountAnalyticsService({
        getDashboard: async () =>
          ({ schemaVersion: 1, updatedAt: new Date(now).toISOString(), settings: { refreshIntervalSeconds: 60 }, accounts: [], providers: [] }) as unknown as AccountDashboard,
        createHistoryStore: () => ({ read: async () => null, write: async () => undefined }),
        getActivity: (query, from, to, options) => activity.get(query, from, to, options),
        storedHistory: async () => ({ activity: await store.activityOldest(), quota: null }),
        now: () => now,
        scope: () => path.join(sandbox, 'long-scope'),
      });
    fs.mkdirSync(path.join(sandbox, 'long-scope'), { recursive: true });
    const ninety = await analytics().get({ ...QUERY, range: '90d' });
    expect(ninety.range.preset).toBe('90d');
    expect(Date.parse(ninety.range.from)).toBe(NOW - 90 * DAY);
    expect(ninety.activity.totals?.inputTokens).toBe(4060);
    expect(ninety.range.resolution).toBe('hour');
    const all = await analytics().get({ ...QUERY, range: 'all' });
    expect(Date.parse(all.range.availableFrom)).toBe(Date.parse('2026-08-20T10:00:00Z'));
    expect(all.activity.totals?.inputTokens).toBe(4060);
    const custom = await analytics().get({ ...QUERY, range: 'custom', from: '2026-08-20', to: '2026-10-10' });
    expect(custom.activity.totals?.inputTokens).toBe(4060);
    expect(custom.activity.byDay.map((d) => d.date)).toEqual(['2026-08-20', '2026-10-04']);
    // before the oldest stored day: refused, as without the store a range over 31 days is
    await expect(analytics().get({ ...QUERY, range: 'custom', from: '2026-08-01', to: '2026-08-02' })).rejects.toThrow();
    await store.close();
  });

  it('answers ranges over 92 days in local days', async () => {
    const store = await openStore('days');
    await store.activityIngest([
      rowsReport({
        computer: 'ubuntu',
        tool: 'codex',
        data: [
          worker('codex', [
            // 03:00 UTC is still the previous day in New York
            { at: Date.parse('2026-06-02T03:00:00Z'), session: sessions.c, model: 'gpt-6', input: 7 },
            { at: Date.parse('2026-06-02T15:00:00Z'), session: sessions.c, model: 'gpt-6', input: 11 },
          ]),
        ],
        kind: 'backfill',
        readAt: NOW,
        complete: false,
      }),
    ]);
    const service = activityService(store, {}, () => ({ results: [], states: [] }));
    const from = Date.parse('2026-05-01T04:00:00Z');
    const result = await service.get({ ...QUERY, range: 'custom', tz: 'America/New_York' }, from, NOW, { tz: 'America/New_York' });
    expect((result as { resolution?: string }).resolution).toBe('day');
    expect(result.byDay.map((d) => [d.date, d.inputTokens])).toEqual([
      ['2026-06-01', 7],
      ['2026-06-02', 11],
    ]);
    await store.close();
  });

  it('shows an unreachable computer as a gap, never as zero', async () => {
    const store = await openStore('gaps');
    const day = Date.parse('2026-11-01T00:00:00Z');
    const mac = (readAt: number, state: 'ok' | 'unreadable', input: number) => ({
      schema: 'aac.usage-report.v1' as const,
      computer: 'mac',
      tool: 'claude',
      readAt: new Date(readAt).toISOString(),
      state,
      complete: state === 'ok',
      coverFrom: state === 'ok' ? new Date(readAt - DAY - (readAt % H)).toISOString() : null,
      cause: state === 'ok' ? null : 'ssh_unreachable',
      cells:
        state === 'ok'
          ? [{ hour: new Date(readAt - (readAt % H) - H).toISOString(), sessionKey: '00112233aabbccdd', route: '', modelRaw: 'claude-opus-5-5', inputTokens: input, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, costUsd: 0.01 }]
          : [],
      sessions: [],
    });
    await store.activityIngest([mac(day + 12 * H, 'ok', 5)]);
    for (const d of [1, 2, 3]) await store.activityIngest([mac(day + d * DAY + 12 * H, 'unreadable', 0)]);
    await store.activityIngest([mac(day + 4 * DAY + 12 * H, 'ok', 9)]);
    const service = activityService(store, {}, () => ({ results: [], states: [] }));
    const result = (await service.get(QUERY, day, day + 5 * DAY, { tz: 'UTC' })) as AccountAnalyticsActivity & {
      history?: { computers: Array<{ host: string; gaps: Array<[string, string]> }> };
    };
    expect(result.byDay.map((d) => d.date)).toEqual(['2026-11-01', '2026-11-05']);
    const gaps = result.history?.computers.find((c) => c.host === 'mac')?.gaps ?? [];
    expect(gaps[0]).toEqual(['2026-11-01T13:00:00.000Z', '2026-11-04T12:00:00.000Z']);
    await store.close();
  });

  it('collects in the background with no page open, so history keeps growing', async () => {
    const store = await openStore('background');
    const f = fixture();
    const service = activityService(store, f.local, f.remote);
    service.startBackground(1, 5);
    try {
      for (let i = 0; i < 200 && (await store.activityCounts()).cells === 0; i++)
        await new Promise((resolve) => setTimeout(resolve, 10));
      await service.historyIdle();
      expect((await store.activityCounts()).cells).toBeGreaterThan(0);
    } finally {
      service.stopBackground();
      await service.historyIdle();
      await store.close();
    }
  });

  it('backfills once: old caches first, then the logs as far back as they go, never lowering', async () => {
    const store = await openStore('backfill');
    const scope = path.join(sandbox, 'backfill-scope');
    fs.mkdirSync(scope, { recursive: true });
    const ancient = Date.parse('2026-02-03T05:00:00Z');
    let deepReads = 0;
    const service = new AccountAnalyticsActivityService({
      scope: () => scope,
      now: () => NOW,
      requests: () => [{ provider: 'codex', request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture' } }],
      loadWorker: async (request) => {
        deepReads++;
        // the backfill reads back to 2000-01-01, so the old month comes back too
        expect((request as { activity?: { minDate: number } }).activity?.minDate ?? Date.UTC(2000, 0, 1)).toBeLessThan(ancient);
        return worker('codex', [
          { at: ancient, session: sessions.c, model: 'gpt-6', input: 321 },
          { at: NOW - 2 * DAY, session: sessions.c, model: 'gpt-6', input: 5 },
        ]);
      },
      remote: async () => ({
        results: [{ tool: 'codex' as const, data: { ...worker('codex', [{ at: ancient + DAY, session: sessions.a, model: 'gpt-6', input: 9 }]), host: 'mac' } }],
        states: [{ tool: 'codex' as const, host: 'mac' as const, state: 'ok' as const, lastScanAt: new Date(NOW).toISOString(), rowCount: 1, detail: null }],
      }),
      historyStore: async () => store,
    });
    expect(await service.backfillPass(scope)).toBe('done');
    expect(deepReads).toBe(1);
    const range = await store.activityRange({ fromMs: ancient - DAY, toMs: NOW });
    expect(range.rows.reduce((n, r) => n + r[5], 0)).toBe(321 + 5 + 9);
    expect(await store.activityOldest()).toBe(ancient);
    const sources = await store.activitySources({ fromMs: ancient, toMs: NOW });
    expect(sources.sources.find((s) => s.computer === 'ubuntu')?.backfill?.state).toBe('done');
    // a finished backfill does not run again
    expect(await service.backfillPass(scope)).toBe('done');
    expect(deepReads).toBe(1);
    await store.close();
  });
});

