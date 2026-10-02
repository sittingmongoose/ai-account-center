import { randomBytes } from 'crypto';
import { SignInOutputParser } from './signin-output';
import {
  spawnSignInProcess,
  type SignInCommand,
  type SignInProcessHandle,
  type SignInSpawner,
} from './signin-process';
import {
  SIGNIN_CODE_PATTERN,
  SIGNIN_ERROR_MESSAGES,
  SignInJobError,
  SignInJobConflict,
  type RestartedSignInJob,
  type SignInFlowSpec,
  type SignInJob,
  type SignInJobErrorCode,
  type SignInJobProvider,
  type SignInJobState,
} from './signin-job-types';

export * from './signin-job-types';

/**
 * The sign-in job runner (CONTRACT-registry-lifecycle section 6.6).
 *
 * - One running job per provider and three in all; jobs live in memory only
 *   and never resume after a restart.
 * - The CLI's output is parsed in memory for the verification URL and code and
 *   then discarded; it is never logged or persisted.
 * - A first-output timeout (30 s) or an unrecognized answer fails the job with
 *   `unexpected_output`; the overall timeout ends it as `expired`. Cancel,
 *   timeout and shutdown kill the CLI's whole process group.
 * - Once the CLI has exited 0 the job installs the login (`complete`). From
 *   then on its timers are off, and a cancel or shutdown only asks it to stop:
 *   `complete` checks that request under its own lock before it commits, and
 *   the job ends as cancelled only if nothing was installed. `cleanup` runs
 *   only after `prepare` or `complete` has settled, so it never races them.
 * - A finished job stays readable for 10 minutes.
 */
export type TimerScheduler = (callback: () => void, ms: number) => () => void;

export interface SignInJobRunnerDeps {
  spawn?: SignInSpawner;
  now?: () => number;
  setTimer?: TimerScheduler;
  firstOutputTimeoutMs?: number;
  finishedRetentionMs?: number;
  /** Every state change, for the /ws push. */
  onChange?: (job: SignInJob) => void;
  /** Once per job when it ends, for the audit line. */
  onFinish?: (job: SignInJob) => void;
}

const MAX_RUNNING = 3;
const MAX_ISSUED_IDS = 512;
const FINISHED: ReadonlySet<SignInJobState> = new Set([
  'succeeded',
  'failed',
  'expired',
  'cancelled',
]);

interface JobRecord {
  job: SignInJob;
  spec: SignInFlowSpec;
  process: SignInProcessHandle | null;
  parser: SignInOutputParser;
  timers: Array<() => void>;
  finishedAt: number | null;
  codeSent: boolean;
  /** The running `prepare` or `complete`; cleanup waits for it. */
  busy: Promise<unknown> | null;
  /** True while `complete` installs the login: the job cannot be ended from outside. */
  installing: boolean;
  /** How the job ends if `complete` stops before committing (cancel or shutdown during install). */
  stop: { state: 'cancelled' | 'failed' | 'expired'; code: SignInJobErrorCode | null } | null;
}

const defaultTimer: TimerScheduler = (callback, ms) => {
  const handle = setTimeout(callback, ms);
  handle.unref?.();
  return () => clearTimeout(handle);
};

function copy(job: SignInJob): SignInJob {
  return {
    ...job,
    verification: job.verification ? { ...job.verification } : null,
    result: job.result ? { ...job.result } : null,
    error: job.error ? { ...job.error } : null,
  };
}

export function isFinishedSignInJob(job: { state: SignInJobState }): boolean {
  return FINISHED.has(job.state);
}

export class SignInJobRunner {
  private readonly jobs = new Map<string, JobRecord>();
  private readonly issued = new Set<string>();

  constructor(private readonly deps: SignInJobRunnerDeps = {}) {}

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private timer(callback: () => void, ms: number): () => void {
    return (this.deps.setTimer ?? defaultTimer)(callback, ms);
  }

  private prune(): void {
    const retention = this.deps.finishedRetentionMs ?? 10 * 60_000;
    const now = this.now();
    for (const [id, record] of this.jobs) {
      if (record.finishedAt !== null && now - record.finishedAt >= retention) this.jobs.delete(id);
    }
  }

  private running(): JobRecord[] {
    return [...this.jobs.values()].filter((record) => record.finishedAt === null);
  }

