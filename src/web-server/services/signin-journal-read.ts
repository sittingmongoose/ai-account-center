import { randomBytes } from 'node:crypto';
import { requestRustAgent } from '../rust-service-transport';
import { agentTransport, agentVerbs } from './agent-verbs';
import {
  JOB_ID_PATTERN,
  SIGNIN_ERROR_MESSAGES,
  type SignInJob,
  type SignInJobErrorCode,
} from './signin-job-types';
import { SignInOutputParser } from './signin-output';
import { CODEX_DEVICE_AUTH_ORIGINS } from './codex-account-lifecycle';
import { MUSE_DEVICE_AUTH_ORIGINS } from './muse-account-lifecycle';

const origins: Record<SignInJob['provider'], readonly string[]> = {
  codex: CODEX_DEVICE_AUTH_ORIGINS,
  muse: MUSE_DEVICE_AUTH_ORIGINS,
  antigravity: ['https://accounts.google.com'],
};
const record = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;

function oneOf<const T extends string>(value: unknown, ...words: T[]): value is T {
  return words.some((word) => value === word);
}

function text(value: unknown, limit: number): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= limit &&
    !/[\u0000-\u001f\u007f]/.test(value)
  );
}

function optionalText(value: unknown, limit: number): value is string | null {
  return value === null || text(value, limit);
}

function timestamp(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(value) &&
    Number.isFinite(Date.parse(value))
  );
}

function parseJournalReply(input: string, id: string): SignInJob | null | undefined {
  const payload = record(JSON.parse(input) as unknown);
  if (payload?.job === null) return null;
  const job = record(payload?.job);
  if (
    !job ||
    job.id !== id ||
    !oneOf(job.provider, 'codex', 'muse', 'antigravity') ||
    !oneOf(job.kind, 'device-code', 'supervised-cli') ||
    !oneOf(job.mode, 'add', 'signin-again') ||
    !oneOf(job.platform, 'ubuntu', 'mac') ||
    !oneOf(
      job.state,
      'starting',
      'waiting',
      'awaiting_code',
      'verifying',
      'succeeded',
      'failed',
      'expired',
      'cancelled'
    ) ||
    !optionalText(job.accountId, 128) ||
    !optionalText(job.profileName, 128) ||
    !timestamp(job.startedAt) ||
    !timestamp(job.updatedAt) ||
    !timestamp(job.expiresAt)
  )
    return undefined;

  let verification: SignInJob['verification'] = null;
  if (job.verification !== null) {
    const value = record(job.verification);
    if (
      !value ||
      !text(value.url, 8192) ||
      !optionalText(value.userCode, 13) ||
      !(value.expiresAt === null || timestamp(value.expiresAt))
    )
      return undefined;
    const input = new SignInOutputParser({
      allowedOrigins: origins[job.provider],
      expectsUserCode: job.kind === 'device-code',
      keepQuery: job.kind === 'supervised-cli',
    });
    input.push(value.url + '\n' + (value.userCode ?? '') + '\n');
    const admitted = input.verification();
    if (!admitted || admitted.url !== value.url || admitted.userCode !== value.userCode)
      return undefined;
    verification = { ...admitted, expiresAt: value.expiresAt };
  }

  let result: SignInJob['result'] = null;
  if (job.result !== null) {
    const value = record(job.result);
    if (
      !value ||
      !text(value.accountId, 128) ||
      !optionalText(value.email, 254) ||
      !optionalText(value.plan, 128)
    )
      return undefined;
    result = { accountId: value.accountId, email: value.email, plan: value.plan };
  }

  let error: SignInJob['error'] = null;
  if (job.error !== null) {
    const code = record(job.error)?.code;
    if (
      typeof code !== 'string' ||
      !Object.prototype.hasOwnProperty.call(SIGNIN_ERROR_MESSAGES, code)
    )
      return undefined;
    const admitted = code as SignInJobErrorCode;
    error = { code: admitted, message: SIGNIN_ERROR_MESSAGES[admitted] };
  }

  // Reconstruct the public DTO. The transport never gets a request body or a session secret.
  return {
    id,
    provider: job.provider,
    kind: job.kind,
    mode: job.mode,
    accountId: job.accountId,
    profileName: job.profileName,
    platform: job.platform,
    state: job.state,
    verification,
    result,
    error,
    startedAt: job.startedAt,
    updatedAt: job.updatedAt,
    expiresAt: job.expiresAt,
  };
}

/** Lane G's get action returns null for expired issued jobs, or a job DTO. */
export async function readSignInJournal(
  ccs: string,
  id: string
): Promise<SignInJob | null | undefined> {
  if (!JOB_ID_PATTERN.test(id) || agentVerbs() !== 'on' || agentTransport() !== 'on')
    return undefined;
  try {
    const reply = await requestRustAgent(ccs, 'signin-session', {
      computer: { platform: 'ubuntu', sshHost: null },
      args: { action: 'get', jobId: id, ccs },
      deadlineMs: 5000,
      timeoutMs: 7000,
      maxBytes: 32 * 1024,
      callId: randomBytes(16).toString('hex'),
    });
    return typeof reply === 'string' ? parseJournalReply(reply, id) : undefined;
  } catch {
    return undefined;
  }
}
