/**
 * `ai-account-center bar stop` — stop the detached AI Account Center server.
 *
 * Reads ~/.ccs/bar/server.pid, sends SIGTERM, then removes pid + bar.json.
 * ASCII output only. Non-fatal if the server is already gone.
 */

import * as fs from 'fs';
import { getCcsDir } from '../../config/config-loader-facade';
import { getBarJsonPath, getServerPidPath } from './bar-paths';
import {
  getProcessBirthIdentity,
  parseLegacyServerPid,
  removeBarDiscoveryIfNoProcess,
  stopBarServerProcessFile,
  stopRecordedBarServer,
  waitForProcessExit,
} from './bar-process-control';
import type { ClaimedBarServerStopOutcome } from './bar-process-control';

// ---------------------------------------------------------------------------
// Types — injectable deps
// ---------------------------------------------------------------------------

export interface StopDeps {
  /** Returns ~/.ccs dir (respects CCS_HOME). */
  getCcsDir: () => string;
  /**
   * Read the server.pid file. Returns the raw string content, or null when
   * the file is absent or unreadable.
   */
  readPidFile: (pidPath: string) => string | null;
  /**
   * Send SIGTERM to the given PID.
   * Throws if the signal cannot be delivered (e.g. ESRCH — no such process).
   */
  killProcess: (pid: number, signal: 'SIGTERM') => void;
  getProcessBirthIdentity: (pid: number) => string | null;
  waitForProcessExit: typeof waitForProcessExit;
  stopProcessFile: (pidPath: string) => Promise<ClaimedBarServerStopOutcome>;
  /** Remove a file, ignoring errors if absent. */
  removeFile: (filePath: string) => void;
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

function defaultKillProcess(pid: number, signal: 'SIGTERM'): void {
  process.kill(pid, signal);
}

function defaultRemoveFile(filePath: string): void {
  try {
    fs.unlinkSync(filePath);
  } catch {
    /* ignore — file may already be gone */
  }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

export async function handleBarStop(_args: string[], deps: Partial<StopDeps> = {}): Promise<void> {
  const ccsDir = (deps.getCcsDir ?? defaultGetCcsDir)();
  const readPidFile = deps.readPidFile ?? defaultReadPidFile;
  const killProcess = deps.killProcess ?? defaultKillProcess;
  const removeFile = deps.removeFile ?? defaultRemoveFile;
  const readProcessBirthIdentity = deps.getProcessBirthIdentity ?? getProcessBirthIdentity;
  const waitForExit = deps.waitForProcessExit ?? waitForProcessExit;

  const pidPath = getServerPidPath(ccsDir);
  const barJsonPath = getBarJsonPath(ccsDir);

  let outcome: ClaimedBarServerStopOutcome;
  if (deps.stopProcessFile !== undefined || deps.readPidFile === undefined) {
    const stopProcessFile =
      deps.stopProcessFile ??
      ((filePath: string) =>
        stopBarServerProcessFile(filePath, {
          getProcessBirthIdentity: readProcessBirthIdentity,
          killProcess,
          waitForProcessExit: waitForExit,
        }));
    outcome = await stopProcessFile(pidPath);
    if (outcome.result === 'no-record') {
      console.log('[i] AI Account Center server is not running (no server.pid found).');
      return;
    }
  } else {
    // Injected record reads remain available for isolated unit tests. Production
    // always uses the atomic file claim above.
    const pidRaw = readPidFile(pidPath);
    if (pidRaw === null) {
      console.log('[i] AI Account Center server is not running (no server.pid found).');
      return;
    }
    const recordedOutcome = await stopRecordedBarServer(pidRaw, {
      getProcessBirthIdentity: readProcessBirthIdentity,
      killProcess,
      waitForProcessExit: waitForExit,
    });
    outcome = {
      ...recordedOutcome,
      legacyPid:
        recordedOutcome.result === 'legacy-record'
          ? (parseLegacyServerPid(pidRaw) ?? undefined)
          : undefined,
    };
  }
  const pid = outcome.record?.pid;

  if (outcome.result === 'stopped' || outcome.result === 'stale') {
    if (outcome.result === 'stopped') {
      console.log(`[OK] AI Account Center server stopped (PID ${pid}).`);
    } else {
      console.log(`[i] Server PID ${pid} is no longer running. Cleaning up stale files.`);
    }
    if (deps.readPidFile !== undefined) {
      removeFile(pidPath);
      removeFile(barJsonPath);
      console.log('[i] Removed server.pid and bar.json.');
    } else {
      const removedDiscovery = removeBarDiscoveryIfNoProcess(barJsonPath, pidPath);
      if (removedDiscovery) console.log('[i] Removed server.pid and bar.json.');
      else
        console.log('[i] A replacement server record appeared; its recovery state was preserved.');
    }
    return;
  }

  if (outcome.result === 'legacy-record') {
    const legacyPid = outcome.legacyPid;
    console.error(
      `[X] Legacy server.pid records PID ${legacyPid} without process identity; no signal was sent.`
    );
    console.error(`[i] Verify manually with: ps -p ${legacyPid} -o command=`);
    console.error(
      `[i] If it is AI Account Center, stop it manually, then remove ${outcome.recoveryPath ?? pidPath} and ${barJsonPath}.`
    );
    process.exitCode = 1;
    return;
  }

  const reason =
    outcome.result === 'invalid-record'
      ? 'server.pid does not contain a verified process record'
      : outcome.result === 'identity-mismatch'
        ? `PID ${pid} belongs to a different process`
        : outcome.result === 'permission-denied'
          ? `permission denied while signaling PID ${pid}`
          : outcome.result === 'timeout'
            ? `PID ${pid} did not exit within 3 seconds`
            : `failed to signal PID ${pid}: ${outcome.error?.message ?? 'unknown error'}`;
  console.error(`[X] Refusing to remove AI Account Center recovery state: ${reason}.`);
  process.exitCode = 1;
}
