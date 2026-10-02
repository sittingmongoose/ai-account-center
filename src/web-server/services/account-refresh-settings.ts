import fs from 'fs';
import path from 'path';
import { randomBytes } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { ConfigError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';

export interface AccountRefreshSettings {
  refreshIntervalSeconds: number;
}

export const DEFAULT_ACCOUNT_REFRESH_SECONDS = 60;

const SETTINGS_FILE = 'account-refresh-settings.json';
const MAX_SETTINGS_BYTES = 4096;

const logger = createLogger('account-refresh-settings');
const warned = new Set<string>();

export function isAccountRefreshSettings(value: unknown): value is AccountRefreshSettings {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const object = value as Record<string, unknown>;
  return (
    Object.keys(object).length === 1 &&
    typeof object.refreshIntervalSeconds === 'number' &&
    Number.isInteger(object.refreshIntervalSeconds) &&
    object.refreshIntervalSeconds >= 30 &&
    object.refreshIntervalSeconds <= 3600
  );
}

/** One fixed line per settings file; the path and contents are never logged. */
function warnCorruptSettings(file: string): void {
  if (warned.has(file)) return;
  warned.add(file);
  try {
    logger.warn(
      'account-refresh-settings.corrupt',
      'Usage refresh settings are truncated or unparsable; the default applies until the next save.'
    );
  } catch {
    /* Logging is best effort. */
  }
}

export function readAccountRefreshSettings(ccsDir = getCcsDir()): AccountRefreshSettings {
  const file = path.join(ccsDir, SETTINGS_FILE);
  let corrupt = false;
  try {
    const stat = fs.lstatSync(file);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      // A planted symlink or special file is never followed; the default applies.
      return { refreshIntervalSeconds: DEFAULT_ACCOUNT_REFRESH_SECONDS };
    }
    if (stat.size <= MAX_SETTINGS_BYTES) {
      const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
      if (isAccountRefreshSettings(value)) return { ...value };
    }
    corrupt = true;
  } catch (error) {
    // A missing file keeps the documented default; an unreadable one warns below.
    corrupt = (error as NodeJS.ErrnoException).code !== 'ENOENT';
  }
  if (corrupt) warnCorruptSettings(file);
  return { refreshIntervalSeconds: DEFAULT_ACCOUNT_REFRESH_SECONDS };
}

/** Best effort: makes the rename durable where the platform can sync a folder. */
function syncDirectory(directory: string): void {
  if (process.platform === 'win32') return;
  let fd: number | undefined;
  try {
    fd = fs.openSync(directory, 'r');
    fs.fsyncSync(fd);
  } catch {
    /* Some file systems cannot sync a folder; the file itself was synced. */
  } finally {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd);
      } catch {
        /* The descriptor may already be unusable. */
      }
    }
  }
}

/** Temporary files left by an interrupted write are removed after a successful save. */
function removeLeftoverTemporaries(directory: string): void {
  let entries: string[];
  try {
    entries = fs.readdirSync(directory);
  } catch {
    return;
  }
  for (const entry of entries) {
    if (!entry.startsWith(`${SETTINGS_FILE}.`) || !entry.endsWith('.tmp')) continue;
    try {
      fs.unlinkSync(path.join(directory, entry));
    } catch {
      /* Best effort; another process may have removed it first. */
    }
  }
}

/**
 * Durable save, like the private JSON store: a 0600 temporary file in the same
 * folder, fsync, rename over the target, then a best-effort fsync of the folder,
 * so a power loss cannot expose a truncated or empty settings file.
 */
export function writeAccountRefreshSettings(
  value: AccountRefreshSettings,
  ccsDir = getCcsDir()
): AccountRefreshSettings {
  if (!isAccountRefreshSettings(value)) throw new ConfigError('Invalid usage refresh interval');
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  const file = path.join(ccsDir, SETTINGS_FILE);
  const temporary = `${file}.${randomBytes(12).toString('hex')}.tmp`;
  let fd: number | undefined;
  try {
    fd = fs.openSync(temporary, 'wx', 0o600);
    fs.writeFileSync(fd, `${JSON.stringify(value)}\n`);
    fs.fsyncSync(fd);
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(temporary, file);
    syncDirectory(ccsDir);
    removeLeftoverTemporaries(ccsDir);
  } finally {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd);
      } catch {
        /* The descriptor may already be unusable. */
      }
    }
    try {
      fs.unlinkSync(temporary);
    } catch {
      // The rename normally removed the temporary file.
    }
  }
  return { ...value };
}

export function getAccountRefreshIntervalSeconds(): number {
  return readAccountRefreshSettings().refreshIntervalSeconds;
}
