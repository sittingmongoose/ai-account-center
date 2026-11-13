/** Active AAC fields and the CLIProxy configuration held for central-logins research. */
import type {
  AccountConfig,
  ProfileConfig,
  DashboardAuthConfig,
  DashboardTlsConfig,
  DashboardNetworkConfig,
} from './auth';
import { DEFAULT_DASHBOARD_AUTH_CONFIG } from './auth';
import type { CLIProxyConfig } from './cliproxy';
import { CLIPROXY_SUPPORTED_PROVIDERS, DEFAULT_CLIPROXY_SAFETY_CONFIG } from './cliproxy';
import type { LoggingConfig, PreferencesConfig } from './logging';
import { DEFAULT_LOGGING_CONFIG } from './logging';
import type { CliproxyServerConfig, OpenAICompatProxyConfig } from './proxy-server';
import { DEFAULT_CLIPROXY_SERVER_CONFIG, DEFAULT_OPENAI_COMPAT_PROXY_CONFIG } from './proxy-server';
import type { QuotaManagementConfig } from './quota';
import { DEFAULT_QUOTA_MANAGEMENT_CONFIG } from './quota';
import { UNIFIED_CONFIG_VERSION } from './version';
export interface UnifiedConfig {
  /** Config version */
  version: number;
  /** Flag indicating setup wizard has been completed */
  setup_completed?: boolean;
  /** Default profile name to use when none specified */
  default?: string;
  /** Account-based profiles (isolated Claude instances) */
  accounts: Record<string, AccountConfig>;
  /** API-based profiles (env var injection) */
  profiles: Record<string, ProfileConfig>;
  /** CLIProxy configuration */
  cliproxy: CLIProxyConfig;
  /** OpenAI-compatible local proxy configuration */
  proxy?: OpenAICompatProxyConfig;
  /** CCS-owned structured logging configuration */
  logging?: LoggingConfig;
  /** User preferences */
  preferences: PreferencesConfig;
  /** CLIProxy server configuration for remote/local mode */
  cliproxy_server?: CliproxyServerConfig;
  /** Quota management configuration (v7+) */
  quota_management?: QuotaManagementConfig;
  /** Dashboard authentication configuration (optional) */
  dashboard_auth?: DashboardAuthConfig;
  /** Dashboard secure transport (optional, off by default) */
  dashboard_tls?: DashboardTlsConfig;
  /** Dashboard trusted local network (optional, off by default) */
  dashboard_network?: DashboardNetworkConfig;
  [section: string]: unknown;
}
export function createEmptyUnifiedConfig(): UnifiedConfig {
  return {
    version: UNIFIED_CONFIG_VERSION,
    default: undefined,
    accounts: {},
    profiles: {},
    cliproxy: {
      backend: 'original',
      oauth_accounts: {},
      providers: [...CLIPROXY_SUPPORTED_PROVIDERS],
      variants: {},
      logging: {
        enabled: false,
        request_log: false,
      },
      safety: { ...DEFAULT_CLIPROXY_SAFETY_CONFIG },
      auto_sync: true,
      routing: {
        strategy: 'round-robin',
        session_affinity: false,
        session_affinity_ttl: '1h',
      },
    },
    proxy: {
      port: DEFAULT_OPENAI_COMPAT_PROXY_CONFIG.port,
      profile_ports: { ...DEFAULT_OPENAI_COMPAT_PROXY_CONFIG.profile_ports },
      routing: {
        ...DEFAULT_OPENAI_COMPAT_PROXY_CONFIG.routing,
      },
    },
    logging: { ...DEFAULT_LOGGING_CONFIG },
    preferences: {
      theme: 'system',
      telemetry: false,
      auto_update: true,
    },
    cliproxy_server: { ...DEFAULT_CLIPROXY_SERVER_CONFIG },
    quota_management: { ...DEFAULT_QUOTA_MANAGEMENT_CONFIG },
    dashboard_auth: { ...DEFAULT_DASHBOARD_AUTH_CONFIG },
  };
}

/**
 * Type guard for UnifiedConfig.
 * Relaxed validation: accepts configs with version >= 1 and any subset of sections.
 * Missing sections will be filled with defaults during merge.
 */
export function isUnifiedConfig(obj: unknown): obj is UnifiedConfig {
  if (typeof obj !== 'object' || obj === null) return false;
  const config = obj as Record<string, unknown>;
  // Only require version to be a number >= 1 (allow future versions)
  // Sections are optional - will be merged with defaults in loadOrCreateUnifiedConfig
  return typeof config.version === 'number' && config.version >= 1;
}
