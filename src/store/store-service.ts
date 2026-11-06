/**
 * The TypeScript face of the AAC store (docs/store.md). One store per dashboard process, under
 * `<getCcsDir()>/store`. Every call is asynchronous (the native module works on libuv's thread
 * pool). When the module is absent, disabled (`AAC_STORE=off`) or fails to open, `getAacStore()`
 * is null and the dashboard keeps working without history.
 */
import * as fs from 'fs';
import * as path from 'path';
import { getCcsDir } from '../utils/config-manager';
import { createLogger } from '../services/logging';
import {
  loadNativeStore,
  type NativeLoadResult,
  type NativeStoreHandle,
  type StoreUnavailableReason,
} from './native-loader';
import type { StoreObservation } from './usage-observations';
import type { AccountAnalyticsObservation } from '../web-server/services/account-analytics-history';

const logger = createLogger('store');

export const STORE_RETENTION_DAYS = 90;
const MINUTE = 60_000;
const DAY = 86_400_000;
const MAINTAIN_EVERY_MS = 15 * MINUTE;
const BACKUP_EVERY_MS = DAY;
const REDB_BACKUP_EVERY_MS = 7 * DAY;
const VERIFY_EVERY_MS = 7 * DAY;

/** A store error's fixed code (`lock_held`, `newer_store`, `invalid`...), from `<code>: <message>`. */
export function storeErrorCode(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  const match = /^([a-z_]+): /.exec(message);
  return match ? match[1] : 'unknown';
}

export interface RecordResult {
  sampled: number;
  seen: number;
  skipped: number;
  rejected: number;
}

export type Thinning =
  | { kind: 'bucket'; bucketMs: number }
  | { kind: 'history' }
  | { kind: 'none' };

export interface SeriesRef {
  identity: string;
  window: string;
}

export interface LongBurnSpan {
  spanMs: number;
  consumed: number | null;
  rate: number | null;
  coverage: number;
}

export interface SeriesPointsQuery {
  identities?: string[];
  accountIds?: string[];
  provider?: string;
  windowKey?: string;
  fromMs: number;
  toMs: number;
  resolution: 'raw' | 'hour' | 'day';
  limit?: number;
}

export interface StoreSeriesPoints {
  resolution: string;
  columns: string[];
  series: Array<{
    identity: string;
    accountId: string;
    provider: string;
    plan: string | null;
    window: {
      identity: string;
      key: string;
      label: string;
      kind: string | null;
      unit: string | null;
      poolId: string | null;
      poolLabel: string | null;
    };
    points: Array<Array<number | boolean | null>>;
  }>;
  truncated: boolean;
}

export class AacStore {
  constructor(
    private readonly handle: NativeStoreHandle,
    readonly directory: string
  ) {}

  private async call<T>(method: string, args: unknown = {}): Promise<T> {
    return JSON.parse(await this.handle.call(method, JSON.stringify(args))) as T;
  }

  status(): Promise<Record<string, unknown>> {
    return this.call('status');
  }
  counts(): Promise<Record<string, number>> {
    return this.call('counts');
  }
  record(observations: StoreObservation[]): Promise<RecordResult> {
    return this.call('record', { observations });
  }
  historyRecords(query: {
    fromMs: number;
    toMs: number;
    nowMs: number;
    thinning: Thinning;
    identities?: string[];
  }): Promise<AccountAnalyticsObservation[]> {
    return this.call('historyRecords', query);
  }
  async oldest(): Promise<number | null> {
    return this.call<number | null>('oldest');
  }
  burnLong(
    series: SeriesRef[],
    spansMs: number[],
    nowMs: number
  ): Promise<Array<{ identity: string; window: string; spans: LongBurnSpan[] }>> {
    return this.call('burnLong', { series, spansMs, nowMs });
  }
  periods(
    series: SeriesRef[],
    fromMs: number,
    toMs: number
  ): Promise<Array<{ identity: string; window: string; periods: Array<Record<string, unknown>> }>> {
    return this.call('periods', { series, fromMs, toMs });
  }
  seriesPoints(query: SeriesPointsQuery): Promise<StoreSeriesPoints> {
    return this.call('seriesPoints', query);
  }
  usageAt(t: number): Promise<Record<string, unknown>> {
    return this.call('usageAt', { t });
  }
  maintain(options: {
    nowMs?: number;
    verify?: boolean;
    compact?: boolean;
  }): Promise<Record<string, unknown>> {
    return this.call('maintain', options);
  }
  backup(options: {
    roots: string[];
    withRedb: boolean;
    keepDaily?: number;
    keepWithRedb?: number;
  }): Promise<Record<string, unknown>> {
    return this.call('backup', options);
  }
  getMeta(key: string): Promise<string | null> {
    return this.call('getMeta', { key });
  }
  async setMeta(key: string, value: string): Promise<void> {
    await this.call('setMeta', { key: `ts:${key}`, value });
  }
  async close(): Promise<void> {
    await this.call('close');
  }
}

export interface StoreState {
  store: AacStore | null;
  /** Why there is no store: the module's reason, or an open error's code. */
  reason: StoreUnavailableReason | string | null;
}

let state: StoreState = { store: null, reason: null };
let opening: Promise<StoreState> | null = null;

export function storeDirectory(ccsDir: string = getCcsDir()): string {
  return path.join(ccsDir, 'store');
}

