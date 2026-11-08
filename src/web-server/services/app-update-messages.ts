/** Fixed row messages by code; helpers send only a code, never free text. */
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
  helper_sync_failed:
    'The update helpers could not be synchronized; no apps were updated on this computer.',
  busy: 'Another app update is already running on this computer.',
  timeout: 'The update did not finish within its time limit.',
  signature_failed: 'The downloaded app did not pass its publisher verification.',
  skipped_cancelled: 'Skipped: cancelled',
  host_unknown: 'Unknown: this computer is not reachable.',
  readiness_unknown: 'Unknown: the readiness check could not run.',
  quit_first: 'Quit the app, then run Update apps again.',
  in_use: 'The app is in use, so nothing changed; run Update apps again once it is closed.',
  source_disabled:
    'Its package source is turned off on this computer, so it cannot update here. An Ubuntu upgrade turns these sources off; turn it back on to update.',
  check_in_app: 'The download was blocked; open the app to check for updates.',
  codex_busy: 'Codex is busy with a task; run Update apps again when it is idle.',
  check_timeout: 'Check timed out: the app did not answer in time.',
  host_timeout: 'Timed out: this computer did not finish in time.',
  task_failed: "The Windows updater task stopped before reporting this app's result.",
  held_for_review:
    'Update held: the newest Antigravity version is waiting for a switching review; the reviewed version stays installed.',
  held_unchecked:
    'Update held: the newest Antigravity version could not be checked against the switching review, so nothing was installed.',
  updated_unreviewed:
    'Updated, but this Antigravity version has no switching review yet; Antigravity switching is paused until it is reviewed. Copies already open keep the old version until restarted.',
  runtime_unavailable:
    'The Antigravity switching runtime is not running or did not answer, so the managed update could not check for running sessions; nothing was installed.',
  runtime_paused:
    'Antigravity switching is paused on an unreviewed build, so the update ran without its runtime and did not finish; nothing running was stopped.',
  runtime_paused_blocked:
    'Antigravity switching is paused on an unreviewed build and its runtime is stopped; the update could not safely start without it (a pending switch recovery, a failed backup, or a changed runtime or build), so nothing was installed.',
  t3_updated: 'Updated T3 Code and its installed server runtime.',
  t3_restart_scheduled:
    'Updated; the T3 server restart is scheduled about 30 seconds after this update job finishes. Running T3 threads will disconnect. A release that upgrades a large T3 database can keep T3 offline for several minutes before it reconnects.',
  desktop_reopened: 'Updated: it closed, installed the update and reopened.',
  // Older helpers and saved rows: T3 sessions were the only background sessions recognised.
  t3_sessions_kept:
    'Updated and restarted running terminal instances. Running T3 sessions were left alone and keep the previous version until T3 starts them again.',
  background_sessions_kept:
    'Updated; running terminal instances reopened idle. Background sessions (T3 and other runs without a terminal) were left running and keep the previous version until they are started again.',
  codex_sessions_stopped:
    'Running background Codex CLI sessions were stopped for the Windows update; start those sessions again.',
  codex_force_failed:
    'The forced Codex CLI update failed; background sessions may have stopped. Start any interrupted sessions again.',
  zcode_release_unavailable:
    'The official ZCode release could not be checked; no update was attempted. Try Update apps again.',
  // Older helpers and saved rows: winget's answer, which never saw this package.
  store_no_newer:
    'Already current from the Microsoft Store: the Store offers no newer build to this computer yet.',
  store_update_in_app:
    'The Microsoft Store has a newer build that Update all cannot install yet; nothing changed. Use the update button in the app.',
  package_mismatch:
    'The published package is not the version its release feed names, so nothing was installed.',
  package_in_use:
    'Windows still reports the app package in use after the close; the update was attempted but did not install.',
} as const;
export type MessageCode = keyof typeof MESSAGES;

/** Fixed older wording remains readable in saved jobs. Never emitted for new results. */
export const LEGACY_MESSAGES: Partial<Record<MessageCode, string>> = {
  updated_unreviewed:
    'Updated, but this Antigravity version has no switching review yet; Antigravity switching is paused until it is reviewed.',
  runtime_paused_blocked:
    'Antigravity switching is paused on an unreviewed build and its runtime is stopped; the update could not safely start without it (a pending switch recovery, a process check that could not finish, or a failed backup), so nothing was installed.',
};
