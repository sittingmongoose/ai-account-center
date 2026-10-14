import { describe, expect, it } from 'bun:test';
import path from 'path';
import {
  AdditionalAccountService,
  type AdditionalAccountDeps,
} from '../../../src/web-server/services/additional-account-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import {
  ADDITIONAL_PROVIDERS,
  type CollectorCredential,
} from '../../../src/web-server/services/additional-usage-transport';
import type {
  AccountRegistryRead,
  RegistryAccount,
} from '../../../src/web-server/services/account-registry-v2';
import type { UsageFailoverTarget } from '../../../src/web-server/services/usage-failover-contract';

const observed = '2026-10-14T00:00:00.000Z';

function reading(template: DashboardAccount): DashboardAccount {
  return {
    ...template,
    email: 'same@example.test',
    plan: 'Pro',
    platform: 'windows',
    source: 'Account on Windows',
    status: 'cached',
    message: null,
    fetchedAt: observed,
    sampledAt: observed,
    windows: [
      {
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 0,
        remainingPercent: 100,
        used: 0,
        limit: 100,
        resetAt: null,
        windowMinutes: 10080,
        unit: 'credits',
      },
    ],
  };
}

function registryAccount(
  provider: RegistryAccount['provider'],
  credential: CollectorCredential,
  suffix = '1234abcd'
): RegistryAccount {
  return {
    id: credential.kind === 'discover' ? `${provider}:usage` : `${provider}:acct:${suffix}`,
    provider,
    credential,
    platform: 'mac',
    sshHost: 'fixture-mac',
    label: 'Saved account',
    createdAt: null,
    createdBy: credential.kind === 'discover' ? 'migration' : 'dashboard',
  };
}

function fixture(overrides: AdditionalAccountDeps = {}) {
  const calls: UsageFailoverTarget[] = [];
  const peeks: UsageFailoverTarget[] = [];
  let now = 0;
  const service = new AdditionalAccountService({
    ccsDir: '/tmp/additional-failover-fixture',
    readManifest: async () =>
      JSON.stringify({
        version: 1,
        sources: [{ provider: 'muse', platform: 'mac', sshHost: 'fixture-mac' }],
      }),
    now: () => now,
    failover: {
      collect: async (target, template) => {
        calls.push(target);
        return reading(template);
      },
      peek: (target, template) => {
        peeks.push(target);
        return reading(template);
      },
    },
    ...overrides,
  });
  return { service, calls, peeks, setTime: (value: number) => (now = value) };
}

