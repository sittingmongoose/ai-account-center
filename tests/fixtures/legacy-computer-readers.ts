/**
 * Frozen reader/validation rules from merge base d5ec0fc64d8a75141a8fe9768c33ed0236eabca5.
 * Keep the original four-host allowlists, 40-row guard and preference validation
 * independent of the new registry. Only the unchanged update row decoder and
 * time-zone validator are shared; no writer, transport or provider code runs.
 */
import fs from 'fs';
import path from 'path';
import { isAccountAnalyticsTimeZone } from '../../src/web-server/services/account-analytics-range';
import {
  MESSAGES,
  MAX_OUTPUT,
  UPDATE_APP_LABELS,
  record,
  isUpdateAppId,
  normalizeAppUpdateResults,
  rowMessage,
  APP_UPDATE_PHASES,
  type MessageCode,
  type UpdateAppId,
  type UpdatePlatform,
  type AppUpdateResult,
  type AppUpdateJob,
  type AppUpdateHostProgress,
  type AppUpdatePhase,
} from '../../src/web-server/services/app-update-contract';
import { LEGACY_MESSAGES } from '../../src/web-server/services/app-update-messages';

const PLATFORMS: UpdatePlatform[] = ['ubuntu', 'mac', 'windows', 'nas1'];
const EXPECTED_RESULTS = 40;
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

const logger = { warn: (..._args: unknown[]) => {} };
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

export function readDashboardPreferences(ccsDir: string): DashboardPreferences {
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

function waitingHosts(): Record<UpdatePlatform, AppUpdateHostProgress> {
  return {
    ubuntu: { state: 'waiting', currentApp: null, phase: null },
    mac: { state: 'waiting', currentApp: null, phase: null },
    windows: { state: 'waiting', currentApp: null, phase: null },
    nas1: { state: 'waiting', currentApp: null, phase: null },
  };
}

/** Computers added after jobs were first saved: an older job has no entry for them. */
const LATER_PLATFORMS: readonly UpdatePlatform[] = ['nas1'];

/**
 * Restores saved host progress only when every field is one of the fixed values.
 * A job saved before a later computer existed never ran it, so its missing entry
 * restores as done; a present entry must still be well-formed.
 */
function restoreHosts(value: unknown): Record<UpdatePlatform, AppUpdateHostProgress> | null {
  const raw = record(value);
  if (!raw) return null;
  const hosts = waitingHosts();
  for (const platform of PLATFORMS) {
    if (
      LATER_PLATFORMS.includes(platform) &&
      !Object.prototype.hasOwnProperty.call(raw, platform)
    ) {
      hosts[platform] = { state: 'done', currentApp: null, phase: null };
      continue;
    }
    const host = record(raw[platform]);
    if (
      !host ||
      !['waiting', 'running', 'done'].includes(host.state as string) ||
      !(host.currentApp === null || isUpdateAppId(host.currentApp)) ||
      !(host.phase === null || APP_UPDATE_PHASES.includes(host.phase as AppUpdatePhase))
    )
      return null;
    hosts[platform] = {
      state: host.state as AppUpdateHostProgress['state'],
      currentApp: host.currentApp as UpdateAppId | null,
      phase: host.phase as AppUpdateHostProgress['phase'],
    };
    if (typeof host.phaseSince === 'string' && !Number.isNaN(Date.parse(host.phaseSince)))
      hosts[platform].phaseSince = new Date(Date.parse(host.phaseSince)).toISOString();
  }
  return hosts;
}

/** Same Linux identity as app_update_common.process_identity; null when it cannot be read. */
export function processIdentity(pid: number): string | null {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    const start = stat
      .slice(stat.lastIndexOf(')') + 2)
      .trim()
      .split(/\s+/)[19];
    const boot = fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8').trim();
    return start && boot ? `${boot}:${start}` : null;
  } catch {
    return null;
  }
}

export class LegacyAppUpdateReader {
  private job: AppUpdateJob | null = null;
  private readonly directory: string;
  private lockHeld = false;
  private readonly deps = { persist: true, now: Date.now, identity: processIdentity };

  constructor(ccsDir: string) {
    this.directory = path.join(ccsDir, 'app-updates');
  }

