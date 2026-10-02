import * as fs from 'fs';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { aggregateRows, type CompactEntry } from '../usage/account-activity-collector';
import type { UsageWorkerResult } from '../usage/worker-client';
import {
  runAnalyticsRemoteHelper,
  resolveAnalyticsRemoteHosts,
  type AnalyticsRemoteFingerprint,
  type AnalyticsRemoteHost,
  type AnalyticsRemoteKind,
  type AnalyticsRemoteResponse,
  type AnalyticsRemoteRow,
} from './analytics-remote-transport';

export type AnalyticsSourceTool = 'omp' | 'muse' | 'zcode';
export type AnalyticsSourceState = 'ok' | 'cached' | 'unavailable' | 'not_installed';

export interface AnalyticsRemoteSourceState {
  tool: AnalyticsSourceTool;
  host: AnalyticsRemoteHost;
  state: AnalyticsSourceState;
  lastScanAt: string | null;
  rowCount: number;
  detail: string | null;
}

export interface AnalyticsRemoteSourceDeps {
  hosts?: () => Promise<{ mac: string | null; windows: string | null }>;
  runHelper?: (
    sshHost: string,
    platform: AnalyticsRemoteHost,
    request: {
      kinds: AnalyticsRemoteKind[];
      minDateMs: number;
      fingerprints: Record<string, Record<string, AnalyticsRemoteFingerprint>>;
    }
  ) => Promise<AnalyticsRemoteResponse>;
  now?: () => number;
  cacheDir?: string;
}

/** Remote coverage: OMP on the Mac and Windows; Muse and zcode on the Mac only. */
const REMOTE_TARGETS: Record<AnalyticsRemoteHost, AnalyticsRemoteKind[]> = {
  mac: ['omp', 'muse', 'zcode'],
  windows: ['omp'],
};

const MAX_CACHED_ROWS = 100_000;

interface RemoteCache {
  version: 1;
  fingerprints: Record<string, Record<string, AnalyticsRemoteFingerprint>>;
  rows: AnalyticsRemoteRow[];
  lastScanAt: string | null;
}

function cacheFile(cacheDir: string, host: AnalyticsRemoteHost): string {
  return path.join(cacheDir, 'analytics-remote-v1', `${host}.json`);
}

function blankCache(): RemoteCache {
  return { version: 1, fingerprints: {}, rows: [], lastScanAt: null };
}

function loadCache(file: string): RemoteCache {
  try {
    if (fs.statSync(file).size > 8 * 1024 * 1024) return blankCache();
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as RemoteCache;
    if (
      value.version !== 1 ||
      !value.fingerprints ||
      typeof value.fingerprints !== 'object' ||
      !Array.isArray(value.rows) ||
      value.rows.length > MAX_CACHED_ROWS ||
      (value.lastScanAt !== null && typeof value.lastScanAt !== 'string')
    )
      return blankCache();
    return value;
  } catch {
    return blankCache();
  }
}

function saveCache(file: string, value: RemoteCache): void {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const body = JSON.stringify({ ...value, rows: value.rows.slice(0, MAX_CACHED_ROWS) });
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, body, { mode: 0o600 });
  fs.renameSync(temporary, file);
  fs.chmodSync(file, 0o600);
}

function hourEpoch(hour: string): number {
  return Date.parse(`${hour.replace(' ', 'T')}:00Z`);
}

function samePrint(
  left: AnalyticsRemoteFingerprint | undefined,
  right: AnalyticsRemoteFingerprint | undefined
): boolean {
  return (
    !!left &&
    !!right &&
    left.size === right.size &&
    left.mtimeMs === right.mtimeMs &&
    (left.head ?? null) === (right.head ?? null) &&
    (left.tail ?? null) === (right.tail ?? null)
  );
}

function toCompact(rows: AnalyticsRemoteRow[]): CompactEntry[] {
  return rows.map((row) => ({
    entry: {
      inputTokens: Math.floor(row.i),
      outputTokens: Math.floor(row.o),
      cacheCreationTokens: Math.floor(row.cw),
      cacheReadTokens: Math.floor(row.cr),
      model: row.m,
      sessionId: '',
      timestamp: `${row.h.replace(' ', 'T')}:00Z`,
      projectPath: '',
      target: row.p ?? row.k,
      ...(row.c > 0 ? { costUsd: row.c } : {}),
    },
    events: Math.floor(row.n),
  }));
}

function toWorkerResult(rows: AnalyticsRemoteRow[], kind: AnalyticsRemoteKind): UsageWorkerResult {
  const { hourly, session } = aggregateRows(toCompact(rows), `${kind}-remote`);
  return {
    daily: [],
    monthly: [],
    hourly,
    session,
    eventCount: rows.reduce((sum, row) => sum + Math.floor(row.n), 0),
    scan: {
      complete: true,
      completedFiles: 0,
      totalFiles: 0,
      skippedLines: 0,
      failedFiles: 0,
      readBytes: 0,
      unfinishedFiles: 0,
    },
  };
}

/**
 * Collect remote usage over ssh, one helper invocation per host. A timeout or
 * failure leaves that source `cached` (previous aggregates) or `unavailable`,
 * never failing the page.
 */
