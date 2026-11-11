/**
 * The agent usage snapshot: AI Account Center writes its account usage to a folder every agent
 * can already read (a network share every computer mounts), so
 * Claude, Codex, OMP, Muse, zcode, Cursor, Antigravity and any other agent can see how much quota
 * each provider and account has left, how fast it is being used and when it resets, with no key,
 * network route or client library.
 *
 * - `usage.json`: every account with its identity (provider, label, email, AAC profile, computer,
 *   active flag), each window's reading, and burn rates with a projected run-out time;
 * - `usage.md`: the same in plain words;
 * - `history.json`: the rolling 7-day readings of every window, for agents that gauge burn rate
 *   themselves;
 * - `locations.json`: where each group of computers finds this folder (`--location`), for the
 *   reader's rules block.
 *
 * Off until `ai-account-center dashboard usage-snapshot set <folder>` saves a folder in
 * `~/.ccs/agent-usage-snapshot.json`; the running dashboard reads that setting each time, so no
 * restart is needed. The files are written after every analytics sample (the dashboard refresh
 * interval, one minute by default), from the readings that sample just recorded, so agents add no
 * provider traffic. Every account field passes the allowlist projection: tokens, credentials,
 * file paths and raw provider answers never reach the folder. One write runs at a time: on a hard
 * network mount a stalled file server holds one write, never the dashboard.
 */
import { randomBytes } from 'crypto';
import fs from 'fs';
import { hostname } from 'os';
import { ValidationError } from '../../errors/error-types';
import path from 'path';
import { createLogger } from '../../services/logging';
import {
  longBurnKey,
  onAccountAnalyticsSample,
  type AccountAnalyticsSampleEvent,
} from '../services/account-analytics-service';
import { getAacStore, type LongBurnSpan } from '../../store/store-service';
import {
  buildRollupExports,
  exportHour,
  SNAPSHOT_HISTORY_DAILY,
  SNAPSHOT_HISTORY_HOURLY,
  type RollupExportFile,
} from '../../store/usage-exports';
import { getAccountRefreshIntervalSeconds } from '../services/account-refresh-settings';
import {
  readSnapshotSettings,
  renderSnapshotLocations,
  SNAPSHOT_LOCATIONS,
  type SnapshotSettings,
} from './agent-usage-settings';
import { readPackagedSource } from '../../utils/packaged-source';
import {
  HISTORY_HOURS,
  RecentReadings,
  burnKey,
  usageHistory,
  type BurnRate,
  type HistorySeries,
} from './agent-usage-history';
import { projectAgentUsage, type AgentUsageWindow } from './agent-usage-projection';
import {
  renderSnapshotMarkdown,
  type AgentUsageSnapshot,
  type SnapshotAccount,
  type SnapshotSignInProfile,
  type SnapshotSignInRenewal,
} from './agent-usage-markdown';

const logger = createLogger('agent-usage-snapshot');

export * from './agent-usage-settings';

export const SNAPSHOT_JSON = 'usage.json';
export const SNAPSHOT_MARKDOWN = 'usage.md';
export const SNAPSHOT_HISTORY = 'history.json';
/** A write still running after this long is reported once in the dashboard log. */
const STALL_LOG_MS = 5 * 60_000;
/** The sign-in fetch waits at most this long inside a publish slot. */
const SIGNIN_FETCH_LIMIT_MS = 60_000;

export interface AgentUsageHistoryFile {
  version: 1;
  generatedAt: string;
  hours: number;
  /** Each point is [time, used percent, remaining amount, reset time]; null when that reading had none. */
  series: HistorySeries[];
}

