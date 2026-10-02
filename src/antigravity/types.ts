/** Mac/Windows controls address this Ubuntu target; their local accounts are untouched. */
export type AntigravityHostId = 'ubuntu';

/** Driver-private native data. Never serialize this object into an HTTP response. */
export interface NativeCredential {
  format: string;
  bytes: Buffer;
}

/** Authoritative provider/runtime identity, not an unverified decoded JWT claim. */
export interface VerifiedIdentity {
  email: string;
  subject: string;
  plan: string | null;
  verifiedAt: string;
  source: 'provider-userinfo' | 'native-runtime';
}

export interface ProcessIdentity {
  pid: number;
  startTime: string;
  ownerId: string;
  fingerprint: string;
}

export type ProcessRole = 'cli' | 'desktop' | 'language-server';

export interface ReviewedProcess {
  identity: ProcessIdentity;
  role: ProcessRole;
}

export interface ProcessPlan {
  processes: ReviewedProcess[];
  /** True only after the driver reviews executable, owner, args and ancestry. */
  complete: boolean;
  /** Hash of exact session/checkpoint/history path identities; never prompts or paths. */
  continuity: { fingerprint: string; restorable: boolean } | null;
}

export interface StopReceipt {
  stopped: ProcessIdentity[];
  complete: boolean;
  /** Args, working directories, environment and PTY data stay driver-private. */
  restartState: unknown;
}

export interface InstallReceipt {
  installedFingerprint: string;
  /** An opaque rollback handle bound to the exact owned native replacement. */
  rollbackState: unknown;
}

export interface RuntimeProof {
  identity: VerifiedIdentity;
  credentialFingerprint: string;
  runtimeStarted: boolean;
  /** Exact session restored in its original project and attached terminal, not merely same cwd. */
  sessionRestored: boolean;
}

/** Driver-private earned stop proof, never a public settings or HTTP field. */
export interface QuiescedHostProof {
  hostId: 'ubuntu';
  available: boolean;
  complete: boolean;
  busy: boolean;
  manualActivationInProgress: boolean;
  sampledAt: string;
}

/**
 * Each driver addresses one explicit host. Implementations must not modify
 * Gemini credentials, browser logins, application configuration or history.
 * Errors are never passed through to HTTP because native tools may echo secrets.
 */
export interface AntigravitySwitchDriver {
  hostId: AntigravityHostId;
  canProveRuntimeIdentity(): Promise<boolean>;
  readCurrentCredential(): Promise<NativeCredential>;
  validateCredential(credential: NativeCredential): Promise<VerifiedIdentity>;
  inspectProcesses(): Promise<ProcessPlan>;
  /** Fresh owned idle proof binds the exact plan/account/revision before any stop. */
  approveOwnedIdlePlan?(
    plan: ProcessPlan,
    expected: VerifiedIdentity,
    credentialFingerprint: string
  ): Promise<boolean>;
  /** Earn a new quiesced proof from the exact owned stop receipt, not cached idle. */
  revalidateQuiescedStop?(
    receipt: StopReceipt,
    expected: VerifiedIdentity,
    credentialFingerprint: string
  ): Promise<QuiescedHostProof | null>;
  /** Release blocked foreground input only after the bound account/session proof. */
  completeActivation?(expected: VerifiedIdentity): Promise<void>;
  /** Source/native executable pin only; does not pretend a stopped runtime is live. */
  canRestartSupportedNative?(): Promise<boolean>;
  /**
   * Revalidate each PID's birth, owner, fingerprint and fresh continuity checkpoint
   * immediately before signalling. Refuse work without exact resume/PTY support.
   */
  stopProcesses(plan: ProcessPlan): Promise<StopReceipt>;
  /**
   * Compare current native data with expectedFingerprint immediately before an
   * atomic replacement. The receipt must support rollback only while this exact
   * owned replacement remains installed, preserving foreign replacements.
   */
  installCredential(
    credential: NativeCredential,
    expectedFingerprint: string
  ): Promise<InstallReceipt>;
  readStoredIdentity(): Promise<VerifiedIdentity>;
  restartProcesses(receipt: StopReceipt): Promise<void>;
  /**
   * Start an owned, non-inference runtime probe when no programs were running.
   * The proof must come from that runtime, not merely a native file/keyring read.
   */
  proveRuntimeIdentity(expected: VerifiedIdentity): Promise<RuntimeProof>;
  /** Never overwrite an unrelated/foreign credential replacement during rollback. */
  rollbackCredential(receipt: InstallReceipt, previous: NativeCredential): Promise<boolean>;
  /** Stop only transaction-owned restarts/probes, with PID/birth revalidation. */
  stopOwnedRestarts(): Promise<void>;
}

export interface SafeProcessDisplay {
  pid: number;
  label: 'Antigravity CLI' | 'Antigravity Desktop' | 'Antigravity language server';
  role: ProcessRole;
}

export interface ActivationConfirmation {
  token: string;
  expiresAt: string;
  profileId: string;
  hostId: AntigravityHostId;
  email: string;
  processes: SafeProcessDisplay[];
  warning: string;
}

export type ActivationStatus =
  | 'active'
  | 'already-active'
  | 'busy'
  | 'confirmation-required'
  | 'stale-confirmation'
  | 'invalid-profile'
  | 'unsupported-runtime-probe'
  | 'deferred'
  | 'failed-rolled-back'
  | 'recovery-required';

export interface ActivationResult {
  status: ActivationStatus;
  profileId: string;
  hostId: AntigravityHostId;
  email?: string;
  reason?:
    | 'activation-running'
    | 'running-processes'
    | 'unreviewed-processes'
    | 'active-identity-changed'
    | 'quota-changed'
    | 'identity-verification-failed'
    | 'transaction-failed'
    | 'foreign-replacement';
  confirmation?: ActivationConfirmation;
}

export interface AutomaticRevalidationContext {
  hostId: AntigravityHostId;
  currentIdentityKey: string;
  targetIdentityKey: string;
  currentEmail: string;
  targetEmail: string;
  phase?: 'before-stop' | 'before-install';
  quiescedHost?: QuiescedHostProof;
}

export interface ActivateRequest {
  profileId: string;
  hostId: AntigravityHostId;
  mode: 'manual' | 'automatic';
  confirmationToken?: string;
  /** Required for automatic activation; binds a policy decision to the actual live account. */
  expectedActiveIdentityKey?: string;
  /** Runs under the transaction lock before any credential write or process stop. */
  revalidateAutomatic?: (context: AutomaticRevalidationContext) => Promise<boolean>;
}

export interface ProfileDto {
  id: string;
  email: string;
  plan: string | null;
  identityKey: string;
  hosts: Array<{
    hostId: AntigravityHostId;
    available: boolean;
    active: boolean;
    selected?: boolean;
    verifiedAt: string | null;
    verification: 'runtime' | 'stored-only';
  }>;
}
