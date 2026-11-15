import type { DashboardProvider } from './account-dashboard-types';
import { providerIds, type Assert, type Same, type TableProviderId } from '../../shared/tables';

/** Table rows that are not dashboard providers (`dashboard: false`): failover's OpenCode wallet. */
export type NonDashboardTableProvider = 'opencode-wallet';

/**
 * `DashboardProvider` (account-dashboard-types.ts) names exactly the table's dashboard rows:
 * adding or removing a provider on one side only fails `tsc`.
 */
export type DashboardProviderMatchesTable = Assert<
  Same<Exclude<TableProviderId, NonDashboardTableProvider>, DashboardProvider>
>;

/**
 * The server's provider table: every supported provider, in display order
 * (Claude, Codex, Antigravity, then the rest). Page routes such as
 * /accounts/<provider> resolve only for ids listed here, so a provider that
 * leaves the table stops resolving there too. A view of config/tables/providers.json
 * (`dashboard: true` rows, in file order).
 */
export const DASHBOARD_PROVIDER_IDS: readonly DashboardProvider[] = Object.freeze(
  providerIds((row) => row.dashboard) as DashboardProvider[]
);

export function isDashboardProviderId(value: unknown): value is DashboardProvider {
  return typeof value === 'string' && (DASHBOARD_PROVIDER_IDS as readonly string[]).includes(value);
}
