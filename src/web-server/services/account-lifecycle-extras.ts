import { LifecycleHttpError } from './account-lifecycle-accounts';
import {
  resolveIn,
  signInState,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import { updateAccountRegistry, type RegistryAccount } from './account-registry-v2';
import {
  entryView,
  invalid,
  jobBody,
  keyInfo,
  keys,
  label,
  secure,
} from './account-lifecycle-helpers';

/**
 * Label, Re-check, Open and the sign-in job routes (CONTRACT-registry-lifecycle
 * 6.5 and 6.6).
 */

const RECHECK_INTERVAL_MS = 10_000;
const lastRecheck = new Map<string, number>();

export async function relabel(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): Promise<LifecycleResult> {
  keys(body, ['label']);
  const name = label(body.label);
  const account = await resolveIn(env, id);
  if (account.kind !== 'additional') throw new LifecycleHttpError(409, 'not_implemented');
  const registry = await updateAccountRegistry(env.ccsDir(), (current) => ({
    ...current,
    accounts: current.accounts.map((entry) =>
      entry.id === id ? { ...entry, label: name } : entry
    ),
  })).catch(() => {
    throw new LifecycleHttpError(500, 'write_failed');
  });
  const entry = registry.accounts.find((candidate) => candidate.id === id) as RegistryAccount;
  env.onChanged();
  return {
    status: 200,
    body: {
      account: entryView(
        entry,
        await keyInfo(env, entry),
        signInState(env, entry.provider, context.secure).unavailableReason === null
      ),
    },
  };
}

/** POST /api/accounts/:id/recheck: one account now, at most once per 10 s. */
export async function recheck(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>
): Promise<LifecycleResult> {
  keys(body, []);
  const account = await resolveIn(env, id);
  if (account.kind !== 'additional') throw new LifecycleHttpError(409, 'not_implemented');
  const now = env.now();
  // One limiter per CCS scope and account.
  const key = `${env.ccsDir()}\0${id}`;
  const last = lastRecheck.get(key);
  if (last !== undefined && now - last < RECHECK_INTERVAL_MS) {
    const retry = Math.max(1, Math.ceil((RECHECK_INTERVAL_MS - (now - last)) / 1000));
    throw new LifecycleHttpError(429, 'rate_limited', { retryAfterSeconds: retry });
  }
  lastRecheck.set(key, now);
  while (lastRecheck.size > 256) {
    const oldest = lastRecheck.keys().next().value;
    if (oldest === undefined) break;
    lastRecheck.delete(oldest);
  }
  const row = await env.refreshAdditional(id);
  if (!row) throw new LifecycleHttpError(404, 'unknown_account');
  env.replaceRow(row);
  return {
    status: 200,
    body: {
      account: {
        ...row,
        switchable: false,
        lifecycle: { state: 'ready', jobId: null },
      },
    },
  };
}

/** POST /api/accounts/:id/open { platform } (Cursor on the Mac). */
export async function openApp(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>
): Promise<LifecycleResult> {
  keys(body, ['platform']);
  if (body.platform !== 'mac' && body.platform !== 'windows') throw invalid();
  const account = await resolveIn(env, id);
  if (
    account.kind !== 'additional' ||
    account.provider !== 'cursor' ||
    body.platform !== 'mac' ||
    account.entry.platform !== 'mac' ||
    account.entry.sshHost === null
  ) {
    throw new LifecycleHttpError(409, 'not_configured');
  }
  try {
    await env.openCursor(account.entry.sshHost);
  } catch {
    throw new LifecycleHttpError(502, 'host_unreachable', { host: 'mac' });
  }
  return { status: 200, body: { opened: true } };
}

/** GET /api/accounts/signin-jobs/:jobId */
export function readJob(
  env: LifecycleEnv,
  jobId: string,
  context: LifecycleContext
): LifecycleResult {
  const job = env.runner().get(jobId);
  if (!job) throw new LifecycleHttpError(404, 'unknown_job');
  return { status: 200, body: job.provider === null ? job : jobBody(job, context) };
}

export function cancelJob(
  env: LifecycleEnv,
  jobId: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): LifecycleResult {
  keys(body, []);
  const job = env.runner().cancel(jobId);
  if (!job) throw new LifecycleHttpError(404, 'unknown_job');
  return { status: 200, body: jobBody(job, context) };
}

export function submitJobCode(
  env: LifecycleEnv,
  jobId: string,
  body: Record<string, unknown>,
  context: LifecycleContext
): LifecycleResult {
  secure(context);
  keys(body, ['code']);
  if (typeof body.code !== 'string') throw invalid();
  const job = env.runner().submitCode(jobId, body.code);
  if (job === null) throw new LifecycleHttpError(404, 'unknown_job');
  if (job === 'not_expected') throw new LifecycleHttpError(409, 'code_not_expected');
  return { status: 200, body: jobBody(job, context) };
}
