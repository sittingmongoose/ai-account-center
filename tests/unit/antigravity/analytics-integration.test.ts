import { describe, expect, it } from 'bun:test';
import {
  analyticsWindow,
  analyticsWindowIdentity,
  appendAccountAnalyticsSnapshot,
  type AccountAnalyticsHistoryData,
} from '../../../src/web-server/services/account-analytics-history';
import { AccountAnalyticsService } from '../../../src/web-server/services/account-analytics-service';
import type {
  AccountDashboard,
  DashboardAccount,
  DashboardAccountWindow,
} from '../../../src/web-server/services/account-dashboard-types';
import type { AccountAnalyticsActivity } from '../../../src/web-server/services/account-analytics-types';

const NOW = Date.parse('2026-10-01T17:00:00Z');
const RESET = '2026-10-02T17:00:00.000Z';
const POOL = `bucket-group:${'a'.repeat(64)}`;
const OTHER_POOL = `bucket-group:${'b'.repeat(64)}`;
const PRIVATE = 'FIXTURE_PRIVATE_SENTINEL';

function window(poolId = POOL): DashboardAccountWindow {
  return {
    key: 'gemini-weekly',
    label: 'Weekly',
    poolId,
    poolIdSource: 'provider-bucket-membership',
    poolLabel: 'Gemini Models',
    usedPercent: 15.25,
    remainingPercent: 84.75,
    resetAt: RESET,
    windowMinutes: 10080,
    used: null,
    limit: null,
    unit: null,
    modelIds: ['gemini/example-reported-model'],
    kind: 'rate_limit',
  };
}
function account(id: string, email = `${id}@example.com`): DashboardAccount {
  return {
    id: `antigravity:profile:${id}`,
    provider: 'antigravity',
    providerLabel: 'Antigravity',
    label: email,
    email,
    plan: 'Google AI Pro',
    platform: 'ubuntu',
    source: 'Antigravity saved login on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: new Date(NOW).toISOString(),
    sampledAt: new Date(NOW).toISOString(),
    isActive: id === 'gmail',
    windows: [window()],
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: id,
      antigravityHostIds: ['ubuntu'],
      antigravityCanActivate: false,
    },
  };
}
const NO_ACTIVITY: AccountAnalyticsActivity = {
  status: 'unavailable',
  scope: 'multi-host-cli',
  timezone: 'UTC',
  accountAttribution: 'unavailable',
  costBasis: 'estimated-api-equivalent',
  fetchedAt: null,
  message: 'Fixture has no inference activity.',
  totals: null,
  providers: [],
  byDay: [],
  byHour: [],
  models: [],
};
function dashboard(accounts: DashboardAccount[]): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: new Date(NOW).toISOString(),
    settings: { refreshIntervalSeconds: 60 },
    accounts,
    codexAutoSwitch: {
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      message: 'Fixture disabled.',
      activationInProgress: false,
    },
  };
}

