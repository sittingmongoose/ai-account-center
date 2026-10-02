import { describe, expect, it } from 'bun:test';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import { AdditionalAccountService } from '../../../src/web-server/services/additional-account-service';
import type {
  DashboardAccount,
  DashboardProvider,
} from '../../../src/web-server/services/account-dashboard-types';
import type { AntigravityDashboardAccount } from '../../../src/antigravity/usage-contract';
import { disabledAntigravityAutoSwitchStatus } from '../../../src/antigravity/runtime-composition';

const NOW = '2026-10-01T17:00:00.000Z';
const PROVIDERS: DashboardProvider[] = [
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
];
const POOL = `bucket-group:${'a'.repeat(64)}`;

function account(provider: DashboardProvider, id = `${provider}:usage`): DashboardAccount {
  return {
    id,
    provider,
    providerLabel: provider,
    label: `${provider}@example.com`,
    email: `${provider}@example.com`,
    plan: 'Pro',
    platform: 'ubuntu',
    source: 'Fixture usage',
    status: 'ok',
    message: null,
    fetchedAt: NOW,
    sampledAt: NOW,
    isActive: false,
    windows: [
      {
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 10,
        remainingPercent: 90,
        resetAt: '2026-10-02T17:00:00.000Z',
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
        kind: 'rate_limit',
      },
    ],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}
function antigravity(id: string): AntigravityDashboardAccount {
  return {
    ...account('antigravity', `antigravity:profile:${id}`),
    email: `${id}@example.com`,
    label: `${id}@example.com`,
    source: 'Antigravity saved login on Ubuntu',
    windows: [
      {
        ...account('antigravity').windows[0],
        poolId: POOL,
        poolIdSource: 'provider-bucket-membership',
        poolLabel: 'Gemini Models',
      },
    ],
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: id,
      antigravityHostIds: ['ubuntu'],
      antigravityCanActivate: false,
    },
  };
}
function deps(extra: AccountDashboardDeps = {}): AccountDashboardDeps {
  const codexProfiles = ['gmail', 'party', 'lexxmariah'];
  return {
    getCodexSummary: async () => ({
      active: { name: 'party', source: 'default', codexHome: '/fixture' },
      activated: { name: 'party', email: 'party@example.com', plan: 'pro', codexHome: '/fixture' },
      default: 'party',
      profiles: codexProfiles.map((name) => ({
        name,
        email: `${name}@example.com`,
        plan: 'pro',
        accountId: `fixture-${name}`,
        codexHome: '/fixture',
        lastUsed: null,
        authValid: true,
      })),
    }),
    getCodexRows: async () => [],
    getCachedCodexRows: () => [],
    listClaudeProfiles: async () =>
      ['platyr', 'gmail', 'party', 'me'].map((id) => ({
        id,
        email: `${id}@example.com`,
        mac: {
          launcherName: 'Fixture',
          sshHost: 'fixture-host',
          launcherPath: '/fixture.app',
          profilePath: '/fixture-profile',
        },
        windows: { launcherName: 'Fixture', profilePath: 'C:\\fixture' },
      })),
    getClaudeUsage: async (platform) => ({ platform, fetchedAt: NOW, profiles: [] }),
    getLiveClaudeUsage: async () => null,
    getCachedLiveClaudeUsage: async () => null,
    getAdditionalAccounts: async () => PROVIDERS.map((provider) => account(provider)),
    getOptionalWalletAccounts: async () => [
      {
        ...account('opencode-go', 'plan-opencode-go-console-mac-000000000000'),
        platform: 'mac',
        source: 'Authenticated OpenCode console workspace on Mac',
        label: 'OpenCode console wallet',
        email: null,
        windows: [
          {
            key: 'zen-balance',
            label: 'Console wallet',
            kind: 'balance',
            unit: 'USD',
            remaining: 0,
            usedPercent: null,
            remainingPercent: null,
            resetAt: null,
            windowMinutes: null,
            used: null,
            limit: null,
          },
        ],
      },
    ],
    getAutoSwitchStatus: () => ({
      enabled: true,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'healthy',
      message: 'Fixture healthy',
      activationInProgress: false,
    }),
    getAntigravityAutoSwitchStatus: disabledAntigravityAutoSwitchStatus,
    invalidateClaudeCache: () => {},
    scope: () => '/tmp/aac-explicit-injected-scope',
    now: () => Date.parse(NOW),
    refreshIntervalSeconds: () => 60,
    responseBudgetMs: 1000,
    hasAntigravityProfiles: () => false,
    getCachedAntigravityAccounts: () => [antigravity('gmail'), antigravity('party')],
    ...extra,
  };
}

