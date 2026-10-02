import { ADDITIONAL_PROVIDERS } from './account-dashboard-projection';
import type { DashboardAccount, DashboardProvider } from './account-dashboard-types';

/** One row of the server's provider table, as Analytics needs it. */
export interface AnalyticsProviderEntry {
  id: DashboardProvider;
  label: string;
  order: number;
  switchable: boolean;
}

const SWITCHABLE: ReadonlySet<DashboardProvider> = new Set(['codex', 'antigravity']);

/**
 * The provider table the dashboard projection already uses: Claude, Codex,
 * Antigravity, then the other additional providers in their dashboard order.
 * A dashboard response that carries its own `providers` registry overrides
 * labels, order and visibility at request time.
 */
export function defaultAnalyticsProviderTable(): AnalyticsProviderEntry[] {
  const rows: Array<[DashboardProvider, string]> = [
    ['claude', 'Claude'],
    ['codex', 'Codex'],
    ...ADDITIONAL_PROVIDERS,
  ];
  return rows.map(([id, label], order) => ({ id, label, order, switchable: SWITCHABLE.has(id) }));
}

export function defaultAnalyticsProviderIds(): string[] {
  return defaultAnalyticsProviderTable().map((row) => row.id);
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Registry facts a newer dashboard response may carry; absent fields keep the defaults. */
export interface DashboardRegistryFacts {
  table: AnalyticsProviderEntry[];
  visible: (provider: DashboardProvider) => boolean;
  hidden: (account: DashboardAccount) => boolean;
  switchable: (account: DashboardAccount) => boolean;
}

/**
 * Read the optional provider registry and visibility settings from a
 * dashboard response without depending on their types, so this works before
 * and after the registry fields exist.
 */
export function dashboardRegistryFacts(
  dashboard: unknown,
  table: AnalyticsProviderEntry[] = defaultAnalyticsProviderTable()
): DashboardRegistryFacts {
  const source = record(dashboard) ? dashboard : {};
  const entries = Array.isArray(source.providers) ? source.providers.filter(record) : [];
  const settings = record(source.settings) ? source.settings : {};
  const hiddenProviders = new Set(
    Array.isArray(settings.hiddenProviders)
      ? settings.hiddenProviders.filter((id): id is string => typeof id === 'string')
      : []
  );
  const hiddenAccountIds = new Set(
    Array.isArray(settings.hiddenAccountIds)
      ? settings.hiddenAccountIds.filter((id): id is string => typeof id === 'string')
      : []
  );
  const merged = table.map((row) => {
    const entry = entries.find((candidate) => candidate.id === row.id);
    return {
      ...row,
      label:
        typeof entry?.label === 'string' && entry.label.length > 0 && entry.label.length <= 80
          ? entry.label
          : row.label,
      order:
        typeof entry?.order === 'number' && Number.isFinite(entry.order) ? entry.order : row.order,
      switchable: typeof entry?.switchable === 'boolean' ? entry.switchable : row.switchable,
      visible: typeof entry?.visible === 'boolean' ? entry.visible : !hiddenProviders.has(row.id),
    };
  });
  const byId = new Map(merged.map((row) => [row.id, row]));
  return {
    table: merged
      .map(({ visible: _visible, ...row }) => row)
      .sort((a, b) => a.order - b.order || a.id.localeCompare(b.id)),
    visible: (provider) => byId.get(provider)?.visible ?? !hiddenProviders.has(provider),
    hidden: (account) => {
      const own = (account as unknown as { hidden?: unknown }).hidden;
      if (typeof own === 'boolean') return own;
      return !(byId.get(account.provider)?.visible ?? true) || hiddenAccountIds.has(account.id);
    },
    switchable: (account) => {
      const own = (account as unknown as { switchable?: unknown }).switchable;
      if (typeof own === 'boolean') return own;
      if (!(byId.get(account.provider)?.switchable ?? false)) return false;
      if (account.provider === 'codex')
        return account.capabilities.codexProfile !== null && account.status !== 'needs_sign_in';
      if (account.provider === 'antigravity')
        return (
          typeof account.capabilities.antigravityProfileId === 'string' &&
          (account.capabilities.antigravityCanActivate === true || account.isActive)
        );
      return false;
    },
  };
}
