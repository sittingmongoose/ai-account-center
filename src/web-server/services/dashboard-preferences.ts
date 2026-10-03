import fs from 'fs';
import path from 'path';
import { randomBytes } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { ConfigError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';
import { isAccountAnalyticsTimeZone } from './account-analytics-range';

/**
 * Dashboard preferences (Settings): the display time zone, the Claude history
 * snapshot cleanup and the extra usage-log sources. One durable JSON file in
 * the CCS folder, saved through PUT /api/accounts/preferences, read by the
 * dashboard, the analytics day buckets and the collectors. Same durability as
 * the usage refresh settings: a 0600 temporary file, fsync, rename, folder
 * sync, so a power loss cannot expose a truncated file.
 */

export const DEFAULT_DASHBOARD_TIME_ZONE = 'America/New_York';
export const MAX_USAGE_LOG_SOURCES = 64;

export const USAGE_LOG_TOOLS = ['omp', 'muse', 'zcode', 'claude-code', 'codex', 'jsonl'] as const;
export type UsageLogTool = (typeof USAGE_LOG_TOOLS)[number];

export const USAGE_LOG_HOSTS = ['ubuntu', 'mac', 'windows'] as const;
export type UsageLogHost = (typeof USAGE_LOG_HOSTS)[number];

/** A dot path into a generic JSONL record (`usage.input_tokens`). */
const FIELD_PATH = /^[A-Za-z][A-Za-z0-9]*(?:\.[A-Za-z][A-Za-z0-9]*)*$/;
const MAPPING_KEYS = ['timestamp', 'model', 'inputTokens', 'outputTokens', 'cost'] as const;

export interface UsageLogSource {
  id: string;
  tool: UsageLogTool;
  host: UsageLogHost;
  path: string;
  fieldMapping?: Partial<Record<(typeof MAPPING_KEYS)[number], string>>;
}

export interface DashboardPreferences {
  timeZone: string;
  snapshotCleanup: { auto: boolean };
  usageLogSources: UsageLogSource[];
}

const SOURCE_ID = /^[a-z0-9][a-z0-9-]{0,31}$/;

function isUsageLogSource(value: unknown): value is UsageLogSource {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const source = value as Record<string, unknown>;
  const keys = Object.keys(source);
  if (
    !keys.every((key) => ['id', 'tool', 'host', 'path', 'fieldMapping'].includes(key)) ||
    typeof source.id !== 'string' ||
    !SOURCE_ID.test(source.id) ||
    typeof source.tool !== 'string' ||
    !(USAGE_LOG_TOOLS as readonly string[]).includes(source.tool) ||
    typeof source.host !== 'string' ||
    !(USAGE_LOG_HOSTS as readonly string[]).includes(source.host) ||
    typeof source.path !== 'string' ||
    source.path.length === 0 ||
    source.path.length > 1024 ||
    source.path.includes('\0') ||
    // A Windows path follows Windows syntax; every other host is POSIX.
    // `..` stays refused everywhere so an extra root cannot escape upward.
    !(
      path.posix.isAbsolute(source.path) ||
      (source.host === 'windows' && /^[A-Za-z]:[\\/]/.test(source.path))
    ) ||
    source.path.includes('..')
  ) {
    return false;
  }
  if (source.fieldMapping === undefined) return source.tool !== 'jsonl' ? true : false;
  if (
    !source.fieldMapping ||
    typeof source.fieldMapping !== 'object' ||
    Array.isArray(source.fieldMapping)
  ) {
    return false;
  }
  const mapping = source.fieldMapping as Record<string, unknown>;
  const names = Object.keys(mapping);
  if (names.length === 0 || names.length > MAPPING_KEYS.length) return false;
  if (typeof mapping.timestamp !== 'string' || typeof mapping.model !== 'string') return false;
  return names.every(
    (name) =>
      (MAPPING_KEYS as readonly string[]).includes(name) &&
      typeof mapping[name] === 'string' &&
      FIELD_PATH.test(mapping[name] as string)
  );
}

export function isDashboardPreferences(value: unknown): value is DashboardPreferences {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const prefs = value as Record<string, unknown>;
  if (
    !Object.keys(prefs).every((key) =>
      ['timeZone', 'snapshotCleanup', 'usageLogSources'].includes(key)
    ) ||
    !isAccountAnalyticsTimeZone(prefs.timeZone)
  ) {
    return false;
  }
  const cleanup = prefs.snapshotCleanup as Record<string, unknown> | null;
  if (!cleanup || typeof cleanup !== 'object' || Array.isArray(cleanup)) return false;
  if (!Object.keys(cleanup).every((key) => key === 'auto') || typeof cleanup.auto !== 'boolean') {
    return false;
  }
  if (
    !Array.isArray(prefs.usageLogSources) ||
    prefs.usageLogSources.length > MAX_USAGE_LOG_SOURCES
  ) {
    return false;
  }
  const ids = new Set<string>();
  for (const source of prefs.usageLogSources) {
    if (!isUsageLogSource(source)) return false;
    if (ids.has(source.id)) return false;
    ids.add(source.id);
  }
  return true;
}

export function defaultDashboardPreferences(): DashboardPreferences {
  return {
    timeZone: DEFAULT_DASHBOARD_TIME_ZONE,
    snapshotCleanup: { auto: true },
    usageLogSources: [],
  };
}

const PREFERENCES_FILE = 'dashboard-preferences.json';
const MAX_PREFERENCES_BYTES = 64 * 1024;

const logger = createLogger('dashboard-preferences');
let warned = false;

function warnCorrupt(): void {
  if (warned) return;
  warned = true;
  try {
    logger.warn(
      'dashboard-preferences.corrupt',
      'Dashboard preferences are truncated or unparsable; the defaults apply until the next save.'
    );
  } catch {
    /* Logging is best effort. */
  }
}

export function readDashboardPreferences(ccsDir = getCcsDir()): DashboardPreferences {
  const file = path.join(ccsDir, PREFERENCES_FILE);
  let corrupt = false;
  try {
    const stat = fs.lstatSync(file);
    if (stat.isSymbolicLink() || !stat.isFile()) return defaultDashboardPreferences();
    if (stat.size <= MAX_PREFERENCES_BYTES) {
      const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
      if (isDashboardPreferences(value)) {
        return {
          timeZone: value.timeZone,
          snapshotCleanup: { ...value.snapshotCleanup },
          usageLogSources: value.usageLogSources.map((source) => ({ ...source })),
        };
      }
    }
    corrupt = true;
  } catch (error) {
    corrupt = (error as NodeJS.ErrnoException).code !== 'ENOENT';
  }
  if (corrupt) warnCorrupt();
  return defaultDashboardPreferences();
}

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

function removeLeftoverTemporaries(directory: string): void {
  let entries: string[];
  try {
    entries = fs.readdirSync(directory);
  } catch {
    return;
  }
  for (const entry of entries) {
    if (!entry.startsWith(`${PREFERENCES_FILE}.`) || !entry.endsWith('.tmp')) continue;
    try {
      fs.unlinkSync(path.join(directory, entry));
    } catch {
      /* Best effort; another process may have removed it first. */
    }
  }
}

export function writeDashboardPreferences(
  value: DashboardPreferences,
  ccsDir = getCcsDir()
): DashboardPreferences {
  if (!isDashboardPreferences(value)) throw new ConfigError('Invalid dashboard preferences');
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  const file = path.join(ccsDir, PREFERENCES_FILE);
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
  return readDashboardPreferences(ccsDir);
}

/** The display time zone Settings saved (America/New_York until then). */
export function getDashboardTimeZone(): string {
  return readDashboardPreferences().timeZone;
}