/** Opens the process's store once; later calls return the same state. */
export function openAacStore(
  options: {
    ccsDir?: string;
    env?: NodeJS.ProcessEnv;
    load?: () => NativeLoadResult;
  } = {}
): Promise<StoreState> {
  if (opening) return opening;
  opening = (async () => {
    const env = options.env ?? process.env;
    const loaded = (options.load ?? (() => loadNativeStore({ env })))();
    if (!loaded.module) {
      state = { store: null, reason: loaded.reason };
      return state;
    }
    const directory = storeDirectory(options.ccsDir);
    const handle = new loaded.module.NativeStore();
    const cacheMb = Number(env.AAC_STORE_CACHE_MB);
    try {
      await handle.call(
        'open',
        JSON.stringify({
          dir: directory,
          retentionDays: STORE_RETENTION_DAYS,
          ...(Number.isFinite(cacheMb) && cacheMb > 0
            ? { cacheBytes: Math.round(cacheMb) * 1024 * 1024 }
            : {}),
        })
      );
      state = { store: new AacStore(handle, directory), reason: null };
    } catch (error) {
      state = { store: null, reason: storeErrorCode(error) };
      try {
        logger.warn('store.open_failed', 'The history store could not open', {
          code: state.reason,
        });
      } catch {
        /* Logging never stops the dashboard. */
      }
    }
    return state;
  })();
  return opening;
}

export function getAacStore(): AacStore | null {
  return state.store;
}

export function aacStoreState(): StoreState {
  return state;
}

/** Closes the store (seals both logs, marks a clean shutdown). Safe to call more than once. */
export async function closeAacStore(): Promise<void> {
  const store = state.store;
  state = { store: null, reason: 'closed' };
  opening = Promise.resolve(state);
  if (store) await store.close().catch(() => undefined);
}

/** For tests: forget the process's store without closing it. */
export function resetAacStoreForTests(): void {
  state = { store: null, reason: null };
  opening = null;
}

/**
 * Where backups are copied off this computer (decision D3): `AAC_STORE_SHARE_BACKUP_DIR`, else
 * `share/ai-account-center/store-backups/<instance>` beside the agent usage snapshot folder on
 * the shared drive. Only an existing `share/ai-account-center` folder is used: an unmounted share
 * never grows a local folder.
 */
export function shareBackupRoot(
  instanceId: string,
  snapshotDirectory: string | null,
  env: NodeJS.ProcessEnv = process.env
): string | null {
  const explicit = env.AAC_STORE_SHARE_BACKUP_DIR?.trim();
  if (explicit) return explicit.toLowerCase() === 'off' ? null : explicit;
  if (!snapshotDirectory || !/^[a-f0-9]{32}$/.test(instanceId)) return null;
  const project = path.join(path.dirname(snapshotDirectory), 'share', 'ai-account-center');
  try {
    if (!fs.statSync(project).isDirectory()) return null;
  } catch {
    return null;
  }
  return path.join(project, 'store-backups', instanceId);
}

function metaTime(status: Record<string, unknown>, key: string): number {
  const meta = (status.meta ?? {}) as Record<string, unknown>;
  const value = Number(meta[key]);
  return Number.isFinite(value) ? value : 0;
}

/**
 * Runs maintenance every 15 minutes: seal, compress, close days, prune, retire; weekly verify
 * and compaction; a daily backup to `store/backups` and the share copy, with a redb snapshot
 * weekly. Returns the stop function.
 */
export function startAacStoreMaintenance(
  deps: {
    store?: () => AacStore | null;
    snapshotDirectory?: () => Promise<string | null>;
    now?: () => number;
    schedule?: (callback: () => void, ms: number) => ReturnType<typeof setTimeout>;
  } = {}
): () => void {
  const current = deps.store ?? getAacStore;
  const now = deps.now ?? Date.now;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  const run = async (): Promise<void> => {
    const store = current();
    if (!store || running) return;
    running = true;
    try {
      const status = await store.status();
      const at = now();
      const weekly = at - metaTime(status, 'last_verify_ms') >= VERIFY_EVERY_MS;
      await store.maintain({ nowMs: at, verify: weekly, compact: weekly });
      if (at - metaTime(status, 'last_backup_ms') >= BACKUP_EVERY_MS) {
        const lastRedb = Number((await store.getMeta('ts:last_redb_backup_ms')) ?? 0);
        const withRedb = at - (Number.isFinite(lastRedb) ? lastRedb : 0) >= REDB_BACKUP_EVERY_MS;
        const instance = String(status.instanceId ?? '');
        const share = shareBackupRoot(
          instance,
          deps.snapshotDirectory ? await deps.snapshotDirectory() : null
        );
        const result = await store.backup({
          roots: [path.join(store.directory, 'backups'), ...(share ? [share] : [])],
          withRedb,
        });
        if (withRedb) await store.setMeta('last_redb_backup_ms', String(at));
        const mirrors = (result.mirrors ?? []) as Array<{ ok: boolean }>;
        if (share && !mirrors.every((m) => m.ok)) {
          logger.warn('store.share_backup_failed', 'The share copy of the store backup failed');
        }
      }
    } catch (error) {
      try {
        logger.warn('store.maintenance_failed', 'Store maintenance did not finish', {
          code: storeErrorCode(error),
        });
      } catch {
        /* Logging never stops the dashboard. */
      }
    } finally {
      running = false;
    }
  };
  const loop = (delay: number): void => {
    if (stopped) return;
    timer = (deps.schedule ?? setTimeout)(() => {
      void run().finally(() => loop(MAINTAIN_EVERY_MS));
    }, delay);
    timer.unref?.();
  };
  // The first pass waits a few minutes, so a restart never competes with the first samples.
  loop(3 * MINUTE);
  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
  };
}
