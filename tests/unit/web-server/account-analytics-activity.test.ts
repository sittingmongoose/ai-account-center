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
function data(model: string, input: number, cost: number, route?: string): UsageWorkerResult {
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
            ...(route !== undefined && { provider: route }),
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
  // A refresh answers instantly from the last snapshot and runs behind it; poll until the answer
  // is settled (refreshing is false) to assert what the refresh collected.
  async function settled(
    service: AccountAnalyticsActivityService,
    query: AccountAnalyticsQuery = QUERY,
    from: number = FROM,
    to: number = NOW
  ) {
    for (let attempt = 0; attempt < 500; attempt++) {
      const answer = await service.get(query, from, to);
      if (answer.refreshing !== true) return answer;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    throw new Error('background collection did not settle');
  }
  it('publishes real bounded progress, then warm completion, while keeping shared logs unattributed', async () => {
    let now = NOW;
    let complete = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
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
    const warm = await settled(service);
    expect(warm.status).toBe('ok');
    expect(warm.totals).toEqual(partial.totals);
    expect(warm.message).not.toContain('remaining files');
  });
  it('labels old unfinished tails separately from remaining history and never invents cold partial zeros', async () => {
    let events = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
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
    // Claude has a built-in rate. Without a provider, `gpt-5.4` is priced only
    // by the unknown-model fallback, which never yields a cost split.
    expect(result.providers.map((row) => [row.provider, row.totals.costByType !== null])).toEqual([
      ['claude', true],
      ['codex', false],
    ]);
    expect(result.totals?.costByType).toBeNull();
    expect(result.models.find((row) => row.model === 'gpt-5.4')?.rates?.source).toBe('fallback');
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
    expect(result.scope).toBe('multi-host-cli');
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
      remote: async () => ({ results: [], states: [] }),
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
      remote: async () => ({ results: [], states: [] }),
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
    const refreshing = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(refreshing.status).toBe('cached');
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.totals?.inputTokens).toBe(20);
    const refreshed = await settled(service);
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
      remote: async () => ({ results: [], states: [] }),
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
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(10);
    expect((await settled(service, QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect(calls).toBe(2);
    input = 30;
    now += 30_000;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect(calls).toBe(2);
    refreshIntervalSeconds = 30;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect((await settled(service, QUERY, FROM, now)).totals?.inputTokens).toBe(30);
    expect(calls).toBe(3);
  });

  it('serves the cached snapshot instantly while a refresh runs, spending the budget only with nothing to serve', async () => {
    let calls = 0;
    let elapsed = 0;
    const requests = () => [
      {
        provider: 'codex' as const,
        request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
      },
    ];
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests,
      loadWorker: async () => {
        calls++;
        if (calls === 1) return data('gpt-5.4', 10, 0);
        return new Promise<UsageWorkerResult>(() => {});
      },
      now: () => NOW,
      scope: () => '/fixture-budget',
      responseBudgetMs: 1000,
      // Every projection appears to cost 900 ms of the 1,000 ms budget.
      elapsedMs: () => (elapsed += 900),
    });
    expect((await service.get(QUERY, FROM, NOW)).status).toBe('ok');
    const started = performance.now();
    const refreshing = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    const waited = performance.now() - started;
    expect(refreshing.status).toBe('cached');
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.totals?.inputTokens).toBe(10);
    // The cached snapshot answers at once; the hung refresh never holds it.
    expect(waited).toBeLessThan(600);
    // With nothing to serve, a first run still waits, bounded by the budget minus recent
    // projection time: about 50 ms here (200 ms budget, 150 ms projections), never the whole
    // budget on top of the projection.
    let coldElapsed = 0;
    const cold = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests,
      loadWorker: () => new Promise<UsageWorkerResult>(() => {}),
      now: () => NOW,
      scope: () => '/fixture-budget-cold',
      responseBudgetMs: 200,
      elapsedMs: () => (coldElapsed += 150),
    });
    expect((await cold.get(QUERY, FROM, NOW)).status).toBe('loading');
    const coldStarted = performance.now();
    const loading = await cold.get(QUERY, FROM, NOW);
    const coldWaited = performance.now() - coldStarted;
    expect(loading.status).toBe('loading');
    expect(loading.totals).toBeNull();
    expect(coldWaited).toBeGreaterThanOrEqual(40);
    expect(coldWaited).toBeLessThan(600);
  });

  it('coalesces concurrent manual refreshes into one bounded scan and exposes cached data while it runs', async () => {
    let pending = false;
    let calls = 0;
    let live = 0;
    let maximum = 0;
    const resolvers: Array<(value: UsageWorkerResult) => void> = [];
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
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
      remote: async () => ({ results: [], states: [] }),
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
      remote: async () => ({ results: [], states: [] }),
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
    expect((await settled(service)).totals?.inputTokens).toBe(80);
    expect(calls).toBe(4);
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(80);
    expect(calls).toBe(4);
  });

  it('keeps last usable records and their original timestamp when an explicit refresh fails', async () => {
    let now = NOW;
    let fail = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
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
      remote: async () => ({ results: [], states: [] }),
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

  it('starts no worker for an exact-account filter or a bare tool id, and filters every usage provider in the projection', async () => {
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => {
        calls++;
        return [{ provider: 'omp', request: { kind: 'omp', roots: ['/fixture/omp'] } }];
      },
      loadWorker: async () => data('qwen3.8-max', 10, 0.002, 'alibaba-token-plan'),
      now: () => NOW,
    });
    // Tool ids are not usage providers; only routed providers and `all` select.
    const tool = await service.get(
      { ...QUERY, provider: 'omp' as 'qwen' },
      FROM,
      NOW
    );
    const account = await service.get({ ...QUERY, account: 'codex:active' }, FROM, NOW);
    expect(calls).toBe(0);
    expect(tool.totals).toBeNull();
    expect(account.totals).toBeNull();
    // Every provider the route validates reaches the projection, which keeps the usage it served.
    const qwen = await service.get({ ...QUERY, provider: 'qwen' }, FROM, NOW);
    expect(calls).toBe(1);
    expect(qwen.providers.map((row) => row.provider)).toEqual(['qwen']);
    expect(qwen.totals?.inputTokens).toBe(10);
    // A provider with nothing in the range says so, instead of reading as a broken filter.
    const empty = await service.get({ ...QUERY, provider: 'zai' }, FROM, NOW);
    expect(empty.totals).toBeNull();
    expect(empty.message).toContain('No CLI usage log in this range was served by this provider');
  });

  it('projects activity for routed providers served by other tools', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [],
      now: () => NOW,
    });
    const qwen = await service.get({ ...QUERY, provider: 'qwen' }, FROM, NOW);
    // A routed provider with no usage in range reports unavailable with a
    // provider-specific message, not the unsupported-selection refusal.
    expect(qwen.status).toBe('unavailable');
    expect(qwen.message).toContain('served by this provider');
  });

  it('merges remote sessions into the snapshot like local ones', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({
        results: [{ tool: 'omp' as const, data: data('some-model', 100, 0.005) }],
        states: [],
      }),
      requests: () => [],
      now: () => NOW,
    });
    const answer = await settled(service);
    expect(answer.status).toBe('ok');
    expect(answer.totals?.inputTokens).toBe(100);
    expect(answer.sessions?.total).toBe(1);
    expect(answer.sessions?.sample).toHaveLength(1);
    expect(answer.sessions?.sample[0].provider).toBe('other');
  });

  it('lists every remote kind when the remote scans never answer', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => {
        throw new Error('no route to host');
      },
      remoteCached: () => null,
      requests: () => [],
      now: () => NOW,
    });
    const answer = await settled(service);
    expect(answer.status).toBe('unavailable');
    const remote = answer.sources.filter(
      (entry) =>
        entry.host !== 'ubuntu' && ['claude', 'codex', 'omp', 'muse', 'zcode'].includes(entry.tool)
    );
    expect(remote).toHaveLength(10);
    for (const tool of ['claude', 'codex', 'omp', 'muse', 'zcode'] as const) {
      for (const host of ['mac', 'windows'] as const) {
        expect(remote).toContainEqual(
          expect.objectContaining({ tool, host, state: 'unavailable' })
        );
      }
    }
  });

  it('persists generic jsonl sources in the snapshot', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-jsonl-'));
    try {
      const requests = () => [
        {
          provider: 'jsonl' as const,
          request: {
            kind: 'jsonl' as const,
            roots: ['/fixture'],
            mapping: { timestamp: 'ts' },
          },
        },
      ];
      const first = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('generic-model', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
      });
      expect((await first.get(QUERY, FROM, NOW)).status).toBe('ok');
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      expect(JSON.parse(fs.readFileSync(file, 'utf8')).sources).toHaveLength(1);
      const second = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('generic-model', 20, 0),
        now: () => NOW + 61_000,
        scope: () => scope,
        persistSnapshot: true,
      });
      const instant = await second.get(QUERY, FROM, NOW);
      expect(instant.status).toBe('cached');
      expect(instant.totals?.inputTokens).toBe(10);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('persists the snapshot and serves it instantly after a restart while the first scan runs', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-'));
    try {
      const requests = () => [
        {
          provider: 'codex' as const,
          request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ];
      const first = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('gpt-5.4', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
      });
      expect((await first.get(QUERY, FROM, NOW)).status).toBe('ok');
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      expect(fs.existsSync(file)).toBe(true);
      expect(JSON.parse(fs.readFileSync(file, 'utf8')).sources).toHaveLength(1);
      // A restarted server (a new instance on the same scope) serves the snapshot at once, with
      // the first scan running behind it, instead of answering 'loading' with no data.
      const second = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('gpt-5.4', 20, 0),
        now: () => NOW + 61_000,
        scope: () => scope,
        persistSnapshot: true,
      });
      const instant = await second.get(QUERY, FROM, NOW);
      expect(instant.status).toBe('cached');
      expect(instant.refreshing).toBe(true);
      expect(instant.totals?.inputTokens).toBe(10);
      const fresh = await settled(second);
      expect(fresh.status).toBe('ok');
      expect(fresh.refreshing).toBe(false);
      expect(fresh.totals?.inputTokens).toBe(20);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('ignores a missing or invalid persisted snapshot like a first run', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-'));
    try {
      const dir = path.join(scope, 'cache', 'account-activity-v1');
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'analytics-activity-snapshot-v1.json'), 'not json{');
      const service = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests: () => [
          {
            provider: 'codex' as const,
            request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
          },
        ],
        loadWorker: async () => data('gpt-5.4', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
        responseBudgetMs: 50,
      });
      // The corrupt file is not served and does not fail the collection; the scan fills it in.
      expect((await service.get(QUERY, FROM, NOW)).status).toBe('ok');
      expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(10);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('publishes fresh local rows before a slow remote scan answers, keeping carried remote states', async () => {
    let input = 10;
    let releaseRemote: ((value: { results: []; states: [] }) => void) | null = null;
    let remoteCalls = 0;
    const remoteData = data('remote-model', 70, 0);
    const service = new AccountAnalyticsActivityService({
      remote: async () => {
        remoteCalls++;
        if (remoteCalls === 1)
          return { results: [{ tool: 'omp' as const, data: remoteData }], states: [] };
        return new Promise<{ results: []; states: [] }>((resolve) => {
          releaseRemote = resolve;
        });
      },
      requests: () => [
        {
          provider: 'codex' as const,
          request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => data('gpt-5.4', input, 0),
      now: () => NOW,
      scope: () => '/fixture-phased',
    });
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(80);
    input = 20;
    await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    // The local phase publishes while the remote scan is still gated: fresh local rows (20)
    // plus the carried remote rows (70), with the refresh still running behind the answer.
    let phased = await service.get(QUERY, FROM, NOW);
    for (let attempt = 0; attempt < 500 && phased.totals?.inputTokens !== 90; attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 5));
      phased = await service.get(QUERY, FROM, NOW);
    }
    expect(phased.totals?.inputTokens).toBe(90);
    expect(phased.refreshing).toBe(true);
    expect(releaseRemote).not.toBeNull();
    releaseRemote?.({ results: [], states: [] });
    const done = await settled(service);
    expect(done.refreshing).toBe(false);
    // The answered (empty) remote scan replaces the carried rows; only fresh local rows remain.
    expect(done.totals?.inputTokens).toBe(20);
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
      remote: async () => ({ results: [], states: [] }),
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

  it('rescans remotes on manual refresh and names the hosts it waits on while the scan runs', async () => {
    let remoteCalls = 0;
    let release = () => {};
    const service = new AccountAnalyticsActivityService({
      remote: async (_minDateMs, opts) => {
        remoteCalls++;
        opts?.onHostScan?.('mac', 'start');
        opts?.onHostScan?.('windows', 'start');
        await new Promise<void>((resolve) => {
          release = resolve;
        });
        opts?.onHostScan?.('mac', 'done');
        opts?.onHostScan?.('windows', 'done');
        return { results: [], states: [] };
      },
      requests: () => [],
      now: () => NOW,
      responseBudgetMs: 20,
      scope: () => '/fixture-refresh-remote',
    });
    // The first answer arrives while the remote scan is still running: the
    // page can say which hosts the refresh waits on.
    const flying = await service.get(QUERY, FROM, NOW);
    expect(flying.refreshing).toBe(true);
    expect(flying.refreshingRemote).toEqual(['mac', 'windows']);
    release();
    const first = await settled(service);
    expect(first.refreshing).toBe(false);
    expect(first.refreshingRemote).toEqual([]);
    expect(remoteCalls).toBe(1);
    // A manual refresh runs the remote scan again instead of serving the cache.
    const refreshing = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.refreshingRemote).toEqual(['mac', 'windows']);
    release();
    const second = await settled(service);
    expect(second.refreshing).toBe(false);
    expect(second.refreshingRemote).toEqual([]);
    expect(remoteCalls).toBe(2);
  });
});
