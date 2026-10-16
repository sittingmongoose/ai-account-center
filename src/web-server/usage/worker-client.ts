import * as path from 'path';
import * as fs from 'fs';
import { Worker } from 'worker_threads';
import { getCcsDir } from '../../config/config-loader-facade';
import { CCSError } from '../../errors/error-types';
import type { DailyUsage, HourlyUsage, MonthlyUsage, SessionUsage } from './types';
import type {
  AccountActivityScanOptions,
  CodexPartitionFileResult,
} from './account-activity-collector';

/**
 * Dot paths into a generic JSONL record (`usage.input_tokens`), from a saved
 * `jsonl` usage-log source. Only `timestamp` is required; a missing token or
 * cost field counts as zero/unlogged, never as an error.
 */
export interface JsonlFieldMapping {
  timestamp: string;
  model?: string;
  inputTokens?: string;
  outputTokens?: string;
  cost?: string;
}

/**
 * One shard of a fanned-out codex scan (N14): the parent worker discovered,
 * mtime-filtered, deduped and sorted every rollout file, then split the list
 * into disjoint contiguous chunks. A partition reader parses only its own
 * files — exactly one reader per file — against the shared checkpoint
 * directory, stopping new reads at `deadline` (the parent's own scan
 * deadline, so a fanned-out pass never outruns the worker's time bound).
 */
export interface CodexPartitionSpec {
  files: string[];
  checkpointDir: string;
  deadline: number;
}

/** Compact checkpoint identity, never rollout contents. */
export interface LargeCodexReadSpec {
  file: string;
  checkpoint: string;
  revision: string;
  minDate: number;
}

export interface LargeCodexCompletion {
  /** A compare-and-swap guard against a normal pass advancing the checkpoint. */
  baseline: string;
  body: string;
}

export function codexFileRevision(stats: fs.Stats): string {
  return `${stats.dev}:${stats.ino}:${stats.size}:${stats.mtimeMs}:${stats.ctimeMs}`;
}

export const LARGE_CODEX_READ_TIMEOUT_MS = 15 * 60 * 1000;
const LARGE_CODEX_RETRY_MS = 24 * 60 * 60 * 1000;

export type UsageWorkerRequest =
  /**
   * `experimentRoots` / `experimentDbs` (experiment-usage-roots.ts) and `homeRoots`
   * (t3-usage-roots.ts) make a separate request that reads only those roots, deduplicated record
   * by record; the default root then serves only as the reference copies are checked against
   * (see collectExperimentActivity).
   */
  | {
      kind: 'claude';
      projectsDir: string;
      experimentRoots?: string[];
      /**
       * T3 Code account homes (t3-usage-roots.ts), read like experiment roots but in full depth
       * and following their symlinks: each real file once, copies of default records dropped.
       */
      homeRoots?: string[];
      /** Experiment requests: every default Claude root, whose responses' copies never count. */
      referenceRoots?: string[];
      activity?: AccountActivityScanOptions;
    }
  | {
      kind: 'codex';
      codexHome: string;
      cacheDir: string;
      experimentRoots?: string[];
      /** T3 Code shadow homes' `sessions` folders that are not links into the default root. */
      homeRoots?: string[];
      activity?: AccountActivityScanOptions;
      partition?: CodexPartitionSpec;
      largeRead?: { spec: LargeCodexReadSpec; deadline: number };
    }
  | { kind: 'omp'; roots: string[]; activity?: AccountActivityScanOptions }
  | {
      kind: 'muse';
      sessionsDir: string;
      experimentRoots?: string[];
      activity?: AccountActivityScanOptions;
    }
  | {
      kind: 'zcode';
      dbPath: string;
      experimentDbs?: string[];
      activity?: AccountActivityScanOptions;
    }
  | {
      kind: 'jsonl';
      roots: string[];
      mapping: JsonlFieldMapping;
      activity?: AccountActivityScanOptions;
    }
  /**
   * Antigravity's conversation databases, read by the packaged helper on this host
   * (antigravity-native-usage-collector.ts), which finds the roots itself.
   */
  | { kind: 'antigravity'; activity?: AccountActivityScanOptions }
  | { kind: 'droid'; homeDir: string }
  /** One slice of the experiment-root walk (experiment-usage-roots.ts); returns no usage. */
  | { kind: 'experiment-roots'; cacheDir: string };

