/** Merge active defaults and held CLIProxy defaults; all other sections stay opaque. */
import {
  createEmptyUnifiedConfig,
  DEFAULT_CLIPROXY_SERVER_CONFIG,
  DEFAULT_CLIPROXY_SAFETY_CONFIG,
  DEFAULT_OPENAI_COMPAT_PROXY_CONFIG,
  DEFAULT_QUOTA_MANAGEMENT_CONFIG,
  DEFAULT_DASHBOARD_AUTH_CONFIG,
  DEFAULT_LOGGING_CONFIG,
  effectiveSessionLifetimeDays,
} from '../unified-config-types';
import type { UnifiedConfig } from '../unified-config-types';
import { normalizeSessionAffinityTtl } from './normalizers';

// ---------------------------------------------------------------------------
// mergeWithDefaults
// ---------------------------------------------------------------------------

function normalizeOptionalString(value: unknown): string | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }

  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}
export function mergeWithDefaults(partial: Partial<UnifiedConfig>): UnifiedConfig {
  const defaults = createEmptyUnifiedConfig();
  return {
    ...partial,
    version: partial.version ?? defaults.version,
    setup_completed: partial.setup_completed,
    default: partial.default ?? defaults.default,
    accounts: partial.accounts ?? defaults.accounts,
    profiles: partial.profiles ?? defaults.profiles,
    cliproxy: {
      ...partial.cliproxy,
      oauth_accounts: partial.cliproxy?.oauth_accounts ?? defaults.cliproxy.oauth_accounts,
      providers: defaults.cliproxy.providers, // Always use defaults for providers
      variants: partial.cliproxy?.variants ?? defaults.cliproxy.variants,
      logging: {
        enabled: partial.cliproxy?.logging?.enabled ?? defaults.cliproxy.logging?.enabled ?? false,
        request_log:
          partial.cliproxy?.logging?.request_log ?? defaults.cliproxy.logging?.request_log ?? false,
      },
      safety: {
        antigravity_ack_bypass:
          partial.cliproxy?.safety?.antigravity_ack_bypass ??
          DEFAULT_CLIPROXY_SAFETY_CONFIG.antigravity_ack_bypass,
      },
      // Kiro browser behavior setting (optional)
      kiro_no_incognito: partial.cliproxy?.kiro_no_incognito,
      // Auth config - preserve user values, no defaults (uses constants as fallback)
      auth: partial.cliproxy?.auth,
      // Background token refresh config (optional)
      token_refresh: partial.cliproxy?.token_refresh,
      // Backend selection - validate and preserve user choice (original vs plus)
      backend:
        partial.cliproxy?.backend === 'original' || partial.cliproxy?.backend === 'plus'
          ? partial.cliproxy.backend
          : undefined, // Invalid values become undefined (defaults to 'original' at runtime)
      management_panel_repository: normalizeOptionalString(
        partial.cliproxy?.management_panel_repository
      ),
      // Auto-sync - default to true
      auto_sync: partial.cliproxy?.auto_sync ?? defaults.cliproxy.auto_sync ?? true,
      routing: {
        strategy:
          partial.cliproxy?.routing?.strategy === 'fill-first' ||
          partial.cliproxy?.routing?.strategy === 'round-robin'
            ? partial.cliproxy.routing.strategy
            : defaults.cliproxy.routing?.strategy,
        session_affinity:
          typeof partial.cliproxy?.routing?.session_affinity === 'boolean'
            ? partial.cliproxy.routing.session_affinity
            : defaults.cliproxy.routing?.session_affinity,
        session_affinity_ttl: normalizeSessionAffinityTtl(
          partial.cliproxy?.routing?.session_affinity_ttl,
          defaults.cliproxy.routing?.session_affinity_ttl ?? '1h'
        ),
      },
    },
    proxy: {
      port: partial.proxy?.port ?? DEFAULT_OPENAI_COMPAT_PROXY_CONFIG.port,
      profile_ports: partial.proxy?.profile_ports ?? {
        ...DEFAULT_OPENAI_COMPAT_PROXY_CONFIG.profile_ports,
      },
      routing: {
        default: partial.proxy?.routing?.default ?? defaults.proxy?.routing?.default,
        background: partial.proxy?.routing?.background ?? defaults.proxy?.routing?.background,
        think: partial.proxy?.routing?.think ?? defaults.proxy?.routing?.think,
        longContext: partial.proxy?.routing?.longContext ?? defaults.proxy?.routing?.longContext,
        webSearch: partial.proxy?.routing?.webSearch ?? defaults.proxy?.routing?.webSearch,
        longContextThreshold:
          partial.proxy?.routing?.longContextThreshold ??
          defaults.proxy?.routing?.longContextThreshold,
      },
    },
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
    // CLIProxy server config - remote/local CLIProxyAPI settings
    cliproxy_server: {
      remote: {
        enabled:
          partial.cliproxy_server?.remote?.enabled ?? DEFAULT_CLIPROXY_SERVER_CONFIG.remote.enabled,
        host: partial.cliproxy_server?.remote?.host ?? DEFAULT_CLIPROXY_SERVER_CONFIG.remote.host,
        // Port is optional - undefined means use protocol default (443 for HTTPS, 8317 for HTTP)
        port: partial.cliproxy_server?.remote?.port,
        protocol:
          partial.cliproxy_server?.remote?.protocol ??
          DEFAULT_CLIPROXY_SERVER_CONFIG.remote.protocol,
        auth_token:
          partial.cliproxy_server?.remote?.auth_token ??
          DEFAULT_CLIPROXY_SERVER_CONFIG.remote.auth_token,
        // management_key is optional - falls back to auth_token when not set
        management_key: partial.cliproxy_server?.remote?.management_key,
      },
      fallback: {
        enabled:
          partial.cliproxy_server?.fallback?.enabled ??
          DEFAULT_CLIPROXY_SERVER_CONFIG.fallback.enabled,
        auto_start:
          partial.cliproxy_server?.fallback?.auto_start ??
          DEFAULT_CLIPROXY_SERVER_CONFIG.fallback.auto_start,
      },
      local: {
        port: partial.cliproxy_server?.local?.port ?? DEFAULT_CLIPROXY_SERVER_CONFIG.local.port,
        auto_start:
          partial.cliproxy_server?.local?.auto_start ??
          DEFAULT_CLIPROXY_SERVER_CONFIG.local.auto_start,
      },
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
