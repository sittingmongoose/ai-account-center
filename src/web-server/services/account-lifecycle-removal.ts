import { CONFIRMATION_TOKEN_PATTERN, stateFingerprint } from './account-confirmations';
import { isKeyProvider } from './account-key-store';
import {
  LifecycleHttpError,
  readAdditionalEntries,
  type ResolvedAccount,
} from './account-lifecycle-accounts';
import {
  keyQueue,
  resolveIn,
  serialized,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import { updateAccountRegistry, type RegistryAccount } from './account-registry-v2';
import { ClaudeLifecycleError } from './claude-account-lifecycle';
import { TRASH_ID } from './claude-account-stores';
import { CodexLifecycleError } from './codex-account-lifecycle';
import { AntigravityLifecycleError } from '../../antigravity/account-lifecycle';

/**
 * Remove and trash (CONTRACT-registry-lifecycle 6.7 and 6.8). One route, two
 * calls: `{}` returns a confirmation token with fixed effect sentences, and
 * `{confirmationToken}` performs the removal. Every refusal is checked at both
 * calls; the token is bound to the session and to the reviewed state. A
 * refusal check that cannot run refuses (500 `remove_failed`), never passes.
 */
const PLATFORM_NAMES = { ubuntu: 'Ubuntu', mac: 'Mac', windows: 'Windows' } as const;
const STATUS: Record<string, number> = {
  unknown_account: 404,
  remove_failed: 500,
  write_failed: 500,
  restore_failed: 500,
  host_unreachable: 502,
  unknown_trash: 404,
};

function confirmationToken(body: Record<string, unknown>): string | null {
  const names = Object.keys(body);
  if (names.length === 0) return null;
  if (
    names.length !== 1 ||
    names[0] !== 'confirmationToken' ||
    typeof body.confirmationToken !== 'string' ||
    !CONFIRMATION_TOKEN_PATTERN.test(body.confirmationToken)
  ) {
    throw new LifecycleHttpError(400, 'invalid_body');
  }
  return body.confirmationToken;
}

interface RemovePlan {
  kind: string;
  effects: string[];
  refusal: () => Promise<string | null>;
  fingerprint: () => Promise<string>;
  commit: () => Promise<{ trashId: string | null; purgeAfter: string | null }>;
}

function planFor(env: LifecycleEnv, account: ResolvedAccount): RemovePlan {
  const running = () => env.runner().runningForAccount(account.id) !== null;
  if (account.kind === 'codex') {
    const codex = env.codex();
    return {
      kind: 'device-code',
      effects: [
        'The saved Codex login of this profile is deleted from Ubuntu.',
        'The live Codex login and the other profiles are not changed.',
      ],
      refusal: () => codex.removeRefusal(account.name, running()),
      fingerprint: () => codex.removeFingerprint(account.name),
      commit: async () => {
        await codex.remove(account.name);
        return { trashId: null, purgeAfter: null };
      },
    };
  }
  if (account.kind === 'claude') {
    const claude = env.claude();
    // A computer's default Claude profile is refused first (and audited like every refusal), whether or
    // not the host steps are on.
    if (account.profile.isDefault) {
      return {
        kind: 'desktop-profile',
        effects: [],
        refusal: async () => 'account_protected',
        fingerprint: async () => claude.removeFingerprint(account.profile),
        commit: async () => {
          throw new LifecycleHttpError(409, 'account_protected');
        },
      };
    }
    if (!claude.enabled) throw new LifecycleHttpError(409, 'not_implemented');
    return {
      kind: 'desktop-profile',
      effects: [
        'Its Claude data moves to the trash on Mac and Windows for 30 days.',
        'Its launchers are removed on Mac and Windows.',
      ],
      // Each host is asked whether the profile's app runs, at prepare and at commit.
      refusal: async () =>
        (await claude.removeRefusal(account.profile, true)) ??
        (running() ? 'signin_running' : null),
      fingerprint: async () => claude.removeFingerprint(account.profile),
      commit: () => claude.remove(account.profile),
    };
  }
  if (account.kind === 'additional') {
    const entry = account.entry;
    const keyed = entry.credential.kind === 'aac-key' && isKeyProvider(account.provider);
    return {
      kind: keyed ? 'api-key' : entry.credential.kind,
      effects: keyed
        ? [`The stored key is deleted from ${PLATFORM_NAMES[entry.platform]}.`]
        : [
            'The account is no longer read by the dashboard.',
            `Its sign-in on ${PLATFORM_NAMES[entry.platform]} is not changed.`,
          ],
      refusal: async () => (running() ? 'signin_running' : null),
      fingerprint: async () => reviewedEntry(entry),
      // In the provider's key queue, so a Replace key can never write the key back
      // after this deleted it (the queue also holds Add and the orphan-key sweep).
      commit: () => serialized(keyQueue(entry.provider), () => removeAdditional(env, entry)),
    };
  }
  if (account.kind === 'antigravity' && env.antigravity) {
    const agy = env.antigravity();
    return {
      kind: 'supervised-cli',
      effects: [
        'The saved Antigravity login of this profile is deleted from Ubuntu.',
        'The live Antigravity login, its history and the other profiles are not changed.',
      ],
      // The live native login is asked at both calls; a check that cannot run refuses.
      refusal: () =>
        agy.removeRefusal(account.profileId, { signinRunning: running(), fresh: true }),
      fingerprint: async () => agy.removeFingerprint(account.profileId),
      commit: async () => {
        const { leftInPlace } = await agy.remove(account.profileId);
        // Files that could not be proven ours stay for review; say so.
        if (leftInPlace > 0)
          env.audit('accounts.remove.left_in_place', {
            provider: 'antigravity',
            count: leftInPlace,
          });
        return { trashId: null, purgeAfter: null };
      },
    };
  }
  // Console wallets.
  throw new LifecycleHttpError(409, 'not_implemented');
}

function reviewedEntry(entry: RegistryAccount): string {
  return stateFingerprint({
    id: entry.id,
    platform: entry.platform,
    sshHost: entry.sshHost,
    credential: entry.credential,
  });
}

/**
 * The additional-provider remove, inside the provider's queue: the entry is
 * resolved again, dropped from the registry, and only then is its key deleted.
 * A key that cannot be deleted puts the entry back where it was, so the reply
 * "Nothing was changed" holds.
 */
async function removeAdditional(
  env: LifecycleEnv,
  reviewed: RegistryAccount
): Promise<{ trashId: null; purgeAfter: null }> {
  const { entries } = await readAdditionalEntries(env.ccsDir());
  const entry = entries.find((candidate) => candidate.id === reviewed.id);
  // Removed, or its store changed, since the user reviewed it.
  if (!entry || reviewedEntry(entry) !== reviewedEntry(reviewed)) {
    throw new LifecycleHttpError(409, 'confirmation_stale');
  }
  const credential = entry.credential;
  const keyed = credential.kind === 'aac-key' && isKeyProvider(entry.provider);
  const store = keyed ? env.keyStore({ platform: entry.platform, sshHost: entry.sshHost }) : null;
  if (keyed && !store) throw new LifecycleHttpError(500, 'remove_failed');
  let position = -1;
  await updateAccountRegistry(env.ccsDir(), (current) => {
    position = current.accounts.findIndex((candidate) => candidate.id === entry.id);
    if (position < 0) throw new LifecycleHttpError(409, 'confirmation_stale');
    return {
      ...current,
      accounts: current.accounts.filter((candidate) => candidate.id !== entry.id),
    };
  }).catch((error) => {
    throw error instanceof LifecycleHttpError
      ? error
      : new LifecycleHttpError(500, 'remove_failed');
  });
  if (store && credential.kind === 'aac-key' && isKeyProvider(entry.provider)) {
    try {
      await store.delete(entry.provider, credential.keyId);
    } catch {
      await updateAccountRegistry(env.ccsDir(), (current) => {
        if (current.accounts.some((candidate) => candidate.id === entry.id)) return current;
        const accounts = [...current.accounts];
        accounts.splice(Math.min(position, accounts.length), 0, entry);
        return { ...current, accounts };
      }).catch(() => undefined);
      throw new LifecycleHttpError(500, 'remove_failed');
    }
  }
  return { trashId: null, purgeAfter: null };
}

function mapError(error: unknown): LifecycleHttpError {
  if (error instanceof LifecycleHttpError) return error;
  if (
    error instanceof CodexLifecycleError ||
    error instanceof ClaudeLifecycleError ||
    error instanceof AntigravityLifecycleError
  ) {
    const host = error instanceof ClaudeLifecycleError && error.host ? { host: error.host } : {};
    return new LifecycleHttpError(STATUS[error.code] ?? 409, error.code, host);
  }
  return new LifecycleHttpError(500, 'remove_failed');
}

/** POST /api/accounts/:id/remove */
export async function removeAccount(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  const token = confirmationToken(body);
  let account: ResolvedAccount;
  try {
    account = await resolveIn(env, id);
  } catch (error) {
    // A confirmed remove of an account that is already gone: that review is stale.
    if (token !== null && error instanceof LifecycleHttpError && error.code === 'unknown_account') {
      throw new LifecycleHttpError(409, 'confirmation_stale');
    }
    throw error;
  }
  const plan = planFor(env, account);
  const refused = async () => {
    let code: string | null;
    try {
      code = await plan.refusal();
    } catch {
      // A safety check that could not run is never a pass.
      env.audit('accounts.remove.refused', { provider: account.provider, code: 'remove_failed' });
      throw new LifecycleHttpError(500, 'remove_failed');
    }
    if (code) {
      env.audit('accounts.remove.refused', { provider: account.provider, code });
      throw new LifecycleHttpError(409, code);
    }
  };
  await refused();
  const binding = {
    action: 'remove' as const,
    subject: account.id,
    sessionKey: context.sessionKey,
    stateFingerprint: await plan.fingerprint(),
  };
  if (token === null) {
    const issued = env.confirmations().issue(binding);
    return {
      status: 200,
      body: { confirmation: { ...issued, effects: plan.effects } },
    };
  }
  if (!env.confirmations().consume(token, binding)) {
    throw new LifecycleHttpError(409, 'confirmation_stale');
  }
  await refused();
  let outcome: { trashId: string | null; purgeAfter: string | null };
  try {
    outcome = await plan.commit();
  } catch (error) {
    throw mapError(error);
  }
  env.audit('accounts.remove', {
    provider: account.provider,
    kind: plan.kind,
    trashed: outcome.trashId !== null,
  });
  env.onChanged();
  return { status: 200, body: { removed: true, ...outcome } };
}

/** GET /api/accounts/trash */
export async function trashListing(env: LifecycleEnv): Promise<LifecycleResult> {
  const entries = await env
    .claude()
    .listTrash()
    .catch(() => {
      throw new LifecycleHttpError(500, 'registry_unavailable');
    });
  return { status: 200, body: { entries } };
}

/** POST /api/accounts/trash/:trashId/restore (confirm-token flow). */
export async function restoreTrash(
  env: LifecycleEnv,
  trashId: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  if (!TRASH_ID.test(trashId)) throw new LifecycleHttpError(400, 'invalid_account');
  const token = confirmationToken(body);
  const claude = env.claude();
  if (!claude.enabled) throw new LifecycleHttpError(409, 'not_implemented');
  const entry = await claude.findTrash(trashId).catch(() => null);
  if (!entry || entry.state !== 'trashed') throw new LifecycleHttpError(404, 'unknown_trash');
  const profileId = entry.accountId.slice('claude:'.length);
  if (await claude.findProfile(profileId).catch(() => null)) {
    throw new LifecycleHttpError(409, 'id_in_use');
  }
  const binding = {
    action: 'trash-restore' as const,
    subject: trashId,
    sessionKey: context.sessionKey,
    stateFingerprint: claude.restoreFingerprint(entry),
  };
  if (token === null) {
    return {
      status: 200,
      body: {
        confirmation: {
          ...env.confirmations().issue(binding),
          effects: [
            'Its Claude data moves back on Mac and Windows.',
            'Its launchers are created again.',
          ],
        },
      },
    };
  }
  if (!env.confirmations().consume(token, binding)) {
    throw new LifecycleHttpError(409, 'confirmation_stale');
  }
  let restored: { accountId: string };
  try {
    restored = await claude.restore(trashId);
  } catch (error) {
    throw mapError(error);
  }
  env.audit('accounts.trash.restore', { provider: 'claude' });
  env.onChanged();
  return { status: 200, body: { restored: true, accountId: restored.accountId } };
}
