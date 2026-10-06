import { createHash } from 'crypto';
import type {
  AccountDashboard,
  DashboardAccount,
  DashboardAccountWindow,
} from '../services/account-dashboard-types';
import type {
  ClaudeUsageBody,
  ClaudeUsageWindowBody,
  CodexUsageBody,
  CodexUsageWindowBody,
  UsageHubAuthFile,
  UsageHubProvider,
} from './usage-hub-contract';

/**
 * Dashboard rows -> what the usage hub serves. Every output object is built
 * field by field from an allowlist (never spread from an input), so nothing a
 * collector attached to a row (paths, messages, sources, credentials) can
 * reach T3. Readings are the dashboard's own: the same windows, percentages
 * and reset times, from its cache.
 */
export interface UsageHubAccount {
  authFile: UsageHubAuthFile;
  /** The cached upstream-shaped answer, or null when AAC has no usable reading. */
  usage: CodexUsageBody | ClaudeUsageBody | null;
}

/** Model-scoped Claude weeklies T3 can show ("Weekly · <model>"); other weeklies are left out. */
const CLAUDE_MODEL_SCOPES: Readonly<Record<string, string>> = {
  seven_day_opus: 'Opus',
  seven_day_sonnet: 'Sonnet',
  seven_day_fable: 'Fable',
  seven_day_haiku: 'Haiku',
};

/** Stable and opaque: the same AAC account always gets the same index. */
export function usageHubAuthIndex(accountId: string): string {
  return createHash('sha256')
    .update(`aac-usage-hub\0${accountId}`, 'utf8')
    .digest('hex')
    .slice(0, 16);
}

function isHubProvider(provider: string): provider is UsageHubProvider {
  return provider === 'codex' || provider === 'claude';
}

/** A window T3 may show: a real percentage whose reset has not passed (no stale bar, no invented 0%). */
function usableWindow(window: DashboardAccountWindow): window is DashboardAccountWindow & {
  usedPercent: number;
} {
  return (
    typeof window.usedPercent === 'number' &&
    Number.isFinite(window.usedPercent) &&
    window.usedPercent >= 0 &&
    window.resetPassed !== true &&
    (window.kind === undefined || window.kind === 'rate_limit')
  );
}

function isoOrNull(value: string | null | undefined): string | null {
  if (typeof value !== 'string') return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? new Date(parsed).toISOString() : null;
}

function epochSecondsOrNull(value: string | null | undefined): number | null {
  const iso = isoOrNull(value);
  if (!iso) return null;
  const seconds = Math.floor(Date.parse(iso) / 1000);
  return seconds > 0 ? seconds : null;
}

function codexWindow(window: DashboardAccountWindow | undefined): CodexUsageWindowBody | null {
  if (!window || !usableWindow(window)) return null;
  return {
    used_percent: window.usedPercent,
    reset_at: epochSecondsOrNull(window.resetAt),
    ...(typeof window.windowMinutes === 'number' && window.windowMinutes > 0
      ? { limit_window_seconds: Math.round(window.windowMinutes * 60) }
      : {}),
  };
}

export function codexUsageBody(account: DashboardAccount): CodexUsageBody | null {
  const find = (key: string) => account.windows.find((window) => window.key === key);
  const primary = codexWindow(find('five_hour'));
  const secondary = codexWindow(find('seven_day'));
  if (!primary && !secondary) return null;
  const plan = typeof account.plan === 'string' && /^[a-z0-9_]{1,64}$/.test(account.plan);
  return {
    ...(plan ? { plan_type: account.plan as string } : {}),
    rate_limit: { primary_window: primary, secondary_window: secondary },
  };
}

function claudeWindow(window: DashboardAccountWindow | undefined): ClaudeUsageWindowBody | null {
  if (!window || !usableWindow(window)) return null;
  return { utilization: window.usedPercent, resets_at: isoOrNull(window.resetAt) };
}

