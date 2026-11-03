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
import { ValidationError } from '../../errors/error-types';
import path from 'path';
import { createLogger } from '../../services/logging';
import {
  onAccountAnalyticsSample,
  type AccountAnalyticsSampleEvent,
} from '../services/account-analytics-service';
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
import {
  projectAgentUsage,
  type AgentUsageAccount,
  type AgentUsageWindow,
} from './agent-usage-projection';

const logger = createLogger('agent-usage-snapshot');

export * from './agent-usage-settings';

export const SNAPSHOT_JSON = 'usage.json';
export const SNAPSHOT_MARKDOWN = 'usage.md';
export const SNAPSHOT_HISTORY = 'history.json';
/** A write still running after this long is reported once in the dashboard log. */
const STALL_LOG_MS = 5 * 60_000;

export interface SnapshotWindow extends AgentUsageWindow {
  burn: BurnRate | null;
}

export interface SnapshotAccount extends Omit<AgentUsageAccount, 'windows'> {
  windows: SnapshotWindow[];
}

/** One machine's Claude sign-in expiry report (see scripts/account-usage/signin_expiry.py). */
export interface SnapshotSignInProfile {
  name: string;
  provider: string;
  platform: string;
  /** The machine this report came from; set by the health service. */
  machine?: string;
  configDir?: string;
  accessExpiresAt?: string | null;
  refreshExpiresAt?: string | null;
  /** The first access expiry that falls after the refresh expiry; after this the login is dead. */
  loggedOutAt?: string | null;
  state: 'ok' | 'warn' | 'critical' | 'logged_out' | 'unavailable' | 'unsupported';
  fixCommand: string;
  /** Why a machine's report is unavailable; absent otherwise. */
  error?: string;
  /** The account this sign-in belongs to (from the renewal runner). */
  email?: string | null;
  /** Renewal runner fields: whether it renews by itself, from when, and how the last one went. */
  autoRenew?: boolean;
  renewFrom?: string | null;
  lastRenewAt?: string | null;
  lastResult?: string | null;
  nextVerifyAt?: string | null;
  reverts?: number;
}

/** The renewal runner's summary: who it is, whether it is current and what needs a person. */
export interface SnapshotSignInRenewal {
  runner: string;
  generatedAt: string;
  stale: boolean;
  needsYou: Array<{ email: string; reason: string; how: string; since: string | null }>;
}

