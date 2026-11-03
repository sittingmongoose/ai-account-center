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

export interface SignInProfile extends SnapshotSignInProfile {}

export interface SignInHealth {
  generatedAt: string;
  machines: Record<string, SignInProfile[]>;
}

const CACHE_TTL_MS = 5 * 60 * 1000;
const COLLECTOR = 'signin_expiry.py';

let cache: { at: number; health: Promise<SignInHealth> } | null = null;

/** The ssh hosts the usage helpers already run on, in helper-sync order. */
const REMOTE_HOSTS: readonly string[] = ['mac', 'windows'];

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

function parseCollector(stdout: string, platform: string): SignInProfile[] {
  try {
    const value = JSON.parse(stdout);
    if (value && value.schemaVersion === 1 && Array.isArray(value.profiles)) {
      return value.profiles
        .filter(
          (p: { name?: unknown; state?: unknown }) =>
            typeof p.name === 'string' && typeof p.state === 'string'
        )
        .map((p: Record<string, unknown>) => ({ ...p, platform }));
    }
  } catch {
    /* a partial or unreadable answer reads as unavailable */
  }
  return [];
}

async function collectLocal(helperDir: string): Promise<SignInProfile[]> {
  const stdout = await runCollector('python3', [path.join(helperDir, COLLECTOR)], 20_000);
  return parseCollector(stdout, 'ubuntu');
}

async function collectRemote(sshHost: string, helperDir: string): Promise<SignInProfile[]> {
  const command = `python3 ${helperDir}/${COLLECTOR} 2>/dev/null || python ${helperDir}/${COLLECTOR} 2>/dev/null`;
  const stdout = await new Promise<string>((resolve, reject) => {
    const child = execFile(
      'ssh',
      [
        '-T',
        '-o',
        'BatchMode=yes',
        '-o',
        'ConnectTimeout=5',
        '-o',
        'ConnectionAttempts=1',
        sshHost,
        command,
      ],
      { timeout: 30_000, maxBuffer: 1024 * 1024 },
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
  const platform = sshHost === 'mac' ? 'mac' : 'windows';
  // The answer may carry ssh banners; take the last JSON object line onward.
  const start = stdout.lastIndexOf('{"schemaVersion"');
  if (start < 0) return [];
  return parseCollector(stdout.slice(start), platform);
}

export function getSignInHealth(
  helperDir = path.join(
    process.env.CCS_DIR ?? path.join(process.env.HOME ?? '', '.ccs'),
    'account-usage'
  ),
  remoteHosts: readonly string[] = REMOTE_HOSTS
): Promise<SignInHealth> {
  const now = Date.now();
  if (cache && now - cache.at < CACHE_TTL_MS) return cache.health;
  const health = (async (): Promise<SignInHealth> => {
    const machines: Record<string, SignInProfile[]> = {};
    const settled = await Promise.allSettled([
      collectLocal(helperDir),
      ...remoteHosts.map((host) => collectRemote(host, helperDir)),
    ]);
    machines.ubuntu = settled[0].status === 'fulfilled' ? settled[0].value : [];
    remoteHosts.forEach((host, index) => {
      const result = settled[index + 1];
      machines[host] = result.status === 'fulfilled' ? result.value : [];
    });
    return { generatedAt: new Date().toISOString(), machines };
  })();
  cache = { at: now, health };
  return health;
}

export function invalidateSignInHealthCache(): void {
  cache = null;
}
