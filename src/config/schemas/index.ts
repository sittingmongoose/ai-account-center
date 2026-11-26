/**
 * Config schema barrel re-exports.
 *
 * Active AAC schemas. Retired launcher and CLIProxy sections remain
 * opaque values and do not have typed accessors or defaults.
 */

// Version constant
export { UNIFIED_CONFIG_VERSION } from './version';

// Account, profile, auth types
export type {
  AccountConfig,
  ProfileConfig,
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

// Main unified config interface, factory, and type guard
export { createEmptyUnifiedConfig, isUnifiedConfig } from './unified-config';
export type { UnifiedConfig } from './unified-config';