export interface UsageWorkerResult {
  /** Saved result's original read time when an experiment replays its last safe aggregate. */
  readAt?: string;
  daily: DailyUsage[];
  hourly: HourlyUsage[];
  monthly: MonthlyUsage[];
  session: SessionUsage[];
  eventCount: number;
  /**
   * Set only by a codex partition reader: per-file outcomes for the merge.
   * Every other field is empty there; only the parent reads this field.
   */
  partitionFiles?: CodexPartitionFileResult[];
  largeCodexCompletion?: LargeCodexCompletion;
  /** Internal bounded-scan progress; never claims partial history is complete. */
  scan?: {
    complete: boolean;
    completedFiles: number;
    totalFiles: number;
    skippedLines: number;
    failedFiles: number;
    readBytes: number;
    unfinishedFiles?: number;
    largeCodexReads?: LargeCodexReadSpec[];
  };
}

export type UsageWorkerResponse =
  | { ok: true; data: UsageWorkerResult }
  | { ok: false; error: string };

function getUsageWorkerPath(): string {
  // Bun runs source tests directly; installed Node runs the emitted neighbor.
  return path.join(__dirname, `native-usage-worker${path.extname(__filename)}`);
}

/** Dedicated low-priority worker; the normal pass's heap and deadlines are unchanged. */
export function spawnLargeCodexReader(
  spec: LargeCodexReadSpec,
  signal: AbortSignal,
  timeoutMs = LARGE_CODEX_READ_TIMEOUT_MS,
  workerPath = getUsageWorkerPath()
): Promise<LargeCodexCompletion | undefined> {
  if (signal.aborted) return Promise.resolve(undefined);
  return new Promise((resolve) => {
    const boundedMs = Math.max(1, Math.min(LARGE_CODEX_READ_TIMEOUT_MS, timeoutMs));
    const worker = new Worker(workerPath, {
      workerData: {
        kind: 'codex',
        largeRead: { spec, deadline: Date.now() + boundedMs },
      },
      env: { ...process.env, CCS_DIR: getCcsDir() },
      resourceLimits: { maxOldGenerationSizeMb: 256, stackSizeMb: 4 },
    });
    let settled = false;
    const finish = (result?: LargeCodexCompletion): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal.removeEventListener('abort', abort);
      worker.removeAllListeners();
      void worker
        .terminate()
        .catch(() => 0)
        .then(() => resolve(result));
    };
    const abort = (): void => finish();
    const timer = setTimeout(abort, boundedMs);
    signal.addEventListener('abort', abort, { once: true });
    worker.once('message', (response: UsageWorkerResponse) =>
      finish(response?.ok === true ? response.data?.largeCodexCompletion : undefined)
    );
    worker.once('error', abort);
    worker.once('exit', abort);
  });
}

export type LargeCodexReader = typeof spawnLargeCodexReader;

/** One reader per dashboard, with a bounded queue and persistent retry back-off. */
export class LargeCodexReadQueue {
  private readonly queued = new Map<string, LargeCodexReadSpec>();
  private active: { spec: LargeCodexReadSpec; controller: AbortController } | undefined;
  private pending: Promise<void> | undefined;
  private closed = false;

  constructor(
    private readonly reader: LargeCodexReader = spawnLargeCodexReader,
    private readonly timeoutMs = LARGE_CODEX_READ_TIMEOUT_MS
  ) {}

  isRunning(specs: readonly LargeCodexReadSpec[]): boolean {
    return specs.some(
      (spec) =>
        spec.checkpoint === this.active?.spec.checkpoint &&
        spec.revision === this.active.spec.revision
    );
  }

  enqueue(specs: readonly LargeCodexReadSpec[]): void {
    if (this.closed) return;
    for (const spec of specs) {
      if (this.queued.size >= 64) break;
      if (!this.isRunning([spec])) this.queued.set(spec.checkpoint, spec);
    }
    if (!this.pending) {
      this.pending = this.drain().finally(() => {
        this.pending = undefined;
        if (this.queued.size && !this.closed) this.enqueue([]);
      });
    }
  }

