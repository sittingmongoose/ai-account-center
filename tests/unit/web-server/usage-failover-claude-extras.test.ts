import { describe, expect, it } from 'bun:test';
import type { DashboardAccountWindow } from '../../../src/web-server/services/account-dashboard-types';
import {
  parseUsageReading,
  type UsageReading,
} from '../../../src/web-server/services/usage-failover-contract';
import {
  projectClaudeFailoverExtras,
  retainClaudeFailoverExtras,
} from '../../../src/web-server/services/usage-failover-claude-extras';

const NOW = Date.parse('2026-10-14T12:00:00.000Z');
const DAY = 24 * 60 * 60 * 1000;
const CONTEXT = 'a'.repeat(64);
function window(key: string, remaining = 1): DashboardAccountWindow {
  return {
    key,
    label: key,
    usedPercent: null,
    remainingPercent: null,
    resetAt: null,
    expiresAt: null,
    windowMinutes: null,
    used: null,
    limit: null,
    remaining,
    unit: 'USD',
  };
}
function reading(at = NOW, overrides: Partial<UsageReading> = {}): UsageReading {
  return {
    status: 'ok',
    email: 'same@example.test',
    plan: 'max',
    sampledAt: new Date(at).toISOString(),
    fetchedAt: new Date(at).toISOString(),
    windows: [window('five_hour')],
    optionalExtrasContext: CONTEXT,
    optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' },
    ...overrides,
  };
}
function previous(): UsageReading {
  return reading(NOW - 60_000, {
    windows: [
      window('five_hour'),
      window('reset_credits_available', 0),
      window('reset_credit_saved_grant_1', 2),
      window('prepaid_balance', -3),
    ],
  });
}

