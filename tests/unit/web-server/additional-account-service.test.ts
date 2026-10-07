import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import {
  AdditionalAccountService,
  type AdditionalAccountDeps,
} from '../../../src/web-server/services/additional-account-service';
import {
  ADDITIONAL_PROVIDERS,
  AdditionalUsageTransportError,
  type AdditionalUsageSource,
} from '../../../src/web-server/services/additional-usage-transport';
import { additionalAccounts } from '../../../src/web-server/services/account-dashboard-projection';
import { AccountDashboardService } from '../../../src/web-server/services/account-dashboard-service';

const instant = '2026-10-01T02:00:00Z';
const temporaryDirectories: string[] = [];

function payload(extra: Record<string, unknown> = {}): string {
  return JSON.stringify({
    status: 'ok',
    email: 'person@example.com',
    plan: 'Pro',
    fetchedAt: instant,
    sampledAt: instant,
    windows: [
      {
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 23.5,
        remainingPercent: 76.5,
        resetAt: '2026-10-08T02:00:00Z',
        windowMinutes: 10080,
        used: 235,
        limit: 1000,
        unit: 'credits',
      },
    ],
    ...extra,
  });
}

function fixture(overrides: AdditionalAccountDeps = {}) {
  let now = 0;
  let contents: string | null = null;
  const calls: AdditionalUsageSource[] = [];
  const service = new AdditionalAccountService({
    ccsDir: '/tmp/ccs-usage-test',
    now: () => now,
    readManifest: async () => contents,
    runSource: async (source) => {
      calls.push(source);
      return payload();
    },
    ...overrides,
  });
  return {
    service,
    calls,
    setTime: (value: number) => {
      now = value;
    },
    setManifest: (value: string | null) => {
      contents = value;
    },
  };
}

function configuration(sources: unknown[]) {
  return JSON.stringify({ version: 1, sources });
}

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((dir) => fs.rm(dir, { recursive: true })));
});

