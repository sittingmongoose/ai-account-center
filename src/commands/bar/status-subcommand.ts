/**
 * `ai-account-center bar status` — report whether the AI Account Center server is running.
 *
 * Checks server.pid for the PID, then verifies the process is alive and
 * the server proves its identity at GET /api/bar/auth. ASCII output only.
 */

import * as fs from 'fs';
import { getCcsDir } from '../../config/config-loader-facade';
import { getBarJsonPath, getServerPidPath } from './bar-paths';
import { parseBarServerProcessRecord, parseLegacyServerPid } from './bar-process-control';
import { probeRecordedBarServer } from './bar-server-probe';

// ---------------------------------------------------------------------------
// Types — injectable deps
// ---------------------------------------------------------------------------

export interface StatusDeps {
  /** Returns ~/.ccs dir (respects CCS_HOME). */
  getCcsDir: () => string;
  /**
   * Read the server.pid file. Returns the raw string content, or null when
   * absent or unreadable.
   */
  readPidFile: (pidPath: string) => string | null;
  /**
   * Check whether a process is alive.
   * Uses kill(pid, 0) semantics: no error = alive, ESRCH = gone.
   * Returns true when alive, false otherwise.
   */
  isProcessAlive: (pid: number) => boolean;
  /**
   * Probe whether the recorded loopback server proves its identity at
   * GET {baseUrl}/api/bar/auth. A plain HTTP 200 is insufficient. Never throws.
   */
  probeServer: (baseUrl: string) => Promise<boolean>;
  /**
   * Read bar.json and return the baseUrl field, or null when absent/malformed.
   */
  readBarJsonBaseUrl: (barJsonPath: string) => string | null;
}

// ---------------------------------------------------------------------------
// Default implementations
// ---------------------------------------------------------------------------

function defaultGetCcsDir(): string {
  return getCcsDir();
}

function defaultReadPidFile(pidPath: string): string | null {
  try {
    return fs.readFileSync(pidPath, 'utf8').trim();
  } catch {
    return null;
  }
}

function defaultIsProcessAlive(pid: number): boolean {
  try {
    // kill(pid, 0) is a POSIX trick: sends no signal but checks if the
    // process exists and is accessible. Throws ESRCH when gone.
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function defaultReadBarJsonBaseUrl(barJsonPath: string): string | null {
  try {
    const raw = fs.readFileSync(barJsonPath, 'utf8');
    const parsed = JSON.parse(raw) as Partial<{ baseUrl: string }>;
    return typeof parsed.baseUrl === 'string' ? parsed.baseUrl : null;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

export async function handleBarStatus(
  _args: string[],
  deps: Partial<StatusDeps> = {}
): Promise<void> {
  const ccsDir = (deps.getCcsDir ?? defaultGetCcsDir)();
  const readPidFile = deps.readPidFile ?? defaultReadPidFile;
  const isProcessAlive = deps.isProcessAlive ?? defaultIsProcessAlive;
  const probeServer = deps.probeServer ?? ((baseUrl) => probeRecordedBarServer(ccsDir, baseUrl));
  const readBarJsonBaseUrl = deps.readBarJsonBaseUrl ?? defaultReadBarJsonBaseUrl;

  const pidPath = getServerPidPath(ccsDir);
  const barJsonPath = getBarJsonPath(ccsDir);

  // 1. Check PID file.
  const pidRaw = readPidFile(pidPath);
  if (pidRaw === null) {
    console.log('[i] AI Account Center server: stopped (no server.pid)');
    return;
  }

  const processRecord = parseBarServerProcessRecord(pidRaw);
  if (processRecord === null) {
    const legacyPid = parseLegacyServerPid(pidRaw);
    if (legacyPid !== null) {
      console.log(
        `[!] AI Account Center server: legacy server.pid has unverified PID ${legacyPid}; status cannot safely identify it.`
      );
      console.log(`[i] Verify manually with: ps -p ${legacyPid} -o command=`);
      console.log(
        '[i] If it is AI Account Center, stop it manually, remove server.pid, then restart.'
      );
      return;
    }
    console.log(`[!] AI Account Center server: server.pid is invalid ("${pidRaw}")`);
    return;
  }
  const { pid } = processRecord;

  // 2. Check process liveness.
  const alive = isProcessAlive(pid);
  if (!alive) {
    console.log(`[!] AI Account Center server: PID ${pid} is no longer running (stale server.pid)`);
    console.log(
      '[i] Run `ai-account-center bar stop` to clean up, then `ai-account-center bar` to restart.'
    );
    return;
  }

  // 3. Probe HTTP reachability.
  const baseUrl = readBarJsonBaseUrl(barJsonPath) ?? 'http://127.0.0.1:3000';
  const reachable = await probeServer(baseUrl);

  if (reachable) {
    console.log(`[OK] AI Account Center server: running (PID ${pid}, ${baseUrl})`);
  } else {
    console.log(
      `[!] AI Account Center server: PID ${pid} alive but HTTP probe failed at ${baseUrl}`
    );
    console.log('[i] The server may still be starting up. Try again in a moment.');
  }
}