  /** Reserve a job and start it; throws SignInJobConflict when the limits are reached. */
  start(spec: SignInFlowSpec): SignInJob {
    this.prune();
    const running = this.running();
    const sameProvider = running.find((record) => record.job.provider === spec.provider);
    if (sameProvider) throw new SignInJobConflict('job_running', sameProvider.job.id);
    if (running.length >= MAX_RUNNING) throw new SignInJobConflict('too_many_jobs', null);
    const id = `job_${randomBytes(8).toString('hex')}`;
    const now = this.now();
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
    const record: JobRecord = {
      job,
      spec,
      process: null,
      parser: new SignInOutputParser({
        allowedOrigins: spec.allowedOrigins,
        expectsUserCode: spec.kind === 'device-code',
        keepQuery: spec.kind === 'supervised-cli',
      }),
      timers: [],
      finishedAt: null,
      codeSent: false,
      busy: null,
      installing: false,
      stop: null,
    };
    this.jobs.set(id, record);
    this.issued.add(id);
    while (this.issued.size > MAX_ISSUED_IDS) {
      const oldest = this.issued.values().next().value;
      if (oldest === undefined || this.jobs.has(oldest)) break;
      this.issued.delete(oldest);
    }
    record.timers.push(
      this.timer(() => this.requestStop(record, 'expired', 'timeout'), spec.timeoutMs),
      this.timer(() => {
        if (record.job.state === 'starting') this.finish(record, 'failed', 'unexpected_output');
      }, this.deps.firstOutputTimeoutMs ?? 30_000)
    );
    this.emit(record);
    void this.launch(record);
    return copy(job);
  }

  private async launch(record: JobRecord): Promise<void> {
    let command: SignInCommand;
    const preparing = Promise.resolve().then(() => record.spec.prepare(record.job.id));
    record.busy = preparing;
    try {
      command = await preparing;
    } catch (error) {
      if (record.busy === preparing) record.busy = null;
      this.finish(record, 'failed', error instanceof SignInJobError ? error.code : 'write_failed');
      return;
    }
    if (record.busy === preparing) record.busy = null;
    if (record.finishedAt !== null) return;
    let child: SignInProcessHandle;
    try {
      child = (this.deps.spawn ?? spawnSignInProcess)(command);
    } catch {
      this.finish(record, 'failed', 'tool_missing');
      return;
    }
    record.process = child;
    child.onData((chunk) => this.output(record, chunk));
    child.onExit((code, failure) => void this.exited(record, code, failure));
  }

  private output(record: JobRecord, chunk: string): void {
    if (record.finishedAt !== null || record.job.state !== 'starting') return;
    const state = record.parser.push(chunk);
    if (state === 'rejected') {
      this.finish(record, 'failed', 'unexpected_output');
    } else if (state === 'ready') {
      this.reachedVerification(record);
    }
  }

  private reachedVerification(record: JobRecord): void {
    const verification = record.parser.verification();
    if (!verification) return;
    record.job.verification = { ...verification, expiresAt: record.job.expiresAt };
    this.update(record, record.spec.kind === 'supervised-cli' ? 'awaiting_code' : 'waiting');
  }

