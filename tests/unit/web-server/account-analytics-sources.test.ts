import { describe, expect, it } from 'bun:test';
import { AccountAnalyticsActivityService } from '../../../src/web-server/services/account-analytics-activity';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

const FROM = Date.parse('2026-10-01T00:00:00Z');
const TO = Date.parse('2026-10-02T00:00:00Z');
const NOW = Date.parse('2026-10-02T00:00:00Z');

function workerResult(
  model: string,
  tokens: { input: number; output: number; read: number; write: number; cost: number }
): UsageWorkerResult {
  return {
    daily: [],
    monthly: [],
    hourly: [
      {
        hour: '2026-10-01 15:00',
        source: 'fixture',
        inputTokens: tokens.input,
        outputTokens: tokens.output,
        cacheCreationTokens: tokens.write,
        cacheReadTokens: tokens.read,
        cost: tokens.cost,
        totalCost: tokens.cost,
        modelsUsed: [model],
        modelBreakdowns: [
          {
            modelName: model,
            inputTokens: tokens.input,
            outputTokens: tokens.output,
            cacheCreationTokens: tokens.write,
            cacheReadTokens: tokens.read,
            cost: tokens.cost,
          },
        ],
        requestCount: 3,
      },
    ],
    session: [],
    eventCount: 3,
    scan: {
      complete: true,
      completedFiles: 1,
      totalFiles: 1,
      skippedLines: 0,
      failedFiles: 0,
      readBytes: 10,
    },
  };
}

function service() {
  return new AccountAnalyticsActivityService({
    now: () => NOW,
    requests: () => [
      {
        provider: 'claude',
        request: { kind: 'claude', projectsDir: '/fixture/claude', activity: undefined },
      },
      {
        provider: 'omp',
        request: { kind: 'omp', roots: ['/fixture/omp'], activity: undefined },
      },
    ],
    loadWorker: async (request) => {
      if (request.kind === 'claude')
        return workerResult('claude-model-a', {
          input: 100,
          output: 10,
          read: 0,
          write: 0,
          cost: 1,
        });
      return workerResult('no-such-model-xyz', {
        input: 50,
        output: 5,
        read: 0,
        write: 0,
        cost: 2,
      });
    },
    remote: async () => ({
      results: [
        {
          tool: 'omp',
          data: workerResult('remote-model-b', {
            input: 70,
            output: 7,
            read: 0,
            write: 0,
            cost: 4,
          }),
        },
      ],
      states: [
        {
          tool: 'omp',
          host: 'mac',
          state: 'ok',
          lastScanAt: new Date(NOW).toISOString(),
          rowCount: 3,
          detail: null,
        },
        {
          tool: 'omp',
          host: 'windows',
          state: 'unavailable',
          lastScanAt: null,
          rowCount: 0,
          detail: 'remote scan failed',
        },
      ],
    }),
  });
}

const QUERY = {
  platform: 'mac' as const,
  range: '7d' as const,
  provider: 'all' as const,
  account: 'all',
};

describe('analytics activity across sources', () => {
  it('merges local and remote sources into one model breakdown', async () => {
    const activity = await service().get(QUERY, FROM, TO, { tz: 'UTC' });
    expect(activity.scope).toBe('multi-host-cli');
    expect(activity.totals?.inputTokens).toBe(220);
    expect(activity.totals?.estimatedCostUsd).toBe(7);
    const models = new Map(activity.models.map((row) => [`${row.provider}:${row.model}`, row]));
    expect(models.get('claude:claude-model-a')?.inputTokens).toBe(100);
    expect(models.get('omp:no-such-model-xyz')?.inputTokens).toBe(50);
    expect(models.get('omp:remote-model-b')?.inputTokens).toBe(70);
    const dayModels = new Map(
      activity.byDayModel.map((row) => [`${row.provider}:${row.model}`, row])
    );
    expect(dayModels.get('omp:remote-model-b')?.outputTokens).toBe(7);
    expect(activity.providers.map((row) => row.provider).sort()).toEqual(['claude', 'omp']);
  });

  it('shows unknown-rate models as not logged, never as confident splits', async () => {
    const activity = await service().get(QUERY, FROM, TO, { tz: 'UTC' });
    const unknown = activity.models.find((row) => row.model === 'no-such-model-xyz');
    expect(unknown?.rates?.source).toBe('fallback');
    expect(unknown?.costByType).toBeNull();
    expect(activity.totals?.costByType).toBeNull();
  });

  it('lists every source state with fixed entries for missing tools', async () => {
    const activity = await service().get(QUERY, FROM, TO, { tz: 'UTC' });
    const sources = new Map(activity.sources.map((row) => [`${row.tool}:${row.host}`, row]));
    expect(sources.get('claude:ubuntu')).toMatchObject({ state: 'ok', rowCount: 3 });
    expect(sources.get('omp:ubuntu')).toMatchObject({ state: 'ok', rowCount: 3 });
    expect(sources.get('codex:ubuntu')?.state).toBe('not_installed');
    expect(sources.get('omp:mac')).toMatchObject({ state: 'ok', rowCount: 3 });
    expect(sources.get('omp:windows')?.state).toBe('unavailable');
    const antigravity = sources.get('antigravity:mac');
    expect(antigravity?.state).toBe('unavailable');
    expect(antigravity?.detail).toContain('no local usage log');
    expect(sources.get('cursor:windows')?.detail).toContain('no local usage log');
    expect(sources.get('muse:windows')?.state).toBe('not_installed');
  });

  it('describes the real multi-host coverage', async () => {
    const activity = await service().get(QUERY, FROM, TO, { tz: 'UTC' });
    expect(activity.message).not.toContain('Local Ubuntu CLI activity');
    expect(activity.message).toContain('Ubuntu, Mac and Windows');
  });
});