export interface AgentUsageSnapshot {
  version: 1;
  source: 'AI Account Center';
  generatedAt: string;
  /** How often the dashboard samples, so a reader can tell a late snapshot. */
  refreshSeconds: number;
  /** Older than this, the dashboard has stopped writing: treat the readings as stale. */
  staleAfterSeconds: number;
  accounts: SnapshotAccount[];
  /** Claude sign-in expiry per machine, worst first in the markdown; absent until first collected. */
  signIn?: SnapshotSignInProfile[];
  /** The sign-in renewal runner's summary; absent when no runner publishes. */
  signInRenewal?: SnapshotSignInRenewal;
}

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
  const accounts: SnapshotAccount[] = projected.accounts.map((account) => ({
    ...account,
    windows: account.windows.map((window) => ({
      ...window,
      burn:
        window.resetPassed || !current(account, window)
          ? null
          : (rates.get(account.id)?.get(burnKey(window.key, window.unit, window.poolId)) ?? null),
    })),
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

function utc(iso: string): string {
  return iso.replace('T', ' ').replace(/:\d\d(\.\d+)?Z$/, ' UTC');
}

/** "2h 35m", "3d 4h", "45s"; for a time in the past, "now". */
export function span(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return 'now';
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return `${Math.floor(seconds)}s`;
}

function amount(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(2);
}

function burnText(
  burn: BurnRate | null,
  now: number,
  usedPercent: number | null = null
): string | null {
  if (!burn || burn.pace === null || burn.paceMinutes === null) return null;
  const over =
    burn.paceMinutes < 60 ? `${burn.paceMinutes} min` : `${Math.round(burn.paceMinutes / 60)} h`;
  const hour =
    burn.paceMinutes < 60 && burn.lastHour !== null ? `; ${amount(burn.lastHour)}%/h over 1 h` : '';
  const pace = `${amount(burn.pace)}%/h over the last ${over}${hour}`;
  if (burn.pace < 0) return `usage falling ${pace}`;
  if (!burn.projectedFullAt) return `burning ${pace}`;
  if (Date.parse(burn.projectedFullAt) <= now) {
    // "At its limit" is a measured 100%; a projection that has just come due is not.
    if (usedPercent !== null && usedPercent >= 100)
      return `at its limit${burn.runsOutBeforeReset === true ? ' until the reset' : ''} (${pace})`;
    return `reaching its limit about now at this pace (${pace})`;
  }
  const full = `full in ${span((Date.parse(burn.projectedFullAt) - now) / 1000)}`;
  const reset =
    burn.runsOutBeforeReset === true
      ? ' (before the reset)'
      : burn.runsOutBeforeReset === false
        ? ' (after the reset)'
        : '';
  return `burning ${pace}, ${full}${reset}`;
}

function windowText(window: SnapshotWindow, now: number): string {
  const parts: string[] = [];
  if (window.unlimited === true) parts.push('unlimited');
  else if (window.usedPercent !== null) {
    const left =
      window.remainingPercent !== null ? `, ${amount(window.remainingPercent)}% left` : '';
    parts.push(`${amount(window.usedPercent)}% used${left}`);
  }
  const unit = window.unit ? ` ${window.unit}` : '';
  if (window.remaining !== null && window.limit !== null)
    parts.push(`${amount(window.remaining)} of ${amount(window.limit)}${unit} left`);
  else if (window.remaining !== null) parts.push(`${amount(window.remaining)}${unit} left`);
  else if (window.used !== null && window.limit !== null)
    parts.push(`${amount(window.used)} of ${amount(window.limit)}${unit} used`);
  else if (window.used !== null) parts.push(`${amount(window.used)}${unit} used`);
  if (window.resetAt)
    parts.push(
      window.resetPassed
        ? `reset at ${utc(window.resetAt)} already passed, new reading pending`
        : `resets ${utc(window.resetAt)} (in ${span((Date.parse(window.resetAt) - now) / 1000)})`
    );
  if (window.expiresAt) parts.push(`expires ${utc(window.expiresAt)}`);
  if (!window.resetPassed) {
    const burn = burnText(window.burn, now, window.usedPercent);
    if (burn) parts.push(burn);
  }
  if (parts.length === 0) parts.push('no number reported');
  if (window.cached)
    parts.push(
      window.sampledAt
        ? `earlier reading from ${span((now - Date.parse(window.sampledAt)) / 1000)} ago`
        : 'earlier reading, time unknown'
    );
  const pool = window.pool ? ` [${window.pool}]` : '';
  return `${window.label}${pool}: ${parts.join(', ')}`;
}

const STATUS_WORDS: Record<AgentUsageAccount['status'], string> = {
  ok: 'current',
  cached: 'cached earlier reading',
  unavailable: 'no current reading',
  error: 'reading failed',
  needs_sign_in: 'needs sign-in',
};

function hostName(account: AgentUsageAccount): string {
  return { ubuntu: 'Ubuntu VM', nas1: 'Nas1', mac: 'Mac', windows: 'Windows' }[account.host];
}

/** The same snapshot in plain words, one provider section and one line per window. */
const SIGNIN_STATE_WORDS: Record<string, string> = {
  ok: 'healthy',
  warn: 'ends within 24 hours',
  critical: 'ends within 4 hours',
  logged_out: 'already logged out',
  unavailable: 'not readable on this machine',
  unsupported: 'no expiry published',
};

/** The sign-in section, worst state first; a machine with nothing readable still names itself. */
function renderSignInMarkdown(
  machines: { machines?: SnapshotSignInProfile[] },
  now: number
): string[] {
  const lines: string[] = [];
  const profiles = (machines.machines ?? []).map((row) => ({
    ...row,
    machine: row.machine ?? row.platform ?? 'unknown',
  }));
  const rank: Record<string, number> = {
    logged_out: 0,
    critical: 1,
    warn: 2,
    unavailable: 3,
    unsupported: 4,
    ok: 5,
  };
  profiles.sort((a, b) => (rank[a.state] ?? 9) - (rank[b.state] ?? 9));
  for (const row of profiles) {
    const word = SIGNIN_STATE_WORDS[row.state] ?? row.state;
    // The published end is a bound, not always the exact time: when the
    // collector could not name one, the wording says the window closes soon
    // instead of inventing a time. The fix text is state-aware.
    const detail = (): string => {
      if (row.state === 'unavailable')
        return `not readable on this machine${row.error ? ` (${row.error})` : ''}`;
      if (row.state === 'unsupported') return `no login end time is published by ${row.provider}`;
      if (row.autoRenew && row.state !== 'logged_out')
        return [
          row.refreshExpiresAt ? `sign-in ends ${utc(row.refreshExpiresAt)}` : 'sign-in end not published',
          row.renewFrom ? `renews by itself from ${utc(row.renewFrom)}` : 'renews by itself',
          row.lastResult === 'renewed' && row.lastRenewAt ? `last renewed ${utc(row.lastRenewAt)}` : '',
          row.lastResult && row.lastResult !== 'renewed' ? `last attempt: ${row.lastResult.replace(/_/g, ' ')}` : '',
        ]
          .filter(Boolean)
          .join('; ');
      if (row.autoRenew) return 'logged out; the renewal runner signs it in again on its next run';
      if (row.loggedOutAt)
        return `sign-in ends ${utc(row.loggedOutAt)}${row.fixCommand ? `; re-login before then: \`${row.fixCommand}\`` : ''}`;
      return 're-login window closes soon; no exact end published';
    };
    const who = row.email ? ` (${row.email})` : '';
    lines.push(`- **${row.provider} · ${row.machine}/${row.name}**${who}: ${word}. ${detail()}`);
  }
  void now;
  return lines;
}

/** Who keeps the sign-ins alive, whether it is running, and what only a person can do. */
function renderRenewalMarkdown(renewal: SnapshotSignInRenewal | undefined, now: number): string[] {
  if (!renewal) return [];
  const lines: string[] = [];
  if (renewal.stale)
    lines.push(
      `**The sign-in renewal runner on ${renewal.runner} last reported ${utc(renewal.generatedAt)}; ` +
        'renewals are not happening until it runs again.**',
      ''
    );
  else
    lines.push(
      `Claude Code sign-ins renew by themselves (runner on ${renewal.runner}, ` +
        `last run ${span(Math.max(0, Math.round((now - Date.parse(renewal.generatedAt)) / 1000)))} ago).`,
      ''
    );
  for (const need of renewal.needsYou)
    lines.push(
      `- **NEEDS YOU · ${need.email}**: ${need.how || need.reason.replace(/_/g, ' ')}` +
        (need.since ? ` (since ${utc(need.since)})` : '')
    );
  if (renewal.needsYou.length > 0) lines.push('');
  return lines;
}

export function renderSnapshotMarkdown(snapshot: AgentUsageSnapshot): string {
  const now = Date.parse(snapshot.generatedAt);
  const lines = [
    '# AI Account Center usage',
    '',
    `Snapshot ${utc(snapshot.generatedAt)}. The AI Account Center dashboard rewrites it about every ` +
      `${span(snapshot.refreshSeconds)}; if it is more than ${span(snapshot.staleAfterSeconds)} old, ` +
      "the dashboard has stopped updating it. Percentages and amounts are the providers' own; a " +
      'missing number was not reported, not zero. Burn rates come from measured readings since ' +
      'the window last reset. Machine-readable: usage.json, 7-day readings: history.json, or run ' +
      'aac-usage in this folder.',
    '',
  ];
  const visible = snapshot.accounts.filter((account) => !account.hidden);
  if (visible.length === 0) lines.push('No account readings yet.', '');
  let provider: string | null = null;
  for (const account of visible) {
    if (account.provider !== provider) {
      provider = account.provider;
      lines.push(`## ${account.providerLabel}`, '');
    }
    const who = [
      account.email && account.email !== account.label ? account.email : null,
      account.profile && account.profile !== account.label ? `profile ${account.profile}` : null,
    ].filter((part): part is string => !!part);
    const facts = [account.plan, hostName(account), account.active ? 'active' : null].filter(
      (fact): fact is string => !!fact
    );
    const age =
      account.ageSeconds === null
        ? 'sample time unknown'
        : account.ageSeconds < 1
          ? 'sampled just now'
          : `sampled ${span(account.ageSeconds)} ago`;
    lines.push(
      `- **${account.label}**${who.length ? ` (${who.join(', ')})` : ''}, ${facts.join(', ')}: ` +
        `${STATUS_WORDS[account.status]}, ${age}`
    );
    if (account.status === 'ok' || account.status === 'cached')
      for (const window of account.windows) lines.push(`  - ${windowText(window, now)}`);
    lines.push('');
  }
  const signInProfiles = snapshot.signIn ?? [];
  if (signInProfiles.length > 0) {
    lines.push('');
    lines.push('## Sign-in health');
    lines.push('');
    lines.push(...renderRenewalMarkdown(snapshot.signInRenewal, now));
    lines.push(
      ...renderSignInMarkdown(
        { machines: signInProfiles },
        snapshot.generatedAt ? Date.parse(snapshot.generatedAt) : Date.now()
      )
    );
    lines.push('');
  }
  return `${lines.join('\n').trimEnd()}\n`;
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

export interface SnapshotPublisherDeps {
  settings?: () => Promise<SnapshotSettings | null>;
  write?: (directory: string, name: string, body: string) => Promise<void>;
  isDirectory?: (directory: string) => Promise<boolean>;
  refreshSeconds?: () => number;
  syncClient?: (directory: string) => Promise<void>;
  /** Sign-in expiry reports per machine; best effort, a failure publishes without them. */
  signIn?: () => Promise<SnapshotSignInProfile[]>;
  /** The renewal runner's summary (claude-signins.json in the same folder); best effort. */
  signInRenewal?: () => Promise<SnapshotSignInRenewal | null>;
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
    // The ssh-backed sign-in fetch starts before the busy guard so a slow
    // remote never holds a sample's publish slot.
    const signInPending: Promise<SnapshotSignInProfile[]> = Promise.resolve(signIn()).catch(
      () => []
    );
    const renewalPending: Promise<SnapshotSignInRenewal | null> = Promise.resolve(
      signInRenewal()
    ).catch(() => null);
    busy = true;
    busySince = Date.now();
    stallLogged = false;
    let directory = '';
    try {
      const chosen = await settings();
      if (!chosen) return false;
      directory = chosen.directory;
      // Until the dashboard has read any account, there is nothing true to publish.
      if (event.accounts.length === 0) return false;
      if (!(await isDirectory(directory)))
        throw new ValidationError('The usage snapshot folder is not a folder.');
      const signInProfiles = await signInPending;
      const { snapshot, history } = buildSnapshotFiles(
        event,
        refreshSeconds(),
        recent,
        signInProfiles,
        await renewalPending
      );
      await write(directory, SNAPSHOT_HISTORY, `${JSON.stringify(history)}\n`);
      await write(directory, SNAPSHOT_MARKDOWN, renderSnapshotMarkdown(snapshot));
      // usage.json last: its time says every file of this sample is in place.
      await write(directory, SNAPSHOT_JSON, `${JSON.stringify(snapshot, null, 2)}\n`);
      warned = null;
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
