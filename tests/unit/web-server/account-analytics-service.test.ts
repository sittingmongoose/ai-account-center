import { describe, expect, it } from 'bun:test';
import { AccountAnalyticsService } from '../../../src/web-server/services/account-analytics-service';
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
  AccountAnalyticsQuery,
} from '../../../src/web-server/services/account-analytics-types';

const NOW = Date.parse('2026-10-01T16:10:00Z');
const QUERY: AccountAnalyticsQuery = {
  platform: 'mac',
  range: '7d',
  provider: 'all',
  account: 'all',
};
const noActivity: AccountAnalyticsActivity = {
  status: 'unavailable',
  scope: 'ubuntu-local-cli',
  timezone: 'UTC',
  accountAttribution: 'unavailable',
  costBasis: 'estimated-api-equivalent',
  fetchedAt: null,
  message: 'No fixture local logs',
  totals: null,
  providers: [],
  byDay: [],
  byHour: [],
  models: [],
};

function account(
  provider: DashboardProvider = 'codex',
  id = `${provider}:first`
): DashboardAccount {
  return {
    id,
    provider,
    providerLabel: provider,
    label: 'first',
    email: `${provider}@example.com`,
    plan: 'paid',
    platform: provider === 'claude' ? 'mac' : 'ubuntu',
    source: `${provider} quota`,
    status: 'ok',
    message: null,
    fetchedAt: new Date(NOW).toISOString(),
    sampledAt: new Date(NOW).toISOString(),
    isActive: provider === 'codex',
    capabilities: {
      codexProfile: provider === 'codex' ? 'first' : null,
      claudeProfileId: null,
      claudePlatforms: [],
    },
    windows: [
      {
        key: 'five_hour',
        label: '5 hour',
        usedPercent: 50,
        remainingPercent: 50,
        resetAt: '2026-10-01T19:00:00Z',
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
    ],
  };
}

function dashboard(accounts: DashboardAccount[]): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: new Date(NOW).toISOString(),
    accounts,
    settings: { refreshIntervalSeconds: 60 },
    codexAutoSwitch: {
      enabled: true,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'healthy',
      message: 'Healthy',
      activationInProgress: false,
    },
  };
}

function memory(
  initial: AccountAnalyticsHistoryData | null = null
): AccountAnalyticsHistoryStore & { data: AccountAnalyticsHistoryData | null; writes: number } {
  return {
    data: initial,
    writes: 0,
    async read() {
      return this.data;
    },
    async write(data) {
      this.writes++;
      this.data = structuredClone(data);
    },
  };
}