/** The snapshot and history files for one sample. */
export function buildSnapshotFiles(
  event: AccountAnalyticsSampleEvent,
  refreshSeconds: number,
  recent: RecentReadings | null = null,
  signIn: SnapshotSignInProfile[] = [],
  signInRenewal: SnapshotSignInRenewal | null = null
): { snapshot: AgentUsageSnapshot; history: AgentUsageHistoryFile } {
  const now = event.sampledAt;
  const projected = projectAgentUsage(event.accounts, { now, includeHidden: true });
  const { rates, series } = usageHistory(
    event.accounts,
    projected.accounts,
    event.history,
    now,
    recent
  );
  // A pace says where usage is heading from now, so it is published only for a current reading: not
  // after a passed reset (the reading is history), not for an account that failed or needs a sign-in,
  // and not for a reading older than the snapshot's own stale limit (a retained or cached one).
  const freshMs = staleAfterSeconds(refreshSeconds) * 1000;
  const current = (account: { status: string }, window: { sampledAt: string | null }): boolean => {
    if (account.status !== 'ok' && account.status !== 'cached') return false;
    const sampled = Date.parse(window.sampledAt ?? '');
    return Number.isFinite(sampled) && now - sampled <= freshMs;
  };
  // With the history store, a current pace also carries the 7- and 30-day burn across resets.
  const longFor = (accountId: string, window: AgentUsageWindow): Partial<BurnRate> => {
    if (!event.longBurn) return {};
    const row = event.accounts.find((candidate) => candidate.id === accountId);
    const source = row?.windows?.find(
      (candidate) =>
        candidate.key === window.key &&
        (candidate.unit ?? null) === window.unit &&
        (candidate.poolId ?? null) === window.poolId
    );
    const long = row && source ? event.longBurn.get(longBurnKey(row, source)) : undefined;
    if (!long) return {};
    const rate = (span: LongBurnSpan): number | null => (span.coverage >= 0.5 ? span.rate : null);
    return {
      last7Days: rate(long.last7Days),
      last30Days: rate(long.last30Days),
      coverage7Days: long.last7Days.coverage,
      coverage30Days: long.last30Days.coverage,
    };
  };
  const accounts: SnapshotAccount[] = projected.accounts.map((account) => ({
    ...account,
    windows: account.windows.map((window) => {
      const rate =
        window.resetPassed || !current(account, window)
          ? null
          : (rates.get(account.id)?.get(burnKey(window.key, window.unit, window.poolId)) ?? null);
      return { ...window, burn: rate ? { ...rate, ...longFor(account.id, window) } : null };
    }),
  }));
  const generatedAt = new Date(now).toISOString();
  return {
    snapshot: {
      version: 1,
      source: 'AI Account Center',
      generatedAt,
      refreshSeconds,
      staleAfterSeconds: staleAfterSeconds(refreshSeconds),
      accounts,
      ...(signIn.length > 0 ? { signIn } : {}),
      ...(signInRenewal ? { signInRenewal } : {}),
    },
    history: { version: 1, generatedAt, hours: HISTORY_HOURS, series },
  };
}

/** Three missed samples, and never under 5 minutes. */
function staleAfterSeconds(refreshSeconds: number): number {
  return Math.max(300, refreshSeconds * 3);
}

/**
 * Writes `name` by renaming a new temporary file over it. The folder is shared, so the temporary
 * name is unpredictable and created exclusively (`wx`): a link or file planted under that name makes
 * the write fail instead of being followed and truncated. The rename replaces a planted link
 * itself, never its target.
 */
export async function writeAtomically(
  directory: string,
  name: string,
  body: string,
  mode = 0o644
): Promise<void> {
  const target = path.join(directory, name);
  const temporary = path.join(directory, `.${name}.${randomBytes(12).toString('hex')}.tmp`);
  const handle = await fs.promises.open(temporary, 'wx', mode);
  try {
    try {
      await handle.writeFile(body);
    } finally {
      await handle.close();
    }
    await fs.promises.rename(temporary, target);
  } catch (error) {
    await fs.promises.rm(temporary, { force: true }).catch(() => {});
    throw error;
  }
}

/**
 * The reader agents run and its guide, shipped in this package (`scripts/agent-usage`) and kept
 * current in the snapshot folder, so the reader always matches the snapshot format it reads.
 */
export const CLIENT_FILES: ReadonlyArray<{ name: string; mode: number; crlf: boolean }> = [
  { name: 'aac-usage', mode: 0o755, crlf: false },
  { name: 'aac-usage.cmd', mode: 0o644, crlf: true },
  { name: 'README.md', mode: 0o644, crlf: false },
];

export function clientSourceFolder(): string {
  return path.resolve(__dirname, '../../../scripts/agent-usage');
}

/**
 * The Claude sign-in renewal runner (`scripts/claude-signin`), kept current in the folder's
 * `claude-signin/` subfolder. The installed runner on the Mac copies itself from there on each run,
 * so a dashboard upgrade reaches it without anyone reinstalling it.
 */
export const RUNNER_FILES: ReadonlyArray<{ name: string; mode: number; crlf: boolean }> = [
  { name: 'claude_signin.py', mode: 0o755, crlf: false },
  { name: 'browser_auth.mjs', mode: 0o644, crlf: false },
  { name: 'README.md', mode: 0o644, crlf: false },
];
export const RUNNER_FOLDER = 'claude-signin';

