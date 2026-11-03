import { execFile } from 'child_process';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import type { SnapshotSignInProfile } from '../agent-usage/agent-usage-snapshot';
import {
  configuredUsageHosts,
  localUsagePython,
  signinHelperBundle,
  usageHelperCommand,
} from './usage-failover-transport';

/**
 * Sign-in expiry health for every Claude OAuth login on the machines AAC
 * collects from. The collector (scripts/account-usage/signin_expiry.py) reads
 * only the two OAuth times and token presence from each config dir — token
 * values are never read or transported. Remote machines run the collector
 * from an in-memory stdin bundle, so nothing is ever installed on their
 * disks; the hosts are the same configured ones every other usage transport
 * uses. Results are cached for at most five minutes, never past a published
 * deadline: a stamp that crosses after collection drops the cache on read
 * (deadlines under the 30 s floor age out with the floor), while stamps
 * already past at collection are facts about the past and keep the cache.
 */

export interface SignInProfile extends SnapshotSignInProfile {
  /** The machine this report came from; present on every published row. */
  machine: string;
  /** A failed or unreachable host names itself instead of disappearing. */
  error?: string;
}

export interface SignInHealth {
  generatedAt: string;
  machines: Record<string, SignInProfile[]>;
}

const CACHE_MIN_MS = 30_000;
const CACHE_MAX_MS = 5 * 60 * 1000;
/** Each host's collection settles inside this; the publisher's slot is longer. */
const HOST_LIMIT_MS = 45_000;
const COLLECTOR = 'signin_expiry.py';
/** Accepted as-is; anything else is sanitized to a safe bounded identifier. */
const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.@+ -]{0,63}$/;
/** A published timestamp must parse as a real calendar time; anything else reads as absent. */
const STAMP_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;

function runCollector(
  command: string,
  args: string[],
  timeoutMs: number,
  input?: string
): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = execFile(
      command,
      args,
      { timeout: timeoutMs, maxBuffer: 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error && stdout.length === 0) {
          // The cause is whatever the process said — never the command line
          // (it names the ssh target) and never a Node error string.
          const cause = stderr ? stderr.replace(/\s+/g, ' ').trim().slice(0, 200) : '';
          reject(new Error(cause || 'collector failed'));
          return;
        }
        resolve(stdout);
      }
    );
    child.on('error', () => {
      /* handled through the empty-stdout branch above */
    });
    if (input !== undefined) {
      child.stdin?.on('error', () => {
        /* ssh exit is handled without remote text */
      });
      child.stdin?.end(input);
    }
  });
}

/** A safe bounded identifier: an unreadable name never deletes the row. */
export function sanitizeName(raw: unknown): string {
  if (typeof raw !== 'string') return 'unnamed';
  const cleaned = raw
    .replace(/[^\w.@+ -]/g, '_')
    .trim()
    .slice(0, 64);
  return cleaned || 'unnamed';
}

function stamp(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > 40 || !STAMP_PATTERN.test(value)) return null;
  const at = Date.parse(value);
  if (Number.isNaN(at)) return null;
  // Calendar, not just shape: 2026-02-30 parses nowhere but must not publish.
  if (value.endsWith('Z') && new Date(at).toISOString().slice(0, 10) !== value.slice(0, 10))
    return null;
  return value;
}

/** A repair command that survives publication byte-exact: single-line, and
 * never truncated mid-quote. A command containing a backtick cannot live in
 * a markdown code span without changing what it does, so — like an overlong
 * one — it is omitted entirely: half or altered commands are worse than none. */
export function publishableFixCommand(value: unknown): string {
  if (typeof value !== 'string') return '';
  const cleaned = value.replace(/[\n\r]/g, ' ');
  return cleaned.includes('`') || cleaned.length > 200 ? '' : cleaned;
}

/** The published DTO is allowlisted; a changed helper cannot add fields or
 * break a row past its bounds — bounds clip, they never delete. */
