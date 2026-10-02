/**
 * Lifecycle fields of GET /api/accounts/dashboard once the lifecycle routes
 * ship (CONTRACT-registry-lifecycle sections 2 and 6): the default provider
 * facts, running sign-in jobs on rows, pending Claude profiles, and the cache
 * being dropped after a change. Fixtures only.
 */
import { describe, expect, it } from 'bun:test';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import { lifecycleProviderFacts } from '../../../src/web-server/services/account-lifecycle-runtime';

const NOW = '2026-10-02T08:00:00.000Z';

function zai(id: string, status: DashboardAccount['status'] = 'ok'): DashboardAccount {
  return {
    id,
    provider: 'zai',
    providerLabel: 'Z.ai',
    label: 'Z.ai',
    email: null,
    plan: null,
    platform: 'ubuntu',
    source: 'Account on Ubuntu',
    status,
    message: null,
    fetchedAt: NOW,
    sampledAt: NOW,
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

function deps(
  extra: AccountDashboardDeps = {}
): AccountDashboardDeps & { collections: () => number } {
  let collections = 0;
  return {
    getCodexSummary: async () => ({
      active: null,
      activated: { name: 'gmail', email: 'gmail@example.com', plan: 'pro', codexHome: '/f' },
      default: 'gmail',
      profiles: ['gmail', 'party'].map((name) => ({
        name,
        email: `${name}@example.com`,
        plan: 'pro',
        accountId: `fixture-${name}`,
        codexHome: '/f',
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
    getAdditionalSnapshot: async () => {
      collections += 1;
      return { registry: 'v2', accounts: [zai('zai:usage'), zai('zai:acct:9f2c41d0')] };
    },
    getOptionalWalletAccounts: async () => [],
    getAutoSwitchStatus: () => ({
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      message: '',
      activationInProgress: false,
    }),
    invalidateClaudeCache: () => {},
    scope: () => '/tmp/aac-lifecycle-fields-fixture-scope',
    now: () => Date.parse(NOW),
    refreshIntervalSeconds: () => 600,
    responseBudgetMs: 1000,
    hasAntigravityProfiles: () => false,
    readVisibility: async () => ({
      state: 'ok',
      visibility: { hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] },
    }),
    providerFacts: (context) =>
      lifecycleProviderFacts(context, {
        codexCliAvailable: () => true,
        claudeEnabled: () => false,
        antigravityFlow: () => 'preflight_failed',
      }),
    accountJobStates: () => new Map(),
    listClaudePending: async () => [],
    ...extra,
    collections: () => collections,
  };
}

describe('lifecycle provider facts', () => {
  it('advertises the served flows and nothing else', async () => {
    const secure = await new AccountDashboardService(deps()).get('mac', false, {
      secureTransport: true,
    });
    const entry = (id: string) => secure.providers?.find((provider) => provider.id === id);
    expect(entry('codex')?.signIn).toMatchObject({ available: true, unavailableReason: null });
    expect(entry('codex')?.capabilities).toMatchObject({
      add: true,
      signInAgain: true,
      remove: true,
      recheck: false,
    });
    expect(entry('zai')?.capabilities).toMatchObject({
      add: true,
      replaceKey: true,
      recheck: true,
    });
    expect(entry('claude')?.signIn.unavailableReason).toBe('not_implemented');
    expect(entry('claude')?.capabilities).toMatchObject({
      add: false,
      remove: false,
      signInAgain: true,
      openApp: ['mac', 'windows'],
    });
    expect(entry('muse')?.signIn.unavailableReason).toBe('not_implemented');
    // Antigravity signs in from a terminal: the UI shows the command (contract 6.2).
    expect(entry('antigravity')?.signIn).toMatchObject({
      kind: 'supervised-cli',
      available: false,
      unavailableReason: 'preflight_failed',
    });
    expect(entry('antigravity')?.capabilities).toMatchObject({
      add: false,
      remove: true,
      recheck: false,
    });
    expect(entry('cursor')?.capabilities).toMatchObject({ openApp: ['mac'], recheck: true });

    const plain = await new AccountDashboardService(deps()).get('mac', false, {
      secureTransport: false,
    });
    const plainEntry = (id: string) => plain.providers?.find((provider) => provider.id === id);
    expect(plainEntry('codex')?.signIn.unavailableReason).toBe('secure_transport_required');
    expect(plainEntry('zai')?.capabilities.add).toBe(false);
    expect(plainEntry('cursor')?.signIn.available).toBe(true);
  });

  it('reports a missing codex binary as tool_missing', () => {
    const facts = lifecycleProviderFacts(
      { secureTransport: true },
      { codexCliAvailable: () => false, claudeEnabled: () => true }
    );
    expect(facts.flows?.codex).toBe('tool_missing');
    expect(facts.flows?.claude).toBeUndefined();
    expect(facts.remove?.claude).toBe(true);
  });
});

describe('lifecycle fields on rows', () => {
  it('shows a running sign-in on its account and pending Claude profiles', async () => {
    const dashboard = await new AccountDashboardService(
      deps({
        accountJobStates: () =>
          new Map([
            ['codex:party', { state: 'verifying' as const, jobId: 'job_0123456789abcdef' }],
          ]),
        listClaudePending: async () => [
          {
            id: 'work2',
            label: 'Work 2',
            mac: { launcherName: 'x', profilePath: '/m', sshHost: 'jared-mac' },
            windows: { launcherName: 'x', profilePath: 'C:\\w', sshHost: 'jared-windows' },
            createdAt: '2026-10-02T07:00:00Z',
          },
          {
            id: 'me',
            label: null,
            mac: { launcherName: 'x', profilePath: '/m', sshHost: 'jared-mac' },
            windows: { launcherName: 'x', profilePath: 'C:\\w', sshHost: 'jared-windows' },
            createdAt: '2026-10-02T07:00:00Z',
          },
        ],
      })
    ).get('windows');
    const byId = (id: string) => dashboard.accounts.find((account) => account.id === id);
    expect(byId('codex:party')?.lifecycle).toEqual({
      state: 'verifying',
      jobId: 'job_0123456789abcdef',
    });
    expect(byId('codex:gmail')?.lifecycle).toEqual({ state: 'ready', jobId: null });
    expect(byId('claude:work2')).toMatchObject({
      label: 'Work 2',
      email: null,
      status: 'needs_sign_in',
      platform: 'windows',
      switchable: false,
      hidden: false,
      windows: [],
      lifecycle: { state: 'pending_sign_in', jobId: null },
      capabilities: { claudeProfileId: 'work2', claudePlatforms: [] },
    });
    // An inventory profile with the same id is never doubled by a pending entry.
    expect(dashboard.accounts.filter((account) => account.id === 'claude:me')).toHaveLength(1);
    expect(dashboard.providers?.find((entry) => entry.id === 'claude')?.accountCount).toBe(2);
  });

  it('collects again after invalidate and swaps in a re-checked row', async () => {
    const fixture = deps();
    const service = new AccountDashboardService(fixture);
    await service.get('mac');
    await service.get('mac');
    expect(fixture.collections()).toBe(1);
    service.invalidate();
    await service.get('mac');
    expect(fixture.collections()).toBe(2);
    service.replaceAdditionalRow({ ...zai('zai:acct:9f2c41d0', 'error'), message: 'Re-checked' });
    const after = await service.get('mac');
    expect(after.accounts.find((account) => account.id === 'zai:acct:9f2c41d0')).toMatchObject({
      status: 'error',
      message: 'Re-checked',
    });
    expect(fixture.collections()).toBe(2);
  });
});
