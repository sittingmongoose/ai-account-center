import { describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import type { CodexAuthProfilesSummary } from '../../../src/codex-auth/codex-auth-dashboard-service';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import type { ClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-profile-service';
import type { UsageFailoverTarget } from '../../../src/web-server/services/usage-failover-contract';

const NOW = Date.parse('2026-10-14T12:00:00.000Z');
const SAMPLE = '2026-10-14T11:59:00.000Z';
const SCOPE = path.join(os.tmpdir(), 'aac-dashboard-failover-fixture-unused');
const profile: ClaudeDesktopProfile = {
  id: 'fixture-person',
  email: 'fixture@example.test',
  mac: {
    launcherName: 'Fixture Claude',
    launcherPath: '/Applications/Fixture Claude.app',
    profilePath: '/private/fixture-profile',
    sshHost: 'fixture-mac',
  },
  windows: {
    launcherName: 'Fixture Claude',
    profilePath: 'C:\\fixture-profile',
    sshHost: 'fixture-windows',
  },
};

function quota(template: DashboardAccount, status: 'ok' | 'cached' = 'ok'): DashboardAccount {
  return {
    ...template,
    status,
    source: 'Verified quota on Nas1 (fallback)',
    host: 'nas1',
    plan: 'max',
    sampledAt: SAMPLE,
    fetchedAt: SAMPLE,
    message: null,
    windows: [
      {
        key: 'five_hour',
        label: 'Five-hour usage',
        usedPercent: 0,
        remainingPercent: 100,
        resetAt: '2026-10-14T16:00:00.000Z',
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
    ],
  };
}

function codexFixture() {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dashboard-codex-concurrent-'));
  const ccs = path.join(temporary, '.ccs');
  const authFile = path.join(ccs, 'codex-instances', 'saved-fixture', 'auth.json');
  fs.mkdirSync(path.dirname(authFile), { recursive: true });
  const select = (workspace: string, valid = true): CodexAuthProfilesSummary => {
    const payload = {
      email: 'fixture@example.test',
      sub: 'fixture-person',
      iss: 'https://auth.openai.com',
      'https://api.openai.com/auth': {
        chatgpt_account_id: workspace,
        chatgpt_user_id: 'fixture-person',
      },
    };
    fs.writeFileSync(
      authFile,
      JSON.stringify({
        tokens: {
          access_token: 'fixture-private-access',
          account_id: workspace,
          id_token: `e30.${Buffer.from(JSON.stringify(payload)).toString('base64url')}.fixture`,
        },
      })
    );
    return {
      active: null,
      activated: null,
      default: 'saved-fixture',
      profiles: [
        {
          name: 'saved-fixture',
          email: 'fixture@example.test',
          plan: 'pro',
          accountId: workspace,
          codexHome: path.dirname(authFile),
          lastUsed: null,
          authValid: valid,
        },
      ],
    };
  };
  return { ccs, select, cleanup: () => fs.rmSync(temporary, { recursive: true, force: true }) };
}

/** Every OS/provider boundary is injected; an empty Codex inventory reads no auth files. */
function deps(overrides: AccountDashboardDeps = {}): AccountDashboardDeps {
  return {
    now: () => NOW,
    scope: () => SCOPE,
    responseBudgetMs: 1000,
    refreshIntervalSeconds: () => 60,
    serverInfo: () => undefined,
    getCodexSummary: async () => ({ active: null, activated: null, default: null, profiles: [] }),
    getCodexRows: async (names) => {
      expect(names).toEqual([]);
      return [];
    },
    getCachedCodexRows: () => [],
    getRenewalStatus: async () => ({ profiles: [] }),
    listClaudeProfiles: async () => [profile],
    listClaudePending: async () => [],
    getClaudeMacSignIn: async () => 'signed-in',
    getClaudeUsage: async () => {
      throw new Error('Legacy Claude history must not run through the failover branch.');
    },
    getLiveClaudeUsage: async () => {
      throw new Error('Legacy Claude live collector must not run through the failover branch.');
    },
    getCachedLiveClaudeUsage: async () => {
      throw new Error('Legacy Claude cache must not run through the failover branch.');
    },
    getAdditionalAccounts: async () => [],
    getConfiguredAdditional: () => ({ registry: 'v1', accounts: [] }),
    hasAntigravityProfiles: () => false,
    getAntigravityAutoSwitchStatus: () => ({
      enabled: false,
      thresholdUsedPercent: 95,
      pollIntervalSeconds: 60,
      maxQuotaAgeSeconds: 120,
      cooldownSeconds: 60,
      selectedHostIds: [],
      requestedPoolId: null,
      outcome: 'disabled',
      message: 'Disabled',
      activationInProgress: false,
    }),
    getAutoSwitchStatus: () => ({
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      message: 'Disabled',
      activationInProgress: false,
    }),
    accountJobStates: () => new Map(),
    providerFacts: () => ({ lifecycleRoutes: false, secureTransport: false }),
    invalidateClaudeCache: () => {},
    readVisibility: async () => ({
      state: 'ok',
      visibility: {
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      },
    }),
    ...overrides,
  };
}

describe('account dashboard shared usage failover integration', () => {
  it('never rebinds a published Codex reading to another platform collection after a same-email workspace change', async () => {
    const fixture = codexFixture();
    let release!: () => void;
    let entered!: () => void;
    const pending = new Promise<void>((resolve) => {
      release = resolve;
    });
    const started = new Promise<void>((resolve) => {
      entered = resolve;
    });
    try {
      let summary = fixture.select('workspace-before');
      let slow = false;
      const service = new AccountDashboardService(
        deps({
          scope: () => fixture.ccs,
          responseBudgetMs: 5,
          listClaudeProfiles: async () => [],
          getCodexSummary: async () => summary,
          getCodexRows: async () => {
            if (slow) {
              entered();
              await pending;
            }
            return [];
          },
          failover: {
            peek: (_, template) => template,
            collect: async (_, template) => template,
            recover: async (_, template) => quota(template),
          },
        })
      );
      expect((await service.get('mac')).accounts[0].windows).toHaveLength(1);
      summary = fixture.select('workspace-after');
      slow = true;
      const windows = service.get('windows');
      await started;
      const oldPlatform = (await service.peek('mac')).accounts[0];
      expect(oldPlatform).toMatchObject({ status: 'unavailable', windows: [], sampledAt: null });
      expect(oldPlatform.message).toContain('saved Codex login changed');
      release();
      await windows;
    } finally {
      release();
      fixture.cleanup();
    }
  });

  it('does not overlay fallback quota on a Codex profile whose current summary requires sign-in', async () => {
    const fixture = codexFixture();
    try {
      let summary = fixture.select('workspace');
      let peeks = 0;
      const service = new AccountDashboardService(
        deps({
          scope: () => fixture.ccs,
          listClaudeProfiles: async () => [],
          getCodexSummary: async () => summary,
          getCodexRows: async () => [],
          failover: {
            peek: (_, template) => {
              peeks++;
              return quota(template);
            },
            collect: async (_, template) => template,
            recover: async (_, template) => quota(template),
          },
        })
      );
      expect((await service.get('mac')).accounts[0].windows).toHaveLength(1);
      const initialPeeks = peeks;
      summary = fixture.select('workspace', false);
      const current = (await service.peek('mac')).accounts[0];
      expect(current).toMatchObject({ status: 'needs_sign_in', windows: [], switchable: false });
      expect(peeks).toBe(initialPeeks);
    } finally {
      fixture.cleanup();
    }
  });

  it('expires retained Claude optional balances on a dashboard cache hit without another collection', async () => {
    let now = NOW;
    let collections = 0;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        failover: {
          peek: (_, template) => template,
          collect: async (_, template) => {
            collections++;
            const row = quota(template);
            return {
              ...row,
              windows: [
                ...row.windows,
                {
                  ...row.windows[0],
                  key: 'prepaid_balance',
                  label: 'Prepaid balance',
                  status: 'cached',
                  sampledAt: new Date(NOW - 86_400_000 + 10).toISOString(),
                },
              ],
            };
          },
          recover: async (_, template) => template,
        },
      })
    );
    expect((await service.get('mac')).accounts[0].windows).toHaveLength(2);
    now += 100;
    expect((await service.peek('mac')).accounts[0].windows.map((window) => window.key)).toEqual([
      'five_hour',
    ]);
    expect(collections).toBe(1);
  });

  it('expires previously fresh Claude optional balances when the dashboard serves the whole row from cache', async () => {
    let now = NOW;
    let collections = 0;
    const sampledAt = new Date(NOW - 86_400_000 + 10).toISOString();
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        failover: {
          peek: (_, template) => template,
          collect: async (_, template) => {
            collections++;
            const row = quota(template);
            return {
              ...row,
              sampledAt,
              fetchedAt: sampledAt,
              windows: [
                ...row.windows,
                { ...row.windows[0], key: 'prepaid_balance', label: 'Prepaid balance' },
              ],
            };
          },
          recover: async (_, template) => template,
        },
      })
    );
    const initial = (await service.get('mac')).accounts[0];
    expect(initial.status).toBe('ok');
    expect(initial.windows).toHaveLength(2);
    expect(initial.windows[1].status).toBeUndefined();
    now += 100;
    const cached = (await service.peek('mac')).accounts[0];
    expect(cached.status).toBe('cached');
    expect(cached.windows.map((window) => window.key)).toEqual(['five_hour']);
    expect(cached.sampledAt).toBe(sampledAt);
    expect(cached.fetchedAt).toBe(sampledAt);
    expect(collections).toBe(1);
  });

  for (const summaryFailure of ['timeout', 'error'] as const) {
    for (const authChange of ['replaced', 'removed'] as const) {
      it(`revalidates cached Codex auth after it is ${authChange} when the summary returns ${summaryFailure}`, async () => {
        const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dashboard-codex-guard-'));
        try {
          const ccs = path.join(temporary, '.ccs');
          const authFile = path.join(ccs, 'codex-instances', 'saved-fixture', 'auth.json');
          const jwt = (value: unknown) =>
            `${Buffer.from('{"alg":"none"}').toString('base64url')}.${Buffer.from(JSON.stringify(value)).toString('base64url')}.fixture`;
          const auth = (person = 'fixture-person') => ({
            tokens: {
              access_token: 'fixture-private-access',
              account_id: 'fixture-workspace',
              id_token: jwt({
                email: 'fixture@example.test',
                sub: person,
                iss: 'https://auth.openai.com',
                'https://api.openai.com/auth': {
                  chatgpt_account_id: 'fixture-workspace',
                  chatgpt_user_id: person,
                },
              }),
            },
          });
          fs.mkdirSync(path.dirname(authFile), { recursive: true });
          fs.writeFileSync(authFile, JSON.stringify(auth()));
          const summary: CodexAuthProfilesSummary = {
            active: null,
            activated: null,
            default: 'saved-fixture',
            profiles: [
              {
                name: 'saved-fixture',
                email: 'fixture@example.test',
                plan: 'pro',
                accountId: 'fixture-workspace',
                codexHome: path.dirname(authFile),
                lastUsed: null,
                authValid: true,
              },
            ],
          };
          let unavailableSummary = false;
          const service = new AccountDashboardService(
            deps({
              scope: () => ccs,
              responseBudgetMs: 5,
              listClaudeProfiles: async () => [],
              getCodexSummary: async () => {
                if (!unavailableSummary) return summary;
                if (summaryFailure === 'error') throw new Error('Fixture summary unavailable.');
                return new Promise<CodexAuthProfilesSummary>(() => {});
              },
              getCodexRows: async () => [],
              failover: {
                peek: (_, template) => template,
                collect: async (_, template) => template,
                recover: async (_, template) => quota(template),
              },
            })
          );
          const initial = (await service.get('mac')).accounts[0];
          expect(initial.windows).toHaveLength(1);
          expect(initial.source).toBe('Verified quota on Nas1 (fallback)');
          if (authChange === 'removed') fs.unlinkSync(authFile);
          else fs.writeFileSync(authFile, JSON.stringify(auth('different-person')));
          unavailableSummary = true;
          const current = (await service.peek('mac')).accounts[0];
          expect(current).toMatchObject({
            id: 'codex:saved-fixture',
            status: 'unavailable',
            email: null,
            plan: null,
            sampledAt: null,
            fetchedAt: null,
            windows: [],
          });
          expect(current.message).toContain('saved Codex login changed');
        } finally {
          fs.rmSync(temporary, { recursive: true, force: true });
        }
      });
    }
  }

  for (const platform of ['mac', 'windows'] as const) {
    it(`preserves the ${platform} launcher while verified quota comes from Nas1`, async () => {
      const targets: UsageFailoverTarget[] = [];
      let legacyCalls = 0;
      const service = new AccountDashboardService(
        deps({
          getClaudeUsage: async () => {
            legacyCalls++;
            throw new Error('unexpected legacy collector');
          },
          failover: {
            peek: (_, template) => template,
            collect: async (target, template) => {
              targets.push(target);
              return quota(template);
            },
            recover: async () => {
              throw new Error('No saved Codex profiles should require recovery.');
            },
          },
        })
      );
      const dashboard = await service.get(platform, true);
      const row = dashboard.accounts.find((account) => account.id === 'claude:fixture-person');
      expect(legacyCalls).toBe(0);
      expect(targets).toHaveLength(1);
      expect(targets[0]).toMatchObject({
        accountId: 'claude:fixture-person',
        provider: 'claude',
        ccsDir: SCOPE,
        expectedEmail: profile.email,
        binding: JSON.stringify(profile),
        primarySources: [
          {
            host: 'windows',
            platform: 'windows',
            sshHost: 'fixture-windows',
            selector: 'claude-desktop-profile:fixture-person',
          },
        ],
      });
      expect(row).toMatchObject({
        platform,
        host: 'nas1',
        source: 'Verified quota on Nas1 (fallback)',
        status: 'ok',
        plan: 'max',
        sampledAt: SAMPLE,
        capabilities: {
          codexProfile: null,
          claudeProfileId: 'fixture-person',
          claudePlatforms: ['mac', 'windows'],
        },
      });
      expect(row?.windows[0].usedPercent).toBe(0);
      expect(JSON.stringify(dashboard)).not.toContain('fixture-windows');
      expect(JSON.stringify(dashboard)).not.toContain('private/fixture-profile');
    });
  }

  for (const windows of [
    { ...profile.windows!, isDefault: true },
    { ...profile.windows!, profilePath: 'C:\\fixture\\AppData\\Roaming\\Claude' },
  ])
    it('uses the desktop default selector without changing the named dashboard account', async () => {
      const namedDefault = { ...profile, windows };
      let selected: UsageFailoverTarget | undefined;
      const service = new AccountDashboardService(
        deps({
          listClaudeProfiles: async () => [namedDefault],
          failover: {
            peek: (_, template) => template,
            collect: async (target, template) => {
              selected = target;
              return quota(template);
            },
            recover: async (_, template) => template,
          },
        })
      );
      const dashboard = await service.get('mac');
      expect(selected?.primarySources[0].selector).toBe('claude-desktop-profile:default');
      expect(selected?.expectedEmail).toBe(profile.email);
      expect(selected?.primarySources[0].claudeProfile).toEqual({
        id: profile.id!,
        email: profile.email,
        profilePath: windows.profilePath,
        isDefault: true,
      });
      expect(dashboard.accounts[0].id).toBe('claude:fixture-person');
      expect(dashboard.accounts[0].capabilities.claudeProfileId).toBe('fixture-person');
    });

  it('merges Mac sign-out with Windows sign-in requirements without discarding cached quota', async () => {
    const service = new AccountDashboardService(
      deps({
        getClaudeMacSignIn: async () => 'signed-out',
        failover: {
          peek: (_, template) => template,
          collect: async (_, template) => ({
            ...quota(template, 'cached'),
            signInNeeded: ['windows'],
          }),
          recover: async (_, template) => template,
        },
      })
    );
    const row = (await service.get('mac')).accounts[0];
    expect(row.signInNeeded).toEqual(['mac', 'windows']);
    expect(row.status).toBe('cached');
    expect(row.message).toContain('Sign-in needed on Mac and Windows');
    expect(row.sampledAt).toBe(SAMPLE);
    expect(row.windows[0].usedPercent).toBe(0);
    expect(row.capabilities.claudePlatforms).toEqual(['mac', 'windows']);
  });

  it('keeps cached quota visible while an asynchronous shared refresh is still pending', async () => {
    let resolve!: (row: DashboardAccount) => void;
    let template!: DashboardAccount;
    const pending = new Promise<DashboardAccount>((done) => {
      resolve = done;
    });
    const service = new AccountDashboardService(
      deps({
        responseBudgetMs: 5,
        failover: {
          peek: (_, placeholder) => quota(placeholder, 'cached'),
          collect: async (_, placeholder) => {
            template = placeholder;
            return pending;
          },
          recover: async (_, placeholder) => placeholder,
        },
      })
    );
    const first = await service.get('mac');
    expect(first.accounts[0]).toMatchObject({ status: 'cached', sampledAt: SAMPLE });
    resolve(quota(template));
    await pending;
    await new Promise<void>((done) => setTimeout(done, 0));
    const later = await service.peek('mac');
    expect(later.accounts[0].sampledAt).toBe(SAMPLE);
    expect(later.accounts[0].windows[0].usedPercent).toBe(0);
  });
});
