/**
 * CONTRACT-analytics.md section 10: ranges and zones, cost by type, the
 * session sample, byDayModel, request counts, anomalies, active and
 * switchable fields, and the provider list from data. Fixtures only: a
 * temporary CCS_HOME, an injected clock and injected worker data.
 */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AccountAnalyticsActivityService,
  defaultAccountAnalyticsPricing,
  projectAccountAnalyticsActivity,
  type AccountAnalyticsPricingLookup,
  type SourceData,
} from '../../../src/web-server/services/account-analytics-activity';
import {
  AccountAnalyticsService,
  type AccountAnalyticsDeps,
} from '../../../src/web-server/services/account-analytics-service';
import {
  defaultDashboardPreferences,
  writeDashboardPreferences,
} from '../../../src/web-server/services/dashboard-preferences';
import {
  AccountAnalyticsQueryError,
  canonicalAccountAnalyticsTimeZone,
  isAccountAnalyticsTimeZone,
  localDate,
  localMidnight,
  resolveAccountAnalyticsRange,
} from '../../../src/web-server/services/account-analytics-range';
import {
  appendAccountAnalyticsSnapshot,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsHistoryStore,
} from '../../../src/web-server/services/account-analytics-history';
import type {
  AccountDashboard,
  DashboardAccount,
  DashboardProvider,
} from '../../../src/web-server/services/account-dashboard-types';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsModelRates,
  AccountAnalyticsQuery,
} from '../../../src/web-server/services/account-analytics-types';
import type {
  HourlyUsage,
  ModelBreakdown,
  SessionUsage,
} from '../../../src/web-server/usage/types';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

