import os from 'os';
import {
  ANTIGRAVITY_MAX_PROFILES,
  AntigravityLifecycleError,
  type AntigravityAccountLifecycle,
  type AntigravityLifecycleCode,
} from '../../antigravity/account-lifecycle';
import {
  BWRAP,
  antigravitySignInPreflight,
  prepareSignInStaging,
  removeSignInStaging,
  sandboxArgs,
  sandboxEnvironment,
  type SignInPreflight,
  type SignInStaging,
} from '../../antigravity/signin-sandbox';
import { claimAntigravitySignInMarker, type SignInMarker } from '../../antigravity/signin-marker';
import { readMaskedCredential } from '../../antigravity/terminal-signin';
import {
  refreshRuntimeDescriptor,
  type RuntimeRefreshResult,
} from '../../antigravity/runtime-refresh';
import { AGY_DRIVER_PYTHON, AGY_SIGNIN_DRIVER_PROGRAM } from '../../antigravity/signin-driver';
import { LifecycleHttpError } from './account-lifecycle-accounts';
import {
  signInState,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import {
  antigravityTerminal,
  invalid,
  jobBody,
  keys,
  secure,
  startJob,
} from './account-lifecycle-helpers';
import {
  SignInJobError,
  SignInJobStopped,
  type SignInCompleteControl,
  type SignInFlowSpec,
} from './signin-jobs';

/**
 * Antigravity Add and Sign in again (CONTRACT-registry-lifecycle 6.2, 6.3, 6.6).
 *
 * When the isolated sign-in preflight passes on the dashboard host, both routes
 * run the official CLI as a supervised PTY sign-in job — the same runner Codex
 * and Muse use — so a new user adds an account from the dashboard with no
 * terminal. The job (antigravity/signin-driver.ts) starts the CLI inside the
 * signin-sandbox isolation, presses Enter through its first-run login-method
 * screen, surfaces the one Google authorization URL to the page, feeds the
 * pasted code back to the CLI, and stops it the moment the new credential is
 * complete. `complete` then imports and provider-verifies that credential
 * (importSignIn), best-effort refreshes the runtime descriptor, and the
 * runner's accounts-changed hint shows the account.
 *
 * When the preflight cannot run (no CLI, no bubblewrap, no unprivileged user
 * namespaces) or the transport is not trusted, the routes fall back to the
 * terminal command (`ai-account-center antigravity signin <profile>`): 409
 * `preflight_failed`/`tool_missing`, or 403 `secure_transport_required`, each
 * preflight/secure case carrying
 * `fallback: {kind:'terminal', host:'ubuntu', command}`. Remove lives with the
 * other removals (account-lifecycle-removal.ts).
 */

/** The one origin the driver and the job's output parser allow for the URL. */
export const ANTIGRAVITY_OAUTH_ORIGIN = 'https://accounts.google.com';
/** The CLI's own 15-minute sign-in window (terminal-signin.ts TIMEOUT_MS). */
export const ANTIGRAVITY_SIGNIN_TIMEOUT_MS = 15 * 60_000;

function jobErrorCode(code: AntigravityLifecycleCode): SignInJobError {
  switch (code) {
    case 'duplicate_identity':
      return new SignInJobError('duplicate_identity');
    case 'identity_mismatch':
      return new SignInJobError('identity_mismatch');
    default:
      return new SignInJobError('write_failed');
  }
}

/**
 * The supervised sign-in job for one Antigravity profile. `prepare` claims the
 * per-profile marker and an isolated staging home, then hands the runner the
 * fixed driver argv (bubblewrap + the official CLI, run on the driver's own
 * PTY). `complete` reads the new credential, imports and verifies it under the
 * registry lock (never committing after a cancel), and refreshes the runtime
 * descriptor. `cleanup` removes the staging home and releases the marker.
 */
export function antigravityJobFlow(
  agy: AntigravityAccountLifecycle,
  ccsDir: string,
  home: string,
  profileName: string,
  mode: 'add' | 'signin-again',
  deps: {
    preflight?: (home: string) => SignInPreflight;
    refreshDescriptor?: (request: {
      ccsDir: string;
      home: string;
    }) => Promise<RuntimeRefreshResult>;
  } = {}
): SignInFlowSpec {
  const accountId = mode === 'signin-again' ? `antigravity:profile:${profileName}` : null;
  let staging: SignInStaging | null = null;
  let marker: SignInMarker | null = null;
  return {
    provider: 'antigravity',
    kind: 'supervised-cli',
    mode,
    accountId,
    profileName,
    platform: 'ubuntu',
    allowedOrigins: [ANTIGRAVITY_OAUTH_ORIGIN],
    timeoutMs: ANTIGRAVITY_SIGNIN_TIMEOUT_MS,
    prepare: async () => {
      const preflight = (deps.preflight ?? antigravitySignInPreflight)(home);
      if (!preflight.ok) {
        throw new SignInJobError(
          preflight.reason === 'tool_missing' ? 'tool_missing' : 'write_failed'
        );
      }
      // A terminal sign-in for this profile holds the marker; never run twice.
      marker = claimAntigravitySignInMarker(ccsDir, profileName);
      if (!marker) throw new SignInJobError('write_failed');
      try {
        staging = prepareSignInStaging(ccsDir);
      } catch (error) {
        marker.release();
        marker = null;
        throw error instanceof AntigravityLifecycleError
          ? jobErrorCode(error.code)
          : new SignInJobError('write_failed');
      }
      return {
        file: AGY_DRIVER_PYTHON,
        args: [
          '-c',
          AGY_SIGNIN_DRIVER_PROGRAM,
          staging.token,
          ANTIGRAVITY_OAUTH_ORIGIN,
          '--',
          BWRAP,
          ...sandboxArgs(staging, {
            realHome: home,
            nativeBinary: preflight.nativeBinary,
            apparmorLeaf: preflight.apparmorLeaf,
            runtimeDir: preflight.runtimeDir ?? null,
          }),
        ],
        env: sandboxEnvironment(staging, { realHome: home, source: process.env }),
        // The driver allocates the CLI's PTY itself and needs plain pipes to
        // read the CLI's raw screen bytes and inject the menu Enter and code.
        pty: false,
      };
    },
    complete: async (_jobId: string, control: SignInCompleteControl) => {
      if (!staging) throw new SignInJobError('write_failed');
      if (control.stopped()) throw new SignInJobStopped();
      let credential;
      try {
        credential = readMaskedCredential(staging);
      } catch {
        throw new SignInJobError('write_failed');
      }
      let result;
      try {
        result = await agy.importSignIn({
          profileId: profileName,
          mode,
          credential,
          stopped: control.stopped,
        });
      } catch (error) {
        if (control.stopped()) throw new SignInJobStopped();
        throw error instanceof AntigravityLifecycleError
          ? jobErrorCode(error.code)
          : new SignInJobError('write_failed');
      }
      // Best effort: re-pin the installed runtime descriptor against the
      // reviewed set so a later activation can run. Never fails the sign-in.
      void (deps.refreshDescriptor ?? refreshRuntimeDescriptor)({ ccsDir, home }).catch(
        () => undefined
      );
      return {
        accountId: `antigravity:profile:${profileName}`,
        email: result.email,
        plan: result.plan,
      };
    },
    cleanup: async () => {
      if (staging) {
        try {
          removeSignInStaging(staging);
        } catch {
          /* Swept within a day. */
        }
        staging = null;
      }
      if (marker) {
        try {
          marker.release();
        } catch {
          /* Gone already. */
        }
        marker = null;
      }
    },
  };
}

/**
 * The transport/preflight gate for a route whose races already passed. Throws
 * the fallback-carrying refusal when the supervised flow cannot run now, else
 * returns so the caller starts the job. `reason` is the sign-in state's reason.
 */
function gateOrThrow(reason: string | null, context: LifecycleContext, profileName: string): void {
  if (reason === 'secure_transport_required') {
    // The code needs a trusted connection; offer the terminal command instead.
    secure(context, antigravityTerminal(profileName));
  }
  if (reason === 'tool_missing' || reason === 'not_implemented') {
    throw new LifecycleHttpError(409, reason);
  }
  if (reason) {
    // preflight_failed (or any other reason): the terminal command still works.
    throw new LifecycleHttpError(409, 'preflight_failed', {
      fallback: antigravityTerminal(profileName),
    });
  }
}

/** Add: refusals (`id_in_use`, `too_many_accounts`) come first, then the job. */
export async function addAntigravity(
  env: LifecycleEnv,
  agy: AntigravityAccountLifecycle,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  keys(body, ['provider', 'profileName']);
  if (agy.nameError(body.profileName)) throw invalid();
  const name = body.profileName as string;
  let exists: boolean;
  let count: number;
  try {
    exists = agy.hasProfile(name);
    count = agy.profileCount();
  } catch {
    throw new LifecycleHttpError(500, 'registry_unavailable');
  }
  if (exists) throw new LifecycleHttpError(409, 'id_in_use');
  if (count >= ANTIGRAVITY_MAX_PROFILES) throw new LifecycleHttpError(409, 'too_many_accounts');
  const reason = signInState(env, 'antigravity', context.secure).unavailableReason;
  gateOrThrow(reason, context, name);
  // The flow can run: refuse the transient races before reserving a job.
  try {
    if (agy.activationRunning()) throw new LifecycleHttpError(409, 'activation_running');
    if (agy.signInRunning(name)) throw new LifecycleHttpError(409, 'signin_running');
  } catch (error) {
    if (error instanceof LifecycleHttpError) throw error;
    throw new LifecycleHttpError(500, 'registry_unavailable');
  }
  const build =
    env.antigravityJobFlow ?? antigravityJobFlow.bind(null, agy, env.ccsDir(), os.homedir());
  const job = startJob(env, build(name, 'add'));
  env.audit('accounts.add', { provider: 'antigravity', kind: 'supervised-cli' });
  return { status: 202, body: { job: jobBody(job, context) } };
}

/**
 * Antigravity Sign in again: refusals first (the live login and the transient
 * races), then the supervised job, or the terminal fallback when the flow
 * cannot run. The live native login is asked too; when that check cannot run,
 * the terminal command asks again before it starts anything.
 */
export async function antigravitySignInAgain(
  env: LifecycleEnv,
  agy: AntigravityAccountLifecycle,
  profileId: string,
  context: LifecycleContext
): Promise<LifecycleResult> {
  const reason = signInState(env, 'antigravity', context.secure).unavailableReason;
  if (reason === 'tool_missing' || reason === 'not_implemented') {
    throw new LifecycleHttpError(409, reason);
  }
  let refusal: string | null;
  try {
    refusal = agy.activationRunning()
      ? 'activation_running'
      : agy.runtimeActiveProfileId() === profileId
        ? 'account_active'
        : agy.signInRunning(profileId)
          ? 'signin_running'
          : null;
  } catch {
    throw new LifecycleHttpError(500, 'registry_unavailable');
  }
  if (!refusal) {
    // The live native login is asked too; when that check cannot run, the
    // terminal command asks again before it starts anything.
    refusal = (await agy.isLiveNativeProfile(profileId).catch(() => false))
      ? 'account_active'
      : null;
  }
  if (refusal) throw new LifecycleHttpError(409, refusal);
  gateOrThrow(reason, context, profileId);
  const build =
    env.antigravityJobFlow ?? antigravityJobFlow.bind(null, agy, env.ccsDir(), os.homedir());
  const job = startJob(env, build(profileId, 'signin-again'));
  env.audit('accounts.signin-again', { provider: 'antigravity', kind: 'supervised-cli' });
  return { status: 202, body: { job: jobBody(job, context) } };
}
