import type { CodexAuthProfilesSummary } from '../../codex-auth/codex-auth-dashboard-service';
import type { BarSummaryRow } from '../routes/bar-routes';
import {
  canOpenClaudeMacProfile,
  type ClaudeDesktopProfile,
} from './claude-desktop-profile-service';
import type { ClaudeDesktopUsage } from './claude-desktop-usage-service';
import type { ClaudeDesktopLiveUsage } from './claude-desktop-live-service';
import type {
  ClaudeDashboardPlatform,
  DashboardAccount,
  DashboardAccountWindow,
  DashboardProvider,
} from './account-dashboard-types';

export const ADDITIONAL_PROVIDERS: Array<[DashboardProvider, string]> = [
  ['antigravity', 'Antigravity'],
  ['muse', 'Muse Code'],
  ['cursor', 'Cursor'],
  ['kimi-code', 'Kimi Code'],
  ['qwen', 'Qwen token plan'],
  ['zai', 'Z.ai coding plan'],
  ['opencode-go', 'OpenCode Go'],
];

function timestamp(value: unknown): string | null {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}T/.test(value)) return null;
  const parsed = new Date(value);
  return Number.isFinite(parsed.getTime()) ? parsed.toISOString() : null;
}

function number(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function text(value: unknown, maxLength = 120): string | null {
  if (typeof value !== 'string' || /[\u0000-\u001f\u007f]/.test(value)) return null;
  const trimmed = value.trim();
  return trimmed && trimmed.length <= maxLength ? trimmed : null;
}

function email(value: unknown): string | null {
  const result = text(value, 254);
  return result && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(result) ? result : null;
}

export const emailForComparison = email;

function emptyCapabilities(): DashboardAccount['capabilities'] {
  return { codexProfile: null, claudeProfileId: null, claudePlatforms: [] };
}

function quotaWindow(window: Partial<DashboardAccountWindow>): DashboardAccountWindow {
  const usedPercent = number(window.usedPercent);
  const sampledAt = window.status === 'cached' ? timestamp(window.sampledAt) : null;
  return {
    key: text(window.key, 64) ?? 'usage',
    label: text(window.label, 80) ?? 'Usage',
    usedPercent,
    remainingPercent:
      number(window.remainingPercent) ??
      (usedPercent === null ? null : Math.max(0, 100 - usedPercent)),
    resetAt: timestamp(window.resetAt),
    windowMinutes: number(window.windowMinutes),
    used: number(window.used),
    limit: number(window.limit),
    unit: text(window.unit, 40),
    ...(window.kind && ['rate_limit', 'balance', 'spend', 'extra_usage'].includes(window.kind)
      ? { kind: window.kind }
      : {}),
    ...(window.remaining !== undefined
      ? {
          remaining:
            typeof window.remaining === 'number' && Number.isFinite(window.remaining)
              ? window.remaining
              : null,
        }
      : {}),
    ...(window.expiresAt !== undefined ? { expiresAt: timestamp(window.expiresAt) } : {}),
    ...(typeof window.unlimited === 'boolean' ? { unlimited: window.unlimited } : {}),
    ...(typeof window.enabled === 'boolean' ? { enabled: window.enabled } : {}),
    ...(sampledAt ? { status: 'cached' as const, sampledAt } : {}),
  };
}

export function codexAccount(
  profile: CodexAuthProfilesSummary['profiles'][number],
  activated: CodexAuthProfilesSummary['activated'],
  row: BarSummaryRow | undefined
): DashboardAccount {
  const connected = profile.authValid;
  const status =
    !connected || row?.needsReauth
      ? 'needs_sign_in'
      : row?.quotaStatus === 'ok'
        ? row.cached
          ? 'cached'
          : 'ok'
        : row?.quotaStatus === 'error'
          ? 'error'
          : 'unavailable';
  return {
    id: `codex:${profile.name}`,
    provider: 'codex',
    providerLabel: 'Codex',
    label: profile.name,
    email: email(profile.email),
    plan: text(profile.plan, 80),
    platform: 'ubuntu',
    source: row?.quotaSource === 'local' ? 'Codex local quota' : 'Codex saved login on Ubuntu',
    status,
    message:
      status === 'needs_sign_in'
        ? 'This saved Codex login needs to be renewed.'
        : status === 'error' || status === 'unavailable'
          ? 'Codex usage is temporarily unavailable.'
          : null,
    fetchedAt: timestamp(row?.fetchedAt),
    // A local fallback reading is fetched now but was written when its session file
    // last changed (staleAsOf, set once that is over five minutes ago).
    sampledAt: timestamp(row?.staleAsOf) ?? timestamp(row?.fetchedAt),
    isActive: activated?.name === profile.name,
    windows:
      connected && row?.quotaStatus === 'ok'
        ? [...(row.quotaWindows ?? []), ...(row.balanceWindows ?? [])].map((window) =>
            quotaWindow(window)
          )
        : [],
    capabilities: { ...emptyCapabilities(), codexProfile: profile.name },
  };
}

export function claudeAccount(
  profile: ClaudeDesktopProfile,
  platform: ClaudeDashboardPlatform,
  usage?: ClaudeDesktopUsage['profiles'][number]
): DashboardAccount {
  const status =
    usage?.status === 'needs-sign-in'
      ? 'needs_sign_in'
      : usage?.status === 'cached'
        ? 'cached'
        : 'unavailable';
  const windows: DashboardAccountWindow[] = [];
  for (const [field, key, label, minutes] of [
    ['fiveHour', 'five_hour', '5-hour limit', 300],
    ['weekly', 'seven_day', 'Weekly limit', 10080],
    ['weeklyOpus', 'seven_day_opus', 'Opus weekly', 10080],
    ['weeklySonnet', 'seven_day_sonnet', 'Sonnet weekly', 10080],
    ['extra', 'extra_usage', 'Extra usage', null],
  ] as const) {
    const usedPercent = number(usage?.utilization[field]);
    if (usedPercent !== null)
      windows.push(
        quotaWindow({
          key,
          label,
          usedPercent,
          windowMinutes: minutes,
          ...(field === 'extra' ? { kind: 'extra_usage' as const } : {}),
        })
      );
  }
  return {
    id: `claude:${profile.id ?? profile.email}`,
    provider: 'claude',
    providerLabel: 'Claude',
    label: profile.id ?? profile.email,
    email: email(profile.email),
    plan: null,
    platform,
    source: `Claude desktop on ${platform === 'mac' ? 'Mac' : 'Windows'}`,
    status,
    message:
      status === 'needs_sign_in'
        ? 'No usage sample is available. This profile may need its first sign-in on this computer.'
        : status === 'unavailable'
          ? 'Claude desktop usage is temporarily unavailable.'
          : null,
    fetchedAt: timestamp(usage?.fetchedAt),
    sampledAt: timestamp(usage?.sampledAt),
    isActive: false,
    windows,
    capabilities: {
      codexProfile: null,
      claudeProfileId: profile.id ?? null,
      claudePlatforms: [
        ...(canOpenClaudeMacProfile(profile) ? ['mac' as const] : []),
        ...(profile.id && profile.windows ? ['windows' as const] : []),
      ],
    },
  };
}

export function applyClaudeLiveUsage(
  account: DashboardAccount,
  live: ClaudeDesktopLiveUsage | null
): DashboardAccount {
  if (
    !live ||
    live.profileId !== account.capabilities.claudeProfileId ||
    live.email !== account.email ||
    !timestamp(live.fetchedAt) ||
    !Array.isArray(live.windows) ||
    live.windows.length === 0
  )
    return account;
  return {
    ...account,
    status: 'ok',
    source: 'Claude Desktop live quota on Windows',
    plan: live.plan === 'max' || live.plan === 'pro' ? live.plan : account.plan,
    message: null,
    fetchedAt: timestamp(live.fetchedAt),
    sampledAt: timestamp(live.fetchedAt),
    windows: live.windows.slice(0, 256).map(quotaWindow),
  };
}

export function additionalFallback(
  provider: DashboardProvider,
  providerLabel: string
): DashboardAccount {
  return {
    id: `${provider}:usage`,
    provider,
    providerLabel,
    label: providerLabel,
    email: null,
    plan: null,
    platform: 'ubuntu',
    source: 'Saved account usage',
    status: 'unavailable',
    message: 'Account usage is temporarily unavailable.',
    fetchedAt: null,
    sampledAt: null,
    isActive: false,
    windows: [],
    capabilities: emptyCapabilities(),
  };
}

/** Project only documented fields; added providers never acquire activation controls. */
export function additionalAccounts(rows: DashboardAccount[]): DashboardAccount[] {
  const wallets = rows.filter(
    (row) =>
      row.provider === 'opencode-go' &&
      /^plan-opencode-go-console-mac-[a-f0-9]{12}$/.test(row.id) &&
      row.platform === 'mac' &&
      row.source === 'Authenticated OpenCode console workspace on Mac' &&
      row.label === 'OpenCode console wallet' &&
      row.email === null &&
      (row.status === 'ok' || row.status === 'cached') &&
      Array.isArray(row.windows) &&
      row.windows.some(
        (window) =>
          window.key === 'zen-balance' &&
          window.kind === 'balance' &&
          window.unit === 'USD' &&
          typeof window.remaining === 'number' &&
          Number.isFinite(window.remaining)
      )
  );
  const projected = (
    row: DashboardAccount,
    provider: DashboardProvider,
    providerLabel: string
  ): DashboardAccount => {
    const fallback = additionalFallback(provider, providerLabel);
    return {
      id: /^[a-zA-Z0-9][a-zA-Z0-9:_-]{0,127}$/.test(row.id) ? row.id : fallback.id,
      provider,
      providerLabel,
      label: text(row.label) ?? providerLabel,
      email: email(row.email),
      plan: text(row.plan, 80),
      platform: row.platform === 'mac' || row.platform === 'windows' ? row.platform : 'ubuntu',
      source: text(row.source, 80) ?? fallback.source,
      status: ['ok', 'cached', 'unavailable', 'error', 'needs_sign_in'].includes(row.status)
        ? row.status
        : 'unavailable',
      message: text(row.message, 300),
      fetchedAt: timestamp(row.fetchedAt),
      sampledAt: timestamp(row.sampledAt),
      isActive: false,
      windows: Array.isArray(row.windows) ? row.windows.slice(0, 256).map(quotaWindow) : [],
      capabilities: emptyCapabilities(),
    };
  };
  const accounts = ADDITIONAL_PROVIDERS.map(([provider, providerLabel]) => {
    const row = rows.find(
      (candidate) =>
        candidate.provider === provider && !candidate.id.startsWith('plan-opencode-go-console-')
    );
    return row
      ? projected(row, provider, providerLabel)
      : additionalFallback(provider, providerLabel);
  });
  // A console workspace has not been linked to the API key account. Preserve
  // its distinct identity rather than attaching a wallet to the primary card.
  for (const wallet of wallets.slice(0, 4)) {
    if (!accounts.some((account) => account.id === wallet.id))
      accounts.push(projected(wallet, 'opencode-go', 'OpenCode Go'));
  }
  return accounts;
}
