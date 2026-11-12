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
const identical = new Map<string, Promise<ManagedCommandResult>>();
let broker: ChildProcess | undefined;
let starting: Promise<ChildProcess> | undefined;
let sequence = 0;
let launches = 0;
let stopping = false;
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
  try {
    child.kill('SIGTERM');
  } catch {
    /* Already gone. */
  }
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
  const command = {
    file,
    args: [...args],
    options: {
      ...options,
      coalesce: undefined,
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
    if (existing) return existing;
  }
  // Count callers awaiting startup too; a burst must not retain unbounded stdin/env copies.
  if (outstanding >= 64 || inputBytes + bytes > 16 * 1024 * 1024) return Promise.resolve(failed());
  outstanding++;
  inputBytes += bytes;
  const work = (async (): Promise<ManagedCommandResult> => {
    let child: ChildProcess;
    try {
      child = await startProcessBroker();
    } catch {
      return failed();
    }
    if (pending.size >= 16 || broker !== child || stopping) return failed();
    const id = ++sequence;
    clearTimeout(idleTimer);
    child.ref();
    child.channel?.ref?.();
    return new Promise((resolve) => {
      const timer = setTimeout(
        () => {
          const request = pending.get(id);
          killOwned(request?.pid);
          settle(id, { ...failed(), timedOut: true });
          // A missing reply also retires the broker, so no unknown command keeps running.
          lost(child);
        },
        options.timeoutMs + command.options.graceMs + 1000
      );
      pending.set(id, { resolve, timer });
      try {
        child.send({ type: 'run', id, ...command }, (error) => {
          if (error) lost(child);
        });
      } catch {
        lost(child);
      }
    });
  })().finally(() => {
    outstanding--;
    inputBytes -= bytes;
  });
  if (key) {
    const coalesced = work.finally(() => identical.delete(key));
    identical.set(key, coalesced);
    return coalesced;
  }
  return work;
}

export function processBrokerStats(): {
  pid: number | null;
  launches: number;
  pending: number;
  coalesced: number;
} {
  return { pid: broker?.pid ?? null, launches, pending: pending.size, coalesced: identical.size };
}

export async function stopProcessBroker(): Promise<void> {
  stopping = true;
  const child = broker;
  if (child) {
    lost(child);
    await new Promise<void>((resolve) => {
      if (child.exitCode !== null || child.signalCode !== null) {
        resolve();
        return;
      }
      const timer = setTimeout(() => {
        child.kill('SIGKILL');
        resolve();
      }, 1000);
      child.once('exit', () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }
  identical.clear();
  stopping = false;
}
