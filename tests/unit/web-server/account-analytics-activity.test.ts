import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AccountAnalyticsActivityService,
  loadAccountAnalyticsWorker,
  projectAccountAnalyticsActivity,
} from '../../../src/web-server/services/account-analytics-activity';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import type { AccountAnalyticsQuery } from '../../../src/web-server/services/account-analytics-types';
import type {
  UsageWorkerRequest,
  UsageWorkerResult,
} from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-10-01T16:30:00Z');
const FROM = NOW - 86_400_000;
const QUERY: AccountAnalyticsQuery = {
  platform: 'mac',
  range: '24h',
  provider: 'all',
  account: 'all',
};
function data(model: string, input: number, cost: number): UsageWorkerResult {
  return {
    daily: [],
    monthly: [],
    eventCount: 99,
    hourly: [
      {
        hour: '2026-10-01 15:00',
        source: 'native',
        inputTokens: input,
        outputTokens: 5,
        cacheCreationTokens: 2,
        cacheReadTokens: 3,
        cost,
        totalCost: cost,
        modelsUsed: [model],
        requestCount: 4,
        modelBreakdowns: [
          {
            modelName: model,
            inputTokens: input,
            outputTokens: 5,
            cacheCreationTokens: 2,
            cacheReadTokens: 3,
            cost,
          },
        ],
      },
    ],
    session: [
      {
        sessionId: 'shared-id',
        projectPath: '/private/project',
        inputTokens: input,
        outputTokens: 5,
        cacheCreationTokens: 2,
        cacheReadTokens: 3,
        cost,
        totalCost: cost,
        lastActivity: '2026-10-01T15:45:00Z',
        versions: [],
        modelsUsed: [model],
        modelBreakdowns: [],
        source: 'native',
      },
    ],
  };
}
function sources() {
  return [
    {
      provider: 'claude' as const,
      data: [data('claude-sonnet-4-6', 100, 0.005)],
      fetchedAt: new Date(NOW).toISOString(),
    },
    {
      provider: 'codex' as const,
      data: [data('gpt-5.4', 200, 0.01)],
      fetchedAt: new Date(NOW).toISOString(),
    },
  ];
}

