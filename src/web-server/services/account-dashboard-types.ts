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
  };
}

export interface AccountDashboard {
  schemaVersion: 1;
  updatedAt: string;
  settings?: { refreshIntervalSeconds: number };
  accounts: DashboardAccount[];
  codexAutoSwitch: {
    enabled: boolean;
    thresholdPercent: number;
    pollIntervalSeconds: number;
    outcome: string;
    message: string;
    activationInProgress: boolean;
    lastCheckedAt?: string;
    lastSwitchedAt?: string;
  };
}
