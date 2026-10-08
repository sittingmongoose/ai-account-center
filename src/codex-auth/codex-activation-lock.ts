import * as os from 'os';
import * as path from 'path';
import * as lockfile from 'proper-lockfile';

/** Native shared login; never infer it from the legacy per-profile CODEX_HOME. */
export function getActivationCodexHome(): string {
  return path.join(os.homedir(), '.codex');
}

function lockOptions(codexHome: string) {
  return {
    realpath: false,
    lockfilePath: path.join(codexHome, '.ccs-activation.lock'),
    stale: 120_000,
    update: 5_000,
  };
}

/** Acquire before any registry write lock; spans activation/removal and recovery. */
export function acquireCodexActivationLock(codexHome: string): Promise<() => Promise<void>> {
  return lockfile.lock(codexHome, {
    ...lockOptions(codexHome),
    retries: { retries: 100, factor: 1, minTimeout: 100, maxTimeout: 100 },
  });
}

/**
 * The same lock without waiting: rejects at once while another holder has it.
 * Background work (saved-login renewal) uses this and skips its turn instead of
 * delaying an activation, sign-in or removal.
 */
export function tryAcquireCodexActivationLock(codexHome: string): Promise<() => Promise<void>> {
  return lockfile.lock(codexHome, { ...lockOptions(codexHome), retries: 0 });
}
