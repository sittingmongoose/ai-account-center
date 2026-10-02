/**
 * Registry fields of GET /api/accounts/dashboard (CONTRACT-registry-lifecycle
 * sections 2, 3.3 and 4): providers[] with visible and trayVisible,
 * settings.hiddenProviders, hiddenAccountIds and trayHiddenProviders,
 * accounts[].hidden. Fixtures only; no provider or real CCS dir.
 */
import { describe, expect, it } from 'bun:test';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import type {
  AccountDashboard,
  DashboardAccount,
  DashboardProvider,
} from '../../../src/web-server/services/account-dashboard-types';
import type { AccountVisibilityRead } from '../../../src/web-server/services/account-visibility';
import { buildDashboardProviders } from '../../../src/web-server/services/dashboard-provider-registry';
import type { AdditionalAccountsSnapshot } from '../../../src/web-server/services/additional-account-service';

const NOW = '2026-10-02T08:00:00.000Z';
const ORDER: DashboardProvider[] = [
  'claude',
  'codex',
  'antigravity',
  'cursor',
  'muse',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
];

function additional(id: string, label: string | null = null): DashboardAccount {
  const provider = id.split(':')[0] as DashboardProvider;
  return {
    id,
    provider,
    providerLabel: provider,
    label: label ?? provider,
    email: null,
    plan: 'Pro',
    platform: 'ubuntu',
    source: 'Account on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: NOW,
    sampledAt: NOW,
    isActive: false,
    windows: [
      {
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 40,
        remainingPercent: 60,
        resetAt: null,
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
      },
    ],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

/** Registry v2 with two Z.ai keys and no Kimi Code account. */
const V2: AdditionalAccountsSnapshot = {
  registry: 'v2',
  accounts: [
    additional('cursor:usage'),
    additional('muse:usage'),
    additional('qwen:usage'),
    additional('zai:usage'),
    additional('zai:acct:9f2c41d0', 'Work'),
    additional('opencode-go:usage'),
  ],
};

function deps(extra: AccountDashboardDeps = {}): AccountDashboardDeps {
  return {
    getCodexSummary: async () => ({
      active: { name: 'gmail', source: 'default', codexHome: '/fixture' },
      activated: { name: 'gmail', email: 'gmail@example.com', plan: 'pro', codexHome: '/fixture' },
      default: 'gmail',
      profiles: ['gmail', 'party'].map((name) => ({
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
    listClaudeProfiles: async () => [
      { id: 'me', email: 'me@example.com', mac: { launcherName: 'Fixture' } },
    ],
    getClaudeUsage: async (platform) => ({ platform, fetchedAt: NOW, profiles: [] }),
    getLiveClaudeUsage: async () => null,
    getCachedLiveClaudeUsage: async () => null,
    getAdditionalSnapshot: async () => V2,
    getOptionalWalletAccounts: async () => [],
    getAutoSwitchStatus: () => ({
      enabled: true,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'healthy',
      message: 'Fixture healthy',
      activationInProgress: false,
    }),
    invalidateClaudeCache: () => {},
    scope: () => '/tmp/aac-registry-fields-fixture-scope',
    now: () => Date.parse(NOW),
    refreshIntervalSeconds: () => 60,
    responseBudgetMs: 1000,
    hasAntigravityProfiles: () => false,
    readVisibility: async () => ({
      state: 'ok',
      visibility: { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] },
    }),
    ...extra,
  };
}

describe('providers[] in the dashboard DTO', () => {
  it('lists all nine providers in order with their kinds, and counts every account', async () => {
    const dashboard = await new AccountDashboardService(deps()).get('mac');
    const providers = dashboard.providers ?? [];
    expect(providers.map((entry) => entry.id)).toEqual(ORDER);
    expect(providers.map((entry) => entry.order)).toEqual(ORDER.map((_, index) => index));
    expect(providers.map((entry) => entry.iconKey)).toEqual(ORDER);
    expect(Object.fromEntries(providers.map((entry) => [entry.id, entry.signIn.kind]))).toEqual({
      claude: 'desktop-profile',
      codex: 'device-code',
      antigravity: 'supervised-cli',
      cursor: 'app-session',
      muse: 'device-code',
      'kimi-code': 'api-key',
      qwen: 'browser-session',
      zai: 'api-key',
      'opencode-go': 'api-key',
    });
    expect(providers.find((entry) => entry.id === 'antigravity')).toMatchObject({
      label: 'Antigravity',
      longLabel: 'Google Antigravity CLI',
      switchable: true,
    });
    expect(providers.find((entry) => entry.id === 'qwen')?.signIn.label).toBe(
      'Console session by browser extension'
    );
    expect(providers.find((entry) => entry.id === 'codex')?.signIn.platforms).toEqual(['ubuntu']);
    expect(providers.find((entry) => entry.id === 'claude')?.signIn.platforms).toEqual([
      'mac',
      'windows',
    ]);
    expect(providers.find((entry) => entry.id === 'muse')?.extras).toEqual({
      kind: 'browser-extension',
      platform: 'mac',
      label: 'Quota sync by browser extension',
    });
    for (const entry of providers) {
      expect(entry.accountCount).toBe(
        dashboard.accounts.filter((account) => account.provider === entry.id).length
      );
      expect(entry.signIn.secureTransportRequired).toBe(
        ['api-key', 'device-code', 'supervised-cli'].includes(entry.signIn.kind)
      );
      // With nothing hidden, every provider shows on the dashboard and in the trays.
      expect([entry.visible, entry.trayVisible]).toEqual([true, true]);
    }
    expect(providers.find((entry) => entry.id === 'zai')?.accountCount).toBe(2);
    expect(providers.filter((entry) => entry.switchable).map((entry) => entry.id)).toEqual([
      'codex',
      'antigravity',
    ]);
  });

  it('keeps a provider with no accounts (Kimi Code going away) and drops only its rows', async () => {
    const dashboard = await new AccountDashboardService(deps()).get('mac');
    expect(dashboard.accounts.some((account) => account.provider === 'kimi-code')).toBe(false);
    expect(dashboard.providers?.find((entry) => entry.id === 'kimi-code')).toMatchObject({
      accountCount: 0,
      visible: true,
      trayVisible: true,
    });
    expect(
      dashboard.accounts
        .filter((account) => account.provider === 'zai')
        .map((account) => [account.id, account.label])
    ).toEqual([
      ['zai:usage', 'zai'],
      ['zai:acct:9f2c41d0', 'Work'],
    ]);
  });

  it('reports every sign-in as not implemented when the lifecycle routes are not served', async () => {
    const dashboard = await new AccountDashboardService(
      deps({
        providerFacts: (context) => ({
          lifecycleRoutes: false,
          secureTransport: context.secureTransport === true,
        }),
      })
    ).get('mac', false, {
      secureTransport: true,
    });
    for (const entry of dashboard.providers ?? []) {
      expect(entry.signIn).toMatchObject({
        available: false,
        unavailableReason: 'not_implemented',
      });
      expect(entry.capabilities).toMatchObject({
        add: false,
        signInAgain: false,
        replaceKey: false,
        remove: false,
        recheck: false,
        activate: entry.switchable,
        autoSwitch: entry.switchable,
      });
      // Only routes that exist today are advertised: Claude's Open.
      expect(entry.capabilities.openApp).toEqual(entry.id === 'claude' ? ['mac', 'windows'] : []);
    }
  });

  it('derives capabilities from live facts once the lifecycle routes are served', () => {
    const accounts = V2.accounts;
    const secure = buildDashboardProviders(accounts, [], [], {
      lifecycleRoutes: true,
      secureTransport: true,
    });
    const byId = (list: typeof secure, id: DashboardProvider) =>
      list.find((entry) => entry.id === id);
    // Kimi Code at 0 accounts can be added; a single-account provider at 1 cannot.
    expect(byId(secure, 'kimi-code')?.capabilities).toMatchObject({
      add: true,
      replaceKey: true,
      signInAgain: false,
      remove: true,
      recheck: true,
    });
    expect(byId(secure, 'cursor')?.capabilities).toMatchObject({
      multiAccount: false,
      add: false,
      openApp: ['mac'],
      signInAgain: true,
    });
    expect(byId(secure, 'qwen')?.capabilities.add).toBe(false);
    expect(
      byId(
        buildDashboardProviders([], [], [], { lifecycleRoutes: true, secureTransport: true }),
        'cursor'
      )?.capabilities.add
    ).toBe(true);
    // 16 accounts is the per-provider limit.
    const sixteen = Array.from({ length: 16 }, (_, index) =>
      additional(`zai:acct:${index.toString(16).padStart(8, '0')}`)
    );
    expect(
      byId(
        buildDashboardProviders(sixteen, [], [], { lifecycleRoutes: true, secureTransport: true }),
        'zai'
      )?.capabilities.add
    ).toBe(false);

    const plain = buildDashboardProviders(accounts, ['zai'], [], {
      lifecycleRoutes: true,
      secureTransport: false,
    });
    expect(byId(plain, 'kimi-code')?.signIn).toMatchObject({
      available: false,
      unavailableReason: 'secure_transport_required',
    });
    expect(byId(plain, 'kimi-code')?.capabilities.add).toBe(false);
    expect(byId(plain, 'claude')?.signIn).toMatchObject({
      available: true,
      unavailableReason: null,
    });
    expect(byId(plain, 'claude')?.capabilities.signInAgain).toBe(true);
    expect(byId(plain, 'zai')?.visible).toBe(false);
    // Hiding on the dashboard does not hide in the trays.
    expect(byId(plain, 'zai')?.trayVisible).toBe(true);

    const preflight = buildDashboardProviders(accounts, [], [], {
      lifecycleRoutes: true,
      secureTransport: true,
      flows: { antigravity: 'preflight_failed', muse: 'tool_missing' },
    });
    expect(byId(preflight, 'antigravity')?.signIn.unavailableReason).toBe('preflight_failed');
    expect(byId(preflight, 'antigravity')?.capabilities.add).toBe(false);
    expect(byId(preflight, 'muse')?.signIn.unavailableReason).toBe('tool_missing');
  });

  it('passes the request transport into the facts', async () => {
    const seen: unknown[] = [];
    await new AccountDashboardService(
      deps({
        providerFacts: (context) => {
          seen.push(context);
          return { lifecycleRoutes: false, secureTransport: context.secureTransport === true };
        },
      })
    ).get('windows', false, { secureTransport: true });
    expect(seen).toEqual([{ secureTransport: true }]);
  });
});

describe('visibility in the dashboard DTO', () => {
  it('carries the hidden lists and marks rows hidden without dropping or changing them', async () => {
    const visible = await new AccountDashboardService(deps()).get('mac');
    const hiddenDashboard = await new AccountDashboardService(
      deps({
        readVisibility: async () => ({
          state: 'ok',
          visibility: {
            hiddenProviders: ['zai'],
            hiddenAccountIds: ['codex:gmail', 'claude:retired-profile'],
            trayHiddenProviders: [],
          },
        }),
      })
    ).get('mac');
    expect(hiddenDashboard.settings).toEqual({
      refreshIntervalSeconds: 60,
      hiddenProviders: ['zai'],
      hiddenAccountIds: ['codex:gmail', 'claude:retired-profile'],
      trayHiddenProviders: [],
      visibilityAvailable: true,
    });
    // Hiding on the dashboard only leaves trayVisible true.
    expect(hiddenDashboard.providers?.find((entry) => entry.id === 'zai')).toMatchObject({
      visible: false,
      trayVisible: true,
      accountCount: 2,
    });
    expect(
      hiddenDashboard.accounts.filter((account) => account.hidden).map((account) => account.id)
    ).toEqual(['codex:gmail', 'zai:usage', 'zai:acct:9f2c41d0']);
    // Display only: the same rows, still collected, still active and switchable.
    expect(hiddenDashboard.accounts.map((account) => account.id)).toEqual(
      visible.accounts.map((account) => account.id)
    );
    const strip = (account: DashboardAccount) => ({ ...account, hidden: undefined });
    expect(hiddenDashboard.accounts.map(strip)).toEqual(visible.accounts.map(strip));
    const gmail = hiddenDashboard.accounts.find((account) => account.id === 'codex:gmail');
    expect(gmail).toMatchObject({ hidden: true, isActive: true, switchable: true });
    expect(
      hiddenDashboard.accounts.find((account) => account.id === 'zai:acct:9f2c41d0')?.windows
    ).toHaveLength(1);
    expect(hiddenDashboard.codexAutoSwitch).toEqual(visible.codexAutoSwitch);
    expect(visible.accounts.every((account) => account.hidden === false)).toBe(true);
  });

  it('hides in the trays independently of the dashboard', async () => {
    const dashboard = await new AccountDashboardService(
      deps({
        readVisibility: async () => ({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            hiddenAccountIds: [],
            trayHiddenProviders: ['zai', 'qwen'],
          },
        }),
      })
    ).get('mac');
    expect(dashboard.settings).toEqual({
      refreshIntervalSeconds: 60,
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: ['zai', 'qwen'],
      visibilityAvailable: true,
    });
    // Hiding in the tray only leaves visible true, and no row becomes hidden.
    expect(
      (dashboard.providers ?? [])
        .filter((entry) => ['qwen', 'zai'].includes(entry.id))
        .map((entry) => [entry.id, entry.visible, entry.trayVisible])
    ).toEqual([
      ['qwen', true, false],
      ['zai', true, false],
    ]);
    expect(dashboard.accounts.some((account) => account.hidden)).toBe(false);
  });

  it('says when the visibility file could not be read, and hides nothing', async () => {
    const dashboard = await new AccountDashboardService(
      deps({ readVisibility: async () => ({ state: 'unavailable' }) })
    ).get('mac');
    expect(dashboard.settings).toMatchObject({
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: [],
      visibilityAvailable: false,
    });
    expect(dashboard.accounts.some((account) => account.hidden)).toBe(false);
    const thrown = await new AccountDashboardService(
      deps({
        readVisibility: async () => {
          throw new TypeError('PRIVATE_PATH');
        },
      })
    ).get('mac');
    expect(thrown.settings?.visibilityAvailable).toBe(false);
    expect(JSON.stringify(thrown)).not.toContain('PRIVATE_PATH');
  });
});

describe('an unreadable visibility file after a good read', () => {
  it('keeps the last good lists, says they could not be read, and never unhides rows', async () => {
    const reads: Array<() => Promise<AccountVisibilityRead>> = [
      async () => ({
        state: 'ok',
        visibility: {
          hiddenProviders: ['zai'],
          hiddenAccountIds: ['codex:gmail'],
          trayHiddenProviders: ['qwen'],
        },
      }),
      async () => ({ state: 'unavailable' }),
      async () => {
        throw new TypeError('PRIVATE_PATH');
      },
      async () => ({
        state: 'ok',
        visibility: { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] },
      }),
    ];
    let index = 0;
    const service = new AccountDashboardService(
      deps({ readVisibility: () => reads[Math.min(index++, reads.length - 1)]() })
    );
    const hiddenIds = (dashboard: AccountDashboard) =>
      dashboard.accounts.filter((account) => account.hidden).map((account) => account.id);
    const good = await service.get('mac');
    expect(good.settings).toMatchObject({
      hiddenProviders: ['zai'],
      hiddenAccountIds: ['codex:gmail'],
      trayHiddenProviders: ['qwen'],
      visibilityAvailable: true,
    });
    const expected = ['codex:gmail', 'zai:usage', 'zai:acct:9f2c41d0'];
    expect(hiddenIds(good)).toEqual(expected);
    for (const label of ['unavailable', 'thrown']) {
      const bad = await service.get('mac');
      expect({ label, settings: bad.settings }).toMatchObject({
        label,
        settings: {
          hiddenProviders: ['zai'],
          hiddenAccountIds: ['codex:gmail'],
          trayHiddenProviders: ['qwen'],
          visibilityAvailable: false,
        },
      });
      expect(hiddenIds(bad)).toEqual(expected);
      expect(bad.providers?.find((entry) => entry.id === 'zai')?.visible).toBe(false);
      // The tray list falls back to the last good read exactly like the others.
      expect(bad.providers?.find((entry) => entry.id === 'qwen')?.trayVisible).toBe(false);
      expect(JSON.stringify(bad)).not.toContain('PRIVATE_PATH');
    }
    // A later good read (here: everything shown again on purpose) replaces the memory.
    const repaired = await service.get('mac');
    expect(repaired.settings).toMatchObject({
      hiddenProviders: [],
      trayHiddenProviders: [],
      visibilityAvailable: true,
    });
    expect(repaired.providers?.find((entry) => entry.id === 'qwen')?.trayVisible).toBe(true);
    expect(hiddenIds(repaired)).toEqual([]);
  });

  it('keeps each scope separate', async () => {
    let scope = '/tmp/aac-scope-a';
    let failing = false;
    const service = new AccountDashboardService(
      deps({
        scope: () => scope,
        readVisibility: async (current) =>
          failing
            ? { state: 'unavailable' }
            : {
                state: 'ok',
                visibility: {
                  hiddenProviders: current === '/tmp/aac-scope-a' ? ['zai'] : [],
                  hiddenAccountIds: [],
                  trayHiddenProviders: [],
                },
              },
      })
    );
    await service.get('mac');
    failing = true;
    scope = '/tmp/aac-scope-b';
    // Scope b never had a good read: nothing hidden, and unavailable.
    expect((await service.get('mac')).settings).toMatchObject({
      hiddenProviders: [],
      trayHiddenProviders: [],
      visibilityAvailable: false,
    });
    scope = '/tmp/aac-scope-a';
    expect((await service.get('mac')).settings).toMatchObject({
      hiddenProviders: ['zai'],
      trayHiddenProviders: [],
      visibilityAvailable: false,
    });
  });
});

describe('a slow visibility read', () => {
  it('keeps the last read for this scope instead of unhiding rows', async () => {
    let slow = false;
    const service = new AccountDashboardService(
      deps({
        responseBudgetMs: 10,
        readVisibility: () =>
          slow
            ? new Promise(() => {})
            : Promise.resolve({
                state: 'ok' as const,
                visibility: {
                  hiddenProviders: ['zai' as const],
                  hiddenAccountIds: [],
                  trayHiddenProviders: [],
                },
              }),
      })
    );
    expect((await service.get('mac')).settings?.hiddenProviders).toEqual(['zai']);
    slow = true;
    const started = Date.now();
    const next = await service.get('mac', true);
    expect(Date.now() - started).toBeLessThan(1000);
    expect(next.settings).toMatchObject({ hiddenProviders: ['zai'], visibilityAvailable: true });
    expect(next.accounts.find((account) => account.id === 'zai:usage')?.hidden).toBe(true);
  });
});

describe('additional placeholders while collection is slow', () => {
  it('uses the configured registry v2 accounts, not the seven defaults', async () => {
    const never = new Promise<AdditionalAccountsSnapshot>(() => {});
    const dashboard = await new AccountDashboardService(
      deps({
        responseBudgetMs: 10,
        getAdditionalSnapshot: () => never,
        getConfiguredAdditional: () => ({
          registry: 'v2',
          accounts: V2.accounts.map((account) => ({
            ...account,
            status: 'unavailable',
            windows: [],
          })),
        }),
      })
    ).get('mac');
    const rows = dashboard.accounts.filter(
      (account) => !['codex', 'claude'].includes(account.provider)
    );
    expect(rows.map((account) => [account.id, account.status])).toEqual(
      // Rows keep the existing additional-provider order; clients order sections by providers[].order.
      [
        'muse:usage',
        'cursor:usage',
        'qwen:usage',
        'zai:usage',
        'zai:acct:9f2c41d0',
        'opencode-go:usage',
      ].map((id) => [id, 'unavailable'])
    );
    const legacy = await new AccountDashboardService(
      deps({
        responseBudgetMs: 10,
        getAdditionalSnapshot: () => never,
        getConfiguredAdditional: () => null,
      })
    ).get('mac');
    expect(
      legacy.accounts
        .filter((account) => !['codex', 'claude'].includes(account.provider))
        .map((account) => account.id)
    ).toEqual(
      ['antigravity', 'muse', 'cursor', 'kimi-code', 'qwen', 'zai', 'opencode-go'].map(
        (provider) => `${provider}:usage`
      )
    );
  });
});
