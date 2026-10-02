import { ConfigError } from '../../errors/error-types';

export const UPDATE_APP_LABELS = {
  'antigravity-cli': 'Antigravity CLI',
  'muse-code': 'Muse Code',
  omp: 'OMP',
  'codex-cli': 'Codex CLI',
  'codex-desktop': 'Codex Desktop',
  'claude-code': 'Claude Code',
  'claude-desktop': 'Claude Desktop',
} as const;
export type UpdateAppId = keyof typeof UPDATE_APP_LABELS;
export type UpdatePlatform = 'ubuntu' | 'mac' | 'windows';
export type UpdateResultStatus =
  | 'updated'
  | 'current'
  | 'not_installed'
  | 'failed'
  | 'restart_failed';
export interface AppUpdateResult {
  appId: UpdateAppId;
  appLabel: string;
  platform: UpdatePlatform;
  status: UpdateResultStatus;
  previousVersion: string | null;
  version: string | null;
  manager: string | null;
  message: string;
  updateAttempted: boolean;
  restartedProcesses: number;
  forcedStops: number;
  restartTargets: Array<{
    kind: 'tmux' | 'terminal' | 'windows-terminal';
    server?: string;
    session?: string;
  }>;
}
export interface AppUpdateJob {
  id: string;
  state: 'running' | 'completed' | 'failed';
  startedAt: string;
  finishedAt: string | null;
  activePlatform: UpdatePlatform | null;
  results: AppUpdateResult[];
  /**
   * How many results the job will produce (apps times platforms attempted),
   * fixed when it starts; null for a job saved before this field existed.
   */
  expectedResults: number | null;
}

export const MESSAGES = {
  updated:
    'Updated and restarted running instances. CLI sessions reopen idle or resume an explicitly selected session.',
  current: 'Already current; its processes were left running.',
  not_installed: 'This app is not installed on this computer.',
  unsupported: 'This installation has no supported unattended updater.',
  restart_context: 'A running instance cannot be restarted safely; no update was attempted.',
  update_failed: 'The app update failed; check its supported installer.',
  version_unknown: 'The installed version could not be verified after the update.',
  restart_failed: 'The update finished, but an original instance could not restart.',
  host_unavailable: 'This computer could not complete the update request.',
  helper_invalid: 'The update helper returned an unsupported result.',
  busy: 'Another app update is already running on this computer.',
  timeout: 'The update did not finish within its time limit.',
  signature_failed: 'The downloaded app did not pass its publisher verification.',
} as const;
export type MessageCode = keyof typeof MESSAGES;
export const PLATFORMS: UpdatePlatform[] = ['ubuntu', 'mac', 'windows'];
/** Every platform yields one result per app, a failure row included when its host is down. */
export const EXPECTED_RESULTS = PLATFORMS.length * Object.keys(UPDATE_APP_LABELS).length;
const STATUSES: UpdateResultStatus[] = [
  'updated',
  'current',
  'not_installed',
  'failed',
  'restart_failed',
];
const MANAGERS = ['native', 'npm', 'brew', 'winget', 'msix', 'apt', 'official-download'];
export const MAX_OUTPUT = 64 * 1024;

export function record(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function safeVersion(value: unknown): string | null {
  return typeof value === 'string' &&
    value.length <= 64 &&
    /^\d+(?:\.[0-9A-Za-z-]+){1,5}(?:[+-][0-9A-Za-z.-]+)?$/.test(value)
    ? value
    : null;
}

export function failure(
  platform: UpdatePlatform,
  appId: UpdateAppId,
  code: MessageCode
): AppUpdateResult {
  return {
    appId,
    appLabel: UPDATE_APP_LABELS[appId],
    platform,
    status: 'failed',
    previousVersion: null,
    version: null,
    manager: null,
    message: MESSAGES[code],
    updateAttempted: false,
    restartedProcesses: 0,
    forcedStops: 0,
    restartTargets: [],
  };
}

/** Only fixed identifiers, bounded versions, enums and counters cross the helper boundary. */
export function normalizeAppUpdateResults(
  raw: string,
  platform: UpdatePlatform
): AppUpdateResult[] {
  let payload: Record<string, unknown> | undefined;
  try {
    if (Buffer.byteLength(raw, 'utf8') > MAX_OUTPUT)
      throw new ConfigError('App update result exceeded its limit.');
    payload = record(JSON.parse(raw));
  } catch {
    payload = undefined;
  }
  const rows = Array.isArray(payload?.results) ? payload.results : [];
  return (Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]).map((appId) => {
    const matches = rows.map(record).filter((row) => row?.appId === appId);
    const row = matches.length === 1 ? matches[0] : undefined;
    if (!row || !STATUSES.includes(row.status as UpdateResultStatus))
      return failure(platform, appId, 'helper_invalid');
    if (
      typeof row.messageCode !== 'string' ||
      !Object.prototype.hasOwnProperty.call(MESSAGES, row.messageCode)
    )
      return failure(platform, appId, 'helper_invalid');
    const code = row.messageCode as MessageCode;
    const count =
      typeof row.restartedProcesses === 'number' &&
      Number.isSafeInteger(row.restartedProcesses) &&
      row.restartedProcesses >= 0 &&
      row.restartedProcesses <= 10000
        ? row.restartedProcesses
        : 0;
    const forced =
      typeof row.forcedStops === 'number' &&
      Number.isSafeInteger(row.forcedStops) &&
      row.forcedStops >= 0 &&
      row.forcedStops <= 10000
        ? row.forcedStops
        : 0;
    const targets: AppUpdateResult['restartTargets'] = [];
    for (const candidate of (Array.isArray(row.restartTargets) ? row.restartTargets : []).slice(
      0,
      100
    )) {
      const target = record(candidate);
      if (target?.kind === 'terminal' || target?.kind === 'windows-terminal')
        targets.push({ kind: target.kind });
      else if (
        target?.kind === 'tmux' &&
        typeof target.server === 'string' &&
        /^ccs-updates-[a-f0-9]{12}$/.test(target.server) &&
        typeof target.session === 'string' &&
        /^ccs-updated-(?:antigravity-cli|muse-code|omp|codex-cli|claude-code)-[1-9]\d{0,3}$/.test(
          target.session
        )
      )
        targets.push({ kind: 'tmux', server: target.server, session: target.session });
    }
    if (
      row.status === 'updated' &&
      (!safeVersion(row.version) ||
        typeof row.manager !== 'string' ||
        !MANAGERS.includes(row.manager))
    )
      return failure(platform, appId, 'helper_invalid');
    return {
      appId,
      appLabel: UPDATE_APP_LABELS[appId],
      platform,
      status: row.status as UpdateResultStatus,
      previousVersion: safeVersion(row.previousVersion),
      version: safeVersion(row.version),
      manager:
        typeof row.manager === 'string' && MANAGERS.includes(row.manager) ? row.manager : null,
      message: MESSAGES[code],
      updateAttempted: row.updateAttempted === true,
      restartedProcesses: count,
      forcedStops: forced,
      restartTargets: targets,
    };
  });
}
