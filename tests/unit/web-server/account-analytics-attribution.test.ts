import { describe, expect, it } from 'bun:test';
import {
  USAGE_ROUTE_PROVIDERS,
  attributeActivitySources,
  routeProvider,
  usageProviderFor,
} from '../../../src/web-server/services/account-analytics-attribution';
import {
  projectAccountAnalyticsActivity,
  type SourceData,
} from '../../../src/web-server/services/account-analytics-projection';
import type { AccountAnalyticsPricingLookup } from '../../../src/web-server/services/account-analytics-pricing';
import { aggregateRows } from '../../../src/web-server/usage/account-activity-collector';
import type {
  HourlyUsage,
  ModelBreakdown,
  SessionUsage,
} from '../../../src/web-server/usage/types';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

// Usage counts under the dashboard provider that served it (the route the log records), never under the tool
// that logged it; a route no provider claims is "other", never a guess.
const NOW = Date.parse('2026-10-02T12:00:00Z');
const QUERY = {
  platform: 'mac' as const,
  range: '7d' as const,
  provider: 'all' as const,
  account: 'all',
};
const pricing: AccountAnalyticsPricingLookup = () => null;

function breakdown(
  model: string,
  route: string | undefined,
  input: number,
  requestCount?: number
): ModelBreakdown {
  return {
    modelName: model,
    ...(route !== undefined && { provider: route }),
    inputTokens: input,
    outputTokens: input / 10,
    cacheCreationTokens: 0,
    cacheReadTokens: input * 4,
    cost: input / 1e6,
    ...(requestCount !== undefined && { requestCount }),
  };
}
function sum(
  list: ModelBreakdown[],
  field: 'inputTokens' | 'outputTokens' | 'cacheReadTokens' | 'cost'
) {
  return list.reduce((total, item) => total + item[field], 0);
}
function hour(
  at: string,
  list: ModelBreakdown[],
  requestCount = 9,
  extra: Partial<HourlyUsage> = {}
): HourlyUsage {
  return {
    hour: `${at.slice(0, 10)} ${at.slice(11, 13)}:00`,
    source: 'fixture',
    inputTokens: sum(list, 'inputTokens'),
    outputTokens: sum(list, 'outputTokens'),
    cacheCreationTokens: 0,
    cacheReadTokens: sum(list, 'cacheReadTokens'),
    cost: sum(list, 'cost'),
    totalCost: sum(list, 'cost'),
    modelsUsed: list.map((item) => item.modelName),
    modelBreakdowns: list,
    requestCount,
    ...extra,
  };
}
function session(id: string, list: ModelBreakdown[]): SessionUsage {
  return {
    sessionId: id,
    projectPath: '',
    inputTokens: sum(list, 'inputTokens'),
    outputTokens: sum(list, 'outputTokens'),
    cacheCreationTokens: 0,
    cacheReadTokens: sum(list, 'cacheReadTokens'),
    cost: sum(list, 'cost'),
    totalCost: sum(list, 'cost'),
    firstActivity: '2026-10-02T09:10:00.000Z',
    lastActivity: '2026-10-02T10:10:00.000Z',
    versions: [],
    modelsUsed: list.map((item) => item.modelName),
    modelBreakdowns: list,
  };
}
function result(hourly: HourlyUsage[], sessions: SessionUsage[] = []): UsageWorkerResult {
  return { daily: [], monthly: [], hourly, session: sessions, eventCount: hourly.length };
}
function src(provider: SourceData['provider'], ...data: UsageWorkerResult[]): SourceData {
  return { provider, data, fetchedAt: new Date(NOW).toISOString() };
}

