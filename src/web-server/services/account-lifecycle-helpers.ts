import type { DashboardAccount } from './account-dashboard-types';
import { isKeyProvider, type StoredKeyInfo } from './account-key-store';
import {
  accountView,
  credentialView,
  LifecycleHttpError,
  type AccountActions,
  type RegistryAccountView,
} from './account-lifecycle-accounts';
import type { LifecycleContext, LifecycleEnv } from './account-lifecycle-env';
import { isAccountLabel, type RegistryAccount } from './account-registry-v2';
import { redactSignInJob } from './account-lifecycle-runtime';
import { SignInJobConflict, type SignInFlowSpec, type SignInJob } from './signin-jobs';

/** Shared checks and views of the lifecycle actions. */
export const CODEX_TERMINAL = {
  kind: 'terminal' as const,
  host: 'ubuntu' as const,
  command: 'ai-account-center codex-auth login <profile-name>',
};

export function invalid(): LifecycleHttpError {
  return new LifecycleHttpError(400, 'invalid_body');
}

export function keys(
  body: Record<string, unknown>,
  required: string[],
  optional: string[] = []
): void {
  const names = Object.keys(body);
  if (
    !required.every((key) => Object.prototype.hasOwnProperty.call(body, key)) ||
    !names.every((key) => required.includes(key) || optional.includes(key))
  ) {
    throw invalid();
  }
}

export function label(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  if (!isAccountLabel(value)) throw invalid();
  return value;
}

export function secure(
  context: LifecycleContext,
  fallback: typeof CODEX_TERMINAL | null = null
): void {
  if (!context.secure) {
    throw new LifecycleHttpError(403, 'secure_transport_required', fallback ? { fallback } : {});
  }
}

export function jobBody(job: SignInJob, context: LifecycleContext): SignInJob {
  return context.secure ? job : redactSignInJob(job);
}

export function startJob(env: LifecycleEnv, spec: SignInFlowSpec): SignInJob {
  try {
    return env.runner().start(spec);
  } catch (error) {
    if (error instanceof SignInJobConflict) {
      throw new LifecycleHttpError(409, error.code, error.jobId ? { jobId: error.jobId } : {});
    }
    throw new LifecycleHttpError(500, 'write_failed');
  }
}

export function keyActions(entry: RegistryAccount, replaceKey: boolean): AccountActions {
  return {
    signInAgain: entry.provider === 'cursor' || entry.provider === 'qwen',
    replaceKey: replaceKey && entry.credential.kind === 'aac-key',
    remove: true,
    open:
      entry.provider === 'cursor' && entry.platform === 'mac' && entry.sshHost !== null
        ? ['mac']
        : [],
    recheck: true,
  };
}

export function entryView(
  entry: RegistryAccount,
  key: StoredKeyInfo | null,
  replaceKey: boolean,
  row: Partial<DashboardAccount> = {}
): RegistryAccountView {
  return accountView(
    {
      id: entry.id,
      provider: entry.provider,
      label: entry.label ?? row.label ?? entry.provider,
      email: row.email ?? null,
      platform: entry.platform,
      hidden: row.hidden,
      lifecycle: row.lifecycle,
    },
    credentialView(entry, key),
    keyActions(entry, replaceKey),
    null
  );
}

export async function keyInfo(
  env: LifecycleEnv,
  entry: RegistryAccount
): Promise<StoredKeyInfo | null> {
  if (entry.credential.kind !== 'aac-key' || !isKeyProvider(entry.provider)) return null;
  const store = env.keyStore({ platform: entry.platform, sshHost: entry.sshHost });
  return store ? store.info(entry.provider, entry.credential.keyId) : null;
}

/** GET /api/accounts/registry */
