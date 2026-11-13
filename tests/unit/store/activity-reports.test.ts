/**
 * Analytics usage history (docs/store.md section 12): what the TypeScript side forwards and reads
 * back, without the native module. A record dated after now is counted and never sent, so the
 * store's 72-hour settle line never passes now (review H1); usage whose output counts reasoning
 * twice (the retired Codex cache, review M1) stays in the totals and out of model comparisons.
 */
import { describe, expect, it } from 'bun:test';
import {
  buildActivityReports,
  rangeToSources,
  rowsReport,
  SCANNER_ONLY_TOOLS,
} from '../../../src/store/activity-history';
import { FUTURE_SLACK_MS, type ActivityRange } from '../../../src/store/usage-report';
import { cellsFromCompact } from '../../../src/web-server/usage/activity-cells';
import { aggregateRows } from '../../../src/web-server/usage/account-activity-collector';
import { projectAccountAnalyticsActivity } from '../../../src/web-server/services/account-analytics-projection';
import type { CompactEntry } from '../../../src/web-server/usage/types';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';
import type {
  AccountAnalyticsActivityProvider,
  AccountAnalyticsQuery,
  AccountAnalyticsSource,
} from '../../../src/web-server/services/account-analytics-types';

const H = 3_600_000;
const DAY = 86_400_000;
const NOW = Date.parse('2026-11-13T16:30:00Z');

function rows(uses: Array<{ at: number; input: number; session?: string }>): CompactEntry[] {
  return uses.map((u) => ({
    entry: {
      inputTokens: u.input,
      outputTokens: 10,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      model: 'gpt-6',
      sessionId: u.session ?? 'session-a',
      timestamp: new Date(u.at).toISOString(),
      projectPath: '',
      target: 'codex',
    },
    events: 1,
  }));
}

function worker(uses: Array<{ at: number; input: number; session?: string }>): UsageWorkerResult {
  const list = rows(uses);
  return {
    daily: [],
    monthly: [],
    ...aggregateRows(list, 'codex-native', 'codex'),
    eventCount: list.length,
  } as UsageWorkerResult;
}

describe('a record dated after now (review H1)', () => {
  it('is left out of the cells and counted', () => {
    const future = { count: 0 };
    const cells = cellsFromCompact(
      rows([
        { at: NOW - H, input: 100 },
        { at: NOW + FUTURE_SLACK_MS - 1, input: 7 },
        { at: NOW + 10 * DAY, input: 5 },
      ]),
      'codex',
      false,
      { now: NOW, future }
    );
    expect(cells.reduce((n, c) => n + c.inputTokens, 0)).toBe(107);
    expect(future.count).toBe(1);
  });

  it('never moves the read time past now, and the report says how many it left out', () => {
    // the review's probe: a read whose newest record is ten days ahead
    const ahead = NOW + 10 * DAY;
    const reports = buildActivityReports({
      local: new Map([
        [
          'codex',
          [
            worker([
              { at: NOW - H, input: 100 },
              { at: ahead, input: 5, session: 'session-b' },
            ]),
          ],
        ],
      ]),
      remote: [],
      states: [
        {
          tool: 'codex',
          host: 'ubuntu',
          state: 'ok',
          lastScanAt: new Date(ahead).toISOString(),
          rowCount: 2,
          detail: null,
        },
      ],
      windowFromMs: NOW - 31 * DAY,
      now: NOW,
      localComplete: () => true,
    });
    const [report] = reports;
    expect(Date.parse(report.readAt)).toBeLessThanOrEqual(NOW);
    expect(report.complete).toBe(true);
    expect(report.cells.every((c) => Date.parse(c.hour) <= NOW + FUTURE_SLACK_MS)).toBe(true);
    expect(report.cells.reduce((n, c) => n + c.inputTokens, 0)).toBe(100);
    // the worker ran on the real clock, so it may have counted the record before the builder did
    expect(report.counts?.futureDated).toBe(1);
    // a session that starts after now is not sent; none ends after now plus the slack
    expect(report.sessions.every((s) => Date.parse(s.last) <= NOW + FUTURE_SLACK_MS)).toBe(true);
    expect(Date.parse(report.coverFrom ?? '')).toBeLessThanOrEqual(NOW);
  });

  it('is counted from old cells too, and a deep read never claims a later read time', () => {
    const data: UsageWorkerResult = {
      daily: [],
      monthly: [],
      hourly: [],
      session: [],
      eventCount: 2,
      cells: [
        {
          hour: new Date(NOW - 2 * H).toISOString(),
          sessionKey: null,
          route: '',
          modelRaw: 'gpt-6',
          inputTokens: 3,
          outputTokens: 0,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          events: 1,
        },
        {
          hour: new Date(NOW + 2 * DAY).toISOString(),
          sessionKey: null,
          route: '',
          modelRaw: 'gpt-6',
          inputTokens: 4,
          outputTokens: 0,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          events: 2,
        },
      ],
    };
    const report = rowsReport({
      computer: 'mac',
      tool: 'codex',
      data: [data],
      kind: 'backfill',
      readAt: NOW + DAY,
      complete: true,
      now: NOW,
    });
    expect(Date.parse(report.readAt)).toBe(NOW);
    expect(report.cells.map((c) => c.inputTokens)).toEqual([3]);
    expect(report.counts?.futureDated).toBe(2);
  });
});