  getStatus(): { job: AppUpdateJob | null } {
    this.restore();
    return { job: this.job };
  }
  private lockOwnerAlive(file = path.join(this.directory, 'dashboard-update.lock')): boolean {
    try {
      const stat = fs.statSync(file);
      const malformedAlive = (): boolean => this.deps.now() - stat.mtimeMs < 5000;
      if (stat.size > 512) return malformedAlive();
      let lock: Record<string, unknown> | undefined;
      try {
        lock = record(JSON.parse(fs.readFileSync(file, 'utf8')));
      } catch {
        return malformedAlive();
      }
      const owner = lock?.pid;
      if (typeof owner !== 'number' || !Number.isSafeInteger(owner) || owner < 1)
        return malformedAlive();
      const identity = this.deps.identity(owner);
      if (typeof lock?.identity === 'string' && identity !== null && lock.identity !== identity)
        return false;
      try {
        process.kill(owner, 0);
        return true;
      } catch (error) {
        return (error as NodeJS.ErrnoException).code !== 'ESRCH';
      }
    } catch (error) {
      return (error as NodeJS.ErrnoException).code !== 'ENOENT';
    }
  }

  private restore(): void {
    if (this.deps.persist === false || this.lockHeld) return;
    try {
      const file = path.join(this.directory, 'dashboard-job.json');
      if (fs.statSync(file).size > MAX_OUTPUT) return;
      const raw = record(record(JSON.parse(fs.readFileSync(file, 'utf8')))?.job);
      if (
        !raw ||
        typeof raw.id !== 'string' ||
        !/^[a-f0-9-]{36}$/i.test(raw.id) ||
        !['running', 'completed', 'failed'].includes(raw.state as string) ||
        typeof raw.startedAt !== 'string' ||
        raw.startedAt.length > 40 ||
        !Number.isFinite(Date.parse(raw.startedAt)) ||
        !Array.isArray(raw.results) ||
        raw.results.length > EXPECTED_RESULTS
      )
        return;
      const results: AppUpdateResult[] = [];
      for (const value of raw.results) {
        const row = record(value);
        if (
          !row ||
          typeof row.appId !== 'string' ||
          !Object.prototype.hasOwnProperty.call(UPDATE_APP_LABELS, row.appId) ||
          !PLATFORMS.includes(row.platform as UpdatePlatform)
        )
          continue;
        // Saved rows keep only their words; map them back through rowMessage so a
        // row whose words name the app ("Quit Codex Desktop to finish its update")
        // restores as itself instead of as helper_invalid. Its saved strays and adapter
        // parts take part, so a row whose words name a stray or a held part restores too.
        const appId = row.appId as UpdateAppId;
        const code =
          (Object.keys(MESSAGES) as MessageCode[]).find(
            (candidate) =>
              rowMessage(row, appId, candidate) === row.message ||
              (appId === 'antigravity-cli' &&
                candidate === 'updated' &&
                MESSAGES.updated === row.message) ||
              (LEGACY_MESSAGES[candidate] !== undefined &&
                LEGACY_MESSAGES[candidate] === row.message)
          ) ?? 'helper_invalid';
        const restored = normalizeAppUpdateResults(
          JSON.stringify({ results: [{ ...row, messageCode: code }] }),
          row.platform as UpdatePlatform
        ).find((result) => result.appId === row.appId);
        if (restored) results.push(restored);
      }
      const abandoned = raw.state === 'running' && !this.lockOwnerAlive();
      this.job = {
        id: raw.id,
        state: abandoned ? 'failed' : (raw.state as AppUpdateJob['state']),
        startedAt: new Date(raw.startedAt).toISOString(),
        finishedAt: abandoned
          ? new Date(this.deps.now()).toISOString()
          : typeof raw.finishedAt === 'string' &&
              raw.finishedAt.length <= 40 &&
              Number.isFinite(Date.parse(raw.finishedAt))
            ? new Date(raw.finishedAt).toISOString()
            : null,
        activePlatform:
          !abandoned && PLATFORMS.includes(raw.activePlatform as UpdatePlatform)
            ? (raw.activePlatform as UpdatePlatform)
            : null,
        hosts: abandoned ? null : restoreHosts(raw.hosts),
        cancelRequested: raw.cancelRequested === true,
        results,
        expectedResults:
          typeof raw.expectedResults === 'number' &&
          Number.isSafeInteger(raw.expectedResults) &&
          raw.expectedResults >= results.length &&
          raw.expectedResults <= EXPECTED_RESULTS
            ? raw.expectedResults
            : null,
      };
    } catch {
      /* An absent/corrupt result never triggers or replays an update. */
    }
  }
}
