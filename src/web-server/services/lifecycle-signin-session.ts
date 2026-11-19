import { randomBytes } from 'node:crypto';
import { ConfigError } from '../../errors/error-types';
import { canonicalRoot, controlLifecycleAgent, requestRustAgent } from '../rust-service-transport';
import { parseJournalReply } from './signin-journal-read';
import type { SignInCommand, SignInFlowSpec, SignInJob } from './signin-job-types';

export interface NativeSignInSession {
  start(
    id: string,
    flow: SignInFlowSpec,
    command: SignInCommand,
    progress: (job: SignInJob) => void
  ): Promise<{ job: SignInJob; installPending: boolean } | undefined>;
  submit(id: string, code: string): Promise<boolean>;
  cancel(id: string): Promise<void>;
  settle(job: SignInJob): Promise<void>;
}

const MAX_REPLY_BYTES = 1024 * 1024;
const local = { platform: 'ubuntu', sshHost: null } as const;

/** Null is an authoritative missing identity. Undefined leaves the legacy probe eligible. */
export async function readMuseLoginThroughAgent(
  root: string,
  sshHost: string,
  homeId: string | null,
  enabled: boolean
): Promise<string | null | undefined> {
  if (!enabled) return undefined;
  const reply = await requestRustAgent(
    root,
    'muse-login-identity',
    {
      computer: { platform: 'mac', sshHost },
      args: { homeId },
      deadlineMs: 58_000,
      timeoutMs: 60_000,
      maxBytes: 4096,
      callId: randomBytes(16).toString('hex'),
    },
    undefined,
    true
  );
  if (reply === undefined || reply === null) return undefined;
  const { email } = JSON.parse(reply) as { email?: unknown };
  return typeof email === 'string' ? email : null;
}

export function nativeSignInSession(
  root: string,
  enabled: boolean
): NativeSignInSession | undefined {
  if (!enabled) return undefined;
  const ccs = canonicalRoot(root);
  const active = new Map<string, AbortController>();
  const journal = (args: Record<string, unknown>) =>
    requestRustAgent(root, 'signin-session', {
      computer: local,
      args: { ...args, ccs },
      deadlineMs: 5000,
      timeoutMs: 7000,
      maxBytes: MAX_REPLY_BYTES,
      callId: randomBytes(16).toString('hex'),
    });
  return {
    async start(id, flow, command, progress) {
      const deadlineMs = flow.timeoutMs + 60_000;
      const boundJob = (input: string) => {
        const job = parseJournalReply(input, id);
        return job &&
          job.provider === flow.provider &&
          job.kind === flow.kind &&
          job.mode === flow.mode &&
          job.platform === flow.platform &&
          job.accountId === flow.accountId &&
          job.profileName === flow.profileName
          ? job
          : undefined;
      };
      let seenProgress = false;
      const controller = new AbortController();
      active.set(id, controller);
      const answer = await requestRustAgent(
        root,
        'lifecycle-signin',
        {
          computer: local,
          args: {
            provider: flow.provider,
            kind: flow.kind,
            mode: flow.mode,
            accountId: flow.accountId,
            profileName: flow.profileName,
            platform: flow.platform,
            allowedOrigins: flow.allowedOrigins,
            timeoutMs: flow.timeoutMs,
            jobId: id,
            frontDoorPid: process.pid,
            command,
          },
          deadlineMs,
          timeoutMs: deadlineMs + 5000,
          maxBytes: MAX_REPLY_BYTES,
          callId: id,
        },
        (event) => {
          seenProgress = true;
          const job = boundJob(JSON.stringify(event));
          if (job && ['waiting', 'awaiting_code'].includes(job.state)) progress(job);
        },
        true,
        controller.signal
      ).finally(() => active.delete(id));
      if (answer === null || answer === undefined) {
        if (!seenProgress) return undefined;
        throw new ConfigError('The sign-in dispatch outcome could not be confirmed.');
      }
      const job = boundJob(answer);
      const installPending = JSON.parse(answer).installPending === true;
      if (
        !job ||
        (installPending && job.state !== 'verifying') ||
        (!installPending && !['failed', 'expired', 'cancelled'].includes(job.state))
      ) {
        throw new ConfigError('Invalid sign-in dispatch reply.');
      }
      return { job, installPending };
    },
    async submit(id, code) {
      return (await controlLifecycleAgent(root, id, { input: code }))?.accepted === true;
    },
    async cancel(id) {
      const receipt = await controlLifecycleAgent(root, id, { cancel: true }).catch(
        () => undefined
      );
      if (receipt?.cancelled) return;
      // The request may still be between HTTP admission and the first agent frame.
      // Ending its stream also cancels anything whose control slot appeared late.
      active.get(id)?.abort();
      const reply = await journal({ action: 'cancel', jobId: id });
      if (typeof reply === 'string' && parseJournalReply(reply, id)?.state === 'verifying') {
        await journal({
          action: 'settle',
          jobId: id,
          state: 'cancelled',
          result: null,
          error: null,
        });
      }
    },
    async settle(job) {
      await journal({
        action: 'settle',
        jobId: job.id,
        state: job.state,
        result: job.result,
        error: job.error?.code ?? null,
      });
    },
  };
}