export function claudeUsageBody(account: DashboardAccount): ClaudeUsageBody | null {
  const find = (key: string) => account.windows.find((window) => window.key === key);
  const five_hour = claudeWindow(find('five_hour'));
  const seven_day = claudeWindow(find('seven_day'));
  const limits: ClaudeUsageBody['limits'] = [];
  for (const window of account.windows) {
    const displayName = CLAUDE_MODEL_SCOPES[window.key];
    if (!displayName || !usableWindow(window)) continue;
    if (limits.some((limit) => limit.scope.model.display_name === displayName)) continue;
    limits.push({
      kind: 'weekly_scoped',
      percent: window.usedPercent,
      resets_at: isoOrNull(window.resetAt),
      scope: { model: { display_name: displayName } },
    });
  }
  if (!five_hour && !seven_day && limits.length === 0) return null;
  return { five_hour, seven_day, limits };
}

function sampledAt(account: DashboardAccount): number {
  const value = Date.parse(account.sampledAt ?? account.fetchedAt ?? '');
  return Number.isFinite(value) ? value : -Infinity;
}

function usageFor(account: DashboardAccount): CodexUsageBody | ClaudeUsageBody | null {
  return account.provider === 'codex' ? codexUsageBody(account) : claudeUsageBody(account);
}

/**
 * One row per Codex and Claude account across the Mac and Windows dashboard
 * projections (Claude rows differ per desktop; Codex rows are the same). Per
 * account the row with a usable reading wins, then the newer sample, as the
 * Analytics sampler chooses. Claude profiles still waiting for their first
 * sign-in are not accounts yet and are left out.
 */
export function mergeHubRows(dashboards: readonly AccountDashboard[]): DashboardAccount[] {
  const order: string[] = [];
  const chosen = new Map<string, DashboardAccount>();
  for (const dashboard of dashboards) {
    for (const account of dashboard.accounts) {
      if (!isHubProvider(account.provider)) continue;
      if (account.lifecycle?.state === 'pending_sign_in') continue;
      const previous = chosen.get(account.id);
      if (!previous) {
        order.push(account.id);
        chosen.set(account.id, account);
        continue;
      }
      const usable = usageFor(account) !== null;
      const previousUsable = usageFor(previous) !== null;
      if (
        (usable && !previousUsable) ||
        (usable === previousUsable && sampledAt(account) > sampledAt(previous))
      ) {
        chosen.set(account.id, account);
      }
    }
  }
  const codex = order.filter((id) => chosen.get(id)?.provider === 'codex');
  const claude = order.filter((id) => chosen.get(id)?.provider === 'claude');
  return [...codex, ...claude].map((id) => chosen.get(id) as DashboardAccount);
}

const MESSAGES = {
  active: 'Cached AI Account Center reading.',
  needsSignIn: 'This account needs to sign in again in AI Account Center.',
  noReading: 'AI Account Center has no current reading for this account.',
} as const;

export function projectHubAccount(account: DashboardAccount): UsageHubAccount | null {
  if (!isHubProvider(account.provider)) return null;
  const provider = account.provider;
  const usage = usageFor(account);
  const label =
    typeof account.label === 'string' && account.label.length > 0 ? account.label : account.id;
  const authFile: UsageHubAuthFile = {
    id: account.id,
    auth_index: usageHubAuthIndex(account.id),
    provider,
    type: provider,
    label,
    ...(typeof account.email === 'string' && account.email.length > 0
      ? { email: account.email }
      : {}),
    disabled: false,
    status: usage ? 'active' : 'error',
    status_message: usage
      ? MESSAGES.active
      : account.status === 'needs_sign_in'
        ? MESSAGES.needsSignIn
        : MESSAGES.noReading,
    sampled_at: usage ? isoOrNull(account.sampledAt ?? account.fetchedAt) : null,
  };
  return { authFile, usage };
}

export function projectHubAccounts(dashboards: readonly AccountDashboard[]): UsageHubAccount[] {
  return mergeHubRows(dashboards)
    .map(projectHubAccount)
    .filter((account): account is UsageHubAccount => account !== null);
}
