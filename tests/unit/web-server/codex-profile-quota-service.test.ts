import { describe, expect, it } from 'bun:test';
import { getCodexProfileQuotas } from '../../../src/web-server/services/codex-profile-quota-service';
import type { BarSummaryRow } from '../../../src/web-server/routes/bar-routes';
import { CODEX_RENEWAL_MESSAGES } from '../../../src/codex-auth/codex-renewal-types';
import { renewalEntry } from './codex-renewal-entry-fixture';

function row(profile: string, overrides: Partial<BarSummaryRow> = {}): BarSummaryRow {
  return {
    account_id: `ccsx:${profile}`,
    profile,
    provider: 'codex',
    displayName: profile,
    tier: 'pro',
    paused: false,
    quota_percentage: 40,
    quotaStatus: 'ok',
    next_reset: '2026-10-02T12:00:00Z',
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: '2026-10-01T12:00:00Z',
    needsReauth: false,
    quotaWindows: [
      {
        key: 'five_hour',
        label: '5h',
        usedPercent: 25,
        remainingPercent: 75,
        resetAt: '2026-10-01T15:00:00Z',
        windowMinutes: 300,
      },
      {
        key: 'seven_day',
        label: 'week',
        usedPercent: 60,
        remainingPercent: 40,
        resetAt: null,
        windowMinutes: 10080,
      },
    ],
    ...overrides,
  };
}

