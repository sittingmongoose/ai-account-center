import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createHash } from 'crypto';
import type { CodexAuthProfilesSummary } from '../../../src/codex-auth/codex-auth-dashboard-service';
import { CODEX_RENEWAL_MESSAGES } from '../../../src/codex-auth/codex-renewal-types';
import { renewalEntry } from './codex-renewal-entry-fixture';
import type { BarSummaryRow } from '../../../src/web-server/services/account-dashboard-types';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import {
  readAccountVisibility,
  replaceVisibilityAccountIds,
  writeAccountVisibility,
} from '../../../src/web-server/services/account-visibility';
import { claimFileLease } from '../../../src/shared/private-file-lease';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import { antigravityPlanDisplay } from '../../../src/antigravity/plan';
import type { AntigravityDashboardAccount } from '../../../src/antigravity/usage-contract';
import {
  listClaudeDesktopProfiles,
  type ClaudeDesktopProfile,
} from '../../../src/web-server/services/claude-desktop-profile-service';
import {
  ClaudeDesktopLiveUsageError,
  ClaudeDesktopSignInNeededError,
  getCachedClaudeDesktopLiveUsage,
  type ClaudeDesktopLiveUsage,
} from '../../../src/web-server/services/claude-desktop-live-service';
import { writeClaudeDesktopLiveSnapshot } from '../../../src/web-server/services/claude-desktop-live-cache';

function liveClaude(profileId: string): ClaudeDesktopLiveUsage {
  return {
    profileId,
    email: `${profileId}@example.com`,
    platform: 'windows',
    source: 'Claude Desktop live quota on Windows',
    plan: 'max',
    fetchedAt: '2026-10-01T12:00:00Z',
    windows: [
      {
        key: 'seven_day',
        label: 'Weekly usage',
        usedPercent: 125.5,
        remainingPercent: 0,
        resetAt: '2026-10-02T12:00:00Z',
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
      },
      {
        key: 'prepaid_balance',
        label: 'Prepaid balance',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
        unit: 'USD',
        kind: 'balance',
        remaining: 50,
        expiresAt: '2026-10-29T12:00:00Z',
      },
    ],
  };
}

function summary(active = 'gmail'): CodexAuthProfilesSummary {
  return {
    active: { name: 'party', source: 'default', codexHome: '/private-default-path' },
    activated: {
      name: active,
      email: `${active}@example.com`,
      plan: 'pro',
      codexHome: '/private-live-path',
    },
    default: 'party',
    profiles: ['gmail', 'party', 'lime'].map((name) => ({
      name,
      email: `${name}@example.com`,
      plan: 'pro',
      accountId: 'private-workspace-sentinel',
      codexHome: '/private-saved-path',
      lastUsed: null,
      authValid: true,
    })),
  };
}

function row(profile: string, overrides: Partial<BarSummaryRow> = {}): BarSummaryRow {
  return {
    profile,
    provider: 'codex',
    account_id: 'private-quota-account-sentinel',
    displayName: profile,
    tier: 'pro',
    paused: false,
    quota_percentage: 70,
    quotaStatus: 'ok',
    next_reset: null,
    is_default: profile === 'party',
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: '2026-10-01T12:00:00Z',
    needsReauth: false,
    quotaSource: 'network',
    quotaWindows: [
      {
        key: 'five_hour',
        label: '5h',
        usedPercent: 30,
        remainingPercent: 70,
        resetAt: '2026-10-01T15:00:00Z',
        windowMinutes: 300,
      },
    ],
    ...overrides,
  };
}

const claudeProfiles: ClaudeDesktopProfile[] = ['plum', 'gmail', 'party', 'me'].map((id) => ({
  id,
  email: `${id}@example.com`,
  mac: {
    launcherName: 'Mac',
    sshHost: 'private-host-sentinel',
    launcherPath: '/private-app.app',
    profilePath: '/private-profile-path',
  },
  windows: { launcherName: 'Windows', profilePath: 'C:\\private-profile-path' },
}));

function additional(provider: DashboardAccount['provider'] = 'cursor'): DashboardAccount {
  return {
    id: `${provider}:usage`,
    provider,
    providerLabel: 'ignored label',
    label: 'Personal',
    email: 'cursor@example.com',
    plan: 'Pro',
    platform: 'mac',
    source: 'Cursor on Mac',
    status: 'ok',
    message: null,
    fetchedAt: '2026-10-01T12:00:00Z',
    sampledAt: null,
    isActive: true,
    windows: [
      {
        key: 'monthly',
        label: 'Monthly',
        usedPercent: 20,
        remainingPercent: 80,
        resetAt: null,
        windowMinutes: null,
        used: 20,
        limit: 100,
        unit: 'requests',
      },
    ],
    capabilities: {
      codexProfile: 'malicious-profile',
      claudeProfileId: 'malicious-claude',
      claudePlatforms: ['mac'],
    },
  };
}

function deps(overrides: AccountDashboardDeps = {}): AccountDashboardDeps {
  return {
    getCodexSummary: async () => summary(),
    getCodexRows: async (names) => names.map((name) => row(name)),
    getCachedCodexRows: () => [],
    listClaudeProfiles: async () => claudeProfiles,
    getClaudeUsage: async (platform) => ({
      platform,
      fetchedAt: '2026-10-01T12:00:00Z',
      profiles: claudeProfiles.map((profile) => ({
        id: profile.id,
        email: profile.email,
        status: 'cached',
        cached: true,
        fetchedAt: '2026-10-01T12:00:00Z',
        sampledAt: '2026-10-01T11:00:00Z',
        utilization: { fiveHour: 0, weekly: 43 },
      })),
    }),
    getAdditionalAccounts: async () => [additional()],
    getLiveClaudeUsage: async () => null,
    invalidateClaudeCache: () => {},
    scope: () => 'fixture-scope',
    refreshIntervalSeconds: () => 60,
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

describe('Antigravity coding plan dashboard JSON', () => {
  it('carries the optional sample-time plan contract to registered accounts and cached tray responses', async () => {
    const observed = '2026-12-01T00:00:00Z';
    const account: AntigravityDashboardAccount = {
      ...additional('antigravity'),
      id: 'antigravity:profile:fixture',
      email: 'fixture@example.com',
      platform: 'ubuntu',
      ...antigravityPlanDisplay('Google AI Pro (trial)', observed),
      fetchedAt: observed,
      sampledAt: observed,
      capabilities: {
        codexProfile: null,
        claudeProfileId: null,
        claudePlatforms: [],
        antigravityProfileId: 'fixture',
        antigravityHostIds: ['ubuntu'],
      },
      windows: [
        {
          ...additional().windows[0],
          key: 'google-ai-credits',
          label: 'Google AI credits',
          kind: 'balance',
          remaining: 12,
          unit: 'credits',
          usedPercent: null,
          remainingPercent: null,
          used: null,
          limit: null,
        },
      ],
    };
    Object.assign(account.antigravityPlan!, { private: 'private-plan-sentinel' });
    const service = new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => true,
        getAntigravityAccounts: async () => [account],
        getCachedAntigravityAccounts: () => [account],
        readSelectedAntigravityProfileId: async () => null,
        now: () => Date.parse(observed),
      })
    );
    const result = await service.get();
    const ag = result.accounts.find((row) => row.provider === 'antigravity');
    expect(ag?.antigravityPlan).toMatchObject({
      class: 'pro-trial',
      quotaPolicy: 'five-hour-weekly',
      thirdPartyModels: false,
      creditsOverage: false,
    });
    expect(ag?.antigravityPlan?.models).not.toContain('GPT-OSS-120B');
    expect(ag?.windows[0]).toMatchObject({
      key: 'google-ai-credits',
      label: 'AI credits (overage)',
      remaining: 12,
      limit: null,
    });
    expect(JSON.stringify(ag)).not.toContain('private-plan-sentinel');
    expect(
      (await service.get()).accounts.find((row) => row.id === account.id)?.antigravityPlan
    ).toEqual(ag?.antigravityPlan);
  });

  it('normalizes legacy collector rows while unknown tiers remain optional and unchanged', async () => {
    const known = {
      ...additional('antigravity'),
      plan: 'free-tier',
      sampledAt: '2026-11-01T23:59:59Z',
    };
    const unknown = { ...known, id: 'antigravity:unknown', plan: 'standard-tier' };
    const result = await new AccountDashboardService(
      deps({
        hasAntigravityProfiles: () => false,
        getAdditionalAccounts: async () => [known, unknown],
      })
    ).get();
    expect(result.accounts.find((row) => row.id === known.id)).toMatchObject({
      plan: 'Free',
      antigravityPlan: { class: 'free', thirdPartyModels: true },
    });
    expect(result.accounts.find((row) => row.id === unknown.id)?.plan).toBe('standard-tier');
    expect(result.accounts.find((row) => row.id === unknown.id)?.antigravityPlan).toBeUndefined();
  });
});

