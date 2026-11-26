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
  verifiedAt: string | null;
  hostId: AntigravityHostId;
}

export interface AntigravityInventory {
  schemaVersion: 1;
  hostId: AntigravityHostId;
  profiles: AntigravityPublicProfile[];
}

export interface AntigravityApiDependencies {
  getInventory(): Promise<AntigravityInventory>;
  getAccounts(options: { refresh: boolean }): Promise<AntigravityDashboardAccount[]>;
  invalidateUsage?(): void;
}
