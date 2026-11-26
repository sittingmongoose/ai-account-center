import type { ClaudeDashboardPlatform, DashboardAccount } from './account-dashboard-types';
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
 * `lifecycle` for one dashboard row. An Antigravity setup row reads `pending_sign_in` with no usage; a Claude profile
 * from Add keeps `pending_sign_in` until its first reading; every other row is `ready`. Running sign-in jobs are laid
 * over this by withJobState.
 */
export function withAccountState(account: DashboardAccount): DashboardAccount {
  if (account.provider === 'claude' && account.lifecycle?.state === 'pending_sign_in') {
    return account;
  }
  if (isAntigravitySetupRow(account)) {
    return {
      ...account,
      status: 'needs_sign_in',
      windows: [],
      lifecycle: { state: 'pending_sign_in', jobId: null },
    };
  }
  return { ...account, lifecycle: { state: 'ready', jobId: null } };
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
 * usage, and it advertises no platforms: the Open route accepts its id, but no
 * row offers Open for it yet.
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
    lifecycle: { state: 'pending_sign_in', jobId: null },
  };
}