describe('private Claude failover optional extras', () => {
  it('expires retained extras on cache projection while preserving base quota and original timestamps', () => {
    const old = new Date(NOW - DAY - 1).toISOString();
    const boundary = new Date(NOW - DAY).toISOString();
    const sample = reading(undefined, {
      windows: [
        { ...window('five_hour'), status: 'cached', sampledAt: old },
        { ...window('reset_credits_available'), status: 'cached', sampledAt: old },
        { ...window('prepaid_balance'), status: 'cached', sampledAt: old },
        { ...window('reset_credit_saved_grant_1'), status: 'cached', sampledAt: boundary },
      ],
    });
    const projected = projectClaudeFailoverExtras(sample, NOW);
    expect(projected.windows.map((item) => item.key)).toEqual([
      'five_hour',
      'reset_credit_saved_grant_1',
    ]);
    expect(projected.windows[0]).toEqual(sample.windows[0]);
    expect(projected.windows[1].sampledAt).toBe(boundary);
    expect(projected.sampledAt).toBe(sample.sampledAt);
    expect(sample.windows).toHaveLength(4);
    const restored = parseUsageReading(JSON.parse(JSON.stringify(projected)), NOW + 1)!;
    expect(projectClaudeFailoverExtras(restored, NOW + 1).windows.map((item) => item.key)).toEqual([
      'five_hour',
    ]);
  });

  it('drops retained extras with missing, invalid or future times without dropping fresh extras', () => {
    for (const sampledAt of [undefined, 'invalid', new Date(NOW + 1).toISOString()]) {
      const sample = reading(undefined, {
        windows: [
          window('five_hour'),
          { ...window('prepaid_balance'), status: 'cached', sampledAt },
        ],
      });
      expect(projectClaudeFailoverExtras(sample, NOW).windows.map((item) => item.key)).toEqual([
        'five_hour',
      ]);
    }
    const fresh = reading(NOW - DAY - 1, {
      windows: [window('five_hour'), window('prepaid_balance', 0)],
    });
    expect(projectClaudeFailoverExtras(fresh, NOW)).toBe(fresh);
  });

  it('preserves unavailable groups with original times alongside fresh quota', () => {
    const old = previous();
    const next = reading();
    const result = retainClaudeFailoverExtras(old, next, true, NOW);
    expect(result.sampledAt).toBe(next.sampledAt);
    expect(result.windows[0]).toEqual(next.windows[0]);
    expect(
      result.windows.slice(1).map((item) => [item.remaining, item.status, item.sampledAt])
    ).toEqual([
      [0, 'cached', old.fetchedAt],
      [2, 'cached', old.fetchedAt],
      [-3, 'cached', old.fetchedAt],
    ]);
    expect(old.windows[1]).not.toHaveProperty('status');
  });

  it('clears a successfully empty group while independently retaining the failed group', () => {
    const next = reading(undefined, {
      optionalExtras: { resetCredits: 'ok', prepaidBalance: 'unavailable' },
    });
    expect(
      retainClaudeFailoverExtras(previous(), next, true, NOW).windows.map((item) => item.key)
    ).toEqual(['five_hour', 'prepaid_balance']);
    const empty = reading(undefined, {
      optionalExtras: { resetCredits: 'ok', prepaidBalance: 'ok' },
    });
    expect(retainClaudeFailoverExtras(previous(), empty, true, NOW)).toBe(empty);
  });

  it('does not reintroduce old grants when the fresh reset group reports zero', () => {
    const next = reading(undefined, {
      windows: [
        window('five_hour'),
        window('reset_credits_available', 0),
        window('prepaid_balance', 0),
      ],
    });
    expect(retainClaudeFailoverExtras(previous(), next, true, NOW)).toBe(next);
  });

  it('expires from the original optional sample rather than renewing it at each refresh', () => {
    const old = reading(NOW - DAY, { windows: [window('prepaid_balance')] });
    const first = retainClaudeFailoverExtras(old, reading(), true, NOW);
    expect(first.windows.at(-1)?.sampledAt).toBe(old.fetchedAt);
    const next = reading(NOW + 1);
    expect(retainClaudeFailoverExtras(first, next, true, NOW + 1)).toBe(next);
  });

  it('rejects changed identity, revision, host, capsule context, person or plan', () => {
    const old = previous();
    const unchanged = reading();
    expect(retainClaudeFailoverExtras(old, unchanged, false, NOW)).toBe(unchanged);
    for (const changes of [
      { optionalExtrasContext: 'b'.repeat(64) },
      { optionalExtrasContext: undefined },
      { optionalExtrasContext: 'not-a-digest' },
      { email: 'other@example.test' },
      { plan: 'pro' },
      { optionalExtras: undefined },
    ]) {
      const next = reading(undefined, changes);
      expect(retainClaudeFailoverExtras(old, next, true, NOW)).toBe(next);
    }
  });

  it('does not retain invalid, future or newer-than-response timestamps', () => {
    for (const sampledAt of ['invalid', new Date(NOW + 1).toISOString()]) {
      const old = reading(undefined, { windows: [{ ...window('prepaid_balance'), sampledAt }] });
      const next = reading();
      expect(retainClaudeFailoverExtras(old, next, true, NOW)).toBe(next);
    }
    const next = reading(NOW - 120_000);
    expect(retainClaudeFailoverExtras(previous(), next, true, NOW)).toBe(next);
  });

  it('parses only bounded private availability metadata and a digest', () => {
    const result = parseUsageReading(
      {
        ...reading(),
        optionalExtras: { resetCredits: 'ok', prepaidBalance: 'unavailable', token: 'PRIVATE' },
        auth: 'PRIVATE',
      },
      NOW
    );
    expect(result?.optionalExtrasContext).toBe(CONTEXT);
    expect(result?.optionalExtras).toEqual({ resetCredits: 'ok', prepaidBalance: 'unavailable' });
    expect(JSON.stringify(result)).not.toContain('PRIVATE');
    const invalid = parseUsageReading(
      {
        ...reading(),
        optionalExtrasContext: 'PRIVATE',
        optionalExtras: { resetCredits: 'unknown', prepaidBalance: 'unavailable' },
      },
      NOW
    );
    expect(invalid).not.toHaveProperty('optionalExtrasContext');
    expect(invalid).not.toHaveProperty('optionalExtras');
  });
});