describe('usage attribution: the route table', () => {
  it('maps every logged route to its dashboard provider', () => {
    expect(USAGE_ROUTE_PROVIDERS).toEqual({
      anthropic: 'claude',
      claude: 'claude',
      openai: 'codex',
      codex: 'codex',
      'alibaba-token-plan': 'qwen',
      zai: 'zai',
      'zai-coding-plan': 'zai',
      'kimi-code': 'kimi-code',
      'opencode-go': 'opencode-go',
      'opencode-zen': 'opencode-go',
      cursor: 'cursor',
      'muse-code': 'muse',
      'google-antigravity': 'antigravity',
    });
    for (const [route, provider] of Object.entries(USAGE_ROUTE_PROVIDERS))
      expect(usageProviderFor('omp', route, 'any-model')).toBe(provider);
    // zcode's built-in route, any case and surrounding space
    expect(routeProvider('builtin:zai-coding-plan')).toBe('zai');
    expect(usageProviderFor('zcode', 'builtin:zai-coding-plan', 'GLM-5.3-Flash')).toBe('zai');
    expect(routeProvider('  Kimi-Code ')).toBe('kimi-code');
  });

  it('puts a route no provider claims under "other", never a guess', () => {
    // seen in the real logs: a local vLLM server, a local flash-next server, OpenRouter
    for (const route of [
      'vllm',
      'flashnext',
      'openrouter',
      'google',
      'alibaba',
      'moonshotai',
      'builtin:other',
    ])
      expect(usageProviderFor('omp', route, 'glm-5.3-flash')).toBe('other');
    expect(routeProvider('constructor')).toBeNull();
    expect(routeProvider('__proto__')).toBeNull();
  });

  it('uses the tool for Claude Code, Codex and the Muse Code CLI, else the route, the model prefix, or "other"', () => {
    expect(usageProviderFor('claude', 'anthropic', 'claude-opus-5-5')).toBe('claude');
    expect(usageProviderFor('codex', 'openai', 'gpt-6-sol')).toBe('codex');
    // Muse records no route; every Muse Code session row is Muse Code's
    expect(usageProviderFor('muse', '', 'muse-spark-1.3-contributor')).toBe('muse');
    // no route: the model id's provider prefix, when the table names it
    expect(usageProviderFor('omp', '', 'zai/glm-5')).toBe('zai');
    expect(usageProviderFor('omp', undefined, 'stealth/union-alpha')).toBe('other');
    expect(usageProviderFor('zcode', undefined, 'GLM-5.3')).toBe('other');
  });
});

