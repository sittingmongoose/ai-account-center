/**
 * M4c: the agent-backed sign-in job runner. With `agentVerbs: on` and `agentTransport: on`
 * and a live service connection, the interactive CLI session runs in the `signin-session`
 * agent verb on the target computer; this runner keeps the synchronous route contract by
 * mirroring the job states the verb streams as progress events.
 *
 * The install commit stays here until M5: on the verb's `installPending` terminal the
 * unchanged flow `complete` runs under its identity guards and registry lock, then the
 * journal is settled. Every unavailable, unproven or failed agent path keeps the labelled
 * TypeScript runner (the base class) as the fallback; an executing session's failure
 * remains a failure.
 */
import { randomBytes } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { createLogger } from '../../services/logging';
import { controlRustAgent, requestRustAgent, rustAgentConnected } from '../rust-service-transport';
import { agentTransport, agentVerbs } from './agent-verbs';
import {
  SIGNIN_CODE_PATTERN,
  SIGNIN_ERROR_MESSAGES,
  SignInJobConflict,
  SignInJobError,
  SignInJobRunner,
  type RestartedSignInJob,
  type SignInFlowSpec,
  type SignInJob,
  type SignInJobErrorCode,
  type SignInJobRunnerDeps,
  type SignInJobState,
} from './signin-jobs';

const logger = createLogger('signin-session-agent');

const MAX_RUNNING = 3;
const FINISHED_RETENTION_MS = 10 * 60_000;
/** The install and cleanup margin past the CLI's own window (the verb's deadline cap). */
const INSTALL_MARGIN_MS = 60_000;
const MAX_REPLY_BYTES = 1024 * 1024;

const FINISHED: Readonly<Record<'succeeded' | 'failed' | 'expired' | 'cancelled', true>> = {
  succeeded: true,
  failed: true,
  expired: true,
  cancelled: true,
};
const RUNNING_STATES: readonly SignInJobState[] = [
  'starting',
  'waiting',
  'awaiting_code',
  'verifying',
];

interface AgentJobRecord {
  job: SignInJob;
  /** Null for a journaled job hydrated after a restart: its session is not ours to drive. */
  spec: SignInFlowSpec | null;
  callId: string;
  codeSent: boolean;
  installing: boolean;
  stop: { state: 'cancelled' | 'failed' | 'expired'; code: SignInJobErrorCode | null } | null;
  busy: Promise<unknown> | null;
  finishedAt: number | null;
}

function copy(job: SignInJob): SignInJob {
  return {
    ...job,
    verification: job.verification ? { ...job.verification } : null,
    result: job.result ? { ...job.result } : null,
    error: job.error ? { ...job.error } : null,
  };
}

/** The switches plus a live connection; the flow's platform must be one the verb admits. */
export function signInSessionAgentEnabled(platform: string): boolean {
  return (
    agentVerbs() === 'on' &&
    agentTransport() === 'on' &&
    (platform === 'ubuntu' || platform === 'mac') &&
    rustAgentConnected(getCcsDir())
  );
}

export class AgentSignInJobRunner extends SignInJobRunner {
  private readonly agentJobs = new Map<string, AgentJobRecord>();
  private readonly agentDeps: SignInJobRunnerDeps;

  constructor(deps: SignInJobRunnerDeps = {}) {
    super(deps);
    this.agentDeps = deps;
    void this.hydrate();
  }

  private nowMs(): number {
    return (this.agentDeps.now ?? Date.now)();
  }

  private emitJob(job: SignInJob): void {
    try {
      this.agentDeps.onChange?.(copy(job));
    } catch {
      /* The push is a hint; clients also poll. */
    }
  }

  private auditJob(job: SignInJob): void {
    try {
      this.agentDeps.onFinish?.(copy(job));
    } catch {
      /* Audit is best effort. */
    }
  }

  private pruneAgent(): void {
    const now = this.nowMs();
    for (const [id, record] of this.agentJobs) {
      if (record.finishedAt !== null && now - record.finishedAt >= FINISHED_RETENTION_MS) {
        this.agentJobs.delete(id);
      }
    }
  }

  private runningAgent(): AgentJobRecord[] {
    return [...this.agentJobs.values()].filter((record) => record.finishedAt === null);
  }