  private async drain(): Promise<void> {
    while (!this.closed && this.queued.size) {
      const spec = this.queued.values().next().value as LargeCodexReadSpec;
      this.queued.delete(spec.checkpoint);
      const attemptPath = `${spec.checkpoint}.large-attempt`;
      try {
        const attempt = JSON.parse(await fs.promises.readFile(attemptPath, 'utf8'));
        if (attempt.revision === spec.revision && attempt.retryAt > Date.now()) continue;
      } catch {
        /* First attempt, or an invalid marker. */
      }
      if (this.closed) break;
      try {
        if (codexFileRevision(await fs.promises.stat(spec.file)) !== spec.revision) continue;
        await fs.promises.writeFile(
          attemptPath,
          JSON.stringify({
            revision: spec.revision,
            retryAt: Date.now() + LARGE_CODEX_RETRY_MS,
          }),
          { mode: 0o600 }
        );
        if (this.closed) break;
        const controller = new AbortController();
        this.active = { spec, controller };
        const completion = await this.reader(spec, controller.signal, this.timeoutMs);
        if (
          !this.closed &&
          !controller.signal.aborted &&
          completion &&
          Buffer.byteLength(completion.body) <= 8 * 1024 * 1024 &&
          codexFileRevision(await fs.promises.stat(spec.file)) === spec.revision
        ) {
          // Only the next normal pass adopts this result. A killed/timed-out worker never writes
          // canonical checkpoints, and a concurrent pass cannot overwrite a completed handoff.
          const body = JSON.parse(completion.body);
          body.largeReadBaseline = completion.baseline;
          const temporary = `${spec.checkpoint}.large-complete.tmp`;
          try {
            await fs.promises.writeFile(temporary, JSON.stringify(body), { mode: 0o600 });
            if (!this.closed && !controller.signal.aborted)
              await fs.promises.rename(temporary, `${spec.checkpoint}.large-complete`);
          } finally {
            await fs.promises.unlink(temporary).catch(() => undefined);
          }
        }
      } catch {
        /* Failure keeps the paused checkpoint and its detail untouched. */
      } finally {
        this.active = undefined;
      }
    }
  }

  async shutdown(): Promise<void> {
    this.closed = true;
    this.queued.clear();
    this.active?.controller.abort();
    await this.pending;
  }
}

/** A codex partition reader failed or overran its time bound; `timedOut` tells which. */
export class CodexPartitionError extends CCSError {
  readonly timedOut: boolean;

  constructor(timedOut: boolean) {
    super(
      timedOut
        ? 'Codex partition reader timed out.'
        : 'Codex partition reader could not return its files.'
    );
    this.timedOut = timedOut;
  }
}

/**
 * Read one codex partition in a nested worker thread and return its per-file
 * outcomes. Like the top-level collectors, the reader runs at a low thread
 * priority (the worker entry lowers it) and settles only once its thread has
 * stopped, so a reader cut off by its time bound is never still writing a
 * checkpoint when the next collection reads the same files.
 */
export function spawnCodexPartitionReader(
  request: Extract<UsageWorkerRequest, { kind: 'codex' }>,
  options: AccountActivityScanOptions,
  partition: CodexPartitionSpec,
  timeoutMs: number,
  workerPath = getUsageWorkerPath()
): Promise<CodexPartitionFileResult[]> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(workerPath, {
      workerData: { ...request, activity: options, partition },
      env: { ...process.env, CCS_DIR: getCcsDir() },
      resourceLimits: { maxOldGenerationSizeMb: 256, stackSizeMb: 4 },
    });
    let settled = false;
    const finish = (files?: CodexPartitionFileResult[], timedOut = false): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      worker.removeAllListeners();
      void worker
        .terminate()
        .catch(() => 0)
        .then(() => {
          if (files) resolve(files);
          else reject(new CodexPartitionError(timedOut));
        });
    };
    const timer = setTimeout(() => finish(undefined, true), Math.max(1, timeoutMs));
    worker.once('message', (response: UsageWorkerResponse) => {
      const files = response?.ok === true ? response.data?.partitionFiles : undefined;
      finish(Array.isArray(files) ? files : undefined);
    });
    worker.once('error', () => finish());
    worker.once('exit', () => finish());
  });
}

/** One-shot workers leave refresh coalescing and cache ownership in the caller. */
export function loadUsageInWorker(
  request: UsageWorkerRequest,
  workerPath = getUsageWorkerPath()
): Promise<UsageWorkerResult> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(workerPath, {
      workerData: request,
      // Scoped config and --config-dir are not inherited by worker threads.
      // Resolve the effective directory in the HTTP thread for pricing caches.
      env: { ...process.env, CCS_DIR: getCcsDir() },
    });
    let settled = false;
    const finish = (error?: Error, result?: UsageWorkerResult): void => {
      if (settled) return;
      settled = true;
      worker.removeAllListeners();
      void worker.terminate().catch(() => {});
      if (error) reject(error);
      else if (result) resolve(result);
      else reject(new CCSError('Usage worker returned no result'));
    };
    worker.once('message', (response: UsageWorkerResponse) => {
      if (response?.ok === true && response.data) finish(undefined, response.data);
      else {
        finish(
          new CCSError(
            response?.ok === false ? response.error : 'Usage worker returned an invalid result'
          )
        );
      }
    });
    worker.once('error', (error) => finish(error));
    worker.once('exit', (code) =>
      finish(new CCSError(`Usage worker exited before returning a result (code ${code})`))
    );
  });
}
