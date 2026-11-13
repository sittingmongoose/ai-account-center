/**
 * Config schema barrel re-exports.
 *
 * Active AAC and held CLIProxy schemas. Ignored launcher sections remain
 * opaque values and do not have typed accessors or defaults.
 */

// Version constant
export { UNIFIED_CONFIG_VERSION } from './version';

// Account, profile, OAuth, auth types
export type {
  AccountConfig,
  ProfileConfig,
  OAuthAccounts,
  CLIProxyAuthConfig,
  TokenRefreshSettings,
  DashboardAuthConfig,
  DashboardTlsConfig,
  DashboardNetworkConfig,
  SessionLifetimeDays,
} from './auth';
export {
  DEFAULT_DASHBOARD_AUTH_CONFIG,
  DEFAULT_SESSION_LIFETIME_DAYS,
  SESSION_LIFETIME_DAYS,
  effectiveSessionLifetimeDays,
  isSessionLifetimeDays,
  nearestSessionLifetimeDays,
} from './auth';

// CLIProxy provider, variant, routing, safety, logging types
export { CLIPROXY_SUPPORTED_PROVIDERS, DEFAULT_CLIPROXY_SAFETY_CONFIG } from './cliproxy';
export type {
  CLIProxyVariantConfig,
  CompositeTierConfig,
  CompositeVariantConfig,
  CLIProxyLoggingConfig,
  CLIProxySafetyConfig,
  CLIProxyRoutingConfig,
  CLIProxyRetryConfig,
  CLIProxyPoolRoutingConfig,
  CLIProxyOAuthModelAliasEntry,
  CLIProxyOAuthModelAliasConfig,
  CLIProxyPayloadModelSelector,
  CLIProxyPayloadOverrideRule,
  CLIProxyPayloadConfig,
  CLIProxyConfig,
} from './cliproxy';

// Quota management types and defaults
export {
  DEFAULT_AUTO_QUOTA_CONFIG,
  DEFAULT_MANUAL_QUOTA_CONFIG,
  DEFAULT_RUNTIME_MONITOR_CONFIG,
  DEFAULT_QUOTA_MANAGEMENT_CONFIG,
} from './quota';
export type {
  AutoQuotaConfig,
  RuntimeMonitorConfig,
  ManualQuotaConfig,
  QuotaManagementMode,
  QuotaManagementConfig,
} from './quota';

// Logging and preferences types and defaults
export { DEFAULT_LOGGING_CONFIG } from './logging';
export type { LoggingLevel, LoggingConfig, PreferencesConfig } from './logging';

// CLIProxy proxy configuration held for central-logins research.
export type {
  ProxyRemoteConfig,
  ProxyFallbackConfig,
  ProxyLocalConfig,
  OpenAICompatProxyRoutingConfig,
  OpenAICompatProxyConfig,
  CliproxyServerConfig,
} from './proxy-server';
export { DEFAULT_CLIPROXY_SERVER_CONFIG, DEFAULT_OPENAI_COMPAT_PROXY_CONFIG } from './proxy-server';

// Main unified config interface, factory, and type guard
export { createEmptyUnifiedConfig, isUnifiedConfig } from './unified-config';
export type { UnifiedConfig } from './unified-config';