describe('consolidated account dashboard', () => {
  it('keeps all seven primary usage rows responsive while an optional wallet is slow', async () => {
    let release: (value: DashboardAccount[]) => void = () => {};
    const wallet = new Promise<DashboardAccount[]>((resolve) => {
      release = resolve;
    });
    const service = new AccountDashboardService(
      deps({
        responseBudgetMs: 10,
        getAdditionalAccounts: async () => [additional('opencode-go')],
        getOptionalWalletAccounts: async () => wallet,
      })
    );
    const result = await service.get();
    expect(result.accounts).toHaveLength(14);
    expect(
      result.accounts.find((account) => account.id === 'opencode-go:usage')?.windows[0].usedPercent
    ).toBe(20);
    // REVIEW-cheap M1 (usage-cached): the primary read finished, so it stays current.
    const primary = result.accounts.find((account) => account.id === 'opencode-go:usage');
    expect(primary?.status).toBe('ok');
    expect(primary?.cacheReason).toBeUndefined();
    release([]);
    await wallet;
  });
  it('appends an optional console wallet separately from the OpenCode API-key account', async () => {
    const primary = additional('opencode-go');
    const wallet: DashboardAccount = {
      ...additional('opencode-go'),
      id: 'plan-opencode-go-console-mac-012345abcdef',
      email: null,
      label: 'OpenCode console wallet',
      source: 'Authenticated OpenCode console workspace on Mac',
      windows: [
        {
          key: 'zen-balance',
          label: 'Zen balance',
          kind: 'balance',
          remaining: -2.5,
          usedPercent: null,
          remainingPercent: null,
          resetAt: null,
          windowMinutes: null,
          used: null,
          limit: null,
          unit: 'USD',
        },
      ],
    };
    const result = await new AccountDashboardService(
      deps({ getAdditionalAccounts: async () => [wallet, primary] })
    ).get();
    expect(result.accounts).toHaveLength(15);
    const rows = result.accounts.filter((account) => account.provider === 'opencode-go');
    expect(rows.map((account) => account.id)).toEqual(['opencode-go:usage', wallet.id]);
    expect(rows[0].email).toBe(primary.email);
    expect(rows[0].windows).toHaveLength(1);
    expect(rows[1].email).toBeNull();
    expect(rows[1].windows[0].remaining).toBe(-2.5);
    expect(rows[1].capabilities.codexProfile).toBeNull();
  });
  it('returns three saved Codex accounts, four selected Claude accounts and seven usage providers', async () => {
    // The fixture readings are current: the Codex five-hour reset is still ahead.
    const result = await new AccountDashboardService(
      deps({ now: () => Date.parse('2026-10-01T12:30:00Z') })
    ).get();
    expect(result.schemaVersion).toBe(1);
    expect(result.accounts).toHaveLength(14);
    expect(result.accounts.filter((account) => account.provider === 'codex')).toHaveLength(3);
    expect(result.accounts.filter((account) => account.provider === 'claude')).toHaveLength(4);
    expect(
      result.accounts.filter((account) => account.isActive).map((account) => account.id)
    ).toEqual(['codex:gmail']);
    expect(result.codexAutoSwitch.enabled).toBe(false);
    const codex = result.accounts[0];
    expect(codex.windows[0]).toEqual({
      key: 'five_hour',
      label: '5h',
      usedPercent: 30,
      remainingPercent: 70,
      resetAt: '2026-10-01T15:00:00.000Z',
      windowMinutes: 300,
      used: null,
      limit: null,
      unit: null,
    });
    expect(codex.capabilities.codexProfile).toBe('gmail');
    const claude = result.accounts[3];
    expect(claude.platform).toBe('mac');
    expect(claude.status).toBe('cached');
    expect(claude.windows[0].usedPercent).toBe(0);
    expect(claude.windows.every((window) => window.resetAt === null)).toBe(true);
    expect(claude.capabilities.claudePlatforms).toEqual(['mac', 'windows']);
    const cursor = result.accounts.find((account) => account.provider === 'cursor');
    expect(cursor?.providerLabel).toBe('Cursor');
    expect(cursor?.isActive).toBe(false);
    expect(cursor?.capabilities).toEqual({
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
    });
    const serialized = JSON.stringify(result);
    for (const secret of [
      'private-workspace',
      'private-quota',
      'private-host',
      'private-profile',
      'private-live',
      'private-default',
      'private-saved',
      'malicious-profile',
      'malicious-claude',
    ]) {
      expect(serialized).not.toContain(secret);
    }
  });

  it('selects Claude platform independently without duplicating their four cards', async () => {
    const seen: string[] = [];
    const base = deps();
    const service = new AccountDashboardService(
      deps({
        getClaudeUsage: async (platform) => {
          seen.push(platform);
          return base.getClaudeUsage!(platform);
        },
      })
    );
    await service.get('mac');
    const windows = await service.get('windows');
    expect(seen).toEqual(['mac', 'windows']);
    expect(
      windows.accounts
        .filter((account) => account.provider === 'claude')
        .map((account) => account.platform)
    ).toEqual(['windows', 'windows', 'windows', 'windows']);
  });

  it('keeps per-account auth failures separate and never fills unknown usage with zero', async () => {
    const broken = summary();
    broken.profiles[1].authValid = false;
    const result = await new AccountDashboardService(
      deps({
        getCodexSummary: async () => broken,
        getCodexRows: async () => [
          row('gmail'),
          row('party'),
          row('lime', { quotaStatus: 'error', needsReauth: true }),
        ],
        getClaudeUsage: async (platform) => ({
          platform,
          fetchedAt: '2026-10-01T12:00:00Z',
          profiles: [
            {
              id: 'plum',
              email: 'plum@example.com',
              status: 'needs-sign-in',
              cached: true,
              fetchedAt: '2026-10-01T12:00:00Z',
              sampledAt: null,
              utilization: {},
            },
          ],
        }),
      })
    ).get();
    expect(result.accounts[0].status).toBe('ok');
    expect(result.accounts[1].status).toBe('needs_sign_in');
    expect(result.accounts[1].windows).toEqual([]);
    expect(result.accounts[2].status).toBe('needs_sign_in');
    expect(result.accounts[3].status).toBe('needs_sign_in');
    expect(result.accounts[4].status).toBe('unavailable');
    expect(result.accounts.find((account) => account.provider === 'qwen')?.windows).toEqual([]);
  });

  it('isolates a failing remote source from healthy sources and sanitizes thrown errors', async () => {
    const result = await new AccountDashboardService(
      deps({
        getClaudeUsage: async () => {
          throw new Error('private SSH host and password sentinel');
        },
        getAdditionalAccounts: async () => {
          throw new Error('raw credential response sentinel');
        },
      })
    ).get();
    expect(result.accounts).toHaveLength(14);
    expect(result.accounts[0].status).toBe('ok');
    expect(result.accounts[3].status).toBe('unavailable');
    expect(
      result.accounts
        .filter((account) => account.provider !== 'codex')
        .every((account) => account.windows.length === 0)
    ).toBe(true);
    expect(JSON.stringify(result)).not.toContain('sentinel');
  });

  it('coalesces concurrent requests, debounces refresh and passes force only when eligible', async () => {
    let now = 100_000;
    const refreshes: boolean[] = [];
    let invalidations = 0;
    let additionalCalls = 0;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        getCodexRows: async (names, refresh) => {
          refreshes.push(refresh);
          return names.map((name) => row(name));
        },
        getAdditionalAccounts: async () => {
          additionalCalls += 1;
          return [additional()];
        },
        invalidateClaudeCache: () => {
          invalidations += 1;
        },
      })
    );
    await Promise.all([service.get('mac', true), service.get('mac', true), service.get('mac')]);
    expect(refreshes).toEqual([true]);
    expect(additionalCalls).toBe(1);
    expect(invalidations).toBe(1);
    now += 1000;
    await service.get('mac', true);
    expect(refreshes).toEqual([true]);
    now += 5000;
    await service.get('mac', true);
    expect(refreshes).toEqual([true, true]);
    now += 60_001;
    await service.get('mac');
    expect(refreshes).toEqual([true, true, true]);
  });

  it('uses the configured cadence for actual source checks and permits manual refresh sooner', async () => {
    let now = 100_000;
    let interval = 120;
    let calls = 0;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        refreshIntervalSeconds: () => interval,
        getCodexRows: async (names, force) => {
          expect(force).toBe(true);
          calls++;
          return names.map((name) => row(name));
        },
      })
    );
    expect((await service.get()).settings).toEqual({
      refreshIntervalSeconds: 120,
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: [],
      trayHiddenAccountIds: [],
      visibilityAvailable: true,
    });
    now += 60_000;
    await service.get();
    expect(calls).toBe(1);
    await service.get('mac', true);
    expect(calls).toBe(2);
    now += 30_000;
    interval = 30;
    expect((await service.get()).settings?.refreshIntervalSeconds).toBe(30);
    expect(calls).toBe(3);
  });

  it('refreshes the active account while cached quota samples are retained', async () => {
    let active = 'gmail';
    let rowCalls = 0;
    const service = new AccountDashboardService(
      deps({
        getCodexSummary: async () => summary(active),
        getCodexRows: async (names) => {
          rowCalls += 1;
          return names.map((name) => row(name));
        },
      })
    );
    await service.get();
    active = 'party';
    const updated = await service.get();
    expect(rowCalls).toBe(1);
    expect(
      updated.accounts.filter((account) => account.isActive).map((account) => account.id)
    ).toEqual(['codex:party']);
  });

  it('responds within its budget and retains late remote results for the next poll', async () => {
    let release: (accounts: DashboardAccount[]) => void = () => {};
    const pending = new Promise<DashboardAccount[]>((resolve) => {
      release = resolve;
    });
    const service = new AccountDashboardService(
      deps({ responseBudgetMs: 10, getAdditionalAccounts: () => pending })
    );
    const started = Date.now();
    const first = await service.get();
    expect(Date.now() - started).toBeLessThan(100);
    expect(first.accounts).toHaveLength(14);
    expect(first.accounts.find((account) => account.provider === 'cursor')?.status).toBe(
      'unavailable'
    );
    release([additional()]);
    await pending;
    await Promise.resolve();
    const next = await service.get();
    expect(next.accounts.find((account) => account.provider === 'cursor')?.status).toBe('ok');
  });

  it('bounds a slow live identity check instead of doubling the response budget', async () => {
    const never = new Promise<CodexAuthProfilesSummary>(() => {});
    const service = new AccountDashboardService(
      deps({ responseBudgetMs: 10, getCodexSummary: () => never })
    );
    const started = Date.now();
    const result = await service.get();
    expect(Date.now() - started).toBeLessThan(100);
    expect(result.accounts.filter((account) => account.provider !== 'codex')).toHaveLength(11);
    expect(result.accounts.some((account) => account.isActive)).toBe(false);
  });

  it('strips unexpected fields from additional providers and keeps unknown values null', async () => {
    const account = additional();
    Object.assign(account, {
      accessToken: 'credential-sentinel',
      sshHost: 'private-host-sentinel',
    });
    Object.assign(account.windows[0], {
      resetAt: 'invalid',
      usedPercent: NaN,
      remainingPercent: null,
      used: -1,
      limit: Infinity,
      rawResponse: 'upstream-sentinel',
    });
    const result = await new AccountDashboardService(
      deps({ getAdditionalAccounts: async () => [account] })
    ).get();
    const cursor = result.accounts.find((entry) => entry.provider === 'cursor');
    expect(cursor?.windows[0]).toMatchObject({
      usedPercent: null,
      remainingPercent: null,
      resetAt: null,
      used: null,
      limit: null,
    });
    expect(JSON.stringify(result)).not.toContain('sentinel');
  });

  it('keeps cached results isolated by CCS scope', async () => {
    let scope = 'first';
    let calls = 0;
    const service = new AccountDashboardService(
      deps({
        scope: () => scope,
        getAdditionalAccounts: async () => {
          calls += 1;
          return [additional()];
        },
      })
    );
    await service.get();
    scope = 'second';
    await service.get();
    scope = 'first';
    await service.get();
    expect(calls).toBe(2);
  });

  it('tells a new profile the installed collector needs an update instead of sign-in', async () => {
    const added: ClaudeDesktopProfile = {
      id: 'added-profile',
      email: 'added@example.com',
      mac: {
        launcherName: 'Mac',
        sshHost: 'fixture-mac',
        launcherPath: '/Applications/Fixture.app',
        profilePath: '/fixture/profile',
      },
      windows: {
        launcherName: 'Windows',
        sshHost: 'fixture-windows',
        profilePath: 'C:\\fixture\\profile',
      },
    };
    const result = await new AccountDashboardService(
      deps({
        listClaudeProfiles: async () => [added],
        getClaudeUsage: async (platform) => ({
          platform,
          fetchedAt: '2026-10-01T12:00:00Z',
          profiles: [],
        }),
        getLiveClaudeUsage: async () => {
          throw new ClaudeDesktopLiveUsageError(true);
        },
      })
    ).get('mac');
    const row = result.accounts.find((account) => account.id === 'claude:added-profile');
    expect(row?.status).toBe('unavailable');
    expect(row?.message).toBe('Update the usage helper on Windows.');
    expect(row?.windows).toEqual([]);
  });

  describe('Sign-in needed before Open (fake profiles only)', () => {
    const fake: ClaudeDesktopProfile = {
      id: 'fake-one',
      email: 'fake-one@example.com',
      mac: {
        launcherName: 'Mac',
        sshHost: 'fixture-mac',
        launcherPath: '/Applications/Fixture.app',
        profilePath: '/Users/fixture/Library/Application Support/Claude-fake-one',
      },
      windows: {
        launcherName: 'Windows',
        sshHost: 'fixture-windows',
        profilePath: 'C:\\fixture\\Claude-fake-one',
      },
    };
    const history = async (platform: 'mac' | 'windows') => ({
      platform,
      fetchedAt: '2026-10-01T12:00:00Z',
      profiles: [],
    });

    it('says Windows needs a sign-in when the usage helper says so, and keeps Open', async () => {
      const result = await new AccountDashboardService(
        deps({
          listClaudeProfiles: async () => [fake],
          getClaudeUsage: history,
          getLiveClaudeUsage: async () => {
            throw new ClaudeDesktopSignInNeededError();
          },
          getClaudeMacSignIn: async () => 'signed-in',
        })
      ).get('mac');
      const row = result.accounts.find((account) => account.id === 'claude:fake-one');
      expect(row?.signInNeeded).toEqual(['windows']);
      expect(row?.status).toBe('needs_sign_in');
      // No login reads the account, so the prompt names the control that signs in.
      expect(row?.message).toBe(
        "No working Claude sign-in for this account on Windows. To sign in, use Open on Windows in Details: it opens this account's Claude Desktop, which shows its sign-in window."
      );
      expect(row?.capabilities.claudePlatforms).toEqual(['mac', 'windows']);
    });

    it('says Mac and Windows when both computers need it; readings already shown stay', async () => {
      const result = await new AccountDashboardService(
        deps({
          listClaudeProfiles: async () => [fake],
          getClaudeUsage: async (platform) => ({
            platform,
            fetchedAt: '2026-10-01T12:00:00Z',
            profiles: [
              {
                id: 'fake-one',
                email: 'fake-one@example.com',
                status: 'cached',
                cached: true,
                fetchedAt: '2026-10-01T12:00:00Z',
                sampledAt: '2026-10-01T11:00:00Z',
                utilization: { fiveHour: 10, weekly: 20 },
              },
            ],
          }),
          getLiveClaudeUsage: async () => {
            throw new ClaudeDesktopSignInNeededError();
          },
          getClaudeMacSignIn: async () => 'signed-out',
        })
      ).get('mac');
      const row = result.accounts.find((account) => account.id === 'claude:fake-one');
      expect(row?.signInNeeded).toEqual(['mac', 'windows']);
      expect(row?.status).toBe('cached');
      expect(row?.windows.length).toBeGreaterThan(0);
      expect(row?.message).toContain(
        'No working Claude sign-in for this account on Mac and Windows. To sign in, use Open on Mac or Open on Windows in Details'
      );
    });

    it('treats an unreachable computer as unknown, never as needing a sign-in', async () => {
      const result = await new AccountDashboardService(
        deps({
          listClaudeProfiles: async () => [fake],
          getClaudeUsage: history,
          getLiveClaudeUsage: async () => null,
          getClaudeMacSignIn: async () => null,
        })
      ).get('mac');
      const row = result.accounts.find((account) => account.id === 'claude:fake-one');
      expect(row && 'signInNeeded' in row).toBe(false);
      expect(row?.status).toBe('unavailable');
    });

    it('clears Windows once a verified reading arrives', async () => {
      let signedIn = false;
      let clock = Date.parse('2026-10-01T12:00:00Z');
      const service = new AccountDashboardService(
        deps({
          now: () => clock,
          listClaudeProfiles: async () => [{ ...fake, email: 'fake-one@example.com' }],
          getClaudeUsage: history,
          getLiveClaudeUsage: async (profileId) => {
            if (!signedIn) throw new ClaudeDesktopSignInNeededError();
            return { ...liveClaude(profileId), email: 'fake-one@example.com' };
          },
          getClaudeMacSignIn: async () => 'signed-in',
        })
      );
      const first = await service.get('mac', true);
      expect(first.accounts.find((a) => a.id === 'claude:fake-one')?.signInNeeded).toEqual([
        'windows',
      ]);
      signedIn = true;
      clock += 10_000;
      const second = await service.get('mac', true);
      const row = second.accounts.find((a) => a.id === 'claude:fake-one');
      expect(row && 'signInNeeded' in row).toBe(false);
      expect(row?.status).toBe('ok');
    });
  });

  it('uses verified live Claude quota while keeping the selected launcher platform', async () => {
    const result = await new AccountDashboardService(
      deps({
        getLiveClaudeUsage: async (profileId) => ({
          profileId,
          email: profileId === 'gmail' ? 'gmail@example.com' : 'someone-else@example.com',
          platform: 'windows',
          plan: 'max',
          source: 'ignored helper source',
          fetchedAt: '2026-10-01T12:00:00Z',
          windows: [
            {
              key: 'seven_day',
              label: 'Weekly limit',
              usedPercent: 50,
              remainingPercent: 50,
              resetAt: '2026-10-02T12:00:00Z',
              windowMinutes: 10080,
              used: null,
              limit: null,
              unit: null,
            },
          ],
        }),
      })
    ).get('mac');
    const gmail = result.accounts.find((account) => account.id === 'claude:gmail');
    expect(gmail?.platform).toBe('mac');
    expect(gmail?.status).toBe('ok');
    expect(gmail?.source).toBe('Claude Desktop live quota on Windows');
    expect(gmail?.plan).toBe('max');
    expect(gmail?.windows[0].resetAt).toBe('2026-10-02T12:00:00.000Z');
    const plum = result.accounts.find((account) => account.id === 'claude:plum');
    expect(plum?.status).toBe('cached');
    expect(plum?.windows[0].usedPercent).toBe(0);
    expect(JSON.stringify(result)).not.toContain('someone-else');
  });

  it('carries dynamic keys, labels, minutes, clears and the session release untouched', async () => {
    const result = await new AccountDashboardService(
      deps({
        now: () => Date.parse('2026-10-01T12:00:00Z'),
        getLiveClaudeUsage: async (profileId) => ({
          profileId,
          email: `${profileId}@example.com`,
          platform: 'windows',
          plan: 'max',
          source: 'ignored helper source',
          fetchedAt: '2026-10-01T12:00:00Z',
          windows: [
            {
              key: 'seven_day_future_model',
              label: 'Weekly Future usage',
              usedPercent: 33,
              remainingPercent: 67,
              resetAt: '2026-10-08T12:00:00Z',
              windowMinutes: 10080,
              kind: 'rate_limit',
              used: null,
              limit: null,
              unit: null,
            },
            {
              key: 'reset_credit_used_grant_1',
              label: 'Used rate-limit reset grant 1',
              usedPercent: null,
              remainingPercent: null,
              resetAt: null,
              windowMinutes: null,
              used: 0,
              limit: 1,
              unit: 'resets',
              kind: 'balance',
              remaining: 1,
              expiresAt: '2026-10-22T16:00:00Z',
              clears: ['five_hour', 'seven_day'],
            },
            {
              key: 'session_reset_allowance',
              label: 'Weekly session reset',
              usedPercent: null,
              remainingPercent: null,
              resetAt: '2026-10-09T12:00:00Z',
              windowMinutes: null,
              used: 1,
              limit: 3,
              unit: 'resets',
              kind: 'balance',
              remaining: 2,
            },
          ],
        }),
      })
    ).get('mac');
    const gmail = result.accounts.find((account) => account.id === 'claude:gmail');
    expect(gmail?.status).toBe('ok');
    expect(gmail?.windows.map((window) => window.key)).toEqual([
      'seven_day_future_model',
      'reset_credit_used_grant_1',
      'session_reset_allowance',
    ]);
    expect(gmail?.windows[0]).toMatchObject({
      label: 'Weekly Future usage',
      windowMinutes: 10080,
      usedPercent: 33,
    });
    expect(gmail?.windows[1].clears).toEqual(['five_hour', 'seven_day']);
    expect(gmail?.windows[2]).toMatchObject({
      remaining: 2,
      unit: 'resets',
      resetAt: '2026-10-09T12:00:00.000Z',
    });
  });

  it('serves cached Claude usage during a slow live lookup and retains the verified result', async () => {
    let release: (value: null) => void = () => {};
    const pending = new Promise<null>((resolve) => {
      release = resolve;
    });
    const service = new AccountDashboardService(
      deps({ responseBudgetMs: 10, getLiveClaudeUsage: () => pending })
    );
    const result = await service.get();
    const claude = result.accounts.find((account) => account.id === 'claude:gmail');
    expect(claude?.status).toBe('cached');
    expect(claude?.windows[0].usedPercent).toBe(0);
    release(null);
    await pending;
  });

  it('publishes full live Claude quota before slow Mac history and never downgrades on its completion', async () => {
    let releaseHistory: () => void = () => {};
    const blocked = new Promise<void>((resolve) => {
      releaseHistory = resolve;
    });
    const history = deps().getClaudeUsage!;
    const service = new AccountDashboardService(
      deps({
        now: () => Date.parse('2026-10-01T12:00:00Z'),
        responseBudgetMs: 10,
        getClaudeUsage: async (platform) => {
          await blocked;
          return history(platform);
        },
        getLiveClaudeUsage: async (profileId) => liveClaude(profileId),
      })
    );
    const first = (await service.get('mac')).accounts.find(
      (account) => account.id === 'claude:gmail'
    );
    expect(first?.platform).toBe('mac');
    expect(first?.source).toBe('Claude Desktop live quota on Windows');
    expect(first?.windows).toHaveLength(2);
    expect(first?.windows[0].usedPercent).toBe(125.5);
    expect(first?.windows[0].resetAt).toBe('2026-10-02T12:00:00.000Z');
    expect(first?.windows[1].remaining).toBe(50);
    releaseHistory();
    await new Promise((resolve) => setTimeout(resolve, 0));
    const next = (await service.get('mac')).accounts.find(
      (account) => account.id === 'claude:gmail'
    );
    expect(next?.source).toBe(first?.source);
    expect(next?.sampledAt).toBe(first?.sampledAt);
    expect(next?.windows).toEqual(first?.windows);
  });

  it('cold Mac and Windows dashboards load all 22 persisted verified windows before sparse Mac history, then accept fresh zero/null updates', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-dashboard-cold-'));
    const previousCcsDir = process.env.CCS_DIR;
    process.env.CCS_DIR = root;
    let release: () => void = () => {};
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    try {
      fs.writeFileSync(
        path.join(root, 'claude-desktop-profiles.json'),
        JSON.stringify({
          version: 1,
          profiles: claudeProfiles.map((profile) => ({
            ...profile,
            windows: { ...profile.windows, sshHost: 'fixture-windows' },
          })),
        })
      );
      const profiles = await listClaudeDesktopProfiles();
      const sampledAt = new Date(Date.now() - 5 * 60_000).toISOString();
      const samples = profiles.map((profile, index): ClaudeDesktopLiveUsage => {
        const base = liveClaude(profile.id!);
        const rate = {
          ...base.windows[0],
          key: 'five_hour',
          label: 'Five-hour usage',
          usedPercent: 45,
          remainingPercent: 55,
          windowMinutes: 300,
        };
        const balance = {
          ...base.windows[1],
          key: 'reset_credits_available',
          label: 'Rate-limit resets available',
          remaining: 1,
          unit: 'resets',
          expiresAt: null,
        };
        const windows: ClaudeDesktopLiveUsage['windows'] = [
          rate,
          base.windows[0],
          {
            ...rate,
            key: 'extra_usage',
            label: 'Extra usage',
            kind: 'extra_usage',
            enabled: false,
            usedPercent: null,
            remainingPercent: null,
            resetAt: null,
            windowMinutes: null,
          },
          balance,
          {
            ...balance,
            key: 'reset_credit_available_grant_1',
            expiresAt: '2026-10-29T12:00:00Z',
          },
          ...(index < 2 ? [base.windows[1]] : []),
        ];
        return { ...base, fetchedAt: sampledAt, windows };
      });
      await Promise.all(
        profiles.map((profile, index) =>
          writeClaudeDesktopLiveSnapshot(
            root,
            profile.id!,
            createHash('sha256').update(JSON.stringify(profile)).digest('hex'),
            samples[index]
          )
        )
      );
      // Every persisted sample comes back through the real reader first. The service then gets
      // them from memory, so its 100 ms response budget races only promise work: under load a
      // disk read could outlast the budget and leave the rows empty (cold-dashboard-flake-T1),
      // which is timing, not the behaviour checked here.
      const persisted = new Map(
        await Promise.all(
          profiles.map(
            async (profile) =>
              [profile.id!, await getCachedClaudeDesktopLiveUsage(profile.id!)] as const
          )
        )
      );
      expect([...persisted.values()].map((sample) => sample?.windows.length)).toEqual([6, 6, 5, 5]);
      const currentAt = new Date().toISOString();
      const service = new AccountDashboardService(
        deps({
          scope: () => root,
          responseBudgetMs: 100,
          listClaudeProfiles: async () => profiles,
          getCachedLiveClaudeUsage: async (id) => persisted.get(id) ?? null,
          getLiveClaudeUsage: async (id) => {
            await blocked;
            const sample = samples.find((candidate) => candidate.profileId === id)!;
            return {
              ...sample,
              fetchedAt: currentAt,
              windows: sample.windows.slice(0, 2).map((window) => ({
                ...window,
                usedPercent: 0,
                remainingPercent: 100,
                resetAt: null,
              })),
            };
          },
        })
      );
      for (const platform of ['mac', 'windows'] as const) {
        const accounts = (await service.get(platform)).accounts.filter(
          (account) => account.provider === 'claude'
        );
        expect(accounts.map((account) => account.windows.length)).toEqual([6, 6, 5, 5]);
        expect(accounts.reduce((total, account) => total + account.windows.length, 0)).toBe(22);
        for (const account of accounts) {
          expect(account.platform).toBe(platform);
          expect(account.source).toBe('Claude Desktop live quota on Windows');
          expect(account.status).toBe('cached');
          expect(account.sampledAt).toBe(sampledAt);
          expect(account.capabilities.claudePlatforms).toEqual(['mac', 'windows']);
          expect(account.windows[1].usedPercent).toBe(125.5);
          expect(account.windows[1].resetAt).toBe('2026-10-02T12:00:00.000Z');
          expect(account.windows[2].enabled).toBe(false);
          expect(account.windows[4].expiresAt).toBe('2026-10-29T12:00:00.000Z');
        }
      }
      release();
      await new Promise((resolve) => setTimeout(resolve, 0));
      const updated = (await service.get('mac')).accounts.filter(
        (account) => account.provider === 'claude'
      );
      expect(updated.map((account) => account.windows.length)).toEqual([2, 2, 2, 2]);
      for (const account of updated) {
        expect(account.source).toBe('Claude Desktop live quota on Windows');
        expect(account.sampledAt).toBe(currentAt);
        expect(account.windows.map((window) => [window.usedPercent, window.resetAt])).toEqual([
          [0, null],
          [0, null],
        ]);
      }
    } finally {
      release();
      if (previousCcsDir === undefined) delete process.env.CCS_DIR;
      else process.env.CCS_DIR = previousCcsDir;
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('shares matching verified Claude quota across launcher platforms and retains it through offline refreshes', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let online = true;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        getLiveClaudeUsage: async (profileId) => (online ? liveClaude(profileId) : null),
      })
    );
    const windows = (await service.get('windows')).accounts.find(
      (account) => account.id === 'claude:gmail'
    );
    online = false;
    const mac = (await service.get('mac')).accounts.find(
      (account) => account.id === 'claude:gmail'
    );
    expect(mac?.platform).toBe('mac');
    expect(mac?.source).toBe('Claude Desktop live quota on Windows');
    expect(mac?.status).toBe('cached');
    expect(mac?.windows).toEqual(windows?.windows);
    now += 61_000;
    const offline = (await service.get('mac', true)).accounts.find(
      (account) => account.id === 'claude:gmail'
    );
    expect(offline?.status).toBe('cached');
    expect(offline?.sampledAt).toBe(mac?.sampledAt);
    expect(offline?.windows).toEqual(mac?.windows);
  });

  it('does not reuse retained Claude quota after its manifest source changes', async () => {
    let changed = false;
    const service = new AccountDashboardService(
      deps({
        listClaudeProfiles: async () =>
          claudeProfiles.map((profile) => ({
            ...profile,
            windows: {
              ...profile.windows!,
              sshHost: changed ? 'changed-source' : 'original-source',
            },
          })),
        getLiveClaudeUsage: async (profileId) => (changed ? null : liveClaude(profileId)),
      })
    );
    await service.get('windows');
    changed = true;
    const mac = (await service.get('mac')).accounts.find(
      (account) => account.id === 'claude:gmail'
    );
    expect(mac?.source).toBe('Claude desktop on Mac');
    expect(mac?.windows.every((window) => window.resetAt === null)).toBe(true);
    expect(mac?.windows.some((window) => window.kind === 'balance')).toBe(false);
  });

  it('suppresses cached Codex quota when the saved workspace or authentication changes', async () => {
    let workspace = 'first-workspace';
    let valid = true;
    const service = new AccountDashboardService(
      deps({
        getCodexSummary: async () => {
          const result = summary();
          result.profiles[0].accountId = workspace;
          result.profiles[0].authValid = valid;
          return result;
        },
      })
    );
    expect((await service.get()).accounts[0].windows).toHaveLength(1);
    workspace = 'different-workspace';
    const changed = await service.get();
    expect(changed.accounts[0].windows).toEqual([]);
    expect(changed.accounts[0].status).toBe('unavailable');
    valid = false;
    const invalid = await service.get();
    expect(invalid.accounts[0].status).toBe('needs_sign_in');
    expect(invalid.accounts[0].windows).toEqual([]);
  });

  it('preserves explicit balances, extra usage and expiration separately from quota resets', async () => {
    const account = additional();
    account.windows = [
      {
        key: 'credit_balance',
        label: 'Credit balance',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
        unit: 'credits',
        kind: 'balance',
        remaining: -2,
        expiresAt: '2026-12-31T12:00:00Z',
        unlimited: false,
      },
      {
        key: 'extra_usage',
        label: 'Extra usage',
        usedPercent: 10,
        remainingPercent: 90,
        resetAt: '2026-11-01T00:00:00Z',
        windowMinutes: null,
        used: 5,
        limit: 50,
        unit: null,
        kind: 'extra_usage',
        enabled: false,
        expiresAt: null,
      },
    ];
    const result = await new AccountDashboardService(
      deps({ getAdditionalAccounts: async () => [account] })
    ).get();
    const windows = result.accounts.find((entry) => entry.provider === 'cursor')?.windows;
    expect(windows?.[0]).toMatchObject({
      kind: 'balance',
      remaining: -2,
      expiresAt: '2026-12-31T12:00:00.000Z',
      resetAt: null,
      unlimited: false,
      usedPercent: null,
      remainingPercent: null,
    });
    expect(windows?.[1]).toMatchObject({
      kind: 'extra_usage',
      enabled: false,
      resetAt: '2026-11-01T00:00:00.000Z',
      expiresAt: null,
      unit: null,
    });
  });

  it('merges Codex credits with all reported quota windows without fabricating a credit gauge', async () => {
    const result = await new AccountDashboardService(
      deps({
        getCodexRows: async (names) =>
          names.map((name) => {
            const sample = row(name);
            sample.quotaWindows?.push({
              key: 'code_review_primary',
              label: 'Code review',
              usedPercent: 15,
              remainingPercent: 85,
              resetAt: '2026-10-08T12:00:00Z',
              windowMinutes: 10080,
            });
            sample.balanceWindows = [
              {
                key: 'credits',
                label: 'Credits',
                usedPercent: null,
                remainingPercent: null,
                resetAt: null,
                windowMinutes: null,
                kind: 'balance',
                remaining: 62500,
                unit: 'credits',
                expiresAt: null,
                unlimited: false,
              },
            ];
            return sample;
          }),
      })
    ).get();
    expect(result.accounts[0].windows).toHaveLength(3);
    expect(result.accounts[0].windows[1].key).toBe('code_review_primary');
    expect(result.accounts[0].windows[2]).toMatchObject({
      kind: 'balance',
      remaining: 62500,
      unit: 'credits',
      usedPercent: null,
      remainingPercent: null,
      resetAt: null,
      expiresAt: null,
    });
  });

  it('marks a persisted Claude reading whose reset passed after it was sampled, keeping it as history', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-dashboard-reset-'));
    const previousCcsDir = process.env.CCS_DIR;
    process.env.CCS_DIR = root;
    try {
      fs.writeFileSync(
        path.join(root, 'claude-desktop-profiles.json'),
        JSON.stringify({
          version: 1,
          profiles: claudeProfiles.map((profile) => ({
            ...profile,
            windows: { ...profile.windows, sshHost: 'fixture-windows' },
          })),
        })
      );
      const profiles = await listClaudeDesktopProfiles();
      const gmail = profiles.find((profile) => profile.id === 'gmail')!;
      const now = Date.now();
      const base = liveClaude('gmail');
      const sample: ClaudeDesktopLiveUsage = {
        ...base,
        // Read three hours ago; the five-hour window reset an hour ago.
        fetchedAt: new Date(now - 3 * 3_600_000).toISOString(),
        windows: [
          {
            ...base.windows[0],
            key: 'five_hour',
            label: 'Five-hour usage',
            usedPercent: 87.5,
            remainingPercent: 12.5,
            resetAt: new Date(now - 3_600_000).toISOString(),
            windowMinutes: 300,
          },
          { ...base.windows[0], resetAt: new Date(now + 3 * 86_400_000).toISOString() },
          base.windows[1],
        ],
      };
      await writeClaudeDesktopLiveSnapshot(
        root,
        'gmail',
        createHash('sha256').update(JSON.stringify(gmail)).digest('hex'),
        sample
      );
      const service = new AccountDashboardService(
        deps({
          scope: () => root,
          listClaudeProfiles: async () => profiles,
          getCachedLiveClaudeUsage: getCachedClaudeDesktopLiveUsage,
          // The live source is unreachable, so only the retained reading exists.
          getLiveClaudeUsage: async () => null,
        })
      );
      const account = (await service.get('mac')).accounts.find(
        (entry) => entry.id === 'claude:gmail'
      )!;
      expect(account.source).toBe('Claude Desktop live quota on Windows');
      expect(account.status).toBe('cached');
      expect(account.sampledAt).toBe(sample.fetchedAt);
      expect(account.windows[0]).toMatchObject({
        key: 'five_hour',
        usedPercent: 87.5,
        remainingPercent: 12.5,
        resetPassed: true,
      });
      expect(account.windows[1].usedPercent).toBe(125.5);
      expect(account.windows[1].resetPassed).toBeUndefined();
      expect(account.windows[2]).toMatchObject({ kind: 'balance', resetAt: null });
      expect(account.windows[2].resetPassed).toBeUndefined();
    } finally {
      if (previousCcsDir === undefined) delete process.env.CCS_DIR;
      else process.env.CCS_DIR = previousCcsDir;
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('recomputes reset marks on every response from each window or account sample time', async () => {
    let now = Date.parse('2026-10-01T14:59:59.999Z');
    let collections = 0;
    const window = (overrides: Partial<DashboardAccount['windows'][number]> = {}) => ({
      key: 'weekly',
      label: 'Weekly',
      usedPercent: 64,
      remainingPercent: 36,
      resetAt: '2026-10-01T15:00:00Z',
      windowMinutes: 10080,
      used: null,
      limit: null,
      unit: null,
      ...overrides,
    });
    const cursor: DashboardAccount = {
      ...additional('cursor'),
      sampledAt: '2026-10-01T15:30:00Z',
      windows: [
        // Retained from an older reading: its own sample time decides.
        window({ key: 'retained', status: 'cached', sampledAt: '2026-10-01T14:00:00Z' }),
        // Read with the account, after the reset: a current reading.
        window({ key: 'current' }),
      ],
    };
    const kimi: DashboardAccount = {
      ...additional('kimi-code'),
      sampledAt: null,
      windows: [window({ key: 'unknown-sample' })],
    };
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        refreshIntervalSeconds: () => 3600,
        getAdditionalAccounts: async () => {
          collections++;
          return [cursor, kimi];
        },
      })
    );
    const marks = async () => {
      const accounts = (await service.get()).accounts;
      return Object.fromEntries(
        accounts.flatMap((account) =>
          account.windows.map((entry) => [`${account.id}/${entry.key}`, entry.resetPassed ?? false])
        )
      );
    };
    expect(await marks()).toMatchObject({
      'codex:gmail/five_hour': false,
      'cursor:usage/retained': false,
      'cursor:usage/current': false,
      'kimi-code:usage/unknown-sample': false,
    });
    now = Date.parse('2026-10-01T15:00:00Z');
    expect(await marks()).toMatchObject({
      // Codex rows were read at 12:00, before their 15:00 reset.
      'codex:gmail/five_hour': true,
      'cursor:usage/retained': true,
      'cursor:usage/current': false,
      'kimi-code:usage/unknown-sample': true,
    });
    // The same cached collection served both responses.
    expect(collections).toBe(1);
    const kept = (await service.get()).accounts.find((account) => account.id === 'cursor:usage')!;
    expect(kept.windows[0]).toMatchObject({ usedPercent: 64, remainingPercent: 36 });
  });

  it('judges a stale Codex local reading by its session time, not by when it was fetched', async () => {
    const result = await new AccountDashboardService(
      deps({
        now: () => Date.parse('2026-10-01T15:10:00Z'),
        getCodexRows: async (names) =>
          names.map((name) =>
            name === 'gmail'
              ? // Local fallback read at 15:10 from a session file last written at 14:00,
                // before the 15:00 five-hour reset: history, not the current window.
                row(name, {
                  quotaSource: 'local',
                  health: 'warning',
                  fetchedAt: '2026-10-01T15:10:00Z',
                  staleAsOf: '2026-10-01T14:00:00Z',
                })
              : // A fresh local reading (no staleAsOf) after the reset is current.
                row(name, { quotaSource: 'local', fetchedAt: '2026-10-01T15:10:00Z' })
          ),
      })
    ).get();
    const codex = (name: string) =>
      result.accounts.find((account) => account.id === `codex:${name}`)!;
    expect(codex('gmail')).toMatchObject({
      fetchedAt: '2026-10-01T15:10:00.000Z',
      sampledAt: '2026-10-01T14:00:00.000Z',
    });
    expect(codex('gmail').windows[0]).toMatchObject({
      key: 'five_hour',
      usedPercent: 30,
      resetPassed: true,
    });
    expect(codex('party').sampledAt).toBe('2026-10-01T15:10:00.000Z');
    expect(codex('party').windows[0].resetPassed).toBeUndefined();
  });
});