describe('output that counts reasoning twice (review M1)', () => {
  const range: ActivityRange = {
    columns: [],
    computers: [],
    tools: ['codex'],
    routes: [''],
    models: ['gpt-5.6-sol', 'gpt-6'],
    rows: [
      // the same model and hour, imported from the retired cache (flagged) and read live (clean)
      [NOW - 3 * H, null, 0, 0, 0, 100, 900, 0, 50, null, null, null, 3, 1],
      [NOW - 3 * H, null, 0, 0, 0, 10, 20, 0, 0, null, null, null, 1, 0],
      [NOW - 2 * H, null, 0, 0, 1, 40, 4, 0, 0, null, null, null, 1, 0],
    ],
    quality: [
      {
        tool: 'codex',
        flags: ['outputIncludesReasoningTwice'],
        fromMs: NOW - 3 * H,
        toMs: NOW - 2 * H,
      },
    ],
    sessions: { total: 0, signatures: [], sample: [], truncated: false },
  };

  it('keeps flawed and clean rows of one model apart', () => {
    const [source] = rangeToSources(range, (b) => b, new Date(NOW).toISOString());
    const hour = source.data[0].hourly.find(
      (h) => h.modelBreakdowns.length === 2 && h.inputTokens === 110
    );
    expect(hour?.modelBreakdowns.map((b) => [b.modelName, b.outputTokens, b.quality ?? 0])).toEqual(
      [
        ['gpt-5.6-sol', 900, 1],
        ['gpt-5.6-sol', 20, 0],
      ]
    );
  });

  it('stays in the totals and out of the model comparison', () => {
    const sources = rangeToSources(range, (b) => b, new Date(NOW).toISOString());
    const query: AccountAnalyticsQuery = {
      platform: 'mac',
      range: '24h',
      provider: 'all',
      account: 'all',
      tz: 'UTC',
    };
    const projected = projectAccountAnalyticsActivity(sources, query, NOW - DAY, NOW, 'ok', '', {
      tz: 'UTC',
    });
    expect(projected.totals.outputTokens).toBe(900 + 20 + 4);
    const sol = projected.models.find((m) => m.model === 'gpt-5.6-sol');
    // only the clean part of the model is compared: input exact either way, output without the double count
    expect(sol?.inputTokens).toBe(10);
    expect(sol?.outputTokens).toBe(20);
    expect(projected.models.find((m) => m.model === 'gpt-6')?.outputTokens).toBe(4);
  });
});

