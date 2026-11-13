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
} from '../../../src/store/activity-history';
import { FUTURE_SLACK_MS, type ActivityRange } from '../../../src/store/usage-report';
import { cellsFromCompact } from '../../../src/web-server/usage/activity-cells';
import { aggregateRows } from '../../../src/web-server/usage/account-activity-collector';
import { projectAccountAnalyticsActivity } from '../../../src/web-server/services/account-analytics-projection';
import type { CompactEntry } from '../../../src/web-server/usage/types';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';
import type { AccountAnalyticsQuery } from '../../../src/web-server/services/account-analytics-types';

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