describe('Codex saved-login renewal in the dashboard', () => {
  it('shows a rejected saved login as needing sign in and leaves the other logins alone', async () => {
    const result = await new AccountDashboardService(
      deps({
        getCodexSummary: async () => summary('lime'),
        getRenewalStatus: async () => ({ profiles: [renewalEntry('gmail', 'failed', 'dead')] }),
      })
    ).get();
    expect(result.accounts.find((account) => account.id === 'codex:gmail')).toMatchObject({
      status: 'needs_sign_in',
      message: CODEX_RENEWAL_MESSAGES.dead,
    });
    const party = result.accounts.find((account) => account.id === 'codex:party');
    expect(party?.status).not.toBe('needs_sign_in');
    expect(party?.message).toBeNull();
  });

  it('keeps the rows as they were when the renewal status cannot be read', async () => {
    const result = await new AccountDashboardService(
      deps({
        getCodexSummary: async () => summary('lime'),
        getRenewalStatus: async () => {
          throw new Error('unreadable');
        },
      })
    ).get();
    const gmail = result.accounts.find((account) => account.id === 'codex:gmail');
    expect(gmail?.status).not.toBe('needs_sign_in');
    expect(gmail?.message).toBeNull();
  });

  it('never adds a renewal note to the login Codex is using', async () => {
    const result = await new AccountDashboardService(
      deps({
        getCodexSummary: async () => summary('gmail'),
        getRenewalStatus: async () => ({ profiles: [renewalEntry('gmail', 'failed', 'dead')] }),
      })
    ).get();
    expect(result.accounts.find((account) => account.id === 'codex:gmail')).toMatchObject({
      isActive: true,
      message: null,
    });
  });

  it('keeps the renewal note on the cached Codex rows used while a collection is slow', async () => {
    const service = new AccountDashboardService(
      deps({
        getCodexSummary: async () => summary('lime'),
        getRenewalStatus: async () => ({ profiles: [renewalEntry('gmail', 'failed', 'dead')] }),
      })
    );
    await service.get();
    const cached = (
      service as unknown as { cachedCodex(scope: string): DashboardAccount[] }
    ).cachedCodex('fixture-scope');
    expect(cached.find((account) => account.id === 'codex:gmail')).toMatchObject({
      status: 'needs_sign_in',
      message: CODEX_RENEWAL_MESSAGES.dead,
    });
  });
});