describe('additional accounts use verified failover sources', () => {
  it('routes every additional provider through the coordinator and preserves the serving host', async () => {
    const { service, calls } = fixture();
    const rows = await service.get();
    expect(calls.map((call) => call.provider).sort()).toEqual([...ADDITIONAL_PROVIDERS].sort());
    const muse = rows.find((row) => row.provider === 'muse')!;
    expect(muse).toMatchObject({
      id: 'muse:usage',
      platform: 'windows',
      source: 'Account on Windows',
      status: 'cached',
      sampledAt: observed,
    });
    expect(muse.windows[0].usedPercent).toBe(0);
    expect(calls.find((call) => call.provider === 'muse')).toMatchObject({
      accountId: 'muse:usage',
      ccsDir: path.resolve('/tmp/additional-failover-fixture'),
      primarySources: [
        { host: 'mac', platform: 'mac', sshHost: 'fixture-mac', selector: 'discover' },
      ],
    });
  });

  it('selects each registry credential by its own locator instead of the first default key', async () => {
    const accounts = [
      registryAccount('zai', { kind: 'aac-key', keyId: 'aabbccdd' }),
      registryAccount('opencode-go', { kind: 'aac-key', keyId: '11223344' }),
      registryAccount('qwen', { kind: 'browser-capsule', capsuleId: 'a1b2c3d4' }),
      registryAccount('muse', { kind: 'config-home', homeId: 'aa11bb22' }),
      registryAccount('antigravity', { kind: 'antigravity-profile', profileId: 'work-account' }),
    ];
    const { service, calls } = fixture({
      readRegistry: async () => ({ state: 'ok', registry: { version: 2, accounts } }),
    });
    await service.get();
    expect(calls.map((call) => call.primarySources[0].selector)).toEqual([
      'aac-key:aabbccdd',
      'aac-key:11223344',
      'browser-capsule:a1b2c3d4',
      'config-home:aa11bb22',
      'antigravity-profile:work-account',
    ]);
    expect(calls.map((call) => call.accountId)).toEqual(accounts.map((account) => account.id));
  });

  it('passes independent bindings for two accounts of the same provider', async () => {
    const accounts = [
      registryAccount('zai', { kind: 'aac-key', keyId: 'aabbccdd' }, '11111111'),
      registryAccount('zai', { kind: 'aac-key', keyId: '11223344' }, '22222222'),
    ];
    const { service, calls } = fixture({
      readRegistry: async () => ({ state: 'ok', registry: { version: 2, accounts } }),
    });
    const rows = await service.get();
    expect(rows.map((row) => row.id)).toEqual(['zai:acct:11111111', 'zai:acct:22222222']);
    expect(calls[0].binding).not.toBe(calls[1].binding);
  });

  it('invalidates the binding when a saved account changes host or credential', async () => {
    let account = registryAccount('zai', { kind: 'aac-key', keyId: 'aabbccdd' });
    const { service, calls } = fixture({
      readRegistry: async () => ({ state: 'ok', registry: { version: 2, accounts: [account] } }),
    });
    await service.get();
    account = { ...account, platform: 'ubuntu', sshHost: 'nas1-agent' };
    await service.get();
    account = { ...account, credential: { kind: 'aac-key', keyId: '11223344' } };
    await service.get();
    expect(new Set(calls.map((call) => call.binding)).size).toBe(3);
    expect(calls[1].primarySources[0]).toMatchObject({ host: 'nas1', platform: 'ubuntu' });
  });

  it('never revives an old per-host sample after the coordinator rejects the account', async () => {
    let rejected = false;
    const { service, setTime } = fixture({
      failover: {
        collect: async (_target, template) =>
          rejected ? { ...template, status: 'unavailable', windows: [] } : reading(template),
        peek: (_target, template) => template,
      },
    });
    expect((await service.get()).every((row) => row.windows.length === 1)).toBe(true);
    rejected = true;
    setTime(120_001);
    expect((await service.get({ refresh: true })).every((row) => row.windows.length === 0)).toBe(
      true
    );
  });

  it('uses coordinator observations for known placeholders without retiming them', async () => {
    const { service, peeks } = fixture();
    expect(service.configured()).toBeNull();
    await service.get();
    const known = service.configured()!;
    expect(peeks).toHaveLength(ADDITIONAL_PROVIDERS.length);
    expect(known.accounts.every((row) => row.sampledAt === observed)).toBe(true);
  });

  it('never probes a corrupt registry or falls back to the old manifest', async () => {
    const { service, calls, peeks } = fixture({
      readRegistry: async (): Promise<AccountRegistryRead> => ({ state: 'invalid' }),
    });
    const rows = await service.get();
    expect(calls).toHaveLength(0);
    expect(rows.every((row) => row.status === 'unavailable' && !row.windows.length)).toBe(true);
    service.configured();
    expect(peeks).toHaveLength(0);
  });

  it('keeps concurrent reads deduplicated and respects excluded Antigravity rows', async () => {
    const { service, calls } = fixture();
    await Promise.all([
      service.get({ excludeAntigravity: true }),
      service.get({ excludeAntigravity: true }),
    ]);
    expect(calls).toHaveLength(ADDITIONAL_PROVIDERS.length - 1);
    expect(calls.some((call) => call.provider === 'antigravity')).toBe(false);
  });

  it('retains the explicit legacy transport seam for isolated existing fixtures', async () => {
    const seen: string[] = [];
    const service = new AdditionalAccountService({
      ccsDir: '/tmp/additional-legacy-fixture',
      readManifest: async () => null,
      runSource: async (source) => {
        seen.push(source.provider);
        return JSON.stringify({ status: 'unavailable', windows: [] });
      },
    });
    await service.get();
    expect(seen.sort()).toEqual([...ADDITIONAL_PROVIDERS].sort());
  });
});