describe('account quota analytics', () => {
  it('passes explicit activity refresh through without forcing quota sources twice or exposing the flag as a filter', async () => {
    const dashboardCalls: Array<[string | undefined, boolean | undefined]> = [];
    const activityCalls: AccountAnalyticsQuery[] = [];
    const service = new AccountAnalyticsService({
      getDashboard: async (...args) => {
        dashboardCalls.push(args);
        return dashboard([account()]);
      },
      getActivity: async (query) => {
        activityCalls.push(query);
        return noActivity;
      },
      createHistoryStore: () => memory(),
      now: () => NOW,
    });
    const result = await service.get({ ...QUERY, refresh: true });
    expect(dashboardCalls).toEqual([['mac', false]]);
    expect(activityCalls).toEqual([{ ...QUERY, refresh: true }]);
    expect(result.filters).toEqual({ platform: 'mac', provider: 'all', account: 'all' });
  });
  it('includes all nine providers and all fourteen logical accounts with the actual active Codex row', async () => {
    const accounts = [
      account('codex', 'codex:a'),
      account('codex', 'codex:b'),
      account('codex', 'codex:c'),
      ...['a', 'b', 'c', 'd'].map((id) => account('claude', `claude:${id}`)),
      ...(
        ['antigravity', 'muse', 'cursor', 'kimi-code', 'qwen', 'zai', 'opencode-go'] as const
      ).map((provider) => account(provider)),
    ];
    accounts.forEach((row) => {
      row.isActive = row.id === 'codex:c';
    });
    const store = memory();
    const calls: Array<[string | undefined, boolean | undefined]> = [];
    const service = new AccountAnalyticsService({
      getDashboard: async (...args) => {
        calls.push(args);
        return dashboard(accounts);
      },
      createHistoryStore: () => store,
      getActivity: async () => noActivity,
      now: () => NOW,
    });
    const result = await service.get(QUERY);
    expect(result.summary).toEqual({
      accountCount: 14,
      availableAccounts: 14,
      activeCodexAccountId: 'codex:c',
      sampleCount: 14,
    });
    expect(result.providers).toHaveLength(9);
    expect(result.accounts).toHaveLength(14);
    expect(calls).toEqual([['mac', false]]);
    expect(result.accounts[0].windows[0].points[0].usedPercent).toBe(50);
    expect(result.accounts[0].windows[0].points[0].resetAt).toBe('2026-10-01T19:00:00.000Z');
    expect(store.writes).toBe(1);
    await service.get({ ...QUERY, platform: 'windows' });
    expect(store.writes).toBe(1);
    expect(store.data?.records).toHaveLength(14);
  });

  it('filters exact logical account/provider and never attributes shared CLI history to it', async () => {
    const first = account();
    const other = account('qwen');
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([first, other]),
      createHistoryStore: () => memory(),
      getActivity: async (query) => ({ ...noActivity, message: query.account }),
      now: () => NOW,
    });
    const result = await service.get({ ...QUERY, provider: 'codex', account: first.id });
    expect(result.accounts.map((row) => row.id)).toEqual([first.id]);
    expect(result.providers.map((row) => row.provider)).toEqual(['codex']);
    expect(result.activity.totals).toBeNull();
    const mismatch = await service.get({ ...QUERY, provider: 'qwen', account: first.id });
    expect(mismatch.accounts).toEqual([]);
    expect(mismatch.summary.accountCount).toBe(0);
    expect(mismatch.summary.activeCodexAccountId).toBe(first.id);
  });

  it('deduplicates retained balances at their original time while fresh core readings continue', async () => {
    let now = NOW;
    const originalSample = new Date(NOW - 3_600_000).toISOString();
    const current = account('claude');
    current.source = 'Claude Desktop live quota on Windows';
    current.windows.push({
      key: 'prepaid_balance',
      label: 'Extra balance',
      kind: 'balance',
      usedPercent: null,
      remainingPercent: null,
      used: 12.123456,
      limit: 20.654321,
      remaining: -4.123456,
      resetAt: '2026-10-02T01:02:03Z',
      expiresAt: '2026-10-20T04:05:06Z',
      unit: 'USD',
      windowMinutes: null,
      status: 'cached',
      sampledAt: originalSample,
    });
    const store = memory();
    const deps = {
      getDashboard: async () => dashboard([current]),
      createHistoryStore: () => store,
      getActivity: async () => noActivity,
      now: () => now,
    };
    const service = new AccountAnalyticsService(deps);
    const query = { ...QUERY, range: '24h' as const };
    await service.get(query);
    for (let index = 1; index <= 2; index++) {
      now = NOW + index * 3_600_000;
      current.sampledAt = current.fetchedAt = new Date(now).toISOString();
      current.windows[0].usedPercent = 50 + index;
      await service.get(query);
    }
    // A new service simulates loading persisted normalized observations cold.
    const result = await new AccountAnalyticsService(deps).get(query);
    const balance = result.accounts[0].windows.find((window) => window.key === 'prepaid_balance');
    expect(balance?.points).toHaveLength(1);
    expect(balance?.points[0]).toMatchObject({
      sampledAt: originalSample,
      observedAt: new Date(now).toISOString(),
      status: 'cached',
      source: 'Claude Desktop live quota on Windows (cached window)',
      platform: 'windows',
      used: 12.123456,
      limit: 20.654321,
      remaining: -4.123456,
      resetAt: '2026-10-02T01:02:03.000Z',
      expiresAt: '2026-10-20T04:05:06.000Z',
    });
    const core = result.accounts[0].windows.find((window) => window.key === 'five_hour');
    expect(core?.points.map((point) => point.sampledAt)).toEqual(
      [NOW, NOW + 3_600_000, NOW + 2 * 3_600_000].map((time) => new Date(time).toISOString())
    );
    expect(core?.points.map((point) => point.usedPercent)).toEqual([50, 51, 52]);
    expect(core?.points.every((point) => point.status === 'ok')).toBe(true);
    expect(core?.points.every((point) => point.source === current.source)).toBe(true);
  });

  it('does not bring an old cached optional sample into a newer selected range', async () => {
    const current = account('claude');
    current.windows.push({
      ...current.windows[0],
      key: 'extra_usage',
      kind: 'extra_usage',
      status: 'cached',
      sampledAt: new Date(NOW - 25 * 3_600_000).toISOString(),
    });
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([current]),
      createHistoryStore: () => memory(),
      getActivity: async () => noActivity,
      now: () => NOW,
    });
    const result = await service.get({ ...QUERY, range: '24h' });
    expect(
      result.accounts[0].windows.find((window) => window.key === 'extra_usage')?.points
    ).toEqual([]);
    expect(
      result.accounts[0].windows.find((window) => window.key === 'five_hour')?.points
    ).toHaveLength(1);
  });

  it('retains genuine reset drops, signed balances, and unavailable gaps rather than zero-filling them', async () => {
    let now = NOW;
    let current = account();
    current.windows.push({
      key: 'wallet',
      label: 'Extra credits',
      kind: 'balance',
      usedPercent: null,
      remainingPercent: null,
      used: null,
      limit: null,
      unit: 'credits',
      remaining: -1.75,
      expiresAt: '2026-10-10T00:00:00Z',
      resetAt: null,
      windowMinutes: null,
    });
    const store = memory();
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([current]),
      now: () => now,
      createHistoryStore: () => store,
      getActivity: async () => noActivity,
    });
    await service.get(QUERY);
    now += 3_600_000;
    current = { ...current, status: 'error', windows: [] };
    await service.get(QUERY);
    now += 3_600_000;
    current = account();
    current.sampledAt = new Date(now).toISOString();
    current.windows[0].usedPercent = 0;
    current.windows[0].remainingPercent = 100;
    current.windows[0].resetAt = null;
    const result = await service.get(QUERY);
    const points = result.accounts[0].windows.find((row) => row.key === 'five_hour')?.points;
    expect(points?.map((point) => point.usedPercent)).toEqual([50, null, 0]);
    expect(points?.map((point) => point.status)).toEqual(['ok', 'error', 'ok']);
    expect(points?.[2].resetAt).toBeNull();
    const wallet = result.accounts[0].windows.find((row) => row.key === 'wallet');
    expect(wallet?.points[0].remaining).toBe(-1.75);
    expect(wallet?.points[0].expiresAt).toBe('2026-10-10T00:00:00.000Z');
    expect(wallet?.points[1].remaining).toBeNull();
    expect(wallet?.remaining).toBeNull();
    expect(wallet?.expiresAt).toBeNull();
    expect(result.accounts[0].windows.find((row) => row.key === 'five_hour')?.usedPercent).toBe(0);
    expect(
      result.accounts[0].windows.find((row) => row.key === 'five_hour')?.remainingPercent
    ).toBe(100);
  });

  it.each(['ok', 'unavailable'] as const)(
    'keeps prior Ubuntu Muse graph points but clears current metrics when the latest Mac source has no quotas (%s)',
    async (status) => {
      let now = NOW;
      let current = account('muse');
      current.platform = 'ubuntu';
      current.source = 'Muse authenticated quota on Ubuntu';
      current.windows[0] = {
        ...current.windows[0],
        usedPercent: 0,
        remainingPercent: 100,
        used: 0,
        limit: 50,
        remaining: 50,
        unlimited: true,
        enabled: true,
        expiresAt: '2026-10-10T00:00:00Z',
      };
      current.windows.push({
        ...current.windows[0],
        key: 'weekly',
        label: 'Weekly',
        windowMinutes: 10080,
        usedPercent: 6,
        remainingPercent: 94,
        used: 3,
        remaining: 47,
      });
      const store = memory();
      const service = new AccountAnalyticsService({
        getDashboard: async () => dashboard([current]),
        now: () => now,
        createHistoryStore: () => store,
        getActivity: async () => noActivity,
      });
      await service.get(QUERY);
      now += 3_600_000;
      current = {
        ...current,
        status,
        platform: 'mac',
        source: 'Muse signed native account on Mac',
        windows: [],
        fetchedAt: new Date(now).toISOString(),
        sampledAt: new Date(now).toISOString(),
      };
      const result = await service.get(QUERY);
      expect(result.accounts[0].source).toBe('Muse signed native account on Mac');
      expect(result.accounts[0].status).toBe(status);
      expect(result.accounts[0].windows).toHaveLength(2);
      for (const window of result.accounts[0].windows) {
        expect({
          usedPercent: window.usedPercent,
          remainingPercent: window.remainingPercent,
          used: window.used,
          limit: window.limit,
          remaining: window.remaining,
          resetAt: window.resetAt,
          expiresAt: window.expiresAt,
        }).toEqual({
          usedPercent: null,
          remainingPercent: null,
          used: null,
          limit: null,
          remaining: null,
          resetAt: null,
          expiresAt: null,
        });
        expect(window.unlimited).toBeUndefined();
        expect(window.enabled).toBeUndefined();
        expect(window.points[0].platform).toBe('ubuntu');
        expect(window.points[0].source).toBe('Muse authenticated quota on Ubuntu');
        expect(window.points[0].sampledAt).toBe(new Date(NOW).toISOString());
        expect(window.points[1].platform).toBe('mac');
        expect(window.points[1].usedPercent).toBeNull();
      }
      const weekly = result.accounts[0].windows.find((window) => window.key === 'weekly');
      expect(weekly?.label).toBe('Weekly');
      expect(weekly?.windowMinutes).toBe(10080);
      expect(weekly?.points[0].usedPercent).toBe(6);
      expect(
        result.accounts[0].windows.find((window) => window.key === 'five_hour')?.points[0]
          .usedPercent
      ).toBe(0);
    }
  );

  it('keeps a removed unit as history without attaching its old balance to the new current unit', async () => {
    let now = NOW;
    let current = account('qwen');
    current.windows = [
      {
        ...current.windows[0],
        key: 'allowance',
        label: 'Credits',
        unit: 'credits',
        kind: 'balance',
        remaining: 250,
        usedPercent: null,
        remainingPercent: null,
      },
    ];
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([current]),
      now: () => now,
      createHistoryStore: () => memory(),
      getActivity: async () => noActivity,
    });
    await service.get(QUERY);
    now += 3_600_000;
    current = {
      ...current,
      sampledAt: new Date(now).toISOString(),
      windows: [{ ...current.windows[0], label: 'Tokens', unit: 'tokens', remaining: 700 }],
    };
    const result = await service.get(QUERY);
    const credits = result.accounts[0].windows.find((window) => window.unit === 'credits');
    const tokens = result.accounts[0].windows.find((window) => window.unit === 'tokens');
    expect(tokens?.remaining).toBe(700);
    expect(credits?.remaining).toBeNull();
    expect(credits?.points.map((point) => point.remaining)).toEqual([250, null]);
    expect(tokens?.points.map((point) => point.remaining)).toEqual([null, 700]);
  });

  it('does not mix prior quota data into a changed account email even when the profile id is reused', async () => {
    let current = account();
    const store = memory();
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([current]),
      createHistoryStore: () => store,
      getActivity: async () => noActivity,
      now: () => NOW,
    });
    await service.get(QUERY);
    current = { ...current, email: 'other@example.com', windows: [] };
    const result = await service.get(QUERY);
    expect(result.accounts[0].sampleCount).toBe(0);
    expect(result.accounts[0].windows).toEqual([]);
  });

  it('reports unreadable history without overwriting it and still returns current quota', async () => {
    let writes = 0;
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([account()]),
      createHistoryStore: () => ({
        read: async () => {
          throw new Error('private raw secret sentinel');
        },
        write: async () => {
          writes++;
        },
      }),
      getActivity: async () => {
        throw new Error('raw token sentinel');
      },
      now: () => NOW,
    });
    const result = await service.get(QUERY);
    expect(result.history.status).toBe('unavailable');
    expect(result.accounts[0].windows[0].usedPercent).toBe(50);
    expect(result.activity.status).toBe('unavailable');
    expect(JSON.stringify(result)).not.toContain('sentinel');
    expect(writes).toBe(0);
  });

  it('downsamples actual readings to bounded latest points without averaging unrelated quotas', async () => {
    let history: AccountAnalyticsHistoryData | null = null;
    for (let index = 0; index < 2880; index++) {
      const timestamp = NOW - (2879 - index) * 60_000;
      const current = account();
      current.sampledAt = new Date(timestamp).toISOString();
      current.windows[0].usedPercent = index % 101;
      history = appendAccountAnalyticsSnapshot(history, [current], timestamp);
    }
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([account()]),
      createHistoryStore: () => memory(history),
      getActivity: async () => noActivity,
      now: () => NOW,
    });
    const result = await service.get({ ...QUERY, range: '24h' });
    expect(result.range.bucketMinutes).toBe(10);
    expect(result.accounts[0].windows[0].points.length).toBeLessThanOrEqual(145);
    expect(result.accounts[0].windows[0].points.at(-1)?.usedPercent).toBe(50);
  });

  it('coalesces background desktop selectors into one measured account and uses actual sample source', async () => {
    const row = account('claude');
    row.source = 'Claude Desktop live quota on Windows';
    const store = memory();
    let resolve!: (value: AccountDashboard) => void;
    const pending = new Promise<AccountDashboard>((complete) => {
      resolve = complete;
    });
    let calls = 0;
    const service = new AccountAnalyticsService({
      getDashboard: async () => {
        calls++;
        return pending;
      },
      createHistoryStore: () => store,
      now: () => NOW,
      getActivity: async () => noActivity,
    });
    const first = service.sample();
    const second = service.sample();
    expect(first).toBe(second);
    expect(calls).toBe(2);
    resolve(dashboard([row]));
    await Promise.all([first, second]);
    expect(store.data?.records).toHaveLength(1);
    expect(store.data?.records[0].platform).toBe('windows');
  });

  it('prefers a usable quota source over an unavailable desktop selector for the same account', async () => {
    const mac = account('claude');
    mac.status = 'unavailable';
    mac.windows = [];
    const windows = account('claude');
    windows.platform = 'windows';
    windows.sampledAt = new Date(NOW - 60_000).toISOString();
    const store = memory();
    const service = new AccountAnalyticsService({
      getDashboard: async (platform) => dashboard([platform === 'mac' ? mac : windows]),
      createHistoryStore: () => store,
      now: () => NOW,
      getActivity: async () => noActivity,
    });
    await service.sample();
    expect(store.data?.records).toHaveLength(1);
    expect(store.data?.records[0].status).toBe('ok');
    expect(store.data?.records[0].platform).toBe('windows');
    expect(store.data?.records[0].windows[0].usedPercent).toBe(50);
  });

  it('reschedules saved frequency once and ignores old in-flight completion and canceled callbacks', async () => {
    let interval = 60_000;
    let resolve!: (value: AccountDashboard) => void;
    let pending = new Promise<AccountDashboard>((complete) => {
      resolve = complete;
    });
    const timers: Array<{
      callback: () => void;
      interval: number;
      handle: ReturnType<typeof setTimeout>;
    }> = [];
    const canceled: Array<ReturnType<typeof setTimeout> | undefined> = [];
    const service = new AccountAnalyticsService({
      getDashboard: async () => pending,
      createHistoryStore: () => memory(),
      now: () => NOW,
      samplingIntervalMs: () => interval,
      schedule: (callback, intervalMs) => {
        const handle = { unref() {} } as ReturnType<typeof setTimeout>;
        timers.push({ callback, interval: intervalMs, handle });
        return handle;
      },
      cancelSchedule: (handle) => {
        canceled.push(handle);
      },
    });
    service.start();
    interval = 120_000;
    service.reschedule();
    expect(timers.map((timer) => timer.interval)).toEqual([120_000]);
    resolve(dashboard([account()]));
    await service.sample();
    await Promise.resolve();
    expect(timers).toHaveLength(1);
    pending = new Promise<AccountDashboard>((complete) => {
      resolve = complete;
    });
    timers[0].callback();
    service.stop();
    service.start();
    resolve(dashboard([account()]));
    await service.sample();
    await Promise.resolve();
    await Promise.resolve();
    expect(timers).toHaveLength(2);
    expect(timers[1].interval).toBe(120_000);
    timers[0].callback();
    await Promise.resolve();
    expect(timers).toHaveLength(2);
    service.stop();
    expect(canceled.length).toBeGreaterThanOrEqual(3);
  });
});
