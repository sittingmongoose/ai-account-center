import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AdditionalAccountService,
  type AdditionalAccountDeps,
} from '../../../src/web-server/services/additional-account-service';
import {
  ACCOUNT_REGISTRY_FILE,
  parseAccountRegistry,
  type AccountRegistryRead,
} from '../../../src/web-server/services/account-registry-v2';
import {
  ADDITIONAL_PROVIDERS,
  AdditionalUsageTransportError,
  type AdditionalUsageSource,
} from '../../../src/web-server/services/additional-usage-transport';
import { additionalAccounts } from '../../../src/web-server/services/account-dashboard-projection';

const instant = '2026-10-02T07:00:00Z';
const temporaryDirs: string[] = [];
afterEach(() => {
  for (const dir of temporaryDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

function payload(extra: Record<string, unknown> = {}): string {
  return JSON.stringify({
    status: 'ok',
    email: null,
    plan: 'Pro',
    fetchedAt: instant,
    sampledAt: instant,
    windows: [{ key: 'weekly', label: 'Weekly', usedPercent: 12, windowMinutes: 10080 }],
    ...extra,
  });
}

function entry(
  id: string,
  credential: Record<string, unknown>,
  extra: Record<string, unknown> = {}
) {
  const provider = id.split(':')[0];
  return { id, provider, platform: 'ubuntu', sshHost: null, label: null, credential, ...extra };
}

function registry(accounts: unknown[]): AccountRegistryRead {
  const parsed = parseAccountRegistry({ version: 2, accounts });
  if (!parsed) throw new TypeError('fixture registry is invalid');
  return { state: 'ok', registry: parsed };
}

const TWO_ZAI = [
  entry('zai:usage', { kind: 'discover' }),
  entry('zai:acct:9f2c41d0', { kind: 'aac-key', keyId: '9f2c41d0' }, { label: 'Work' }),
  entry('zai:acct:0a1b2c3d', { kind: 'aac-key', keyId: '0a1b2c3d' }, { label: 'Personal' }),
  entry('cursor:usage', { kind: 'discover' }, { platform: 'mac', sshHost: 'mac-usage' }),
];

function fixture(read: () => AccountRegistryRead, overrides: AdditionalAccountDeps = {}) {
  let now = 0;
  const calls: AdditionalUsageSource[] = [];
  let manifestReads = 0;
  const service = new AdditionalAccountService({
    ccsDir: '/tmp/aac-registry-v2-service-fixture',
    now: () => now,
    readRegistry: async () => read(),
    readManifest: async () => {
      manifestReads += 1;
      return JSON.stringify({
        version: 1,
        sources: [{ provider: 'zai', platform: 'mac', sshHost: 'PRIVATE_V1' }],
      });
    },
    runSource: async (source) => {
      calls.push(source);
      return payload();
    },
    ...overrides,
  });
  return {
    service,
    calls,
    manifestReads: () => manifestReads,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

describe('additional accounts from registry v2', () => {
  it('reads the version 1 manifest exactly as before when registry v2 is absent', async () => {
    const { service, calls, manifestReads } = fixture(() => ({ state: 'absent' }));
    const snapshot = await service.snapshot();
    expect(snapshot.registry).toBe('v1');
    expect(manifestReads()).toBe(1);
    expect(calls).toHaveLength(7);
    expect(calls.every((source) => source.account === undefined)).toBe(true);
    expect(calls.find((source) => source.provider === 'zai')).toEqual({
      provider: 'zai',
      platform: 'mac',
      sshHost: 'PRIVATE_V1',
    });
    expect(snapshot.accounts.map((row) => row.id)).toEqual(
      ADDITIONAL_PROVIDERS.map((provider) => `${provider}:usage`)
    );
  });

  it('makes a valid registry the only source: N accounts per provider, ids and labels from it', async () => {
    const { service, calls, manifestReads } = fixture(() => registry(TWO_ZAI));
    const snapshot = await service.snapshot();
    expect(snapshot.registry).toBe('v2');
    expect(manifestReads()).toBe(0);
    expect(snapshot.accounts.map((row) => [row.id, row.provider, row.label, row.platform])).toEqual(
      [
        ['zai:usage', 'zai', 'Z.ai coding plan', 'ubuntu'],
        ['zai:acct:9f2c41d0', 'zai', 'Work', 'ubuntu'],
        ['zai:acct:0a1b2c3d', 'zai', 'Personal', 'ubuntu'],
        ['cursor:usage', 'cursor', 'Cursor', 'mac'],
      ]
    );
    expect(calls.map((source) => source.account?.id)).toEqual([
      'zai:usage',
      'zai:acct:9f2c41d0',
      'zai:acct:0a1b2c3d',
      'cursor:usage',
    ]);
    expect(calls[3]).toMatchObject({ provider: 'cursor', platform: 'mac', sshHost: 'mac-usage' });
    expect(JSON.stringify(snapshot)).not.toContain('PRIVATE_V1');
    // The dashboard keeps all three Z.ai rows and adds no placeholders for unlisted providers.
    expect(additionalAccounts(snapshot.accounts, snapshot.registry).map((row) => row.id)).toEqual([
      'cursor:usage',
      'zai:usage',
      'zai:acct:9f2c41d0',
      'zai:acct:0a1b2c3d',
    ]);
  });

  it('gives two key accounts of one provider separate caches and backoffs', async () => {
    const failing = new Set(['zai:acct:9f2c41d0']);
    const { service, calls, advance } = fixture(() => registry(TWO_ZAI), {
      runSource: async (source) => {
        calls.push(source);
        if (failing.has(source.account?.id ?? '')) throw new AdditionalUsageTransportError();
        return payload({
          windows: [{ key: 'weekly', label: 'Weekly', usedPercent: calls.length }],
        });
      },
    });
    const first = await service.get();
    expect(first.find((row) => row.id === 'zai:acct:9f2c41d0')?.status).toBe('error');
    expect(first.find((row) => row.id === 'zai:acct:0a1b2c3d')?.status).toBe('ok');
    const before = calls.length;
    advance(10_000);
    // A forced refresh re-reads the healthy account; the failed one stays in its 30 s backoff.
    await service.get({ refresh: true });
    const refreshed = calls.slice(before).map((source) => source.account?.id);
    expect(refreshed).toContain('zai:acct:0a1b2c3d');
    expect(refreshed).not.toContain('zai:acct:9f2c41d0');
    advance(30_000);
    failing.clear();
    const recovered = await service.get({ refresh: true });
    expect(recovered.find((row) => row.id === 'zai:acct:9f2c41d0')?.status).toBe('ok');
    // The healthy account's reading was never shared with the failed one.
    expect(first.find((row) => row.id === 'zai:acct:9f2c41d0')?.windows).toEqual([]);
  });

  it('keeps unchanged accounts cached when an account is added, and drops removed ones', async () => {
    let accounts: unknown[] = TWO_ZAI.slice(0, 2);
    const { service, calls } = fixture(() => registry(accounts));
    await service.get();
    expect(calls).toHaveLength(2);
    accounts = [
      ...TWO_ZAI.slice(0, 2),
      entry('opencode-go:acct:11111111', { kind: 'aac-key', keyId: '11111111' }),
    ];
    const rows = await service.get();
    expect(calls.map((source) => source.account?.id)).toEqual([
      'zai:usage',
      'zai:acct:9f2c41d0',
      'opencode-go:acct:11111111',
    ]);
    expect(rows).toHaveLength(3);
    // Re-pointing an account at another host is a new source with its own cache.
    accounts = [
      entry('zai:usage', { kind: 'discover' }, { platform: 'mac', sshHost: 'mac-usage' }),
    ];
    await service.get();
    expect(calls.at(-1)).toMatchObject({ provider: 'zai', platform: 'mac', sshHost: 'mac-usage' });
    expect(calls).toHaveLength(4);
  });

  it('shows every provider unavailable for an invalid registry, never the version 1 defaults', async () => {
    const { service, calls, manifestReads } = fixture(() => ({ state: 'invalid' }));
    const snapshot = await service.snapshot();
    expect(snapshot.registry).toBe('v2-invalid');
    expect(calls).toHaveLength(0);
    expect(manifestReads()).toBe(0);
    expect(snapshot.accounts.map((row) => [row.id, row.status, row.message])).toEqual(
      ADDITIONAL_PROVIDERS.map((provider) => [
        `${provider}:usage`,
        'unavailable',
        'Account list could not be read safely.',
      ])
    );
  });

  it('reads the real registry file and refuses an unsafe one', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-registry-v2-disk-'));
    temporaryDirs.push(dir);
    const calls: AdditionalUsageSource[] = [];
    const service = new AdditionalAccountService({
      ccsDir: dir,
      runSource: async (source) => {
        calls.push(source);
        return payload();
      },
    });
    const file = path.join(dir, ACCOUNT_REGISTRY_FILE);
    fs.writeFileSync(file, JSON.stringify({ version: 2, accounts: TWO_ZAI }), { mode: 0o600 });
    expect((await service.get()).map((row) => row.id)).toEqual(
      TWO_ZAI.map((account) => account.id)
    );
    fs.chmodSync(file, 0o644);
    const unsafe = await service.snapshot();
    expect(unsafe.registry).toBe('v2-invalid');
    expect(unsafe.accounts.every((row) => row.status === 'unavailable')).toBe(true);
  });

  it('reports an older helper as unavailable with the host to update', async () => {
    const { service } = fixture(() => registry(TWO_ZAI), {
      runSource: async (source) => {
        if (source.account?.credential.kind === 'aac-key')
          throw new AdditionalUsageTransportError(false, true);
        return payload();
      },
    });
    const rows = await service.get();
    expect(rows.find((row) => row.id === 'zai:acct:9f2c41d0')).toMatchObject({
      status: 'unavailable',
      message: 'Update the usage helper on Ubuntu.',
      windows: [],
    });
    expect(rows.find((row) => row.id === 'zai:usage')?.status).toBe('ok');
  });

  it('does not run a helper for kinds no helper reads yet', async () => {
    const { service, calls } = fixture(() =>
      registry([
        entry('muse:acct:22222222', { kind: 'config-home', homeId: '22222222' }),
        entry('antigravity:acct:33333333', { kind: 'antigravity-profile', profileId: 'party' }),
        entry(
          'kimi-code:acct:44444444',
          { kind: 'aac-key', keyId: '44444444' },
          { platform: 'mac', sshHost: 'mac-usage' }
        ),
      ])
    );
    const rows = await service.get();
    expect(calls.map((source) => source.account?.id)).toEqual(['kimi-code:acct:44444444']);
    expect(rows.slice(0, 2).map((row) => [row.status, row.message])).toEqual([
      ['unavailable', 'This account type is not read by the usage helper yet.'],
      ['unavailable', 'This account type is not read by the usage helper yet.'],
    ]);
    expect((await service.get({ excludeAntigravity: true })).map((row) => row.id)).toEqual([
      'muse:acct:22222222',
      'kimi-code:acct:44444444',
    ]);
  });

  it('offers placeholder rows for the configured accounts once the list is loaded', async () => {
    const { service } = fixture(() => registry(TWO_ZAI));
    expect(service.configured()).toBeNull();
    await service.get();
    const configured = service.configured();
    expect(configured?.registry).toBe('v2');
    expect(configured?.accounts.map((row) => [row.id, row.status, row.windows.length])).toEqual(
      TWO_ZAI.map((account) => [account.id, 'unavailable', 0])
    );
  });
});

describe('additionalAccounts projection without the one-row limit', () => {
  function row(id: string, provider = id.split(':')[0]) {
    return {
      id,
      provider,
      providerLabel: 'ignored',
      label: id,
      email: null,
      plan: null,
      platform: 'ubuntu',
      source: 'Account on Ubuntu',
      status: 'ok',
      message: null,
      fetchedAt: instant,
      sampledAt: instant,
      isActive: true,
      windows: [],
      capabilities: { codexProfile: 'x', claudeProfileId: 'y', claudePlatforms: ['mac'] },
    } as unknown as Parameters<typeof additionalAccounts>[0][number];
  }

  it('keeps several rows per provider in provider order, drops duplicates and foreign ids', () => {
    const projected = additionalAccounts(
      [
        row('zai:acct:00000002'),
        row('cursor:usage'),
        row('zai:acct:00000001'),
        row('zai:acct:00000002'),
        row('codex:party', 'zai'),
      ],
      'v2'
    );
    expect(projected.map((account) => account.id)).toEqual([
      'cursor:usage',
      'zai:acct:00000002',
      'zai:acct:00000001',
      'zai:usage',
    ]);
    expect(
      projected.every((account) => !account.isActive && account.capabilities.codexProfile === null)
    ).toBe(true);
  });

  it('caps a provider at 16 rows and fills empty providers only for version 1', () => {
    const many = Array.from({ length: 20 }, (_, index) =>
      row(`zai:acct:${index.toString(16).padStart(8, '0')}`)
    );
    expect(additionalAccounts(many, 'v2')).toHaveLength(16);
    const v1 = additionalAccounts([row('zai:usage')]);
    expect(v1.map((account) => account.provider)).toEqual([...ADDITIONAL_PROVIDERS]);
    expect(additionalAccounts([], 'v2')).toEqual([]);
  });
});
