import type { ClaudeSignInRenewal } from './claude-signin-renewal';
import type { AntigravityAutoSwitchStatus } from '../../antigravity/auto-switch/types';
import type { AntigravityPlan } from '../../antigravity/plan';
/** Public account-only contract shared by the dashboard and native clients. */
export type DashboardProvider =
  | 'codex'
  | 'claude'
  | 'antigravity'
  | 'muse'
  | 'cursor'
  | 'kimi-code'
  | 'qwen'
  | 'zai'
  | 'opencode-go';

export type DashboardPlatform = 'ubuntu' | 'mac' | 'windows';
export type ClaudeDashboardPlatform = 'mac' | 'windows';
export type DashboardAccountStatus = 'ok' | 'cached' | 'unavailable' | 'error' | 'needs_sign_in';
/**
 * Why a row has its status, when one precise reason matters to clients.
 * `identity_unbound`: an Antigravity reading could not be bound to the saved
 * account it was taken for (another identity, another credential revision or
 * no email), so the row is `error` and is not a switch target.
 */
export type DashboardAccountStatusReason = 'identity_unbound';

export interface DashboardAccountWindow {
  key: string;
  label: string;
  usedPercent: number | null;
  remainingPercent: number | null;
  resetAt: string | null;
  windowMinutes: number | null;
  used: number | null;
  limit: number | null;
  unit: string | null;
  kind?: 'rate_limit' | 'balance' | 'spend' | 'extra_usage';
  remaining?: number | null;
  expiresAt?: string | null;
  /** Reset grants only: the limit types refilled by this grant (slugs like `five_hour`, `seven_day`). */
  clears?: string[];
  unlimited?: boolean;
  enabled?: boolean;
  /** Retained optional quota rows keep their original observation time. */
  status?: 'cached';
  sampledAt?: string;
  /**
   * Present, and always true, only when `resetAt` is at or before the response time and
   * the reading was sampled before it (the window's `sampledAt`, else the account's) or
   * at an unknown time. The percent and amount fields then hold the reading from before
   * the reset, as history only: show "Reset at <time> · new reading pending", never 0%.
   */
  resetPassed?: true;
  poolId?: string;
  poolIdSource?: 'provider-id' | 'provider-bucket-membership';
  poolLabel?: string;
  modelIds?: string[];
}

/** Account lifecycle (CONTRACT-registry-lifecycle section 2). */
export type DashboardAccountLifecycleState =
  'ready' | 'pending_sign_in' | 'signing_in' | 'verifying';

export interface DashboardAccountLifecycle {
  state: DashboardAccountLifecycleState;
  /** The running sign-in job for this account, if any. */
  jobId: string | null;
}

export interface DashboardAccount {
  id: string;
  provider: DashboardProvider;
  providerLabel: string;
  label: string;
  email: string | null;
  plan: string | null;
  /**
   * Claude only: the entitlement class the plan display name may not carry
   * ("Ultra" is a Max-class plan). Drives the Fable column, which gates on
   * the class, not the name. Absent for other providers and older samples.
   */
  planClass?: 'max' | 'pro';
  /** Antigravity coding entitlement at the sample time; absent for unknown plans. */
  antigravityPlan?: AntigravityPlan;
  platform: DashboardPlatform;
  /**
   * Display only: 'nas1' on a usage source that runs on Nas1, the second Ubuntu
   * computer. That source is stored and reported as platform 'ubuntu' with its
   * fixed ssh alias, so the registry, quota history and older packages are
   * unchanged. Absent on every other row.
   */
  host?: 'nas1';
  source: string;
  status: DashboardAccountStatus;
  /** Present only with a precise reason; absent otherwise. */
  statusReason?: DashboardAccountStatusReason;
  /**
   * Claude only: the computers where this profile is not signed in, so the
   * clients can say "Sign-in needed on Windows" before Open. Each computer's
   * profile signs in on its own; Open stays allowed and shows Claude's own
   * sign-in window. Windows comes from the usage helper's own answer, Mac
   * from the profile's plaintext sign-in marker; nothing secret is read.
   * Absent when no computer is known to need a sign-in (an unreachable
   * computer is unknown, never "needed"). Order: mac, then windows.
   */
  signInNeeded?: ClaudeDashboardPlatform[];
  message: string | null;
  fetchedAt: string | null;
  sampledAt: string | null;
  isActive: boolean;
  windows: DashboardAccountWindow[];
  capabilities: {
    codexProfile: string | null;
    claudeProfileId: string | null;
    claudePlatforms: ClaudeDashboardPlatform[];
    antigravityProfileId?: string;
    antigravityHostIds?: Array<'ubuntu'>;
    antigravityCanActivate?: boolean;
  };
  /**
   * The provider is switchable and this account can be a switch target. Always
   * set on GET /api/accounts/dashboard rows; optional for other producers.
   */
  switchable?: boolean;
  /** Always set on GET /api/accounts/dashboard rows; optional for other producers. */
  lifecycle?: DashboardAccountLifecycle;
  /**
   * Display only: the provider is hidden or this id is in hiddenAccountIds.
   * A hidden account is still collected and still an auto-switch candidate.
   * Always set on GET /api/accounts/dashboard rows. The dashboard follows
   * this; the trays never do.
   */
  hidden?: boolean;
  /**
   * Display only, for the trays: the provider is in trayHiddenProviders or
   * this id is in trayHiddenAccountIds. Independent of `hidden`: hiding on
   * the dashboard never sets it and hiding in the trays never sets `hidden`.
   * Always set on GET /api/accounts/dashboard rows.
   */
  trayHidden?: boolean;
}

