import { runLifecycleEffects } from './lifecycle-effect-pipe';
import type { DashboardAccount } from './account-dashboard-types';
import { lifecyclePlan } from '../../shared/private-file-lease';
import { LifecycleHttpError } from './account-lifecycle-accounts';
import { ClaudeLifecycleError, isClaudeEmail } from './claude-account-lifecycle';
import {
  resolveIn,
  signInState,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import { registrySource, updateAccountRegistry } from './account-registry-v2';
import { editRegistry } from './account-lifecycle-transitions';
import { SIGNIN_CODE_PATTERN } from './signin-jobs';
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
  const root = env.ccsDir();
  const registry = await updateAccountRegistry(root, (current) => {
    const edit = editRegistry(root, current, { kind: 'label', id, label: name });
    // Removed between the lookup and the write: nothing to label.
    if ('refusal' in edit) throw new LifecycleHttpError(404, 'unknown_account');
    return edit.registry;
  }).catch((error) => {
    throw error instanceof LifecycleHttpError ? error : new LifecycleHttpError(500, 'write_failed');
  });
  const entry = registry.accounts.find((candidate) => candidate.id === id);
  if (!entry) throw new LifecycleHttpError(404, 'unknown_account');
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

function checkRecheckLimit(env: LifecycleEnv, id: string): void {
  const now = env.now();
  // One limiter per CCS scope and account.
  const root = env.ccsDir();
  const key = `${root}\0${id}`;
  const last = lastRecheck.get(key);
  const decision = lifecyclePlan(root, {
    operation: 'recheck',
    now,
    last: last ?? null,
    interval: RECHECK_INTERVAL_MS,
  });
  const retry =
    decision === undefined
      ? last !== undefined && now - last < RECHECK_INTERVAL_MS
        ? Math.max(1, Math.ceil((RECHECK_INTERVAL_MS - (now - last)) / 1000))
        : null
      : decision;
  if (typeof retry === 'number' && Number.isSafeInteger(retry) && retry > 0)
    throw new LifecycleHttpError(429, 'rate_limited', { retryAfterSeconds: retry });
  lastRecheck.set(key, now);
  while (lastRecheck.size > 256) {
    const oldest = lastRecheck.keys().next().value;
    if (oldest === undefined) break;
    lastRecheck.delete(oldest);
  }
}

/** POST /api/accounts/:id/recheck: one account now, at most once per 10 s. */
export async function recheck(
  env: LifecycleEnv,
  id: string,
  body: Record<string, unknown>,
  native?: (id: string) => Promise<{ status: 200 | 404; body: unknown } | null>
): Promise<LifecycleResult> {
  const account = await resolveIn(env, id);
  if (account.kind === 'claude') return recheckClaude(env, account, body);
  if (account.kind !== 'additional') throw new LifecycleHttpError(409, 'not_implemented');
  keys(body, []);
  checkRecheckLimit(env, id);
  const reply = await native?.(account.id);
  if (reply?.status === 404) throw new LifecycleHttpError(404, 'unknown_account');
  if (reply) return { status: 200, body: reply.body as Record<string, unknown> };
  const { entry } = account;
  const reviewed = registrySource(entry);
  if (account.mode === 'v1') delete reviewed.account;
  const binding = JSON.stringify([entry.provider, entry.platform, entry.sshHost, entry.credential]);
  const reading: { row: DashboardAccount | null } = { row: null };
  const completed = await runLifecycleEffects(
    env.ccsDir(),
    ['--additional-recheck'],
    ['collect', 'publish-if-current'] as const,
    async (effect) => {
      if (effect === 'collect') {
        const row = await env.refreshAdditional(id, reviewed);
        reading.row = row;
        return row?.id === id && row.provider === entry.provider && row.platform === entry.platform;
      }
      const { row } = reading;
      if (!row) return false;
      const current = await resolveIn(env, id);
      if (current.kind !== 'additional') return false;
      const source = current.entry;
      if (
        JSON.stringify([source.provider, source.platform, source.sshHost, source.credential]) !==
        binding
      )
        return false;
      env.replaceRow(row);
      return true;
    },
    async (perform) =>
      (await perform('collect')) === true && (await perform('publish-if-current')) === true
  );
  const refreshed = reading.row;
  if (!completed || !refreshed) throw new LifecycleHttpError(404, 'unknown_account');
  return {
    status: 200,
    body: {
      account: {
        ...refreshed,
        lifecycle: { state: 'ready', jobId: null },
      },
    },
  };
}

/**
 * Re-check a Claude profile after the user signs in: pending ids check the
 * sign-in marker on the platform the app was opened on, and promote on a
 * session. Listed ids are already signed in, so they answer found.
 */
async function recheckClaude(
  env: LifecycleEnv,
  account: { id: string; profile: { id: string; label: string } },
  body: Record<string, unknown>
): Promise<LifecycleResult> {
  keys(body, [], ['email', 'platform']);
  if (body.platform !== 'mac' && body.platform !== 'windows') throw invalid();
  const email = body.email === undefined ? null : body.email;
  if (email !== null && !isClaudeEmail(email)) throw invalid();
  checkRecheckLimit(env, account.id);
  const profileId = account.profile.id;
  let result;
  try {
    result = await env.claude().checkPendingSignIn(profileId, body.platform, email);
  } catch (error) {
    if (!(error instanceof ClaudeLifecycleError)) {
      throw new LifecycleHttpError(500, 'write_failed');
    }
    const status =
      error.code === 'host_unreachable' ? 502 : error.code === 'write_failed' ? 500 : 409;
    throw new LifecycleHttpError(status, error.code, error.host ? { host: error.host } : {});
  }
  if (!result) throw new LifecycleHttpError(404, 'unknown_account');
  if (result.promoted) env.onChanged();
  return {
    status: 200,
    body: {
      account: {
        id: account.id,
        provider: 'claude',
        label: account.profile.label,
        email: result.email,
        status: result.promoted ? 'ok' : 'needs_sign_in',
        lifecycle: {
          state: result.promoted ? 'ready' : 'pending_sign_in',
          jobId: null,
        },
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
  const additional = account.kind === 'additional';
  const platform = additional ? account.entry.platform : null;
  const host = additional ? account.entry.sshHost : null;
  const reply = lifecyclePlan(env.ccsDir(), {
    operation: 'open-cursor',
    additional,
    provider: account.provider,
    requestPlatform: body.platform,
    accountPlatform: platform,
    hostConfigured: host !== null,
  });
  const allowed =
    reply === undefined
      ? additional && account.provider === 'cursor' && body.platform === 'mac' && platform === 'mac'
      : reply === true;
  if (!allowed || host === null) {
    throw new LifecycleHttpError(409, 'not_configured');
  }
  try {
    await env.openCursor(host);
  } catch {
    throw new LifecycleHttpError(502, 'host_unreachable', { host: 'mac' });
  }
  return { status: 200, body: { opened: true } };
}

/** GET /api/accounts/signin-jobs/:jobId */
export async function readJob(
  env: LifecycleEnv,
  jobId: string,
  context: LifecycleContext
): Promise<LifecycleResult> {
  const current = env.runner().get(jobId);
  const recovered = current?.provider === null ? await env.readSignInJournal?.(jobId) : undefined;
  const job = recovered === undefined ? current : recovered;
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
  // A malformed code is a bad body (400), not a job that is not waiting (409).
  if (typeof body.code !== 'string' || !SIGNIN_CODE_PATTERN.test(body.code)) throw invalid();
  const job = env.runner().submitCode(jobId, body.code);
  if (job === null) throw new LifecycleHttpError(404, 'unknown_job');
  if (job === 'not_expected') throw new LifecycleHttpError(409, 'code_not_expected');
  return { status: 200, body: jobBody(job, context) };
}
