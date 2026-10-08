/**
 * Additive dashboard DTO fields (CONTRACT-serving-misc section 4):
 * codexAutoSwitch.thresholdUsedPercent, the Antigravity "Needs setup" row with
 * accounts[].switchable and accounts[].lifecycle, server.version/commit and the
 * app-update expectedResults. Fixtures only; no provider, host or real CCS dir.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import { setDashboardBuildCommit } from '../../../src/web-server/services/dashboard-server-info';
import {
  AppUpdateService,
  UPDATE_APP_LABELS,
} from '../../../src/web-server/services/app-update-service';
import type { DashboardProvider } from '../../../src/web-server/services/account-dashboard-types';
import { AntigravityUsageService } from '../../../src/antigravity/usage-service';
import type { AntigravityUsageProfile } from '../../../src/antigravity/usage-contract';
import { disabledAntigravityAutoSwitchStatus } from '../../../src/antigravity/runtime-composition';

const NOW = '2026-10-02T08:00:00.000Z';
const ADDITIONAL: DashboardProvider[] = [
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
];
const packageVersion = JSON.parse(
  fs.readFileSync(path.resolve(import.meta.dir, '../../../package.json'), 'utf8')
).version as string;
const directories: string[] = [];

afterEach(() => {
  setDashboardBuildCommit(null);
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});

function antigravityProfile(
  id: string,
  changes: Partial<AntigravityUsageProfile> = {}
): AntigravityUsageProfile {
  return {
    id,
    email: `${id}@example.com`,
    plan: 'Pro',
    identityKey: `identity-${id}`,
    credentialRevision: `revision-${id}`,
    identityVerified: true,
    available: true,
    selected: false,
    runtimeVerified: false,
    verifiedAt: NOW,
    ...changes,
  };
}

/** Registry rows exactly as the Antigravity runtime projects them (no sample yet). */
function antigravityRows(profiles: AntigravityUsageProfile[]) {
  const usage = new AntigravityUsageService({
    readProfiles: async () => profiles,
    collectQuota: async () => {
      throw new Error('No provider call in this fixture.');
    },
  });
  return usage.cachedAccounts(profiles);
}

