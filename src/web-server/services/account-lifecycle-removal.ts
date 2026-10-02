import { CONFIRMATION_TOKEN_PATTERN, stateFingerprint } from './account-confirmations';
import { isKeyProvider } from './account-key-store';
import { LifecycleHttpError, type ResolvedAccount } from './account-lifecycle-accounts';
import {
  resolveIn,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import { updateAccountRegistry } from './account-registry-v2';
import { ClaudeLifecycleError } from './claude-account-lifecycle';
import { TRASH_ID } from './claude-account-stores';
import { CodexLifecycleError } from './codex-account-lifecycle';

/**
 * Remove and trash (CONTRACT-registry-lifecycle 6.7 and 6.8). One route, two
 * calls: `{}` returns a confirmation token with fixed effect sentences, and
 * `{confirmationToken}` performs the removal. Every refusal is checked at both
 * calls; the token is bound to the session and to the reviewed state.
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
      fingerprint: async () =>
        stateFingerprint({
          id: entry.id,
          platform: entry.platform,
          sshHost: entry.sshHost,
          credential: entry.credential,
        }),
      commit: async () => {
        if (keyed && entry.credential.kind === 'aac-key' && isKeyProvider(entry.provider)) {
          const store = env.keyStore({ platform: entry.platform, sshHost: entry.sshHost });
          if (!store) throw new LifecycleHttpError(500, 'remove_failed');
          await store.delete(entry.provider, entry.credential.keyId).catch(() => {
            throw new LifecycleHttpError(500, 'remove_failed');
          });
        }
        await updateAccountRegistry(env.ccsDir(), (current) => ({
          ...current,
          accounts: current.accounts.filter((candidate) => candidate.id !== entry.id),
        })).catch(() => {
          throw new LifecycleHttpError(500, 'remove_failed');
        });
        return { trashId: null, purgeAfter: null };
      },
    };
  }
  // Antigravity snapshots (Codex's registry lane) and console wallets.
  throw new LifecycleHttpError(409, 'not_implemented');
}

function mapError(error: unknown): LifecycleHttpError {
  if (error instanceof LifecycleHttpError) return error;
  if (error instanceof CodexLifecycleError || error instanceof ClaudeLifecycleError) {
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
    const code = await plan.refusal().catch(() => null);
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
