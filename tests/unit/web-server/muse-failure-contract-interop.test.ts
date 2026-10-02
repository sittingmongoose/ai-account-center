import { describe, expect, it } from 'bun:test';
import { execFileSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import { AdditionalAccountService } from '../../../src/web-server/services/additional-account-service';
import { AccountDashboardService } from '../../../src/web-server/services/account-dashboard-service';
import { AdditionalUsageTransportError } from '../../../src/web-server/services/additional-usage-transport';

interface Scenario {
  code: string;
  initial: Record<string, unknown>;
  failed: Record<string, unknown>;
  subsequent: Record<string, unknown>;
  recovered: Record<string, unknown>;
  nativeCleared: boolean;
  nextRequestAt: number;
  externalRequests: number;
}

const childHome = process.env.CCS_HOME ?? process.env.CCS_TEST_BOOTSTRAP_HOME;
if (!childHome || !path.isAbsolute(childHome)) {
  throw new Error('The Muse interop test requires its isolated fixture home.');
}
const childTmp = path.join(childHome, 'muse-python-fixture-tmp');
fs.mkdirSync(childTmp, { recursive: true });
const childEnvironment = {
  PATH: '/usr/bin:/bin',
  PYTHONDONTWRITEBYTECODE: '1',
  HOME: childHome,
  USERPROFILE: childHome,
  CCS_HOME: childHome,
  CCS_DIR: path.join(childHome, '.ccs'),
  XDG_CONFIG_HOME: path.join(childHome, '.config'),
  XDG_CACHE_HOME: path.join(childHome, '.cache'),
  XDG_STATE_HOME: path.join(childHome, '.state'),
  TMPDIR: childTmp,
  TEMP: childTmp,
  TMP: childTmp,
};

const fixtures: {
  cases: Scenario[];
  nativeRateLimited: Scenario;
  blankPortalEmail: Record<string, unknown>;
} = JSON.parse(
  execFileSync(
    '/usr/bin/python3',
    [
      '-c',
      [
        'import os, pathlib, re, runpy, sys',
        'assert str(pathlib.Path.home()) == os.environ["CCS_HOME"] == os.environ["HOME"]',
        'assert not any(re.search(r"TOKEN|API_KEY|SECRET|PASSWORD|ACCESS_KEY|COOKIE", key) for key in os.environ)',
        'runpy.run_path(sys.argv[1], run_name="__main__")',
      ].join('\n'),
      path.resolve(__dirname, '../account-usage/muse_failure_contract_fixture.py'),
    ],
    {
      encoding: 'utf8',
      timeout: 15_000,
      maxBuffer: 256 * 1024,
      env: childEnvironment,
    }
  )
);

const transient = new Set(['rate_limited', 'provider_error', 'network_error']);

function serviceFixture(sequence: Record<string, unknown>[]) {
  let phase = 0;
  let now = 0;
  const service = new AdditionalAccountService({
    ccsDir: '/tmp/muse-cross-layer-fixture',
    now: () => now,
    readManifest: async () =>
      JSON.stringify({
        version: 1,
        sources: [{ provider: 'muse', platform: 'mac', sshHost: 'fixture-mac' }],
      }),
    runSource: async (source) =>
      JSON.stringify(source.provider === 'muse' ? sequence[phase] : sequence[0]),
  });
  const dashboard = new AccountDashboardService({
    now: () => now,
    scope: () => 'muse-cross-layer-dashboard-fixture',
    refreshIntervalSeconds: () => 60,
    getCodexSummary: async () => ({ active: null, activated: null, default: null, profiles: [] }),
    getCodexRows: async () => [],
    getCachedCodexRows: () => [],
    listClaudeProfiles: async () => [],
    getClaudeUsage: async (platform) => ({ platform, fetchedAt: null, profiles: [] }),
    getLiveClaudeUsage: async () => null,
    getCachedLiveClaudeUsage: async () => null,
    getAdditionalAccounts: () => service.get({ refresh: true }),
    getOptionalWalletAccounts: async () => [],
    invalidateClaudeCache: () => {},
    getAutoSwitchStatus: () => ({
      enabled: false,
      thresholdPercent: 15,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      message: 'Disabled fixture',
      activationInProgress: false,
    }),
  });
  return {
    async read(index: number) {
      phase = index;
      now = index * 31_000;
      return (await dashboard.get('mac', true)).accounts.find(
        (account) => account.provider === 'muse'
      )!;
    },
  };
}

describe('real Python Muse helper to Node cache boundary', () => {
  it.each(fixtures.cases.filter((fixture) => !transient.has(fixture.code)))(
    'clears Node and native old quota for $code and does not resurrect it on a later network error',
    async (fixture) => {
      expect(fixture.externalRequests).toBe(0);
      expect(fixture.nativeCleared).toBe(true);
      expect(fixture.failed.failureCode).toBe(
        fixture.code === 'unexpected_failure' ? 'error' : fixture.code
      );
      const service = serviceFixture([
        fixture.initial,
        fixture.failed,
        fixture.subsequent,
        fixture.recovered,
      ]);
      const original = await service.read(0);
      expect(original.windows).toHaveLength(2);
      const rejected = await service.read(1);
      expect(rejected.status).toBe(
        fixture.code === 'needs_sign_in' ? 'needs_sign_in' : 'unavailable'
      );
      expect(rejected.windows).toEqual([]);
      expect(rejected.sampledAt).toBeNull();
      expect(rejected).not.toHaveProperty('failureCode');
      const later = await service.read(2);
      expect(later.status).toBe('unavailable');
      expect(later.windows).toEqual([]);
      expect(later.sampledAt).toBeNull();
      const recovered = await service.read(3);
      expect(recovered.status).toBe('ok');
      expect(recovered.windows).toHaveLength(1);
      expect(recovered.windows[0].usedPercent).toBe(0);
      expect(recovered.windows[0].resetAt).toBeNull();
      expect(recovered.sampledAt).toBe('2026-10-01T12:30:00.000Z');
    }
  );

  it.each(fixtures.cases.filter((fixture) => transient.has(fixture.code)))(
    'retains original Node observation only for explicit $code and replaces it after recovery',
    async (fixture) => {
      expect(fixture.failed.failureCode).toBe(fixture.code);
      const service = serviceFixture([fixture.initial, fixture.failed, fixture.recovered]);
      const original = await service.read(0);
      const retained = await service.read(1);
      expect(retained.status).toBe('cached');
      expect(retained.sampledAt).toBe(original.sampledAt);
      expect(retained.fetchedAt).toBe(original.fetchedAt);
      expect(retained.windows).toEqual(original.windows);
      expect(retained.message).toContain('last successful Muse usage reading');
      expect(retained).not.toHaveProperty('failureCode');
      expect(JSON.stringify(retained)).not.toContain('synthetic-fixture');
      const recovered = await service.read(2);
      expect(recovered.status).toBe('ok');
      expect(recovered.windows).toHaveLength(1);
      expect(recovered.windows[0].usedPercent).toBe(0);
      expect(recovered.windows[0].resetAt).toBeNull();
      expect(recovered.sampledAt).toBe('2026-10-01T12:30:00.000Z');
    }
  );

  it('serves a real reading when the portal omits the email but the capsule session names its user', async () => {
    const helper = fixtures.blankPortalEmail;
    expect(helper.status).toBe('ok');
    expect(helper).not.toHaveProperty('failureCode');
    const service = serviceFixture([helper]);
    const account = await service.read(0);
    expect(account.status).toBe('ok');
    expect(account.email).toBe('fixture@example.com');
    expect(account.plan).toBe('Muse Code High Usage');
    expect(account.sampledAt).toBe('2026-10-02T12:48:09.000Z');
    expect(account.message).toBeNull();
    expect(account.windows.map((window) => [window.key, window.label, window.usedPercent])).toEqual(
      [
        ['window', 'Current usage (5-hour)', 0],
        ['weekly', 'Weekly limit', 6.3],
      ]
    );
    expect(JSON.stringify(account)).not.toContain('synthetic-fixture');
    expect(JSON.stringify(account)).not.toContain('synthetic-user');
  });

  it('preserves native 429 cooldown and actual cached timestamps without public classification fields', async () => {
    const fixture = fixtures.nativeRateLimited;
    expect(fixture.nativeCleared).toBe(false);
    expect(fixture.nextRequestAt).toBe(3200);
    expect(fixture.failed.status).toBe('cached');
    expect(fixture.failed.sampledAt).toBe(fixture.initial.sampledAt);
    const service = serviceFixture([fixture.initial, fixture.failed]);
    const original = await service.read(0);
    const cached = await service.read(1);
    expect(cached.sampledAt).toBe(original.sampledAt);
    expect(cached.message).toContain('limiting requests');
    expect(cached.windows).toEqual(original.windows);
    expect(cached).not.toHaveProperty('failureCode');
  });

  it.each([undefined, 'unknown_failure', 'needs_sign_in', 'invalid_response'])(
    'does not retain old quota with missing, unrecognized, or unsafe failure classification %s',
    async (failureCode) => {
      const initial = fixtures.cases[0].initial;
      const unavailable = {
        ...initial,
        status: 'unavailable',
        windows: [],
        sampledAt: null,
        failureCode,
      };
      const service = serviceFixture([initial, unavailable, fixtures.cases[0].subsequent]);
      await service.read(0);
      expect((await service.read(1)).windows).toEqual([]);
      expect((await service.read(2)).windows).toEqual([]);
    }
  );

  it.each([
    { email: null },
    { email: 'other@example.com' },
    { plan: null },
    { plan: 'Other Plan' },
  ])(
    'rejects stale reuse when a transient failure cannot match current account and plan %j',
    async (changed) => {
      const initial = fixtures.cases[0].initial;
      const failed = {
        ...initial,
        ...changed,
        status: 'unavailable',
        windows: [],
        sampledAt: null,
        failureCode: 'rate_limited',
      };
      const service = serviceFixture([initial, failed, fixtures.cases[0].subsequent]);
      await service.read(0);
      expect((await service.read(1)).windows).toEqual([]);
      expect((await service.read(2)).windows).toEqual([]);
    }
  );

  it('clears Muse old quota on an unclassified transport failure before a classified later retry', async () => {
    let phase = 0;
    const initial = fixtures.cases[0].initial;
    const service = new AdditionalAccountService({
      ccsDir: '/tmp/muse-transport-fixture',
      now: () => phase * 31_000,
      readManifest: async () => null,
      runSource: async (source) => {
        if (source.provider !== 'muse' || phase === 0) return JSON.stringify(initial);
        if (phase === 1) throw new AdditionalUsageTransportError(true);
        return JSON.stringify(fixtures.cases[0].subsequent);
      },
    });
    expect((await service.get()).find((row) => row.provider === 'muse')!.windows).toHaveLength(2);
    phase = 1;
    const failed = (await service.get({ refresh: true })).find((row) => row.provider === 'muse')!;
    expect(failed.status).toBe('error');
    expect(failed.windows).toEqual([]);
    phase = 2;
    expect(
      (await service.get({ refresh: true })).find((row) => row.provider === 'muse')!.windows
    ).toEqual([]);
  });

  it('does not attach the previous host sample to the same account after a configured source change', async () => {
    let phase = 0;
    let sshHost = 'fixture-first';
    const service = new AdditionalAccountService({
      ccsDir: '/tmp/muse-source-fixture',
      now: () => phase * 31_000,
      readManifest: async () =>
        JSON.stringify({ version: 1, sources: [{ provider: 'muse', platform: 'mac', sshHost }] }),
      runSource: async () =>
        JSON.stringify(phase === 0 ? fixtures.cases[0].initial : fixtures.cases[0].subsequent),
    });
    expect((await service.get()).find((row) => row.provider === 'muse')!.windows).toHaveLength(2);
    phase = 1;
    sshHost = 'fixture-second';
    const changed = (await service.get({ refresh: true })).find((row) => row.provider === 'muse')!;
    expect(changed.status).toBe('unavailable');
    expect(changed.windows).toEqual([]);
    expect(changed.sampledAt).toBeNull();
  });
});