  /** After a restart the journal still names the jobs: load them into the mirror. */
  private async hydrate(): Promise<void> {
    try {
      if (!signInSessionAgentEnabled('ubuntu')) return;
      const reply = await requestRustAgent(getCcsDir(), 'signin-session', {
        computer: { platform: 'ubuntu', sshHost: null },
        args: { action: 'list', ccs: getCcsDir() },
        deadlineMs: 5_000,
        timeoutMs: 7_000,
        maxBytes: MAX_REPLY_BYTES,
        callId: randomBytes(16).toString('hex'),
      });
      if (typeof reply !== 'string') return;
      const value: unknown = JSON.parse(reply);
      if (!value || typeof value !== 'object' || !('jobs' in value)) return;
      const jobs = value.jobs;
      if (!Array.isArray(jobs)) return;
      for (const item of jobs) {
        if (!item || typeof item !== 'object') continue;
        // The journal's DTO; the id and state word are checked before it is mirrored.
        const job = item as SignInJob;
        if (typeof job.id !== 'string' || this.agentJobs.has(job.id)) continue;
        if (!(job.state in FINISHED) && !RUNNING_STATES.includes(job.state)) continue;
        this.agentJobs.set(job.id, {
          job,
          spec: null,
          callId: '',
          codeSent: false,
          installing: false,
          stop: null,
          busy: null,
          finishedAt: job.state in FINISHED ? this.nowMs() : null,
        });
      }
    } catch {
      /* Hydration is a hint; the journal stays authoritative. */
    }
  }

  override start(spec: SignInFlowSpec): SignInJob {
    const running = this.list();
    const sameProvider = running.find((job) => job.provider === spec.provider);
    if (sameProvider) throw new SignInJobConflict('job_running', sameProvider.id);
    if (running.length >= MAX_RUNNING) throw new SignInJobConflict('too_many_jobs', null);
    if (this.agentDeps.nativeSession?.(spec) || !signInSessionAgentEnabled(spec.platform)) {
      return super.start(spec);
    }
    const id = `job_${randomBytes(8).toString('hex')}`;
    const now = this.nowMs();
    const job: SignInJob = {
      id,
      provider: spec.provider,
      kind: spec.kind,
      mode: spec.mode,
      accountId: spec.accountId,
      profileName: spec.profileName,
      platform: spec.platform,
      state: 'starting',
      verification: null,
      result: null,
      error: null,
      startedAt: new Date(now).toISOString(),
      updatedAt: new Date(now).toISOString(),
      expiresAt: new Date(now + spec.timeoutMs).toISOString(),
    };
    const record: AgentJobRecord = {
      job,
      spec,
      callId: randomBytes(16).toString('hex'),
      codeSent: false,
      installing: false,
      stop: null,
      busy: null,
      finishedAt: null,
    };
    this.agentJobs.set(id, record);
    this.emitJob(job);
    void this.drive(record);
    return copy(job);
  }

  private async drive(record: AgentJobRecord): Promise<void> {
    const spec = record.spec;
    if (!spec) return;
    let command;
    const preparing = Promise.resolve().then(() => spec.prepare(record.job.id));
    record.busy = preparing;
    try {
      command = await preparing;
    } catch (error) {
      if (record.busy === preparing) record.busy = null;
      this.finishAgent(
        record,
        'failed',
        error instanceof SignInJobError ? error.code : 'write_failed'
      );
      return;
    }
    if (record.busy === preparing) record.busy = null;
    if (record.finishedAt !== null) return;
    const reply = await requestRustAgent(
      getCcsDir(),
      'signin-session',
      {
        computer: { platform: spec.platform, sshHost: null },
        args: {
          action: 'start',
          jobId: record.job.id,
          frontDoorPid: process.pid,
          ccs: getCcsDir(),
          provider: spec.provider,
          kind: spec.kind,
          mode: spec.mode,
          accountId: spec.accountId,
          profileName: spec.profileName,
          platform: spec.platform,
          allowedOrigins: spec.allowedOrigins,
          timeoutMs: spec.timeoutMs,
          command: {
            file: command.file,
            args: command.args,
            env: command.env,
            pty: command.pty,
          },
        },
        deadlineMs: spec.timeoutMs + INSTALL_MARGIN_MS,
        timeoutMs: spec.timeoutMs + INSTALL_MARGIN_MS + 5_000,
        maxBytes: MAX_REPLY_BYTES,
        callId: record.callId,
      },
      (data) => this.progress(record, data)
    );
    if (record.finishedAt !== null) return;
    if (typeof reply !== 'string') {
      // The service disappeared or the transport failed mid-session: an honest stop.
      logger.info('signin.session.fallback', 'fallback: typescript runner', {
        provider: spec.provider,
        reason: reply === undefined ? 'unavailable' : 'transport',
      });
      this.finishAgent(record, 'failed', 'server_restarted');
      return;
    }
    let value: { job?: SignInJob; installPending?: boolean; conflict?: { code: string } };
    try {
      value = JSON.parse(reply);
    } catch {
      this.finishAgent(record, 'failed', 'server_restarted');
      return;
    }
    if (value.conflict) {
      // The journal's limits are the backstop for a race the mirror did not see.
      this.finishAgent(record, 'failed', 'write_failed');
      return;
    }
    if (!value.job) {
      this.finishAgent(record, 'failed', 'server_restarted');
      return;
    }
    if (!value.installPending) {
      const state = value.job.state;
      if (state !== 'failed' && state !== 'expired' && state !== 'cancelled') {
        this.finishAgent(record, 'failed', 'server_restarted');
        return;
      }
      record.job = value.job;
      this.finishAgent(record, state, value.job.error?.code ?? null);
      return;
    }
    await this.installAgent(record, spec);
  }