export function allowProfile(raw: unknown, machine: string): SignInProfile | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const row = raw as Record<string, unknown>;
  const states = ['ok', 'warn', 'critical', 'logged_out', 'unavailable', 'unsupported'];
  if (typeof row.state !== 'string' || !states.includes(row.state)) return null;
  const provider = row.provider === 'codex' ? 'codex' : 'claude';
  const configDir = typeof row.configDir === 'string' ? row.configDir.slice(0, 200) : undefined;
  const profile: SignInProfile = {
    name:
      typeof row.name === 'string' && NAME_PATTERN.test(row.name)
        ? row.name
        : sanitizeName(row.name),
    provider,
    platform: machine,
    machine,
    configDir,
    accessExpiresAt: stamp(row.accessExpiresAt),
    refreshExpiresAt: stamp(row.refreshExpiresAt),
    loggedOutAt: stamp(row.loggedOutAt),
    state: row.state as SignInProfile['state'],
    fixCommand: publishableFixCommand(row.fixCommand),
    ...(row.state === 'unavailable' && typeof row.error === 'string' && row.error !== ''
      ? { error: row.error.replace(/\s+/g, ' ').trim().slice(0, 120) }
      : {}),
    ...(row.state === 'unsupported' && Array.isArray(row.fieldsAvailable)
      ? {
          fieldsAvailable: row.fieldsAvailable
            .filter((f): f is string => typeof f === 'string' && NAME_PATTERN.test(f))
            .slice(0, 32),
        }
      : {}),
  };
  return profile;
}

function unavailableHost(machine: string, error: string): SignInProfile[] {
  return [
    {
      name: machine,
      provider: 'claude',
      platform: machine,
      machine,
      state: 'unavailable',
      fixCommand: '',
      error: error.replace(/\s+/g, ' ').trim().slice(0, 120),
    },
  ];
}

async function collectLocal(): Promise<string> {
  // The packaged helper, resolved from this module like the sibling usage
  // transports, run by this host's own installed python when it has one,
  // scoped to the dashboard's configured CCS directory (CCS_HOME and
  // --config-dir installs keep their saved-instance homes and venv).
  const helper = path.resolve(__dirname, '../../../scripts/account-usage', COLLECTOR);
  const ccsDir = getCcsDir();
  return runCollector(localUsagePython({ ccsDir }), [helper, '--ccs-dir', ccsDir], 20_000);
}

type UsageHostEntry = Awaited<ReturnType<typeof configuredUsageHosts>>[number];

async function collectRemote(host: UsageHostEntry): Promise<string> {
  // The collector runs from an in-memory stdin bundle: helper source never
  // lands on the remote disk, and a clean host reports instead of failing.
  const bundle = await signinHelperBundle();
  return runCollector(
    'ssh',
    [
      '-T',
      '-o',
      'BatchMode=yes',
      '-o',
      'ConnectTimeout=5',
      '-o',
      'ConnectionAttempts=1',
      '-o',
      'ServerAliveInterval=5',
      '-o',
      'ServerAliveCountMax=1',
      '--',
      host.sshHost as string,
      usageHelperCommand(host.platform),
    ],
    30_000,
    JSON.stringify({ request: {}, helperSource: bundle })
  );
}

export function parseRows(stdout: string, machine: string): SignInProfile[] {
  try {
    const value = JSON.parse(stdout);
    if (value && value.schemaVersion === 1 && Array.isArray(value.profiles)) {
      return value.profiles
        .map((row: unknown) => allowProfile(row, machine))
        .filter((row: SignInProfile | null): row is SignInProfile => row !== null);
    }
  } catch {
    /* a partial or unreadable answer reads as unavailable */
  }
  return [];
}

interface CacheEntry {
  /** When collection started: only deadlines crossing after this moment
   * invalidate the cache — stamps already past at collection are facts
   * about the past (an idle login's expired access token, a logged-out
   * row's end), not news, and must not force a recollection every read. */
  collectedAt: number;
  until: number;
  health: Promise<SignInHealth>;
}

let cache: CacheEntry | null = null;

export function hasPassedDeadline(health: SignInHealth, since: number, now: number): boolean {
  for (const rows of Object.values(health.machines)) {
    for (const row of rows) {
      for (const value of [row.accessExpiresAt, row.refreshExpiresAt, row.loggedOutAt]) {
        if (!value) continue;
        const at = Date.parse(value);
        if (Number.isNaN(at) || at <= since || at > now) continue;
        return true;
      }
    }
  }
  return false;
}