describe('Antigravity distinct quota history', () => {
  it('persists exact pool provenance and resets while stripping private and unreported metadata', () => {
    const result = analyticsWindow({
      ...window(),
      privateCredential: PRIVATE,
      modelIds: ['gemini/example-reported-model', `bearer ${PRIVATE}`, '../invalid model', null],
    });
    expect(result?.poolId).toBe(POOL);
    expect(result?.poolIdSource).toBe('provider-bucket-membership');
    expect(result?.poolLabel).toBe('Gemini Models');
    expect(result?.modelIds).toEqual(['gemini/example-reported-model']);
    expect(result?.resetAt).toBe(RESET);
    expect(JSON.stringify(result)).not.toContain(PRIVATE);
    const absent = analyticsWindow({
      ...window(),
      poolId: undefined,
      poolIdSource: undefined,
      poolLabel: undefined,
      modelIds: undefined,
    });
    expect(absent).not.toHaveProperty('poolId');
    expect(absent).not.toHaveProperty('modelIds');
  });

  it('retains the old non-pool series key and separates genuinely different reported group membership', () => {
    const legacy = { ...window(), poolId: undefined };
    expect(analyticsWindowIdentity(legacy)).toBe(
      JSON.stringify(['gemini-weekly', 'rate_limit', null])
    );
    expect(analyticsWindowIdentity(window())).not.toBe(analyticsWindowIdentity(window(OTHER_POOL)));
  });

  it('keeps both verified profile streams separate even when email matches and never remaps legacy IDs', () => {
    const old = { ...account('legacy'), id: 'plan-antigravity-ubuntu-legacy' };
    const first = appendAccountAnalyticsSnapshot(null, [old], NOW);
    const history = appendAccountAnalyticsSnapshot(
      first,
      [account('gmail', 'shared@example.com'), account('party', 'shared@example.com')],
      NOW
    );
    expect(history.records).toHaveLength(3);
    expect(new Set(history.records.map((row) => row.identity)).size).toBe(3);
    expect(history.records.map((row) => row.accountId)).toEqual([
      'plan-antigravity-ubuntu-legacy',
      'antigravity:profile:gmail',
      'antigravity:profile:party',
    ]);
  });

  it('shows both registered accounts by default and an exact account filter without synthesizing provider activity', async () => {
    let history: AccountAnalyticsHistoryData | null = null;
    let activityCalls = 0;
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([account('gmail'), account('party')]),
      getActivity: async () => {
        activityCalls++;
        return NO_ACTIVITY;
      },
      createHistoryStore: () => ({
        read: async () => history,
        write: async (data) => {
          history = structuredClone(data);
        },
      }),
      scope: () => '/fixture-injected-no-files',
      now: () => NOW,
    });
    const all = await service.get({
      platform: 'mac',
      range: '24h',
      provider: 'all',
      account: 'all',
    });
    expect(all.accounts.map((row) => row.id)).toEqual([
      'antigravity:profile:gmail',
      'antigravity:profile:party',
    ]);
    expect(all.accounts.every((row) => row.windows[0].points.length === 1)).toBe(true);
    expect(all.accounts[0].windows[0].poolId).toBe(POOL);
    expect(all.accounts[1].windows[0].points[0].resetAt).toBe(RESET);
    const party = await service.get({
      platform: 'windows',
      range: '24h',
      provider: 'antigravity',
      account: 'antigravity:profile:party',
    });
    expect(party.accounts.map((row) => row.id)).toEqual(['antigravity:profile:party']);
    expect(party.activity.status).toBe('unavailable');
    expect(activityCalls).toBe(2);
  });

  it('keeps changed group history without attaching its old reading to the new current pool', async () => {
    let now = NOW;
    let current = account('gmail');
    let history: AccountAnalyticsHistoryData | null = null;
    const service = new AccountAnalyticsService({
      getDashboard: async () => dashboard([current]),
      getActivity: async () => NO_ACTIVITY,
      createHistoryStore: () => ({
        read: async () => history,
        write: async (data) => {
          history = structuredClone(data);
        },
      }),
      scope: () => '/fixture-injected-no-files',
      now: () => now,
    });
    await service.get({ platform: 'mac', range: '24h', provider: 'all', account: 'all' });
    now += 3_600_000;
    current = { ...current, sampledAt: new Date(now).toISOString(), windows: [window(OTHER_POOL)] };
    const result = await service.get({
      platform: 'mac',
      range: '24h',
      provider: 'all',
      account: 'all',
    });
    const previous = result.accounts[0].windows.find((row) => row.poolId === POOL);
    const latest = result.accounts[0].windows.find((row) => row.poolId === OTHER_POOL);
    expect(previous?.points.filter((point) => point.usedPercent !== null)).toHaveLength(1);
    expect(previous?.points[previous.points.length - 1]?.usedPercent).toBeNull();
    expect(previous?.usedPercent).toBeNull();
    expect(latest?.points.filter((point) => point.usedPercent !== null)).toHaveLength(1);
    expect(latest?.usedPercent).toBe(15.25);
  });
});