describe('Codex native profile quota DTO', () => {
  it('keeps saved profile identity and explicit used percentages/reset times', async () => {
    const response = await getCodexProfileQuotas({
      listProfiles: async () => [
        { name: 'party', authValid: true },
        { name: 'gmail', authValid: true },
      ],
      getRows: async () => [row('gmail'), row('party')],
      // The fixture reading is current: its five-hour reset is still ahead.
      now: () => Date.parse('2026-10-01T12:30:00Z'),
    });
    expect(response.profiles.map((profile) => profile.profileName)).toEqual(['party', 'gmail']);
    expect(response.profiles[0]).toEqual({
      profileName: 'party',
      status: 'available',
      fetchedAt: '2026-10-01T12:00:00Z',
      windows: [
        {
          key: 'five_hour',
          label: '5-hour limit',
          usedPercent: 25,
          resetsAt: '2026-10-01T15:00:00Z',
        },
        { key: 'seven_day', label: 'Weekly limit', usedPercent: 60 },
      ],
    });
    expect(JSON.stringify(response)).not.toContain('account_id');
  });

  it('marks a reading whose reset has passed since it was sampled, keeping it as history', async () => {
    const listProfiles = async () => [
      { name: 'party', authValid: true },
      { name: 'gmail', authValid: true },
    ];
    const getRows = async () => [
      row('party'),
      // Sampled after the five-hour reset: a current reading of the new window.
      row('gmail', { fetchedAt: '2026-10-01T15:00:00Z' }),
    ];
    const response = await getCodexProfileQuotas({
      listProfiles,
      getRows,
      now: () => Date.parse('2026-10-01T15:00:00Z'),
    });
    expect(response.profiles[0].windows).toEqual([
      {
        key: 'five_hour',
        label: '5-hour limit',
        usedPercent: 25,
        resetsAt: '2026-10-01T15:00:00Z',
        resetPassed: true,
      },
      { key: 'seven_day', label: 'Weekly limit', usedPercent: 60 },
    ]);
    expect(response.profiles[1].windows[0]).toEqual({
      key: 'five_hour',
      label: '5-hour limit',
      usedPercent: 25,
      resetsAt: '2026-10-01T15:00:00Z',
    });
    const before = await getCodexProfileQuotas({
      listProfiles,
      getRows,
      now: () => Date.parse('2026-10-01T14:59:59.999Z'),
    });
    expect(JSON.stringify(before)).not.toContain('resetPassed');
  });

  it('judges a stale local reading by its session time, not by when it was fetched', async () => {
    const response = await getCodexProfileQuotas({
      listProfiles: async () => [
        { name: 'default', authValid: true },
        { name: 'gmail', authValid: true },
      ],
      getRows: async () => [
        // Fetched at 15:10 from a session file last written at 14:00, before the reset.
        row('default', {
          quotaSource: 'local',
          health: 'warning',
          fetchedAt: '2026-10-01T15:10:00Z',
          staleAsOf: '2026-10-01T14:00:00Z',
        }),
        // A fresh local reading carries no staleAsOf and is current after the reset.
        row('gmail', { quotaSource: 'local', fetchedAt: '2026-10-01T15:10:00Z' }),
      ],
      now: () => Date.parse('2026-10-01T15:10:00Z'),
    });
    expect(response.profiles[0]).toMatchObject({
      status: 'available',
      fetchedAt: '2026-10-01T15:10:00Z',
    });
    expect(response.profiles[0].windows[0]).toEqual({
      key: 'five_hour',
      label: '5-hour limit',
      usedPercent: 25,
      resetsAt: '2026-10-01T15:00:00Z',
      resetPassed: true,
    });
    expect(JSON.stringify(response.profiles[1])).not.toContain('resetPassed');
  });

  it('distinguishes expired, missing, and not-yet-retrieved saved logins without zeros', async () => {
    const response = await getCodexProfileQuotas({
      listProfiles: async () => [
        { name: 'gmail', authValid: true },
        { name: 'party', authValid: false },
        { name: 'lime', authValid: true },
      ],
      getRows: async () => [
        row('gmail', { quotaStatus: 'error', needsReauth: true, quotaWindows: [] }),
      ],
    });
    expect(response.profiles.map((profile) => profile.status)).toEqual([
      'reauth_required',
      'not_connected',
      'unavailable',
    ]);
    expect(response.profiles.every((profile) => profile.windows.length === 0)).toBe(true);
  });

  it('returns cached data within its response budget while the slow refresh continues', async () => {
    let release: (rows: BarSummaryRow[]) => void = () => {};
    const pending = new Promise<BarSummaryRow[]>((resolve) => {
      release = resolve;
    });
    const result = await getCodexProfileQuotas({
      listProfiles: async () => [{ name: 'gmail', authValid: true }],
      getRows: () => pending,
      getCachedRows: () => [row('gmail')],
      responseBudgetMs: 1,
    });
    expect(result.profiles[0].status).toBe('available');
    release([row('gmail')]);
    await pending;
  });

  it('suppresses stale usage when the saved profile no longer has valid authentication', async () => {
    const result = await getCodexProfileQuotas({
      listProfiles: async () => [{ name: 'gmail', authValid: false }],
      getRows: async () => [row('gmail')],
    });
    expect(result.profiles[0].status).toBe('not_connected');
    expect(result.profiles[0].windows).toEqual([]);
  });
});

describe('Codex profile quota DTO and saved-login renewal', () => {
  const now = () => Date.parse('2026-10-01T12:30:00Z');

  it('reports a rejected saved login as needing renewal, even while its last reading still shows', async () => {
    const response = await getCodexProfileQuotas({
      listProfiles: async () => [{ name: 'gmail', authValid: true }],
      getRows: async () => [row('gmail')],
      getRenewalStatus: async () => ({ profiles: [renewalEntry('gmail', 'failed', 'dead')] }),
      now,
    });
    expect(response.profiles[0]).toMatchObject({
      profileName: 'gmail',
      status: 'reauth_required',
      message: CODEX_RENEWAL_MESSAGES.dead,
      fetchedAt: '2026-10-01T12:00:00Z',
    });
    expect(response.profiles[0].windows).toHaveLength(2);
  });

  it('keeps the reading when the renewal status cannot be read', async () => {
    const response = await getCodexProfileQuotas({
      listProfiles: async () => [{ name: 'gmail', authValid: true }],
      getRows: async () => [row('gmail')],
      getRenewalStatus: async () => {
        throw new Error('unreadable');
      },
      now,
    });
    expect(response.profiles[0]).toMatchObject({ profileName: 'gmail', status: 'available' });
    expect(response.profiles[0].message).toBeUndefined();
  });
});
