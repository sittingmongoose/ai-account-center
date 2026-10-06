import * as path from 'path';
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

export type UsageWorkerRequest =
  | { kind: 'claude'; projectsDir: string; activity?: AccountActivityScanOptions }
  | {
      kind: 'codex';
      codexHome: string;
      cacheDir: string;
      activity?: AccountActivityScanOptions;
      partition?: CodexPartitionSpec;
      /**
       * Experiment Codex session dirs, owned by the experiment-roots lane
       * (fix/aac-fw4-experiment-roots-20261006): when set, roots come from
       * here instead of codexHome, and the request bypasses the N14 fan-out
       * (its checkpoint mode differs), reading inline.
       */
      experimentRoots?: string[];
    }
  | { kind: 'omp'; roots: string[]; activity?: AccountActivityScanOptions }
  | { kind: 'muse'; sessionsDir: string; activity?: AccountActivityScanOptions }
  | { kind: 'zcode'; dbPath: string; activity?: AccountActivityScanOptions }
  | {
      kind: 'jsonl';
      roots: string[];
      mapping: JsonlFieldMapping;
      activity?: AccountActivityScanOptions;
    }
  | { kind: 'droid'; homeDir: string };

export interface UsageWorkerResult {
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
  /** Internal bounded-scan progress; never claims partial history is complete. */
  scan?: {
    complete: boolean;
    completedFiles: number;
    totalFiles: number;
    skippedLines: number;
    failedFiles: number;
    readBytes: number;
    unfinishedFiles?: number;
  };
}

export type UsageWorkerResponse =
  | { ok: true; data: UsageWorkerResult }
  | { ok: false; error: string };

function getUsageWorkerPath(): string {
  // Bun runs source tests directly; installed Node runs the emitted neighbor.
  return path.join(__dirname, `native-usage-worker${path.extname(__filename)}`);
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
