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
}

/** The running server: package version and the build's commit. No paths or hosts. */
export interface DashboardServerInfo {
  version: string;
  commit: string | null;
}

export interface AccountDashboard {
  schemaVersion: 1;
  updatedAt: string;
  settings?: { refreshIntervalSeconds: number };
  accounts: DashboardAccount[];
  antigravityAutoSwitch?: AntigravityAutoSwitchStatus;
  codexAutoSwitch: CodexAutoSwitchDashboardStatus;
  server?: DashboardServerInfo;
}
