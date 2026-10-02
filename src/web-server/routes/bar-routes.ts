/** Native bar process-identity probe and shared native quota snapshot types. */

import { createApiRouter } from './api-router';

/**
 * Per-window quota detail for native subscription rows (Claude/Codex).
 *
 * Carries BOTH used and remaining percent so the macOS bar never re-derives
 * them. CLIProxy rows omit the parent `quotaWindows` field entirely, keeping
 * the payload backward compatible.
 *
 * JSON shape (decode test pins these exact keys): the inner object keys stay
 * camelCase (usedPercent/remainingPercent/resetAt/windowMinutes); only the
 * parent field name serializes to snake_case ("quota_windows").
 */
export interface QuotaWindowDetail {
  /** Stable key: "five_hour" | "seven_day" | "seven_day_opus" | "seven_day_sonnet". */
  key: string;
  /** Display label: "5h" | "week" | "Opus · week" | "Sonnet · week". */
  label: string;
  /** Actual provider-reported used percentage; overages may exceed 100. */
  usedPercent: number;
  /** Remaining percentage (0-100). Carried explicitly, never derived in Swift. */
  remainingPercent: number;
  /** ISO timestamp when this window resets, null if unknown. */
  resetAt: string | null;
  /** Window length in minutes (300 | 10080), null if unknown. */
  windowMinutes: number | null;
  kind?: 'rate_limit' | 'balance' | 'spend' | 'extra_usage';
  remaining?: number | null;
  used?: number | null;
  limit?: number | null;
  unit?: string | null;
  expiresAt?: string | null;
  unlimited?: boolean;
  enabled?: boolean;
}

/** Balances retain unknown percentages instead of inventing a quota gauge. */
export type BalanceWindowDetail = Omit<QuotaWindowDetail, 'usedPercent' | 'remainingPercent'> & {
  usedPercent: number | null;
  remainingPercent: number | null;
};

/** Internal native quota snapshot consumed by saved-profile account collectors. */
export interface BarSummaryRow {
  /** Account identifier (email or custom name) */
  account_id: string;
  /** CLIProxy provider: agy | codex | gemini | claude | ghcp | … */
  provider: string;
  /** Nickname or fallback to account_id */
  displayName: string | null;
  /** Account tier: free | pro | ultra | unknown | null on error */
  tier: string | null;
  /** Whether account is user-paused */
  paused: boolean;
  /** Best-guess quota remaining percentage (0-100), null on error */
  quota_percentage: number | null;
  /**
   * Tri-state quota availability for this account:
   *   'ok'          — provider has a quota API and the fetch succeeded
   *   'unsupported' — provider has no quota API at all (e.g. ghcp, kiro)
   *   'error'       — provider should report quota but the fetch failed/timed out/needs reauth
   * The UI uses this to render "no quota" (unsupported) vs "quota ?" (error)
   * instead of a bare "--" that conflates the two.
   */
  quotaStatus: 'ok' | 'unsupported' | 'error';
  /** ISO timestamp of next quota reset, null if unknown */
  next_reset: string | null;
  /** Whether this is the provider's default account (drives the active/default badge) */
  is_default: boolean;
  /** ISO timestamp this account was last used, null if never/unknown */
  last_activity_at: string | null;
  /** Today's attributed cost in USD, null if unavailable */
  today_cost: number | null;
  /** Health status derived from this account's quota result */
  health: 'ok' | 'warning' | 'error';
  /** True when value came from cache; false when freshly fetched */
  cached: boolean;
  /** ISO timestamp of when this data was fetched/cached */
  fetchedAt: string;
  /** True if account token is expired and needs re-authentication */
  needsReauth: boolean;
  /**
   * Native subscription surface: "ccs" (Claude Code) or "ccsx" (Codex).
   * Present ONLY on native subscription rows; omitted on CLIProxy pool rows.
   */
  surface?: string;
  /**
   * Native profile name (e.g. "work", "ck", "personal").
   * Present ONLY on native subscription rows; omitted on CLIProxy pool rows.
   */
  profile?: string;
  /**
   * Explicit native-subscription flag. true on all native rows; omitted on
   * CLIProxy pool rows (decodes to false/nil). Replaces the brittle
   * accountId == "claude-code" heuristic in Swift.
   */
  is_subscription?: boolean;
  /**
   * Native-only per-window quota breakdown (Claude: 5h/week/opus/sonnet,
   * Codex: 5h/week). CLIProxy rows OMIT this field so existing decode/encode
   * tests and the Swift legacy path stay unaffected. Serialized as
   * "quota_windows".
   */
  quotaWindows?: QuotaWindowDetail[];
  balanceWindows?: BalanceWindowDetail[];
  /** Native quota provenance; only network snapshots can drive account rotation. */
  quotaSource?: 'network' | 'local';
  /**
   * Native-only ISO mtime of the source session that supplied a stale Codex
   * reading. Present only when stale; serialized as "stale_as_of".
   */
  staleAsOf?: string | null;
}

const barRoutes = createApiRouter();

// Access and nonce-bound HMAC validation run in the authenticated API aggregator.
// This liveness response never reads account state or starts provider collection.
barRoutes.get('/auth', (_req, res) => res.json({ status: 'ok' }));

export default barRoutes;