export function runnerSourceFolder(): string {
  return path.resolve(__dirname, '../../../scripts/claude-signin');
}

async function syncFiles(
  directory: string,
  source: string,
  files: ReadonlyArray<{ name: string; mode: number; crlf: boolean }>
): Promise<void> {
  for (const file of files) {
    const lf = (await readPackagedSource(path.join(source, file.name))).toString('utf8');
    const body = file.crlf ? lf.replace(/\n/g, '\r\n') : lf;
    const current = await fs.promises
      .readFile(path.join(directory, file.name), 'utf8')
      .catch(() => null);
    if (current !== body) await writeAtomically(directory, file.name, body, file.mode);
  }
}

/** Copies each client file whose bytes differ; a file already current is left untouched. */
export async function syncSnapshotClient(
  directory: string,
  source = clientSourceFolder(),
  runnerSource: string | null = runnerSourceFolder()
): Promise<void> {
  await syncFiles(directory, source, CLIENT_FILES);
  if (!runnerSource) return;
  const runner = path.join(directory, RUNNER_FOLDER);
  const info = await fs.promises.lstat(runner).catch(() => null);
  if (info && (!info.isDirectory() || info.isSymbolicLink())) return;
  if (!info) await fs.promises.mkdir(runner, { mode: 0o755 });
  await syncFiles(runner, runnerSource, RUNNER_FILES);
}

/** Publication-time recheck, on copies: a live row whose deadline crossed
 * in (since, now] while collection and the folder checks were still
 * running must not publish as a future end, in either file. The collected
 * rows belong to the health service's cache and are never mutated here;
 * stamps already past at `since` are facts the collected state already
 * accounts for, exactly as in the service's own crossed-row downgrade. */
export function downgradeCrossedSignInRows(
  rows: SnapshotSignInProfile[],
  since: number,
  now: number
): SnapshotSignInProfile[] {
  return rows.map((row) => {
    if (row.state !== 'ok' && row.state !== 'warn' && row.state !== 'critical') return row;
    // The known login end counts crossed whenever it is already past at
    // `now` (a behind-clock remote), exactly as in the service; the
    // renewable stamps count only inside (since, now]. With no named
    // end, a refresh end already past is the same skew shape: no
    // single-clock collector emits it live, and a named future end
    // (the legitimate dying window) exempts it.
    const end = row.loggedOutAt ? Date.parse(row.loggedOutAt) : NaN;
    const refresh = row.refreshExpiresAt ? Date.parse(row.refreshExpiresAt) : NaN;
    const crossed =
      (!Number.isNaN(end) && end <= now) ||
      (Number.isNaN(end) && !Number.isNaN(refresh) && refresh <= now) ||
      [row.accessExpiresAt, row.refreshExpiresAt].some((value) => {
        if (!value) return false;
        const at = Date.parse(value);
        return !Number.isNaN(at) && at > since && at <= now;
      });
    if (!crossed) return row;
    return {
      ...row,
      state: 'unavailable',
      accessExpiresAt: null,
      refreshExpiresAt: null,
      loggedOutAt: null,
      error: 'sign-in state changed during collection; recheck pending',
    };
  });
}

export interface SnapshotPublisherDeps {
  settings?: () => Promise<SnapshotSettings | null>;
  write?: (directory: string, name: string, body: string) => Promise<void>;
  isDirectory?: (directory: string) => Promise<boolean>;
  refreshSeconds?: () => number;
  syncClient?: (directory: string) => Promise<void>;
  /** Sign-in expiry reports per machine; a failed fetch publishes a named collection-failed row. */
  signIn?: () => Promise<SnapshotSignInProfile[]>;
  /** The renewal runner's summary (claude-signins.json in the same folder); best effort. */
  signInRenewal?: () => Promise<SnapshotSignInRenewal | null>;
  /** The history store's hourly and daily rollup files, or null without the store. */
  rollupExports?: (
    event: AccountAnalyticsSampleEvent
  ) => Promise<{ hourly: RollupExportFile; daily: RollupExportFile } | null>;
}

/** The row a failed or timed-out sign-in fetch publishes: the section
 * stays and names the failed collection, instead of the absence of a
 * section reading as "no sign-in problems" on every machine. */
