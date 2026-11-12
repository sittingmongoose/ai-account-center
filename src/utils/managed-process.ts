import { fork, type ChildProcess } from 'child_process';
import { createHash } from 'crypto';
import path from 'path';
import { NetworkError } from '../errors/error-types';

export interface ManagedCommandOptions {
  timeoutMs: number;
  maxBytes: number;
  graceMs?: number;
  input?: string;
  env?: NodeJS.ProcessEnv;
  cwd?: string;
  /** Absolute caller deadline, including source loading, startup and IPC queueing. */
  deadlineAt?: number;
  /** Only for read-only commands: identical concurrent requests share one child. */
  coalesce?: boolean;
}

export interface ManagedCommandResult {
  code: number | null;
  stdout: string;
  timedOut: boolean;
  missing: boolean;
  failed: boolean;
}

const failed = (): ManagedCommandResult => ({
  code: null,
  stdout: '',
  timedOut: false,
  missing: false,
  failed: true,
});
const pending = new Map<
  number,
  {
    resolve: (result: ManagedCommandResult) => void;
    timer: ReturnType<typeof setTimeout>;
    pid?: number;
  }
>();
interface CommandWaiter {
  resolve: (result: ManagedCommandResult) => void;
  timer: ReturnType<typeof setTimeout>;
}
interface CommandJob {
  hardDeadlineAt: number;
  deadlineAt: number;
  waiters: Set<CommandWaiter>;
  abandoned: boolean;
  child?: ChildProcess;
  id?: number;
}
const identical = new Map<string, CommandJob>();
const retired = new Map<ChildProcess, Promise<void>>();
let broker: ChildProcess | undefined;
let starting: Promise<ChildProcess> | undefined;
let sequence = 0;
let launches = 0;
let stopping = false;
let stoppingWork: Promise<void> | undefined;
let outstanding = 0;
let inputBytes = 0;
let idleTimer: ReturnType<typeof setTimeout> | undefined;
const script = path.resolve(__dirname, '../../scripts/process-broker.cjs');

function idle(child: ChildProcess): void {
  child.unref();
  child.channel?.unref?.();
  // Bun 1.4 does not expose channel.unref. Retire an idle broker there so standalone
  // transports/tests can exit; active polling keeps the same small command parent.
  if (typeof child.channel?.unref !== 'function') {
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      if (broker === child && pending.size === 0) void stopProcessBroker();
    }, 5000);
    idleTimer.unref();
  }
}

function killOwned(pid: number | undefined): void {
  if (pid === undefined) return;
  try {
    process.kill(process.platform === 'win32' ? pid : -pid, 'SIGKILL');
  } catch {
    /* The owned command may already have exited. */
  }
}

function settle(id: number, result: ManagedCommandResult): void {
  const request = pending.get(id);
  if (!request) return;
  pending.delete(id);
  clearTimeout(request.timer);
  request.resolve(result);
  if (pending.size === 0) {
    if (broker) idle(broker);
  }
}

/** Every lost child keeps its own teardown deadline and ownership until exit. */
function retire(child: ChildProcess): Promise<void> {
  const existing = retired.get(child);
  if (existing) return existing;
  if (
    child.pid === undefined ||
    typeof child.exitCode === 'number' ||
    typeof child.signalCode === 'string'
  )
    return Promise.resolve();
  const done = new Promise<void>((resolve) => {
    const force = setTimeout(() => {
      try {
        child.kill('SIGKILL');
      } catch {
        // Keep ownership until the exit event, even if signalling fails.
      }
    }, 1000);
    child.once('exit', () => {
      clearTimeout(force);
      retired.delete(child);
      resolve();
    });
    // An unresponsive broker cannot handle SIGTERM or its IPC disconnect itself.
    child.ref();
    child.channel?.ref?.();
    try {
      child.kill('SIGTERM');
    } catch {
      // Its independent force timer still applies.
    }
  });
  retired.set(child, done);
  return done;
}