/** How a provider's accounts are signed in (CONTRACT-registry-lifecycle section 2). */
export type DashboardSignInKind =
  | 'desktop-profile'
  | 'device-code'
  | 'supervised-cli'
  | 'api-key'
  | 'app-session'
  | 'browser-session';

export type DashboardSignInUnavailableReason =
  | 'secure_transport_required'
  | 'not_implemented'
  | 'preflight_failed'
  | 'tool_missing'
  | 'isolation_unproven'
  | 'extension_update_required';

export interface DashboardProviderEntry {
  id: DashboardProvider;
  label: string;
  longLabel: string;
  /** marks.js key; equals id. */
  iconKey: string;
  /** 0-based display order: claude, codex, antigravity, then the rest. */
  order: number;
  /** Not in settings.hiddenProviders. */
  visible: boolean;
  /**
   * Not in settings.trayHiddenProviders. Independent of visible: hiding on
   * the dashboard does not hide in the trays, and the reverse also holds.
   */
  trayVisible: boolean;
  /** Every account of this provider in accounts[], hidden ones included. */
  accountCount: number;
  switchable: boolean;
  signIn: {
    kind: DashboardSignInKind;
    label: string;
    platforms: DashboardPlatform[];
    secureTransportRequired: boolean;
    available: boolean;
    unavailableReason: DashboardSignInUnavailableReason | null;
  };
  extras: null | { kind: 'browser-extension'; platform: ClaudeDashboardPlatform; label: string };
  capabilities: {
    multiAccount: boolean;
    add: boolean;
    signInAgain: boolean;
    replaceKey: boolean;
    remove: boolean;
    activate: boolean;
    autoSwitch: boolean;
    openApp: ClaudeDashboardPlatform[];
    recheck: boolean;
  };
}

export interface AccountDashboardSettings {
  refreshIntervalSeconds: number;
  /** Always set by the dashboard service; empty when nothing is hidden. */
  hiddenProviders?: DashboardProvider[];
  /** Always set by the dashboard service; ids may name accounts that are briefly absent. */
  hiddenAccountIds?: string[];
  /** Always set by the dashboard service; empty when no provider is hidden in the trays. */
  trayHiddenProviders?: DashboardProvider[];
  /** Always set by the dashboard service; accounts the trays leave out, independent of hiddenAccountIds. */
  trayHiddenAccountIds?: string[];
  /**
   * False when the visibility file exists but could not be read safely. The
   * lists are then the last good read of this server (never re-shown as
   * "nothing hidden"), or empty when there has never been one, and the page
   * should say that the hidden list could not be read.
   */
  visibilityAvailable?: boolean;
}

export interface CodexAutoSwitchDashboardStatus {
  enabled: boolean;
  /** Switch when the active account has this % remaining (5 means at 95% used). */
  thresholdPercent: number;
  /** The same threshold in % used: 100 - thresholdPercent. */
  thresholdUsedPercent: number;
  pollIntervalSeconds: number;
  outcome: string;
  message: string;
  activationInProgress: boolean;
  lastCheckedAt?: string;
  lastSwitchedAt?: string;
  /** Profile the monitor chose but could not switch to yet. Present only for waiting_idle. */
  candidate?: string;
  /** True when the blocked message warns about paid credits being spent. */
  usingCredits?: boolean;
}

/** The running server: package version and the build's commit. No paths or hosts. */
export interface DashboardServerInfo {
  version: string;
  commit: string | null;
}

export interface AccountDashboard {
  schemaVersion: 1;
  updatedAt: string;
  settings?: AccountDashboardSettings;
  /** Every supported provider in display order, even at 0 accounts. Always set by the service. */
  providers?: DashboardProviderEntry[];
  accounts: DashboardAccount[];
  antigravityAutoSwitch?: AntigravityAutoSwitchStatus;
  codexAutoSwitch: CodexAutoSwitchDashboardStatus;
  server?: DashboardServerInfo;
  /**
   * Claude Code sign-in renewal (scripts/claude-signin), when a runner publishes into the usage
   * snapshot folder: each sign-in's end and renewal state, and what needs a person.
   */
  claudeSignIns?: ClaudeSignInRenewal;
}

// ============================================================================
// NATIVE BAR QUOTA SNAPSHOT ROWS
// Moved from routes/bar-routes.ts (untangle audit U3): shared shapes produced by
// usage collectors and services, not route-local contracts.
// ============================================================================

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
