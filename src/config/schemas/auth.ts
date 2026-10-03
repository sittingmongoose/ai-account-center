/**
 * Account, profile, and authentication config types.
 *
 * Covers:
 * - AccountConfig: isolated Claude instances via CLAUDE_CONFIG_DIR
 * - ProfileConfig: API-based profiles (env var injection)
 * - OAuthAccounts: CLIProxy nickname-to-email mapping
 * - CLIProxyAuthConfig: API key and management secret customization
 * - TokenRefreshSettings: background token refresh worker config
 * - DashboardAuthConfig: dashboard login protection
 */

import type { TargetType } from '../../targets/target-adapter';

/**
 * Account configuration (formerly in profiles.json).
 * Represents an isolated Claude instance via CLAUDE_CONFIG_DIR.
 */
export interface AccountConfig {
  /** ISO timestamp when account was created */
  created: string;
  /** ISO timestamp of last usage, null if never used */
  last_used: string | null;
  /** Context mode for project workspace data */
  context_mode?: 'isolated' | 'shared';
  /** Context-sharing group when context_mode='shared' */
  context_group?: string;
  /** Shared continuity depth when context_mode='shared' */
  continuity_mode?: 'standard' | 'deeper';
  /** Account-level shared resource behavior for plugins, commands, skills, agents, settings.json, and CLAUDE.md */
  shared_resource_mode?: 'shared' | 'profile-local';
  /** Bare profile: no shared symlinks (commands, skills, agents, settings.json, CLAUDE.md) */
  bare?: boolean;
}

/**
 * API-based profile configuration.
 * Injects environment variables for alternative providers (GLM, Kimi, etc.).
 *
 * Settings are stored in separate *.settings.json files (matching Claude's pattern)
 * to allow users to edit them directly without touching config.yaml.
 */
export interface ProfileConfig {
  /** Profile type - currently only 'api' */
  type: 'api';
  /** Path to settings file (e.g., "~/.ccs/glm.settings.json") */
  settings: string;
  /** Target CLI to use for this profile (default: 'claude') */
  target?: TargetType;
}

/**
 * CLIProxy OAuth account nickname mapping.
 * Maps user-friendly nicknames to email addresses.
 */
export type OAuthAccounts = Record<string, string>;

/**
 * CLIProxy authentication configuration.
 * Allows customization of API key and management secret for CLIProxyAPI.
 */
export interface CLIProxyAuthConfig {
  /** API key for CCS-managed requests (default: 'ccs-internal-managed') */
  api_key?: string;
  /** Management secret for Control Panel login (default: 'ccs') */
  management_secret?: string;
}

/**
 * Token refresh configuration.
 * Manages background token refresh worker settings.
 */
export interface TokenRefreshSettings {
  /** Enable background token refresh (default: false) */
  enabled?: boolean;
  /** Refresh check interval in minutes (default: 30) */
  interval_minutes?: number;
  /** Preemptive refresh time in minutes (default: 45) */
  preemptive_minutes?: number;
  /** Maximum retry attempts per token (default: 3) */
  max_retries?: number;
  /** Enable verbose logging (default: false) */
  verbose?: boolean;
}

/**
 * Dashboard authentication configuration.
 * Optional login protection for CCS dashboard.
 * Disabled by default for backward compatibility.
 */
export interface DashboardAuthConfig {
  /** Enable dashboard authentication (default: false) */
  enabled: boolean;
  /** Username for dashboard login */
  username: string;
  /** Bcrypt-hashed password (use: npx bcrypt-cli hash 'password') */
  password_hash: string;
  /** Session timeout in hours (default: 24; legacy mirror of session_lifetime_days) */
  session_timeout_hours?: number;
  /**
   * Dashboard session lifetime in days: 1, 7, 30, 90 or 365 (default: 30).
   * The session cookie and the idle expiry follow it. Saved from Settings.
   */
  session_lifetime_days?: number;
  /** ISO time of the last password change made by the dashboard (CONTRACT-auth-devices 3) */
  password_changed_at?: string;
}

/**
 * Secure transport for the dashboard (CONTRACT-auth-devices section 2a). Every
 * key is optional and the whole block is off by default: nothing here turns on
 * a listener or trusts a proxy until it is set in config.yaml.
 */
export interface DashboardTlsConfig {
  /** A local TLS proxy on loopback whose `X-Forwarded-Proto: https` is trusted */
  trusted_proxy?: 'tailscale-serve' | 'loopback-https-proxy';
  /** Optional in-process HTTPS listener */
  https_listener?: {
    enabled?: boolean;
    port?: number;
    cert_path?: string;
    key_path?: string;
  };
  /** The secure address the page links to and the trays connect to */
  public_origin?: string;
}

/**
 * Trusted local network for the dashboard (CONTRACT-auth-devices section 2a,
 * rule 4, amended 2026-10-02). Off by default. When on, plain HTTP from a peer
 * in `trusted_networks` counts as a secure transport.
 */
export interface DashboardNetworkConfig {
  /** Trust peers on the local network (default: false). Only `true` turns it on. */
  trust_local_network?: boolean;
  /**
   * Trusted ranges as CIDRs (default: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
   * and fc00::/7). A VPN subnet outside them can be added here. Loopback is never
   * trusted by this rule (the dashboard computer is secure by its own rule).
   */
  trusted_networks?: string[];
}

/**
 * Default dashboard auth configuration.
 * Disabled by default - must be explicitly enabled.
 */
/** The dashboard session lifetimes Settings offers, in days. */
export const SESSION_LIFETIME_DAYS = [1, 7, 30, 90, 365] as const;
export type SessionLifetimeDays = (typeof SESSION_LIFETIME_DAYS)[number];
export const DEFAULT_SESSION_LIFETIME_DAYS: SessionLifetimeDays = 30;

export function isSessionLifetimeDays(value: unknown): value is SessionLifetimeDays {
  return typeof value === 'number' && (SESSION_LIFETIME_DAYS as readonly number[]).includes(value);
}

/**
 * The effective session lifetime in days for a dashboard auth config. A saved
 * `session_lifetime_days` wins; otherwise a legacy `session_timeout_hours`
 * maps to the nearest offered lifetime (a bare 24 is the old default, so it
 * takes the new 30-day default); anything else is the 30-day default.
 */
export function effectiveSessionLifetimeDays(auth: {
  session_lifetime_days?: number;
  session_timeout_hours?: number;
}): SessionLifetimeDays {
  if (isSessionLifetimeDays(auth.session_lifetime_days)) return auth.session_lifetime_days;
  const hours = auth.session_timeout_hours;
  if (typeof hours === 'number' && Number.isFinite(hours) && hours > 0 && hours !== 24) {
    return nearestSessionLifetimeDays(hours);
  }
  return DEFAULT_SESSION_LIFETIME_DAYS;
}

/** A legacy `session_timeout_hours` value maps to the nearest offered lifetime. */
export function nearestSessionLifetimeDays(hours: number): SessionLifetimeDays {
  let best: SessionLifetimeDays = DEFAULT_SESSION_LIFETIME_DAYS;
  let gap = Number.POSITIVE_INFINITY;
  for (const days of SESSION_LIFETIME_DAYS) {
    const distance = Math.abs(days * 24 - hours);
    if (distance < gap) {
      gap = distance;
      best = days;
    }
  }
  return best;
}

export const DEFAULT_DASHBOARD_AUTH_CONFIG: DashboardAuthConfig = {
  enabled: false,
  username: '',
  password_hash: '',
  session_timeout_hours: 24,
  session_lifetime_days: 30,
};
