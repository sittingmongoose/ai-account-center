import type { AntigravityAutoSwitchStatus } from '../../antigravity/auto-switch/types';
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
  | 'ready'
  | 'pending_sign_in'
  | 'signing_in'
  | 'verifying';

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
  platform: DashboardPlatform;
  source: string;
  status: DashboardAccountStatus;
  /** Present only with a precise reason; absent otherwise. */
  statusReason?: DashboardAccountStatusReason;
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
}
