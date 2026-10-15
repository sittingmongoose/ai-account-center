import fs from 'fs';
import { withPrivateFileLock } from './private-json-store';
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

/**
 * The computers an extra usage-log folder can sit on. Nas1 is a second Ubuntu computer,
 * so its paths are POSIX.
 */
export const USAGE_LOG_HOSTS = ['ubuntu', 'mac', 'windows', 'nas1'] as const;
export type UsageLogHost = (typeof USAGE_LOG_HOSTS)[number];

/** A dot path into a generic JSONL record (`usage.input_tokens`). */
const FIELD_PATH = /^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)*$/;
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

function isUsageLogSource(value: unknown, allowRemoteJsonl = false): value is UsageLogSource {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const source = value as Record<string, unknown>;
  if (!allowRemoteJsonl && source.tool === 'jsonl' && source.host !== 'ubuntu') return false;
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
    // A Windows path follows Windows syntax; every other host (Nas1 included) is POSIX.
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
      let value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
      // Older releases accepted remote JSONL roots. Migrate only those entries;
      // strict write validation must never reset unrelated saved preferences.
      if (value && typeof value === 'object' && !Array.isArray(value)) {
        const prefs = value as Record<string, unknown>;
        if (Array.isArray(prefs.usageLogSources)) {
          const sources = prefs.usageLogSources.filter(
            (source) =>
              !(
                isUsageLogSource(source, true) &&
                source.tool === 'jsonl' &&
                source.host !== 'ubuntu'
              )
          );
          if (sources.length !== prefs.usageLogSources.length) {
            const migrated = { ...prefs, usageLogSources: sources };
            if (isDashboardPreferences(migrated)) {
              value = migrated;
              try {
                logger.warn(
                  'dashboard-preferences.unsupported-sources',
                  'Ignoring saved remote JSONL usage log sources; only ubuntu supports generic JSONL.'
                );
              } catch {
                /* Logging is best effort. */
              }
            }
          }
        }
      }
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
  if (value?.usageLogSources?.some((source) => source.tool === 'jsonl' && source.host !== 'ubuntu'))
    throw new ConfigError('Generic JSONL usage log sources are supported only on ubuntu');
  if (!isDashboardPreferences(value)) throw new ConfigError('Invalid dashboard preferences');
  const normalized = value.usageLogSources.map((source) => {
    const syntax = source.host === 'windows' ? path.win32 : path.posix;
    let root = syntax.normalize(source.path);
    if (source.host === 'ubuntu' && process.platform !== 'win32') {
      try {
        root = fs.realpathSync(root);
      } catch {
        /* Missing sources can be saved; lexical overlap is still checked. */
      }
    }
    if (source.tool === 'codex') root = syntax.join(root, 'sessions');
    root = root.replace(/[\\/]+$/, '');
    if (source.host === 'windows') root = root.toLowerCase();
    return { source, root, sep: syntax.sep };
  });
  for (let index = 0; index < normalized.length; index++) {
    const current = normalized[index];
    // Standard built-in tool homes on every host. Custom/environment roots are also
    // canonicalized and excluded by the readers on the computer that owns them.
    const portable = current.root.replace(/\\/g, '/');
    const home = portable.match(
      /^(?:\/Users\/[^/]+|\/home\/[^/]+|\/root|[a-z]:\/users\/[^/]+)(?:\/|$)/
    )?.[0];
    const relative = home ? portable.slice(home.length) : null;
    const builtins: Partial<Record<UsageLogTool, string[]>> = {
      'claude-code': ['.claude/projects'],
      codex: ['.codex/sessions'],
      omp: ['.omp/agent/sessions'],
      muse: ['.local/share/muse/sessions'],
      zcode: ['.zcode/cli/db'],
    };
    const profile = relative?.match(/^\.omp\/profiles\/[^/]+/)?.[0];
    if (profile) builtins.omp?.push(`${profile}/agent/sessions`);
    const readRoots =
      current.source.tool === 'jsonl'
        ? Object.values(builtins).flat()
        : (builtins[current.source.tool] ?? []);
    const overlap =
      relative !== null &&
      readRoots.some(
        (root) =>
          !relative ||
          relative === root ||
          relative.startsWith(root + '/') ||
          root.startsWith(relative + '/')
      );
    if (overlap)
      throw new ConfigError(`Usage log source ${current.source.id} overlaps a built-in root`);
    for (const prior of normalized.slice(0, index)) {
      if (prior.source.host !== current.source.host) continue;
      if (
        prior.source.tool !== current.source.tool &&
        prior.source.tool !== 'jsonl' &&
        current.source.tool !== 'jsonl'
      )
        continue;
      if (
        current.root === prior.root ||
        current.root.startsWith(prior.root + current.sep) ||
        prior.root.startsWith(current.root + current.sep)
      )
        throw new ConfigError(
          `Usage log sources ${prior.source.id} and ${current.source.id} overlap`
        );
    }
  }
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

/** Only the named fields or source ids change, under the same mutex as the route's PUT. */
export interface DashboardPreferencesPatch {
  timeZone?: string;
  snapshotCleanup?: { auto: boolean };
  addUsageLogSource?: UsageLogSource;
  removeUsageLogSource?: string;
}

export function isDashboardPreferencesPatch(value: unknown): value is DashboardPreferencesPatch {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const patch = value as Record<string, unknown>;
  const keys = Object.keys(patch);
  return (
    keys.length > 0 &&
    keys.every((key) => {
      if (key === 'timeZone') return isAccountAnalyticsTimeZone(patch[key]);
      if (key === 'snapshotCleanup')
        return isDashboardPreferences({
          ...defaultDashboardPreferences(),
          snapshotCleanup: patch[key],
        });
      if (key === 'addUsageLogSource') return isUsageLogSource(patch[key]);
      if (key === 'removeUsageLogSource')
        return typeof patch[key] === 'string' && SOURCE_ID.test(patch[key] as string);
      return false;
    })
  );
}

export function withDashboardPreferencesLock<T>(
  task: () => Promise<T>,
  ccsDir = getCcsDir()
): Promise<T> {
  return withPrivateFileLock(path.join(ccsDir, PREFERENCES_FILE), task);
}

export function patchDashboardPreferences(
  patch: DashboardPreferencesPatch,
  ccsDir = getCcsDir()
): Promise<DashboardPreferences> {
  if (!isDashboardPreferencesPatch(patch))
    throw new ConfigError('Invalid dashboard preferences patch');
  return withDashboardPreferencesLock(async () => {
    const current = readDashboardPreferences(ccsDir);
    if (patch.timeZone !== undefined) current.timeZone = patch.timeZone;
    if (patch.snapshotCleanup !== undefined) current.snapshotCleanup = patch.snapshotCleanup;
    if (patch.removeUsageLogSource !== undefined)
      current.usageLogSources = current.usageLogSources.filter(
        (source) => source.id !== patch.removeUsageLogSource
      );
    if (patch.addUsageLogSource !== undefined)
      current.usageLogSources.push(patch.addUsageLogSource);
    return writeDashboardPreferences(current, ccsDir);
  }, ccsDir);
}
