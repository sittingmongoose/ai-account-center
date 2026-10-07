import { describe, expect, it } from 'bun:test';
import type {
  AntigravityUsageProfile,
  AntigravityUsageSample,
} from '../../../src/antigravity/usage-contract';
import {
  AntigravityUsageService,
  mergeAntigravityAccounts,
} from '../../../src/antigravity/usage-service';
import { poolWindow } from '../../../src/antigravity/usage-normalization';

const SAMPLE_TIME = '2026-10-01T17:00:00.000Z';

function profile(id: string, selected = false): AntigravityUsageProfile {
  return {
    id,
    email: `${id}@example.com`,
    plan: 'Google AI Pro',
    identityKey: `identity-${id}`,
    credentialRevision: `revision-${id}`,
    identityVerified: true,
    available: true,
    selected,
    runtimeVerified: false,
    verifiedAt: SAMPLE_TIME,
  };
}

function sample(row: AntigravityUsageProfile, usedPercent = 10): AntigravityUsageSample {
  return {
    profileId: row.id,
    identityKey: row.identityKey,
    credentialRevision: row.credentialRevision,
    status: 'ok',
    email: row.email,
    plan: row.plan,
    fetchedAt: SAMPLE_TIME,
    sampledAt: SAMPLE_TIME,
    windows: [
      {
        key: 'claude-pool',
        poolId: 'claude-pool',
        modelIds: ['claude-opus-4-6', 'claude-sonnet-4-6'],
        label: 'Claude models',
        usedPercent,
        remainingPercent: 100 - usedPercent,
        resetAt: '2026-10-02T17:00:00.000Z',
        windowMinutes: null,
        used: null,
        limit: null,
        unit: null,
        kind: 'rate_limit',
      },
    ],
  };
}

function fixture(initial = [profile('gmail', true), profile('party')]) {
  let profiles = initial;
  let now = Date.parse(SAMPLE_TIME);
  const calls: string[] = [];
  let handler: (row: AntigravityUsageProfile) => Promise<AntigravityUsageSample> = async (row) =>
    sample(row);
  const service = new AntigravityUsageService({
    readProfiles: async () => profiles,
    collectQuota: async (id) => {
      calls.push(id);
      return handler(profiles.find((row) => row.id === id) as AntigravityUsageProfile);
    },
    now: () => now,
  });
  return {
    service,
    calls,
    profiles: () => profiles,
    setProfiles: (value: AntigravityUsageProfile[]) => {
      profiles = value;
    },
    advance: (milliseconds: number) => {
      now += milliseconds;
    },
    handle: (value: typeof handler) => {
      handler = value;
    },
  };
}