describe('flagged usage in the day-level views (recheck 1 L-c)', () => {
  it('is marked apart in byDayModel and kept out of the day-level cost spike', () => {
    const day = (n: number) => Date.parse('2026-09-01T10:00:00Z') + n * DAY;
    const nano = (usd: number) => Math.round(usd * 1e9);
    const range: ActivityRange = {
      columns: [],
      computers: [],
      tools: ['codex'],
      routes: [''],
      models: ['gpt-5.6-sol'],
      rows: [
        // day 0: the old cache, inflated output; days 0 to 3: clean live reads of the same model
        [day(0), null, 0, 0, 0, 100, 9000, 0, 0, null, nano(50), 0, 3, 1],
        [day(0), null, 0, 0, 0, 10, 10, 0, 0, null, nano(1), 0, 1, 0],
        [day(1), null, 0, 0, 0, 10, 10, 0, 0, null, nano(1), 0, 1, 0],
        [day(2), null, 0, 0, 0, 10, 10, 0, 0, null, nano(1), 0, 1, 0],
        [day(3), null, 0, 0, 0, 10, 10, 0, 0, null, nano(1), 0, 1, 0],
      ],
      sessions: { total: 0, signatures: [], sample: [], truncated: false },
    };
    const sources = rangeToSources(range, (b) => b, new Date(day(4)).toISOString());
    const query: AccountAnalyticsQuery = {
      platform: 'mac',
      range: '7d',
      provider: 'all',
      account: 'all',
      tz: 'UTC',
    };
    const projected = projectAccountAnalyticsActivity(
      sources,
      query,
      day(0) - DAY,
      day(4),
      'ok',
      '',
      { tz: 'UTC' }
    );
    const first = projected.byDayModel.filter((r) => r.date === '2026-09-01');
    expect(first.map((r) => [r.outputTokens, r.quality ?? null])).toEqual([
      [9000, ['outputIncludesReasoningTwice']],
      [10, null],
    ]);
    // the day with the flagged hours is no cost spike (its total keeps the inflated output cost)
    expect(projected.anomalies?.items.filter((i) => i.type === 'cost_spike')).toEqual([]);
    // and the totals still hold it
    expect(projected.totals?.outputTokens).toBe(9000 + 40);
  });
});

describe('reads of the usage scanner (aac-usage-scan) on this computer (Stage 1 review M3)', () => {
  const state = (
    tool: 'codex' | 'cursor' | 'claude',
    host: 'ubuntu' | 'mac',
    st: 'ok' | 'no_usage' = 'ok'
  ) => ({
    tool,
    host,
    state: st,
    lastScanAt: new Date(NOW - 60_000).toISOString(),
    rowCount: 1,
    detail: null,
  });
  const base = {
    windowFromMs: NOW - 31 * DAY,
    now: NOW,
    localComplete: () => false,
  };
  const input = (r: { cells: Array<{ inputTokens: number }> }) =>
    r.cells.reduce((n, c) => n + c.inputTokens, 0);

  it('keeps the scanner rows of the tools it owns, never the collector rows as well', () => {
    const reports = buildActivityReports({
      ...base,
      // the collector still holds older Codex rows; the scanner owns Codex and reads Cursor
      local: new Map([['codex', [worker([{ at: NOW - 2 * H, input: 999 }])]]]),
      remote: [
        { tool: 'codex', host: 'ubuntu', data: worker([{ at: NOW - H, input: 100 }]) },
        { tool: 'cursor', data: { ...worker([{ at: NOW - H, input: 40 }]), host: 'ubuntu' } },
        { tool: 'codex', host: 'mac', data: worker([{ at: NOW - H, input: 7 }]) },
      ],
      states: [state('codex', 'ubuntu'), state('cursor', 'ubuntu'), state('codex', 'mac')],
      scannerOwned: new Set(['codex', 'cursor']),
    });
    const of = (tool: string, host: string) =>
      reports.find((r) => r.tool === tool && r.computer === host)!;
    expect(input(of('codex', 'ubuntu'))).toBe(100);
    expect(input(of('cursor', 'ubuntu'))).toBe(40);
    expect(input(of('codex', 'mac'))).toBe(7);
    // a scanner read that finished is complete, like a remote one; the collector's flag does not apply
    expect(of('codex', 'ubuntu').complete).toBe(true);
    expect(of('cursor', 'ubuntu').complete).toBe(true);
  });

  it('reads an owned tool with no usage as the scanner says, and leaves other tools to the collector', () => {
    const reports = buildActivityReports({
      ...base,
      localComplete: (tool) => tool === 'claude',
      local: new Map([
        ['codex', [worker([{ at: NOW - 2 * H, input: 999 }])]],
        ['claude', [worker([{ at: NOW - H, input: 5 }])]],
      ]),
      remote: [],
      states: [state('codex', 'ubuntu', 'no_usage'), state('claude', 'ubuntu')],
      scannerOwned: new Set(['codex']),
    });
    const codex = reports.find((r) => r.tool === 'codex')!;
    expect(codex.cells).toEqual([]);
    expect(codex.complete).toBe(true);
    const claude = reports.find((r) => r.tool === 'claude')!;
    expect(input(claude)).toBe(5);
    expect(claude.complete).toBe(true);
  });
});

