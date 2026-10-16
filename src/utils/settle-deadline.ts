/**
 * Settle Deadline
 *
 * Abort signals and process kill timers only work when the underlying work
 * cooperates. A request whose completion is lost (no socket, no child, no
 * timer left) never settles, and anything coalesced onto it waits forever.
 * withDeadline settles the caller's promise regardless.
 */

/** The work did not settle before its deadline. It may still be running. */
export class DeadlineExceededError extends Error {
  constructor(label = 'Operation') {
    super(`${label} did not finish before its deadline`);
    this.name = 'DeadlineExceededError';
  }
}

/**
 * Resolve or reject with `work`, or reject with DeadlineExceededError after
 * `ms`. `onTimeout` runs first so the caller can abort or kill the work.
 * The work is not cancelled by this helper: callers must commit shared state
 * only after the awaited result, so a late result is discarded.
 */
export function withDeadline<T>(
  work: Promise<T>,
  ms: number,
  onTimeout?: () => void,
  label?: string
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return new Promise<T>((resolve, reject) => {
    timer = setTimeout(
      () => {
        try {
          onTimeout?.();
        } catch {
          // A failed abort or kill must not prevent settlement.
        }
        reject(new DeadlineExceededError(label));
      },
      Math.max(0, ms)
    );
    work.then(resolve, reject);
  }).finally(() => clearTimeout(timer));
}

/** Time after a process's own timeout (SIGTERM) before it is killed and settled. */
export const PROCESS_SETTLE_GRACE_MS = 5_000;

/**
 * execFile's timeout sends SIGTERM and then waits for the exit callback. A
 * child that ignores the signal, or a lost callback, would leave the caller
 * waiting. Settle `graceMs` after the process timeout, SIGKILL the child, and
 * reject with `expired()`.
 */
export function withProcessDeadline<T>(
  work: Promise<T>,
  child: () => { kill(signal?: NodeJS.Signals): boolean } | undefined,
  processTimeoutMs: number,
  expired: () => Error,
  graceMs = PROCESS_SETTLE_GRACE_MS
): Promise<T> {
  return withDeadline(work, processTimeoutMs + graceMs, () => child()?.kill('SIGKILL')).catch(
    (error: unknown) => {
      throw error instanceof DeadlineExceededError ? expired() : error;
    }
  );
}
