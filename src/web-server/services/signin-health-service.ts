import { execFile } from 'child_process';
import * as path from 'path';
import type { SnapshotSignInProfile } from '../agent-usage/agent-usage-snapshot';
import {
  configuredUsageHosts,
  localUsagePython,
  signinHelperBundle,
  signinHelperCommand,
} from './usage-failover-transport';

/**
 * Sign-in expiry health for every Claude OAuth login on the machines AAC
 * collects from. The collector (scripts/account-usage/signin_expiry.py) reads
 * only the two OAuth times and token presence from each config dir — token
 * values are never read or transported. Remote machines run the collector
 * from an in-memory stdin bundle, so nothing is ever installed on their
 * disks; the hosts are the same configured ones every other usage transport
 * uses. Results are cached until the next state deadline or five minutes,
 * whichever comes first: the underlying times move in hours, not seconds,
 * but a cached "critical" must not outlive the deadline it names.
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
const COLLECTOR = 'signin_expiry.py';
/** Accepted as-is; anything else is sanitized to a safe bounded identifier. */
const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.@+ -]{0,63}$/;
/** A published timestamp must look like one; anything else reads as absent. */
const STAMP_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;

let cache: { until: number; health: Promise<SignInHealth> } | null = null;

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
          // The command line is noise; the cause is whatever the process said.
          const cause = (stderr || error.message).replace(/\s+/g, ' ').trim();
          reject(new Error(cause.slice(0, 200) || 'collector failed'));
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
  return typeof value === 'string' && value.length <= 40 && STAMP_PATTERN.test(value)
    ? value
    : null;
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
  const fixCommand = typeof row.fixCommand === 'string' ? row.fixCommand.slice(0, 200) : '';
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
    fixCommand,
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
  // transports, run by the host's own installed python when it has one.
  const helper = path.resolve(__dirname, '../../../scripts/account-usage', COLLECTOR);
  return runCollector(localUsagePython({}), [helper], 20_000);
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
      signinHelperCommand(host.platform),
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

/** Cache until the next deadline — a cached critical must not outlive the
 * end time it publishes — but never shorter than the floor. */
export function cacheTtlFor(health: SignInHealth, nowMs: number): number {
  const deadline = nextDeadlineAt(health, nowMs);
  if (deadline === null) return CACHE_MAX_MS;
  return Math.max(CACHE_MIN_MS, Math.min(CACHE_MAX_MS, deadline - nowMs));
}

export function getSignInHealth(): Promise<SignInHealth> {
  const now = Date.now();
  if (cache && now < cache.until) return cache.health;
  const entry: { until: number; health: Promise<SignInHealth> } = {
    until: now + CACHE_MAX_MS,
    health: undefined as unknown as Promise<SignInHealth>,
  };
  entry.health = (async (): Promise<SignInHealth> => {
    const machines: Record<string, SignInProfile[]> = {};
    try {
      const hosts = await configuredUsageHosts();
      const results = await Promise.allSettled(
        hosts.map((host) => (host.sshHost ? collectRemote(host) : collectLocal()))
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
        const reason =
          result.status === 'rejected'
            ? result.reason instanceof Error
              ? result.reason.message
              : String(result.reason)
            : 'no profiles reported';
        machines[host.host] = unavailableHost(host.host, reason);
      });
    } catch (error) {
      // Host resolution is best effort by contract: the section simply
      // stays absent, and the retry waits only the floor.
      void error;
      entry.until = Date.now() + CACHE_MIN_MS;
      return { generatedAt: new Date().toISOString(), machines };
    }
    entry.until = Date.now() + cacheTtlFor({ generatedAt: '', machines }, Date.now());
    return { generatedAt: new Date().toISOString(), machines };
  })();
  cache = entry;
  return entry.health;
}

export function invalidateSignInHealthCache(): void {
  cache = null;
}
