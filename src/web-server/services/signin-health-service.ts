import { execFile } from 'child_process';
import * as path from 'path';
import type { SnapshotSignInProfile } from '../agent-usage/agent-usage-snapshot';

/**
 * Sign-in expiry health for every Claude OAuth login on the machines AAC
 * collects from. The collector (scripts/account-usage/signin_expiry.py) reads
 * only the two OAuth times and token presence from each config dir — token
 * values are never read or transported. Results are cached for five minutes:
 * the underlying times move in hours, not seconds.
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

const CACHE_TTL_MS = 5 * 60 * 1000;
const COLLECTOR = 'signin_expiry.py';
const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_. -]{0,63}$/;

let cache: { at: number; key: string; health: Promise<SignInHealth> } | null = null;

function runCollector(command: string, args: string[], timeoutMs: number): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = execFile(
      command,
      args,
      { timeout: timeoutMs, maxBuffer: 1024 * 1024 },
      (error, stdout) => {
        if (error && stdout.length === 0) {
          reject(error);
          return;
        }
        resolve(stdout);
      }
    );
    child.on('error', () => {
      /* handled through the empty-stdout branch above */
    });
  });
}

/** The published DTO is allowlisted: a changed helper cannot add fields. */
export function allowProfile(raw: unknown, machine: string): SignInProfile | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const row = raw as Record<string, unknown>;
  if (typeof row.name !== 'string' || !NAME_PATTERN.test(row.name)) return null;
  const states = ['ok', 'warn', 'critical', 'logged_out', 'unavailable', 'unsupported'];
  if (typeof row.state !== 'string' || !states.includes(row.state)) return null;
  const stamps = ['accessExpiresAt', 'refreshExpiresAt', 'loggedOutAt'];
  for (const key of stamps) {
    const value = row[key];
    if (value !== undefined && value !== null && (typeof value !== 'string' || value.length > 40))
      return null;
  }
  if (
    row.fixCommand !== undefined &&
    (typeof row.fixCommand !== 'string' || row.fixCommand.length > 200)
  )
    return null;
  if (
    row.configDir !== undefined &&
    (typeof row.configDir !== 'string' || row.configDir.length > 200)
  )
    return null;
  const provider = row.provider === 'codex' ? 'codex' : 'claude';
  return {
    name: row.name,
    provider,
    platform: machine,
    machine,
    configDir: typeof row.configDir === 'string' ? row.configDir : undefined,
    accessExpiresAt: typeof row.accessExpiresAt === 'string' ? row.accessExpiresAt : null,
    refreshExpiresAt: typeof row.refreshExpiresAt === 'string' ? row.refreshExpiresAt : null,
    loggedOutAt: typeof row.loggedOutAt === 'string' ? row.loggedOutAt : null,
    state: row.state as SignInProfile['state'],
    fixCommand: typeof row.fixCommand === 'string' ? row.fixCommand : '',
    ...(row.state === 'unavailable' && typeof row.error === 'string' && row.error !== ''
      ? { error: row.error.slice(0, 120) }
      : {}),
    ...(row.state === 'unsupported' && Array.isArray(row.fieldsAvailable)
      ? {
          fieldsAvailable: row.fieldsAvailable
            .filter((f): f is string => typeof f === 'string' && NAME_PATTERN.test(f))
            .slice(0, 32),
        }
      : {}),
  };
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
      error: error.slice(0, 120),
    },
  ];
}

async function collectLocal(): Promise<string> {
  // The packaged helper, resolved from this module like the sibling usage
  // transports — nothing installs the collector into ~/.ccs on a clean machine.
  const helper = path.resolve(__dirname, '../../../scripts/account-usage', COLLECTOR);
  return runCollector('python3', [helper], 20_000);
}

async function collectMac(): Promise<string> {
  // The Mac runs the same collector from its synced helper folder.
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
      'mac',
      'python3 ~/.ccs/account-usage/' +
        COLLECTOR +
        ' 2>/dev/null || python3 ~/.ccs/account-usage/' +
        COLLECTOR,
    ],
    30_000
  );
}

async function collectWindows(): Promise<string> {
  // Windows: the synced helper folder under the remote %HOME%, run by the
  // remote python (the same layout the claude usage helper uses).
  const script = [
    "$ErrorActionPreference = 'Stop'",
    "$env:PYTHONIOENCODING = 'utf-8'",
    "$env:PYTHONUTF8 = '1'",
    "$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', 'signin_expiry.py')",
    "$python = 'python.exe'",
    '& $python $helper',
  ].join('; ');
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
      'windows',
      'powershell.exe -NoProfile -NonInteractive -EncodedCommand ' +
        Buffer.from(script, 'utf16le').toString('base64'),
    ],
    30_000
  );
}

/** Each host is paired with its own collector; results never shift position. */
export function hostCollectors(
  remoteHosts: readonly string[]
): { host: string; run: () => Promise<string> }[] {
  const list: { host: string; run: () => Promise<string> }[] = [
    { host: 'ubuntu', run: collectLocal },
  ];
  for (const host of remoteHosts) {
    if (host === 'mac') list.push({ host, run: collectMac });
    else if (host === 'windows') list.push({ host, run: collectWindows });
  }
  return list;
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

const REMOTE_HOSTS: readonly string[] = ['mac', 'windows'];

export function getSignInHealth(
  remoteHosts: readonly string[] = REMOTE_HOSTS
): Promise<SignInHealth> {
  const now = Date.now();
  const key = remoteHosts.join(',');
  if (cache && cache.key === key && now - cache.at < CACHE_TTL_MS) return cache.health;
  const health = (async (): Promise<SignInHealth> => {
    const machines: Record<string, SignInProfile[]> = {};
    const pairs = hostCollectors(remoteHosts);
    const results = await Promise.allSettled(pairs.map((pair) => pair.run()));
    pairs.forEach(({ host }, index) => {
      const result = results[index];
      if (result.status === 'fulfilled') {
        const rows = parseRows(result.value, host);
        if (rows.length > 0) {
          machines[host] = rows;
          return;
        }
      }
      const reason =
        result.status === 'rejected'
          ? result.reason instanceof Error
            ? result.reason.message
            : String(result.reason)
          : 'no profiles reported';
      machines[host] = unavailableHost(host, reason);
    });
    return { generatedAt: new Date().toISOString(), machines };
  })();
  cache = { at: now, key, health };
  return health;
}

export function invalidateSignInHealthCache(): void {
  cache = null;
}