it('stale-serves expired rows during collection but explicit refresh waits for it', async () => {
  let now = 100_000;
  let hold = false;
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const service = new AccountDashboardService(
    deps({
      now: () => now,
      getCodexRows: async (names) => {
        if (hold) await gate;
        return names.map((name) => row(name));
      },
    })
  );
  await service.get('mac');
  now += 60_001;
  hold = true;
  const stale = await service.get('mac');
  expect(stale.accounts.find((account) => account.provider === 'codex')?.status).toBe('cached');
  let finished = false;
  const refresh = service.get('mac', true).then(() => {
    finished = true;
  });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(finished).toBe(false);
  release();
  await refresh;
  expect(finished).toBe(true);
});

describe('usage freshness between dashboard refreshes', () => {
  it('keeps successful quota fresh on normal Home, Accounts and snapshot reads', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let calls = 0;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        getAdditionalAccounts: async () => {
          calls++;
          return [additional()];
        },
        getLiveClaudeUsage: async (id) => liveClaude(id),
      })
    );
    const first = await service.get('windows');
    now += 30_000;
    for (const next of [await service.get('windows'), await service.peek('windows')]) {
      for (const id of ['codex:gmail', 'claude:gmail', 'cursor:usage']) {
        const row = next.accounts.find((account) => account.id === id)!;
        expect(row.status).toBe('ok');
        expect(row.sampledAt).toBe(first.accounts.find((account) => account.id === id)!.sampledAt);
      }
    }
    expect(calls).toBe(1);
  });

  it('marks an overdue read-only snapshot as pending without calling a provider', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let calls = 0;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        getAdditionalAccounts: async () => {
          calls++;
          return [additional()];
        },
      })
    );
    await service.get('windows');
    now += 61_000;
    const next = await service.peek('windows');
    expect(next.accounts.find((a) => a.id === 'cursor:usage')).toMatchObject({
      status: 'cached',
      cacheReason: 'refresh_pending',
    });
    expect(calls).toBe(1);
  });

  it('returns the last good rows with their read time while a read is still running', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let blocked = false;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        responseBudgetMs: 10,
        getAdditionalAccounts: async () => (blocked ? new Promise(() => {}) : [additional()]),
      })
    );
    const first = (await service.get('windows')).accounts.find((a) => a.id === 'cursor:usage')!;
    now += 61_000;
    blocked = true;
    const next = (await service.get('windows', true)).accounts.find(
      (a) => a.id === 'cursor:usage'
    )!;
    // A slow read is not a failed one: no "did not finish" relabel of a current reading.
    expect(next).toMatchObject({
      status: 'ok',
      message: null,
      sampledAt: first.sampledAt,
      fetchedAt: first.fetchedAt,
    });
    expect(next.cacheReason).toBeUndefined();
  });

  it('shows a hung read as a pending refresh, never as live or failed', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let blocked = false;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        responseBudgetMs: 10,
        getAdditionalAccounts: async () => (blocked ? new Promise(() => {}) : [additional()]),
      })
    );
    const first = (await service.get('windows')).accounts.find((a) => a.id === 'cursor:usage')!;
    blocked = true;
    now += 121_000;
    const next = (await service.get('windows', true)).accounts.find(
      (a) => a.id === 'cursor:usage'
    )!;
    expect(next).toMatchObject({
      status: 'cached',
      cacheReason: 'refresh_pending',
      fetchedAt: first.fetchedAt,
    });
    expect(next.message).not.toContain('did not finish');
  });

  it('labels a part that fails after the deadline as failed, not still running', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let failing = false;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        responseBudgetMs: 10,
        getAdditionalAccounts: async () => {
          if (!failing) return [additional()];
          await new Promise((resolve) => setTimeout(resolve, 40));
          throw new Error('fixture read failed late');
        },
      })
    );
    const first = (await service.get('windows')).accounts.find((a) => a.id === 'cursor:usage')!;
    failing = true;
    now += 121_000;
    const running = (await service.get('windows', true)).accounts.find(
      (a) => a.id === 'cursor:usage'
    )!;
    expect(running).toMatchObject({ status: 'cached', cacheReason: 'refresh_pending' });
    await new Promise((resolve) => setTimeout(resolve, 80));
    const failed = (await service.peek('windows')).accounts.find((a) => a.id === 'cursor:usage')!;
    expect(failed).toMatchObject({
      status: 'cached',
      cacheReason: 'read_failed',
      fetchedAt: first.fetchedAt,
    });
    expect(failed.message).not.toContain('still running');
  });

  it('keeps a real failure a failure', async () => {
    let now = Date.parse('2026-10-01T12:00:00Z');
    let failing = false;
    const service = new AccountDashboardService(
      deps({
        now: () => now,
        responseBudgetMs: 1000,
        getAdditionalAccounts: async () => {
          if (failing) throw new Error('fixture read failed');
          return [additional()];
        },
      })
    );
    const first = (await service.get('windows')).accounts.find((a) => a.id === 'cursor:usage')!;
    now += 61_000;
    failing = true;
    const next = (await service.get('windows', true)).accounts.find(
      (a) => a.id === 'cursor:usage'
    )!;
    expect(next).toMatchObject({
      status: 'cached',
      cacheReason: 'read_failed',
      fetchedAt: first.fetchedAt,
    });
  });
});

