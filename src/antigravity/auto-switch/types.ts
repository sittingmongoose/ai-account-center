/** Antigravity automatic switching is scoped to the existing Ubuntu CLI home. */
export type AntigravityAutoHostId = 'ubuntu';

export interface AntigravityAutoSwitchSettings {
  enabled: boolean;
  /** Percentage USED, unlike the compatibility Codex remaining-percent setting. */
  thresholdUsedPercent: number;
  pollIntervalSeconds: number;
  maxQuotaAgeSeconds: number;
  cooldownSeconds: number;
  selectedHostIds: AntigravityAutoHostId[];
  /** Exact adapter quota-pool ID bound to provider IDs/membership; no label inference. */
  requestedPoolId: string | null;
}

export type AntigravityAutoSwitchSettingsPatch = Partial<AntigravityAutoSwitchSettings>;

export interface AntigravityAutoSwitchLastSwitch {
  at: string;
  hostId: AntigravityAutoHostId;
  profileId: string;
}

export interface AntigravityAutoSwitchStoredState {
  version: 1;
  settings: AntigravityAutoSwitchSettings;
  lastSwitch: AntigravityAutoSwitchLastSwitch | null;
}

/** Internal adapter contract; never serialize credential revisions in public DTOs. */
export interface AntigravityAutoProfile {
  id: string;
  hostId: AntigravityAutoHostId;
  identityKey: string;
  credentialRevision: string;
  authValid: boolean;
  nativeConsumerCompatible: boolean;
  isActive: boolean;
}

export interface AntigravityAutoQuotaWindow {
  key: string;
  kind: 'rate_limit' | 'balance' | 'extra_usage';
  remainingPercent: number | null;
  resetAt: string | null;
  enabled?: boolean;
  unlimited?: boolean;
}

export interface AntigravityAutoQuotaPool {
  /** Supplied by the quota adapter, never guessed from display names/model labels. */
  id: string;
  /** A derived group ID must be identified as such, not called a provider canonical ID. */
  idSource: 'provider-id' | 'provider-bucket-membership';
  /** Reported authenticated quota is eligibility evidence, not an invented plan flag. */
  eligibility: 'reported-quota' | 'provider-entitlement' | 'unverified';
  /** All provider-reported constraints for this pool were successfully parsed. */
  complete: boolean;
  windows: AntigravityAutoQuotaWindow[];
}

export interface AntigravityAutoQuotaSnapshot {
  profileId: string;
  hostId: AntigravityAutoHostId;
  identityKey: string;
  credentialRevision: string;
  source: 'native-consumer';
  status: 'fresh' | 'cached' | 'rate_limited' | 'error' | 'unavailable';
  sampledAt: string;
  pools: AntigravityAutoQuotaPool[];
}

export interface AntigravityAutoHostCensus {
  hostId: AntigravityAutoHostId;
  /** False/unknown host observations are never considered idle. */
  available: boolean;
  complete: boolean;
  busy: boolean;
  manualActivationInProgress: boolean;
  sampledAt: string;
}

export interface AntigravityAutoObservation {
  profiles: AntigravityAutoProfile[];
  quotas: AntigravityAutoQuotaSnapshot[];
  hosts: AntigravityAutoHostCensus[];
}

export type AntigravityAutoSwitchOutcome =
  | 'disabled'
  | 'scheduled'
  | 'setup_required'
  | 'healthy'
  | 'no_fresh_quota'
  | 'no_candidate'
  | 'waiting_idle'
  | 'cooldown'
  | 'switching'
  | 'switched'
  | 'deferred'
  | 'error';

export interface AntigravityAutoSwitchStatus extends AntigravityAutoSwitchSettings {
  outcome: AntigravityAutoSwitchOutcome;
  message: string;
  activationInProgress: boolean;
  lastCheckedAt?: string;
  lastSwitchedAt?: string;
  lastProfileId?: string;
  lastHostId?: AntigravityAutoHostId;
}

export interface AntigravityAutomaticActivationRequest {
  profileId: string;
  hostId: AntigravityAutoHostId;
  mode: 'automatic';
  expectedActiveIdentityKey: string;
  /** MUST run under the manual transaction lock, before preparation or writes. */
  revalidateAutomatic: (context: {
    hostId: AntigravityAutoHostId;
    currentIdentityKey: string;
    targetIdentityKey: string;
    phase?: 'before-stop' | 'before-install';
    quiescedHost?: AntigravityAutoHostCensus;
  }) => Promise<boolean>;
}

export interface AntigravityAutomaticActivationResult {
  status:
    | 'active'
    | 'already-active'
    | 'busy'
    | 'confirmation-required'
    | 'failed-rolled-back'
    | 'recovery-required'
    | 'invalid-profile'
    | 'unsupported-runtime-probe'
    | 'stale-confirmation'
    | 'deferred';
}

export interface AntigravityAutoSwitchStore {
  read(): AntigravityAutoSwitchStoredState;
  write(state: AntigravityAutoSwitchStoredState): void;
}

export interface AntigravityAutoSwitchDeps {
  store: AntigravityAutoSwitchStore;
  /** Must refresh quota/census; cached display DTOs are not accepted as fresh input. */
  observe: (quiescedHost?: AntigravityAutoHostCensus) => Promise<AntigravityAutoObservation>;
  activate: (
    request: AntigravityAutomaticActivationRequest
  ) => Promise<AntigravityAutomaticActivationResult>;
  now?: () => number;
  setTimer?: (callback: () => void, milliseconds: number) => unknown;
  clearTimer?: (timer: unknown) => void;
}
