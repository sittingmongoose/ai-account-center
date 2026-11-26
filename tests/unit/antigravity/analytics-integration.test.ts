import { describe,expect,it } from 'bun:test';
import {
analyticsWindow,
analyticsWindowIdentity,
appendAccountAnalyticsSnapshot
} from '../../../src/web-server/services/account-analytics-history';
import { accountSeries } from '../../../src/web-server/services/account-analytics-compose';
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

  it('orders T3 and app weeklies first and leaves pre-T3 observations missing', () => {
    const app = { ...window(), label: 'Gemini weekly, in the Antigravity app' };
    const cli = {
      ...window(OTHER_POOL),
      key: 'cli-gemini-weekly',
      label: 'Gemini weekly, in T3 (Antigravity CLI)',
      usedPercent: 34,
      remainingPercent: 66,
    };
    const other = { ...window(), key: 'cli-gemini-5h' };
    const current = { ...account('gmail'), windows: [other, app, cli] };
    const history = appendAccountAnalyticsSnapshot(null, [{ ...current, windows: [app] }], NOW);
    const series = accountSeries(current, history, NOW - 3_600_000, NOW, 10, { hidden: false });
    expect(series.windows.map((row) => row.key)).toEqual([cli.key, app.key, other.key]);
    expect(series.windows[0].usedPercent).toBe(34);
    expect(series.windows[0].points.map((point) => point.usedPercent)).toEqual([null]);
    expect(series.windows[1].points.map((point) => point.usedPercent)).toEqual([15.25]);
  });
});
