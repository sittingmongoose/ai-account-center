import type { CodexAutoSwitchDashboardStatus, DashboardAccount } from './account-dashboard-types';

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
 * - Antigravity: every registry profile with a verified, available saved login;
 *   a setup row is never a switch target and reads `pending_sign_in`.
 * - Every other provider: not switchable.
 * No sign-in jobs exist yet, so `jobId` is always null here.
 */
export function withAccountState(
  account: DashboardAccount,
  codexAuthValid?: boolean
): DashboardAccount {
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
        typeof account.capabilities.antigravityProfileId === 'string';
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
