/**
 * io-locks.ts
 *
 * Path constants, lockfile management, and config I/O helpers extracted from
 * unified-config-loader.ts (Phase 1 split — issue #1164).
 *
 * Forward-reference note: loadUnifiedConfigWithLockHeld and
 * writeUnifiedConfigWithLockHeld depend on mergeWithDefaults,
 * generateYamlHeader, and generateYamlWithComments
 * which still live in unified-config-loader.ts (extracted in later phases).
 * Those are passed as callbacks to avoid a circular import. Once Phases 2–3
 * land in separate modules with no dependency on this file, they can be
 * imported directly.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as crypto from 'crypto';
import * as yaml from 'js-yaml';
import { ConfigError } from '../../errors/error-types';
import { claimWriterLease } from '../../shared/private-file-lease';
import { getCcsDir } from '../../utils/config-manager';
import {
  isUnifiedConfig,
  createEmptyUnifiedConfig,
  UNIFIED_CONFIG_VERSION,
} from '../unified-config-types';
import type { UnifiedConfig } from '../unified-config-types';

// ---------------------------------------------------------------------------
// Path constants
// ---------------------------------------------------------------------------

export const CONFIG_YAML = 'config.yaml';
export const CONFIG_JSON = 'config.json';
export const CONFIG_LOCK = 'config.yaml.lock';
/** Lock is stale after this many milliseconds */
export const LOCK_STALE_MS = 5000;
export const GO_DURATION_SEGMENT = String.raw`(?:\d+(?:\.\d+)?(?:ns|us|µs|μs|ms|s|m|h))`;
export const GO_DURATION_PATTERN = new RegExp(`^${GO_DURATION_SEGMENT}+$`);

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/**
 * Get path to unified config.yaml
 */
export function getConfigYamlPath(): string {
  return path.join(getCcsDir(), CONFIG_YAML);
}

/**
 * Get path to legacy config.json
 */
export function getConfigJsonPath(): string {
  return path.join(getCcsDir(), CONFIG_JSON);
}

/**
 * Get path to config lockfile (internal)
 */
function getLockFilePath(): string {
  return path.join(getCcsDir(), CONFIG_LOCK);
}

// ---------------------------------------------------------------------------
// Process check
// ---------------------------------------------------------------------------

function deadClaim(content: string): boolean {
  const pidLine = content.trim().split(/\r?\n/, 1)[0];
  const pid = Number(pidLine);
  if (!/^\d+$/.test(pidLine) || pid <= 0 || pid > 0x7fffffff) return false;
  try {
    process.kill(pid, 0);
    return false;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ESRCH';
  }
}

function readClaim(lockPath: string): string {
  const stat = fs.lstatSync(lockPath);
  if (!stat.isFile() || stat.size > 1024) throw new Error('Invalid config writer claim.');
  return fs.readFileSync(lockPath, 'utf8');
}

// ---------------------------------------------------------------------------
// Lockfile primitives
// ---------------------------------------------------------------------------

/**
 * Acquire lockfile for config write operations.
 * Returns a lock token if acquired, null if already locked by another process.
 * Reclaims an unchanged claim only after proving its owner is dead.
 */
export function acquireLock(): string | null {
  const lockPath = getLockFilePath();
  const lockDir = path.dirname(lockPath);
  const lockToken = crypto.randomUUID();
  const lockData = `${process.pid}\n${Date.now()}\n${lockToken}`;

  const pending = path.join(lockDir, '.config-claim-' + lockToken);
  try {
    fs.mkdirSync(lockDir, { recursive: true, mode: 0o700 });
    fs.writeFileSync(pending, lockData, { flag: 'wx', mode: 0o600 });
    try {
      fs.linkSync(pending, lockPath);
      return lockToken;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') return null;
    }

    const held = readClaim(lockPath);
    if (!deadClaim(held)) return null;
    const takeover = claimWriterLease(lockDir, path.join(lockDir, 'config.yaml.reclaim.lock'));
    try {
      const again = readClaim(lockPath);
      if (again !== held || !deadClaim(again)) return null;
      fs.renameSync(pending, lockPath);
      return lockToken;
    } finally {
      takeover.close();
    }
  } catch {
    return null;
  } finally {
    try {
      fs.unlinkSync(pending);
    } catch {
      // The claim was moved into place or the temporary file was never written.
    }
  }
}

/**
 * Release lockfile after config write operation.
 */
export function releaseLock(lockToken: string): void {
  const lockPath = getLockFilePath();
  try {
    if (fs.existsSync(lockPath)) {
      const content = fs.readFileSync(lockPath, 'utf8');
      const fileToken = content.trim().split('\n')[2];
      if (fileToken === lockToken) {
        fs.unlinkSync(lockPath);
      }
    }
  } catch {
    // Ignore cleanup errors
  }
}

// ---------------------------------------------------------------------------
// Config format detection
// ---------------------------------------------------------------------------

/**
 * Check if unified config.yaml exists
 */
export function hasUnifiedConfig(): boolean {
  return fs.existsSync(getConfigYamlPath());
}

/**
 * Check if legacy config.json exists
 */
export function hasLegacyConfig(): boolean {
  return fs.existsSync(getConfigJsonPath());
}

// ---------------------------------------------------------------------------
// Sync sleep
// ---------------------------------------------------------------------------

