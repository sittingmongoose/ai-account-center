import { describe, expect, it } from 'bun:test';
import type {
  AccountDashboard,
  DashboardAccount,
  DashboardAccountWindow,
} from '../../../src/web-server/services/account-dashboard-types';
import {
  claudeUsageBody,
  codexUsageBody,
  mergeHubRows,
  projectHubAccounts,
  usageHubAuthIndex,
} from '../../../src/web-server/usage-hub/usage-hub-projection';

const SENTINEL = 'sentinel-secret-value-9f2c';

function window(overrides: Partial<DashboardAccountWindow>): DashboardAccountWindow {
  return {
    key: 'five_hour',
    label: '5h',
    usedPercent: 30,
    remainingPercent: 70,
    resetAt: '2026-10-06T15:00:00.000Z',
    windowMinutes: 300,
    used: null,
    limit: null,
    unit: null,
    ...overrides,
  };
}

function account(overrides: Partial<DashboardAccount>): DashboardAccount {
  return {
    id: 'codex:alpha',
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'alpha',
    email: 'alpha@example.com',
    plan: 'pro',
    platform: 'ubuntu',
    source: `Codex saved login on Ubuntu ${SENTINEL}`,
    status: 'cached',
    message: SENTINEL,
    fetchedAt: '2026-10-06T12:00:00.000Z',
    sampledAt: '2026-10-06T12:00:00.000Z',
    isActive: true,
    windows: [
      window({}),
      window({
        key: 'seven_day',
        label: 'week',
        usedPercent: 78,
        remainingPercent: 22,
        resetAt: '2026-10-09T21:32:12.000Z',
        windowMinutes: 10080,
      }),
    ],
    capabilities: { codexProfile: 'alpha', claudeProfileId: null, claudePlatforms: [] },
    ...overrides,
  };
}

function claude(overrides: Partial<DashboardAccount> = {}): DashboardAccount {
  return account({
    id: 'claude:beta',
    provider: 'claude',
    providerLabel: 'Claude',
    label: 'beta',
    email: 'beta@example.com',
    plan: 'max',
    platform: 'mac',
    source: 'Claude Desktop live quota on Windows',
    windows: [
      window({ key: 'five_hour', usedPercent: 10, resetAt: null, kind: 'rate_limit' }),
      window({
        key: 'seven_day',
        usedPercent: 50,
        resetAt: '2026-10-10T00:00:00.000Z',
        windowMinutes: 10080,
        kind: 'rate_limit',
      }),
      window({
        key: 'seven_day_opus',
        usedPercent: 80,
        resetAt: null,
        windowMinutes: 10080,
        kind: 'rate_limit',
      }),
      window({ key: 'seven_day_oauth_apps', usedPercent: 5, windowMinutes: 10080 }),
      window({
        key: 'extra_usage',
        usedPercent: 12,
        kind: 'extra_usage',
        windowMinutes: null,
        resetAt: null,
      }),
    ],
    capabilities: { codexProfile: null, claudeProfileId: 'beta', claudePlatforms: ['mac'] },
    ...overrides,
  });
}

function dashboard(accounts: DashboardAccount[]): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: '2026-10-06T12:00:00.000Z',
    accounts,
    codexAutoSwitch: {
      enabled: false,
      thresholdPercent: 5,
      thresholdUsedPercent: 95,
      pollIntervalSeconds: 60,
      outcome: 'idle',
      message: 'Idle',
      activationInProgress: false,
    },
  };
}

describe('usage hub projection: Codex wham/usage body', () => {
  it('maps the five-hour and weekly windows to primary and secondary in epoch seconds', () => {
    expect(codexUsageBody(account({}))).toEqual({
      plan_type: 'pro',
      rate_limit: {
        primary_window: {
          used_percent: 30,
          reset_at: Date.parse('2026-10-06T15:00:00.000Z') / 1000,
          limit_window_seconds: 18000,
        },
        secondary_window: {
          used_percent: 78,
          reset_at: Date.parse('2026-10-09T21:32:12.000Z') / 1000,
          limit_window_seconds: 604800,
        },
      },
    });
  });

  it('leaves out a window whose reset already passed instead of showing a stale bar', () => {
    const body = codexUsageBody(
      account({
        windows: [
          window({ resetPassed: true }),
          window({ key: 'seven_day', usedPercent: 40, windowMinutes: 10080 }),
        ],
      })
    );
    expect(body?.rate_limit?.primary_window).toBeNull();
    expect(body?.rate_limit?.secondary_window?.used_percent).toBe(40);
  });

  it('has no body when no window has a real percentage (never an invented 0%)', () => {
    expect(codexUsageBody(account({ windows: [] }))).toBeNull();
    expect(
      codexUsageBody(account({ windows: [window({ usedPercent: null, remainingPercent: null })] }))
    ).toBeNull();
  });

  it('keeps an unusual plan out of plan_type', () => {
    expect(codexUsageBody(account({ plan: 'Pro Plan!' }))?.plan_type).toBeUndefined();
  });
});

