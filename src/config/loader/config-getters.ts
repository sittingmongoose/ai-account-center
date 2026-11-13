/** Active configuration accessors. */
import { DEFAULT_LOGGING_CONFIG, effectiveSessionLifetimeDays } from '../unified-config-types';
import type { DashboardAuthConfig, LoggingConfig, UnifiedConfig } from '../unified-config-types';
function getConfig(): UnifiedConfig {
  const { loadOrCreateUnifiedConfig } =
    require('../unified-config-loader') as typeof import('../unified-config-loader');
  return loadOrCreateUnifiedConfig();
}
export function isDashboardAuthEnabled(): boolean {
  const envEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;

  if (envEnabled !== undefined) {
    return envEnabled === 'true' || envEnabled === '1';
  }

  const config = getConfig();
  return config.dashboard_auth?.enabled ?? false;
}

export function getDashboardAuthConfig(): DashboardAuthConfig {
  const config = getConfig();

  // ENV vars take precedence
  const envEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  const envUsername = process.env.CCS_DASHBOARD_USERNAME;
  const envPasswordHash = process.env.CCS_DASHBOARD_PASSWORD_HASH;

  return {
    enabled:
      envEnabled !== undefined
        ? envEnabled === 'true' || envEnabled === '1'
        : (config.dashboard_auth?.enabled ?? false),
    username: envUsername ?? config.dashboard_auth?.username ?? '',
    password_hash: envPasswordHash ?? config.dashboard_auth?.password_hash ?? '',
    session_timeout_hours: config.dashboard_auth?.session_timeout_hours ?? 24,
    session_lifetime_days: effectiveSessionLifetimeDays(config.dashboard_auth ?? {}),
  };
}

export function getLoggingConfig(): LoggingConfig {
  const config = getConfig();

  return {
    enabled: config.logging?.enabled ?? DEFAULT_LOGGING_CONFIG.enabled,
    level: config.logging?.level ?? DEFAULT_LOGGING_CONFIG.level,
    rotate_mb: config.logging?.rotate_mb ?? DEFAULT_LOGGING_CONFIG.rotate_mb,
    retain_days: config.logging?.retain_days ?? DEFAULT_LOGGING_CONFIG.retain_days,
    redact: config.logging?.redact ?? DEFAULT_LOGGING_CONFIG.redact,
    live_buffer_size: config.logging?.live_buffer_size ?? DEFAULT_LOGGING_CONFIG.live_buffer_size,
  };
}
