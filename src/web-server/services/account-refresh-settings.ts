import fs from 'fs';
import path from 'path';
import { randomBytes } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { ConfigError } from '../../errors/error-types';

export interface AccountRefreshSettings {
  refreshIntervalSeconds: number;
}

export const DEFAULT_ACCOUNT_REFRESH_SECONDS = 60;

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

export function readAccountRefreshSettings(ccsDir = getCcsDir()): AccountRefreshSettings {
  const file = path.join(ccsDir, 'account-refresh-settings.json');
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 4096) {
      return { refreshIntervalSeconds: DEFAULT_ACCOUNT_REFRESH_SECONDS };
    }
    const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    if (isAccountRefreshSettings(value)) return { ...value };
  } catch {
    // Missing or unreadable settings retain the documented default.
  }
  return { refreshIntervalSeconds: DEFAULT_ACCOUNT_REFRESH_SECONDS };
}

export function writeAccountRefreshSettings(
  value: AccountRefreshSettings,
  ccsDir = getCcsDir()
): AccountRefreshSettings {
  if (!isAccountRefreshSettings(value)) throw new ConfigError('Invalid usage refresh interval');
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  const file = path.join(ccsDir, 'account-refresh-settings.json');
  const temporary = `${file}.${randomBytes(12).toString('hex')}.tmp`;
  try {
    fs.writeFileSync(temporary, `${JSON.stringify(value)}\n`, { mode: 0o600, flag: 'wx' });
    fs.renameSync(temporary, file);
  } finally {
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
