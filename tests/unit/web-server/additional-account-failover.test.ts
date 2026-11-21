import { describe, expect, it } from 'bun:test';
import fs from 'fs/promises';
import os from 'os';
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
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';
import type {
  UsageCandidate,
  UsageFailoverTarget,
  UsageHelperRequest,
  UsageHelperResponse,
  UsageHost,
  UsageReading,
} from '../../../src/web-server/services/usage-failover-contract';

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

  it('shows one row when two registry entries read the same credential identity', async () => {
    const accounts = [
      registryAccount('opencode-go', { kind: 'discover' }),
      registryAccount('opencode-go', { kind: 'aac-key', keyId: '52f3251b' }, '52f3251b'),
      registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
      registryAccount('zai', { kind: 'aac-key', keyId: 'aabbccdd' }, 'aabbccdd'),
      registryAccount('zai', { kind: 'aac-key', keyId: '11223344' }, '11223344'),
    ];
    // discover and key 2e034ec2 resolve to one OpenCode Go account; the Z.ai rows have no
    // known identity yet and a different provider never merges with OpenCode Go.
    const identities: Record<string, { kind: string; key: string } | null> = {
      'opencode-go:usage': { kind: 'opencode-go-key', key: 'identity-a' },
      'opencode-go:acct:52f3251b': { kind: 'opencode-go-key', key: 'identity-b' },
      'opencode-go:acct:2e034ec2': { kind: 'opencode-go-key', key: 'identity-a' },
      'zai:acct:aabbccdd': null,
      'zai:acct:11223344': null,
    };
    const { service, calls } = fixture({
      readRegistry: async () => ({ state: 'ok', registry: { version: 2, accounts } }),
    });
    const base = (service as unknown as { failover: Record<string, unknown> }).failover;
    base.readingIdentity = (target: UsageFailoverTarget) => identities[target.accountId] ?? null;
    const rows = await service.get();
    // The explicitly bound row stays; the discovery row bound to the same key is not shown.
    expect(rows.map((row) => row.id)).toEqual([
      'opencode-go:acct:52f3251b',
      'opencode-go:acct:2e034ec2',
      'zai:acct:aabbccdd',
      'zai:acct:11223344',
    ]);
    expect(calls).toHaveLength(5);
    expect(service.configured()?.accounts.map((row) => row.id)).toEqual(rows.map((row) => row.id));
  });

  describe('every stored OpenCode Go key is read once (card OCG1)', () => {
    const id = (letter: string) => ({ kind: 'opencode-go-credential', key: letter.repeat(64) });
    function stored(
      accounts: RegistryAccount[],
      found: Array<{ identity: { kind: string; key: string }; bound: boolean }> | null,
      named: Record<string, { kind: string; key: string }> = {},
      unread: string[] = []
    ) {
      const asked: string[][] = [];
      const harness = fixture({
        readRegistry: async () => ({ state: 'ok', registry: { version: 2, accounts } }),
      });
      const base = (harness.service as unknown as { failover: Record<string, unknown> }).failover;
      base.discoveredIdentities = async (_target: UsageFailoverTarget, bound: string[]) => {
        asked.push([...bound].sort());
        return found === null ? null : { found, unread };
      };
      base.readingIdentity = (target: UsageFailoverTarget) =>
        target.expectedIdentity ?? named[target.accountId] ?? null;
      return { ...harness, asked };
    }

    it('three distinct stored keys give three rows, each key read once', async () => {
      const { service, calls, asked } = stored(
        [
          registryAccount('opencode-go', { kind: 'discover' }),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '52f3251b' }, '52f3251b'),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
        ],
        [
          { identity: id('a'), bound: true },
          { identity: id('b'), bound: true },
          { identity: id('c'), bound: false },
        ],
        { 'opencode-go:acct:52f3251b': id('b'), 'opencode-go:acct:2e034ec2': id('a') }
      );
      const rows = await service.get();
      expect(asked).toEqual([['aac-key:2e034ec2', 'aac-key:52f3251b']]);
      expect(rows.map((row) => row.id)).toEqual([
        'opencode-go:usage:cccccccc',
        'opencode-go:acct:52f3251b',
        'opencode-go:acct:2e034ec2',
      ]);
      expect(rows[0].label).toBe('Key cccccccc');
      // One read per stored key: the discovery row reads only the key no entry names.
      expect(calls.map((call) => call.accountId)).toEqual(rows.map((row) => row.id));
      expect(calls[0].expectedIdentity).toEqual(id('c'));
      expect(calls[0].primarySources[0].selector).toBe('discover');
      expect(service.configured()?.accounts.map((row) => row.id)).toEqual(rows.map((r) => r.id));
      expect((await service.refreshAccount('opencode-go:usage:cccccccc'))?.id).toBe(
        'opencode-go:usage:cccccccc'
      );
    });

    it('a discovery entry alone gives one row per stored key', async () => {
      const { service, calls } = stored(
        [registryAccount('opencode-go', { kind: 'discover' })],
        [
          { identity: id('a'), bound: false },
          { identity: id('b'), bound: false },
          { identity: id('c'), bound: false },
        ]
      );
      const rows = await service.get();
      expect(rows.map((row) => row.id)).toEqual([
        'opencode-go:usage:aaaaaaaa',
        'opencode-go:usage:bbbbbbbb',
        'opencode-go:usage:cccccccc',
      ]);
      expect(calls.map((call) => call.expectedIdentity)).toEqual([id('a'), id('b'), id('c')]);
    });

    it('adds no row and no read when every stored key already has its own entry', async () => {
      const { service, calls } = stored(
        [
          registryAccount('opencode-go', { kind: 'discover' }),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
        ],
        [{ identity: id('a'), bound: true }],
        { 'opencode-go:acct:2e034ec2': id('a') }
      );
      expect((await service.get()).map((row) => row.id)).toEqual(['opencode-go:acct:2e034ec2']);
      expect(calls.map((call) => call.accountId)).toEqual(['opencode-go:acct:2e034ec2']);
    });

    it('keeps the discovery entry as it was when the stores cannot be read', async () => {
      const { service, calls } = stored(
        [registryAccount('opencode-go', { kind: 'discover' })],
        null
      );
      expect((await service.get()).map((row) => row.id)).toEqual(['opencode-go:usage']);
      expect(calls[0].expectedIdentity).toBeUndefined();
    });

    it('a covered discover entry answers no Re-check and reads nothing unpinned', async () => {
      const { service, calls } = stored(
        [
          registryAccount('opencode-go', { kind: 'discover' }),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
        ],
        [
          { identity: id('a'), bound: true },
          { identity: id('c'), bound: false },
        ],
        { 'opencode-go:acct:2e034ec2': id('a') }
      );
      expect(await service.refreshAccount('opencode-go:usage')).toBeNull();
      expect(await service.refreshAccount('opencode-go:usage:dddddddd')).toBeNull();
      expect(await service.refreshAccount('opencode-go:usage:aaaaaaaa')).toBeNull();
      expect(calls).toHaveLength(0);
      expect((await service.refreshAccount('opencode-go:usage:cccccccc'))?.id).toBe(
        'opencode-go:usage:cccccccc'
      );
      expect(calls.map((call) => call.expectedIdentity)).toEqual([id('c')]);
    });

    it('a named key that cannot be read stands in with its last known identity', async () => {
      const { service, calls } = stored(
        [
          registryAccount('opencode-go', { kind: 'discover' }),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '52f3251b' }, '52f3251b'),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
        ],
        [
          { identity: id('a'), bound: false },
          { identity: id('b'), bound: true },
          { identity: id('c'), bound: false },
        ],
        { 'opencode-go:acct:52f3251b': id('b'), 'opencode-go:acct:2e034ec2': id('a') },
        ['aac-key:2e034ec2']
      );
      expect((await service.get()).map((row) => row.id)).toEqual([
        'opencode-go:usage:cccccccc',
        'opencode-go:acct:52f3251b',
        'opencode-go:acct:2e034ec2',
      ]);
      expect(calls.filter((call) => call.expectedIdentity)).toHaveLength(1);
    });

    it('with no known identity for an unreadable named key, discovery waits (no second row)', async () => {
      const { service, calls } = stored(
        [
          registryAccount('opencode-go', { kind: 'discover' }),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
        ],
        [
          { identity: id('a'), bound: false },
          { identity: id('c'), bound: false },
        ],
        {},
        ['aac-key:2e034ec2']
      );
      expect((await service.get()).map((row) => row.id)).toEqual(['opencode-go:acct:2e034ec2']);
      expect(calls.map((call) => call.accountId)).toEqual(['opencode-go:acct:2e034ec2']);
    });

    it('after a restart, an unreadable named key file still leaves the unnamed key its row (N3)', async () => {
      // The real coordinator on a temporary CCS folder: the first sample reads and saves every
      // key; after a restart the named slot's file is unreadable and its identity is known only
      // on disk. The unnamed key keeps its row, and no account shows twice.
      const ccsDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-ocg1-restart-'));
      try {
        const accounts = [
          registryAccount('opencode-go', { kind: 'discover' }),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '52f3251b' }, '52f3251b'),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
        ];
        const readRegistry = async (): Promise<AccountRegistryRead> => ({
          state: 'ok',
          registry: { version: 2, accounts },
        });
        const mac: UsageHost = { host: 'mac', platform: 'mac', sshHost: 'fixture-mac' };
        const stored = (letter: string, selectors: string[]): UsageCandidate => ({
          locator: letter.repeat(64),
          selectors,
          identity: id(letter),
          revision: '2'.repeat(64),
          email: null,
          plan: 'Go',
        });
        let now = Date.parse('2026-10-14T12:00:00.000Z');
        const sample = (): UsageReading => ({
          status: 'ok',
          email: null,
          plan: 'Go',
          fetchedAt: new Date(now).toISOString(),
          sampledAt: new Date(now).toISOString(),
          windows: [
            {
              key: 'weekly',
              label: 'Weekly',
              usedPercent: 10,
              remainingPercent: 90,
              used: 10,
              limit: 100,
              resetAt: null,
              windowMinutes: 10080,
              unit: 'credits',
            },
          ],
        });
        const collected: Array<{ selectors: string[]; identityKey: string }> = [];
        const serviceFor = (candidates: UsageCandidate[], incompleteSelectors?: string[]) => {
          const transport = async (
            _host: UsageHost,
            request: UsageHelperRequest
          ): Promise<UsageHelperResponse> => {
            if (request.action === 'inventory')
              return {
                version: 1,
                action: 'inventory',
                candidates: structuredClone(candidates),
                ...(incompleteSelectors ? { incompleteSelectors } : {}),
              };
            collected.push({
              selectors: structuredClone(request.candidate?.selectors ?? []),
              identityKey: request.candidate?.identity.key ?? '',
            });
            return {
              version: 1,
              action: 'collect',
              candidate: structuredClone(request.candidate),
              sample: sample(),
              failureCode: null,
            };
          };
          return new AdditionalAccountService({
            ccsDir,
            readRegistry,
            now: () => now,
            failover: new UsageFailoverService({
              transport,
              hosts: async () => [mac],
              now: () => now,
            }),
          });
        };
        const readable = [
          stored('a', ['discover', 'aac-key:2e034ec2']),
          stored('b', ['discover', 'aac-key:52f3251b']),
          stored('c', ['discover']),
        ];
        const first = await serviceFor(readable).get();
        expect(first.map((row) => row.id)).toEqual([
          'opencode-go:usage:cccccccc',
          'opencode-go:acct:52f3251b',
          'opencode-go:acct:2e034ec2',
        ]);
        // Restart: Account 3's key file cannot be read; the stores still list its key.
        const before = collected.length;
        now += 31_000;
        const partial = [
          stored('a', ['discover']),
          stored('b', ['discover', 'aac-key:52f3251b']),
          stored('c', ['discover']),
        ];
        const rows = await serviceFor(partial, ['aac-key:2e034ec2']).get();
        expect(rows.map((row) => row.id)).toEqual([
          'opencode-go:usage:cccccccc',
          'opencode-go:acct:52f3251b',
          'opencode-go:acct:2e034ec2',
        ]);
        // Every key is still read exactly once: Account 3 through the store's own listing of
        // its identity (never through the unreadable slot), the unnamed key pinned to itself.
        const after = collected.slice(before);
        expect(after.some((call) => call.selectors.includes('aac-key:2e034ec2'))).toBe(false);
        expect(after.map((call) => call.identityKey).sort()).toEqual([
          'a'.repeat(64),
          'b'.repeat(64),
          'c'.repeat(64),
        ]);
      } finally {
        await fs.rm(ccsDir, { recursive: true, force: true });
      }
    });

    it('two entries that read the same key still show one row', async () => {
      const { service } = stored(
        [
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
          registryAccount('opencode-go', { kind: 'aac-key', keyId: '9e034ec2' }, '9e034ec2'),
        ],
        null,
        { 'opencode-go:acct:2e034ec2': id('a'), 'opencode-go:acct:9e034ec2': id('a') }
      );
      expect((await service.get()).map((row) => row.id)).toEqual(['opencode-go:acct:2e034ec2']);
    });
  });

  it('keeps two discovery-free rows of one identity as the first in list order', async () => {
    const accounts = [
      registryAccount('opencode-go', { kind: 'aac-key', keyId: '2e034ec2' }, '2e034ec2'),
      registryAccount('opencode-go', { kind: 'aac-key', keyId: '52f3251b' }, '52f3251b'),
    ];
    const { service } = fixture({
      readRegistry: async () => ({ state: 'ok', registry: { version: 2, accounts } }),
    });
    const base = (service as unknown as { failover: Record<string, unknown> }).failover;
    base.readingIdentity = () => ({ kind: 'opencode-go-key', key: 'identity-a' });
    expect((await service.get()).map((row) => row.id)).toEqual(['opencode-go:acct:2e034ec2']);
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
