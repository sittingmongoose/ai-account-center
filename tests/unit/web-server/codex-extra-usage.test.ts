import { describe, expect, test } from 'bun:test';
import {
  codexResetTimestamp,
  normalizeCodexExtraUsage,
  normalizeCodexResetCreditDetails,
} from '../../../src/cliproxy/quota/codex-extra-usage';
import { getCodexAdditionalUsageWindows } from '../../../src/web-server/usage/codex-network-extra-windows';
import type { CodexQuotaResult } from '../../../src/cliproxy/quota/quota-types';

describe('Codex extra usage and packs', () => {
  test('retains exact raw credit units, zero and unknown values', () => {
    expect(
      normalizeCodexExtraUsage({
        credits: { has_credits: true, unlimited: false, balance: '62500' },
      }).credits
    ).toEqual({ hasCredits: true, unlimited: false, balance: 62500 });
    expect(normalizeCodexExtraUsage({ credits: { balance: '0' } }).credits?.balance).toBe(0);
    expect(
      normalizeCodexExtraUsage({ credits: { balance: 'secret-value' } }).credits?.balance
    ).toBeNull();
    expect(normalizeCodexExtraUsage({})).toEqual({});
  });

  test('monthly spending uses provider reset and does not borrow billing expiry', () => {
    const result = normalizeCodexExtraUsage({
      spend_control: {
        individual_limit: {
          used: '2.5',
          limit: 10,
          remaining_percent: 75,
          reset_at: 1791429509,
        },
      },
    });
    expect(result.monthlySpend).toEqual({
      used: 2.5,
      limit: 10,
      remainingPercent: 75,
      resetAt: '2026-10-08T03:18:29.000Z',
    });
    expect(codexResetTimestamp(null)).toBeNull();
    expect(codexResetTimestamp(Infinity)).toBeNull();
    expect(codexResetTimestamp(-1)).toBeNull();
  });

  test('pack details whitelist availability and real nullable expiry without IDs', () => {
    const credits = normalizeCodexResetCreditDetails({
      credits: [
        {
          id: 'never-export',
          status: 'available',
          reset_type: 'codex_rate_limits',
          expires_at: '2026-10-29T19:07:59.188619Z',
        },
        { status: 'available', reset_type: 'codex_rate_limits', expires_at: null },
        { status: 'consumed', reset_type: 'codex_rate_limits', expires_at: '2026-10-30T00:00:00Z' },
      ],
    });
    expect(credits).toEqual([
      { resetType: 'codex_rate_limits', expiresAt: '2026-10-29T19:07:59.188Z' },
      { resetType: 'codex_rate_limits', expiresAt: null },
    ]);
    expect(JSON.stringify(credits)).not.toContain('never-export');
  });

  test('balances and extra model windows never become rotation core keys', () => {
    const quota: CodexQuotaResult = {
      success: true,
      planType: 'pro',
      lastUpdated: 0,
      windows: [
        {
          label: 'Primary',
          category: 'usage',
          cadence: 'weekly',
          usedPercent: 80,
          remainingPercent: 20,
          resetAfterSeconds: null,
          resetAt: null,
        },
        {
          label: 'Code Review',
          featureLabel: 'Code Review',
          category: 'code-review',
          cadence: 'weekly',
          usedPercent: 100,
          remainingPercent: 0,
          resetAfterSeconds: null,
          resetAt: '2026-10-02T00:00:00Z',
        },
      ],
      coreUsage: {
        fiveHour: null,
        weekly: { label: 'Primary', remainingPercent: 20, resetAt: null, resetAfterSeconds: null },
      },
      credits: { balance: 62500, hasCredits: true, unlimited: false },
      resetCredits: {
        available: 1,
        applicable: 0,
        credits: [{ resetType: 'codex_rate_limits', expiresAt: '2026-10-29T19:07:59Z' }],
      },
    };
    const result = getCodexAdditionalUsageWindows(quota);
    expect(result.quotaWindows).toHaveLength(1);
    expect(result.quotaWindows[0].key).toStartWith('extra_');
    expect(result.quotaWindows.map((row) => row.key)).not.toContain('seven_day');
    expect(result.balanceWindows[0]).toMatchObject({
      remaining: 62500,
      usedPercent: null,
      resetAt: null,
      expiresAt: null,
    });
    expect(result.balanceWindows[1]).toMatchObject({
      remaining: 1,
      unit: 'resets',
      resetAt: null,
      expiresAt: '2026-10-29T19:07:59Z',
    });
    expect(quota.coreUsage.weekly?.remainingPercent).toBe(20);
  });

  test('partial pack details do not invent total expiration', () => {
    const result = getCodexAdditionalUsageWindows({
      success: true,
      planType: null,
      lastUpdated: 0,
      windows: [],
      resetCredits: {
        available: 2,
        applicable: 1,
        credits: [{ resetType: 'reset', expiresAt: '2026-10-29T00:00:00Z' }],
      },
    });
    expect(result.balanceWindows).toHaveLength(1);
    expect(result.balanceWindows[0]).toMatchObject({ remaining: 2, expiresAt: null });
  });
});