describe('native local analytics activity', () => {
  it('publishes real bounded progress, then warm completion, while keeping shared logs unattributed', async () => {
    let now = NOW;
    let complete = false;
    const service = new AccountAnalyticsActivityService({
      now: () => now,
      scope: () => 'progress',
      responseBudgetMs: 100,
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => ({
        ...data('gpt-5.4', 100, 0.01),
        scan: {
          complete,
          completedFiles: complete ? 2 : 1,
          totalFiles: 2,
          skippedLines: 0,
          failedFiles: 0,
          readBytes: 100,
          unfinishedFiles: 0,
        },
      }),
    });
    const partial = await service.get(QUERY, FROM, NOW);
    expect(partial.status).toBe('cached');
    expect(partial.totals?.inputTokens).toBe(100);
    expect(partial.message).toContain('1 of 2 local log files');
    expect(partial.accountAttribution).toBe('unavailable');
    complete = true;
    now += 60000;
    const warm = await service.get(QUERY, FROM, NOW);
    expect(warm.status).toBe('ok');
    expect(warm.totals).toEqual(partial.totals);
    expect(warm.message).not.toContain('remaining files');
  });
  it('labels old unfinished tails separately from remaining history and never invents cold partial zeros', async () => {
    let events = false;
    const service = new AccountAnalyticsActivityService({
      now: () => NOW,
      scope: () => 'unfinished',
      responseBudgetMs: 100,
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () =>
        events
          ? {
              ...data('gpt-5.4', 100, 0.01),
              scan: {
                complete: false,
                completedFiles: 1,
                totalFiles: 2,
                skippedLines: 0,
                failedFiles: 0,
                readBytes: 100,
                unfinishedFiles: 1,
              },
            }
          : {
              daily: [],
              hourly: [],
              monthly: [],
              session: [],
              eventCount: 0,
              scan: {
                complete: false,
                completedFiles: 0,
                totalFiles: 2,
                skippedLines: 0,
                failedFiles: 0,
                readBytes: 100,
              },
            },
    });
    const empty = await service.get(QUERY, FROM, NOW);
    expect(empty.status).toBe('unavailable');
    expect(empty.totals).toBeNull();
    expect(empty.providers).toEqual([]);
    events = true;
    const tail = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(tail.status).toBe('cached');
    expect(tail.totals?.inputTokens).toBe(100);
    expect(tail.message).toContain('1 local log files end with unfinished records');
    expect(tail.message).not.toContain('remaining files resume');
  });
  it('reads a real bounded native worker and excludes proxy-backed Codex rollout records', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-analytics-worker-'));
    try {
      const sessions = path.join(root, 'codex', 'sessions');
      fs.mkdirSync(sessions, { recursive: true });
      for (const provider of ['openai', 'cliproxy']) {
        const input = provider === 'openai' ? 100 : 100_000;
        const lines = [
          {
            timestamp: '2026-10-01T15:00:00Z',
            type: 'session_meta',
            payload: { id: provider, model_provider: provider, cwd: '/private-project' },
          },
          {
            timestamp: '2026-10-01T15:00:01Z',
            type: 'turn_context',
            payload: { model: 'gpt-5.4' },
          },
          {
            timestamp: '2026-10-01T15:05:00Z',
            type: 'event_msg',
            payload: {
              type: 'token_count',
              info: {
                total_token_usage: {
                  input_tokens: input,
                  output_tokens: 5,
                  cached_input_tokens: 20,
                  reasoning_output_tokens: 2,
                },
              },
            },
          },
        ];
        fs.writeFileSync(
          path.join(sessions, `rollout-${provider}.jsonl`),
          lines.map((line) => JSON.stringify(line)).join('\n')
        );
      }
      const result = await runWithScopedCcsHome(root, () =>
        loadAccountAnalyticsWorker({
          kind: 'codex',
          codexHome: path.join(root, 'codex'),
          cacheDir: path.join(root, 'cache'),
        })
      );
      expect(result.eventCount).toBe(1);
      expect(result.hourly[0].inputTokens).toBe(80);
      expect(result.hourly[0].outputTokens).toBe(5);
      expect(result.hourly[0].cacheReadTokens).toBe(20);
      expect(result.hourly[0].source).toBe('codex-native');
      const projected = projectAccountAnalyticsActivity(
        [{ provider: 'codex', data: [result], fetchedAt: new Date(NOW).toISOString() }],
        QUERY,
        FROM,
        NOW,
        'ok',
        'Native fixture'
      );
      expect(projected.totals?.inputTokens).toBe(80);
      expect(
        (projected.totals?.inputTokens ?? 0) +
          (projected.totals?.outputTokens ?? 0) +
          (projected.totals?.cacheReadTokens ?? 0)
      ).toBe(105);
      expect(JSON.stringify(projected)).not.toContain('/private-project');
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
  it('retains measured native tool dimensions, token/model breakdowns, UTC buckets and estimate labels', () => {
    const result = projectAccountAnalyticsActivity(
      sources(),
      QUERY,
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(result.totals).toMatchObject({
      inputTokens: 300,
      outputTokens: 10,
      cacheCreationTokens: 4,
      cacheReadTokens: 6,
      estimatedCostUsd: 0.015,
    });
    expect(result.totals?.costByType).not.toBeNull();
    expect(
      result.providers.map((row) => [
        row.provider,
        row.totals.inputTokens,
        row.usageEvents,
        row.sessionCount,
      ])
    ).toEqual([
      ['claude', 100, 4, 1],
      ['codex', 200, 4, 1],
    ]);
    expect(result.byDay.map((row) => [row.date, row.provider, row.inputTokens])).toEqual([
      ['2026-10-01', 'claude', 100],
      ['2026-10-01', 'codex', 200],
    ]);
    expect(result.byHour[0].hour).toBe('2026-10-01T15:00:00Z');
    expect(result.models.map((row) => row.model)).toEqual(['gpt-5.4', 'claude-sonnet-4-6']);
    expect(result.costBasis).toBe('estimated-api-equivalent');
    expect(result.timezone).toBe('UTC');
    expect(result.accountAttribution).toBe('unavailable');
    expect(result.scope).toBe('ubuntu-local-cli');
    expect(JSON.stringify(result)).not.toContain('/private/project');
    expect(JSON.stringify(result)).not.toContain('shared-id');
  });

  it('filters provider before adding totals and never assigns shared local logs to an active account', () => {
    const provider = projectAccountAnalyticsActivity(
      sources(),
      { ...QUERY, provider: 'codex' },
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(provider.totals?.inputTokens).toBe(200);
    expect(provider.providers).toHaveLength(1);
    const account = projectAccountAnalyticsActivity(
      sources(),
      { ...QUERY, account: 'codex:active' },
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(account.status).toBe('unavailable');
    expect(account.totals).toBeNull();
    expect(account.byDay).toEqual([]);
    expect(account.message).toContain('do not reliably identify');
    const qwen = projectAccountAnalyticsActivity(
      sources(),
      { ...QUERY, provider: 'qwen' },
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(qwen.status).toBe('unavailable');
    expect(qwen.totals).toBeNull();
    expect(qwen.message).toContain('quota and balance');
  });

  it('excludes old/future or invalid hourly keys instead of inventing filled daily records', () => {
    const fixture = sources();
    fixture[0].data[0].hourly.push({
      ...fixture[0].data[0].hourly[0],
      hour: '2026-10-03 00:00',
      inputTokens: 1e6,
    });
    fixture[0].data[0].hourly.push({
      ...fixture[0].data[0].hourly[0],
      hour: '2026-09-01 00:00',
      inputTokens: 1e6,
    });
    fixture[0].data[0].hourly.push({
      ...fixture[0].data[0].hourly[0],
      hour: 'invalid',
      inputTokens: 1e6,
    });
    const result = projectAccountAnalyticsActivity(
      fixture,
      QUERY,
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(result.totals?.inputTokens).toBe(300);
    expect(result.byDay).toHaveLength(2);
    expect(result.byHour).toHaveLength(2);
  });

  it('coalesces background reads and returns loading within the response budget', async () => {
    let resolve!: (result: UsageWorkerResult) => void;
    const pending = new Promise<UsageWorkerResult>((complete) => {
      resolve = complete;
    });
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return pending;
      },
      now: () => NOW,
      scope: () => '/fixture',
      responseBudgetMs: 5,
    });
    const [first, second] = await Promise.all([
      service.get(QUERY, FROM, NOW),
      service.get(QUERY, FROM, NOW),
    ]);
    expect(first.status).toBe('loading');
    expect(second.status).toBe('loading');
    expect(calls).toBe(1);
    expect(first.totals).toBeNull();
    resolve(data('gpt-5.4', 20, 0.001));
    await pending;
    await new Promise((complete) => setTimeout(complete, 0));
    const completed = await service.get(QUERY, FROM, NOW);
    expect(completed.status).toBe('ok');
    expect(completed.totals?.inputTokens).toBe(20);
    expect(calls).toBe(1);
  });

  it('refreshes both native providers before the normal cache expires, while ordinary reads stay cached', async () => {
    let input = 10;
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return data('fixture-model', input, 0);
      },
      now: () => NOW,
      scope: () => '/fixture',
    });
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(20);
    input = 25;
    expect((await service.get({ ...QUERY, refresh: false }, FROM, NOW)).totals?.inputTokens).toBe(
      20
    );
    expect(calls).toBe(2);
    const refreshed = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(refreshed.status).toBe('ok');
    expect(refreshed.totals?.inputTokens).toBe(50);
    expect(refreshed.providers.map((row) => row.provider)).toEqual(['claude', 'codex']);
    expect(calls).toBe(4);
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(50);
    expect(calls).toBe(4);
  });

  it('uses the saved refresh interval for ordinary reads and picks up an interval change immediately', async () => {
    let now = NOW;
    let refreshIntervalSeconds = 120;
    let input = 10;
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return data('gpt-5.4', input, 0);
      },
      now: () => now,
      refreshIntervalSeconds: () => refreshIntervalSeconds,
      scope: () => '/fixture',
    });
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(10);
    input = 20;
    now += 119_999;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(10);
    expect(calls).toBe(1);
    now++;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect(calls).toBe(2);
    input = 30;
    now += 30_000;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect(calls).toBe(2);
    refreshIntervalSeconds = 30;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(30);
    expect(calls).toBe(3);
  });

  it('coalesces concurrent manual refreshes into one bounded scan and exposes cached data while it runs', async () => {
    let pending = false;
    let calls = 0;
    let live = 0;
    let maximum = 0;
    const resolvers: Array<(value: UsageWorkerResult) => void> = [];
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        if (!pending) return data('fixture-model', 10, 0);
        live++;
        maximum = Math.max(maximum, live);
        return new Promise((resolve) => {
          resolvers.push((value) => {
            live--;
            resolve(value);
          });
        });
      },
      now: () => NOW,
      scope: () => '/fixture',
      responseBudgetMs: 5,
    });
    await service.get(QUERY, FROM, NOW);
    pending = true;
    const reads = await Promise.all(
      Array.from({ length: 40 }, () => service.get({ ...QUERY, refresh: true }, FROM, NOW))
    );
    expect(calls).toBe(4);
    expect(maximum).toBe(2);
    expect(reads.every((row) => row.status === 'cached' && row.totals?.inputTokens === 20)).toBe(
      true
    );
    expect(reads[0].message).toContain('refreshing');
    resolvers.forEach((resolve) => resolve(data('fixture-model', 30, 0)));
    await new Promise((complete) => setTimeout(complete, 0));
    const completed = await service.get(QUERY, FROM, NOW);
    expect(completed.status).toBe('ok');
    expect(completed.totals?.inputTokens).toBe(60);
    expect(calls).toBe(4);
  });

  it('queues one fresh generation behind an old scan and prevents its obsolete result from overwriting the cache', async () => {
    let now = NOW;
    let calls = 0;
    const resolvers: Array<(value: UsageWorkerResult) => void> = [];
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        if (calls === 1) return data('gpt-5.4', 10, 0);
        return new Promise((resolve) => resolvers.push(resolve));
      },
      now: () => now,
      scope: () => '/fixture',
      responseBudgetMs: 5,
    });
    await service.get(QUERY, FROM, now);
    now += 6 * 60_000;
    await service.get(QUERY, FROM, now);
    expect(calls).toBe(2);
    await Promise.all(
      Array.from({ length: 20 }, () => service.get({ ...QUERY, refresh: true }, FROM, now))
    );
    expect(calls).toBe(2);
    resolvers[0](data('gpt-5.4', 999, 0));
    await new Promise((complete) => setTimeout(complete, 0));
    expect(calls).toBe(3);
    const during = await service.get(QUERY, FROM, now);
    expect(during.status).toBe('cached');
    expect(during.totals?.inputTokens).toBe(10);
    expect(during.fetchedAt).toBe(new Date(NOW).toISOString());
    await service.get({ ...QUERY, refresh: true }, FROM, now);
    expect(calls).toBe(3);
    resolvers[1](data('gpt-5.4', 30, 0));
    await new Promise((complete) => setTimeout(complete, 0));
    const completed = await service.get(QUERY, FROM, now);
    expect(completed.status).toBe('ok');
    expect(completed.totals?.inputTokens).toBe(30);
    expect(completed.fetchedAt).toBe(new Date(now).toISOString());
    expect(calls).toBe(3);
  });

  it('refreshes all native sources even when the selected account cannot show attributable activity', async () => {
    let input = 10;
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return data('fixture-model', input, 0);
      },
      now: () => NOW,
      scope: () => '/fixture',
    });
    await service.get(QUERY, FROM, NOW);
    input = 40;
    const selected = await service.get(
      { ...QUERY, provider: 'qwen', account: 'qwen:first', refresh: true },
      FROM,
      NOW
    );
    expect(selected.status).toBe('unavailable');
    expect(selected.totals).toBeNull();
    expect(calls).toBe(4);
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(80);
    expect(calls).toBe(4);
  });

  it('keeps last usable records and their original timestamp when an explicit refresh fails', async () => {
    let now = NOW;
    let fail = false;
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
      ],
      loadWorker: async () => {
        if (fail) throw new Error('raw secret source sentinel');
        return data('claude-sonnet-4-6', 10, 0.002);
      },
      now: () => now,
      scope: () => '/fixture',
    });
    await service.get(QUERY, FROM, now);
    now += 1000;
    fail = true;
    const retained = await service.get({ ...QUERY, refresh: true }, FROM, now);
    expect(retained.status).toBe('cached');
    expect(retained.totals?.inputTokens).toBe(10);
    expect(retained.fetchedAt).toBe(new Date(NOW).toISOString());
    expect(JSON.stringify(retained)).not.toContain('sentinel');
  });

  it('retains the last usable native history on later worker failures without exposing raw error details', async () => {
    let now = NOW;
    let fail = false;
    const service = new AccountAnalyticsActivityService({
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
      ],
      loadWorker: async () => {
        if (fail) throw new Error('raw secret source sentinel');
        return data('claude-sonnet-4-6', 10, 0.002);
      },
      now: () => now,
      scope: () => '/fixture',
      responseBudgetMs: 20,
    });
    expect((await service.get(QUERY, FROM, NOW)).status).toBe('ok');
    now += 6 * 60_000;
    fail = true;
    const retained = await service.get(QUERY, FROM, now);
    expect(retained.status).toBe('cached');
    expect(retained.totals?.inputTokens).toBe(10);
    expect(retained.fetchedAt).toBe(new Date(NOW).toISOString());
    expect(JSON.stringify(retained)).not.toContain('sentinel');
  });

  it('does not start native workers for unsupported provider or exact-account filters', async () => {
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      requests: () => {
        calls++;
        return [];
      },
      now: () => NOW,
    });
    const qwen = await service.get({ ...QUERY, provider: 'qwen' }, FROM, NOW);
    const account = await service.get({ ...QUERY, account: 'codex:active' }, FROM, NOW);
    expect(calls).toBe(0);
    expect(qwen.totals).toBeNull();
    expect(account.totals).toBeNull();
  });

  it('runs at most two native parsers concurrently even across many local roots', async () => {
    const resolvers: Array<(data: UsageWorkerResult) => void> = [];
    let live = 0;
    let maximum = 0;
    const requests: Array<{ provider: 'claude'; request: UsageWorkerRequest }> = Array.from(
      { length: 4 },
      (_, index) => ({
        provider: 'claude',
        request: { kind: 'claude', projectsDir: `/fixture/${index}` },
      })
    );
    const service = new AccountAnalyticsActivityService({
      requests: () => requests,
      now: () => NOW,
      responseBudgetMs: 5,
      loadWorker: async () => {
        live++;
        maximum = Math.max(maximum, live);
        return new Promise((resolve) => {
          resolvers.push((result) => {
            live--;
            resolve(result);
          });
        });
      },
    });
    await service.get(QUERY, FROM, NOW);
    expect(resolvers).toHaveLength(2);
    expect(maximum).toBe(2);
    resolvers[0](data('claude-sonnet-4-6', 1, 0));
    resolvers[1](data('claude-sonnet-4-6', 1, 0));
    await new Promise((complete) => setTimeout(complete, 0));
    expect(resolvers).toHaveLength(4);
    expect(maximum).toBe(2);
    resolvers[2](data('claude-sonnet-4-6', 1, 0));
    resolvers[3](data('claude-sonnet-4-6', 1, 0));
    await new Promise((complete) => setTimeout(complete, 0));
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(4);
  });
});
