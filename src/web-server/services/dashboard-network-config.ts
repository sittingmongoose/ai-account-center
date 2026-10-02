import { statSync } from 'fs';
import { getConfigYamlPath, loadOrCreateUnifiedConfig } from '../../config/config-loader-facade';
import { createLogger } from '../../services/logging';
import { parseTrustedNetworks, type TrustedNetwork } from '../middleware/trusted-networks';

/**
 * `dashboard_network` in config.yaml (CONTRACT-auth-devices section 2a, rule 4),
 * read-only and normalised:
 *
 *   dashboard_network:
 *     trust_local_network: true          # default false
 *     trusted_networks: [10.6.0.0/24]    # default: the private ranges
 *
 * Only `true` turns the trust on; anything else, a missing block or a config
 * that cannot be read leaves it off.
 */
export interface DashboardNetworkSettings {
  trustLocalNetwork: boolean;
  networks: readonly TrustedNetwork[];
  /** The ranges as canonical CIDR text, for Settings. */
  trustedNetworks: readonly string[];
  /** Entries of `trusted_networks` that were not valid ranges (left out, never trusted). */
  rejectedEntries: number;
}

const logger = createLogger('dashboard-auth');

function freeze(settings: DashboardNetworkSettings): DashboardNetworkSettings {
  return Object.freeze({
    ...settings,
    networks: Object.freeze([...settings.networks]),
    trustedNetworks: Object.freeze([...settings.trustedNetworks]),
  });
}

export function parseDashboardNetworkSettings(raw: unknown): DashboardNetworkSettings {
  const block =
    raw && typeof raw === 'object' && !Array.isArray(raw) ? (raw as Record<string, unknown>) : {};
  const list = parseTrustedNetworks(block.trusted_networks);
  return freeze({
    trustLocalNetwork: block.trust_local_network === true,
    networks: list.networks,
    trustedNetworks: list.networks.map((network) => network.cidr),
    rejectedEntries: list.rejected,
  });
}

const OFF = parseDashboardNetworkSettings(undefined);

let cache: { key: string; settings: DashboardNetworkSettings } | null = null;
let warnedKey: string | null = null;

/** Cached by config.yaml's path, size and modification time (read on most requests). */
export function getDashboardNetworkSettings(): DashboardNetworkSettings {
  let key = '';
  try {
    const file = getConfigYamlPath();
    const stat = statSync(file, { throwIfNoEntry: false });
    key = stat ? `${file}:${stat.size}:${stat.mtimeMs}` : `${file}:absent`;
    if (cache && cache.key === key) return cache.settings;
  } catch {
    key = '';
  }
  let settings: DashboardNetworkSettings;
  try {
    settings = parseDashboardNetworkSettings(loadOrCreateUnifiedConfig().dashboard_network);
  } catch {
    settings = OFF;
  }
  if (settings.rejectedEntries > 0 && warnedKey !== key) {
    warnedKey = key;
    // Counts only: the entries themselves are never logged.
    logger.warn(
      'auth.network.invalid_ranges',
      'Some dashboard_network.trusted_networks entries are not valid ranges and are not trusted.',
      { rejected: settings.rejectedEntries }
    );
  }
  if (key) cache = { key, settings };
  return settings;
}

/** After this process writes config.yaml: the next read parses it again. */
export function invalidateDashboardNetworkSettings(): void {
  cache = null;
}
