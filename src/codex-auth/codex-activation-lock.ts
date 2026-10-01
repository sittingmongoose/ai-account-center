import * as os from 'os';
import * as path from 'path';
import * as lockfile from 'proper-lockfile';

/** Native shared login; never infer it from the legacy per-profile CODEX_HOME. */
export function getActivationCodexHome(): string {
  return path.join(os.homedir(), '.codex');
}

/** Acquire before any registry write lock; spans activation/removal and recovery. */
export function acquireCodexActivationLock(codexHome: string): Promise<() => Promise<void>> {
  return lockfile.lock(codexHome, {
    realpath: false,
    lockfilePath: path.join(codexHome, '.ccs-activation.lock'),
    stale: 120_000,
    update: 5_000,
    retries: { retries: 100, factor: 1, minTimeout: 100, maxTimeout: 100 },
  });
}