describe('usage attribution: aggregation by route', () => {
  it('divides a mixed hour by provider, each part summing only its own breakdowns and events', () => {
    const qwen = breakdown('qwen3.8-max', 'alibaba-token-plan', 2_000_000, 5);
    const kimi = breakdown('k3', 'kimi-code', 300_000, 3);
    const local = breakdown('qwen3.8-27b', 'vllm', 900_000, 1);
    const [source] = attributeActivitySources([
      src('omp', result([hour('2026-10-02T09:00:00Z', [qwen, kimi, local])])),
    ]).filter((item) => item.provider === 'qwen');
    expect(source.data[0].tool).toBe('omp');
    const part = source.data[0].hourly[0];
    expect(part.inputTokens).toBe(2_000_000);
    expect(part.cacheReadTokens).toBe(8_000_000);
    expect(part.requestCount).toBe(5);
    expect(part.modelBreakdowns).toEqual([qwen]);
    const all = attributeActivitySources([
      src('omp', result([hour('2026-10-02T09:00:00Z', [qwen, kimi, local])])),
    ]);
    expect(all.map((item) => item.provider)).toEqual(['kimi-code', 'qwen', 'other']);
    // the parts add up to the hour
    const parts = all.flatMap((item) => item.data.flatMap((data) => data.hourly));
    expect(parts.reduce((total, row) => total + row.inputTokens, 0)).toBe(3_200_000);
    expect(parts.reduce((total, row) => total + (row.requestCount ?? 0), 0)).toBe(9);
  });

  it('keeps a one-route hour whole, and gives unnamed tokens of a mixed hour to "other" with no event count', () => {
    const one = hour(
      '2026-10-02T08:00:00Z',
      [breakdown('GLM-5.3-Flash', 'builtin:zai-coding-plan', 1_000_000)],
      7,
      {
        inputTokens: 1_500_000,
      }
    );
    const [zai] = attributeActivitySources([src('zcode', result([one]))]);
    expect(zai.provider).toBe('zai');
    expect(zai.data[0].hourly[0]).toBe(one);
    const mixed = hour(
      '2026-10-02T09:00:00Z',
      [breakdown('k3', 'kimi-code', 100, 2), breakdown('glm-5.3-flash', 'zai', 100, 1)],
      3,
      {
        inputTokens: 260,
      }
    );
    const other = attributeActivitySources([src('omp', result([mixed]))]).find(
      (item) => item.provider === 'other'
    );
    expect(other?.data[0].hourly[0]).toMatchObject({ inputTokens: 60, modelBreakdowns: [] });
    expect(other?.data[0].hourly[0].requestCount).toBeUndefined();
  });

  it('projects usage by provider: rows, models with their tools, and sessions counted once in the total', () => {
    const shared = 'omp-session-1';
    const activity = projectAccountAnalyticsActivity(
      [
        src(
          'claude',
          result([
            hour('2026-10-02T09:00:00Z', [breakdown('claude-opus-5-5', 'anthropic', 5_000_000)]),
          ])
        ),
        src(
          'omp',
          result(
            [
              hour(
                '2026-10-02T10:00:00Z',
                [
                  breakdown('qwen3.8-max', 'alibaba-token-plan', 2_000_000, 4),
                  breakdown('k3', 'kimi-code', 300_000, 2),
                ],
                6
              ),
            ],
            // one OMP session served by Qwen (most tokens) and Kimi Code (its advisor)
            [
              session(shared, [
                breakdown('qwen3.8-max', 'alibaba-token-plan', 2_000_000, 4),
                breakdown('k3', 'kimi-code', 300_000, 2),
              ]),
            ]
          ),
          result([
            hour(
              '2026-10-02T11:00:00Z',
              [breakdown('muse-spark-1.3-contributor', 'muse-code', 50_000, 1)],
              1
            ),
          ])
        ),
        src(
          'muse',
          result([
            hour(
              '2026-10-02T11:00:00Z',
              [breakdown('muse-spark-1.3-contributor', undefined, 900_000)],
              4
            ),
          ])
        ),
        src(
          'zcode',
          result([
            hour(
              '2026-10-02T08:00:00Z',
              [breakdown('GLM-5.3-Flash', 'builtin:zai-coding-plan', 3_000_000)],
              8
            ),
          ])
        ),
      ],
      QUERY,
      NOW - 7 * 86_400_000,
      NOW,
      'ok',
      'Fixture',
      { pricing }
    );
    expect(activity.providers.map((row) => [row.provider, row.label, row.tools])).toEqual([
      ['claude', 'Claude', ['claude']],
      ['muse', 'Muse Code', ['omp', 'muse']],
      ['kimi-code', 'Kimi Code', ['omp']],
      ['qwen', 'Qwen token plan', ['omp']],
      ['zai', 'Z.ai coding plan', ['zcode']],
    ]);
    const provider = (id: string) => activity.providers.find((row) => row.provider === id);
    expect(provider('qwen')?.usageEvents).toBe(4);
    expect(provider('kimi-code')?.usageEvents).toBe(2);
    expect(provider('muse')?.totals.inputTokens).toBe(950_000);
    // the session counts under both providers that served it, and once in the total
    expect(provider('qwen')?.sessionCount).toBe(1);
    expect(provider('kimi-code')?.sessionCount).toBe(1);
    expect(activity.sessions?.total).toBe(1);
    expect(activity.sessions?.sample.map((row) => row.provider)).toEqual(['qwen']);
    // models: one row per provider and model, with the tools whose logs hold it
    const models = new Map(activity.models.map((row) => [`${row.provider}:${row.model}`, row]));
    expect(models.get('muse:muse-spark-1.3-contributor')?.tools).toEqual(['omp', 'muse']);
    expect(models.get('zai:GLM-5.3-Flash')?.inputTokens).toBe(3_000_000);
    expect([...new Set(activity.byHour.map((row) => row.provider))].sort()).toEqual([
      'claude',
      'kimi-code',
      'muse',
      'qwen',
      'zai',
    ]);
    // no tool name is ever a provider value
    const values = [
      activity.providers,
      activity.byHour,
      activity.byDay,
      activity.models,
      activity.byDayModel,
    ].flatMap((rows) => rows.map((row) => row.provider as string));
    expect(values.some((value) => ['omp', 'zcode'].includes(value))).toBe(false);

    // the server filters by provider too: Z.ai is zcode's GLM usage alone
    const zai = projectAccountAnalyticsActivity(
      [
        src(
          'zcode',
          result([
            hour(
              '2026-10-02T08:00:00Z',
              [breakdown('GLM-5.3-Flash', 'builtin:zai-coding-plan', 3_000_000)],
              8
            ),
          ])
        ),
        src(
          'claude',
          result([
            hour('2026-10-02T09:00:00Z', [breakdown('claude-opus-5-5', 'anthropic', 5_000_000)]),
          ])
        ),
      ],
      { ...QUERY, provider: 'zai' },
      NOW - 7 * 86_400_000,
      NOW,
      'ok',
      'Fixture',
      { pricing }
    );
    expect(zai.providers.map((row) => row.provider)).toEqual(['zai']);
    expect(zai.totals?.inputTokens).toBe(3_000_000);
  });

  it('counts events per route in the collector only for readers that record a route', () => {
    const entry = (model: string, provider: string | undefined, target: string) => ({
      entry: {
        inputTokens: 100,
        outputTokens: 10,
        cacheCreationTokens: 0,
        cacheReadTokens: 0,
        model,
        sessionId: 's1',
        timestamp: '2026-10-02T09:15:00.000Z',
        projectPath: '',
        target,
        ...(provider !== undefined && { provider }),
      },
      events: 3,
    });
    const routed = aggregateRows(
      [entry('qwen3.8-max', 'alibaba-token-plan', 'omp'), entry('k3', 'kimi-code', 'omp')],
      'omp'
    );
    expect(routed.hourly[0].modelBreakdowns.map((row) => [row.provider, row.requestCount])).toEqual(
      expect.arrayContaining([
        ['alibaba-token-plan', 3],
        ['kimi-code', 3],
      ])
    );
    // Claude Code and Codex rows keep their exact shape
    const claude = aggregateRows([entry('claude-opus-5-5', undefined, 'claude')], 'claude');
    expect('requestCount' in claude.hourly[0].modelBreakdowns[0]).toBe(false);
  });
});
