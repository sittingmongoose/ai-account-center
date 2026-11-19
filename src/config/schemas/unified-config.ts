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
  /** Rust agent verbs ceiling; absent and invalid values stay off. `shadow` serves the reference
   * answer and compares the agent alongside (DESIGN 4.5). */
  agentVerbs?: 'off' | 'shadow' | 'on';
  /** Per-family modes under the agentVerbs ceiling; an absent map keeps the ceiling for every
   * verb, a present map turns on only its listed families. Invalid maps stay empty (all off). */
  agentVerbFamilies?: Partial<
    Record<
      | 'inventory'
      | 'signin-expiry'
      | 'quota-read'
      | 'scan-usage'
      | 't3-pins'
      | 'key-store'
      | 'claude-history'
      | 'failover-inventory'
      | 'launch'
      | 'claude-host'
      | 'signin-session',
      'off' | 'shadow' | 'on'
    >
  >;
  /** Existing agents only: the lead's install script places the hash-pinned binary (M3D4 staged,
   * M3D5 pinned). The service never uploads an agent itself. */
  agentTransport?: 'off' | 'on';
  /** Explicit existing identity and pinned host-key source; neither is created or edited. */
  agentTransportSsh?: { identityFile?: string; knownHostsFile?: string };
  /**
   * The private Rust service (M3, L1); `AAC_RUST_SERVICE` overrides. Absent and invalid stay off. `groups.home`
   * (R1, `AAC_RUST_GROUP_HOME` overrides): Home is built from fed generations when the mode is on (default-ready when unset); `groups.status`
   * (R2, `AAC_RUST_GROUP_STATUS` overrides): the same for Status; `groups.analytics` (R5/A2,
   * `AAC_RUST_GROUP_ANALYTICS` overrides): Analytics history and activity are read from the hosted store and
   * composed in Rust, with the TS computation as the fallback. `store` (R3, `AAC_RUST_STORE` overrides)
   * hosts the history store when the mode is on. Off keeps the in-process store; exhausting the restart
   * budget automatically falls back to it on the same files. `homeProducer` (G17, `AAC_RUST_HOME_PRODUCER`
   * overrides): the Rust service produces the Home feed generations from the raw inputs; off by default
   * until the collection feeds it, and the TS producer stays available until the cut.
   */
  rustService?: {
    frontDoor?: 'ts' | 'rust';
    mode?: 'off' | 'shadow' | 'on';
    groups?: {
      home?: 'off' | 'on';
      status?: 'off' | 'on';
      scoreboard?: 'off' | 'on';
      analytics?: 'off' | 'on';
      collection?: 'off' | 'on';
      writes?: 'off' | 'on';
      'app-updates'?: 'off' | 'on';
    };
    store?: 'off' | 'on';
    homeProducer?: 'off' | 'on';
  };
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
