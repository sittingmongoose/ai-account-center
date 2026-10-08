/**
 * The `host` field of GET /api/accounts/dashboard rows: a usage source that runs
 * on Nas1, the second Ubuntu computer, is stored and reported as platform
 * 'ubuntu' behind its fixed ssh alias and shown with host 'nas1'. Also pins that
 * nothing here gives Nas1 a key store, a sign-in or an activation. Fixtures
 * only: ssh is faked at execFile and no provider or real CCS dir is read.
 */
import { afterEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import { disabledAntigravityAutoSwitchStatus } from '../../../src/antigravity/runtime-composition';
import { keyStoreFor } from '../../../src/web-server/services/account-key-store';
import { parseAccountRegistry } from '../../../src/web-server/services/account-registry-v2';
import type { AccountRegistryRead } from '../../../src/web-server/services/account-registry-v2';
import { AccountDashboardService } from '../../../src/web-server/services/account-dashboard-service';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import {
  AdditionalAccountService,
  type AdditionalAccountDeps,
} from '../../../src/web-server/services/additional-account-service';
import { ADDITIONAL_PROVIDERS } from '../../../src/web-server/services/additional-usage-transport';
import { MuseAccountLifecycle } from '../../../src/web-server/services/muse-account-lifecycle';

const NOW = '2026-10-02T08:00:00.000Z';
const READING = JSON.stringify({
  status: 'ok',
  email: null,
  plan: 'Pro',
  fetchedAt: NOW,
  sampledAt: NOW,
  windows: [{ key: 'weekly', label: 'Weekly', usedPercent: 12, windowMinutes: 10080 }],
});

afterEach(() => mock.restore());

/** Stands in for ssh and the local helper: records every execFile call and runs nothing. */
function fakeExec() {
  return spyOn(childProcess, 'execFile').mockImplementation((...args: unknown[]) => {
    const callback = args.at(-1) as (error: null, stdout: string, stderr: string) => void;
    callback(null, READING, '');
    return {} as childProcess.ChildProcess;
  });
}

function manifest(sources: unknown[]): string {
  return JSON.stringify({ version: 1, sources });
}

function registry(accounts: unknown[]): AccountRegistryRead {
  const parsed = parseAccountRegistry({ version: 2, accounts });
  if (!parsed) throw new TypeError('fixture registry is invalid');
  return { state: 'ok', registry: parsed };
}

function dashboardService(additional: AdditionalAccountDeps): AccountDashboardService {
  const accounts = new AdditionalAccountService({
    ccsDir: '/tmp/aac-host-fields-fixture',
    now: () => Date.parse(NOW),
    ...additional,
  });
  return new AccountDashboardService({
    getCodexSummary: async () => ({ active: null, activated: null, default: null, profiles: [] }),
    getCodexRows: async () => [],
    getCachedCodexRows: () => [],
    listClaudeProfiles: async () => [],
    listClaudePending: async () => [],
    getClaudeUsage: async (platform) => ({ platform, fetchedAt: NOW, profiles: [] }),
    getAdditionalSnapshot: (refresh) => accounts.snapshot({ refresh }),
    getOptionalWalletAccounts: async () => [],
    getAutoSwitchStatus: () => ({
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      message: 'Disabled',
      activationInProgress: false,
    }),
    getAntigravityAutoSwitchStatus: disabledAntigravityAutoSwitchStatus,
    invalidateClaudeCache: () => {},
    // Every lifecycle flow on, so a provider that offered Nas1 would show it.
    providerFacts: () => ({ lifecycleRoutes: true, secureTransport: true }),
    accountJobStates: () => new Map(),
    hasAntigravityProfiles: () => false,
    readVisibility: async () => ({
      state: 'ok',
      visibility: {
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      },
    }),
    scope: () => '/tmp/aac-host-fields-fixture-scope',
    now: () => Date.parse(NOW),
    refreshIntervalSeconds: () => 60,
    responseBudgetMs: 1000,
  });
}

/** The body the route sends: serialized and parsed again. */
async function dashboardJson(service: AccountDashboardService): Promise<AccountDashboard> {
  return JSON.parse(JSON.stringify(await service.get('mac', true))) as AccountDashboard;
}

const CURSOR_ON_NAS1 = {
  id: 'cursor:usage',
  provider: 'cursor',
  platform: 'ubuntu',
  sshHost: 'nas1-agent',
  label: null,
  credential: { kind: 'discover' },
};

describe('the host field of the dashboard rows', () => {
  it.each([
    [
      'version 1 manifest source',
      {
        readManifest: async () =>
          manifest([{ provider: 'cursor', platform: 'ubuntu', sshHost: 'nas1-agent' }]),
      },
    ],
    ['registry v2 entry', { readRegistry: async () => registry([CURSOR_ON_NAS1]) }],
  ] as const)(
    'collects a %s over ssh to nas1-agent and reports the row as Nas1',
    async (_name, deps) => {
      const exec = fakeExec();
      const dashboard = await dashboardJson(dashboardService(deps));
      const ssh = exec.mock.calls
        .filter(([binary]) => binary === 'ssh')
        .map(([, args]) => args as string[]);
      expect(ssh).toHaveLength(1);
      expect(ssh[0]!.slice(-3)).toEqual([
        '--',
        'nas1-agent',
        `/usr/bin/python3 "$HOME/.ccs/account-usage/desktop_usage.py" --provider 'cursor' --platform 'ubuntu'`,
      ]);
      expect(dashboard.accounts.find((account) => account.provider === 'cursor')).toMatchObject({
        id: 'cursor:usage',
        platform: 'ubuntu',
        host: 'nas1',
        source: 'Account on Nas1',
        status: 'ok',
      });
    }
  );

  it('adds only the host key, and only to the row on Nas1', async () => {
    const dashboard = await dashboardJson(
      dashboardService({
        readManifest: async () =>
          manifest([
            { provider: 'cursor', platform: 'ubuntu', sshHost: 'nas1-agent' },
            { provider: 'zai', platform: 'ubuntu', sshHost: 'work-ubuntu' },
          ]),
        runSource: async () => READING,
      })
    );
    const row = (provider: string) =>
      dashboard.accounts.find((account) => account.provider === provider)!;
    // A plain local Ubuntu source and an Ubuntu source on another alias stay on Ubuntu.
    for (const local of [row('muse'), row('zai')]) {
      expect(local).toMatchObject({ platform: 'ubuntu', source: 'Account on Ubuntu' });
      expect(local).not.toHaveProperty('host');
    }
    expect(row('cursor')).toMatchObject({ platform: 'ubuntu', host: 'nas1' });
    expect(Object.keys(row('cursor')).filter((key) => key !== 'host')).toEqual(
      Object.keys(row('muse'))
    );
    expect(dashboard.accounts.filter((account) => 'host' in account)).toHaveLength(1);
  });
});

describe('nothing here gives Nas1 a key store, a sign-in or an activation', () => {
  it('has no key store behind the Nas1 alias, though a local or a Mac one still exists', () => {
    const ccsDir = '/tmp/aac-host-fields-keys';
    for (const localPlatform of [undefined, 'ubuntu'] as const) {
      expect(
        keyStoreFor({ platform: 'ubuntu', sshHost: 'nas1-agent' }, { ccsDir, localPlatform })
      ).toBeNull();
    }
    expect(keyStoreFor({ platform: 'ubuntu', sshHost: null }, { ccsDir })).not.toBeNull();
    expect(keyStoreFor({ platform: 'mac', sshHost: 'mac-host' }, { ccsDir })).not.toBeNull();
  });

  it('refuses Sign in again for a Muse account on Nas1', () => {
    const muse = {
      id: 'muse:usage',
      provider: 'muse',
      platform: 'ubuntu',
      sshHost: 'nas1-agent',
      label: null,
      credential: { kind: 'discover' },
      createdAt: null,
      createdBy: null,
    } as const;
    expect(() => new MuseAccountLifecycle({ enabled: true }).signInAgainFlow(muse)).toThrow(
      'not_configured'
    );
  });

  it('leaves every Nas1 row without a switch, an active mark or a host capability', async () => {
    // All seven providers on Nas1, Antigravity (the one with activation) included.
    const dashboard = await dashboardJson(
      dashboardService({
        readManifest: async () =>
          manifest(
            ADDITIONAL_PROVIDERS.map((provider) => ({
              provider,
              platform: 'ubuntu',
              sshHost: 'nas1-agent',
            }))
          ),
        runSource: async () => READING,
      })
    );
    const rows = dashboard.accounts.filter((account) => account.host === 'nas1');
    expect(rows.map((row) => row.provider)).toEqual([...ADDITIONAL_PROVIDERS]);
    for (const row of rows) {
      expect(row).toMatchObject({
        platform: 'ubuntu',
        source: 'Account on Nas1',
        switchable: false,
        isActive: false,
        lifecycle: { state: 'ready', jobId: null },
        capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
      });
      expect(row.capabilities).not.toHaveProperty('antigravityProfileId');
      expect(row.capabilities).not.toHaveProperty('antigravityHostIds');
    }
    // No provider offers a sign-in or Open on Nas1, even with every flow available.
    expect(dashboard.providers?.length).toBeGreaterThan(0);
    for (const provider of dashboard.providers ?? []) {
      expect(provider.signIn.platforms as string[]).not.toContain('nas1');
      expect(provider.capabilities.openApp as string[]).not.toContain('nas1');
    }
  });
});
