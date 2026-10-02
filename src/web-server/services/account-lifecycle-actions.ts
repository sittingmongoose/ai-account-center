import type { DashboardProvider } from './account-dashboard-types';
import { isDashboardProviderId } from './dashboard-provider-table';
import {
  API_KEY_PATTERN,
  isKeyProvider,
  keyFingerprint,
  newKeyId,
  type KeyProvider,
  type StoredKeyInfo,
} from './account-key-store';
import {
  accountView,
  LifecycleHttpError,
  readAdditionalEntries,
  type ResolvedAccount,
} from './account-lifecycle-accounts';
import {
  keyQueue,
  providerAccountCount,
  resolveIn,
  serialized,
  signInState,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import {
  registrySource,
  updateAccountRegistry,
  MAX_ACCOUNTS_PER_PROVIDER,
  MAX_REGISTRY_ACCOUNTS,
  type RegistryAccount,
} from './account-registry-v2';
import { ClaudeLifecycleError } from './claude-account-lifecycle';
import { CLAUDE_PROFILE_ID } from './claude-account-stores';
import { parseSourceManifest, readSourceManifestFile } from './account-usage-manifest';
import {
  CODEX_TERMINAL,
  entryView,
  invalid,
  jobBody,
  keys,
  label,
  secure,
  startJob,
} from './account-lifecycle-helpers';

/**
 * Add, Sign in again, Replace key, label, Re-check, Open and the sign-in job
 * routes (CONTRACT-registry-lifecycle 6.1-6.6). Bodies arrive already checked
 * for Origin, JSON and size; each action checks its own strict schema.
 */
/** The 409 for a flow that cannot run now, in the contract's order. */
async function assertCanAdd(
  env: LifecycleEnv,
  provider: DashboardProvider,
  context: LifecycleContext
): Promise<void> {
  const state = signInState(env, provider, context.secure);
  if (state.unavailableReason === 'secure_transport_required') {
    secure(context, provider === 'codex' ? CODEX_TERMINAL : null);
  }
  if (state.unavailableReason) throw new LifecycleHttpError(409, state.unavailableReason);
  const count = await providerAccountCount(env, provider);
  if (!state.multiAccount && count > 0)
    throw new LifecycleHttpError(409, 'single_account_provider');
  if (count >= MAX_ACCOUNTS_PER_PROVIDER) throw new LifecycleHttpError(409, 'too_many_accounts');
}

export async function addAccount(
  env: LifecycleEnv,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  const provider = body.provider;
  if (!isDashboardProviderId(provider)) throw invalid();
  const kind = signInState(env, provider, context.secure).kind;
  // Rule 5 before any credential in the body is read.
  if (kind === 'api-key' || kind === 'device-code' || kind === 'supervised-cli') {
    secure(context, provider === 'codex' ? CODEX_TERMINAL : null);
  }
  if (kind === 'api-key') return addApiKey(env, provider as KeyProvider, body, context);
  if (provider === 'codex') {
    keys(body, ['provider', 'profileName']);
    const codex = env.codex();
    if (codex.nameError(body.profileName)) throw invalid();
    await assertCanAdd(env, provider, context);
    const name = body.profileName as string;
    if (await codex.nameInUse(name)) throw new LifecycleHttpError(409, 'id_in_use');
    const job = startJob(env, codex.addFlow(name));
    env.audit('accounts.add', { provider, kind });
    return { status: 202, body: { job: jobBody(job, context) } };
  }
  if (provider === 'claude') {
    keys(body, ['provider', 'profileId'], ['label']);
    if (typeof body.profileId !== 'string' || !CLAUDE_PROFILE_ID.test(body.profileId)) {
      throw invalid();
    }
    const name = label(body.label);
    await assertCanAdd(env, provider, context);
    let profile;
    try {
      profile = await env.claude().add({ profileId: body.profileId, label: name });
    } catch (error) {
      if (!(error instanceof ClaudeLifecycleError))
        throw new LifecycleHttpError(500, 'write_failed');
      const status =
        error.code === 'host_unreachable' ? 502 : error.code === 'write_failed' ? 500 : 409;
      throw new LifecycleHttpError(status, error.code, error.host ? { host: error.host } : {});
    }
    env.audit('accounts.add', { provider, kind });
    env.onChanged();
    return {
      status: 201,
      body: {
        account: accountView(
          {
            id: `claude:${profile.id}`,
            provider: 'claude',
            label: profile.label ?? profile.id,
            email: null,
            platform: 'mac',
            lifecycle: { state: 'pending_sign_in', jobId: null },
          },
          null,
          { signInAgain: true, replaceKey: false, remove: true, open: [], recheck: false },
          null
        ),
        launchers: { mac: 'created', windows: 'created' },
      },
    };
  }
  if (provider === 'cursor' || provider === 'qwen') {
    keys(body, ['provider']);
    await assertCanAdd(env, provider, context);
    // The provider's host as the version 1 sources name it (their defaults otherwise).
    const manifest = parseSourceManifest(
      await readSourceManifestFile(env.ccsDir()).catch(() => null)
    );
    const source = manifest.sources.find((candidate) => candidate.provider === provider);
    const registry = await updateAccountRegistry(env.ccsDir(), (current) => {
      if (current.accounts.some((entry) => entry.provider === provider)) {
        throw new LifecycleHttpError(409, 'single_account_provider');
      }
      if (current.accounts.length >= MAX_REGISTRY_ACCOUNTS) {
        throw new LifecycleHttpError(409, 'too_many_accounts');
      }
      return {
        ...current,
        accounts: [
          ...current.accounts,
          {
            id: `${provider}:usage`,
            provider,
            platform: source?.platform ?? 'ubuntu',
            sshHost: source?.sshHost ?? null,
            label: null,
            credential: { kind: 'discover' },
            createdAt: new Date(env.now()).toISOString().replace(/\.\d{3}Z$/, 'Z'),
            createdBy: null,
          },
        ],
      };
    }).catch((error) => {
      throw error instanceof LifecycleHttpError
        ? error
        : new LifecycleHttpError(500, 'write_failed');
    });
    const entry = registry.accounts.find((candidate) => candidate.id === `${provider}:usage`);
    env.audit('accounts.add', { provider, kind });
    env.onChanged();
    const view = entryView(entry as RegistryAccount, null, false);
    view.lifecycle = { state: 'pending_sign_in', jobId: null };
    return { status: 201, body: { account: view } };
  }
  // Muse and Antigravity: their flows are not served yet.
  await assertCanAdd(env, provider, context);
  throw new LifecycleHttpError(409, 'not_implemented');
}

async function addApiKey(
  env: LifecycleEnv,
  provider: KeyProvider,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  keys(body, ['provider', 'key'], ['label']);
  if (typeof body.key !== 'string' || !API_KEY_PATTERN.test(body.key)) throw invalid();
  const secret = body.key;
  const name = label(body.label);
  await assertCanAdd(env, provider, context);
  const store = env.keyStore({ platform: 'ubuntu', sshHost: null });
  if (!store) throw new LifecycleHttpError(500, 'key_store_unavailable');
  return serialized(keyQueue(provider), async () => {
    if ((await store.fingerprints(provider)).has(keyFingerprint(secret))) {
      throw new LifecycleHttpError(409, 'duplicate_key');
    }
    const { entries } = await readAdditionalEntries(env.ccsDir());
    if (
      entries.filter((entry) => entry.provider === provider).length >= MAX_ACCOUNTS_PER_PROVIDER ||
      entries.length >= MAX_REGISTRY_ACCOUNTS
    ) {
      throw new LifecycleHttpError(409, 'too_many_accounts');
    }
    let keyId = newKeyId();
    while (entries.some((entry) => entry.id === `${provider}:acct:${keyId}`)) keyId = newKeyId();
    let info: StoredKeyInfo;
    try {
      info = await store.put(provider, keyId, secret);
    } catch {
      throw new LifecycleHttpError(500, 'key_store_unavailable');
    }
    const entry: RegistryAccount = {
      id: `${provider}:acct:${keyId}`,
      provider,
      platform: 'ubuntu',
      sshHost: null,
      label: name,
      credential: { kind: 'aac-key', keyId },
      createdAt: new Date(env.now()).toISOString().replace(/\.\d{3}Z$/, 'Z'),
      createdBy: 'dashboard',
    };
    const without = (current: { version: 2; accounts: RegistryAccount[] }) => ({
      ...current,
      accounts: current.accounts.filter((candidate) => candidate.id !== entry.id),
    });
    try {
      await updateAccountRegistry(env.ccsDir(), (current) => {
        if (current.accounts.length >= MAX_REGISTRY_ACCOUNTS) {
          throw new LifecycleHttpError(409, 'too_many_accounts');
        }
        return { ...current, accounts: [...current.accounts, entry] };
      });
    } catch (error) {
      await store.delete(provider, keyId).catch(() => undefined);
      throw error instanceof LifecycleHttpError
        ? error
        : new LifecycleHttpError(500, 'write_failed');
    }
    const reading = await env.probe(registrySource(entry));
    if (reading.status === 'needs_sign_in') {
      await updateAccountRegistry(env.ccsDir(), without).catch(() => undefined);
      await store.delete(provider, keyId).catch(() => undefined);
      throw new LifecycleHttpError(422, 'key_rejected');
    }
    const check = reading.status === 'ok' || reading.status === 'cached' ? 'ok' : 'unverified';
    env.audit('accounts.add', { provider, kind: 'api-key' });
    env.onChanged();
    return {
      status: 201,
      body: { account: entryView(entry, info, true, { email: reading.email }), check },
    };
  });
}

/** The account's entry as the registry has it now (inside its key queue); 404 once it is gone. */
async function currentKeyEntry(
  env: LifecycleEnv,
  reviewed: RegistryAccount
): Promise<{ entry: RegistryAccount; entries: RegistryAccount[] }> {
  const { entries } = await readAdditionalEntries(env.ccsDir());
  const entry = entries.find((candidate) => candidate.id === reviewed.id);
  if (
    !entry ||
    entry.credential.kind !== 'aac-key' ||
    reviewed.credential.kind !== 'aac-key' ||
    entry.credential.keyId !== reviewed.credential.keyId ||
    entry.platform !== reviewed.platform ||
    entry.sshHost !== reviewed.sshHost
  ) {
    throw new LifecycleHttpError(404, 'unknown_account');
  }
  return { entry, entries };
}

/** PUT /api/accounts/:id/key */
export async function replaceKey(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  secure(context);
  const account = await resolveIn(env, id);
  if (account.kind !== 'additional' || !isKeyProvider(account.provider)) {
    throw new LifecycleHttpError(409, 'not_aac_owned');
  }
  const entry = account.entry;
  if (entry.credential.kind !== 'aac-key') throw new LifecycleHttpError(409, 'not_aac_owned');
  keys(body, ['key']);
  if (typeof body.key !== 'string' || !API_KEY_PATTERN.test(body.key)) throw invalid();
  const secret = body.key;
  const provider = account.provider;
  const keyId = entry.credential.keyId;
  const store = env.keyStore({ platform: entry.platform, sshHost: entry.sshHost });
  if (!store) throw new LifecycleHttpError(409, 'not_configured');
  // In the provider's key queue, which a Remove of this account also takes.
  return serialized(keyQueue(provider), async () => {
    const { entries } = await currentKeyEntry(env, entry);
    const own = await store.info(provider, keyId);
    const others = await store.fingerprints(provider);
    if (own) others.delete(own.fingerprint);
    if (others.has(keyFingerprint(secret))) throw new LifecycleHttpError(409, 'duplicate_key');
    // Probe the new key under a temporary id that no account and no file uses; the
    // account's own file changes only after.
    const taken = new Set(
      entries.flatMap((candidate) =>
        candidate.provider === provider && candidate.credential.kind === 'aac-key'
          ? [candidate.credential.keyId]
          : []
      )
    );
    let trialId = newKeyId();
    while (taken.has(trialId) || (await store.info(provider, trialId)) !== null) {
      trialId = newKeyId();
    }
    try {
      await store.put(provider, trialId, secret);
    } catch {
      throw new LifecycleHttpError(500, 'key_store_unavailable');
    }
    let info: StoredKeyInfo;
    let check: 'ok' | 'unverified';
    try {
      const trial: RegistryAccount = { ...entry, credential: { kind: 'aac-key', keyId: trialId } };
      const reading = await env.probe(registrySource(trial));
      if (reading.status === 'needs_sign_in') throw new LifecycleHttpError(422, 'key_rejected');
      check = reading.status === 'ok' || reading.status === 'cached' ? 'ok' : 'unverified';
      // The account must still exist before its key is written (never a key without an account).
      await currentKeyEntry(env, entry);
      info = await store.put(provider, keyId, secret).catch(() => {
        throw new LifecycleHttpError(500, 'key_store_unavailable');
      });
    } finally {
      await store.delete(provider, trialId).catch(() => undefined);
    }
    env.audit('accounts.key.replaced', { provider });
    env.onChanged();
    return { status: 200, body: { account: entryView(entry, info, true), check } };
  });
}

/** POST /api/accounts/:id/signin-again */
export async function signInAgain(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  keys(body, []);
  const account = await resolveIn(env, id);
  if (account.kind === 'codex') {
    secure(context, CODEX_TERMINAL);
    const state = signInState(env, 'codex', context.secure);
    if (state.unavailableReason) throw new LifecycleHttpError(409, state.unavailableReason);
    const codex = env.codex();
    if (await codex.isLiveProfile(account.name)) {
      throw new LifecycleHttpError(409, 'account_active');
    }
    const running = env.runner().runningForAccount(account.id);
    if (running) throw new LifecycleHttpError(409, 'job_running', { jobId: running.id });
    const job = startJob(env, codex.signInAgainFlow(account.name));
    return { status: 202, body: { job: jobBody(job, context) } };
  }
  return { status: 200, body: { guide: guideFor(account) } };
}

function guideFor(account: ResolvedAccount): Record<string, unknown> {
  if (account.kind === 'claude') return { kind: 'open-app', platforms: ['mac', 'windows'] };
  if (account.kind === 'wallet') return { kind: 'browser-extension', platform: 'mac' };
  if (account.kind === 'additional') {
    if (account.provider === 'cursor') {
      return {
        kind: 'open-app',
        platforms: account.entry.platform === 'ubuntu' ? [] : [account.entry.platform],
      };
    }
    if (account.provider === 'qwen') {
      return {
        kind: 'browser-extension',
        platform: account.entry.platform === 'mac' ? 'mac' : 'windows',
      };
    }
    if (isKeyProvider(account.provider)) throw new LifecycleHttpError(409, 'use_replace_key');
  }
  // Muse and Antigravity sign-ins are not served yet.
  throw new LifecycleHttpError(409, 'not_implemented');
}

/** PATCH /api/accounts/:id { label } (additional-provider accounts only) */