export async function loadAnalyticsRemoteSources(
  minDateMs: number,
  deps: AnalyticsRemoteSourceDeps = {}
): Promise<{
  results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }>;
  states: AnalyticsRemoteSourceState[];
}> {
  const now = deps.now ?? Date.now;
  const cacheDir = deps.cacheDir ?? path.join(getCcsDir(), 'cache');
  const hosts = await (deps.hosts ?? resolveAnalyticsRemoteHosts)().catch(() => ({
    mac: null as string | null,
    windows: null as string | null,
  }));
  const runHelper = deps.runHelper ?? runAnalyticsRemoteHelper;
  const results: Array<{ tool: AnalyticsSourceTool; data: UsageWorkerResult }> = [];
  const states: AnalyticsRemoteSourceState[] = [];
  const jobs: Array<Promise<void>> = [];
  for (const host of ['mac', 'windows'] as const) {
    jobs.push(
      (async (): Promise<void> => {
        const kinds = REMOTE_TARGETS[host];
        const file = cacheFile(cacheDir, host);
        const cached = loadCache(file);
        const alias = hosts[host];
        if (!alias) {
          for (const tool of kinds)
            states.push({
              tool,
              host,
              state: 'unavailable',
              lastScanAt: cached.lastScanAt,
              rowCount: 0,
              detail: 'remote host is not configured',
            });
          return;
        }
        let response: AnalyticsRemoteResponse;
        try {
          response = await runHelper(alias, host, {
            kinds,
            minDateMs,
            fingerprints: cached.fingerprints,
          });
        } catch {
          // Previous aggregates stay available; nothing remote fails the page.
          for (const tool of kinds) {
            const kept = cached.rows.filter(
              (row) =>
                row.k === tool && Number.isFinite(hourEpoch(row.h)) && hourEpoch(row.h) >= minDateMs
            );
            if (kept.length) {
              results.push({ tool, data: toWorkerResult(kept, tool) });
              states.push({
                tool,
                host,
                state: 'cached',
                lastScanAt: cached.lastScanAt,
                rowCount: kept.reduce((sum, row) => sum + Math.floor(row.n), 0),
                detail: 'remote scan failed; showing previously read aggregates',
              });
            } else {
              states.push({
                tool,
                host,
                state: 'unavailable',
                lastScanAt: cached.lastScanAt,
                rowCount: 0,
                detail: 'remote scan failed',
              });
            }
          }
          return;
        }
        const freshPrints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {};
        for (const tool of kinds) freshPrints[tool] = response.kinds[tool]?.fingerprints ?? {};
        const kept = cached.rows.filter((row) => {
          if (!kinds.includes(row.k)) return false;
          if (!Number.isFinite(hourEpoch(row.h)) || hourEpoch(row.h) < minDateMs) return false;
          const print = freshPrints[row.k]?.[row.f];
          // A truncated scan never visited every file: rows for unvisited
          // files stay until a complete scan revisits them.
          if (print === undefined) return response.truncated;
          return samePrint(print, cached.fingerprints[row.k]?.[row.f]);
        });
        const fresh = response.rows.filter(
          (row) =>
            kinds.includes(row.k) &&
            Number.isFinite(hourEpoch(row.h)) &&
            hourEpoch(row.h) >= minDateMs
        );
        const merged = [...kept, ...fresh].slice(0, MAX_CACHED_ROWS);
        const scannedAt = new Date(now()).toISOString();
        const prints: Record<string, Record<string, AnalyticsRemoteFingerprint>> = {
          ...freshPrints,
        };
        if (response.truncated) {
          for (const [kind, entries] of Object.entries(cached.fingerprints)) {
            for (const [key, print] of Object.entries(entries)) {
              prints[kind] ??= {};
              prints[kind][key] ??= print;
            }
          }
        }
        try {
          saveCache(file, {
            version: 1,
            fingerprints: prints,
            rows: merged,
            lastScanAt: scannedAt,
          });
        } catch {
          /* Cache writes are best-effort; the rows are still served. */
        }
        for (const tool of kinds) {
          const rows = merged.filter((row) => row.k === tool);
          const kindState = response.kinds[tool]?.state ?? 'ok';
          if (kindState === 'not_installed') {
            states.push({
              tool,
              host,
              state: 'not_installed',
              lastScanAt: scannedAt,
              rowCount: 0,
              detail: 'no usage logs found on this host',
            });
            continue;
          }
          if (rows.length) results.push({ tool, data: toWorkerResult(rows, tool) });
          states.push({
            tool,
            host,
            state: response.truncated ? 'cached' : 'ok',
            lastScanAt: scannedAt,
            rowCount: rows.reduce((sum, row) => sum + Math.floor(row.n), 0),
            detail: response.truncated
              ? 'remote scan hit its bounds; showing partial aggregates'
              : null,
          });
        }
      })()
    );
  }
  await Promise.all(jobs);
  states.sort((a, b) => a.tool.localeCompare(b.tool) || a.host.localeCompare(b.host));
  return { results, states };
}
