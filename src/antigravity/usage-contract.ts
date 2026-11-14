import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../web-server/services/account-dashboard-types';
import type { AntigravityReportedPlan } from './plan';

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
  reportedPlan?: AntigravityReportedPlan;
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
 * reviewed native pin: managed updates are paused until the new build is reviewed.
 * Mirrors NativeUpdatePaused in native-version.ts.
 */
export interface AntigravityNativeUpdatePaused {
  installedVersion: string | null;
}

/**
 * Present only when the installed runtime service is not running and a
 * read-only check found why (runtime-health.ts): a parser module the service
 * cannot load, or a failed user unit. Starting the service again would fail.
 */
export interface AntigravityRuntimeServiceProblem {
  reason: 'missing-python-module' | 'parser-mismatch' | 'parser-import-failed' | 'service-failed';
  /** The Python module named by the failure, such as `pyte`. */
  module: string | null;
  /** The system Python minor version that runs the service, such as `3.14`. */
  python: string | null;
  /** The Python minor version the runtime bundle was built with. */
  builtFor: string | null;
  /** The failed unit's main exit status, when known. */
  exitStatus: number | null;
}

export interface AntigravityInventory {
  schemaVersion: 1;
  hostId: AntigravityHostId;
  profiles: AntigravityPublicProfile[];
  nativeUpdatePaused?: AntigravityNativeUpdatePaused;
  runtimeServiceProblem?: AntigravityRuntimeServiceProblem;
}

export interface AntigravityApiDependencies {
  getInventory(): Promise<AntigravityInventory>;
  getAccounts(options: { refresh: boolean }): Promise<AntigravityDashboardAccount[]>;
  invalidateUsage?(): void;
}
