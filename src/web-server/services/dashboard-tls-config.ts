import { statSync } from 'fs';
import * as net from 'net';
import os from 'os';
import path from 'path';
import { getConfigYamlPath, loadOrCreateUnifiedConfig } from '../../config/config-loader-facade';
import { createLogger } from '../../services/logging';
import type { TrustedProxyKind } from '../middleware/secure-transport';
import {
  DEFAULT_TRUSTED_NETWORKS,
  formatAddress,
  isAddressInNetworks,
  isLoopbackAddress,
  parseAddress,
  parseTrustedNetworks,
  type ParsedAddress,
} from '../middleware/trusted-networks';

/**
 * `dashboard_tls` in config.yaml (CONTRACT-auth-devices section 2a), read-only
 * and normalised. Every key is optional and the block is off by default:
 * nothing here turns on a listener or trusts a proxy unless config.yaml says so.
 * A malformed value is treated as absent.
 */
export interface DashboardHttpsListenerSettings {
  port: number;
  certPath: string;
  keyPath: string;
}

export interface DashboardTlsSettings {
  trustedProxy: TrustedProxyKind | null;
  /**
   * The `lan-https-proxy` addresses (`trusted_proxy_addresses`), canonical, at
   * most 8; empty unless that kind is on.
   */
  trustedProxyAddresses: readonly string[];
  /**
   * Why `trusted_proxy: lan-https-proxy` was turned off (an address the list
   * may not hold), or null. The kind fails closed: one bad entry and no peer
   * counts as the proxy.
   */
  trustedProxyProblem: TrustedProxyProblem | null;
  httpsListener: DashboardHttpsListenerSettings | null;
  /** An `https://` origin (scheme, host and optional port only), or null. */
  publicOrigin: string | null;
}

const NONE: DashboardTlsSettings = Object.freeze({
  trustedProxy: null,
  trustedProxyAddresses: Object.freeze([]) as readonly string[],
  trustedProxyProblem: null,
  httpsListener: null,
  publicOrigin: null,
});

const logger = createLogger('dashboard-auth');

function expandHome(value: string): string {
  return value === '~' || value.startsWith('~/') ? path.join(os.homedir(), value.slice(1)) : value;
}

/** An `https://` origin (scheme, host and optional port only), or null. */
export function parsePublicOrigin(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > 512) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.username || url.password) return null;
    if ((url.pathname !== '/' && url.pathname !== '') || url.search || url.hash) return null;
    return url.origin;
  } catch {
    return null;
  }
}

export const TRUSTED_PROXY_ADDRESSES_MAX = 8;

export type TrustedProxyProblemReason =
  | 'not_a_list'
  | 'empty'
  | 'too_many'
  | 'not_an_address'
  | 'unspecified'
  | 'loopback'
  | 'not_private'
  | 'this_computer';

export interface TrustedProxyProblem {
  reason: TrustedProxyProblemReason;
  /** The 0-based position of the refused entry, when one entry was refused. */
  index: number | null;
}

const PROBLEM_TEXT: Record<TrustedProxyProblemReason, string> = {
  not_a_list: 'trusted_proxy_addresses is not a list',
  empty: 'trusted_proxy_addresses is empty',
  too_many: `trusted_proxy_addresses has more than ${TRUSTED_PROXY_ADDRESSES_MAX} entries`,
  not_an_address: 'an entry is not one exact IP address (no ranges, names or ports)',
  unspecified: 'an entry is the unspecified address (0.0.0.0 or ::)',
  loopback: 'an entry is a loopback address (this computer)',
  not_private: 'an entry is not a private LAN address (10/8, 172.16/12, 192.168/16, fc00::/7)',
  this_computer: "an entry is one of this computer's own addresses",
};

/** One plain sentence for a refused proxy list (the status command and the log). */
export function describeTrustedProxyProblem(problem: TrustedProxyProblem): string {
  const where = problem.index === null ? '' : ` (entry ${problem.index + 1})`;
  return `${PROBLEM_TEXT[problem.reason]}${where}`;
}

const PRIVATE_NETWORKS = parseTrustedNetworks(DEFAULT_TRUSTED_NETWORKS).networks;

function isUnspecified(address: ParsedAddress): boolean {
  return address.bytes.every((byte) => byte === 0);
}

let selfAddressesOverride: readonly string[] | null = null;

/** Tests only: this computer's addresses as the parser sees them; null reads the interfaces again. */
export function setThisComputerAddressesForTests(addresses: readonly string[] | null): void {
  selfAddressesOverride = addresses;
  cache = null;
}

/** Every address of this computer's interfaces, loopback included. */
export function thisComputerAddresses(): string[] {
  if (selfAddressesOverride) return [...selfAddressesOverride];
  const result: string[] = [];
  try {
    for (const entries of Object.values(os.networkInterfaces())) {
      for (const entry of entries ?? []) result.push(entry.address);
    }
  } catch {
    /* No interface list: loopback and the private-range rule still apply. */
  }
  return result;
}

export interface TrustedProxyAddressOptions {
  /** This computer's addresses; defaults to its network interfaces. */
  selfAddresses?: readonly string[];
}

export type TrustedProxyAddressList =
  | { ok: true; addresses: string[] }
  | { ok: false; problem: TrustedProxyProblem };