function signInCollectionFailedRow(): SnapshotSignInProfile {
  return {
    name: 'sign-in collection',
    provider: 'claude',
    platform: hostname(),
    machine: hostname(),
    accessExpiresAt: null,
    refreshExpiresAt: null,
    loggedOutAt: null,
    state: 'unavailable',
    fixCommand: '',
    error: 'sign-in collection failed',
  };
}

/** Handles one sample; resolves whether the files were written. */
export function createSnapshotPublisher(
  deps: SnapshotPublisherDeps = {}
): (event: AccountAnalyticsSampleEvent) => Promise<boolean> {
  const settings = deps.settings ?? (() => readSnapshotSettings());
  const write = deps.write ?? writeAtomically;
  const isDirectory =
    deps.isDirectory ??
    (async (directory: string) => (await fs.promises.stat(directory)).isDirectory());
  const refreshSeconds = deps.refreshSeconds ?? getAccountRefreshIntervalSeconds;
  const syncClient = deps.syncClient ?? ((directory: string) => syncSnapshotClient(directory));
  const signIn = deps.signIn ?? (async () => []);
  const signInRenewal = deps.signInRenewal ?? (async () => null);
  const rollupExports =
    deps.rollupExports ??
    (async (event: AccountAnalyticsSampleEvent) => {
      const store = getAacStore();
      if (!store) return null;
      const projected = projectAgentUsage(event.accounts, {
        now: event.sampledAt,
        includeHidden: true,
      });
      return buildRollupExports(store, event.accounts, projected.accounts, event.sampledAt);
    });
  let rollupsWritten: string | null = null;
  const recent = new RecentReadings();
  let busy = false;
  let busySince = 0;
  let stallLogged = false;
  let warned: string | null = null;
  let clientSynced: string | null = null;
  let locationsWritten: string | null = null;
  return async (event) => {
    // Every sample's readings count toward short-span pace, even one whose write is skipped.
    recent.add(event.accounts, event.sampledAt);
    if (busy) {
      // A write that never returns (a dead hard mount) holds the publisher; say so once. Starting
      // more writes would only tie up more of the server's file threads.
      if (!stallLogged && Date.now() - busySince > STALL_LOG_MS) {
        stallLogged = true;
        try {
          logger.warn(
            'agent_usage_snapshot.write_stalled',
            'An agent usage snapshot write has not finished; later samples wait for it'
          );
        } catch {
          /* Logging never stops the dashboard. */
        }
      }
      return false;
    }
    // The busy flag is held synchronously after the guard; the early returns
    // below release it through the finally.
    busy = true;
    busySince = Date.now();
    stallLogged = false;
    let directory = '';
    try {
      const chosen = await settings();
      // Off until a folder is set, and nothing to publish without accounts:
      // neither case may start the sign-in local reads and ssh probes below.
      if (!chosen) return false;
      directory = chosen.directory;
      // Until the dashboard has read any account, there is nothing true to publish.
      if (event.accounts.length === 0) return false;
      // The ssh-backed sign-in fetch starts before the heavier reads below,
      // so a slow remote overlaps them instead of extending the busy window.
      // A fetch that never returns is released at the limit: one lost
      // machine must not stop usage.json from updating for good.
      const signInStartedAt = Date.now();
      // A rejected or timed-out fetch is a collection failure, not an
      // empty result: it publishes as a named unavailable row below.
      const signInPending: Promise<{ rows: SnapshotSignInProfile[]; failed: boolean }> =
        Promise.race([
          Promise.resolve(signIn()).then(
            (rows) => ({ rows, failed: false }),
            () => ({ rows: [], failed: true })
          ),
          new Promise<{ rows: SnapshotSignInProfile[]; failed: boolean }>((resolve) => {
            const timer = setTimeout(
              () => resolve({ rows: [], failed: true }),
              SIGNIN_FETCH_LIMIT_MS
            );
            timer.unref?.();
          }),
        ]);
      const renewalPending: Promise<SnapshotSignInRenewal | null> = Promise.resolve(
        signInRenewal()
      ).catch(() => null);
      if (!(await isDirectory(directory)))
        throw new ValidationError('The usage snapshot folder is not a folder.');
      const fetched = await signInPending;
      const signInProfiles = fetched.failed ? [signInCollectionFailedRow()] : fetched.rows;
      const { snapshot, history } = buildSnapshotFiles(
        event,
        refreshSeconds(),
        recent,
        signInProfiles,
        await renewalPending
      );
      await write(directory, SNAPSHOT_HISTORY, `${JSON.stringify(history)}\n`);
      // Rechecked against publication time, not the sample's, as late as
      // the writes allow: a deadline that crossed while the fetch, the
      // folder check, the history write or either remaining write itself
      // was still running publishes as awaiting-recheck in both files.
      // Downgrades only ever accumulate, so rechecking after each write
      // — the JSON write included — converges: a crossing during one
      // write retires at least one more live row, both files are
      // rewritten, and the pass ends only when a full markdown+JSON
      // pair lands with the rows stable across both writes.
      if (snapshot.signIn)
        snapshot.signIn = downgradeCrossedSignInRows(snapshot.signIn, signInStartedAt, Date.now());
      // usage.json is the commit point of each pass — the last write,
      // whose time says every file of this sample is in place. If a
      // correction pass fails after it landed (a share dying
      // mid-publish), one best-effort JSON rewrite still carries the
      // latest downgraded rows: machines read the JSON, and it must
      // not keep an expired login published as live. The failure
      // itself still propagates.
      let jsonCommitted = false;
      try {
        for (;;) {
          await write(directory, SNAPSHOT_MARKDOWN, renderSnapshotMarkdown(snapshot, Date.now()));
          if (snapshot.signIn) {
            const rechecked = downgradeCrossedSignInRows(
              snapshot.signIn,
              signInStartedAt,
              Date.now()
            );
            if (!rechecked.every((row, index) => row === snapshot.signIn?.[index])) {
              snapshot.signIn = rechecked;
              continue;
            }
          }
          await write(directory, SNAPSHOT_JSON, `${JSON.stringify(snapshot, null, 2)}\n`);
          jsonCommitted = true;
          if (!snapshot.signIn) break;
          const rechecked = downgradeCrossedSignInRows(
            snapshot.signIn,
            signInStartedAt,
            Date.now()
          );
          if (rechecked.every((row, index) => row === snapshot.signIn?.[index])) break;
          snapshot.signIn = rechecked;
        }
      } catch (error) {
        if (jsonCommitted) {
          try {
            if (snapshot.signIn)
              snapshot.signIn = downgradeCrossedSignInRows(
                snapshot.signIn,
                signInStartedAt,
                Date.now()
              );
            await write(directory, SNAPSHOT_JSON, `${JSON.stringify(snapshot, null, 2)}\n`);
          } catch {
            /* The original failure is the one that reports. */
          }
        }
        throw error;
      }
      warned = null;
      // The store's hourly and daily files, once an hour (after usage.json, which commits a pass).
      const hourKey = `${directory}\n${exportHour(event.sampledAt)}`;
      if (rollupsWritten !== hourKey) {
        try {
          const files = await rollupExports(event);
          if (files) {
            await write(directory, SNAPSHOT_HISTORY_HOURLY, `${JSON.stringify(files.hourly)}\n`);
            await write(directory, SNAPSHOT_HISTORY_DAILY, `${JSON.stringify(files.daily)}\n`);
          }
          rollupsWritten = hourKey;
        } catch {
          /* The snapshot itself was written; the next sample retries. */
        }
      }
      // The setting can change without a restart, so this follows it; unchanged, it is not rewritten.
      const locations = renderSnapshotLocations(chosen.locations);
      if (locationsWritten !== `${directory}\n${locations}`) {
        try {
          await write(directory, SNAPSHOT_LOCATIONS, locations);
          locationsWritten = `${directory}\n${locations}`;
        } catch {
          /* The snapshot itself was written; the next sample retries. */
        }
      }
      if (clientSynced !== directory) {
        // Once per folder and dashboard start; a failed copy is retried after the next sample.
        try {
          await syncClient(directory);
          clientSynced = directory;
        } catch {
          /* The snapshot itself was written. */
        }
      }
      return true;
    } catch {
      if (warned !== directory) {
        warned = directory;
        try {
          logger.warn(
            'agent_usage_snapshot.write_failed',
            'The agent usage snapshot could not be written',
            { directory }
          );
        } catch {
          /* Logging never stops the dashboard. */
        }
      }
      return false;
    } finally {
      busy = false;
    }
  };
}

/** Subscribes the snapshot to every analytics sample; returns the unsubscribe call. */
export function startAgentUsageSnapshot(deps: SnapshotPublisherDeps = {}): () => void {
  return onAccountAnalyticsSample(createSnapshotPublisher(deps));
}
