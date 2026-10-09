import { ConfigError } from '../../errors/error-types';
import { DASHBOARD_HOSTS, HOST_OS, type DashboardHost } from './dashboard-hosts';

export const UPDATE_APP_LABELS = {
  'antigravity-cli': 'Antigravity CLI',
  'muse-code': 'Muse Code',
  omp: 'OMP',
  'codex-cli': 'Codex CLI',
  'codex-desktop': 'Codex Desktop',
  'claude-code': 'Claude Code',
  'claude-desktop': 'Claude Desktop',
  't3-code': 'T3 Code',
} as const;
export type UpdateAppId = keyof typeof UPDATE_APP_LABELS;
/** The fixed computers Update apps reaches; Nas1 is a second Ubuntu with its own id. */
export type UpdatePlatform = DashboardHost;
export type UpdateResultStatus =
  | 'updated'
  | 'current'
  | 'not_installed'
  | 'failed'
  | 'restart_failed'
  | 'skipped'
  | 'unknown'
  | 'action_required'
  /** Not installed on purpose: the newest build is waiting for a review (Antigravity CLI). */
  | 'held';
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
  /** The newest build a `held` row did not install, when the helper could read it. */
  heldVersion?: string;
  restartTargets: Array<{
    kind: 'tmux' | 'terminal' | 'windows-terminal' | 'desktop' | 'systemd';
    server?: string;
    session?: string;
    service?: 't3code.service';
    delaySeconds?: 30;
  }>;
}
/** One computer's live progress: every computer runs at the same time. */
export interface AppUpdateHostProgress {
  state: 'waiting' | 'running' | 'done';
  /** The app this computer works on now; null between apps or while it checks them all. */
  currentApp: UpdateAppId | null;
  /**
   * 'checking' while read-only checks run, 'updating' once an app's update
   * starts, 'downloading' while a desktop app's package downloads.
   */
  phase: AppUpdatePhase | null;
  /** When the current phase began (ISO time), so the page can show how long it has run. */
  phaseSince?: string | null;
}
export type AppUpdatePhase = 'checking' | 'updating' | 'downloading';
export const APP_UPDATE_PHASES: readonly AppUpdatePhase[] = ['checking', 'updating', 'downloading'];
export interface AppUpdateJob {
  id: string;
  state: 'running' | 'completed' | 'failed';
  startedAt: string;
  finishedAt: string | null;
  /** The first computer still running (kept for older clients); see hosts. */
  activePlatform: UpdatePlatform | null;
  /** Per-computer progress; null for a job saved before hosts ran in parallel. */
  hosts: Record<UpdatePlatform, AppUpdateHostProgress> | null;
  /**
   * True once a cancel is acknowledged. The running host batch still finishes;
   * every queued app is reported as skipped. Never promises an undo.
   */
  cancelRequested: boolean;
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
  skipped_cancelled: 'Skipped: cancelled',
  host_unknown: 'Unknown: this computer is not reachable.',
  readiness_unknown: 'Unknown: the readiness check could not run.',
  quit_first: 'Quit the app, then run Update apps again.',
  check_in_app: 'The download was blocked; open the app to check for updates.',
  codex_busy: 'Codex is busy with a task; run Update apps again when it is idle.',
  check_timeout: 'Check timed out: the app did not answer in time.',
  host_timeout: 'Timed out: this computer did not finish in time.',
  held_for_review:
    'Update held: the newest Antigravity version is waiting for a switching review; the reviewed version stays installed.',
  held_unchecked:
    'Update held: the newest Antigravity version could not be checked against the switching review, so nothing was installed.',
  updated_unreviewed:
    'Updated, but this Antigravity version has no switching review yet; Antigravity switching is paused until it is reviewed.',
  t3_updated: 'Updated T3 Code and its installed server runtime.',
  t3_restart_scheduled:
    'Updated; the T3 server restart is scheduled about 30 seconds after this update job finishes. Running T3 threads will disconnect.',
  desktop_reopened: 'Updated: it closed, installed the update and reopened.',
} as const;
export type MessageCode = keyof typeof MESSAGES;
export const PLATFORMS: UpdatePlatform[] = [...DASHBOARD_HOSTS];
/** Every platform yields one result per app, a failure row included when its host is down. */
export const EXPECTED_RESULTS = PLATFORMS.length * Object.keys(UPDATE_APP_LABELS).length;
const STATUSES: UpdateResultStatus[] = [
  'updated',
  'current',
  'not_installed',
  'failed',
  'restart_failed',
  'skipped',
  'unknown',
  'action_required',
  'held',
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

/** A queued app dropped by an acknowledged cancel: never failed, never updated. */
export function skipped(platform: UpdatePlatform, appId: UpdateAppId): AppUpdateResult {
  return {
    appId,
    appLabel: UPDATE_APP_LABELS[appId],
    platform,
    status: 'skipped',
    previousVersion: null,
    version: null,
    manager: null,
    message: MESSAGES.skipped_cancelled,
    updateAttempted: false,
    restartedProcesses: 0,
    forcedStops: 0,
    restartTargets: [],
  };
}

/** The outcome could not be determined: the check never ran, so it is not a failure. */
export function unknown(
  platform: UpdatePlatform,
  appId: UpdateAppId,
  code: 'host_unknown' | 'readiness_unknown' | 'host_timeout' | 'check_timeout'
): AppUpdateResult {
  return {
    appId,
    appLabel: UPDATE_APP_LABELS[appId],
    platform,
    status: 'unknown',
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

export function isUpdateAppId(value: unknown): value is UpdateAppId {
  return (
    typeof value === 'string' && Object.prototype.hasOwnProperty.call(UPDATE_APP_LABELS, value)
  );
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
    return normalizeAppUpdateRow(matches.length === 1 ? matches[0] : undefined, appId, platform);
  });
}

/** A row's words; "quit first" names the app, so the page says exactly what to quit. */
export function messageFor(code: MessageCode, appId: UpdateAppId): string {
  if (code === 'quit_first')
    return `Quit ${UPDATE_APP_LABELS[appId]} to finish its update, then run Update apps again.`;
  return MESSAGES[code];
}

/** One helper row for a known app; anything unexpected becomes a fixed helper_invalid row. */
export function normalizeAppUpdateRow(
  row: Record<string, unknown> | undefined,
  appId: UpdateAppId,
  platform: UpdatePlatform
): AppUpdateResult {
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
  // The Linux computers (Ubuntu and Nas1) restart T3 through its systemd service;
  // the Mac and Windows restart its desktop app. Windows alone also closes and
  // reopens a running Codex or Claude desktop app; the Mac asks to quit first.
  const linux = HOST_OS[platform] === 'linux';
  const reopensDesktop =
    HOST_OS[platform] === 'windows' && (appId === 'codex-desktop' || appId === 'claude-desktop');
  const targets: AppUpdateResult['restartTargets'] = [];
  for (const candidate of (Array.isArray(row.restartTargets) ? row.restartTargets : []).slice(
    0,
    100
  )) {
    const target = record(candidate);
    if (target?.kind === 'terminal' || target?.kind === 'windows-terminal')
      targets.push({ kind: target.kind });
    else if (target?.kind === 'desktop' && ((appId === 't3-code' && !linux) || reopensDesktop))
      targets.push({ kind: 'desktop' });
    else if (
      target?.kind === 'systemd' &&
      appId === 't3-code' &&
      linux &&
      target.service === 't3code.service' &&
      target.delaySeconds === 30
    )
      targets.push({ kind: 'systemd', service: 't3code.service', delaySeconds: 30 });
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
  // Only the Antigravity CLI is ever held, and only with a hold message.
  const held = row.status === 'held';
  if (
    ((code === 't3_updated' || code === 't3_restart_scheduled') &&
      (appId !== 't3-code' || row.status !== 'updated')) ||
    (code === 't3_restart_scheduled' &&
      (!linux || !targets.some((target) => target.kind === 'systemd'))) ||
    (code === 'desktop_reopened' && (!reopensDesktop || row.status !== 'updated')) ||
    held !== (code === 'held_for_review' || code === 'held_unchecked') ||
    (held && appId !== 'antigravity-cli') ||
    (code === 'updated_unreviewed' && (appId !== 'antigravity-cli' || row.status !== 'updated'))
  )
    return failure(platform, appId, 'helper_invalid');
  const heldVersion = held && code === 'held_for_review' ? safeVersion(row.heldVersion) : null;
  return {
    appId,
    appLabel: UPDATE_APP_LABELS[appId],
    platform,
    status: row.status as UpdateResultStatus,
    previousVersion: safeVersion(row.previousVersion),
    version: safeVersion(row.version),
    manager: typeof row.manager === 'string' && MANAGERS.includes(row.manager) ? row.manager : null,
    message: messageFor(code, appId),
    updateAttempted: row.updateAttempted === true,
    restartedProcesses: count,
    forcedStops: forced,
    ...(heldVersion ? { heldVersion } : {}),
    restartTargets: targets,
  };
}
