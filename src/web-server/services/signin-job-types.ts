import type { SignInCommand } from './signin-process';

/**
 * The sign-in job's public shape, its fixed messages and errors, and the flow
 * contract a provider implements (CONTRACT-registry-lifecycle section 6.6).
 * The runner lives in signin-jobs.ts, which re-exports all of this.
 */
export type SignInJobProvider = 'codex' | 'muse' | 'antigravity';
export type SignInJobKind = 'device-code' | 'supervised-cli';
export type SignInJobMode = 'add' | 'signin-again';
export type SignInJobState =
  | 'starting'
  | 'waiting'
  | 'awaiting_code'
  | 'verifying'
  | 'succeeded'
  | 'failed'
  | 'expired'
  | 'cancelled';
export type SignInJobErrorCode =
  | 'tool_missing'
  | 'unexpected_output'
  | 'identity_mismatch'
  | 'duplicate_identity'
  | 'timeout'
  | 'provider_denied'
  | 'write_failed'
  | 'server_restarted';

export interface SignInJob {
  id: string;
  provider: SignInJobProvider;
  kind: SignInJobKind;
  mode: SignInJobMode;
  accountId: string | null;
  profileName: string | null;
  platform: 'ubuntu' | 'mac';
  state: SignInJobState;
  verification: null | { url: string; userCode: string | null; expiresAt: string | null };
  result: null | { accountId: string; email: string | null; plan: string | null };
  error: null | { code: SignInJobErrorCode; message: string };
  startedAt: string;
  updatedAt: string;
  expiresAt: string;
}

/** What a job id the server never issued reads as: it began before a restart. */
export interface RestartedSignInJob {
  id: string;
  provider: null;
  kind: null;
  mode: null;
  accountId: null;
  profileName: null;
  platform: null;
  state: 'failed';
  verification: null;
  result: null;
  error: { code: 'server_restarted'; message: string };
  startedAt: null;
  updatedAt: string;
  expiresAt: null;
}

export const SIGNIN_ERROR_MESSAGES: Readonly<Record<SignInJobErrorCode, string>> = Object.freeze({
  tool_missing: 'The sign-in tool is not installed on this server.',
  unexpected_output:
    'The sign-in tool answered in a way this page does not recognize. Nothing was changed.',
  identity_mismatch: 'That sign-in belongs to a different account. Nothing was changed.',
  duplicate_identity: 'That account is already saved in another profile. Nothing was changed.',
  timeout: 'The sign-in was not finished in time. Nothing was changed.',
  provider_denied: 'The sign-in was not approved, or its code expired. Nothing was changed.',
  write_failed: 'The new sign-in could not be saved safely. Nothing was changed.',
  server_restarted: 'The server restarted, so this sign-in stopped. Start it again.',
});

export const JOB_ID_PATTERN = /^job_[a-f0-9]{16}$/;
/** URL-safe characters plus `/` (Google authorization codes look like `4/0Ab...`); one line only. */
export const SIGNIN_CODE_PATTERN = /^[A-Za-z0-9._~/-]{1,2048}$/;

export class SignInJobError extends Error {
  constructor(readonly code: SignInJobErrorCode) {
    super(SIGNIN_ERROR_MESSAGES[code]);
    this.name = 'SignInJobError';
  }
}

export class SignInJobConflict extends Error {
  constructor(
    readonly code: 'job_running' | 'too_many_jobs',
    readonly jobId: string | null
  ) {
    super(code === 'job_running' ? 'A sign-in is already running.' : 'Too many sign-ins running.');
    this.name = 'SignInJobConflict';
  }
}

/** Thrown by `complete` when a stop was requested before it committed anything. */
export class SignInJobStopped extends Error {
  constructor() {
    super('The sign-in was stopped before anything was saved.');
    this.name = 'SignInJobStopped';
  }
}

/** What `complete` may ask the runner while it installs a login. */
export interface SignInCompleteControl {
  /** True once a cancel, the shutdown or another stop asked this job to end. */
  stopped(): boolean;
}

export interface SignInFlowSpec {
  provider: SignInJobProvider;
  kind: SignInJobKind;
  mode: SignInJobMode;
  accountId: string | null;
  profileName: string | null;
  platform: 'ubuntu' | 'mac';
  allowedOrigins: readonly string[];
  timeoutMs: number;
  /** Runs once the id is reserved: staging folders, then the fixed command. */
  prepare(jobId: string): Promise<SignInCommand>;
  /**
   * After the CLI exits 0: verify the identity and install the login. It must
   * check `control.stopped()` under the lock that guards its commit and throw
   * SignInJobStopped when it is set, so a stopped job never installs anything.
   */
  complete(
    jobId: string,
    control: SignInCompleteControl
  ): Promise<{ accountId: string; email: string | null; plan: string | null }>;
  /** Runs once when the job ends, after any running `prepare` or `complete` has settled. */
  cleanup(jobId: string): Promise<void>;
}