  private progress(record: AgentJobRecord, data: unknown): void {
    if (record.finishedAt !== null) return;
    if (!data || typeof data !== 'object' || !('job' in data)) return;
    const candidate = data.job;
    if (!candidate || typeof candidate !== 'object') return;
    // The private socket's DTO; the id and state word are checked before it is mirrored.
    const job = candidate as SignInJob;
    if (job.id !== record.job.id) return;
    if (!(job.state in FINISHED) && !RUNNING_STATES.includes(job.state)) return;
    record.job = job;
    this.emitJob(job);
  }

  /** The unchanged install commit: `complete` under its guards, then settle the journal. */
  private async installAgent(record: AgentJobRecord, spec: SignInFlowSpec): Promise<void> {
    record.installing = true;
    record.job = {
      ...record.job,
      state: 'verifying',
      updatedAt: new Date(this.nowMs()).toISOString(),
    };
    this.emitJob(record.job);
    const completing = Promise.resolve().then(() =>
      spec.complete(record.job.id, { stopped: () => record.stop !== null })
    );
    record.busy = completing;
    let outcome:
      | { ok: true; result: { accountId: string; email: string | null; plan: string | null } }
      | { ok: false; error: unknown };
    try {
      outcome = { ok: true, result: await completing };
    } catch (error) {
      outcome = { ok: false, error };
    }
    record.busy = null;
    record.installing = false;
    if (record.finishedAt !== null) return;
    let state: 'succeeded' | 'failed' | 'expired' | 'cancelled';
    let code: SignInJobErrorCode | null;
    if (outcome.ok) {
      record.job.result = { ...outcome.result };
      if (!record.job.accountId) record.job.accountId = outcome.result.accountId;
      state = 'succeeded';
      code = null;
    } else if (record.stop) {
      state = record.stop.state;
      code = record.stop.code;
    } else {
      state = 'failed';
      code = outcome.error instanceof SignInJobError ? outcome.error.code : 'write_failed';
    }
    this.finishAgent(record, state, code);
    this.settle(record, state, code);
  }

  private settle(
    record: AgentJobRecord,
    state: SignInJobState,
    code: SignInJobErrorCode | null
  ): void {
    void requestRustAgent(getCcsDir(), 'signin-session', {
      computer: { platform: record.job.platform, sshHost: null },
      args: {
        action: 'settle',
        jobId: record.job.id,
        ccs: getCcsDir(),
        state,
        result: record.job.result,
        error: code,
      },
      deadlineMs: 5_000,
      timeoutMs: 7_000,
      maxBytes: MAX_REPLY_BYTES,
      callId: randomBytes(16).toString('hex'),
    }).catch(() => undefined);
  }

