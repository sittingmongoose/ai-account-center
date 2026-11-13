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
  restart_not_running: 'Updated, but Codex did not restart. Start it again when you need it.',
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
  model_catalog_unrefreshed:
    'Muse did not refresh its model catalog; the new models are not registered yet. Check Muse sign-in on this computer, then run Update apps again.',
  mac_keychain_locked:
    'The app could not access the login Keychain even in the Mac GUI session. Unlock the login Keychain or approve its access on the Mac, then run Update apps again.',
  mac_gui_unavailable:
    'The Mac login session is unavailable. Sign in to the Mac desktop, then run Update apps again.',
  mac_gui_cleanup_failed:
    'The Mac GUI update worker could not be stopped. Check the Mac before running Update apps again.',
  cursor_updated:
    'Updated Cursor CLI. Running agents keep their version until they are started again.',
  cursor_update_staged:
    'Cursor staged its update while an agent is running. Let the agent finish, then run Update apps again.',
  cursor_signin_needed:
    "Sign-in needed: Cursor refused the CLI's sign-in when it checked for updates (HTTP 401), so nothing changed. Run cursor-agent login in a terminal on this computer, then run Update apps again. AAC never stores or asks for credentials.",
  cursor_shadowed:
    "Cursor CLI's own install is at the version shown, but another cursor-agent comes first on PATH and runs instead. Remove that copy, or put Cursor's own folder first, then run Update apps again.",
  policy_blocked:
    "A Windows execution policy refused the app's launcher, so nothing changed. AAC runs it the way the app's own .cmd does (a Bypass for that one process); a Group Policy setting overrides that. Ask this computer's administrator to allow it, then run Update apps again.",
  wrong_prefix:
    'npm reported success, but it changed a copy T3 does not run; the copy T3 uses is unchanged. The details name both folders.',
  registry_mismatch:
    "npm installed a version other than the newest published one (an older or mirrored registry?), so the update did not complete. The details list each copy's version.",
  prefix_not_writable:
    "The adapters T3 runs are in a folder this account cannot change, so nothing was installed. Update them as that folder's owner, or reinstall them in a folder you own.",
  antigravity_acp_updated:
    'T3 installed and activated its newest Antigravity ACP runtime and refreshed its models. Existing sessions keep their version; new sessions use the update.',
  antigravity_acp_models_stale:
    'T3 installed and activated its newest Antigravity ACP runtime, but could not refresh its model list. Press Refresh provider status in T3.',
  antigravity_acp_signin_hold:
    "Sign-in needed: the newer Antigravity runtime cannot read the sign-in saved on this computer, so Update apps did not install it. Install it from T3's Antigravity settings when you can sign in again there.",
  antigravity_acp_signin_needed:
    "Updated, but Antigravity needs a new sign-in: the new runtime could not use the saved one. Sign in again in T3's Antigravity settings, then press Refresh provider status.",
  antigravity_acp_offer_unknown:
    "Unknown: neither T3 nor Google's release channel said which Antigravity ACP release is newest, so nothing was installed. Try Update apps again.",
  antigravity_acp_newer_kept:
    "Newer kept: T3's active Antigravity ACP runtime is newer than its pin.",
  antigravity_acp_activation_unsettled:
    "T3's Antigravity ACP activation kept changing. Update apps could not verify a stable newest runtime; run it again.",
  antigravity_acp_staged:
    'An earlier update staged Antigravity ACP. Run Update apps again with T3 running so T3 can install and activate its pin.',
  t3_unreachable:
    'Action required: the local T3 server or bundled CLI is unavailable. Start T3 and run Update apps again. AAC did not write its active runtime pointer.',
  t3_auth_failed:
    'Action required: T3 could not issue, authorize, or revoke the temporary update session. Check local T3 access, then run Update apps again.',
  t3_session_issue_unknown:
    'T3 may have issued an update session, but its reply never arrived and AAC could not find it to revoke; no install started. That session expires within 15 minutes.',
  t3_session_reply_lost:
    'T3 issued an update session, but AAC did not get a usable reply, so no install started. AAC found that session and revoked it.',
  t3_absent:
    'T3 Code is not installed, or has never run, on this computer; its Antigravity ACP runtime updates only through T3.',
  t3_not_ready:
    "T3's server did not answer in time (it may still be starting or upgrading its database), so nothing was installed. Run Update apps again once T3 is open.",
  t3_restart_pending:
    'T3 was updated in this run and its server restarts after the job, so Antigravity ACP is checked against the new T3 on the next Update apps.',
  t3_issuer_protection_failed:
    'T3 session issuer could not be protected against helper termination. No install started. Check the local T3 installation and process permissions.',
  t3_install_state_unknown: 'Install state unknown, check T3.',
  t3_session_revoke_failed:
    'T3 session revocation could not be confirmed; the session expires within 15 minutes. Check T3.',
  t3_install_timeout:
    'T3 installation timed out. Its installer may still be running. Check T3 before retrying.',
  't3_install_failed:protocol':
    'T3 returned an unsupported installation response. Update T3 and retry.',
  't3_install_failed:rpc':
    'T3 rejected the installation request. Check the provider installation in T3.',
  't3_install_failed:provider_unavailable':
    'No Antigravity provider supports managed installation in the local T3 server. Check its provider settings.',
  't3_install_failed:server_outdated':
    'The running T3 server pins an older Antigravity runtime than AAC. Update and restart T3 before retrying.',
  't3_install_failed:readiness_unknown':
    'The active T3 Antigravity runtime could not be verified. Check its installation in T3.',
  't3_install_failed:operation_changed':
    'The T3 installation operation changed while AAC was observing it. Check its status in T3.',
  't3_install_failed:installer':
    'T3 could not install Antigravity ACP. See the provider installation error in T3.',
  't3_install_failed:cancelled': 'The T3 Antigravity installation was cancelled.',
  't3_install_failed:verification':
    'T3 reported completion, but its active Antigravity runtime did not verify at or above its pin.',
  // Older helpers and saved rows: winget's answer, which never saw this package.
  store_no_newer:
    'Already current from the Microsoft Store: the Store offers no newer build to this computer yet.',
  store_update_in_app:
    'The Microsoft Store has a newer build that Update all cannot install yet; nothing changed. Use the update button in the app.',
  package_mismatch:
    'The published package is not the version its release feed names, so nothing was installed.',
  package_in_use:
    'Windows still reports the app package in use after the close; the update was attempted but did not install.',
  older_copy_first:
    'Not current: an older copy is first on the system PATH (apps and scripts run it).',
} as const;
export type MessageCode = keyof typeof MESSAGES;

/** Fixed older wording remains readable in saved jobs. Never emitted for new results. */
export const LEGACY_MESSAGES: Partial<Record<MessageCode, string>> = {
  model_catalog_unrefreshed:
    'The new models are not registered yet. Open Muse once on this computer, then run Update apps again.',
  updated_unreviewed:
    'Updated, but this Antigravity version has no switching review yet; Antigravity switching is paused until it is reviewed.',
  runtime_paused_blocked:
    'Antigravity switching is paused on an unreviewed build and its runtime is stopped; the update could not safely start without it (a pending switch recovery, a process check that could not finish, or a failed backup), so nothing was installed.',
  antigravity_acp_updated:
    'T3 installed and activated its pinned Antigravity ACP runtime. Existing sessions keep their version; new sessions use the update.',
  t3_session_issue_unknown:
    'No T3 session in time; no install started. One session may stay valid up to 15 minutes. Check T3.',
};