describe('additional account usage service', () => {
  it('preserves actual overage with reset dates while clamping only derived remaining', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          windows: [
            { key: 'weekly', label: 'Weekly', usedPercent: 125.5, resetAt: '2026-10-08T02:00:00Z' },
            {
              key: 'credit',
              label: 'Credit balance',
              kind: 'balance',
              remaining: -2.5,
              unit: 'USD',
            },
          ],
        }),
    });
    const [account] = await service.get();
    expect(account.windows[0].usedPercent).toBe(125.5);
    expect(account.windows[0].remainingPercent).toBe(0);
    expect(account.windows[0].resetAt).toBe('2026-10-08T02:00:00.000Z');
    expect(account.windows[1].remaining).toBe(-2.5);
  });
  it('returns all seven stable usage-only rows from local Ubuntu defaults', async () => {
    const { service, calls } = fixture();
    const rows = await service.get();
    expect(calls).toHaveLength(7);
    expect(rows.map((row) => row.provider)).toEqual([...ADDITIONAL_PROVIDERS]);
    for (const row of rows) {
      expect(row.id).toBe(`${row.provider}:usage`);
      expect(row.platform).toBe('ubuntu');
      expect(row.source).toBe('Account on Ubuntu');
      expect(row.isActive).toBe(false);
      expect(row.capabilities).toEqual({
        codexProfile: null,
        claudeProfileId: null,
        claudePlatforms: [],
      });
      expect(row.status).toBe('ok');
      expect(row.fetchedAt).toBe('2026-10-01T02:00:00.000Z');
      expect(row.windows[0]?.usedPercent).toBe(23.5);
    }
  });

  it('coalesces concurrent helper calls, including concurrent forced refreshes', async () => {
    let release!: () => void;
    const ready = new Promise<void>((resolve) => {
      release = resolve;
    });
    let calls = 0;
    const { service } = fixture({
      runSource: async () => {
        calls++;
        await ready;
        return payload();
      },
    });
    const first = service.get();
    const second = service.get({ refresh: true });
    await new Promise((resolve) => setTimeout(resolve, 1));
    expect(calls).toBe(7);
    release();
    expect(await first).toEqual(await second);
    await service.get({ refresh: true });
    expect(calls).toBe(7);
  });

  it('caches for two minutes and debounces a forced refresh for five seconds', async () => {
    const { service, calls, setTime } = fixture();
    await service.get({ refresh: true });
    setTime(4_999);
    expect((await service.get({ refresh: true }))[0]?.status).toBe('cached');
    expect(calls).toHaveLength(7);
    setTime(5_000);
    await service.get({ refresh: true });
    expect(calls).toHaveLength(14);
    setTime(124_999);
    await service.get();
    expect(calls).toHaveLength(14);
    setTime(125_000);
    await service.get();
    expect(calls).toHaveLength(21);
  });

  it('backs off failed requests for thirty seconds after completion even on refresh', async () => {
    let now = 0;
    let calls = 0;
    const { service } = fixture({
      now: () => now,
      runSource: async () => {
        calls++;
        now = Math.max(now, 25_000);
        throw new Error('PRIVATE_CREDENTIAL_FAILURE');
      },
    });
    expect((await service.get())[0]?.status).toBe('error');
    now = 54_999;
    await service.get({ refresh: true });
    expect(calls).toBe(7);
    now = 55_000;
    await service.get({ refresh: true });
    expect(calls).toBe(14);
  });

  it('isolates one provider failure while collecting the other six', async () => {
    const { service } = fixture({
      runSource: async (source) => {
        if (source.provider === 'qwen') throw new Error('PRIVATE_RAW_ERROR');
        return payload();
      },
    });
    const rows = await service.get();
    expect(rows.filter((row) => row.status === 'ok')).toHaveLength(6);
    expect(rows.find((row) => row.provider === 'qwen')?.windows).toEqual([]);
    expect(JSON.stringify(rows)).not.toContain('PRIVATE_RAW_ERROR');
  });

  it('keeps an offline source last sample cached with its real original timestamps and retry backoff', async () => {
    let offline = false;
    let calls = 0;
    const { service, setTime } = fixture({
      runSource: async () => {
        calls++;
        if (offline) throw new Error('PRIVATE_OFFLINE_ERROR');
        return payload();
      },
    });
    await service.get();
    offline = true;
    setTime(120_000);
    const rows = await service.get();
    expect(rows[0]?.status).toBe('cached');
    expect(rows[0]?.fetchedAt).toBe('2026-10-01T02:00:00.000Z');
    expect(rows[0]?.sampledAt).toBe('2026-10-01T02:00:00.000Z');
    expect(rows[0]?.windows[0]?.usedPercent).toBe(23.5);
    expect(rows[0]?.message).toContain('last saved sample');
    expect(JSON.stringify(rows)).not.toContain('PRIVATE');
    setTime(149_999);
    await service.get({ refresh: true });
    expect(calls).toBe(14);
    setTime(150_000);
    await service.get();
    expect(calls).toBe(21);
  });

  it('never attributes a last sample from an old source to a newly configured offline host', async () => {
    const { service, setManifest } = fixture({
      runSource: async (source) => {
        if (source.platform === 'mac') throw new Error('PRIVATE_OFFLINE_ERROR');
        return payload();
      },
    });
    await service.get();
    setManifest(configuration([{ provider: 'cursor', platform: 'mac', sshHost: 'mac-usage' }]));
    const row = (await service.get())[2]!;
    expect(row.platform).toBe('mac');
    expect(row.status).toBe('error');
    expect(row.windows).toEqual([]);
    expect(row.sampledAt).toBeNull();
  });

  it('preserves Muse helper cached timestamps through manual refresh and subsequent portal unavailability', async () => {
    let museCalls = 0;
    let museUnavailable = false;
    const original = '2026-10-01T01:00:00Z';
    const { service, setTime, setManifest } = fixture({
      runSource: async (source) => {
        if (source.provider !== 'muse') return payload();
        museCalls++;
        return museUnavailable
          ? payload({
              status: 'unavailable',
              failureCode: 'rate_limited',
              windows: [],
              fetchedAt: instant,
              sampledAt: null,
            })
          : payload({
              status: 'cached',
              fetchedAt: instant,
              sampledAt: original,
              message:
                'Muse is limiting requests; showing the last successful usage reading. Refresh resumes automatically.',
            });
      },
    });
    setManifest(configuration([{ provider: 'muse', platform: 'mac', sshHost: 'muse-mac' }]));
    const initial = (await service.get()).find((account) => account.provider === 'muse')!;
    expect(initial.status).toBe('cached');
    expect(initial.platform).toBe('mac');
    expect(initial.sampledAt).toBe('2026-10-01T01:00:00.000Z');
    expect(initial.fetchedAt).toBe('2026-10-01T02:00:00.000Z');
    expect(initial.message).toContain('limiting requests');
    setTime(5_000);
    const manual = (await service.get({ refresh: true })).find(
      (account) => account.provider === 'muse'
    )!;
    expect(manual.windows).toEqual(initial.windows);
    expect(manual.fetchedAt).toBe(initial.fetchedAt);
    expect(manual.sampledAt).toBe(initial.sampledAt);
    const projected = additionalAccounts([manual]).find((account) => account.provider === 'muse')!;
    expect(projected.status).toBe('cached');
    expect(projected.sampledAt).toBe(initial.sampledAt);
    expect(projected.fetchedAt).toBe(manual.fetchedAt);
    expect(projected.windows).toEqual(manual.windows);
    expect(projected.message).toBe(manual.message);
    const dashboard = new AccountDashboardService({
      scope: () => 'muse-cached-fixture',
      // The fixture reading is current: its weekly reset is still ahead.
      now: () => Date.parse(instant),
      refreshIntervalSeconds: () => 60,
      getCodexSummary: async () => ({ active: null, activated: null, default: null, profiles: [] }),
      getCodexRows: async () => [],
      getCachedCodexRows: () => [],
      listClaudeProfiles: async () => [],
      getClaudeUsage: async (platform) => ({ platform, fetchedAt: instant, profiles: [] }),
      getAdditionalAccounts: async () => [manual],
      getOptionalWalletAccounts: async () => [],
      invalidateClaudeCache: () => {},
      getAutoSwitchStatus: () => ({
        enabled: false,
        thresholdPercent: 15,
        pollIntervalSeconds: 60,
        outcome: 'disabled',
        message: 'Disabled',
        activationInProgress: false,
      }),
    });
    const consolidated = (await dashboard.get('mac', true)).accounts.find(
      (account) => account.provider === 'muse'
    )!;
    expect(consolidated.status).toBe('cached');
    expect(consolidated.sampledAt).toBe(initial.sampledAt);
    expect(consolidated.fetchedAt).toBe(manual.fetchedAt);
    expect(consolidated.windows).toEqual(manual.windows);
    expect(consolidated.message).toBe(manual.message);
    museUnavailable = true;
    setTime(10_000);
    const unavailable = (await service.get({ refresh: true })).find(
      (account) => account.provider === 'muse'
    )!;
    expect(unavailable.status).toBe('cached');
    expect(unavailable.windows).toEqual(initial.windows);
    expect(unavailable.fetchedAt).toBe(initial.fetchedAt);
    expect(unavailable.sampledAt).toBe(initial.sampledAt);
    expect(unavailable.message).toBeNull();
    expect(museCalls).toBe(3);
    setTime(39_999);
    await service.get({ refresh: true });
    expect(museCalls).toBe(3);
    setTime(40_000);
    await service.get({ refresh: true });
    expect(museCalls).toBe(4);
  });

  it('does not replace newer Muse quota with an older cached helper sample, while fresh zero values replace normally', async () => {
    let phase = 0;
    const { service, setTime } = fixture({
      runSource: async (source) => {
        if (source.provider !== 'muse') return payload();
        if (phase === 1)
          return payload({
            status: 'cached',
            fetchedAt: '2026-10-01T01:00:00Z',
            sampledAt: '2026-10-01T01:00:00Z',
          });
        if (phase === 2)
          return payload({
            fetchedAt: '2026-10-01T03:00:00Z',
            sampledAt: '2026-10-01T03:00:00Z',
            windows: [{ key: 'rolling', label: 'Rolling', usedPercent: 0, resetAt: null }],
          });
        return payload();
      },
    });
    const original = (await service.get()).find((account) => account.provider === 'muse')!;
    phase = 1;
    setTime(5_000);
    const older = (await service.get({ refresh: true })).find(
      (account) => account.provider === 'muse'
    )!;
    expect(older.status).toBe('cached');
    expect(older.windows).toEqual(original.windows);
    expect(older.sampledAt).toBe(original.sampledAt);
    phase = 2;
    setTime(10_000);
    const zero = (await service.get({ refresh: true })).find(
      (account) => account.provider === 'muse'
    )!;
    expect(zero.status).toBe('ok');
    expect(zero.windows).toHaveLength(1);
    expect(zero.windows[0].usedPercent).toBe(0);
    expect(zero.windows[0].resetAt).toBeNull();
    expect(zero.sampledAt).toBe('2026-10-01T03:00:00.000Z');
  });

  it.each(['unavailable', 'error', 'needs_sign_in'])(
    'does not attach previous Muse quota to a different account when its new status is %s',
    async (status) => {
      let changed = false;
      const { service, setTime } = fixture({
        runSource: async (source) =>
          source.provider === 'muse' && changed
            ? payload({ status, email: 'different@example.com', windows: [] })
            : payload(),
      });
      await service.get();
      changed = true;
      setTime(5_000);
      const account = (await service.get({ refresh: true })).find(
        (row) => row.provider === 'muse'
      )!;
      expect(account.status).toBe(status);
      expect(account.email).toBe('different@example.com');
      expect(account.windows).toEqual([]);
    }
  );

  it.each(['not-an-email', 'Bearer PRIVATE_TOKEN', ''])(
    'rejects invalid Muse identity %s and clears previous quota before a later offline retry',
    async (invalidEmail) => {
      let phase = 0;
      const { service, setTime } = fixture({
        runSource: async (source) => {
          if (source.provider !== 'muse' || phase === 0) return payload();
          if (phase === 1) return payload({ status: 'cached', email: invalidEmail });
          return payload({ status: 'unavailable', email: null, windows: [] });
        },
      });
      await service.get();
      phase = 1;
      setTime(5_000);
      const rejected = (await service.get({ refresh: true })).find(
        (row) => row.provider === 'muse'
      )!;
      expect(rejected.status).toBe('error');
      expect(rejected.windows).toEqual([]);
      expect(rejected.email).toBeNull();
      expect(JSON.stringify(rejected)).not.toContain('PRIVATE_TOKEN');
      phase = 2;
      setTime(35_000);
      const offline = (await service.get({ refresh: true })).find(
        (row) => row.provider === 'muse'
      )!;
      expect(offline.status).toBe('unavailable');
      expect(offline.windows).toEqual([]);
    }
  );

  it('does not restore Muse quota after sign-in is required, even when a later transport failure hides identity', async () => {
    let phase = 0;
    const { service, setTime } = fixture({
      runSource: async (source) => {
        if (source.provider !== 'muse' || phase === 0) return payload();
        if (phase === 1) return payload({ status: 'needs_sign_in', windows: [] });
        throw new AdditionalUsageTransportError();
      },
    });
    await service.get();
    phase = 1;
    setTime(5_000);
    const signedOut = (await service.get({ refresh: true })).find(
      (row) => row.provider === 'muse'
    )!;
    expect(signedOut.status).toBe('needs_sign_in');
    expect(signedOut.windows).toEqual([]);
    phase = 2;
    setTime(35_000);
    const offline = (await service.get({ refresh: true })).find((row) => row.provider === 'muse')!;
    expect(offline.status).toBe('error');
    expect(offline.windows).toEqual([]);
  });

  it('does not forward arbitrary Muse cached error bodies or credential text as its explanation', async () => {
    const { service } = fixture({
      runSource: async () => payload({ status: 'cached', message: 'PRIVATE_PASSWORD=hidden' }),
    });
    const account = (await service.get()).find((row) => row.provider === 'muse')!;
    expect(account.status).toBe('cached');
    expect(account.message).toBe('Showing the last saved Muse usage sample.');
    expect(JSON.stringify(account)).not.toContain('PRIVATE_PASSWORD');
  });

  it('serves a message-less Muse cached reading without inventing an explanation', async () => {
    const { service } = fixture({
      runSource: async () => payload({ status: 'cached', message: null }),
    });
    const account = (await service.get()).find((row) => row.provider === 'muse')!;
    expect(account.status).toBe('cached');
    expect(account.message).toBeNull();
    expect(account.sampledAt).toBe('2026-10-01T02:00:00.000Z');
    expect(account.windows[0]?.usedPercent).toBe(23.5);
  });

  it('projects only safe fields and refuses all helper-selected capabilities and identifiers', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          id: 'PRIVATE_PATH',
          provider: 'claude',
          providerLabel: 'PRIVATE_LABEL',
          platform: 'windows',
          source: 'PRIVATE_COMMAND',
          label: 'PRIVATE_LABEL',
          message: 'PRIVATE_UPSTREAM_RESPONSE',
          tokens: { access_token: 'PRIVATE_AUTHENTICATION' },
          isActive: true,
          capabilities: {
            codexProfile: 'gmail',
            claudeProfileId: 'party',
            claudePlatforms: ['mac'],
          },
        }),
    });
    const rows = await service.get();
    expect(JSON.stringify(rows)).not.toContain('PRIVATE');
    expect(rows[0]?.provider).toBe('antigravity');
    expect(rows[0]?.id).toBe('antigravity:usage');
    expect(rows[0]?.platform).toBe('ubuntu');
    expect(rows[0]?.label).toBe('person@example.com');
  });

  it.each(['needs_sign_in', 'unavailable', 'error'])(
    'replaces %s helper messages with static descriptions and omits quota windows',
    async (status) => {
      const { service } = fixture({
        runSource: async () => payload({ status, message: 'PRIVATE_UPSTREAM_ERROR' }),
      });
      const rows = await service.get();
      expect(rows.every((row) => row.status === status)).toBe(true);
      expect(rows.every((row) => row.windows.length === 0)).toBe(true);
      expect(JSON.stringify(rows)).not.toContain('PRIVATE');
    }
  );

  it('treats unknown, empty, invalid and oversized helper responses as safe error rows', async () => {
    const responses = [
      'not json PRIVATE_TOKEN',
      'null',
      '[]',
      payload({ status: 'unknown' }),
      payload({ windows: [] }),
      payload({ windows: [{ usedPercent: -1, remainingPercent: 102 }] }),
      payload({ extra: 'x'.repeat(64 * 1024) }),
    ];
    let index = 0;
    const { service } = fixture({ runSource: async () => responses[index++]! });
    const rows = await service.get();
    expect(rows).toHaveLength(7);
    expect(rows.every((row) => row.status === 'error')).toBe(true);
    expect(rows.every((row) => row.windows.length === 0)).toBe(true);
    expect(JSON.stringify(rows)).not.toContain('PRIVATE');
  });

  it('validates numbers and authoritative reset times without replacing unknowns with zero', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          windows: [
            {
              key: 'bad key',
              label: 'Bearer PRIVATE_SECRET',
              usedPercent: 150,
              remainingPercent: -1,
              resetAt: '2026-10-08',
              windowMinutes: '300',
              used: -10,
              limit: 200,
              unit: { secret: 'PRIVATE' },
            },
            { key: 'five-hour', usedPercent: 0, resetAt: '2026-10-01T05:00:00-04:00' },
            { key: 'unknown', usedPercent: null, resetAt: instant },
          ],
        }),
    });
    const row = (await service.get())[0]!;
    expect(row.windows).toHaveLength(3);
    expect(row.windows[0]).toEqual({
      key: 'usage-1',
      label: 'Usage',
      usedPercent: 150,
      remainingPercent: 0,
      resetAt: null,
      windowMinutes: null,
      used: null,
      limit: 200,
      unit: null,
    });
    expect(row.windows[1]?.usedPercent).toBe(0);
    expect(row.windows[1]?.remainingPercent).toBe(100);
    expect(row.windows[1]?.resetAt).toBe('2026-10-01T09:00:00.000Z');
    expect(row.windows[2]?.usedPercent).toBeNull();
    expect(row.windows[2]?.resetAt).toBe('2026-10-01T02:00:00.000Z');
  });

  it('preserves reported balances, spend, extra usage, expiration and unlimited flags', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          windows: [
            {
              key: 'balance',
              kind: 'balance',
              remaining: -1.25,
              expiresAt: '2026-12-01T00:00:00Z',
              unit: 'USD',
            },
            { key: 'extra', kind: 'extra_usage', enabled: false },
            { key: 'spend', kind: 'spend', used: 1.5, limit: 10, unit: 'USD' },
            { key: 'unlimited', kind: 'rate_limit', unlimited: true },
          ],
        }),
    });
    const windows = (await service.get())[0]!.windows;
    expect(windows).toHaveLength(4);
    expect(windows[0]).toMatchObject({
      kind: 'balance',
      remaining: -1.25,
      expiresAt: '2026-12-01T00:00:00.000Z',
    });
    expect(windows[0]?.resetAt).toBeNull();
    expect(windows[0]?.usedPercent).toBeNull();
    expect(windows[1]).toMatchObject({ kind: 'extra_usage', enabled: false });
    expect(windows[2]).toMatchObject({ kind: 'spend', used: 1.5, limit: 10 });
    expect(windows[3]).toMatchObject({ kind: 'rate_limit', unlimited: true });
    expect(windows[3]?.limit).toBeNull();
  });

  it('rejects malformed optional fields without inferring expiration, balance or enabled state', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          windows: [
            {
              key: 'usage',
              used: 1,
              kind: 'PRIVATE_TOKEN',
              remaining: '300',
              expiresAt: 'next week',
              enabled: 'yes',
              unlimited: 1,
            },
          ],
        }),
    });
    const window = (await service.get())[0]!.windows[0]!;
    expect(window).toHaveProperty('remaining', null);
    expect(window).toHaveProperty('expiresAt', null);
    expect(window).not.toHaveProperty('kind');
    expect(window).not.toHaveProperty('enabled');
    expect(window).not.toHaveProperty('unlimited');
    expect(JSON.stringify(window)).not.toContain('PRIVATE');
  });

  it('retains more than 24 model windows and rejects token-looking plans and invalid account identities', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          email: 'Bearer not-an-email',
          plan: 'sk-privatecredential',
          windows: Array.from({ length: 100 }, (_, index) => ({
            key: `model-${index}`,
            usedPercent: 1,
          })),
        }),
    });
    const row = (await service.get())[0]!;
    expect(row.email).toBeNull();
    expect(row.plan).toBeNull();
    expect(row.windows).toHaveLength(100);
    expect(JSON.stringify(row)).not.toContain('privatecredential');
  });

  it('bounds unusually large model arrays to 256 windows', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({
          windows: Array.from({ length: 300 }, (_, index) => ({
            key: `model-${index}`,
            usedPercent: 1,
          })),
        }),
    });
    expect((await service.get())[0]?.windows).toHaveLength(256);
  });

  it('refuses token-looking window keys as well as labels and other display fields', async () => {
    const { service } = fixture({
      runSource: async () =>
        payload({ windows: [{ key: 'sk_privatecredential', usedPercent: 1 }] }),
    });
    const row = (await service.get())[0]!;
    expect(row.windows[0]?.key).toBe('usage-1');
    expect(JSON.stringify(row)).not.toContain('privatecredential');
  });

  it('does not allow a response consumer to mutate cached rows', async () => {
    const { service } = fixture();
    const first = await service.get();
    first[0]!.windows[0]!.usedPercent = 99;
    first[0]!.capabilities.codexProfile = 'gmail';
    const next = await service.get();
    expect(next[0]!.windows[0]!.usedPercent).toBe(23.5);
    expect(next[0]!.capabilities.codexProfile).toBeNull();
  });

  it('honors validated private sources while keeping provider order stable', async () => {
    const { service, calls, setManifest } = fixture();
    setManifest(
      configuration([
        { provider: 'cursor', platform: 'mac', sshHost: 'mac-usage' },
        { provider: 'kimi-code', platform: 'windows', sshHost: 'win-usage' },
      ])
    );
    const rows = await service.get();
    expect(rows.map((row) => row.provider)).toEqual([...ADDITIONAL_PROVIDERS]);
    expect(rows[2]?.platform).toBe('mac');
    expect(rows[2]?.source).toBe('Account on Mac');
    expect(rows[3]?.platform).toBe('windows');
    expect(calls[2]).toEqual({ provider: 'cursor', platform: 'mac', sshHost: 'mac-usage' });
  });

  it.each([
    '{bad PRIVATE_CONFIG',
    JSON.stringify({ version: 2, sources: [] }),
    JSON.stringify({ version: 1, sources: [], command: 'PRIVATE_COMMAND' }),
    configuration([{ provider: 'claude', platform: 'ubuntu' }]),
    configuration([{ provider: 'cursor', platform: 'linux' }]),
    configuration([{ provider: 'cursor', platform: 'mac', sshHost: '-oProxyCommand=PRIVATE' }]),
    configuration([{ provider: 'cursor', platform: 'mac', sshHost: 'mac;PRIVATE' }]),
    configuration([{ provider: 'cursor', platform: 'mac', helper: '/tmp/PRIVATE' }]),
    configuration([
      { provider: 'cursor', platform: 'mac' },
      { provider: 'cursor', platform: 'windows' },
    ]),
    'x'.repeat(32 * 1024 + 1),
  ])('rejects unsafe private configuration before any helper runs (%#)', async (contents) => {
    const { service, calls, setManifest } = fixture();
    setManifest(contents);
    const rows = await service.get();
    expect(calls).toHaveLength(0);
    expect(rows).toHaveLength(7);
    expect(rows.every((row) => row.status === 'error')).toBe(true);
    expect(JSON.stringify(rows)).not.toContain('PRIVATE');
  });

  it('invalidates samples immediately when the manifest fingerprint changes', async () => {
    const { service, calls, setManifest } = fixture();
    await service.get();
    setManifest(configuration([{ provider: 'cursor', platform: 'mac', sshHost: 'mac-usage' }]));
    const rows = await service.get();
    expect(calls).toHaveLength(14);
    expect(rows[2]?.platform).toBe('mac');
    expect(rows[2]?.status).toBe('ok');
  });

  it('does not let a late result from the previous source replace the changed source cache', async () => {
    let releaseOld!: () => void;
    const old = new Promise<void>((resolve) => {
      releaseOld = resolve;
    });
    const { service, setManifest } = fixture({
      runSource: async (source) => {
        if (source.platform === 'ubuntu') await old;
        return payload({
          email: source.platform === 'ubuntu' ? 'old@example.com' : 'new@example.com',
        });
      },
    });
    const previous = service.get();
    await new Promise((resolve) => setTimeout(resolve, 1));
    setManifest(
      configuration(
        ADDITIONAL_PROVIDERS.map((provider) => ({
          provider,
          platform: 'mac',
          sshHost: 'mac-usage',
        }))
      )
    );
    expect((await service.get())[0]?.email).toBe('new@example.com');
    releaseOld();
    await previous;
    expect((await service.get())[0]?.email).toBe('new@example.com');
  });

  it('sanitizes unreadable configuration and transport timeout errors', async () => {
    const invalid = fixture({
      readManifest: async () => {
        throw new Error('PRIVATE_PATH');
      },
    });
    expect(JSON.stringify(await invalid.service.get())).not.toContain('PRIVATE_PATH');
    expect(invalid.calls).toHaveLength(0);
    const timeout = fixture({
      runSource: async () => {
        throw new AdditionalUsageTransportError(true);
      },
    });
    expect((await timeout.service.get())[0]?.message).toBe('Account usage request timed out.');
  });

  it('reads only the scope-private manifest and isolates two CCS scopes', async () => {
    const firstDir = await fs.mkdtemp(path.join(os.tmpdir(), 'ccs-added-first-'));
    const secondDir = await fs.mkdtemp(path.join(os.tmpdir(), 'ccs-added-second-'));
    temporaryDirectories.push(firstDir, secondDir);
    await fs.writeFile(
      path.join(firstDir, 'account-usage-sources.json'),
      configuration([{ provider: 'cursor', platform: 'mac', sshHost: 'first-mac' }])
    );
    await fs.writeFile(
      path.join(secondDir, 'account-usage-sources.json'),
      configuration([{ provider: 'cursor', platform: 'windows', sshHost: 'second-win' }])
    );
    const runSource = async () => payload();
    const first = new AdditionalAccountService({ ccsDir: firstDir, runSource });
    const second = new AdditionalAccountService({ ccsDir: secondDir, runSource });
    expect((await first.get())[2]?.platform).toBe('mac');
    expect((await second.get())[2]?.platform).toBe('windows');
  });
});