describe('Antigravity account-specific usage cache', () => {
  it('normalizes old cached plan strings at their observation time and clones optional metadata', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({ ...sample(row), plan: 'free-tier', status: 'cached' }));
    const result = (await f.service.getAccounts())[0];
    expect(result.plan).toBe('Free');
    expect(result.antigravityPlan).toMatchObject({
      class: 'free',
      quotaPolicy: 'weekly',
      thirdPartyModels: true,
      creditsOverage: false,
    });
    result.antigravityPlan?.models?.push('mutated');
    expect((await f.service.getAccounts())[0].antigravityPlan?.models).not.toContain('mutated');
  });

  it('allows an explicitly known no-quota plan with no windows and keeps empty unknown samples unavailable', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({
      ...sample(row),
      plan: 'Google Workspace Business Plus',
      windows: [],
    }));
    expect((await f.service.getAccounts())[0]).toMatchObject({
      status: 'ok',
      plan: 'Google Workspace',
      antigravityPlan: { quotaPolicy: 'none' },
      windows: [],
    });
    f.service.invalidate();
    f.handle(async (row) => ({ ...sample(row), plan: 'Unknown plan', windows: [] }));
    const unknown = (await f.service.getAccounts())[0];
    expect(unknown.status).toBe('error');
    expect(unknown.antigravityPlan).toBeUndefined();
  });

  it('uses retained loadCodeAssist tier metadata without leaking it or inferring from an email domain', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({
      ...sample(row),
      plan: 'Standard',
      reportedPlan: { paidTier: { id: 'standard-tier', name: 'Google AI Ultra 5x' } },
    }));
    const result = (await f.service.getAccounts())[0];
    expect(result.plan).toBe('Google AI Ultra 5x');
    expect(result.antigravityPlan?.summary).toContain('5x Google AI Pro capacity');
    expect(result).not.toHaveProperty('reportedPlan');
    f.service.invalidate();
    f.handle(async (row) => ({ ...sample(row), email: row.email, plan: 'Future tier' }));
    expect((await f.service.getAccounts())[0].antigravityPlan).toBeUndefined();
  });

  it('maps the native collector fresh status to an available display reading', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({ ...sample(row), status: 'fresh' }));
    expect((await f.service.getAccounts())[0].status).toBe('ok');
  });

  it('preserves quota-group IDs derived from exact bucket membership with explicit provenance', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({
      ...sample(row),
      windows: [
        {
          ...(sample(row).windows[0] as object),
          poolId: 'pool_000000000000',
          poolIdSource: 'provider-bucket-membership',
          poolLabel: 'Gemini Models',
        },
      ],
    }));
    expect((await f.service.getAccounts())[0].windows[0]).toMatchObject({
      poolId: 'pool_000000000000',
      poolIdSource: 'provider-bucket-membership',
      poolLabel: 'Gemini Models',
    });
  });

  it('keeps two independently verified accounts and the selected Ubuntu profile', async () => {
    const f = fixture();
    const accounts = await f.service.getAccounts();
    expect(accounts.map((row) => row.id)).toEqual([
      'antigravity:profile:gmail',
      'antigravity:profile:party',
    ]);
    expect(accounts.map((row) => row.isActive)).toEqual([true, false]);
    expect(accounts.every((row) => row.platform === 'ubuntu')).toBe(true);
    expect(accounts[1].capabilities.antigravityHostIds).toEqual(['ubuntu']);
    expect(f.calls).toEqual(['gmail', 'party']);
  });

  it('preserves actual pool/model scope and reset without inventing interval or token allowance', async () => {
    const f = fixture([profile('gmail')]);
    const row = (await f.service.getAccounts())[0];
    expect(row.windows[0]).toMatchObject({
      key: 'claude-pool',
      poolId: 'claude-pool',
      modelIds: ['claude-opus-4-6', 'claude-sonnet-4-6'],
      windowMinutes: null,
      used: null,
      limit: null,
      resetAt: '2026-10-02T17:00:00.000Z',
      usedPercent: 10,
      remainingPercent: 90,
    });
  });

  it('projects private registry and upstream extras into an allowlisted DTO', async () => {
    const row = {
      ...profile('gmail'),
      credential: 'private-credential-sentinel',
      subject: 'private-subject-sentinel',
    };
    const f = fixture([row]);
    f.handle(async (current) => ({
      ...sample(current),
      access_token: 'private-token-sentinel',
      windows: [
        {
          ...(sample(current).windows[0] as object),
          secret: 'private-window-sentinel',
          raw: 'raw-sentinel',
        },
      ],
    }));
    const output = JSON.stringify([await f.service.getInventory(), await f.service.getAccounts()]);
    for (const value of ['sentinel', 'identityKey', 'credentialRevision', 'access_token', 'secret'])
      expect(output).not.toContain(value);
  });

  it('does not merge accounts with the same email but different verified subjects', async () => {
    const a = profile('first');
    const b = { ...profile('second'), email: a.email };
    const f = fixture([a, b]);
    expect((await f.service.getAccounts()).length).toBe(2);
  });

  it('rejects ambiguous duplicated registry identities and selected accounts', async () => {
    const a = profile('first', true);
    await expect(
      fixture([a, { ...profile('second'), identityKey: a.identityKey }]).service.getAccounts()
    ).rejects.toThrow('Ambiguous');
    await expect(fixture([a, profile('second', true)]).service.getAccounts()).rejects.toThrow(
      'Ambiguous'
    );
  });

  it('coalesces overlapping quota reads without serial account activation', async () => {
    const f = fixture([profile('gmail')]);
    let release: () => void = () => {};
    const waiting = new Promise<void>((resolve) => {
      release = resolve;
    });
    f.handle(async (row) => {
      await waiting;
      return sample(row);
    });
    const first = f.service.getAccounts();
    const second = f.service.getAccounts({ refresh: true });
    await Promise.resolve();
    await Promise.resolve();
    expect(f.calls).toEqual(['gmail']);
    release();
    expect((await first)[0].windows).toEqual((await second)[0].windows);
  });

  it('retains the original successful sample time through failures and retries after backoff', async () => {
    const f = fixture([profile('gmail')]);
    await f.service.getAccounts();
    f.advance(6_000);
    f.handle(async () => {
      throw new Error('raw provider secret sentinel');
    });
    const failed = (await f.service.getAccounts({ refresh: true }))[0];
    expect(failed.status).toBe('cached');
    expect(failed.sampledAt).toBe(SAMPLE_TIME);
    expect(failed.fetchedAt).toBe(SAMPLE_TIME);
    expect(failed.message).not.toContain('sentinel');
    f.advance(6_000);
    await f.service.getAccounts({ refresh: true });
    expect(f.calls.length).toBe(2);
    f.advance(24_000);
    await f.service.getAccounts();
    expect(f.calls.length).toBe(3);
  });

  it('honors provider cooldown even for valid cached quota and explicit refresh', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({
      ...sample(row),
      status: 'cached',
      retryAfterSeconds: 600,
    }));
    await f.service.getAccounts();
    f.advance(120_000);
    await f.service.getAccounts({ refresh: true });
    expect(f.calls.length).toBe(1);
    f.advance(480_000);
    await f.service.getAccounts({ refresh: true });
    expect(f.calls.length).toBe(2);
  });

  it('clears old quota after a sign-in failure rather than retaining an invalid account sample', async () => {
    const f = fixture([profile('gmail')]);
    await f.service.getAccounts();
    f.advance(6_000);
    f.handle(async (row) => ({
      ...sample(row),
      status: 'needs_sign_in',
      email: null,
      windows: [],
    }));
    const result = (await f.service.getAccounts({ refresh: true }))[0];
    expect(result.status).toBe('needs_sign_in');
    expect(result.windows).toEqual([]);
    f.advance(30_000);
    f.handle(async () => {
      throw new Error('offline');
    });
    expect((await f.service.getAccounts())[0].windows).toEqual([]);
  });

  it('does not inherit a previous account sample when identity binding fails', async () => {
    const f = fixture([profile('gmail')]);
    await f.service.getAccounts();
    f.advance(6_000);
    f.handle(async (row) => ({
      ...sample(row),
      email: 'other@example.com',
      identityKey: 'other-identity',
    }));
    const result = (await f.service.getAccounts({ refresh: true }))[0];
    expect(result.status).toBe('error');
    expect(result.statusReason).toBe('identity_unbound');
    expect(result.windows).toEqual([]);
  });

  it('names identity_unbound for each binding failure and for no other failed reading', async () => {
    const unbound: Array<(row: AntigravityUsageProfile) => AntigravityUsageSample> = [
      (row) => ({ ...sample(row), identityKey: 'other-identity' }),
      (row) => ({ ...sample(row), credentialRevision: 'other-revision' }),
      (row) => ({ ...sample(row), profileId: 'someone-else' }),
      (row) => ({ ...sample(row), email: 'other@example.com' }),
      (row) => ({ ...sample(row), email: null }),
    ];
    for (const answer of unbound) {
      const f = fixture([profile('gmail')]);
      f.handle(async (row) => answer(row));
      const result = (await f.service.getAccounts())[0];
      expect(result).toMatchObject({ status: 'error', statusReason: 'identity_unbound' });
      expect(result.message).toBe('Antigravity usage could not be matched to this saved account.');
    }
    const other: Array<(row: AntigravityUsageProfile) => Promise<AntigravityUsageSample>> = [
      async () => {
        throw new Error('offline');
      },
      async (row) => ({ ...sample(row), status: 'error' }),
      async (row) => ({ ...sample(row), status: 'unavailable', email: null }),
      async (row) => ({ ...sample(row), status: 'needs_sign_in' }),
      async (row) => ({ ...sample(row), windows: [] }),
    ];
    for (const answer of other) {
      const f = fixture([profile('gmail')]);
      f.handle(answer);
      const result = (await f.service.getAccounts())[0];
      expect(result.status).not.toBe('ok');
      expect('statusReason' in result).toBe(false);
    }
  });

  it('recollects on a new login but holds the same account last sample as cached', async () => {
    const f = fixture([profile('gmail')]);
    await f.service.getAccounts();
    f.setProfiles([{ ...profile('gmail'), credentialRevision: 'new-revision' }]);
    f.handle(async () => {
      throw new Error('offline');
    });
    // FW2-B: a terminal sign-in recollects (a fresh call), but the row keeps
    // the same account's last sample, labelled cached, instead of going dark.
    const row = (await f.service.getAccounts())[0];
    expect(f.calls.length).toBe(2);
    expect(row.status).toBe('cached');
    expect(row.windows.length).toBe(1);
    expect(f.service.cachedAccounts(f.profiles())[0].status).toBe('cached');
  });

  it('drops the last sample when the saved identity itself changes', async () => {
    const f = fixture([profile('gmail')]);
    await f.service.getAccounts();
    f.setProfiles([
      {
        ...profile('gmail'),
        identityKey: 'identity-someone-else',
        credentialRevision: 'new-revision',
      },
    ]);
    f.handle(async () => {
      throw new Error('offline');
    });
    // One account's quota is never shown for another identity: the failure
    // surfaces honestly instead of borrowing the old sample.
    const row = (await f.service.getAccounts())[0];
    expect(f.calls.length).toBe(2);
    expect(row.status).toBe('error');
    expect(row.windows).toEqual([]);
  });

  it('does not publish an old in-flight credential sample after a concurrent import', async () => {
    const f = fixture([profile('gmail')]);
    let release: () => void = () => {};
    const waiting = new Promise<void>((resolve) => {
      release = resolve;
    });
    f.handle(async (row) => {
      const result = sample(row);
      await waiting;
      return result;
    });
    const pending = f.service.getAccounts();
    await Promise.resolve();
    await Promise.resolve();
    f.setProfiles([{ ...profile('gmail'), credentialRevision: 'replaced-revision' }]);
    release();
    expect((await pending)[0].windows).toEqual([]);
  });

  it('updates selected state independently of cached quota', async () => {
    const f = fixture();
    await f.service.getAccounts();
    f.setProfiles([profile('gmail'), profile('party', true)]);
    expect((await f.service.getAccounts()).map((row) => row.isActive)).toEqual([false, true]);
    expect(f.calls.length).toBe(2);
  });

  it('ignores unavailable/unverified profiles without executing quota transport', async () => {
    const row = {
      ...profile('party'),
      identityVerified: false,
      available: false,
    };
    const f = fixture([row]);
    expect((await f.service.getAccounts())[0].status).toBe('needs_sign_in');
    expect(f.calls).toEqual([]);
  });

  it('does not overwrite a newer successful reading with an older cached sample', async () => {
    const f = fixture([profile('gmail')]);
    await f.service.getAccounts();
    f.advance(6_000);
    f.handle(async (row) => ({
      ...sample(row, 0),
      status: 'cached',
      sampledAt: '2026-10-01T16:00:00.000Z',
    }));
    const result = (await f.service.getAccounts({ refresh: true }))[0];
    expect(result.sampledAt).toBe(SAMPLE_TIME);
    expect(result.windows[0].usedPercent).toBe(10);
  });

  it('retains explicit zero usage and zero credits without inferring a missing allowance', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({
      ...sample(row, 0),
      windows: [
        ...sample(row, 0).windows,
        {
          key: 'google-ai-credits',
          label: 'Google AI credits',
          kind: 'balance',
          remaining: 0,
          unit: 'credits',
        },
      ],
    }));
    const windows = (await f.service.getAccounts())[0].windows;
    expect(windows[0].usedPercent).toBe(0);
    expect(windows[1]).toMatchObject({
      label: 'AI credits (overage)',
      remaining: 0,
      used: null,
      limit: null,
      resetAt: null,
    });
  });

  it('rejects future sample times and malicious public metadata', async () => {
    const f = fixture([profile('gmail')]);
    f.handle(async (row) => ({
      ...sample(row),
      sampledAt: '2099-01-01T00:00:00Z',
    }));
    expect((await f.service.getAccounts())[0].windows).toEqual([]);
    expect(poolWindow({ key: '../private', usedPercent: 20 })).toBeNull();
    expect(
      poolWindow({
        key: 'pool',
        label: 'Bearer credential-sentinel',
        usedPercent: 20,
      })?.label
    ).toBe('Model quota');
  });

  it('replaces only legacy Antigravity rows and keeps all other accounts and wallets distinct', async () => {
    const existing = [
      { id: 'antigravity:usage', provider: 'antigravity' },
      { id: 'muse:usage', provider: 'muse' },
      { id: 'go:api', provider: 'opencode-go' },
      { id: 'go:wallet', provider: 'opencode-go' },
    ];
    const accounts = await fixture().service.getAccounts();
    const merged = mergeAntigravityAccounts(existing, accounts);
    expect(merged.map((row) => row.id)).toEqual([
      'muse:usage',
      'go:api',
      'go:wallet',
      'antigravity:profile:gmail',
      'antigravity:profile:party',
    ]);
    expect(existing.length).toBe(4);
    expect(mergeAntigravityAccounts(existing, [])).toEqual(existing);
  });
});