  private finishAgent(
    record: AgentJobRecord,
    state: 'succeeded' | 'failed' | 'expired' | 'cancelled',
    code: SignInJobErrorCode | null
  ): void {
    if (record.finishedAt !== null) return;
    record.finishedAt = this.nowMs();
    record.job.verification = null;
    record.job.error = code ? { code, message: SIGNIN_ERROR_MESSAGES[code] } : null;
    record.job.state = state;
    record.job.updatedAt = new Date(this.nowMs()).toISOString();
    this.emitJob(record.job);
    this.auditJob(record.job);
    const spec = record.spec;
    if (spec) {
      const settled = (record.busy ?? Promise.resolve()).then(
        () => undefined,
        () => undefined
      );
      void settled.then(() => spec.cleanup(record.job.id)).catch(() => undefined);
    }
  }

  override get(id: string): SignInJob | RestartedSignInJob | null {
    this.pruneAgent();
    const record = this.agentJobs.get(id);
    if (record) return copy(record.job);
    return super.get(id);
  }

  override cancel(id: string): SignInJob | null {
    const record = this.agentJobs.get(id);
    if (!record) return super.cancel(id);
    if (record.finishedAt === null) {
      if (record.installing) {
        record.stop ??= { state: 'cancelled', code: null };
      } else {
        this.finishAgent(record, 'cancelled', null);
        this.stopRemote(record);
      }
    }
    return copy(record.job);
  }

  /** The control frame first; the journal's second-call cancel is the backstop. */
  private stopRemote(record: AgentJobRecord): void {
    if (record.callId) {
      void controlRustAgent(getCcsDir(), record.callId, { cancel: true })
        .then((answer) => {
          if (answer && answer.cancelled === true) return;
          this.cancelViaJournal(record);
        })
        .catch(() => this.cancelViaJournal(record));
    } else {
      this.cancelViaJournal(record);
    }
  }

  private cancelViaJournal(record: AgentJobRecord): void {
    void requestRustAgent(getCcsDir(), 'signin-session', {
      computer: { platform: record.job.platform, sshHost: null },
      args: { action: 'cancel', jobId: record.job.id, ccs: getCcsDir() },
      deadlineMs: 5_000,
      timeoutMs: 7_000,
      maxBytes: MAX_REPLY_BYTES,
      callId: randomBytes(16).toString('hex'),
    }).catch(() => undefined);
  }

  override submitCode(id: string, code: string): SignInJob | 'not_expected' | null {
    const record = this.agentJobs.get(id);
    if (!record) return super.submitCode(id, code);
    if (
      !record.spec ||
      record.spec.kind !== 'supervised-cli' ||
      record.job.state !== 'awaiting_code' ||
      record.codeSent ||
      !SIGNIN_CODE_PATTERN.test(code)
    ) {
      return 'not_expected';
    }
    record.codeSent = true;
    record.job = {
      ...record.job,
      state: 'verifying',
      updatedAt: new Date(this.nowMs()).toISOString(),
    };
    this.emitJob(record.job);
    void controlRustAgent(getCcsDir(), record.callId, { input: code })
      .then((answer) => {
        if (record.finishedAt !== null) return;
        if (!answer || answer.accepted !== true) {
          this.finishAgent(record, 'failed', 'write_failed');
        }
      })
      .catch(() => {
        if (record.finishedAt === null) this.finishAgent(record, 'failed', 'write_failed');
      });
    return copy(record.job);
  }

  override list(): SignInJob[] {
    this.pruneAgent();
    return [...this.runningAgent().map((record) => copy(record.job)), ...super.list()];
  }

  override runningForAccount(accountId: string): SignInJob | null {
    const record = this.runningAgent().find((candidate) => candidate.job.accountId === accountId);
    return record ? copy(record.job) : super.runningForAccount(accountId);
  }

  override runningForProvider(provider: SignInJob['provider']): SignInJob | null {
    const record = this.runningAgent().find((candidate) => candidate.job.provider === provider);
    return record ? copy(record.job) : super.runningForProvider(provider);
  }

  override accountStates(): Map<string, { state: 'signing_in' | 'verifying'; jobId: string }> {
    const states = super.accountStates();
    for (const record of this.runningAgent()) {
      if (!record.job.accountId) continue;
      states.set(record.job.accountId, {
        state: record.job.state === 'verifying' ? 'verifying' : 'signing_in',
        jobId: record.job.id,
      });
    }
    return states;
  }

  override shutdown(): void {
    for (const record of this.runningAgent()) {
      if (record.installing) {
        record.stop ??= { state: 'failed', code: 'server_restarted' };
      } else {
        this.finishAgent(record, 'failed', 'server_restarted');
        this.stopRemote(record);
      }
    }
    super.shutdown();
  }
}