  private async exited(
    record: JobRecord,
    code: number | null,
    failure: 'missing' | 'failed' | null
  ): Promise<void> {
    if (record.finishedAt !== null) return;
    if (failure) {
      this.finish(record, 'failed', failure === 'missing' ? 'tool_missing' : 'write_failed');
      return;
    }
    if (record.job.state === 'starting') {
      if (record.parser.end() === 'ready') this.reachedVerification(record);
      else {
        this.finish(record, 'failed', code === 127 ? 'tool_missing' : 'unexpected_output');
        return;
      }
    }
    if (code !== 0) {
      this.finish(record, 'failed', 'provider_denied');
      return;
    }
    // Install phase: no timer may end the job while the login is being installed.
    for (const cancel of record.timers.splice(0)) cancel();
    record.installing = true;
    this.update(record, 'verifying');
    const completing = Promise.resolve().then(() =>
      record.spec.complete(record.job.id, { stopped: () => record.stop !== null })
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
    if (outcome.ok) {
      // Committed: a stop that arrived after the commit point does not undo it.
      record.job.result = { ...outcome.result };
      if (!record.job.accountId) record.job.accountId = outcome.result.accountId;
      this.finish(record, 'succeeded', null);
    } else if (record.stop) {
      this.finish(record, record.stop.state, record.stop.code);
    } else {
      const error = outcome.error;
      this.finish(record, 'failed', error instanceof SignInJobError ? error.code : 'write_failed');
    }
  }

  /** End the job now, or, while it installs, ask `complete` to stop before it commits. */
  private requestStop(
    record: JobRecord,
    state: 'cancelled' | 'failed' | 'expired',
    code: SignInJobErrorCode | null
  ): void {
    if (record.finishedAt !== null) return;
    if (record.installing) {
      record.stop ??= { state, code };
      return;
    }
    this.finish(record, state, code);
  }

  private update(record: JobRecord, state: SignInJobState): void {
    record.job.state = state;
    record.job.updatedAt = new Date(this.now()).toISOString();
    this.emit(record);
  }

  private finish(
    record: JobRecord,
    state: 'succeeded' | 'failed' | 'expired' | 'cancelled',
    code: SignInJobErrorCode | null
  ): void {
    if (record.finishedAt !== null) return;
    record.finishedAt = this.now();
    for (const cancel of record.timers.splice(0)) cancel();
    record.process?.kill();
    // The code shown to the user is used up or void once the job ends.
    record.job.verification = null;
    record.job.error = code ? { code, message: SIGNIN_ERROR_MESSAGES[code] } : null;
    this.update(record, state);
    try {
      this.deps.onFinish?.(copy(record.job));
    } catch {
      /* Audit is best effort. */
    }
    // Never while prepare or complete still works in the staging folder.
    const settled = (record.busy ?? Promise.resolve()).then(
      () => undefined,
      () => undefined
    );
    void settled.then(() => record.spec.cleanup(record.job.id)).catch(() => undefined);
  }

  private emit(record: JobRecord): void {
    try {
      this.deps.onChange?.(copy(record.job));
    } catch {
      /* The push is a hint; clients also poll. */
    }
  }

  /** The job, a restarted placeholder for an id this process never issued, or null when expired. */
  get(id: string): SignInJob | RestartedSignInJob | null {
    this.prune();
    const record = this.jobs.get(id);
    if (record) return copy(record.job);
    if (this.issued.has(id)) return null;
    return {
      id,
      provider: null,
      kind: null,
      mode: null,
      accountId: null,
      profileName: null,
      platform: null,
      state: 'failed',
      verification: null,
      result: null,
      error: { code: 'server_restarted', message: SIGNIN_ERROR_MESSAGES.server_restarted },
      startedAt: null,
      updatedAt: new Date(this.now()).toISOString(),
      expiresAt: null,
    };
  }

  /**
   * Cancel a job. While it installs the login the job keeps `verifying` and
   * ends as cancelled only if `complete` stops before committing.
   */
  cancel(id: string): SignInJob | null {
    const record = this.jobs.get(id);
    if (!record) return null;
    this.requestStop(record, 'cancelled', null);
    return copy(record.job);
  }

  /** Supervised flows only: write the authorization code to the CLI once. */
  submitCode(id: string, code: string): SignInJob | 'not_expected' | null {
    const record = this.jobs.get(id);
    if (!record) return null;
    if (
      record.spec.kind !== 'supervised-cli' ||
      record.job.state !== 'awaiting_code' ||
      record.codeSent ||
      !SIGNIN_CODE_PATTERN.test(code)
    ) {
      return 'not_expected';
    }
    record.codeSent = true;
    if (!record.process?.write(`${code}\n`)) {
      this.finish(record, 'failed', 'write_failed');
      return copy(record.job);
    }
    this.update(record, 'verifying');
    return copy(record.job);
  }

  /** Jobs that have not finished. */
  list(): SignInJob[] {
    this.prune();
    return this.running().map((record) => copy(record.job));
  }

  runningForAccount(accountId: string): SignInJob | null {
    const record = this.running().find((candidate) => candidate.job.accountId === accountId);
    return record ? copy(record.job) : null;
  }

  runningForProvider(provider: SignInJobProvider): SignInJob | null {
    const record = this.running().find((candidate) => candidate.job.provider === provider);
    return record ? copy(record.job) : null;
  }

  /** Lifecycle overlay for dashboard rows of accounts with a running sign-in. */
  accountStates(): Map<string, { state: 'signing_in' | 'verifying'; jobId: string }> {
    const states = new Map<string, { state: 'signing_in' | 'verifying'; jobId: string }>();
    for (const record of this.running()) {
      if (!record.job.accountId) continue;
      states.set(record.job.accountId, {
        state: record.job.state === 'verifying' ? 'verifying' : 'signing_in',
        jobId: record.job.id,
      });
    }
    return states;
  }

  /** Server shutdown: stop every CLI; nothing resumes after a restart. */
  shutdown(): void {
    for (const record of this.running()) this.requestStop(record, 'failed', 'server_restarted');
  }
}
