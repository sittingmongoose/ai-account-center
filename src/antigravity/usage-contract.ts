import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../web-server/services/account-dashboard-types';
import type { ActivateRequest, ActivationResult } from './types';
import type { AntigravityRecoveryResult } from './switch-service';

/** Host scope is deliberately fixed: desktop controls target the Ubuntu CLI. */
export type AntigravityHostId = 'ubuntu';

/** Internal registry snapshot. Identity/revision values must never enter HTTP. */
export interface AntigravityUsageProfile {
  id: string;
  email: string;
  plan: string | null;
  identityKey: string;
  credentialRevision: string;
  identityVerified: boolean;
  available: boolean;
  selected: boolean;
  runtimeVerified: boolean;
  verifiedAt: string | null;
}

/** Collected without installing the profile in the live CLI credential store. */
export interface AntigravityUsageSample {
  profileId: string;
  identityKey: string;
  credentialRevision: string;
  status: 'ok' | 'fresh' | 'cached' | 'rate_limited' | 'needs_sign_in' | 'unavailable' | 'error';
  email: string | null;
  plan: string | null;
  fetchedAt: string | null;
  sampledAt: string | null;
  windows: unknown[];
  retryAfterSeconds?: number;
}

export interface AntigravityPoolWindow extends DashboardAccountWindow {
  /** Actual provider ID or a marked ID of exact reported bucket membership. */
  poolId?: string;
  poolIdSource?: 'provider-id' | 'provider-bucket-membership';
  poolLabel?: string;
  modelIds?: string[];
}

export interface AntigravityDashboardAccount extends DashboardAccount {
  windows: AntigravityPoolWindow[];
  capabilities: DashboardAccount['capabilities'] & {
    antigravityProfileId: string;
    antigravityHostIds: AntigravityHostId[];
    antigravityCanActivate?: boolean;
  };
}

export interface AntigravityPublicProfile {
  id: string;
  email: string;
  plan: string | null;
  available: boolean;
  selected: boolean;
  runtimeVerified: boolean;
  verifiedAt: string | null;
  hostId: AntigravityHostId;
}

/**
 * Present only when the installed Antigravity CLI no longer matches the
 * reviewed native pin: switching is paused until the new build is reviewed.
 * Mirrors NativeUpdatePaused in native-version.ts.
 */
export interface AntigravityNativeUpdatePaused {
  installedVersion: string | null;
}

export interface AntigravityInventory {
  schemaVersion: 1;
  hostId: AntigravityHostId;
  profiles: AntigravityPublicProfile[];
  activationSupported?: boolean;
  nativeUpdatePaused?: AntigravityNativeUpdatePaused;
}

export interface AntigravityAutoSettings {
  enabled: boolean;
  thresholdUsedPercent: number;
  pollIntervalSeconds: number;
  maxQuotaAgeSeconds: number;
  cooldownSeconds: number;
  selectedHostIds: AntigravityHostId[];
  requestedPoolId: string | null;
}

export interface AntigravityApiDependencies {
  getInventory(): Promise<AntigravityInventory>;
  getAccounts(options: { refresh: boolean }): Promise<AntigravityDashboardAccount[]>;
  /** mode is supplied by the server; request bodies cannot enable automatic mode. */
  activate(request: ActivateRequest): Promise<ActivationResult>;
  /** Finish or undo a stuck switch; proof-driven, saved profiles untouched. */
  recover(): Promise<AntigravityRecoveryResult>;
  getAutoSwitchStatus(): unknown;
  updateAutoSwitchSettings(patch: Partial<AntigravityAutoSettings>): unknown;
  invalidateUsage?(): void;
}