describe('bounded Antigravity dashboard integration', () => {
  it('retains the legacy usage-only source and all nine providers before registry configuration', async () => {
    let antigravityReads = 0;
    const service = new AccountDashboardService(
      deps({
        getAntigravityAccounts: async () => {
          antigravityReads++;
          return [];
        },
      })
    );
    const result = await service.get('mac');
    expect(result.accounts).toHaveLength(15);
    expect(new Set(result.accounts.map((row) => row.provider)).size).toBe(9);
    expect(
      result.accounts.filter((row) => row.provider === 'antigravity').map((row) => row.id)
    ).toEqual(['antigravity:usage']);
    expect(antigravityReads).toBe(0);
    expect(result.antigravityAutoSwitch?.enabled).toBe(false);
    expect(result.codexAutoSwitch.thresholdPercent).toBe(5);
  });

  it('keeps two Antigravity profiles without duplicating Gmail or dropping another provider/wallet', async () => {
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => [antigravity('gmail'), antigravity('party')],
        getCachedAntigravityAccounts: () => [antigravity('gmail'), antigravity('party')],
        readSelectedAntigravityProfileId: async () => 'gmail',
      })
    );
    const result = await service.get('windows');
    expect(result.accounts).toHaveLength(16);
    expect(new Set(result.accounts.map((row) => row.provider)).size).toBe(9);
    expect(
      result.accounts.filter((row) => row.provider === 'antigravity').map((row) => row.id)
    ).toEqual(['antigravity:profile:gmail', 'antigravity:profile:party']);
    expect(result.accounts.filter((row) => row.provider === 'opencode-go')).toHaveLength(2);
    expect(result.accounts.filter((row) => row.provider === 'claude')).toHaveLength(4);
    expect(result.accounts.filter((row) => row.provider === 'codex')).toHaveLength(3);
    expect(result.accounts.find((row) => row.id === 'antigravity:profile:gmail')?.isActive).toBe(
      true
    );
    expect(result.accounts.find((row) => row.id === 'codex:party')?.isActive).toBe(true);
    expect(
      result.accounts.find((row) => row.id === 'antigravity:profile:party')?.windows[0].poolId
    ).toBe(POOL);
  });

  it('refreshes Antigravity selected identity independently from cached quota and Codex selection', async () => {
    let selected = 'gmail';
    let reads = 0;
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => {
          reads++;
          return [antigravity('gmail'), antigravity('party')];
        },
        readSelectedAntigravityProfileId: async () => selected,
      })
    );
    await service.get('mac');
    selected = 'party';
    const result = await service.get('mac');
    expect(reads).toBe(1);
    expect(
      result.accounts.filter((row) => row.provider === 'antigravity').map((row) => row.isActive)
    ).toEqual([false, true]);
    expect(result.accounts.find((row) => row.id === 'codex:party')?.isActive).toBe(true);
    expect(result.accounts.find((row) => row.id === 'antigravity:profile:party')?.sampledAt).toBe(
      NOW
    );
  });

  it('revalidates saved credential bindings on each dashboard cache hit', async () => {
    let valid = true;
    let collections = 0;
    let cacheReads = 0;
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => {
          collections++;
          return [antigravity('gmail'), antigravity('party')];
        },
        getCachedAntigravityAccounts: () => {
          cacheReads++;
          return [
            antigravity('gmail'),
            {
              ...antigravity('party'),
              ...(valid ? {} : { status: 'unavailable' as const, windows: [], sampledAt: null }),
            },
          ];
        },
        readSelectedAntigravityProfileId: async () => 'gmail',
      })
    );
    const first = await service.get('mac');
    expect(
      first.accounts.find((row) => row.id === 'antigravity:profile:party')?.windows
    ).toHaveLength(1);
    valid = false;
    const second = await service.get('mac');
    const party = second.accounts.find((row) => row.id === 'antigravity:profile:party');
    expect(party?.windows).toHaveLength(0);
    expect(party?.sampledAt).toBeNull();
    expect(party?.status).toBe('unavailable');
    expect(collections).toBe(1);
    expect(cacheReads).toBeGreaterThanOrEqual(2);
    expect(second.accounts).toHaveLength(16);
  });

  it('retains a proven activation capability only while the saved quota binding remains valid', async () => {
    let valid = true;
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => [
          {
            ...antigravity('gmail'),
            capabilities: {
              ...antigravity('gmail').capabilities,
              antigravityCanActivate: true,
            },
          },
          antigravity('party'),
        ],
        getCachedAntigravityAccounts: () => [
          {
            ...antigravity('gmail'),
            ...(valid
              ? {}
              : {
                  status: 'unavailable' as const,
                  windows: [],
                  sampledAt: null,
                }),
          },
          antigravity('party'),
        ],
        readSelectedAntigravityProfileId: async () => 'gmail',
      })
    );
    const first = await service.get('mac');
    expect(
      first.accounts.find((row) => row.id === 'antigravity:profile:gmail')?.capabilities
        .antigravityCanActivate
    ).toBe(true);
    const cached = await service.get('mac');
    expect(
      cached.accounts.find((row) => row.id === 'antigravity:profile:gmail')?.capabilities
        .antigravityCanActivate
    ).toBe(true);
    valid = false;
    const changed = await service.get('mac');
    expect(
      changed.accounts.find((row) => row.id === 'antigravity:profile:gmail')?.capabilities
        .antigravityCanActivate
    ).toBe(false);
  });

  it('does not let a slow Antigravity source hold up all other account groups', async () => {
    let release: (value: AntigravityDashboardAccount[]) => void = () => {};
    const pending = new Promise<AntigravityDashboardAccount[]>((resolve) => {
      release = resolve;
    });
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        responseBudgetMs: 20,
        getAntigravityAccounts: () => pending,
        getCachedAntigravityAccounts: () => [antigravity('gmail'), antigravity('party')],
        readSelectedAntigravityProfileId: async () => 'gmail',
      })
    );
    const result = await service.get('windows');
    expect(result.accounts).toHaveLength(16);
    expect(result.accounts.filter((row) => row.provider === 'muse')).toHaveLength(1);
    expect(result.accounts.filter((row) => row.provider === 'claude')).toHaveLength(4);
    release([antigravity('gmail'), antigravity('party')]);
    await Promise.resolve();
  });

  it('rebuilds Antigravity projections before exposing private adapter extras', async () => {
    const row = {
      ...antigravity('gmail'),
      privateCredential: 'secret-sentinel',
      identityKey: 'identity-sentinel',
      windows: [{ ...antigravity('gmail').windows[0], secret: 'window-sentinel' }],
    };
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => [row],
        readSelectedAntigravityProfileId: async () => 'gmail',
      })
    );
    const output = JSON.stringify(await service.get('mac'));
    expect(output).not.toContain('sentinel');
    expect(output).not.toContain('identityKey');
    expect(output).toContain('bucket-group:');
  });

  it('never claims an active Antigravity account when current native identity is unavailable', async () => {
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => [{ ...antigravity('gmail'), isActive: true }],
        readSelectedAntigravityProfileId: async () => null,
      })
    );
    expect(
      (await service.get('mac')).accounts.find((row) => row.provider === 'antigravity')?.isActive
    ).toBe(false);
  });

  it('preserves the ordinary additional-provider defaults and trusts only an internal Antigravity exclusion', async () => {
    const calls: string[] = [];
    const service = new AdditionalAccountService({
      ccsDir: '/tmp/aac-explicit-additional-fixture',
      readManifest: async () => null,
      runSource: async (source) => {
        calls.push(source.provider);
        return JSON.stringify(account(source.provider));
      },
      now: () => 0,
    });
    const rows = await service.get({ excludeAntigravity: true });
    expect(rows).toHaveLength(6);
    expect(calls).not.toContain('antigravity');
    expect((await service.get()).map((row) => row.provider)).toEqual(PROVIDERS);
    expect(calls.filter((provider) => provider === 'antigravity')).toHaveLength(1);
  });
});
