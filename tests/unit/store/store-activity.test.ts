import { loadStoreTestModule } from '../../support/store-handle';
/**
 * Analytics usage history in the store (docs/store.md section 12), with the real native module
 * (dist/native, built by `bun run store:build`). Skipped, and said so, when this computer has no
 * module. Every store lives in a temporary folder; never the real home.
 */
import { afterAll,beforeAll,describe,expect,it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { nativeModuleName } from '../../../src/store/native-loader';
import { AacStore } from '../../../src/store/store-service';
import {
ActivityIngestor,
buildActivityReports,
rowsReport,
} from '../../../src/store/activity-history';
import { aggregateRows } from '../../../src/web-server/usage/account-activity-collector';
import type { CompactEntry } from '../../../src/web-server/usage/types';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';
import type {
AccountAnalyticsActivityProvider,
AccountAnalyticsQuery
} from '../../../src/web-server/services/account-analytics-types';

const MODULE = path.resolve(__dirname, '../../../dist/native', nativeModuleName());
const loaded = loadStoreTestModule({ env: {}, file: MODULE });
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

  it('a record dated after now never stops live writes (the review probe, through the module)', async () => {
    const store = await openStore('future');
    const now = Date.now();
    const hour = Math.floor(now / H) * H;
    const ahead = hour + 10 * DAY;
    const report = (readAt: number, cells: Array<[number, number]>) => ({
      schema: 'aac.usage-report.v1' as const,
      computer: 'ubuntu',
      tool: 'codex',
      readAt: new Date(readAt).toISOString(),
      state: 'ok' as const,
      complete: true,
      coverFrom: new Date(Math.floor((readAt - 31 * DAY) / H) * H).toISOString(),
      cells: cells.map(([at, input]) => ({
        hour: new Date(at).toISOString(),
        sessionKey: '0123456789abcdef',
        route: '',
        modelRaw: 'gpt-6',
        inputTokens: input,
        outputTokens: 0,
        cacheCreationTokens: 0,
        cacheReadTokens: 0,
        costUsd: 0,
        fallbackCostUsd: 0,
        events: 1,
      })),
      sessions: [],
    });
    // a sender that does not filter: the store leaves the record out itself
    expect((await store.activityIngest([report(ahead, [[ahead, 5]])])).put).toBe(0);
    expect((await store.activityIngest([report(now, [[hour, 100]])])).put).toBe(1);
    const range = await store.activityRange({ fromMs: hour - H, toMs: hour + H });
    expect(range.rows.reduce((n, r) => n + r[5], 0)).toBe(100);
    const sources = await store.activitySources({ fromMs: hour - DAY, toMs: hour });
    expect(sources.sources[0].lastReadMs).toBeLessThanOrEqual(Date.now());
    await store.close();
  });

  it('stores no collector usage for a tool only the scanner reads, and keeps its honest state (recheck 2 M1)', async () => {
    const store = await openStore('scanner-only');
    // the reviewer's input: 999 collector tokens under the tool's name, an empty scanner answer
    for (const tool of ['cursor', 'devin'] as const)
      for (const owned of [undefined, new Set<string>()]) {
        const reports = buildActivityReports({
          local: new Map([
            [
              tool as unknown as AccountAnalyticsActivityProvider,
              [worker('codex', [{ at: NOW - 10 * 60_000, session: sessions.a, model: 'gpt-6', input: 999 }])],
            ],
          ]),
          remote: [],
          states: [
            {
              tool: tool as 'cursor',
              host: 'ubuntu',
              state: 'no_usage',
              rowCount: 0,
              lastScanAt: new Date(NOW).toISOString(),
              detail: null,
            },
          ],
          windowFromMs: NOW - DAY,
          now: NOW,
          localComplete: () => false,
          ...(owned ? { scannerOwned: owned } : {}),
        });
        const result = await store.activityIngest(reports);
        expect(result.refused).toEqual([]);
        expect(result.put).toBe(0);
      }
    const range = await store.activityRange({ fromMs: NOW - DAY, toMs: NOW });
    expect(range.rows).toEqual([]);
    const sources = await store.activitySources({ fromMs: NOW - DAY, toMs: NOW });
    for (const tool of ['cursor', 'devin']) {
      const source = sources.sources.find((s) => s.computer === 'ubuntu' && s.tool === tool);
      expect(source?.state).toBe('no_usage');
      expect(source?.complete).toBe(true);
    }
    // the hours the scanner read count as read: no usage, not a gap
    expect(sources.computers.find((c) => c.computer === 'ubuntu')?.gaps).toEqual([]);
    await store.close();
  });
});
