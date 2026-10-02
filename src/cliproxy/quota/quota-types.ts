/**
 * Shared Quota Type Definitions
 *
 * Codex quota result types shared by the Codex quota fetcher and the saved-profile
 * quota collector.
 */

export interface QuotaErrorMetadata {
  /** Upstream HTTP status when available */
  httpStatus?: number;
  /** Stable machine-readable error code */
  errorCode?: string;
  /** Additional provider-specific detail/code from upstream */
  errorDetail?: string;
  /** True if account lacks quota access (403) */
  isForbidden?: boolean;
  /** Provider-specific remediation guidance */
  actionHint?: string;
  /** True when the failure is temporary and retrying later may help */
  retryable?: boolean;
}

/**
 * Codex quota window (primary, secondary, code review, additional)
 */
export interface CodexQuotaWindow {
  /** Window label: "Primary", "Secondary", "Code Review (Primary)", "Code Review (Secondary)", or "<feature> (Primary|Secondary)" */
  label: string;
  /** Percentage used (0-100) */
  usedPercent: number;
  /** Percentage remaining (100 - usedPercent) */
  remainingPercent: number;
  /** Seconds until quota resets, null if unknown */
  resetAfterSeconds: number | null;
  /** ISO timestamp when quota resets, null if unknown */
  resetAt: string | null;
  /**
   * Window category indicating the bucket this window belongs to.
   * Optional for back-compat with cached data emitted before this field existed.
   * - 'usage' -> standard rate_limit usage windows
   * - 'code-review' -> code_review_rate_limit windows
   * - 'additional' -> additional_rate_limits[] windows (e.g. GPT-5.3 Codex Spark)
   */
  category?: 'usage' | 'code-review' | 'additional';
  /** Cadence from upstream duration; primary/secondary position is used only for legacy payloads. */
  cadence?: '5h' | 'weekly';
  /** Authoritative upstream window length, when supplied; independent of time until reset. */
  limitWindowSeconds?: number;
  /** Raw upstream label (e.g. 'GPT-5.3-Codex-Spark', 'Code Review'); absent for plain usage windows. */
  featureLabel?: string;
}

/** Core Codex usage window (5h/weekly) extracted from raw windows */
export interface CodexCoreUsageWindow {
  /** Source window label */
  label: string;
  /** Percentage remaining (0-100) */
  remainingPercent: number;
  /** Seconds until quota resets, null if unknown */
  resetAfterSeconds: number | null;
  /** ISO timestamp when quota resets, null if unknown */
  resetAt: string | null;
}

/** Core Codex usage summary with explicit 5h and weekly windows */
export interface CodexCoreUsageSummary {
  /** Short-cycle usage limit window (typically 5h) */
  fiveHour: CodexCoreUsageWindow | null;
  /** Long-cycle usage limit window (typically weekly) */
  weekly: CodexCoreUsageWindow | null;
}

/**
 * Codex quota fetch result
 */
export interface CodexQuotaResult extends QuotaErrorMetadata {
  /** Whether fetch succeeded */
  success: boolean;
  /** Quota windows (primary, secondary, code review) */
  windows: CodexQuotaWindow[];
  /** Explicit core usage windows (5h + weekly) for easier reset display */
  coreUsage?: CodexCoreUsageSummary;
  /** Raw provider credit units, matching the official Codex status display. */
  credits?: {
    hasCredits: boolean | null;
    unlimited: boolean | null;
    balance: number | null;
  };
  /** Effective monthly workspace spend control, only when explicitly reported. */
  monthlySpend?: {
    used: number | null;
    limit: number | null;
    remainingPercent: number | null;
    resetAt: string | null;
  };
  /** Banked resets are separate from spendable credits and never redeemed here. */
  resetCredits?: {
    available: number | null;
    applicable: number | null;
    credits?: { resetType: string; expiresAt: string | null }[];
  };
  /** Plan type: free, plus, pro, team, or null if unknown */
  planType: 'free' | 'plus' | 'pro' | 'team' | null;
  /** Timestamp of fetch */
  lastUpdated: number;
  /** Error message if fetch failed */
  error?: string;
  /** Account ID (email) this quota belongs to */
  accountId?: string;
  /** True if token is expired and needs re-authentication */
  needsReauth?: boolean;
  /** True if account lacks quota access (403) - displayed as 0% instead of error */
  isForbidden?: boolean;
}