it('keeps a timed-out Antigravity collection retained in its synchronous projection', async () => {
  let now = Date.parse('2026-10-01T12:00:00Z');
  let blocked = false;
  const account: AntigravityDashboardAccount = {
    ...additional('antigravity'),
    id: 'antigravity:profile:fixture',
    platform: 'ubuntu',
    sampledAt: new Date(now).toISOString(),
    fetchedAt: new Date(now).toISOString(),
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: 'fixture',
      antigravityHostIds: ['ubuntu'],
    },
  };
  const service = new AccountDashboardService(
    deps({
      now: () => now,
      responseBudgetMs: 10,
      hasAntigravityProfiles: () => true,
      failover: null,
      getAntigravityAccounts: async () => (blocked ? new Promise(() => {}) : [account]),
      getCachedAntigravityAccounts: () => [account],
    })
  );
  expect((await service.get('windows')).accounts.find((a) => a.id === account.id)?.status).toBe(
    'ok'
  );
  now += 61_000;
  blocked = true;
  // Still reading at the deadline: the retained row keeps its own read time and status.
  expect(
    (await service.get('windows', true)).accounts.find((a) => a.id === account.id)
  ).toMatchObject({
    status: 'ok',
    sampledAt: account.sampledAt,
  });
  now += 60_000;
  expect(
    (await service.get('windows', true)).accounts.find((a) => a.id === account.id)
  ).toMatchObject({
    status: 'cached',
    cacheReason: 'refresh_pending',
    sampledAt: account.sampledAt,
  });
});

