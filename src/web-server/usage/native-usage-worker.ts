/** Parse history and aggregate it away from the dashboard's HTTP event loop. */
import { parentPort, workerData } from 'worker_threads';
import { ValidationError } from '../../errors/error-types';
import { scanProjectsDirectory } from '../jsonl-parser';
import {
  aggregateDailyUsage,
  aggregateHourlyUsage,
  aggregateMonthlyUsage,
  aggregateSessionUsage,
} from './data-aggregator';
import { scanCodexNativeUsageEntries } from './codex-native-usage-collector';
import { scanDroidNativeUsageEntries } from './droid-native-usage-collector';
import { collectAccountActivity, collectCodexPartition } from './account-activity-collector';
import { lowerCollectorThreadPriority } from './collector-concurrency';
import type { UsageWorkerRequest, UsageWorkerResponse } from './worker-client';

async function collectUsage(request: UsageWorkerRequest): Promise<UsageWorkerResponse> {
  // A fanned-out codex shard: read only the assigned files and hand the
  // per-file outcomes back for the parent's merge. Only `partitionFiles`
  // carries meaning here; the aggregate fields stay empty.
  if (request.kind === 'codex' && request.activity && request.partition)
    return {
      ok: true,
      data: {
        daily: [],
        monthly: [],
        hourly: [],
        session: [],
        eventCount: 0,
        partitionFiles: await collectCodexPartition(request.partition, request.activity),
      },
    };
  if (
    (request.kind === 'claude' ||
      request.kind === 'codex' ||
      request.kind === 'omp' ||
      request.kind === 'muse' ||
      request.kind === 'zcode' ||
      request.kind === 'jsonl') &&
    request.activity
  )
    return { ok: true, data: await collectAccountActivity(request, request.activity) };
  let entries;
  let source;
  switch (request.kind) {
    case 'claude':
      entries = await scanProjectsDirectory({ projectsDir: request.projectsDir });
      source = 'custom-parser';
      break;
    case 'codex':
      entries = await scanCodexNativeUsageEntries({
        env: { ...process.env, CODEX_HOME: request.codexHome },
        cacheDir: request.cacheDir,
      });
      source = 'codex-native';
      break;
    case 'droid':
      entries = await scanDroidNativeUsageEntries({
        env: { ...process.env, CCS_HOME: request.homeDir },
      });
      source = 'droid-native';
      break;
    default:
      throw new ValidationError('Unknown usage worker source');
  }

  // Raw events (including the large native cache) stay in this thread. Only
  // compact summaries cross the boundary, preserving existing source labels.
  return {
    ok: true,
    data: {
      eventCount: entries.length,
      daily: aggregateDailyUsage(entries, source),
      hourly: aggregateHourlyUsage(entries, source),
      monthly: aggregateMonthlyUsage(entries, source),
      session: aggregateSessionUsage(entries, source),
    },
  };
}

if (parentPort) {
  const port = parentPort;
  // Parsing and file reads run on this thread at a low priority, so several collectors running
  // at once never compete with the server's own request handling.
  lowerCollectorThreadPriority();
  void collectUsage(workerData as UsageWorkerRequest)
    .then((response) => port.postMessage(response))
    .catch((error: unknown) => {
      const response: UsageWorkerResponse = {
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      };
      port.postMessage(response);
    })
    .finally(() => port.close());
}
