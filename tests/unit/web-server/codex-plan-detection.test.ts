import { describe, expect, it } from 'bun:test';
import type { CodexAuthProfilesSummary } from '../../../src/codex-auth/codex-auth-dashboard-service';
import type { BarSummaryRow } from '../../../src/web-server/services/account-dashboard-types';
import { codexAccount } from '../../../src/web-server/services/account-dashboard-projection';

type Profile = CodexAuthProfilesSummary['profiles'][number];

/** The saved login's token still claims the plan it was issued under. */
const profile: Profile = {
  name: 'gmail',
  email: 'gmail@example.com',
  plan: 'plus',
  accountId: 'private-workspace-sentinel',
  codexHome: '/private-saved-path',
  lastUsed: null,
  authValid: true,
};

function row(overrides: Partial<BarSummaryRow> = {}): BarSummaryRow {
  return {
    profile: 'gmail',
    provider: 'codex',
    account_id: 'private-quota-account-sentinel',
    displayName: 'gmail',
    tier: 'plus',
    paused: false,
    quota_percentage: 70,
    quotaStatus: 'ok',
    next_reset: null,
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: '2026-11-19T05:00:00Z',
    needsReauth: false,
    quotaSource: 'network',
    quotaWindows: [
      {
        key: 'five_hour',
        label: '5h',
        usedPercent: 30,
        remainingPercent: 70,
        resetAt: '2026-11-19T09:00:00Z',
        windowMinutes: 300,
      },
    ],
    ...overrides,
  };
}

describe('Codex plan detection on the saved-login row', () => {
  it('follows an upgrade between two reads before the token claim catches up', () => {
    expect(codexAccount(profile, null, row()).plan).toBe('plus');
    expect(codexAccount(profile, null, row({ tier: 'pro' })).plan).toBe('pro');
    expect(codexAccount(profile, null, row({ tier: 'prolite' })).plan).toBe('prolite');
  });

  it('keeps a plan Codex names that AAC has no table entry for', () => {
    for (const tier of ['business', 'enterprise', 'edu', 'go'])
      expect(codexAccount(profile, null, row({ tier })).plan).toBe(tier);
  });

  it('uses the token claim only when no successful read named a plan', () => {
    expect(codexAccount(profile, null, row({ tier: null })).plan).toBe('plus');
    expect(codexAccount(profile, null, row({ tier: 'pro', quotaStatus: 'error' })).plan).toBe(
      'plus'
    );
    expect(codexAccount(profile, null, undefined).plan).toBe('plus');
    expect(codexAccount({ ...profile, plan: null }, null, row({ tier: null })).plan).toBeNull();
  });
});
