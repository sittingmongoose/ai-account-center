/**
 * The fixed fixture behind the golden-bytes test of the agent usage snapshot files: the same
 * event renders usage.md, usage.json and history.json before and after the rendering code
 * moves, and the bytes must not change. Every time is fixed; nothing reads the clock.
 */
import {
  accountAnalyticsIdentity,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsObservation,
} from '../../../../src/web-server/services/account-analytics-history';
import { longBurnKey } from '../../../../src/web-server/services/account-analytics-service';
import type { AccountAnalyticsSampleEvent } from '../../../../src/web-server/services/account-analytics-service';
import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../../../../src/web-server/services/account-dashboard-types';
import type {
  SnapshotSignInProfile,
  SnapshotSignInRenewal,
} from '../../../../src/web-server/agent-usage/agent-usage-snapshot';

export const GOLDEN_NOW = Date.parse('2026-10-15T12:00:00.000Z');
export const GOLDEN_SIGN_IN_NOW = Date.parse('2026-10-15T12:00:05.000Z');
const MINUTE = 60_000;

function window(overrides: Partial<DashboardAccountWindow> = {}): DashboardAccountWindow {
  return {
    key: 'five_hour',
    label: '5-hour',
    usedPercent: 40,
    remainingPercent: 60,
    resetAt: '2026-10-15T13:30:00.000Z',
    windowMinutes: 300,
    used: null,
    limit: null,
    unit: null,
    ...overrides,
  };
}

function account(overrides: Partial<DashboardAccount> = {}): DashboardAccount {
  return {
    id: 'codex:gmail',
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'Gmail',
    email: 'person@example.com',
    plan: 'Pro',
    platform: 'ubuntu',
    source: 'Codex usage on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: '2026-10-15T12:00:00.000Z',
    sampledAt: '2026-10-15T12:00:00.000Z',
    isActive: true,
    windows: [window()],
    capabilities: { codexProfile: 'gmail', claudeProfileId: null, claudePlatforms: [] },
    ...overrides,
  };
}

function codexRow(): DashboardAccount {
  return account({
    windows: [
      window(),
      window({
        key: 'weekly_spend',
        label: 'Weekly spend',
        kind: 'spend',
        usedPercent: null,
        remainingPercent: null,
        resetAt: '2026-10-19T00:00:00.000Z',
        windowMinutes: null,
        used: 36,
        limit: 50,
        remaining: 14,
        unit: 'USD',
        expiresAt: '2026-10-20T00:00:00.000Z',
      }),
    ],
  });
}

function claudeRow(): DashboardAccount {
  return account({
    id: 'claude:party',
    provider: 'claude',
    providerLabel: 'Claude',
    label: 'Party',
    email: 'party@example.com',
    plan: null,
    source: 'Verified quota on Ubuntu (fallback)',
    status: 'cached',
    fetchedAt: '2026-10-15T11:30:00.000Z',
    sampledAt: '2026-10-15T11:30:00.000Z',
    isActive: false,
    windows: [
      window({
        usedPercent: 55,
        remainingPercent: 45,
        resetAt: '2026-10-15T16:00:00.000Z',
        status: 'cached',
        sampledAt: '2026-10-15T11:30:00.000Z',
      }),
    ],
    capabilities: { codexProfile: null, claudeProfileId: 'party', claudePlatforms: [] },
  });
}

function zaiRow(): DashboardAccount {
  return account({
    id: 'zai:main',
    provider: 'zai',
    providerLabel: 'Z.ai',
    label: 'GLM coding plan',
    email: 'dev@example.com',
    plan: 'Coding Plan',
    platform: 'mac',
    source: 'Z.ai usage on Mac',
    status: 'needs_sign_in',
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  });
}

function museRow(): DashboardAccount {
  return account({
    id: 'muse:internal',
    provider: 'muse',
    providerLabel: 'Muse Code',
    label: 'Internal build',
    email: null,
    plan: null,
    platform: 'ubuntu',
    source: 'Muse Code usage',
    status: 'ok',
    isActive: false,
    hidden: true,
    windows: [window({ usedPercent: null, remainingPercent: null, unlimited: true, resetAt: null })],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  });
}

function record(
  row: DashboardAccount,
  minutesAgo: number,
  used: number,
  quotaWindow: DashboardAccountWindow
): AccountAnalyticsObservation {
  const at = new Date(GOLDEN_NOW - minutesAgo * MINUTE).toISOString();
  return {
    identity: accountAnalyticsIdentity(row),
    accountId: row.id,
    sampledAt: at,
    observedAt: at,
    platform: row.platform,
    source: row.source,
    status: 'ok',
    isActive: true,
    windows: [
      {
        ...quotaWindow,
        usedPercent: used,
        remainingPercent: 100 - used,
      },
    ],
  };
}