describe('usage hub projection: Claude oauth/usage body', () => {
  it('maps five_hour, seven_day and model weeklies; leaves other weeklies and extras out', () => {
    expect(claudeUsageBody(claude())).toEqual({
      five_hour: { utilization: 10, resets_at: null },
      seven_day: { utilization: 50, resets_at: '2026-10-10T00:00:00.000Z' },
      limits: [
        {
          kind: 'weekly_scoped',
          percent: 80,
          resets_at: null,
          scope: { model: { display_name: 'Opus' } },
        },
      ],
    });
  });

  it('has no body without a usable window', () => {
    expect(claudeUsageBody(claude({ windows: [] }))).toBeNull();
  });
});

describe('usage hub projection: auth-files rows', () => {
  it('lists Codex then Claude accounts with stable opaque indexes; other providers stay out', () => {
    const other = account({ id: 'cursor:usage', provider: 'cursor', label: 'Cursor' });
    const accounts = projectHubAccounts([dashboard([claude(), other, account({})])]);
    expect(accounts.map((entry) => entry.authFile.id)).toEqual(['codex:alpha', 'claude:beta']);
    expect(accounts[0].authFile).toEqual({
      id: 'codex:alpha',
      auth_index: usageHubAuthIndex('codex:alpha'),
      provider: 'codex',
      type: 'codex',
      label: 'alpha',
      email: 'alpha@example.com',
      disabled: false,
      status: 'active',
      status_message: 'Cached AI Account Center reading.',
      sampled_at: '2026-10-06T12:00:00.000Z',
    });
    expect(usageHubAuthIndex('codex:alpha')).toBe(usageHubAuthIndex('codex:alpha'));
    expect(usageHubAuthIndex('codex:alpha')).toMatch(/^[0-9a-f]{16}$/);
    expect(usageHubAuthIndex('codex:alpha')).not.toBe(usageHubAuthIndex('claude:alpha'));
  });

  it('marks an account without a reading as error with fixed wording, and omits a missing email', () => {
    const [entry] = projectHubAccounts([
      dashboard([account({ windows: [], status: 'needs_sign_in', email: null })]),
    ]);
    expect(entry.usage).toBeNull();
    expect(entry.authFile.status).toBe('error');
    expect(entry.authFile.status_message).toBe(
      'This account needs to sign in again in AI Account Center.'
    );
    expect('email' in entry.authFile).toBe(false);
    expect(entry.authFile.sampled_at).toBeNull();
  });

  it('merges the Mac and Windows projections: a usable reading wins, then the newer sample', () => {
    const stale = claude({ windows: [], sampledAt: '2026-10-06T13:00:00.000Z' });
    const older = claude({ sampledAt: '2026-10-06T10:00:00.000Z' });
    const newer = claude({
      sampledAt: '2026-10-06T11:00:00.000Z',
      windows: [window({ key: 'five_hour', usedPercent: 64 })],
    });
    const merged = mergeHubRows([dashboard([stale, older]), dashboard([newer])]);
    expect(merged).toHaveLength(1);
    expect(merged[0].windows[0].usedPercent).toBe(64);
  });

  it('leaves out Claude profiles that still wait for their first sign-in', () => {
    const pending = claude({
      id: 'claude:new',
      lifecycle: { state: 'pending_sign_in', jobId: null },
    });
    expect(mergeHubRows([dashboard([pending, claude()])]).map((row) => row.id)).toEqual([
      'claude:beta',
    ]);
  });

  it('never serialises collector extras, sources, messages or anything token-like', () => {
    const laden = {
      ...account({}),
      accessToken: SENTINEL,
      refresh_token: SENTINEL,
      cookie: SENTINEL,
      codexHome: `/home/someone/${SENTINEL}`,
      windows: [{ ...window({}), token: SENTINEL } as DashboardAccountWindow],
    } as DashboardAccount;
    const json = JSON.stringify(projectHubAccounts([dashboard([laden, claude()])]));
    expect(json).not.toContain(SENTINEL);
    expect(json).not.toMatch(/token|secret|cookie|password|credential|bearer|authorization/i);
  });
});