let ccsHome = '';
let previousCcsHome: string | undefined;
beforeAll(() => {
  previousCcsHome = process.env.CCS_HOME;
  ccsHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-analytics-contract-'));
  process.env.CCS_HOME = ccsHome;
});
afterAll(() => {
  if (previousCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previousCcsHome;
  fs.rmSync(ccsHome, { recursive: true, force: true });
});

const NOW = Date.parse('2026-10-02T12:00:00Z');
const DAY = 86_400_000;
const QUERY: AccountAnalyticsQuery = {
  platform: 'mac',
  range: '7d',
  provider: 'all',
  account: 'all',
};
const RATES: AccountAnalyticsModelRates = {
  inputPerMillion: 3,
  outputPerMillion: 15,
  cacheCreationPerMillion: 3.75,
  cacheReadPerMillion: 0.3,
  source: 'builtin',
};
/** Known list rates for every fixture model, except `unpriced-*` models. */
const pricing: AccountAnalyticsPricingLookup = (model) =>
  model.startsWith('unpriced-') ? null : { ...RATES };

function listCost(tokens: {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}): number {
  return (
    (tokens.inputTokens / 1e6) * RATES.inputPerMillion +
    (tokens.outputTokens / 1e6) * RATES.outputPerMillion +
    (tokens.cacheCreationTokens / 1e6) * RATES.cacheCreationPerMillion +
    (tokens.cacheReadTokens / 1e6) * RATES.cacheReadPerMillion
  );
}

interface ModelFixture {
  model: string;
  input?: number;
  output?: number;
  cacheCreation?: number;
  cacheRead?: number;
  /** Multiplies the list cost to imitate tiered pricing in the worker. */
  costFactor?: number;
}

function breakdown(fixture: ModelFixture): ModelBreakdown {
  const tokens = {
    inputTokens: fixture.input ?? 0,
    outputTokens: fixture.output ?? 0,
    cacheCreationTokens: fixture.cacheCreation ?? 0,
    cacheReadTokens: fixture.cacheRead ?? 0,
  };
  return {
    modelName: fixture.model,
    ...tokens,
    cost: listCost(tokens) * (fixture.costFactor ?? 1),
  };
}

function hourRow(
  hourIso: string,
  models: ModelFixture[],
  requestCount: number | null = 3
): HourlyUsage {
  const breakdowns = models.map(breakdown);
  const sum = (field: keyof Omit<ModelBreakdown, 'modelName' | 'provider'>) =>
    breakdowns.reduce((total, row) => total + row[field], 0);
  const cost = sum('cost');
  return {
    hour: `${hourIso.slice(0, 10)} ${hourIso.slice(11, 13)}:00`,
    source: 'fixture',
    inputTokens: sum('inputTokens'),
    outputTokens: sum('outputTokens'),
    cacheCreationTokens: sum('cacheCreationTokens'),
    cacheReadTokens: sum('cacheReadTokens'),
    cost,
    totalCost: cost,
    modelsUsed: breakdowns.map((row) => row.modelName),
    modelBreakdowns: breakdowns,
    ...(requestCount === null ? {} : { requestCount }),
  };
}

function sessionRow(
  sessionId: string,
  lastActivity: string,
  models: ModelFixture[],
  target?: string
): SessionUsage {
  const breakdowns = models.map(breakdown);
  const sum = (field: keyof Omit<ModelBreakdown, 'modelName' | 'provider'>) =>
    breakdowns.reduce((total, row) => total + row[field], 0);
  return {
    sessionId,
    projectPath: '/home/fixture/secret-project',
    inputTokens: sum('inputTokens'),
    outputTokens: sum('outputTokens'),
    cacheCreationTokens: sum('cacheCreationTokens'),
    cacheReadTokens: sum('cacheReadTokens'),
    cost: sum('cost'),
    totalCost: sum('cost'),
    lastActivity,
    versions: ['9.9.9-fixture'],
    modelsUsed: breakdowns.map((row) => row.modelName),
    modelBreakdowns: breakdowns,
    source: 'fixture',
    ...(target === undefined ? {} : { target }),
  };
}

function result(hourly: HourlyUsage[], session: SessionUsage[] = []): UsageWorkerResult {
  return { daily: [], monthly: [], hourly, session, eventCount: hourly.length };
}

function source(provider: 'claude' | 'codex', ...data: UsageWorkerResult[]): SourceData {
  return { provider, data, fetchedAt: new Date(NOW).toISOString() };
}

function project(
  sources: SourceData[],
  options: { tz?: string; from?: number; to?: number; query?: Partial<AccountAnalyticsQuery> } = {}
): AccountAnalyticsActivity {
  return projectAccountAnalyticsActivity(
    sources,
    { ...QUERY, ...options.query },
    options.from ?? NOW - 7 * DAY,
    options.to ?? NOW,
    'ok',
    'Fixture',
    { tz: options.tz, pricing }
  );
}

function account(
  provider: DashboardProvider,
  id: string,
  overrides: Partial<DashboardAccount> = {}
): DashboardAccount {
  return {
    id,
    provider,
    providerLabel: provider,
    label: id,
    email: `${id.replace(/[^a-z0-9]/gi, '')}@example.com`,
    plan: null,
    platform: provider === 'claude' ? 'mac' : 'ubuntu',
    source: `${provider} fixture`,
    status: 'ok',
    message: null,
    fetchedAt: new Date(NOW).toISOString(),
    sampledAt: new Date(NOW).toISOString(),
    isActive: false,
    capabilities: {
      codexProfile: provider === 'codex' ? id.slice('codex:'.length) : null,
      claudeProfileId: null,
      claudePlatforms: [],
    },
    windows: [
      {
        key: 'seven_day',
        label: 'Weekly',
        usedPercent: 40,
        remainingPercent: 60,
        resetAt: '2026-10-05T00:00:00Z',
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
      },
    ],
    ...overrides,
  };
}

function dashboard(accounts: DashboardAccount[], extra: Record<string, unknown> = {}) {
  return {
    schemaVersion: 1,
    updatedAt: new Date(NOW).toISOString(),
    settings: { refreshIntervalSeconds: 60 },
    accounts,
    codexAutoSwitch: {
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'idle',
      message: 'Idle',
      activationInProgress: false,
    },
    ...extra,
  } as AccountDashboard;
}

function memory(initial: AccountAnalyticsHistoryData | null = null): AccountAnalyticsHistoryStore {
  let data = initial;
  return {
    async read() {
      return data;
    },
    async write(next) {
      data = structuredClone(next);
    },
  };
}

const noActivity: AccountAnalyticsActivity = project([], { query: { provider: 'qwen' } });

function analyticsService(
  accounts: DashboardAccount[],
  options: {
    history?: AccountAnalyticsHistoryData | null;
    activity?: AccountAnalyticsDeps['getActivity'];
    extra?: Record<string, unknown>;
    now?: number;
  } = {}
): AccountAnalyticsService {
  return new AccountAnalyticsService({
    getDashboard: async () => dashboard(accounts, options.extra),
    createHistoryStore: () => memory(options.history ?? null),
    getActivity: options.activity ?? (async () => noActivity),
    now: () => options.now ?? NOW,
    scope: () => ccsHome,
  });
}

describe('analytics contract: ranges and time zones', () => {
  it('starts Month at local midnight on the 1st in the requested zone', () => {
    const range = resolveAccountAnalyticsRange(
      { range: 'month', tz: 'America/New_York' },
      NOW,
      NOW - 31 * DAY
    );
    expect(new Date(range.from).toISOString()).toBe('2026-10-01T04:00:00.000Z');
    expect(range.to).toBe(NOW);
    expect(range.bucketMinutes).toBe(60);
  });

  it('resolves a custom range to inclusive local days, never past now', () => {
    const range = resolveAccountAnalyticsRange(
      { range: 'custom', from: '2026-09-20', to: '2026-09-26', tz: 'America/New_York' },
      NOW,
      NOW
    );
    expect(new Date(range.from).toISOString()).toBe('2026-09-20T04:00:00.000Z');
    expect(new Date(range.to).toISOString()).toBe('2026-09-27T03:59:59.000Z');
    const today = resolveAccountAnalyticsRange(
      { range: 'custom', from: '2026-10-02', to: '2026-10-02' },
      NOW,
      NOW
    );
    expect(new Date(today.from).toISOString()).toBe('2026-10-02T00:00:00.000Z');
    expect(today.to).toBe(NOW);
    expect(today.bucketMinutes).toBe(10);
  });

  it.each([
    ['from after to', { from: '2026-09-26', to: '2026-09-20' }],
    ['more than 31 days', { from: '2026-09-01', to: '2026-10-02' }],
    ['before retention', { from: '2026-08-20', to: '2026-08-25' }],
    ['after today', { from: '2026-10-01', to: '2026-10-03' }],
    ['not a calendar day', { from: '2026-09-31', to: '2026-10-01' }],
    ['missing end', { from: '2026-09-20' }],
  ])('rejects a custom range with %s as invalid_range', (_label, dates) => {
    let error: unknown;
    try {
      resolveAccountAnalyticsRange({ range: 'custom', ...dates }, NOW, NOW);
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(AccountAnalyticsQueryError);
    expect((error as AccountAnalyticsQueryError).code).toBe('invalid_range');
  });

  it('rejects unknown zones and dates on a preset range', () => {
    expect(isAccountAnalyticsTimeZone('Mars/Base')).toBe(false);
    expect(isAccountAnalyticsTimeZone('UTC')).toBe(true);
    expect(isAccountAnalyticsTimeZone('America/New_York')).toBe(true);
    expect(() => resolveAccountAnalyticsRange({ range: '7d', tz: 'Mars/Base' }, NOW, NOW)).toThrow(
      AccountAnalyticsQueryError
    );
    expect(() =>
      resolveAccountAnalyticsRange({ range: '7d', from: '2026-09-30' }, NOW, NOW)
    ).toThrow(AccountAnalyticsQueryError);
  });

  it('finds the first instant of local days across DST changes', () => {
    expect(new Date(localMidnight('2026-11-01', 'America/New_York')).toISOString()).toBe(
      '2026-11-01T04:00:00.000Z'
    );
    expect(new Date(localMidnight('2026-11-02', 'America/New_York')).toISOString()).toBe(
      '2026-11-02T05:00:00.000Z'
    );
    // Santiago skips local midnight when its daylight time starts.
    const santiago = localMidnight('2026-09-06', 'America/Santiago');
    expect(localDate(santiago, 'America/Santiago')).toBe('2026-09-06');
    expect(localDate(santiago - 1000, 'America/Santiago')).toBe('2026-09-05');
    expect(new Date(localMidnight('2026-10-02', 'Asia/Kolkata')).toISOString()).toBe(
      '2026-10-01T18:30:00.000Z'
    );
  });

  it('starts All at the oldest retained quota sample, bounded to 31 days', async () => {
    const codex = account('codex', 'codex:a');
    const oldSample = NOW - 10 * DAY;
    const history = appendAccountAnalyticsSnapshot(
      null,
      [{ ...codex, sampledAt: new Date(oldSample).toISOString() }],
      oldSample
    );
    const result = await analyticsService([codex], { history }).get({ ...QUERY, range: 'all' });
    expect(result.range.preset).toBe('all');
    expect(result.range.from).toBe(new Date(oldSample).toISOString());
    expect(result.range.availableFrom).toBe(new Date(oldSample).toISOString());
    expect(result.range.tz).toBe('America/New_York');
    expect(result.range.dayBucketTz).toBe('America/New_York');
    expect(result.range.bucketMinutes).toBe(180);
    expect(result.accounts[0].sampleCount).toBe(2);
    // a saved display time zone drives the buckets; an explicit zone still wins
    writeDashboardPreferences(
      { ...defaultDashboardPreferences(), timeZone: 'Asia/Tokyo' },
      ccsHome
    );
    try {
      const tokyo = await analyticsService([codex], { history }).get({ ...QUERY, range: 'all' });
      expect(tokyo.range.tz).toBe('Asia/Tokyo');
      expect(tokyo.range.dayBucketTz).toBe('Asia/Tokyo');
      const explicit = await analyticsService([codex], { history }).get({
        ...QUERY,
        range: 'all',
        tz: 'UTC',
      });
      expect(explicit.range.tz).toBe('UTC');
    } finally {
      fs.rmSync(path.join(ccsHome, 'dashboard-preferences.json'), { force: true });
    }

    // Older local activity moves the start back, but never past 31 days.
    const calls: number[] = [];
    const older = await analyticsService([codex], {
      history,
      activity: async (_query, from) => {
        calls.push(from);
        return {
          ...noActivity,
          coverage: { oldestHourAt: NOW - 45 * DAY, providersWithActivity: [] },
        };
      },
    }).get({ ...QUERY, range: 'all' });
    expect(calls).toEqual([NOW - 31 * DAY]);
    expect(older.range.from).toBe(new Date(NOW - 31 * DAY).toISOString());
    expect(older.range.availableFrom).toBe(new Date(NOW - 31 * DAY).toISOString());
  });

  it('passes the zone to activity, echoes it in range, and refuses retention misses with a code', async () => {
    const seen: Array<string | undefined> = [];
    const service = analyticsService([account('codex', 'codex:a')], {
      activity: async (_query, _from, _to, options) => {
        seen.push(options?.tz);
        return noActivity;
      },
    });
    const result = await service.get({ ...QUERY, range: 'month', tz: 'America/New_York' });
    expect(seen).toEqual(['America/New_York']);
    expect(result.range.from).toBe('2026-10-01T04:00:00.000Z');
    expect(result.range.tz).toBe('America/New_York');
    expect(result.range.dayBucketTz).toBe('America/New_York');
    expect('coverage' in result.activity).toBe(false);
    await expect(
      service.get({ ...QUERY, range: 'custom', from: '2026-08-01', to: '2026-08-02' })
    ).rejects.toMatchObject({ code: 'invalid_range' });
    await expect(service.get({ ...QUERY, tz: 'Mars/Base' })).rejects.toMatchObject({
      code: 'invalid_tz',
    });
  });
});

describe('analytics contract: range review fixes', () => {
  it('starts a custom range no earlier than the retained window in a zone far ahead of UTC', async () => {
    // At 2026-10-02T12:00Z the retained window starts at 2026-09-01T12:00Z,
    // which is already 2026-09-02 in Kiritimati (UTC+14); that local day
    // began at 2026-09-01T10:00Z.
    const range = resolveAccountAnalyticsRange(
      { range: 'custom', from: '2026-09-02', to: '2026-09-03', tz: 'Pacific/Kiritimati' },
      NOW,
      NOW
    );
    expect(new Date(range.from).toISOString()).toBe('2026-09-01T12:00:00.000Z');
    expect(new Date(range.to).toISOString()).toBe('2026-09-03T09:59:59.000Z');
    const result = await analyticsService([account('codex', 'codex:a')], {
      activity: async () => ({
        ...noActivity,
        coverage: { oldestHourAt: NOW - 45 * DAY, providersWithActivity: [] },
      }),
    }).get({
      ...QUERY,
      range: 'custom',
      from: '2026-09-02',
      to: '2026-09-03',
      tz: 'Pacific/Kiritimati',
    });
    expect(result.range.availableFrom).toBe('2026-09-01T12:00:00.000Z');
    expect(Date.parse(result.range.from)).toBeGreaterThanOrEqual(
      Date.parse(result.range.availableFrom)
    );
  });

  it('echoes a case variant of a zone under its listed spelling', async () => {
    const seen: Array<string | undefined> = [];
    const service = analyticsService([account('codex', 'codex:a')], {
      activity: async (query, _from, _to, options) => {
        seen.push(query.tz, options?.tz);
        return noActivity;
      },
    });
    const result = await service.get({ ...QUERY, tz: 'america/new_york' });
    expect(seen).toEqual(['America/New_York', 'America/New_York']);
    expect(result.range.tz).toBe('America/New_York');
    expect(result.range.dayBucketTz).toBe('America/New_York');
    expect(canonicalAccountAnalyticsTimeZone(undefined)).toBe('UTC');
    expect(canonicalAccountAnalyticsTimeZone('utc')).toBe('UTC');
    expect(canonicalAccountAnalyticsTimeZone('Europe/London')).toBe('Europe/London');
    expect(canonicalAccountAnalyticsTimeZone('EUROPE/LONDON')).toBe('Europe/London');
  });

  it('records no quota snapshot for a request it refuses', async () => {
    let dashboards = 0;
    let writes = 0;
    const service = new AccountAnalyticsService({
      getDashboard: async () => {
        dashboards++;
        return dashboard([account('codex', 'codex:a')]);
      },
      createHistoryStore: () => ({
        async read() {
          return null;
        },
        async write() {
          writes++;
        },
      }),
      getActivity: async () => noActivity,
      now: () => NOW,
      scope: () => ccsHome,
    });
    await expect(
      service.get({ ...QUERY, range: 'custom', from: '2026-08-01', to: '2026-08-02' })
    ).rejects.toMatchObject({ code: 'invalid_range' });
    await expect(service.get({ ...QUERY, tz: 'Mars/Base' })).rejects.toMatchObject({
      code: 'invalid_tz',
    });
    expect([dashboards, writes]).toEqual([0, 0]);
    await service.get(QUERY);
    expect([dashboards, writes]).toEqual([1, 1]);
  });
});

describe('analytics contract: local day buckets', () => {
  it('puts 03:00Z on the previous day in New York and the same day in UTC', () => {
    const sources = [
      source('claude', result([hourRow('2026-10-02T03:00:00Z', [{ model: 'm', input: 10 }])])),
    ];
    const newYork = project(sources, { tz: 'America/New_York' });
    const utc = project(sources);
    expect(newYork.timezone).toBe('America/New_York');
    expect(newYork.byDay.map((row) => row.date)).toEqual(['2026-10-01']);
    expect(utc.timezone).toBe('UTC');
    expect(utc.byDay.map((row) => row.date)).toEqual(['2026-10-02']);
    // Hours stay UTC instants in both.
    expect(newYork.byHour[0].hour).toBe('2026-10-02T03:00:00Z');
    expect(newYork.byDayModel[0].date).toBe('2026-10-01');
  });

  it('says when a zone with a partial-hour offset can shift a day boundary', () => {
    const sources = [
      source('claude', result([hourRow('2026-10-02T03:00:00Z', [{ model: 'm', input: 10 }])])),
    ];
    expect(project(sources, { tz: 'Asia/Kolkata' }).message).toContain('up to 45 minutes');
    expect(project(sources, { tz: 'America/New_York' }).message).not.toContain('45 minutes');
  });
});

describe('analytics contract: cost by type', () => {
  const tokens = {
    input: 2_000_000,
    output: 400_000,
    cacheCreation: 100_000,
    cacheRead: 5_000_000,
  };

  it('splits cost per token type at list rates and reconciles with the worker total', () => {
    const activity = project([
      source(
        'claude',
        result([hourRow('2026-10-02T10:00:00Z', [{ model: 'model-a', ...tokens }])])
      ),
    ]);
    const expected = { input: 6, output: 6, cacheWrite: 0.375, cacheRead: 1.5 };
    for (const row of [
      activity.totals,
      activity.providers[0].totals,
      activity.byDay[0],
      activity.byHour[0],
      activity.models[0],
      activity.byDayModel[0],
    ]) {
      expect(row?.costByType).not.toBeNull();
      for (const [key, value] of Object.entries(expected))
        expect(Math.abs((row?.costByType as Record<string, number>)[key] - value)).toBeLessThan(
          1e-9
        );
      expect(Math.abs((row?.estimatedCostUsd ?? 0) - 13.875)).toBeLessThan(1e-9);
      expect(row?.costByTypeReconciled).toBe(true);
    }
    expect(activity.models[0].rates).toEqual(RATES);
  });

  it('flags a 2% difference as not reconciled and never rescales the parts', () => {
    const activity = project([
      source(
        'claude',
        result([
          hourRow('2026-10-02T10:00:00Z', [{ model: 'model-a', ...tokens, costFactor: 1.02 }]),
        ])
      ),
    ]);
    expect(activity.totals?.costByTypeReconciled).toBe(false);
    expect(activity.totals?.costByType?.input).toBeCloseTo(6, 9);
    expect(activity.totals?.costByType?.cacheRead).toBeCloseTo(1.5, 9);
    expect(activity.totals?.estimatedCostUsd).toBeCloseTo(13.875 * 1.02, 9);
  });

  it('makes costByType null upward when any contributing model has no rate', () => {
    const activity = project([
      source(
        'claude',
        result([
          hourRow('2026-10-02T10:00:00Z', [
            { model: 'model-a', ...tokens },
            { model: 'unpriced-model', input: 1000 },
          ]),
          hourRow('2026-10-01T10:00:00Z', [{ model: 'model-a', ...tokens }]),
        ])
      ),
      source('codex', result([hourRow('2026-10-02T10:00:00Z', [{ model: 'model-c', input: 10 }])])),
    ]);
    expect(activity.totals?.costByType).toBeNull();
    expect(activity.totals?.costByTypeReconciled).toBe(false);
    const claude = activity.providers.find((row) => row.provider === 'claude');
    expect(claude?.totals.costByType).toBeNull();
    expect(
      activity.providers.find((row) => row.provider === 'codex')?.totals.costByType
    ).not.toBeNull();
    const day = (date: string) =>
      activity.byDay.find((row) => row.date === date && row.provider === 'claude');
    expect(day('2026-10-02')?.costByType).toBeNull();
    expect(day('2026-10-01')?.costByType).not.toBeNull();
    const unpriced = activity.models.find((row) => row.model === 'unpriced-model');
    expect(unpriced?.costByType).toBeNull();
    expect(unpriced?.rates).toBeNull();
    expect(JSON.stringify(activity.totals)).not.toContain('"input":0');
  });

  it('gives no cost split for a model the default rates price only by the unknown-model fallback', () => {
    const activity = projectAccountAnalyticsActivity(
      [
        source(
          'claude',
          result([
            hourRow('2026-10-02T10:00:00Z', [
              { model: 'claude-sonnet-4-6', ...tokens },
              { model: 'aac-fixture-unknown-model', input: 1000 },
            ]),
          ])
        ),
      ],
      QUERY,
      NOW - 7 * DAY,
      NOW,
      'ok',
      'Fixture'
    );
    expect(activity.totals?.costByType).toBeNull();
    expect(activity.totals?.costByTypeReconciled).toBe(false);
    expect(activity.providers[0].totals.costByType).toBeNull();
    expect(activity.byDay[0].costByType).toBeNull();
    expect(activity.byHour[0].costByType).toBeNull();
    const unknown = activity.models.find((row) => row.model === 'aac-fixture-unknown-model');
    expect(unknown?.rates?.source).toBe('fallback');
    expect(unknown?.costByType).toBeNull();
    const known = activity.models.find((row) => row.model === 'claude-sonnet-4-6');
    expect(known?.rates?.source).toBe('builtin');
    expect(known?.costByType).not.toBeNull();
  });

  it('treats tokens no model breakdown names as unpriceable', () => {
    const row = hourRow('2026-10-02T10:00:00Z', []);
    row.inputTokens = 500;
    const activity = project([source('claude', result([row]))]);
    expect(activity.totals?.inputTokens).toBe(500);
    expect(activity.totals?.costByType).toBeNull();
    expect(activity.byDayModel).toHaveLength(1);
    expect(activity.byDayModel[0]).toMatchObject({
      model: 'Other models',
      inputTokens: 500,
      costByType: null,
    });
  });

  it('labels where the default rates came from', () => {
    expect(defaultAccountAnalyticsPricing('claude-sonnet-4-6', undefined)?.source).toBe('builtin');
    const fallback = defaultAccountAnalyticsPricing('aac-fixture-unknown-model', undefined);
    expect(fallback?.source).toBe('fallback');
    expect(fallback?.inputPerMillion).toBeGreaterThan(0);
  });
});

describe('analytics contract: session sample', () => {
  const sessions = Array.from({ length: 60 }, (_, index) =>
    sessionRow(
      `raw-session-id-${String(index).padStart(3, '0')}`,
      new Date(NOW - (index + 1) * 3_600_000).toISOString(),
      [{ model: index % 2 ? 'model-a' : 'model-b', input: 1000 + index, output: 10 }],
      index === 0 ? 'codex' : index === 1 ? 'weird-target' : undefined
    )
  );
  const sources = () => [
    source(
      'claude',
      result(
        [hourRow('2026-10-02T10:00:00Z', [{ model: 'model-a', input: 1 }])],
        sessions.slice(0, 40)
      )
    ),
    source('codex', result([], sessions.slice(40))),
  ];

  it('returns at most 50 rows, newest first, with the total of active sessions', () => {
    const activity = project(sources());
    expect(activity.sessions).not.toBeNull();
    const sample = activity.sessions?.sample ?? [];
    expect(sample).toHaveLength(50);
    expect(activity.sessions?.total).toBe(60);
    expect(activity.sessions?.total).toBe(
      activity.providers.reduce((sum, row) => sum + row.sessionCount, 0)
    );
    expect(activity.sessions?.truncated).toBe(true);
    const times = sample.map((row) => Date.parse(row.lastActivity));
    expect([...times].sort((a, b) => b - a)).toEqual(times);
    expect(sample[0]).toMatchObject({ provider: 'claude', target: 'codex', models: ['model-b'] });
    expect(sample[1].target).toBeNull();
    expect(sample[0].costByType).not.toBeNull();
    expect(sample[0].inputTokens).toBe(1000);
  });

  it('shows each session under its most-used non-synthetic model, never <synthetic>', () => {
    const mixed = sessionRow('mixed-session', new Date(NOW - 3_600_000).toISOString(), [
      { model: '<synthetic>', input: 0, output: 0 },
      { model: 'model-small', input: 100, output: 10 },
      { model: 'model-big', input: 9000, output: 500 },
    ]);
    const onlySynthetic = sessionRow('synthetic-only', new Date(NOW - 2_600_000).toISOString(), [
      { model: '<synthetic>', input: 0, output: 0 },
    ]);
    const activity = project([
      source('claude', result([hourRow('2026-10-02T10:00:00Z', [{ model: 'model-a', input: 1 }])], [mixed, onlySynthetic])),
    ]);
    const sample = activity.sessions?.sample ?? [];
    expect(sample).toHaveLength(2);
    // Most-used first; the synthetic attribution is dropped everywhere.
    expect(sample.find((row) => row.inputTokens === 9100)?.models).toEqual([
      'model-big',
      'model-small',
    ]);
    expect(sample.find((row) => row.inputTokens === 0)?.models).toEqual([]);
    expect(JSON.stringify(sample)).not.toContain('<synthetic>');
  });

  it('never sends paths, project names, raw ids or versions, and keeps keys stable', () => {
    const first = project(sources());
    const second = project(sources());
    const json = JSON.stringify(first);
    expect(json).not.toContain('projectPath');
    expect(json).not.toContain('/home/fixture');
    expect(json).not.toContain('secret-project');
    expect(json).not.toContain('raw-session-id');
    expect(json).not.toContain('9.9.9-fixture');
    expect(json).not.toContain('versions');
    expect(first.sessions?.sample.map((row) => row.key)).toEqual(
      second.sessions?.sample.map((row) => row.key)
    );
    const expectedKey = createHash('sha256')
      .update('aac-session-v1:claude:raw-session-id-000')
      .digest('hex')
      .slice(0, 16);
    expect(first.sessions?.sample[0].key).toBe(expectedKey);
    for (const row of first.sessions?.sample ?? []) expect(row.key).toMatch(/^[0-9a-f]{16}$/);
  });

  it('counts sessions whose activity overlaps the range and says rows carry whole-session totals', () => {
    const from = Date.parse('2026-09-20T00:00:00Z');
    const to = Date.parse('2026-09-20T23:59:59Z');
    const spanning = {
      ...sessionRow('spanning', '2026-09-22T08:00:00Z', [{ model: 'model-a', input: 10 }]),
      firstActivity: '2026-09-19T22:30:00Z',
    };
    const inside = sessionRow('inside', '2026-09-20T12:00:00Z', [{ model: 'model-a', input: 20 }]);
    const later = {
      ...sessionRow('later', '2026-09-21T05:00:00Z', [{ model: 'model-a', input: 30 }]),
      firstActivity: '2026-09-21T01:00:00Z',
    };
    const earlier = {
      ...sessionRow('earlier', '2026-09-19T20:00:00Z', [{ model: 'model-a', input: 40 }]),
      firstActivity: '2026-09-19T10:00:00Z',
    };
    // A row without its first activity is placed at its last activity only.
    const legacy = sessionRow('legacy', '2026-09-22T09:00:00Z', [{ model: 'model-a', input: 50 }]);
    const activity = project(
      [source('claude', result([], [spanning, inside, later, earlier, legacy]))],
      { from, to }
    );
    const key = (id: string) =>
      createHash('sha256').update(`aac-session-v1:claude:${id}`).digest('hex').slice(0, 16);
    expect(activity.sessions?.total).toBe(2);
    expect(activity.providers[0].sessionCount).toBe(2);
    expect(activity.sessions?.sample.map((row) => row.key)).toEqual([
      key('spanning'),
      key('inside'),
    ]);
    expect(activity.sessions?.sample[0].inputTokens).toBe(10);
    expect(activity.message).toContain('whole retained totals');

    // Compacted rows keep the last event of each hour, so the first activity
    // counts from the start of its UTC hour.
    const sameHour = {
      ...sessionRow('same-hour', '2026-09-20T12:00:00Z', [{ model: 'model-a', input: 1 }]),
      firstActivity: '2026-09-20T10:50:00Z',
    };
    const narrow = project([source('claude', result([], [sameHour]))], {
      from: Date.parse('2026-09-20T09:00:00Z'),
      to: Date.parse('2026-09-20T10:20:00Z'),
    });
    expect(narrow.sessions?.total).toBe(1);
  });

  it('is null unless activity is ok or cached', () => {
    const loading = projectAccountAnalyticsActivity(
      sources(),
      QUERY,
      NOW - 7 * DAY,
      NOW,
      'loading',
      'x',
      { pricing }
    );
    expect(loading.sessions).toBeNull();
    expect(loading.anomalies).toBeNull();
    const cached = projectAccountAnalyticsActivity(
      sources(),
      QUERY,
      NOW - 7 * DAY,
      NOW,
      'cached',
      'x',
      { pricing }
    );
    expect(cached.sessions?.sample.length).toBe(50);
  });
});

describe('analytics contract: byDayModel', () => {
  it('names the twelve costliest models and folds the rest per provider and day', () => {
    const claudeModels = Array.from({ length: 15 }, (_, index) => ({
      model: `claude-model-${String(index).padStart(2, '0')}`,
      input: (index + 1) * 100_000,
      output: 1000,
    }));
    const sources = [
      source(
        'claude',
        result([
          hourRow('2026-10-01T10:00:00Z', claudeModels),
          hourRow('2026-10-02T10:00:00Z', claudeModels.slice(0, 3)),
        ])
      ),
      source(
        'codex',
        result([hourRow('2026-10-02T11:00:00Z', [{ model: 'codex-model', input: 5_000_000 }])])
      ),
    ];
    const activity = project(sources);
    const rows = activity.byDayModel;
    const named = new Set(
      rows
        .filter((row) => row.model !== 'Other models')
        .map((row) => `${row.provider}:${row.model}`)
    );
    expect(named.size).toBe(12);
    expect(named.has('codex:codex-model')).toBe(true);
    for (const key of new Set(rows.map((row) => `${row.date}:${row.provider}`))) {
      const group = rows.filter((row) => `${row.date}:${row.provider}` === key);
      expect(group.filter((row) => row.model === 'Other models').length).toBeLessThanOrEqual(1);
      expect(group.filter((row) => row.model !== 'Other models').length).toBeLessThanOrEqual(12);
    }
    // The twelve costliest models over the whole range keep their names.
    const rangeCost = new Map<string, number>();
    for (const row of sources.flatMap((item) =>
      item.data.flatMap((data) =>
        data.hourly.flatMap((hour) =>
          hour.modelBreakdowns.map(
            (model) => [`${item.provider}:${model.modelName}`, model.cost] as const
          )
        )
      )
    ))
      rangeCost.set(row[0], (rangeCost.get(row[0]) ?? 0) + row[1]);
    const topTwelve = [...rangeCost.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, 12)
      .map(([key]) => key);
    expect([...named].sort()).toEqual(topTwelve.sort());
    const otherDay1 = rows.find((row) => row.date === '2026-10-01' && row.model === 'Other models');
    expect(otherDay1?.inputTokens).toBe(
      claudeModels
        .filter((model) => !named.has(`claude:${model.model}`))
        .reduce((sum, model) => sum + model.input, 0)
    );
    expect(otherDay1?.inputTokens).toBeGreaterThan(0);
    for (const day of activity.byDay) {
      const parts = rows.filter((row) => row.date === day.date && row.provider === day.provider);
      for (const field of [
        'inputTokens',
        'outputTokens',
        'cacheCreationTokens',
        'cacheReadTokens',
      ] as const)
        expect(parts.reduce((sum, row) => sum + row[field], 0)).toBe(day[field]);
      expect(parts.reduce((sum, row) => sum + row.estimatedCostUsd, 0)).toBeCloseTo(
        day.estimatedCostUsd,
        9
      );
    }
    const sorted = [...rows].sort(
      (a, b) =>
        a.date.localeCompare(b.date) ||
        a.provider.localeCompare(b.provider) ||
        b.estimatedCostUsd - a.estimatedCostUsd
    );
    expect(rows.map((row) => `${row.date}:${row.provider}:${row.model}`)).toEqual(
      sorted.map((row) => `${row.date}:${row.provider}:${row.model}`)
    );
  });
});

describe('analytics contract: every model is published', () => {
  it('lists more than thirty models, never cutting a cheap or unpriced one, ranked by cost then tokens', () => {
    // 34 priced Claude Code models, then OMP models that logged no cost but used many tokens: before, only the
    // 30 costliest rows were published, so these were the first to disappear from every model view. They log
    // no route here, so they count under "other".
    const claudeModels = Array.from({ length: 34 }, (_, index) => ({
      model: `claude-model-${String(index).padStart(2, '0')}`,
      input: (index + 1) * 100_000,
      output: 1000,
    }));
    const ompModels = [
      { model: 'unpriced-qwen-flash', input: 9_000_000, cacheRead: 600_000_000, costFactor: 0 },
      { model: 'unpriced-qwen-max', input: 8_000_000, cacheRead: 500_000_000, costFactor: 0 },
      { model: 'deepseek-fixture', input: 1_000, output: 10, costFactor: 1 },
    ];
    const omp: SourceData = {
      provider: 'omp',
      data: [result([hourRow('2026-10-01T09:00:00Z', ompModels)])],
      fetchedAt: new Date(NOW).toISOString(),
    };
    const activity = project([
      source('claude', result([hourRow('2026-10-01T10:00:00Z', claudeModels)])),
      omp,
    ]);
    expect(activity.models.length).toBe(37);
    const keys = activity.models.map((row) => `${row.provider}:${row.model}`);
    for (const model of ompModels) expect(keys).toContain(`other:${model.model}`);
    const tokens = (row: (typeof activity.models)[number]) =>
      row.inputTokens + row.outputTokens + row.cacheCreationTokens + row.cacheReadTokens;
    for (let index = 1; index < activity.models.length; index++) {
      const [before, after] = [activity.models[index - 1], activity.models[index]];
      expect(before.estimatedCostUsd).toBeGreaterThanOrEqual(after.estimatedCostUsd);
      if (before.estimatedCostUsd === after.estimatedCostUsd)
        expect(tokens(before)).toBeGreaterThanOrEqual(tokens(after));
    }
    // the two zero-cost models close the list, the larger by tokens first
    expect(keys.slice(-2)).toEqual(['other:unpriced-qwen-flash', 'other:unpriced-qwen-max']);
    // every provider's tokens are on its model rows: nothing is left in the totals alone
    for (const provider of activity.providers) {
      const listed = activity.models
        .filter((row) => row.provider === provider.provider)
        .reduce((sum, row) => sum + tokens(row), 0);
      expect(listed).toBe(
        provider.totals.inputTokens +
          provider.totals.outputTokens +
          provider.totals.cacheCreationTokens +
          provider.totals.cacheReadTokens
      );
    }
  });
});

describe('analytics contract: request counts', () => {
  it('sums complete buckets and gives null, never 0, when any hour lacks a count', () => {
    const activity = project([
      source(
        'claude',
        result([
          hourRow('2026-10-02T10:00:00Z', [{ model: 'm', input: 1 }], 4),
          hourRow('2026-10-02T11:00:00Z', [{ model: 'm', input: 1 }], 0),
          hourRow('2026-10-01T10:00:00Z', [{ model: 'm', input: 1 }], 2),
        ]),
        result([
          hourRow('2026-10-02T10:00:00Z', [{ model: 'm', input: 1 }], 5),
          hourRow('2026-10-02T11:00:00Z', [{ model: 'm', input: 1 }], null),
          hourRow('2026-10-01T10:00:00Z', [{ model: 'm', input: 1 }], 1),
        ])
      ),
    ]);
    const hour = (iso: string) => activity.byHour.find((row) => row.hour === iso);
    expect(hour('2026-10-02T10:00:00Z')?.requestCount).toBe(9);
    expect(hour('2026-10-02T11:00:00Z')?.requestCount).toBeNull();
    expect(hour('2026-10-01T10:00:00Z')?.requestCount).toBe(3);
    const day = (date: string) => activity.byDay.find((row) => row.date === date);
    expect(day('2026-10-02')?.requestCount).toBeNull();
    expect(day('2026-10-01')?.requestCount).toBe(3);
    // A measured zero stays zero; usageEvents keeps its meaning.
    expect(
      project([
        source('codex', result([hourRow('2026-10-02T10:00:00Z', [{ model: 'm', input: 1 }], 0)])),
      ]).byHour[0].requestCount
    ).toBe(0);
    expect(activity.providers[0].usageEvents).toBe(4 + 0 + 2 + 5 + 1);
  });
});

/** f45fa923 `detectAnomalies` and its helpers, verbatim apart from types, as the golden oracle. */
const ORIGINAL_THRESHOLDS = {
  HIGH_INPUT_TOKENS: 10_000_000,
  HIGH_IO_RATIO: 100,
  COST_SPIKE_MULTIPLIER: 2,
  HIGH_CACHE_READ_TOKENS: 1_000_000_000,
};
function originalFormatTokenCount(tokens: number): string {
  if (tokens >= 1_000_000_000) return `${(tokens / 1_000_000_000).toFixed(1)}B`;
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}K`;
  return tokens.toString();
}
interface OriginalDay {
  date: string;
  totalCost: number;
  modelBreakdowns: ModelBreakdown[];
}
function originalDetectAnomalies(dailyData: OriginalDay[]) {
  const anomalies: Array<{
    date: string;
    type: string;
    model?: string;
    value: number;
    threshold: number;
    message: string;
  }> = [];
  const totalCost = dailyData.reduce((sum, day) => sum + day.totalCost, 0);
  const avgDailyCost = dailyData.length > 0 ? totalCost / dailyData.length : 0;
  const costSpikeThreshold = avgDailyCost * ORIGINAL_THRESHOLDS.COST_SPIKE_MULTIPLIER;
  for (const day of dailyData) {
    if (avgDailyCost > 0 && day.totalCost > costSpikeThreshold) {
      const multiplier = Math.round((day.totalCost / avgDailyCost) * 10) / 10;
      anomalies.push({
        date: day.date,
        type: 'cost_spike',
        value: day.totalCost,
        threshold: avgDailyCost,
        message: `Cost ${multiplier}x above daily average ($${Math.round(day.totalCost)} vs $${Math.round(avgDailyCost)})`,
      });
    }
    for (const breakdown of day.modelBreakdowns) {
      if (breakdown.inputTokens > ORIGINAL_THRESHOLDS.HIGH_INPUT_TOKENS) {
        const multiplier =
          Math.round((breakdown.inputTokens / ORIGINAL_THRESHOLDS.HIGH_INPUT_TOKENS) * 10) / 10;
        anomalies.push({
          date: day.date,
          type: 'high_input',
          model: breakdown.modelName,
          value: breakdown.inputTokens,
          threshold: ORIGINAL_THRESHOLDS.HIGH_INPUT_TOKENS,
          message: `Input tokens ${multiplier}x above threshold (${originalFormatTokenCount(breakdown.inputTokens)})`,
        });
      }
      if (breakdown.outputTokens > 0) {
        const ioRatio = breakdown.inputTokens / breakdown.outputTokens;
        if (ioRatio > ORIGINAL_THRESHOLDS.HIGH_IO_RATIO) {
          const multiplier = Math.round((ioRatio / ORIGINAL_THRESHOLDS.HIGH_IO_RATIO) * 10) / 10;
          anomalies.push({
            date: day.date,
            type: 'high_io_ratio',
            model: breakdown.modelName,
            value: ioRatio,
            threshold: ORIGINAL_THRESHOLDS.HIGH_IO_RATIO,
            message: `I/O ratio ${multiplier}x above threshold (${Math.round(ioRatio)}:1)`,
          });
        }
      }
      if (breakdown.cacheReadTokens > ORIGINAL_THRESHOLDS.HIGH_CACHE_READ_TOKENS) {
        const multiplier =
          Math.round(
            (breakdown.cacheReadTokens / ORIGINAL_THRESHOLDS.HIGH_CACHE_READ_TOKENS) * 10
          ) / 10;
        anomalies.push({
          date: day.date,
          type: 'high_cache_read',
          model: breakdown.modelName,
          value: breakdown.cacheReadTokens,
          threshold: ORIGINAL_THRESHOLDS.HIGH_CACHE_READ_TOKENS,
          message: `Cache reads ${multiplier}x above threshold (${originalFormatTokenCount(breakdown.cacheReadTokens)})`,
        });
      }
    }
  }
  return anomalies.sort((a, b) => b.date.localeCompare(a.date));
}

describe('analytics contract: anomalies', () => {
  it('matches the original CCS rules and messages on the same days', () => {
    // One model per day, priced so each day's cost is known; the spike is the last day.
    const days: Array<[string, ModelFixture]> = [
      ['2026-09-27', { model: 'steady', input: 100_000, output: 10_000, costFactor: 1 }],
      ['2026-09-28', { model: 'reader', input: 25_000_000, output: 1_000_000 }],
      ['2026-09-29', { model: 'ratio', input: 1_000_000, output: 5000 }],
      ['2026-09-30', { model: 'cache', cacheRead: 3_200_000_000, output: 100_000 }],
      ['2026-10-01', { model: 'spike', input: 80_000_000, output: 2_000_000 }],
    ];
    const hourly = days.map(([date, model]) => hourRow(`${date}T12:00:00Z`, [model]));
    const activity = project([source('claude', result(hourly))]);
    const oracle = originalDetectAnomalies(
      hourly.map((row) => ({
        date: row.hour.slice(0, 10),
        totalCost: row.totalCost,
        modelBreakdowns: row.modelBreakdowns,
      }))
    );
    expect(oracle.length).toBeGreaterThanOrEqual(5);
    expect(
      activity.anomalies?.items.map(({ provider: _provider, ...item }) => ({
        ...item,
        model: item.model ?? undefined,
      }))
    ).toEqual(oracle.map((item) => ({ ...item, model: item.model })));
    expect(
      activity.anomalies?.items.every(
        (item) => item.provider === (item.type === 'cost_spike' ? null : 'claude')
      )
    ).toBe(true);
    expect(activity.anomalies?.thresholds).toEqual({
      costSpikeMultiplier: 2,
      highInputTokens: 10_000_000,
      highIoRatio: 100,
      highCacheReadTokens: 1_000_000_000,
    });
    expect(activity.anomalies?.summary).toEqual({
      totalAnomalies: oracle.length,
      highInputDays: new Set(
        oracle.filter((item) => item.type === 'high_input').map((item) => item.date)
      ).size,
      highIoRatioDays: new Set(
        oracle.filter((item) => item.type === 'high_io_ratio').map((item) => item.date)
      ).size,
      costSpikeDays: new Set(
        oracle.filter((item) => item.type === 'cost_spike').map((item) => item.date)
      ).size,
      highCacheReadDays: new Set(
        oracle.filter((item) => item.type === 'high_cache_read').map((item) => item.date)
      ).size,
    });
    expect(activity.anomalies?.items.find((item) => item.type === 'cost_spike')?.message).toMatch(
      /^Cost \d+(\.\d)?x above daily average \(\$\d+ vs \$\d+\)$/
    );
  });

  it('keeps at most 100 items, newest first, while the summary counts them all', () => {
    const hourly = Array.from({ length: 31 }, (_, index) =>
      hourRow(new Date(NOW - index * DAY).toISOString(), [
        { model: 'a', input: 20_000_000, output: 1_000_000 },
        { model: 'b', input: 20_000_000, output: 1_000_000 },
        { model: 'c', input: 20_000_000, output: 1_000_000 },
        { model: 'd', input: 20_000_000, output: 1_000_000 },
      ])
    );
    const activity = project([source('codex', result(hourly))], { from: NOW - 31 * DAY });
    expect(activity.anomalies?.items).toHaveLength(100);
    expect(activity.anomalies?.summary.totalAnomalies).toBe(124);
    const dates = activity.anomalies?.items.map((item) => item.date) ?? [];
    expect([...dates].sort().reverse()).toEqual(dates);
  });
});

describe('analytics contract: active and switchable accounts', () => {
  it('reports the active account of each switchable provider and switch targets', async () => {
    const accounts = [
      account('codex', 'codex:a'),
      account('codex', 'codex:b', { isActive: true }),
      account('codex', 'codex:expired', { status: 'needs_sign_in', windows: [] }),
      account('antigravity', 'antigravity:profile:one', {
        isActive: true,
        capabilities: {
          codexProfile: null,
          claudeProfileId: null,
          claudePlatforms: [],
          antigravityProfileId: 'one',
          antigravityHostIds: ['ubuntu'],
          antigravityCanActivate: true,
        },
      }),
      account('antigravity', 'antigravity:profile:two', {
        capabilities: {
          codexProfile: null,
          claudeProfileId: null,
          claudePlatforms: [],
          antigravityProfileId: 'two',
          antigravityHostIds: ['ubuntu'],
          antigravityCanActivate: false,
        },
      }),
      account('claude', 'claude:one'),
      account('kimi-code', 'kimi-code:usage'),
    ];
    const result = await analyticsService(accounts, {
      extra: {
        settings: {
          refreshIntervalSeconds: 60,
          hiddenProviders: ['kimi-code'],
          hiddenAccountIds: ['codex:a'],
        },
      },
    }).get(QUERY);
    expect(result.summary.activeAccountIds).toEqual({
      codex: 'codex:b',
      antigravity: 'antigravity:profile:one',
    });
    expect(result.summary.activeCodexAccountId).toBe(result.summary.activeAccountIds.codex ?? null);
    const byId = new Map(result.accounts.map((row) => [row.id, row]));
    expect(byId.get('antigravity:profile:one')?.isActive).toBe(true);
    expect(
      Object.fromEntries(result.accounts.map((row) => [row.id, [row.switchable, row.hidden]]))
    ).toEqual({
      'codex:a': [true, true],
      'codex:b': [true, false],
      'codex:expired': [false, false],
      'antigravity:profile:one': [true, false],
      'antigravity:profile:two': [false, false],
      'claude:one': [false, false],
      'kimi-code:usage': [false, true],
    });
    // Quota points keep the per-sample active truth for every switchable provider.
    expect(byId.get('antigravity:profile:one')?.windows[0].points[0].isActive).toBe(true);
    expect(result.providers.find((row) => row.provider === 'kimi-code')?.visible).toBe(false);
  });

  it('prefers registry fields a newer dashboard already sends', async () => {
    const result = await analyticsService([
      { ...account('codex', 'codex:a'), switchable: false, hidden: true } as DashboardAccount,
    ]).get(QUERY);
    expect(result.accounts[0]).toMatchObject({ switchable: false, hidden: true });
    expect(result.summary.activeAccountIds).toEqual({ codex: null, antigravity: null });
  });
});

describe('analytics contract: providers from data', () => {
  it('lists providers with accounts or activity, in table order, with activity flags', async () => {
    const accounts = [
      account('qwen', 'qwen:usage'),
      account('codex', 'codex:a'),
      account('antigravity', 'antigravity:usage'),
    ];
    const result = await analyticsService(accounts, {
      activity: async () => ({
        ...noActivity,
        coverage: { oldestHourAt: NOW - DAY, providersWithActivity: ['claude'] },
      }),
    }).get({ ...QUERY, provider: 'codex' });
    expect(
      result.providers.map((row) => [
        row.provider,
        row.order,
        row.accountCount,
        row.hasActivity,
        row.hasQuotaHistory,
      ])
    ).toEqual([
      ['claude', 0, 0, true, false],
      ['codex', 1, 1, false, true],
      ['antigravity', 2, 1, false, true],
      ['qwen', 6, 1, false, true],
    ]);
    expect(result.providers.map((row) => row.label)).toEqual([
      'Claude',
      'Codex',
      'Antigravity',
      'Qwen token plan',
    ]);
    // Kimi Code has no account and no activity, so it is absent.
    expect(result.providers.some((row) => row.provider === 'kimi-code')).toBe(false);
    expect(result.accounts.map((row) => row.id)).toEqual(['codex:a']);
  });

  it('follows a dashboard provider registry when one is present', async () => {
    const result = await analyticsService(
      [account('codex', 'codex:a'), account('zai', 'zai:usage')],
      {
        extra: {
          providers: [
            { id: 'zai', label: 'Z.ai', order: 0, visible: false, switchable: false },
            { id: 'codex', label: 'Codex CLI', order: 5, visible: true, switchable: true },
          ],
        },
      }
    ).get(QUERY);
    expect(result.providers.map((row) => [row.provider, row.label, row.visible])).toEqual([
      ['zai', 'Z.ai', false],
      ['codex', 'Codex CLI', true],
    ]);
  });

  it('computes coverage from the retained snapshot for every provider', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      now: () => NOW,
      scope: () => `${ccsHome}/coverage`,
      responseBudgetMs: 100,
      pricing,
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture/codex', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async (request) =>
        request.kind === 'claude'
          ? result([hourRow(new Date(NOW - 2 * DAY).toISOString(), [{ model: 'm', input: 1 }])])
          : result([hourRow(new Date(NOW - 20 * DAY).toISOString(), [{ model: 'm', input: 1 }])]),
    });
    const week = await service.get(QUERY, NOW - 7 * DAY, NOW);
    expect(week.coverage?.providersWithActivity).toEqual(['claude']);
    expect(week.coverage?.oldestHourAt).toBe(
      Date.parse(new Date(NOW - 20 * DAY).toISOString().slice(0, 13) + ':00:00Z')
    );
    // A quota-only filter still reports coverage from what was already read.
    const quotaOnly = await service.get({ ...QUERY, provider: 'qwen' }, NOW - 30 * DAY, NOW);
    expect(quotaOnly.status).toBe('unavailable');
    expect(quotaOnly.coverage?.providersWithActivity).toEqual(['claude', 'codex']);
  });
});

describe('analytics contract: truthfulness kept', () => {
  it('keeps activity unattributed and never sums quota across accounts', async () => {
    const activity = project(
      [
        source(
          'claude',
          result([hourRow('2026-10-02T10:00:00Z', [{ model: 'model-a', input: 10 }])])
        ),
      ],
      { tz: 'America/New_York' }
    );
    expect(activity.accountAttribution).toBe('unavailable');
    expect(activity.scope).toBe('multi-host-cli');
    expect(activity.costBasis).toBe('estimated-api-equivalent');
    const filtered = project(
      [
        source(
          'claude',
          result([hourRow('2026-10-02T10:00:00Z', [{ model: 'model-a', input: 10 }])])
        ),
      ],
      { query: { account: 'claude:one' } }
    );
    expect(filtered.totals).toBeNull();
    expect(filtered.sessions).toBeNull();
    expect(filtered.byDayModel).toEqual([]);
    const response = await analyticsService([
      account('codex', 'codex:a'),
      account('codex', 'codex:b'),
    ]).get(QUERY);
    for (const row of response.providers)
      expect(Object.keys(row).some((key) => /percent|used|remaining/i.test(key))).toBe(false);
    expect(Object.keys(response.summary).some((key) => /percent|used|remaining/i.test(key))).toBe(
      false
    );
  });
});