describe('OpenCode Go visibility carryover (Low 1)', () => {
  function opencodeRow(id: string, label: string): DashboardAccount {
    return {
      ...additional('opencode-go'),
      id,
      label,
      platform: 'ubuntu',
    };
  }

  it('carries a bare opencode-go hide choice to a unique digest row only when saved, independently for Home and trays', async () => {
    const namedOne = opencodeRow('opencode-go:acct:11111111', 'Key 1');
    const namedTwo = opencodeRow('opencode-go:acct:22222222', 'Key 2');
    const unnamed = opencodeRow('opencode-go:usage:cccccccc', 'Key cccccccc');

    const saveMigration =
      (initial: { hiddenAccountIds: string[]; trayHiddenAccountIds: string[] }) =>
      async (_dir: string, replacements: ReadonlyMap<string, string>) => ({
        state: 'saved' as const,
        visibility: {
          hiddenProviders: [],
          hiddenAccountIds: replaceVisibilityAccountIds(initial.hiddenAccountIds, replacements)
            .list,
          trayHiddenProviders: [],
          trayHiddenAccountIds: replaceVisibilityAccountIds(
            initial.trayHiddenAccountIds,
            replacements
          ).list,
        },
      });

    const homeOnlyInitial = { hiddenAccountIds: ['opencode-go:usage'], trayHiddenAccountIds: [] };
    const homeOnly = await new AccountDashboardService(
      deps({
        getAdditionalAccounts: async () => [namedOne, namedTwo, unnamed],
        readVisibility: async () => ({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            ...homeOnlyInitial,
            trayHiddenProviders: [],
          },
        }),
        migrateVisibility: saveMigration(homeOnlyInitial),
      })
    ).get();
    expect(homeOnly.settings.hiddenAccountIds).toEqual(['opencode-go:usage:cccccccc']);
    expect(homeOnly.settings.trayHiddenAccountIds).toEqual([]);
    expect(homeOnly.accounts.find((a) => a.id === 'opencode-go:usage:cccccccc')).toMatchObject({
      hidden: true,
      trayHidden: false,
    });
    expect(homeOnly.accounts.find((a) => a.id === 'opencode-go:acct:11111111')).toMatchObject({
      hidden: false,
      trayHidden: false,
    });

    const trayOnlyInitial = { hiddenAccountIds: [], trayHiddenAccountIds: ['opencode-go:usage'] };
    const trayOnly = await new AccountDashboardService(
      deps({
        getAdditionalAccounts: async () => [namedOne, namedTwo, unnamed],
        readVisibility: async () => ({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            ...trayOnlyInitial,
            trayHiddenProviders: [],
          },
        }),
        migrateVisibility: saveMigration(trayOnlyInitial),
      })
    ).get();
    expect(trayOnly.settings.hiddenAccountIds).toEqual([]);
    expect(trayOnly.settings.trayHiddenAccountIds).toEqual(['opencode-go:usage:cccccccc']);
    expect(trayOnly.accounts.find((a) => a.id === 'opencode-go:usage:cccccccc')).toMatchObject({
      hidden: false,
      trayHidden: true,
    });

    const bothInitial = {
      hiddenAccountIds: ['codex:party', 'opencode-go:usage', 'opencode-go:usage:cccccccc'],
      trayHiddenAccountIds: ['opencode-go:usage'],
    };
    const both = await new AccountDashboardService(
      deps({
        getAdditionalAccounts: async () => [namedOne, namedTwo, unnamed],
        readVisibility: async () => ({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            ...bothInitial,
            trayHiddenProviders: [],
          },
        }),
        migrateVisibility: saveMigration(bothInitial),
      })
    ).get();
    expect(both.settings.hiddenAccountIds).toEqual(['codex:party', 'opencode-go:usage:cccccccc']);
    expect(both.settings.trayHiddenAccountIds).toEqual(['opencode-go:usage:cccccccc']);
    expect(both.accounts.find((a) => a.id === 'opencode-go:usage:cccccccc')).toMatchObject({
      hidden: true,
      trayHidden: true,
    });

    // When the migration save fails (unavailable or throws), the row stays visible and the bare ID stays untouched.
    for (const failingMigrate of [
      async () => ({ state: 'unavailable' as const }),
      async () => {
        throw new Error('lease busy');
      },
    ]) {
      const unsaved = await new AccountDashboardService(
        deps({
          getAdditionalAccounts: async () => [namedOne, namedTwo, unnamed],
          readVisibility: async () => ({
            state: 'ok',
            visibility: {
              hiddenProviders: [],
              hiddenAccountIds: ['opencode-go:usage'],
              trayHiddenProviders: [],
              trayHiddenAccountIds: ['opencode-go:usage'],
            },
          }),
          migrateVisibility: failingMigrate,
        })
      ).get();
      expect(unsaved.settings.hiddenAccountIds).toEqual(['opencode-go:usage']);
      expect(unsaved.settings.trayHiddenAccountIds).toEqual(['opencode-go:usage']);
      expect(unsaved.accounts.find((a) => a.id === 'opencode-go:usage:cccccccc')).toMatchObject({
        hidden: false,
        trayHidden: false,
      });
    }
  });

  it('does not carry a bare opencode-go hide choice when two digest rows are present', async () => {
    const first = opencodeRow('opencode-go:usage:aaaaaaaa', 'Key aaaaaaaa');
    const second = opencodeRow('opencode-go:usage:bbbbbbbb', 'Key bbbbbbbb');
    const result = await new AccountDashboardService(
      deps({
        getAdditionalAccounts: async () => [first, second],
        readVisibility: async () => ({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            hiddenAccountIds: ['opencode-go:usage'],
            trayHiddenProviders: [],
            trayHiddenAccountIds: ['opencode-go:usage'],
          },
        }),
      })
    ).get();
    expect(result.settings.hiddenAccountIds).toEqual(['opencode-go:usage']);
    expect(result.settings.trayHiddenAccountIds).toEqual(['opencode-go:usage']);
    for (const id of ['opencode-go:usage:aaaaaaaa', 'opencode-go:usage:bbbbbbbb']) {
      expect(result.accounts.find((a) => a.id === id)).toMatchObject({
        hidden: false,
        trayHidden: false,
      });
    }
  });

  it('persists a bare opencode-go hide onto key A independently for Home and tray and never moves it to key B', async () => {
    const idA = 'opencode-go:usage:aaaaaaaa';
    const idB = 'opencode-go:usage:bbbbbbbb';
    const rowA = opencodeRow(idA, 'Key aaaaaaaa');
    const rowB = opencodeRow(idB, 'Key bbbbbbbb');

    for (const trayOnly of [false, true]) {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-opencode-vis-seq-'));
      fs.chmodSync(dir, 0o700);
      try {
        await writeAccountVisibility(dir, {
          hiddenProviders: [],
          hiddenAccountIds: trayOnly ? [] : ['codex:party', 'opencode-go:usage'],
          trayHiddenProviders: [],
          trayHiddenAccountIds: trayOnly ? ['codex:party', 'opencode-go:usage'] : [],
        });

        let now = Date.parse('2026-11-21T12:00:00Z');
        let currentRows: DashboardAccount[] = [rowA];
        const service = new AccountDashboardService(
          deps({
            scope: () => dir,
            now: () => now,
            readVisibility: undefined,
            getAdditionalAccounts: async () => currentRows,
          })
        );

        const expectedHome = trayOnly ? [] : ['codex:party', idA];
        const expectedTray = trayOnly ? ['codex:party', idA] : [];

        // Step 1: only key A exists -> A is hidden and the digest replaces the bare ID on disk.
        const step1 = await service.get('mac', true);
        expect(step1.settings.hiddenAccountIds).toEqual(expectedHome);
        expect(step1.settings.trayHiddenAccountIds).toEqual(expectedTray);
        expect(step1.accounts.find((a) => a.id === idA)).toMatchObject({
          hidden: !trayOnly,
          trayHidden: trayOnly,
        });
        expect(await readAccountVisibility(dir)).toEqual({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            hiddenAccountIds: expectedHome,
            trayHiddenProviders: [],
            trayHiddenAccountIds: expectedTray,
          },
        });

        // Step 2: add key B alongside A -> A stays hidden, B is visible.
        currentRows = [rowA, rowB];
        now += 61_000;
        const step2 = await service.get('mac', true);
        expect(step2.accounts.find((a) => a.id === idA)).toMatchObject({
          hidden: !trayOnly,
          trayHidden: trayOnly,
        });
        expect(step2.accounts.find((a) => a.id === idB)).toMatchObject({
          hidden: false,
          trayHidden: false,
        });

        // Step 3: remove A, leave only B -> B stays visible, saved choice stays on A.
        currentRows = [rowB];
        now += 61_000;
        const step3 = await service.get('mac', true);
        expect(step3.accounts.find((a) => a.id === idB)).toMatchObject({
          hidden: false,
          trayHidden: false,
        });
        expect(step3.settings.hiddenAccountIds).toEqual(expectedHome);
        expect(step3.settings.trayHiddenAccountIds).toEqual(expectedTray);
        expect(await readAccountVisibility(dir)).toEqual({
          state: 'ok',
          visibility: {
            hiddenProviders: [],
            hiddenAccountIds: expectedHome,
            trayHiddenProviders: [],
            trayHiddenAccountIds: expectedTray,
          },
        });
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    }
  });

  it('never alters a visibility choice saved directly on an opencode-go digest id', async () => {
    const idA = 'opencode-go:usage:aaaaaaaa';
    const idB = 'opencode-go:usage:bbbbbbbb';
    const rowA = opencodeRow(idA, 'Key aaaaaaaa');
    const rowB = opencodeRow(idB, 'Key bbbbbbbb');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-opencode-vis-direct-'));
    fs.chmodSync(dir, 0o700);
    try {
      await writeAccountVisibility(dir, {
        hiddenProviders: [],
        hiddenAccountIds: [idA],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [idB],
      });
      const file = path.join(dir, 'account-visibility.json');
      const before = fs.readFileSync(file, 'utf8');

      let now = Date.parse('2026-11-21T12:00:00Z');
      let currentRows: DashboardAccount[] = [rowA];
      const service = new AccountDashboardService(
        deps({
          scope: () => dir,
          now: () => now,
          readVisibility: undefined,
          getAdditionalAccounts: async () => currentRows,
        })
      );

      const s1 = await service.get('mac', true);
      expect(s1.accounts.find((a) => a.id === idA)).toMatchObject({
        hidden: true,
        trayHidden: false,
      });

      currentRows = [rowB];
      now += 61_000;
      const s2 = await service.get('mac', true);
      expect(s2.accounts.find((a) => a.id === idB)).toMatchObject({
        hidden: false,
        trayHidden: true,
      });
      expect(fs.readFileSync(file, 'utf8')).toBe(before);

      // Migrating a bare ID on Home must not alter a direct digest choice on tray.
      await writeAccountVisibility(dir, {
        hiddenProviders: [],
        hiddenAccountIds: ['opencode-go:usage'],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [idB],
      });
      currentRows = [rowA];
      now += 61_000;
      const mixed1 = await service.get('mac', true);
      expect(mixed1.accounts.find((a) => a.id === idA)).toMatchObject({
        hidden: true,
        trayHidden: false,
      });
      expect(await readAccountVisibility(dir)).toEqual({
        state: 'ok',
        visibility: {
          hiddenProviders: [],
          hiddenAccountIds: [idA],
          trayHiddenProviders: [],
          trayHiddenAccountIds: [idB],
        },
      });
      currentRows = [rowB];
      now += 61_000;
      const mixed2 = await service.get('mac', true);
      expect(mixed2.accounts.find((a) => a.id === idB)).toMatchObject({
        hidden: false,
        trayHidden: true,
      });
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('does not remap an unsaved bare opencode-go hide when the lease is busy and never moves it to key B', async () => {
    const idA = 'opencode-go:usage:aaaaaaaa';
    const idB = 'opencode-go:usage:bbbbbbbb';
    const rowA = opencodeRow(idA, 'Key aaaaaaaa');
    const rowB = opencodeRow(idB, 'Key bbbbbbbb');

    // Part 1: hold the visibility lease during the A-only generation, release it, then A+B,
    // then B-only. Tested independently for Home-only and tray-only.
    for (const trayOnly of [false, true]) {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-opencode-vis-busy-'));
      fs.chmodSync(dir, 0o700);
      try {
        const initialHome = trayOnly ? [] : ['codex:party', 'opencode-go:usage'];
        const initialTray = trayOnly ? ['codex:party', 'opencode-go:usage'] : [];
        await writeAccountVisibility(dir, {
          hiddenProviders: [],
          hiddenAccountIds: initialHome,
          trayHiddenProviders: [],
          trayHiddenAccountIds: initialTray,
        });

        let now = Date.parse('2026-11-21T12:00:00Z');
        let currentRows: DashboardAccount[] = [rowA];
        const service = new AccountDashboardService(
          deps({
            scope: () => dir,
            now: () => now,
            readVisibility: undefined,
            getAdditionalAccounts: async () => currentRows,
          })
        );

        const held = claimFileLease(dir, path.join(dir, 'account-visibility.json.lock'));
        try {
          // Step 1: A-only while lease is busy -> A stays visible and bare ID stays on disk.
          const step1 = await service.get('mac', true);
          expect(step1.accounts.find((a) => a.id === idA)).toMatchObject({
            hidden: false,
            trayHidden: false,
          });
          expect(step1.settings.hiddenAccountIds).toEqual(initialHome);
          expect(step1.settings.trayHiddenAccountIds).toEqual(initialTray);
          expect(await readAccountVisibility(dir)).toEqual({
            state: 'ok',
            visibility: {
              hiddenProviders: [],
              hiddenAccountIds: initialHome,
              trayHiddenProviders: [],
              trayHiddenAccountIds: initialTray,
            },
          });
        } finally {
          held.close();
        }

        // Step 2: lease released, A+B present -> neither key is hidden.
        currentRows = [rowA, rowB];
        now += 61_000;
        const step2 = await service.get('mac', true);
        expect(step2.accounts.find((a) => a.id === idA)).toMatchObject({
          hidden: false,
          trayHidden: false,
        });
        expect(step2.accounts.find((a) => a.id === idB)).toMatchObject({
          hidden: false,
          trayHidden: false,
        });

        // Step 3: B-only -> B is never hidden, and disk ends with A's digest or the
        // untouched bare ID, never B's digest.
        currentRows = [rowB];
        now += 61_000;
        const step3 = await service.get('mac', true);
        expect(step3.accounts.find((a) => a.id === idB)).toMatchObject({
          hidden: false,
          trayHidden: false,
        });
        const allowedHome = trayOnly
          ? [[]]
          : [
              ['codex:party', idA],
              ['codex:party', 'opencode-go:usage'],
            ];
        const allowedTray = trayOnly
          ? [
              ['codex:party', idA],
              ['codex:party', 'opencode-go:usage'],
            ]
          : [[]];
        expect(allowedHome).toContainEqual(step3.settings.hiddenAccountIds);
        expect(allowedTray).toContainEqual(step3.settings.trayHiddenAccountIds);
        const saved = await readAccountVisibility(dir);
        expect(saved.state).toBe('ok');
        if (saved.state === 'ok') {
          expect(allowedHome).toContainEqual(saved.visibility.hiddenAccountIds);
          expect(allowedTray).toContainEqual(saved.visibility.trayHiddenAccountIds);
        }
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    }

    // Part 2: next generation retries while A is still sole key, re-reads under the lease,
    // and migrates only lists that still hold the bare ID.
    const retryDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-opencode-vis-retry-'));
    fs.chmodSync(retryDir, 0o700);
    try {
      await writeAccountVisibility(retryDir, {
        hiddenProviders: [],
        hiddenAccountIds: ['codex:party', 'opencode-go:usage'],
        trayHiddenProviders: [],
        trayHiddenAccountIds: ['opencode-go:usage'],
      });

      let now = Date.parse('2026-11-21T12:00:00Z');
      const service = new AccountDashboardService(
        deps({
          scope: () => retryDir,
          now: () => now,
          readVisibility: undefined,
          getAdditionalAccounts: async () => [rowA],
        })
      );

      const held = claimFileLease(retryDir, path.join(retryDir, 'account-visibility.json.lock'));
      try {
        const busy = await service.get('mac', true);
        expect(busy.accounts.find((a) => a.id === idA)).toMatchObject({
          hidden: false,
          trayHidden: false,
        });
        // Concurrent Settings save removes the bare ID from Home while keeping tray bare.
        fs.writeFileSync(
          path.join(retryDir, 'account-visibility.json'),
          JSON.stringify({
            version: 1,
            hiddenProviders: [],
            hiddenAccountIds: ['codex:party'],
            trayHiddenProviders: [],
            trayHiddenAccountIds: ['opencode-go:usage'],
          }) + '\n',
          { mode: 0o600 }
        );
      } finally {
        held.close();
      }

      now += 61_000;
      const retried = await service.get('mac', true);
      expect(retried.accounts.find((a) => a.id === idA)).toMatchObject({
        hidden: false,
        trayHidden: true,
      });
      expect(retried.settings.hiddenAccountIds).toEqual(['codex:party']);
      expect(retried.settings.trayHiddenAccountIds).toEqual([idA]);
      expect(await readAccountVisibility(retryDir)).toEqual({
        state: 'ok',
        visibility: {
          hiddenProviders: [],
          hiddenAccountIds: ['codex:party'],
          trayHiddenProviders: [],
          trayHiddenAccountIds: [idA],
        },
      });
    } finally {
      fs.rmSync(retryDir, { recursive: true, force: true });
    }
  });
});
