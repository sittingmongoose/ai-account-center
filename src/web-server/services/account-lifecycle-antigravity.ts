import {
  ANTIGRAVITY_MAX_PROFILES,
  type AntigravityAccountLifecycle,
} from '../../antigravity/account-lifecycle';
import { LifecycleHttpError } from './account-lifecycle-accounts';
import {
  signInState,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import { antigravityTerminal, invalid, keys } from './account-lifecycle-helpers';

/**
 * Antigravity Add and Sign in again (CONTRACT-registry-lifecycle 6.2, 6.3).
 * The sign-in itself runs in the user's terminal on Ubuntu
 * (`ai-account-center antigravity signin <profile>`, src/antigravity/
 * terminal-signin.ts), so both routes check their refusals and then answer
 * 409 `preflight_failed` with `fallback: {kind: 'terminal', host: 'ubuntu',
 * command}`. Remove lives with the other removals (account-lifecycle-removal.ts).
 */
/**
 * Antigravity Add: the sign-in runs in the user's terminal on Ubuntu, so a
 * valid, free profile name answers 409 `preflight_failed` with the terminal
 * command as `fallback`. Nothing secret crosses HTTP, so this needs no secure
 * transport; refusals (`id_in_use`, `too_many_accounts`, `tool_missing`) come
 * first.
 */
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
  if (reason === 'tool_missing' || reason === 'not_implemented')
    throw new LifecycleHttpError(409, reason);
  throw new LifecycleHttpError(409, 'preflight_failed', { fallback: antigravityTerminal(name) });
}

/** Antigravity Sign in again: refusals first, then the terminal command for this profile. */
export async function antigravitySignInAgain(
  env: LifecycleEnv,
  agy: AntigravityAccountLifecycle,
  profileId: string,
  context: LifecycleContext
): Promise<LifecycleResult> {
  const reason = signInState(env, 'antigravity', context.secure).unavailableReason;
  if (reason === 'tool_missing' || reason === 'not_implemented')
    throw new LifecycleHttpError(409, reason);
  let refusal: string | null;
  try {
    refusal = agy.activationRunning()
      ? 'activation_running'
      : agy.runtimeActiveProfileId() === profileId
        ? 'account_active'
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
  throw new LifecycleHttpError(409, 'preflight_failed', {
    fallback: antigravityTerminal(profileId),
  });
}