/**
 * `dashboard_tls.trusted_proxy_addresses`: 1 to 8 exact addresses of the
 * `lan-https-proxy`, each a private LAN address (10/8, 172.16/12, 192.168/16,
 * fc00::/7, IPv4-mapped forms included) that is not loopback, not the
 * unspecified address and not one of this computer's own addresses. A CIDR, a
 * name, a port, padding or anything else is refused. One refused entry refuses
 * the whole list, so the kind fails closed: a typo trusts nothing, never more.
 * Duplicates are dropped; the result is canonical (`::ffff:10.0.0.5` becomes
 * `10.0.0.5`).
 */
export function parseTrustedProxyAddresses(
  raw: unknown,
  options: TrustedProxyAddressOptions = {}
): TrustedProxyAddressList {
  if (!Array.isArray(raw)) return { ok: false, problem: { reason: 'not_a_list', index: null } };
  if (raw.length === 0) return { ok: false, problem: { reason: 'empty', index: null } };
  if (raw.length > TRUSTED_PROXY_ADDRESSES_MAX) {
    return { ok: false, problem: { reason: 'too_many', index: null } };
  }
  const self = (options.selfAddresses ?? thisComputerAddresses())
    .map((value) => parseAddress(value))
    .filter((value): value is ParsedAddress => value !== null);
  const addresses: string[] = [];
  for (const [index, entry] of raw.entries()) {
    const refuse = (reason: TrustedProxyProblemReason): TrustedProxyAddressList => ({
      ok: false,
      problem: { reason, index },
    });
    if (typeof entry !== 'string' || entry.length === 0 || entry.length > 64) {
      return refuse('not_an_address');
    }
    if (entry.includes('%') || net.isIP(entry) === 0) return refuse('not_an_address');
    const parsed = parseAddress(entry);
    if (!parsed) return refuse('not_an_address');
    if (isUnspecified(parsed)) return refuse('unspecified');
    if (isLoopbackAddress(parsed)) return refuse('loopback');
    if (!isAddressInNetworks(parsed, PRIVATE_NETWORKS)) return refuse('not_private');
    const canonical = formatAddress(parsed);
    if (self.some((own) => formatAddress(own) === canonical)) return refuse('this_computer');
    if (!addresses.includes(canonical)) addresses.push(canonical);
  }
  return { ok: true, addresses };
}

export function parseDashboardTlsSettings(
  raw: unknown,
  options: TrustedProxyAddressOptions = {}
): DashboardTlsSettings {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return NONE;
  const block = raw as Record<string, unknown>;
  let trustedProxy: TrustedProxyKind | null =
    block.trusted_proxy === 'tailscale-serve' ||
    block.trusted_proxy === 'loopback-https-proxy' ||
    block.trusted_proxy === 'lan-https-proxy'
      ? block.trusted_proxy
      : null;
  let trustedProxyAddresses: string[] = [];
  let trustedProxyProblem: TrustedProxyProblem | null = null;
  if (trustedProxy === 'lan-https-proxy') {
    const list = parseTrustedProxyAddresses(block.trusted_proxy_addresses, options);
    if (list.ok) {
      trustedProxyAddresses = list.addresses;
    } else {
      trustedProxy = null;
      trustedProxyProblem = list.problem;
    }
  }
  let httpsListener: DashboardHttpsListenerSettings | null = null;
  const listener = block.https_listener;
  if (listener && typeof listener === 'object' && !Array.isArray(listener)) {
    const entry = listener as Record<string, unknown>;
    const port = entry.port ?? 3443;
    if (
      entry.enabled === true &&
      typeof port === 'number' &&
      Number.isInteger(port) &&
      port > 0 &&
      port < 65536 &&
      typeof entry.cert_path === 'string' &&
      entry.cert_path.length > 0 &&
      typeof entry.key_path === 'string' &&
      entry.key_path.length > 0
    ) {
      httpsListener = {
        port,
        certPath: path.resolve(expandHome(entry.cert_path)),
        keyPath: path.resolve(expandHome(entry.key_path)),
      };
    }
  }
  return {
    trustedProxy,
    trustedProxyAddresses,
    trustedProxyProblem,
    httpsListener,
    publicOrigin: parsePublicOrigin(block.public_origin),
  };
}

let cache: { key: string; settings: DashboardTlsSettings } | null = null;
let warnedKey: string | null = null;

/** Cached by config.yaml's path, size and modification time (read on most requests). */
export function getDashboardTlsSettings(): DashboardTlsSettings {
  let key = '';
  try {
    const file = getConfigYamlPath();
    const stat = statSync(file, { throwIfNoEntry: false });
    key = stat ? `${file}:${stat.size}:${stat.mtimeMs}` : `${file}:absent`;
    if (cache && cache.key === key) return cache.settings;
  } catch {
    key = '';
  }
  let settings: DashboardTlsSettings;
  try {
    settings = parseDashboardTlsSettings(loadOrCreateUnifiedConfig().dashboard_tls);
  } catch {
    settings = NONE;
  }
  const problem = settings.trustedProxyProblem;
  if (problem && warnedKey !== key) {
    // Once per config.yaml change. The reason and position only: no address.
    warnedKey = key;
    logger.warn(
      'auth.tls.proxy_refused',
      `dashboard_tls.trusted_proxy_addresses was refused: ${describeTrustedProxyProblem(problem)}. The LAN HTTPS proxy is not trusted until it is fixed.`,
      { reason: problem.reason, entry: problem.index === null ? null : problem.index + 1 }
    );
  }
  if (key) cache = { key, settings };
  return settings;
}

/** After this process writes config.yaml: the next read parses it again. */
export function invalidateDashboardTlsSettings(): void {
  cache = null;
}
