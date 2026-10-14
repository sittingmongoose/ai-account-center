import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../../../src/web-server/services/account-dashboard-types';
import {
  parseUsageReading,
  type UsageCandidate,
  type UsageFailoverTarget,
  type UsageHelperRequest,
  type UsageHelperResponse,
  type UsageReading,
} from '../../../src/web-server/services/usage-failover-contract';
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';
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
  it('removes expired optional balances after an offline refresh and coordinator reconstruction', async () => {
    const ccsDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-claude-extra-expiry-'));
    try {
      let now = NOW;
      let online = true;
      const candidate: UsageCandidate = {
        locator: '1'.repeat(64),
        revision: '2'.repeat(64),
        selectors: ['claude-desktop-profile:fixture'],
        identity: { kind: 'claude-account-org-v1', key: '3'.repeat(64) },
        email: 'same@example.test',
        plan: 'max',
      };
      const target: UsageFailoverTarget = {
        ccsDir,
        accountId: 'claude:fixture',
        provider: 'claude',
        binding: 'fixture-binding',
        primarySources: [
          {
            host: 'windows',
            platform: 'windows',
            sshHost: 'fixture-windows',
            selector: candidate.selectors[0],
          },
        ],
      };
      const template: DashboardAccount = {
        id: target.accountId,
        provider: 'claude',
        providerLabel: 'Claude',
        label: 'Fixture',
        email: candidate.email,
        plan: null,
        platform: 'windows',
        source: 'Fixture',
        status: 'unavailable',
        message: null,
        sampledAt: null,
        fetchedAt: null,
        windows: [],
        isActive: false,
        capabilities: {
          codexProfile: null,
          claudeProfileId: 'fixture',
          claudePlatforms: ['windows'],
        },
      };
      const deps = {
        now: () => now,
        hosts: async () => [],
        transport: async (
          _host: unknown,
          request: UsageHelperRequest
        ): Promise<UsageHelperResponse> => {
          if (!online) throw new Error('Fixture offline');
          if (request.action === 'inventory')
            return { version: 1, action: 'inventory', candidates: [candidate] };
          return {
            version: 1,
            action: 'collect',
            candidate,
            failureCode: null,
            sample: reading(NOW, {
              windows: [window('five_hour'), window('prepaid_balance')],
            }),
          };
        },
      };
      const service = new UsageFailoverService(deps);
      const live = await service.collect(target, template);
      expect(live.windows.map((item) => item.key)).toEqual(['five_hour', 'prepaid_balance']);
      expect(live.windows[1].status).toBeUndefined();
      online = false;
      now += 25 * 60 * 60 * 1000;
      const offline = await service.collect(target, template);
      expect(offline.status).toBe('cached');
      expect(offline.windows.map((item) => item.key)).toEqual(['five_hour']);
      expect(offline.sampledAt).toBe(live.sampledAt);
      const restored = new UsageFailoverService(deps).peek(target, template);
      expect(restored.status).toBe('cached');
      expect(restored.windows.map((item) => item.key)).toEqual(['five_hour']);
      expect(restored.sampledAt).toBe(live.sampledAt);
    } finally {
      fs.rmSync(ccsDir, { recursive: true, force: true });
    }
  });

  it('expires previously fresh extras when the whole source is cached, including a restored observation', () => {
    const live = previous();
    expect(live.windows.every((item) => item.status === undefined)).toBe(true);
    const restored = parseUsageReading(JSON.parse(JSON.stringify(live)), NOW + DAY + 1)!;
    const cached = { ...restored, status: 'cached' as const };
    const projected = projectClaudeFailoverExtras(cached, NOW + DAY + 1);
    expect(projected.windows.map((item) => item.key)).toEqual(['five_hour']);
    expect(projected.windows[0]).toEqual(live.windows[0]);
    expect(projected.sampledAt).toBe(live.sampledAt);
    expect(projected.fetchedAt).toBe(live.fetchedAt);
    expect(cached.windows).toHaveLength(4);
  });

  it('ages a cached source from original window or sample time before the fetch time', () => {
    const old = new Date(NOW - DAY - 1).toISOString();
    const recent = new Date(NOW - 60_000).toISOString();
    const sample = reading(undefined, {
      status: 'cached',
      sampledAt: old,
      windows: [
        window('five_hour'),
        window('prepaid_balance'),
        { ...window('reset_credit_saved_grant_1'), sampledAt: recent },
      ],
    });
    const projected = projectClaudeFailoverExtras(sample, NOW);
    expect(projected.windows.map((item) => item.key)).toEqual([
      'five_hour',
      'reset_credit_saved_grant_1',
    ]);
    expect(projected.windows[1].sampledAt).toBe(recent);
    const noSample = reading(NOW - DAY - 1, {
      status: 'cached',
      sampledAt: null,
      windows: [window('five_hour'), window('prepaid_balance')],
    });
    expect(projectClaudeFailoverExtras(noSample, NOW).windows.map((item) => item.key)).toEqual([
      'five_hour',
    ]);
    const oldWindow = reading(undefined, {
      status: 'cached',
      windows: [window('five_hour'), { ...window('prepaid_balance'), sampledAt: old }],
    });
    expect(projectClaudeFailoverExtras(oldWindow, NOW).windows.map((item) => item.key)).toEqual([
      'five_hour',
    ]);
  });

  it('keeps whole-source cached extras at the 24-hour boundary and removes them immediately after', () => {
    const sample = reading(NOW - DAY, {
      status: 'cached',
      windows: [window('five_hour'), window('prepaid_balance', 0)],
    });
    expect(projectClaudeFailoverExtras(sample, NOW)).toBe(sample);
    expect(projectClaudeFailoverExtras(sample, NOW + 1).windows.map((item) => item.key)).toEqual([
      'five_hour',
    ]);
    for (const timestamp of [null, 'invalid', new Date(NOW + 1).toISOString()]) {
      const invalid = { ...sample, sampledAt: timestamp, fetchedAt: timestamp };
      expect(projectClaudeFailoverExtras(invalid, NOW).windows.map((item) => item.key)).toEqual([
        'five_hour',
      ]);
    }
  });

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
