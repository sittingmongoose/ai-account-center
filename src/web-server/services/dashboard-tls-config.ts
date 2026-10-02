import { statSync } from 'fs';
import os from 'os';
import path from 'path';
import { getConfigYamlPath, loadOrCreateUnifiedConfig } from '../../config/config-loader-facade';
import type { TrustedProxyKind } from '../middleware/secure-transport';

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
  httpsListener: DashboardHttpsListenerSettings | null;
  /** An `https://` origin (scheme, host and optional port only), or null. */
  publicOrigin: string | null;
}

const NONE: DashboardTlsSettings = Object.freeze({
  trustedProxy: null,
  httpsListener: null,
  publicOrigin: null,
});

function expandHome(value: string): string {
  return value === '~' || value.startsWith('~/') ? path.join(os.homedir(), value.slice(1)) : value;
}

function publicOrigin(value: unknown): string | null {
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

export function parseDashboardTlsSettings(raw: unknown): DashboardTlsSettings {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return NONE;
  const block = raw as Record<string, unknown>;
  const trustedProxy =
    block.trusted_proxy === 'tailscale-serve' || block.trusted_proxy === 'loopback-https-proxy'
      ? block.trusted_proxy
      : null;
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
  return { trustedProxy, httpsListener, publicOrigin: publicOrigin(block.public_origin) };
}

let cache: { key: string; settings: DashboardTlsSettings } | null = null;

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
  if (key) cache = { key, settings };
  return settings;
}