function goldenHistory(
  codex: DashboardAccount,
  claude: DashboardAccount
): AccountAnalyticsHistoryData {
  const codexWindow = codex.windows[0];
  const claudeWindow = claude.windows[0];
  return {
    schemaVersion: 1,
    collectedSince: '2026-10-01T00:00:00.000Z',
    records: [
      record(codex, 60, 10, codexWindow),
      record(codex, 30, 25, codexWindow),
      record(claude, 60, 45, claudeWindow),
      record(claude, 30, 55, claudeWindow),
    ],
  };
}

/** One measured long burn on the codex five-hour window: 7-day covered, 30-day under half. */
function goldenLongBurn(
  codex: DashboardAccount
): NonNullable<AccountAnalyticsSampleEvent['longBurn']> {
  const spans = {
    last7Days: { spanMs: 604_800_000, consumed: 2100, rate: 12.5, coverage: 0.8 },
    last30Days: { spanMs: 2_592_000_000, consumed: 7020, rate: 9.75, coverage: 0.3 },
  };
  return new Map([[longBurnKey(codex, codex.windows[0]), spans]]);
}

/** Two providers with readings (one cached), one needing sign-in, one hidden from the dashboard. */
export function goldenEvent(): AccountAnalyticsSampleEvent {
  const codex = codexRow();
  const claude = claudeRow();
  return {
    accounts: [codex, claude, zaiRow(), museRow()],
    history: goldenHistory(codex, claude),
    sampledAt: GOLDEN_NOW,
    longBurn: goldenLongBurn(codex),
  };
}

/** Every sign-in state, worst first in the rendered section; one renews by itself. */
export function goldenSignIn(): SnapshotSignInProfile[] {
  return [
    {
      name: 'default',
      provider: 'claude',
      platform: 'darwin',
      machine: 'mac',
      configDir: '/Users/dev/.claude',
      accessExpiresAt: '2026-10-16T04:00:00.000Z',
      refreshExpiresAt: '2026-11-12T12:00:00.000Z',
      loggedOutAt: '2026-11-05T12:00:00.000Z',
      state: 'ok',
      fixCommand: 'claude-doctor',
      fieldsAvailable: ['accessExpiresAt', 'refreshExpiresAt'],
      email: 'mac@example.com',
      autoRenew: true,
      renewFrom: '2026-10-20T00:00:00.000Z',
      lastRenewAt: '2026-10-15T06:00:00.000Z',
      lastResult: 'renewed',
    },
    {
      name: 'second',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'vm',
      refreshExpiresAt: '2026-11-01T09:00:00.000Z',
      state: 'ok',
      fixCommand: '',
    },
    {
      name: 'vm',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'vm',
      accessExpiresAt: '2026-10-15T15:00:00.000Z',
      refreshExpiresAt: '2026-10-18T12:00:00.000Z',
      loggedOutAt: '2026-10-15T18:00:00.000Z',
      state: 'warn',
      fixCommand: 'claude-login --profile vm',
    },
    {
      name: 'nas',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'nas1',
      accessExpiresAt: '2026-10-15T12:10:00.000Z',
      refreshExpiresAt: '2026-10-17T12:00:00.000Z',
      loggedOutAt: '2026-10-15T12:30:00.000Z',
      state: 'critical',
      fixCommand: 'claude-login --profile nas',
    },
    {
      name: 'desk',
      provider: 'claude',
      platform: 'win32',
      machine: 'windows',
      loggedOutAt: '2026-10-14T09:00:00.000Z',
      state: 'logged_out',
      fixCommand: 'claude /login:windows',
    },
    {
      name: 'lab',
      provider: 'claude',
      platform: 'darwin',
      machine: 'mac-2',
      state: 'unavailable',
      fixCommand: '',
      error: 'ssh: connect refused (timed out)',
    },
    {
      name: 'build',
      provider: 'muse',
      platform: 'ubuntu',
      machine: 'vm',
      state: 'unsupported',
      fixCommand: '',
    },
  ];
}

export function goldenSignInRenewal(): SnapshotSignInRenewal {
  return {
    runner: 'mac',
    generatedAt: '2026-10-15T11:50:00.000Z',
    stale: false,
    needsYou: [
      {
        email: 'party@example.com',
        reason: 'refresh_expired',
        how: 'Run the sign-in fix on the Windows bar',
        since: '2026-10-15T08:00:00.000Z',
      },
    ],
  };
}
