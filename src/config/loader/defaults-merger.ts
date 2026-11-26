/** Merge active AAC defaults; all retired or unknown sections stay opaque. */
import {
  createEmptyUnifiedConfig,
  DEFAULT_QUOTA_MANAGEMENT_CONFIG,
  DEFAULT_DASHBOARD_AUTH_CONFIG,
  DEFAULT_LOGGING_CONFIG,
  effectiveSessionLifetimeDays,
} from '../unified-config-types';
import type { UnifiedConfig } from '../unified-config-types';

// ---------------------------------------------------------------------------
// mergeWithDefaults
// ---------------------------------------------------------------------------

export function mergeWithDefaults(partial: Partial<UnifiedConfig>): UnifiedConfig {
  const defaults = createEmptyUnifiedConfig();
  return {
    ...partial,
    version: partial.version ?? defaults.version,
    setup_completed: partial.setup_completed,
    default: partial.default ?? defaults.default,
    accounts: partial.accounts ?? defaults.accounts,
    profiles: partial.profiles ?? defaults.profiles,
    logging: {
      enabled: partial.logging?.enabled ?? DEFAULT_LOGGING_CONFIG.enabled,
      level: partial.logging?.level ?? DEFAULT_LOGGING_CONFIG.level,
      rotate_mb: partial.logging?.rotate_mb ?? DEFAULT_LOGGING_CONFIG.rotate_mb,
      retain_days: partial.logging?.retain_days ?? DEFAULT_LOGGING_CONFIG.retain_days,
      redact: partial.logging?.redact ?? DEFAULT_LOGGING_CONFIG.redact,
      live_buffer_size:
        partial.logging?.live_buffer_size ?? DEFAULT_LOGGING_CONFIG.live_buffer_size,
    },
    preferences: {
      ...defaults.preferences,
      ...partial.preferences,
    },
    // Quota management config - hybrid auto+manual account selection
    quota_management: {
      mode: partial.quota_management?.mode ?? DEFAULT_QUOTA_MANAGEMENT_CONFIG.mode,
      auto: {
        preflight_check:
          partial.quota_management?.auto?.preflight_check ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.auto.preflight_check,
        exhaustion_threshold:
          partial.quota_management?.auto?.exhaustion_threshold ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.auto.exhaustion_threshold,
        tier_priority:
          partial.quota_management?.auto?.tier_priority ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.auto.tier_priority,
        cooldown_minutes:
          partial.quota_management?.auto?.cooldown_minutes ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.auto.cooldown_minutes,
      },
      manual: {
        paused_accounts:
          partial.quota_management?.manual?.paused_accounts ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.manual.paused_accounts,
        forced_default:
          partial.quota_management?.manual?.forced_default ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.manual.forced_default,
        tier_lock:
          partial.quota_management?.manual?.tier_lock ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.manual.tier_lock,
      },
      runtime_monitor: {
        enabled:
          partial.quota_management?.runtime_monitor?.enabled ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.runtime_monitor.enabled,
        normal_interval_seconds:
          partial.quota_management?.runtime_monitor?.normal_interval_seconds ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.runtime_monitor.normal_interval_seconds,
        critical_interval_seconds:
          partial.quota_management?.runtime_monitor?.critical_interval_seconds ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.runtime_monitor.critical_interval_seconds,
        warn_threshold:
          partial.quota_management?.runtime_monitor?.warn_threshold ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.runtime_monitor.warn_threshold,
        exhaustion_threshold:
          partial.quota_management?.runtime_monitor?.exhaustion_threshold ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.runtime_monitor.exhaustion_threshold,
        cooldown_minutes:
          partial.quota_management?.runtime_monitor?.cooldown_minutes ??
          DEFAULT_QUOTA_MANAGEMENT_CONFIG.runtime_monitor.cooldown_minutes,
      },
    },
    // Dashboard auth config - disabled by default
    dashboard_auth: {
      enabled: partial.dashboard_auth?.enabled ?? DEFAULT_DASHBOARD_AUTH_CONFIG.enabled,
      username: partial.dashboard_auth?.username ?? DEFAULT_DASHBOARD_AUTH_CONFIG.username,
      password_hash:
        partial.dashboard_auth?.password_hash ?? DEFAULT_DASHBOARD_AUTH_CONFIG.password_hash,
      session_timeout_hours:
        partial.dashboard_auth?.session_timeout_hours ??
        DEFAULT_DASHBOARD_AUTH_CONFIG.session_timeout_hours,
      // A legacy `session_timeout_hours` other than the old 24-hour default maps to
      // the nearest offered lifetime; a bare 24 is the value our own setup code
      // wrote when nothing was chosen, so it takes the 7-day default.
      session_lifetime_days: effectiveSessionLifetimeDays({
        session_lifetime_days: partial.dashboard_auth?.session_lifetime_days,
        session_timeout_hours: partial.dashboard_auth?.session_timeout_hours,
      }),
      ...(typeof partial.dashboard_auth?.password_changed_at === 'string'
        ? { password_changed_at: partial.dashboard_auth.password_changed_at }
        : {}),
    },
    // Dashboard secure transport: passed through only when present (off by default).
    ...(partial.dashboard_tls ? { dashboard_tls: partial.dashboard_tls } : {}),
    // Dashboard trusted local network: passed through only when present (off by default).
    ...(partial.dashboard_network ? { dashboard_network: partial.dashboard_network } : {}),
  };
}
