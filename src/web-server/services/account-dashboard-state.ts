import type {
  ClaudeDashboardPlatform,
  CodexAutoSwitchDashboardStatus,
  DashboardAccount,
} from './account-dashboard-types';
import type { PendingClaudeProfile } from './claude-account-stores';

/**
 * An Antigravity registry profile that cannot be used yet: its saved login is
 * missing, unverified or unavailable (CONTRACT-serving-misc section 4.3). The
 * runtime only lists such a profile with `needs_sign_in` and no Ubuntu host.
 */
export function isAntigravitySetupRow(account: DashboardAccount): boolean {
  return (
    account.provider === 'antigravity' &&
    typeof account.capabilities.antigravityProfileId === 'string' &&
    (account.status === 'needs_sign_in' ||
      account.capabilities.antigravityHostIds?.includes('ubuntu') !== true)
  );
}

/**
 * `switchable` and `lifecycle` for one dashboard row.
 * - Codex: every profile with a valid saved login.
 * - Antigravity: every registry profile with a verified, available saved login,
 *   except one whose latest reading could not be bound to the saved account
 *   (`statusReason: 'identity_unbound'`, set by src/antigravity/usage-service.ts).
 *   Any other reading, a failed one included, keeps the row switchable:
 *   activation still does its own identity and runtime proof. A setup row is
 *   never a switch target and reads `pending_sign_in`.
 * - Every other provider: not switchable.
 * A Claude profile from Add keeps `pending_sign_in` until its first reading.
 * Running sign-in jobs are laid over this by withJobState.
 */
export function withAccountState(
  account: DashboardAccount,
  codexAuthValid?: boolean
): DashboardAccount {
  if (account.provider === 'claude' && account.lifecycle?.state === 'pending_sign_in') {
    return { ...account, switchable: false };
  }
  if (isAntigravitySetupRow(account)) {
    return {
      ...account,
      status: 'needs_sign_in',
      windows: [],
      capabilities: { ...account.capabilities, antigravityCanActivate: false },
      switchable: false,
      lifecycle: { state: 'pending_sign_in', jobId: null },
    };
  }
  const switchable =
    account.provider === 'codex'
      ? (codexAuthValid ?? account.status !== 'needs_sign_in')
      : account.provider === 'antigravity' &&
        typeof account.capabilities.antigravityProfileId === 'string' &&
        account.statusReason !== 'identity_unbound';
  return { ...account, switchable, lifecycle: { state: 'ready', jobId: null } };
}

/** Both switchable sections in one unit: the Codex threshold also in % used. */
export function withThresholdUsedPercent(
  status: Omit<CodexAutoSwitchDashboardStatus, 'thresholdUsedPercent'> & {
    thresholdUsedPercent?: number;
  }
): CodexAutoSwitchDashboardStatus {
  return { ...status, thresholdUsedPercent: 100 - status.thresholdPercent };
}

/** Running sign-in jobs by account id. */
export type AccountJobStates = ReadonlyMap<
  string,
  { state: 'signing_in' | 'verifying'; jobId: string }
>;

/** A row whose account has a running Sign in again job reads signing_in or verifying. */
export function withJobState(account: DashboardAccount, jobs: AccountJobStates): DashboardAccount {
  const job = jobs.get(account.id);
  return job ? { ...account, lifecycle: { state: job.state, jobId: job.jobId } } : account;
}

/**
 * A Claude profile made by Add that has not signed in yet. It has no email or
 * usage, and no Open buttons: the Claude Open route reads only the inventory.
 */
export function pendingClaudeAccount(
  profile: PendingClaudeProfile,
  platform: ClaudeDashboardPlatform
): DashboardAccount {
  return {
    id: `claude:${profile.id}`,
    provider: 'claude',
    providerLabel: 'Claude',
    label: profile.label ?? profile.id,
    email: null,
    plan: null,
    platform,
    source: `Claude desktop on ${platform === 'mac' ? 'Mac' : 'Windows'}`,
    status: 'needs_sign_in',
    message: 'Open Claude on Mac or Windows and sign in to finish setting up this profile.',
    fetchedAt: null,
    sampledAt: null,
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: profile.id, claudePlatforms: [] },
    switchable: false,
    lifecycle: { state: 'pending_sign_in', jobId: null },
  };
}