function lost(child: ChildProcess): void {
  if (broker !== child) return;
  broker = undefined;
  starting = undefined;
  clearTimeout(idleTimer);
  for (const [id, request] of pending) {
    killOwned(request.pid);
    settle(id, failed());
  }
  // Never retry an in-flight operation: it may have been a credential install.
  // The next call starts a new broker; failed providers remain unavailable.
  void retire(child);
}

/** Start early, before analytics loads its large heap. Also lazy for standalone transports. */
export function startProcessBroker(): Promise<ChildProcess> {
  if (starting) return starting;
  if (stopping) return Promise.reject(new NetworkError('Private command transport unavailable.'));
  const child = fork(script, [], {
    execArgv: ['--max-old-space-size=48'],
    stdio: ['ignore', 'ignore', 'ignore', 'ipc'],
    // The broker itself needs no credentials, session ids or loader injection.
    env:
      process.platform === 'win32'
        ? { SystemRoot: process.env.SystemRoot, PATH: process.env.PATH }
        : { PATH: process.env.PATH },
    serialization: 'json',
  });
  broker = child;
  launches++;
  starting = new Promise<ChildProcess>((resolve, reject) => {
    const timeout = setTimeout(() => {
      reject(new NetworkError('Private command transport unavailable.'));
      lost(child);
    }, 5000);
    const failStart = () => {
      clearTimeout(timeout);
      reject(new NetworkError('Private command transport unavailable.'));
      lost(child);
    };
    child.once('error', failStart);
    child.once('exit', failStart);
    child.once('disconnect', failStart);
    child.on('message', (message: unknown) => {
      if (broker !== child || !message || typeof message !== 'object') return;
      const packet = message as {
        type?: string;
        id?: number;
        pid?: number;
        result?: ManagedCommandResult;
      };
      if (packet.type === 'ready') {
        clearTimeout(timeout);
        idle(child);
        resolve(child);
      } else if (packet.id !== undefined && pending.has(packet.id)) {
        if (packet.type === 'started' && Number.isSafeInteger(packet.pid)) {
          const request = pending.get(packet.id);
          if (request) request.pid = packet.pid;
        } else if (packet.type === 'result' && packet.result) {
          settle(packet.id, packet.result);
        }
      }
    });
  });
  return starting;
}

/** Bounded execFile equivalent: same argv/env/cwd/stdin, without forking the dashboard. */
export function runManagedCommand(
  file: string,
  args: string[],
  options: ManagedCommandOptions
): Promise<ManagedCommandResult> {
  if (stopping) return Promise.resolve(failed());
  const now = Date.now();
  const hardDeadlineAt = now + options.timeoutMs + (options.graceMs ?? 5000);
  const deadlineAt = Math.min(options.deadlineAt ?? hardDeadlineAt, hardDeadlineAt);
  if (!Number.isFinite(deadlineAt)) return Promise.resolve(failed());
  if (deadlineAt <= now) return Promise.resolve({ ...failed(), timedOut: true });
  const command = {
    file,
    args: [...args],
    options: {
      ...options,
      coalesce: undefined,
      deadlineAt: undefined,
      input: options.input ?? '',
      env: { ...(options.env ?? process.env) },
      cwd: options.cwd ?? process.cwd(),
      graceMs: options.graceMs ?? 5000,
    },
  };
  const serialized = JSON.stringify(command);
  const bytes = Buffer.byteLength(serialized);
  if (bytes > 5 * 1024 * 1024) return Promise.resolve(failed());
  const key = options.coalesce ? createHash('sha256').update(serialized).digest('hex') : null;
  if (key) {
    const existing = identical.get(key);
    if (existing) return join(existing, Math.min(deadlineAt, existing.hardDeadlineAt));
  }
  if (Date.now() >= deadlineAt) return Promise.resolve({ ...failed(), timedOut: true });
  // Count callers awaiting startup too; a burst must not retain unbounded stdin/env copies.
  if (outstanding >= 64 || inputBytes + bytes > 16 * 1024 * 1024) return Promise.resolve(failed());
  outstanding++;
  inputBytes += bytes;
  const job: CommandJob = {
    hardDeadlineAt,
    deadlineAt,
    waiters: new Set(),
    abandoned: false,
  };
  if (key) identical.set(key, job);
  const result = join(job, deadlineAt);
  const work = (async (): Promise<ManagedCommandResult> => {
    let child: ChildProcess;
    try {
      child = await startProcessBroker();
    } catch {
      return failed();
    }
    // A caller can expire while startup is awaiting readiness. Never submit its work later.
    if (job.abandoned || Date.now() >= job.deadlineAt) return { ...failed(), timedOut: true };
    if (pending.size >= 64 || broker !== child || stopping) return failed();
    const id = ++sequence;
    job.id = id;
    job.child = child;
    clearTimeout(idleTimer);
    child.ref();
    child.channel?.ref?.();
    return new Promise((resolve) => {
      const timer = watchdog(job, child, id);
      pending.set(id, { resolve, timer });
      try {
        child.send(
          {
            type: 'run',
            id,
            ...command,
            deadlineAt: job.deadlineAt,
            hardDeadlineAt: job.hardDeadlineAt,
          },
          (error) => {
            if (error) lost(child);
          }
        );
      } catch {
        lost(child);
      }
    });
  })().finally(() => {
    outstanding--;
    inputBytes -= bytes;
    if (key && identical.get(key) === job) identical.delete(key);
  });
  void work.then((value) => {
    for (const waiter of job.waiters) {
      clearTimeout(waiter.timer);
      waiter.resolve(value);
    }
    job.waiters.clear();
  });
  return result;
}