/**
 * Sync sleep helper for lock retry loops.
 * Uses Atomics.wait when available to avoid CPU-intensive busy-wait.
 */
export function sleepSync(ms: number): void {
  try {
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
  } catch {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      /* busy-wait */
    }
  }
}

// ---------------------------------------------------------------------------
// Lock-aware execution wrapper
// ---------------------------------------------------------------------------

/**
 * Execute a callback while holding the config lock.
 */
export function withConfigWriteLock<T>(callback: () => T): T {
  // Acquire lock (retry for up to 1 second)
  const maxRetries = 10;
  const retryDelayMs = 100;
  let lockToken: string | null = null;
  for (let i = 0; i < maxRetries; i++) {
    const acquiredToken = acquireLock();
    if (acquiredToken) {
      lockToken = acquiredToken;
      break;
    }
    sleepSync(retryDelayMs);
  }

  if (!lockToken) {
    throw new Error('Config file is locked by another process. Wait a moment and try again.');
  }

  try {
    return callback();
  } finally {
    // Always release lock
    releaseLock(lockToken);
  }
}

// ---------------------------------------------------------------------------
// Lock-held read/write helpers
// ---------------------------------------------------------------------------

/**
 * Load unified config directly from disk while lock is already held.
 * Falls back to empty config when file doesn't exist.
 *
 * Forward-reference: mergeWithDefaults is
 * passed as a callback to avoid a circular dependency. They will be imported
 * directly once all phases are complete.
 */
export function loadUnifiedConfigWithLockHeld(
  mergeWithDefaults: (partial: Partial<UnifiedConfig>) => UnifiedConfig
): UnifiedConfig {
  const yamlPath = getConfigYamlPath();
  if (!fs.existsSync(yamlPath)) {
    return createEmptyUnifiedConfig();
  }

  const content = fs.readFileSync(yamlPath, 'utf8');
  const parsed = yaml.load(content);

  if (!isUnifiedConfig(parsed)) {
    throw new Error(`Invalid config format in ${yamlPath}`);
  }

  return mergeWithDefaults(parsed);
}

/** Reuse an identical regular backup, including backups made by earlier versions. */
function preserveRetiredOriginal(yamlPath: string, original: Buffer): string {
  const directory = path.dirname(yamlPath);
  const prefix = `${path.basename(yamlPath)}.retired-`;
  const digest = crypto.createHash('sha256').update(original).digest('hex');
  for (const name of fs.readdirSync(directory).filter((entry) => entry.startsWith(prefix))) {
    const candidate = path.join(directory, name);
    const stat = fs.lstatSync(candidate);
    if (!stat.isFile() || stat.size !== original.length) continue;
    const bytes = fs.readFileSync(candidate);
    if (crypto.createHash('sha256').update(bytes).digest('hex') === digest) {
      if ((stat.mode & 0o777) !== 0o600) fs.chmodSync(candidate, 0o600);
      return candidate;
    }
  }
  const backup = `${yamlPath}.retired-${Date.now()}-${digest}`;
  const descriptor = fs.openSync(backup, 'wx', 0o600);
  try {
    fs.writeFileSync(descriptor, original);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
  return backup;
}

/**
 * Write unified config to disk while lock is already held.
 * Uses atomic write (temp file + rename) to prevent corruption.
 *
 * Forward-reference: generateYamlHeader and generateYamlWithComments are
 * passed as callbacks to avoid a circular dependency. They will be imported
 * directly once all phases are complete.
 */
export function writeUnifiedConfigWithLockHeld(
  config: UnifiedConfig,
  generateYamlHeader: () => string,
  generateYamlWithComments: (
    config: UnifiedConfig,
    existing?: string,
    preserveOriginal?: () => void
  ) => string
): void {
  const yamlPath = getConfigYamlPath();
  const dir = path.dirname(yamlPath);

  // Ensure directory exists
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  }

  // Ensure version is set
  config.version = UNIFIED_CONFIG_VERSION;

  // Read original bytes once; decoding is only for YAML, never for the backup.
  const original = fs.existsSync(yamlPath) ? fs.readFileSync(yamlPath) : undefined;
  let backup: string | undefined;
  const yamlContent = generateYamlWithComments(config, original?.toString('utf8') ?? '', () => {
    if (!original) throw new ConfigError('Cannot back up an absent original configuration.');
    backup = preserveRetiredOriginal(yamlPath, original);
  });
  const content = generateYamlHeader() + yamlContent;

  // Atomic write: write to temp file, then rename
  const tempPath = `${yamlPath}.tmp.${process.pid}`;

  try {
    fs.writeFileSync(tempPath, content, { mode: 0o600 });
    fs.renameSync(tempPath, yamlPath);
  } catch (error) {
    // Clean up temp file on error
    if (fs.existsSync(tempPath)) {
      try {
        fs.unlinkSync(tempPath);
      } catch {
        // Ignore cleanup errors
      }
    }
    // Classify filesystem errors
    const err = error as NodeJS.ErrnoException;
    if (backup) {
      throw new ConfigError(
        `Cannot save configuration: ${err.message}. Original backup: ${backup}`
      );
    }
    if (err.code === 'ENOSPC') {
      throw new Error('Disk full - cannot save config. Free up space and try again.');
    } else if (err.code === 'EROFS' || err.code === 'EACCES') {
      throw new Error(`Cannot write config - check file permissions: ${err.message}`);
    }
    throw error;
  }
}
