import * as path from 'path';
import { Worker } from 'worker_threads';
import { getCcsDir } from '../../config/config-loader-facade';
import { CCSError } from '../../errors/error-types';
import type { DailyUsage, HourlyUsage, MonthlyUsage, SessionUsage } from './types';
import type { AccountActivityScanOptions } from './account-activity-collector';

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

export type UsageWorkerRequest =
  /**
   * `experimentRoots` / `experimentDbs` (experiment-usage-roots.ts) make a separate request that
   * reads only those roots, deduplicated record by record; the default root then serves only as
   * the reference copies are checked against (see collectExperimentActivity).
   */
  | {
      kind: 'claude';
      projectsDir: string;
      experimentRoots?: string[];
      /** Experiment requests: every default Claude root, whose responses' copies never count. */
      referenceRoots?: string[];
      activity?: AccountActivityScanOptions;
    }
  | {
      kind: 'codex';
      codexHome: string;
      cacheDir: string;
      experimentRoots?: string[];
      activity?: AccountActivityScanOptions;
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
  | { kind: 'droid'; homeDir: string }
  /** One slice of the experiment-root walk (experiment-usage-roots.ts); returns no usage. */
  | { kind: 'experiment-roots'; cacheDir: string };

export interface UsageWorkerResult {
  daily: DailyUsage[];
  hourly: HourlyUsage[];
  monthly: MonthlyUsage[];
  session: SessionUsage[];
  eventCount: number;
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
