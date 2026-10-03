import { describe, expect, it } from 'bun:test';
import {
  AccountAnalyticsActivityService,
  type AccountAnalyticsActivityDeps,
} from '../../../src/web-server/services/account-analytics-activity';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

const FROM = Date.parse('2026-10-01T00:00:00Z');
const TO = Date.parse('2026-10-02T00:00:00Z');
const NOW = Date.parse('2026-10-02T00:00:00Z');

function workerResult(
  model: string,
  tokens: {
    input: number;
    output: number;
    read: number;
    write: number;
    cost: number;
    fallback?: number;
    /** The route the log recorded (OMP `message.provider`, zcode `provider_id`). */
    route?: string;
  }
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
        ...(tokens.fallback !== undefined && { fallbackCost: tokens.fallback }),
        modelsUsed: [model],
        modelBreakdowns: [
          {
            modelName: model,
            inputTokens: tokens.input,
            outputTokens: tokens.output,
            cacheCreationTokens: tokens.write,
            cacheReadTokens: tokens.read,
            cost: tokens.cost,
            ...(tokens.fallback !== undefined && { fallbackCost: tokens.fallback }),
            ...(tokens.route !== undefined && { provider: tokens.route }),
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

async function remoteAnswer() {
  return {
    results: [
      {
        tool: 'omp',
        data: workerResult('remote-model-b', {
          input: 70,
          output: 7,
          read: 0,
          write: 0,
          cost: 4,
          route: 'kimi-code',
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
  };
}

function service(overrides: AccountAnalyticsActivityDeps = {}) {
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
      // a local inference server: a route no dashboard provider claims
      return workerResult('no-such-model-xyz', {
        input: 50,
        output: 5,
        read: 0,
        write: 0,
        cost: 2,
        fallback: 1.5,
        route: 'vllm',
      });
    },
    remote: remoteAnswer,
    ...overrides,
  });
}

const QUERY = {
  platform: 'mac' as const,
  range: '7d' as const,
  provider: 'all' as const,
  account: 'all',
};

describe('analytics activity across sources', () => {
  it('merges local and remote sources and groups them by the provider that served them', async () => {
    const activity = await service().get(QUERY, FROM, TO, { tz: 'UTC' });
    expect(activity.scope).toBe('multi-host-cli');
    expect(activity.totals?.inputTokens).toBe(220);
    expect(activity.totals?.estimatedCostUsd).toBe(7);
    const models = new Map(activity.models.map((row) => [`${row.provider}:${row.model}`, row]));
    expect(models.get('claude:claude-model-a')?.inputTokens).toBe(100);
    // OMP usage counts under its logged route: kimi-code is Kimi Code; vllm is no provider's, so "other"
    expect(models.get('other:no-such-model-xyz')?.inputTokens).toBe(50);
    expect(models.get('kimi-code:remote-model-b')?.inputTokens).toBe(70);
    expect(models.get('kimi-code:remote-model-b')?.tools).toEqual(['omp']);
    const dayModels = new Map(
      activity.byDayModel.map((row) => [`${row.provider}:${row.model}`, row])
    );
    expect(dayModels.get('kimi-code:remote-model-b')?.outputTokens).toBe(7);
    expect(activity.providers.map((row) => [row.provider, row.label, row.tools])).toEqual([
      ['claude', 'Claude', ['claude']],
      ['kimi-code', 'Kimi Code', ['omp']],
      ['other', 'Other', ['omp']],
    ]);
    // the tool names never become a provider value
    expect(activity.byHour.every((row) => !['omp', 'muse', 'zcode'].includes(row.provider))).toBe(
      true
    );
  });

  it('shows unknown-rate models as not logged, never as confident splits', async () => {
    const activity = await service().get(QUERY, FROM, TO, { tz: 'UTC' });
    const unknown = activity.models.find((row) => row.model === 'no-such-model-xyz');
    expect(unknown?.rates?.source).toBe('fallback');
    expect(unknown?.costByType).toBeNull();
    expect(activity.totals?.costByType).toBeNull();
    // The fallback-priced part travels with every total, so the page shows it as not logged.
    expect(unknown?.fallbackCostUsd).toBe(1.5);
    expect(activity.totals?.fallbackCostUsd).toBe(1.5);
    expect(activity.providers.find((row) => row.provider === 'other')?.totals.fallbackCostUsd).toBe(
      1.5
    );
    expect(activity.byHour.find((row) => row.provider === 'other')?.fallbackCostUsd).toBe(1.5);
    const known = activity.models.find((row) => row.model === 'claude-model-a');
    expect(known?.fallbackCostUsd).toBe(0);
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

  it('keeps the remote part of the totals when a later remote scan does not answer', async () => {
    let calls = 0;
    const activity = service({
      remote: async () => {
        calls++;
        if (calls > 1) throw new Error('timed out');
        return remoteAnswer();
      },
    });
    const first = await activity.get(QUERY, FROM, TO, { tz: 'UTC' });
    expect(first.totals?.inputTokens).toBe(220);
    const second = await activity.get({ ...QUERY, refresh: true }, FROM, TO, { tz: 'UTC' });
    expect(calls).toBe(2);
    // The Mac's OMP usage stays in the totals, and its cell says it is cached.
    expect(second.totals?.inputTokens).toBe(220);
    expect(second.models.find((row) => row.model === 'remote-model-b')?.inputTokens).toBe(70);
    const mac = second.sources.find((row) => row.tool === 'omp' && row.host === 'mac');
    expect(mac).toMatchObject({ state: 'cached', rowCount: 3 });
    expect(mac?.detail).toContain('timed out');
  });

  it('falls back to the saved remote aggregates when no remote answer has come yet', async () => {
    const saved = await remoteAnswer();
    const activity = service({
      remote: async () => {
        throw new Error('timed out');
      },
      remoteCached: () => ({
        results: saved.results,
        states: saved.states.map((entry) =>
          entry.rowCount > 0
            ? {
                ...entry,
                state: 'cached' as const,
                detail: 'remote scan timed out; showing previously read aggregates',
              }
            : entry
        ),
      }),
    });
    const result = await activity.get(QUERY, FROM, TO, { tz: 'UTC' });
    expect(result.totals?.inputTokens).toBe(220);
    expect(result.sources.find((row) => row.tool === 'omp' && row.host === 'mac')?.state).toBe(
      'cached'
    );
  });
});