/** The earliest future state deadline across a report, or null. */
export function nextDeadlineAt(health: SignInHealth, nowMs: number): number | null {
  let earliest: number | null = null;
  for (const rows of Object.values(health.machines)) {
    for (const row of rows) {
      for (const value of [row.accessExpiresAt, row.refreshExpiresAt, row.loggedOutAt]) {
        if (!value) continue;
        const at = Date.parse(value);
        if (Number.isNaN(at) || at <= nowMs) continue;
        if (earliest === null || at < earliest) earliest = at;
      }
    }
  }
  return earliest;
}

/** Cache until the next published deadline — a cached state must not
 * outlive the time it names — but never shorter than the floor; a deadline
 * under the floor is caught by the crossed-deadline check on read instead. */
export function cacheTtlFor(health: SignInHealth, nowMs: number): number {
  const deadline = nextDeadlineAt(health, nowMs);
  if (deadline === null) return CACHE_MAX_MS;
  return Math.max(CACHE_MIN_MS, Math.min(CACHE_MAX_MS, deadline - nowMs));
}

export function getSignInHealth(): Promise<SignInHealth> {
  const now = Date.now();
  if (cache && now < cache.until) {
    // A deadline that crossed after collection — including one that passed
    // while its own collection was still running — drops the cache on read.
    const entry = cache;
    return entry.health.then((value) => {
      if (!hasPassedDeadline(value, entry.collectedAt, Date.now())) return value;
      cache = null;
      return refresh();
    });
  }
  return refresh();
}

/** One collection pass. Hosts race their own settlement deadline, so a
 * single wedged machine cannot hold allSettled past the publisher's slot
 * and wipe successful machines' rows with it. */
async function collectOnce(): Promise<SignInHealth> {
  const machines: Record<string, SignInProfile[]> = {};
  const hosts = await configuredUsageHosts();
  const results = await Promise.allSettled(
    hosts.map((host) => {
      const runner = host.sshHost ? collectRemote(host) : collectLocal();
      return new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('collection timed out')), HOST_LIMIT_MS);
        timer.unref?.();
        runner.then(
          (value) => {
            clearTimeout(timer);
            resolve(value);
          },
          (error) => {
            clearTimeout(timer);
            reject(error instanceof Error ? error : new Error(String(error)));
          }
        );
      });
    })
  );
  hosts.forEach((host, index) => {
    const result = results[index];
    if (result.status === 'fulfilled') {
      const rows = parseRows(result.value, host.host);
      if (rows.length > 0) {
        machines[host.host] = rows;
        return;
      }
    }
    let reason =
      result.status === 'rejected'
        ? result.reason instanceof Error
          ? result.reason.message
          : String(result.reason)
        : 'no profiles reported';
    // The ssh target's alias or address is our routing detail, not the
    // machine's story; it never reaches the published error.
    if (host.sshHost) reason = reason.split(host.sshHost).join(host.host);
    machines[host.host] = unavailableHost(host.host, reason);
  });
  if (!machines.ubuntu) {
    // A dashboard hosted on the Mac or Windows collects no Ubuntu route:
    // the machine names itself rather than silently disappearing.
    machines.ubuntu = unavailableHost('ubuntu', 'no collection route from this host');
  }
  return { generatedAt: new Date().toISOString(), machines };
}

function refresh(): Promise<SignInHealth> {
  const now = Date.now();
  const entry: CacheEntry = {
    collectedAt: now,
    until: now + CACHE_MAX_MS,
    health: undefined as unknown as Promise<SignInHealth>,
  };
  entry.health = (async (): Promise<SignInHealth> => {
    let value: SignInHealth;
    try {
      value = await collectOnce();
      // A deadline crossed while the hosts were still collecting must not
      // reach the first caller: one bounded revalidation pass, then the
      // report stands as collected.
      if (hasPassedDeadline(value, entry.collectedAt, Date.now())) {
        value = await collectOnce();
      }
    } catch (error) {
      // Host resolution is best effort by contract: the section simply
      // stays absent, and the retry waits only the floor.
      void error;
      entry.until = Date.now() + CACHE_MIN_MS;
      return { generatedAt: new Date().toISOString(), machines: {} };
    }
    entry.until = Date.now() + cacheTtlFor(value, Date.now());
    return value;
  })();
  cache = entry;
  return entry.health;
}

export function invalidateSignInHealthCache(): void {
  cache = null;
}
