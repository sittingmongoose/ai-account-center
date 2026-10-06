import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

/** No more collectors than this run at once, however big the machine. */
export const MAX_CONCURRENT_COLLECTORS = 8;
/** What one collector may need: its 256 MB heap cap, read buffers and the copy of its result. */
const COLLECTOR_MEMORY_BYTES = 1024 ** 3;
/** The nice value a collector thread runs at; the server's own threads keep theirs. */
export const COLLECTOR_NICE = 10;

function availableCpus(): number {
  try {
    return typeof os.availableParallelism === 'function'
      ? os.availableParallelism()
      : os.cpus().length;
  } catch {
    return 1;
  }
}

/**
 * How many local collectors run at once, sized to the machine: a quarter of its CPUs (the rest
 * stay free for the server and whatever else shares the host), no more than its memory holds at
 * COLLECTOR_MEMORY_BYTES each, never fewer than the two the old fixed pairs ran, never more than
 * MAX_CONCURRENT_COLLECTORS. A 56-core, 30 GB host runs 8; a 4-core laptop runs 2.
 */
export function collectorConcurrency(
  cpus: number = availableCpus(),
  totalMemoryBytes: number = os.totalmem()
): number {
  const byCpu = Math.floor(cpus / 4);
  const byMemory = Math.floor(totalMemoryBytes / COLLECTOR_MEMORY_BYTES);
  return Math.max(2, Math.min(MAX_CONCURRENT_COLLECTORS, byCpu, byMemory));
}

/**
 * Run `task` over items as they arrive, at most `limit` at a time. Each item starts the moment a
 * slot frees, so no item ever waits for a slow sibling to finish (there are no fixed batches).
 * Items come in batches: an array's items start at once, before this returns; a batch that is
 * still resolving (a slow discovery step) adds its items when it resolves, and a rejected one adds
 * none. At most `maxItems` items are taken,
 * in arrival order; `index` is an item's arrival position, so callers can keep results in a fixed
 * order however the tasks finish. `stop()` is asked before each start: once it says true, queued
 * items are left unstarted and counted as `skipped`. A task that throws or rejects only ends its
 * own slot. Resolves when every batch is in and every started task has settled.
 */
export function runBounded<T>(
  batches: ReadonlyArray<readonly T[] | Promise<readonly T[]>>,
  limit: number,
  task: (item: T, index: number) => Promise<unknown>,
  options: { stop?: () => boolean; maxItems?: number } = {}
): Promise<{ items: T[]; skipped: number }> {
  const slots = Math.max(1, Math.floor(limit));
  const maxItems = options.maxItems ?? Infinity;
  const stop = options.stop ?? (() => false);
  return new Promise((resolve) => {
    const items: T[] = [];
    const queue: number[] = [];
    let active = 0;
    let waiting = batches.length;
    let skipped = 0;
    let done = false;
    const pump = (): void => {
      while (active < slots && queue.length > 0) {
        const index = queue.shift() as number;
        if (stop()) {
          skipped++;
          continue;
        }
        active++;
        let running: Promise<unknown>;
        try {
          running = Promise.resolve(task(items[index], index));
        } catch (error) {
          running = Promise.reject(error);
        }
        void running
          .catch(() => undefined)
          .then(() => {
            active--;
            pump();
          });
      }
      if (!done && active === 0 && waiting === 0 && queue.length === 0) {
        done = true;
        resolve({ items, skipped });
      }
    };
    const take = (list: readonly T[]): void => {
      for (const item of list) {
        if (items.length >= maxItems) break;
        queue.push(items.length);
        items.push(item);
      }
    };
    for (const batch of batches) {
      if (Array.isArray(batch)) {
        take(batch as readonly T[]);
        waiting--;
        continue;
      }
      void Promise.resolve(batch)
        .then(take, () => undefined)
        .then(() => {
          waiting--;
          pump();
        });
    }
    pump();
  });
}

/**
 * Lower the calling thread's CPU priority to COLLECTOR_NICE (never raising it). On Linux a nice
 * value belongs to one thread, so this touches only a collector's own worker thread and never the
 * server's; the collectors read their log files on that same thread, and the Linux IO schedulers
 * that weigh priorities (BFQ, CFQ) give a thread without an explicit IO class a best-effort IO
 * priority derived from its nice value, so those reads go at low IO priority too (a disk on the
 * `none` or `mq-deadline` scheduler has no IO priorities at all). Elsewhere a nice value is
 * process-wide and would slow the server, so this does nothing. Returns whether it applied.
 */
export function lowerCollectorThreadPriority(
  platform: NodeJS.Platform = process.platform
): boolean {
  if (platform !== 'linux') return false;
  try {
    const tid = Number(path.basename(fs.readlinkSync('/proc/thread-self')));
    if (!Number.isSafeInteger(tid) || tid <= 0) return false;
    if (os.getPriority(tid) < COLLECTOR_NICE) os.setPriority(tid, COLLECTOR_NICE);
    return true;
  } catch {
    return false;
  }
}
