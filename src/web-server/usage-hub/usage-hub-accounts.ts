import { peekAccountDashboard } from '../services/account-dashboard-service';
import type {
  AccountDashboard,
  ClaudeDashboardPlatform,
} from '../services/account-dashboard-types';
import { projectHubAccounts, type UsageHubAccount } from './usage-hub-projection';

/**
 * The hub's account list, from the dashboard's cache only. `peekAccountDashboard`
 * never starts a collection, so a T3 poll never adds a provider quota read; the
 * dashboard's own refresh (the Analytics sampler, every refresh interval, for
 * both desktops) keeps the cache current. One T3 poll is a burst of one
 * auth-files and a few api-calls within a second, so the projection is kept
 * for a few seconds and concurrent callers share one read.
 */
export type PeekDashboard = (platform: ClaudeDashboardPlatform) => Promise<AccountDashboard>;

const PLATFORMS: readonly ClaudeDashboardPlatform[] = ['mac', 'windows'];
export const USAGE_HUB_ACCOUNTS_TTL_MS = 5_000;

export interface UsageHubAccountSource {
  read(): Promise<UsageHubAccount[]>;
}

export function createUsageHubAccountSource(
  options: { peek?: PeekDashboard; now?: () => number; ttlMs?: number } = {}
): UsageHubAccountSource {
  const peek = options.peek ?? ((platform) => peekAccountDashboard(platform));
  const now = options.now ?? Date.now;
  const ttlMs = options.ttlMs ?? USAGE_HUB_ACCOUNTS_TTL_MS;
  let cached: { at: number; accounts: UsageHubAccount[] } | null = null;
  let pending: Promise<UsageHubAccount[]> | null = null;
  return {
    read() {
      if (cached && now() - cached.at < ttlMs) return Promise.resolve(cached.accounts);
      if (pending) return pending;
      pending = Promise.allSettled(PLATFORMS.map((platform) => peek(platform)))
        .then((results) => {
          const dashboards = results.flatMap((result) =>
            result.status === 'fulfilled' ? [result.value] : []
          );
          const accounts = projectHubAccounts(dashboards);
          cached = { at: now(), accounts };
          return accounts;
        })
        .finally(() => {
          pending = null;
        });
      return pending;
    },
  };
}