function deps(extra: AccountDashboardDeps = {}): AccountDashboardDeps {
  const profiles = [
    antigravityProfile('gmail', { selected: true, runtimeVerified: true }),
    // Saved and verified, but never activated: still a switch target.
    antigravityProfile('work'),
    // The second account before its sign-in finished: no verified saved login.
    antigravityProfile('party', {
      identityVerified: false,
      available: false,
      credentialRevision: 'unavailable',
      verifiedAt: null,
    }),
  ];
  return {
    getCodexSummary: async () => ({
      active: { name: 'gmail', source: 'default', codexHome: '/fixture' },
      activated: { name: 'gmail', email: 'gmail@example.com', plan: 'pro', codexHome: '/fixture' },
      default: 'gmail',
      profiles: ['gmail', 'expired'].map((name) => ({
        name,
        email: `${name}@example.com`,
        plan: 'pro',
        accountId: `fixture-${name}`,
        codexHome: '/fixture',
        lastUsed: null,
        authValid: name !== 'expired',
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
    getAdditionalAccounts: async () => [],
    getOptionalWalletAccounts: async () => [],
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
    scope: () => '/tmp/aac-dto-fixture-scope',
    now: () => Date.parse(NOW),
    refreshIntervalSeconds: () => 60,
    responseBudgetMs: 1000,
    hasAntigravityProfiles: () => true,
    getAntigravityAccounts: async () => antigravityRows(profiles),
    getCachedAntigravityAccounts: () => antigravityRows(profiles),
    readSelectedAntigravityProfileId: async () => 'gmail',
    ...extra,
  };
}

describe('codexAutoSwitch.thresholdUsedPercent', () => {
  it('is 100 - thresholdPercent in the dashboard and in the auto-switch status', async () => {
    const dashboard = await new AccountDashboardService(deps()).get('mac');
    expect(dashboard.codexAutoSwitch).toMatchObject({
      thresholdPercent: 5,
      thresholdUsedPercent: 95,
    });
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dto-threshold-'));
    directories.push(directory);
    const service = new CodexAutoSwitchService({
      ccsDir: directory,
      readConfig: () => ({ enabled: false, thresholdPercent: 12 }),
    });
    expect(service.getStatus()).toMatchObject({ thresholdPercent: 12, thresholdUsedPercent: 88 });
  });
});

describe('Antigravity "Needs setup" row, switchable and lifecycle', () => {
  it('lists an unverified Antigravity profile as pending sign-in and never as a switch target', async () => {
    const dashboard = await new AccountDashboardService(deps()).get('mac');
    const party = dashboard.accounts.find((row) => row.id === 'antigravity:profile:party');
    expect(party).toMatchObject({
      provider: 'antigravity',
      email: 'party@example.com',
      status: 'needs_sign_in',
      switchable: false,
      lifecycle: { state: 'pending_sign_in', jobId: null },
      windows: [],
      isActive: false,
    });
    expect(party?.capabilities.antigravityCanActivate).toBe(false);
    for (const id of ['antigravity:profile:gmail', 'antigravity:profile:work']) {
      const row = dashboard.accounts.find((account) => account.id === id);
      expect([id, row?.switchable, row?.lifecycle]).toEqual([
        id,
        true,
        { state: 'ready', jobId: null },
      ]);
      expect(row?.status).not.toBe('needs_sign_in');
    }
  });

  it('never offers a row whose reading could not be bound to its saved identity as a switch target', async () => {
    const profiles = [
      antigravityProfile('gmail', { selected: true, runtimeVerified: true }),
      antigravityProfile('work'),
    ];
    const usage = new AntigravityUsageService({
      readProfiles: async () => profiles,
      now: () => Date.parse(NOW),
      // `work` answers with another account's identity; `gmail` is bound but unavailable.
      collectQuota: async (id) => ({
        profileId: id,
        identityKey: id === 'work' ? 'identity-someone-else' : `identity-${id}`,
        credentialRevision: `revision-${id}`,
        status: 'unavailable',
        email: null,
        plan: null,
        fetchedAt: null,
        sampledAt: null,
        windows: [],
      }),
    });
    await usage.getAccounts({ refresh: true });
    const rows = () => usage.cachedAccounts(profiles);
    const dashboard = await new AccountDashboardService(
      deps({ getAntigravityAccounts: async () => rows(), getCachedAntigravityAccounts: rows })
    ).get('mac');
    expect(dashboard.accounts.find((row) => row.id === 'antigravity:profile:work')).toMatchObject({
      status: 'error',
      statusReason: 'identity_unbound',
      message: 'Antigravity usage could not be matched to this saved account.',
      switchable: false,
      // Not a setup row: the saved login is verified; only this reading failed.
      lifecycle: { state: 'ready', jobId: null },
    });
    const gmail = dashboard.accounts.find((row) => row.id === 'antigravity:profile:gmail');
    expect(gmail).toMatchObject({ status: 'unavailable', switchable: true });
    expect(gmail && 'statusReason' in gmail).toBe(false);
  });

  it('keeps a bound Antigravity row whose reading failed for another reason a switch target', async () => {
    const profiles = [
      antigravityProfile('gmail', { selected: true, runtimeVerified: true }),
      antigravityProfile('work'),
    ];
    const usage = new AntigravityUsageService({
      readProfiles: async () => profiles,
      now: () => Date.parse(NOW),
      // `work` is bound to its saved identity, but the helper call fails.
      collectQuota: async (id) => {
        if (id === 'work') throw new Error('helper failed');
        return {
          profileId: id,
          identityKey: `identity-${id}`,
          credentialRevision: `revision-${id}`,
          status: 'unavailable',
          email: null,
          plan: null,
          fetchedAt: null,
          sampledAt: null,
          windows: [],
        };
      },
    });
    await usage.getAccounts({ refresh: true });
    const rows = () => usage.cachedAccounts(profiles);
    const dashboard = await new AccountDashboardService(
      deps({ getAntigravityAccounts: async () => rows(), getCachedAntigravityAccounts: rows })
    ).get('mac');
    const work = dashboard.accounts.find((row) => row.id === 'antigravity:profile:work');
    expect(work).toMatchObject({
      status: 'error',
      message: 'Antigravity usage is temporarily unavailable.',
      switchable: true,
      lifecycle: { state: 'ready', jobId: null },
    });
    expect(work && 'statusReason' in work).toBe(false);
  });

  it('turns a profile whose saved login became unusable into a setup row on a cache hit', async () => {
    let usable = true;
    const profiles = () => [
      antigravityProfile('gmail', { selected: true, runtimeVerified: true }),
      antigravityProfile('work', usable ? {} : { identityVerified: false, available: false }),
    ];
    const service = new AccountDashboardService(
      deps({
        getAntigravityAccounts: async () => antigravityRows(profiles()),
        getCachedAntigravityAccounts: () => antigravityRows(profiles()),
      })
    );
    const before = await service.get('mac');
    expect(before.accounts.find((row) => row.id === 'antigravity:profile:work')?.switchable).toBe(
      true
    );
    usable = false;
    const after = await service.get('mac');
    expect(after.accounts.find((row) => row.id === 'antigravity:profile:work')).toMatchObject({
      status: 'needs_sign_in',
      switchable: false,
      lifecycle: { state: 'pending_sign_in', jobId: null },
    });
  });

  it('marks Codex profiles with a valid saved login switchable and every other provider not', async () => {
    const dashboard = await new AccountDashboardService(
      deps({
        getAdditionalAccounts: async () =>
          ADDITIONAL.map((provider) => ({
            id: `${provider}:usage`,
            provider,
            providerLabel: provider,
            label: provider,
            email: null,
            plan: null,
            platform: 'ubuntu' as const,
            source: 'Fixture usage',
            status: 'unavailable' as const,
            message: null,
            fetchedAt: null,
            sampledAt: null,
            isActive: false,
            windows: [],
            capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
          })),
      })
    ).get('mac');
    const byId = (id: string) => dashboard.accounts.find((row) => row.id === id);
    expect(byId('codex:gmail')?.switchable).toBe(true);
    expect(byId('codex:expired')).toMatchObject({
      status: 'needs_sign_in',
      switchable: false,
      lifecycle: { state: 'ready', jobId: null },
    });
    expect(byId('claude:me')?.switchable).toBe(false);
    for (const row of dashboard.accounts) {
      expect(typeof row.switchable).toBe('boolean');
      expect(row.lifecycle?.jobId).toBeNull();
      if (row.provider !== 'codex' && row.provider !== 'antigravity')
        expect([row.id, row.switchable]).toEqual([row.id, false]);
    }
  });
});

describe('server version and commit', () => {
  it('reports the package version and the build commit, and nothing else', async () => {
    setDashboardBuildCommit('8fb5e3de');
    const dashboard = await new AccountDashboardService(deps()).get('mac');
    expect(dashboard.server).toEqual({ version: packageVersion, commit: '8fb5e3de' });
    setDashboardBuildCommit('not-a-commit');
    expect((await new AccountDashboardService(deps()).get('mac')).server).toEqual({
      version: packageVersion,
      commit: null,
    });
  });

  it('omits server when its version is unknown', async () => {
    const dashboard = await new AccountDashboardService(deps({ serverInfo: () => undefined })).get(
      'mac'
    );
    expect(dashboard).not.toHaveProperty('server');
  });
});

describe('app update expectedResults', () => {
  function payload() {
    return JSON.stringify({
      results: Object.keys(UPDATE_APP_LABELS).map((appId) => ({
        appId,
        status: 'current',
        messageCode: 'current',
        previousVersion: '1.2.3',
        version: '1.2.3',
        manager: 'native',
        updateAttempted: true,
        restartedProcesses: 0,
      })),
    });
  }

  it('is fixed when the job starts and never changes while it runs', async () => {
    let release = () => {};
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    const service = new AppUpdateService({
      persist: false,
      runHost: async (platform) => {
        if (platform === 'mac') await blocked;
        return payload();
      },
    });
    const expected = 4 * Object.keys(UPDATE_APP_LABELS).length;
    expect(service.start().job.expectedResults).toBe(expected);
    for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 0));
    const running = service.getStatus().job;
    expect(running?.state).toBe('running');
    expect(running?.results.length).toBeLessThan(expected);
    expect(running?.expectedResults).toBe(expected);
    release();
    for (let i = 0; i < 30 && service.getStatus().job?.state === 'running'; i++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    const finished = service.getStatus().job;
    expect(finished?.results).toHaveLength(expected);
    expect(finished?.expectedResults).toBe(expected);
  });

  it('survives a restart and reads null from a job saved before the field existed', async () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dto-updates-'));
    directories.push(directory);
    const first = new AppUpdateService({ ccsDir: directory, runHost: async () => payload() });
    first.start();
    for (let i = 0; i < 30 && first.getStatus().job?.state === 'running'; i++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    const restored = new AppUpdateService({ ccsDir: directory, runHost: async () => payload() });
    expect(restored.getStatus().job?.expectedResults).toBe(32);
    const file = path.join(directory, 'app-updates', 'dashboard-job.json');
    const saved = JSON.parse(fs.readFileSync(file, 'utf8'));
    delete saved.job.expectedResults;
    fs.writeFileSync(file, JSON.stringify(saved), { mode: 0o600 });
    const legacy = new AppUpdateService({ ccsDir: directory, runHost: async () => payload() });
    expect(legacy.getStatus().job?.expectedResults).toBeNull();
    saved.job.expectedResults = 999;
    fs.writeFileSync(file, JSON.stringify(saved), { mode: 0o600 });
    expect(
      new AppUpdateService({ ccsDir: directory, runHost: async () => payload() }).getStatus().job
        ?.expectedResults
    ).toBeNull();
  });
});
