import { execFile } from 'child_process';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
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

let cache: { at: number; health: Promise<SignInHealth> } | null = null;

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
function allowProfile(raw: unknown, machine: string): SignInProfile | null {
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

async function collectLocal(): Promise<SignInProfile[]> {
  const helper = path.join(getCcsDir(), 'account-usage', COLLECTOR);
  const stdout = await runCollector('python3', [helper], 20_000);
  const start = stdout.lastIndexOf('{"schemaVersion"');
  const rows = parseRows(start < 0 ? '' : stdout.slice(start));
  return rows;
}

async function collectMac(): Promise<SignInProfile[]> {
  // The Mac runs the same collector from its synced helper folder.
  const stdout = await runCollector(
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
  const start = stdout.lastIndexOf('{"schemaVersion"');
  return parseRows(start < 0 ? '' : stdout.slice(start));
}

async function collectWindows(): Promise<SignInProfile[]> {
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
  const stdout = await runCollector(
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
  const start = stdout.lastIndexOf('{"schemaVersion"');
  return parseRows(start < 0 ? '' : stdout.slice(start));
}

function parseRows(stdout: string): SignInProfile[] {
  try {
    const value = JSON.parse(stdout);
    if (value && value.schemaVersion === 1 && Array.isArray(value.profiles)) {
      return value.profiles
        .map((row: unknown) => allowProfile(row, ''))
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
  if (cache && now - cache.at < CACHE_TTL_MS) return cache.health;
  const health = (async (): Promise<SignInHealth> => {
    const machines: Record<string, SignInProfile[]> = {};
    const results = await Promise.allSettled([collectLocal(), collectMac(), collectWindows()]);
    const hosts: string[] = ['ubuntu', ...remoteHosts.filter((h) => h !== 'ubuntu')];
    hosts.forEach((host, index) => {
      const result = results[index];
      if (result.status === 'fulfilled' && result.value.length > 0) {
        machines[host] = result.value.map((row) => ({ ...row, machine: host }));
      } else {
        const reason =
          result.status === 'rejected'
            ? result.reason instanceof Error
              ? result.reason.message
              : String(result.reason)
            : 'no profiles reported';
        machines[host] = unavailableHost(host, reason);
      }
    });
    return { generatedAt: new Date().toISOString(), machines };
  })();
  cache = { at: now, health };
  return health;
}

export function invalidateSignInHealthCache(): void {
  cache = null;
}