/**
 * Recheck 2 M1: the reviewer's synthetic input. The collector holds 999 input tokens under a tool
 * only the scanner reads; the scanner answered with no rows and says the tool has no usage.
 */
function reviewerInput(
  tool: 'cursor' | 'devin' | 'codex',
  opts: {
    scannerOwned?: ReadonlySet<string>;
    state?: 'no_usage' | 'ok';
    now?: number;
    localComplete?: boolean;
  } = {}
) {
  const now = opts.now ?? NOW;
  return buildActivityReports({
    local: new Map([
      [
        tool as unknown as AccountAnalyticsActivityProvider,
        [worker([{ at: now - 10 * 60_000, input: 999 }])],
      ],
    ]),
    remote: [],
    states: [
      {
        tool: tool as AccountAnalyticsSource['tool'],
        host: 'ubuntu',
        state: opts.state ?? 'no_usage',
        rowCount: 0,
        lastScanAt: new Date(now).toISOString(),
        detail: null,
      },
    ],
    windowFromMs: now - DAY,
    now,
    localComplete: () => opts.localComplete ?? false,
    ...(opts.scannerOwned ? { scannerOwned: opts.scannerOwned } : {}),
  });
}

describe('tools only the usage scanner reads (recheck 2 M1)', () => {
  it('are Cursor and Devin', () => {
    expect([...SCANNER_ONLY_TOOLS].sort()).toEqual(['cursor', 'devin']);
  });

  for (const tool of ['cursor', 'devin'] as const)
    for (const [label, owned] of [
      ['absent', undefined],
      ['an empty set', new Set<string>()],
    ] as const)
      it(`${tool}: an empty scanner answer is no usage, never the collector rows (scannerOwned ${label})`, () => {
        const [report] = reviewerInput(tool, { scannerOwned: owned });
        expect(report.computer).toBe('ubuntu');
        expect(report.cells).toEqual([]);
        expect(report.sessions).toEqual([]);
        // the scanner's own measured state, complete, never turned into ok or partial by collector cells
        expect(report.state).toBe('no_usage');
        expect(report.complete).toBe(true);
        // even a collector that says it finished does not bring its rows back
        expect(reviewerInput(tool, { scannerOwned: owned, localComplete: true })[0].cells).toEqual(
          []
        );
      });

  it('Codex keeps the collector before the scanner takes it over, and only the scanner after', () => {
    const tokens = (r: { cells: Array<{ inputTokens: number }> }) =>
      r.cells.reduce((n, c) => n + c.inputTokens, 0);
    // before take-over (ownership absent or empty, no scanner rows): the collector's read
    for (const owned of [undefined, new Set<string>()]) {
      const [unfinished] = reviewerInput('codex', { scannerOwned: owned, state: 'ok' });
      expect(tokens(unfinished)).toBe(999);
      expect(unfinished.state).toBe('partial');
      expect(unfinished.complete).toBe(false);
      const [finished] = reviewerInput('codex', {
        scannerOwned: owned,
        state: 'ok',
        localComplete: true,
      });
      expect(tokens(finished)).toBe(999);
      expect(finished.complete).toBe(true);
    }
    // after take-over with an empty answer: no collector rows, the scanner's no usage
    const [after] = reviewerInput('codex', { scannerOwned: new Set(['codex']) });
    expect(after.cells).toEqual([]);
    expect(after.state).toBe('no_usage');
    expect(after.complete).toBe(true);
  });
});