function watchdog(job: CommandJob, child: ChildProcess, id: number): ReturnType<typeof setTimeout> {
  return setTimeout(
    () => {
      killOwned(pending.get(id)?.pid);
      settle(id, { ...failed(), timedOut: true });
      // No replay: a missing cancellation/result reply retires the owning broker.
      lost(child);
    },
    Math.max(0, job.deadlineAt - Date.now()) + 1000
  );
}

function join(job: CommandJob, deadlineAt: number): Promise<ManagedCommandResult> {
  if (deadlineAt <= Date.now()) return Promise.resolve({ ...failed(), timedOut: true });
  if (job.waiters.size >= 64) return Promise.resolve(failed());
  // A later waiter may outlive another caller, but never the first command's hard cap.
  if (deadlineAt > job.deadlineAt) {
    job.deadlineAt = deadlineAt;
    if (job.child && job.id !== undefined) {
      const request = pending.get(job.id);
      if (request) {
        clearTimeout(request.timer);
        request.timer = watchdog(job, job.child, job.id);
      }
      sendControl(job, 'deadline');
    }
  }
  return new Promise((resolve) => {
    const waiter: CommandWaiter = {
      resolve,
      timer: setTimeout(
        () => {
          job.waiters.delete(waiter);
          if (job.waiters.size === 0) {
            job.abandoned = true;
            for (const [key, value] of identical) if (value === job) identical.delete(key);
            // Kill directly too: the broker may be stopped or its IPC queue may be delayed.
            if (job.id !== undefined) killOwned(pending.get(job.id)?.pid);
            sendControl(job, 'cancel');
          }
          resolve({ ...failed(), timedOut: true });
        },
        Math.max(0, deadlineAt - Date.now())
      ),
    };
    job.waiters.add(waiter);
  });
}

function sendControl(job: CommandJob, type: 'deadline' | 'cancel'): void {
  if (!job.child || job.id === undefined) return;
  const child = job.child;
  try {
    child.send({ type, id: job.id, deadlineAt: job.deadlineAt }, (error) => {
      if (error) lost(child);
    });
  } catch {
    lost(child);
  }
}

export function processBrokerStats(): {
  pid: number | null;
  launches: number;
  pending: number;
  coalesced: number;
  retired: number;
} {
  return {
    pid: broker?.pid ?? null,
    launches,
    pending: pending.size,
    coalesced: identical.size,
    retired: retired.size,
  };
}

export function stopProcessBroker(): Promise<void> {
  if (stoppingWork) return stoppingWork;
  stopping = true;
  const child = broker;
  if (child) lost(child);
  stoppingWork = Promise.all([...retired.values()])
    .then(() => undefined)
    .finally(() => {
      identical.clear();
      stopping = false;
      stoppingWork = undefined;
    });
  return stoppingWork;
}
