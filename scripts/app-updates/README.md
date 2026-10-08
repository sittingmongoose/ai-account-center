# Fixed installed-app updates

The dashboard's authenticated `POST /api/app-updates/start` accepts exactly `{}`.
It starts one asynchronous, persisted job and returns immediately. Authenticated
`GET /api/app-updates/status` reads state only. Duplicate active jobs are rejected.
The server invokes only Ubuntu locally and the fixed Mac and Windows
SSH aliases (`APP_UPDATE_SSH_HOSTS`); callers cannot supply hosts, commands, app IDs, paths or download URLs.

## Every computer at once, and never stuck

Ubuntu, Mac and Windows run **at the same time**; each computer still updates
its own apps one at a time (one installer per host). The job ends when the
slowest computer finishes, so a busy or unreachable host never holds the others
back. Per-computer progress is live: the helper prints one JSON line per event
(`{"event":"app","appId":...,"phase":"checking"|"updating"}` and
`{"event":"result","result":{...}}`) when the dashboard sets
`AAC_UPDATE_PROGRESS=1`, then the usual final `{"results":[...]}` line; an older
dashboard gets exactly one document. Windows relays its task child's progress
through a nonce-bound private file. A cancel is a `cancel` line on the helper's
stdin (forwarded over ssh; Windows forwards it to its task child through a
nonce-bound cancel file): the app running at that moment finishes and every app
not yet started reports `skipped`.

Nothing can wait forever:

- every helper command runs in its own session with empty stdin and no
  controlling terminal, so a prompt fails at once instead of waiting; at its
  timeout the command and its helper processes are stopped (`timeout`);
- version probes run side by side (20 s each); a probe that runs out of time
  reports `unknown` / `check_timeout` ("Check timed out"), never "not installed";
- the Ubuntu Codex bridge waits at most 30 s for the account-switch lock and
  60 s for shared Codex work to go idle (it used to wait up to 15 minutes, which
  stalled the whole job on a VM where Codex is always busy); a busy Codex reports
  `action_required` / `codex_busy` and, for the CLI, keeps its pending marker so
  the next click restarts the daemon on the new version once Codex is idle;
- each helper stops starting apps after 15 minutes (`timeout` rows), and the
  dashboard stops a computer after 18 minutes (helper sync included): every app
  without a result then reads `unknown` / `host_timeout` ("Timed out") while
  rows that did arrive keep their real results.

The Python helper defaults to **read-only inventory**. Only `--apply` updates or
restarts apps. It detects the active installation, skips absent apps, and returns
one bounded, whitelist result for each of the eight fixed apps. It does not copy,
modify or export account credentials/configuration.

| App | Detected installation and supported update |
| --- | --- |
| Antigravity CLI | Active native `agy update`, only to a build in the switching review set (held otherwise, see below) |
| Muse Code | Active user launcher, fixed Meta installer with `MUSE_UPGRADE_MODE=1` and no PATH modification; bash on Ubuntu/Mac, PowerShell on Windows |
| OMP | Active standalone `omp update`, installation directory first in PATH |
| Codex CLI | Active native `codex update`; Windows active npm installation uses `@openai/codex@latest` with its existing global prefix |
| Claude Code | Active native `claude update` |
| Codex Desktop | Ubuntu signed-repository `chatgpt` package only; Mac verified OpenAI DMG; Windows same-publisher/same-identity MSIX |
| Claude Desktop | Mac verified Anthropic ZIP from the publisher's own `RELEASES.json` feed (the old claude.ai redirect answers 403 to non-browser clients); Windows same-publisher/same-identity MSIX; absent Ubuntu installations are skipped |
| T3 Code | One `t3-code` row per host covers its nightly desktop/bundled server and any installed standalone runtime; Ubuntu uses the native updater and a detached delayed server restart; Mac verifies SHA512, codesign and notarization before a bundle swap; Windows verifies SHA512 and the T3 Tools Inc Authenticode publisher before the silent NSIS installer |

Official methods: [Antigravity installer](https://antigravity.google/cli/install.sh),
[Meta installer](https://dev.meta.ai/install.sh),
[OMP updater](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/cli/update-cli.ts),
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code setup](https://code.claude.com/docs/en/setup),
[OpenAI Linux package](https://learn.chatgpt.com/docs/linux/linux-app),
[OpenAI app update management](https://learn.chatgpt.com/docs/manage-app-updates).

## T3 Code nightly updates

T3's desktop and server are one app ID because they ship together on the same
nightly release. A Mac with both the desktop and a standalone runtime updates
both under that row; neither absent desktops nor absent standalone runtimes are
installed. The full nightly version is read from the Mac bundle, the native
runtime's bounded `--version` probe, or Windows' bundled ASAR `package.json`.
The Windows PE ProductVersion drops the nightly suffix and cannot identify a
nightly accurately.

Releases come only from [pingdotgg/t3code](https://github.com/pingdotgg/t3code/releases).
The exact asset and nightly version must match its bounded `nightly-mac.yml`
or `nightly.yml` SHA512 entry. Desktop downloads allow at most 800 MiB and ten
minutes, bounded by the host's remaining budget. Mac additionally requires
`com.t3tools.t3code`, signing team `ARK85ZXQ4Z`, strict/deep codesign verification
and `spctl` assessment; it stages beside the installed app before an atomic
rename and retains the old bundle for rollback. Before extraction it rejects
entries beneath symlink ancestors and resolves symlink chains to reject escapes
and cycles; Electron's internal `Versions/Current -> A` framework links remain
supported. Windows requires a valid
Authenticode signature from `T3 Tools Inc`, uses `/S`, and restores a private
copy of the previous installation if installation or relaunch verification fails.
Both desktops close only the captured T3 process family, reopen in the user's
desktop session and check `http://127.0.0.1:3773/`. The Windows flow reuses the
existing `CCS App Updates` InteractiveToken task and process/session guards.
This explicit T3 close/swap/reopen flow is the exception to Codex and Claude's
Mac/Windows quit-first behavior described below.

On Ubuntu, `t3 update <version> --channel nightly` runs with empty, non-TTY stdin
and **without `--yes`**. Its native updater verifies the runtime and rewrites the
service definition while leaving the running server on its old version. T3 is
the last app on each host, and Ubuntu schedules a separate transient user service
that waits for the update helper to exit, every dashboard host to finish and the
dashboard job lock to be released, then waits 30 seconds. AAC passes its resolved
`app-updates` state directory explicitly, including custom `--config-dir`,
`CCS_DIR` and legacy `CCS_HOME` configuration. Without that argument the helper
uses `~/.ccs/app-updates`. A dashboard worker treats a missing job file as active.
It holds both the per-user `helper-update.lock` and that dashboard lock through
the final check, restart and health verification before releasing them. It
restarts **only `t3code.service`**;
`ccs-dashboard.service` is never stopped or restarted. A new update job delays
the restart again. The detached worker has an 18-minute wait limit and verifies
HTTP health after restarting.

The immediate result says the server restart is **scheduled**, with zero
synchronously restarted processes and a fixed `systemd` restart target; it
does not claim the new server is already running. Restarting T3 disconnects its
active agent threads and clients. A version-only pending marker remains if
scheduling, restart or health verification fails; a later explicit click retries.
Restart intent is written before runtime installation, so a post-install version
probe timeout cannot lose it. On a later explicit run, a read-only service PID
and executable-path check clears intent if the running service already uses the
installed version; otherwise the delayed restart is scheduled again.
Inspect `journalctl --user -u 'aac-t3-restart-*'` for the detached outcome.
No status read schedules or retries anything. Separate Muse ACP and ZCode ACP
adapters remain under their existing auto-updaters and are outside this job.

Hosts still run in parallel. Installers stay sequential within each host:
T3's replacement closes its bundled server/process family, Windows installers
can hold executable/package files, and Codex already shares a daemon with its
desktop. Overlapping those operations would weaken stop/restart and rollback
guarantees.

Windows Muse is detected at `%LOCALAPPDATA%\Programs\muse\muse.cmd` (including
the extensionless `muse` shim beside it). Its `.muse-launcher.ps1 --version` is
probed directly with auto-update disabled, retaining the full `1.4.3-R5018.1`
build version. The old generic `.cmd` rejection reported this official install
as unsupported before probing it. Its fixed
[PowerShell installer](https://dev.meta.ai/install.ps1) now runs with
`MUSE_UPGRADE_MODE=1`, `MUSE_NO_MODIFY_PATH=1` and `MUSE_INSTALL_DIR` bound to the
detected official directory. Unrelated Muse shims remain unsupported.

### Live verification after deployment

Only the owner's explicitly authorized Update apps click should exercise real
installers. Before that, read-only `--inventory --platform <host>` should show
eight rows, full T3 nightly versions and Windows Muse's native manager/version.
After an authorized click, confirm 24 result rows and simultaneous host progress.
When T3 updates, verify Mac/Windows bundle versions and port 3773 health, then
wait until the completed job's scheduled Ubuntu restart has finished; inspect
`t3 service status`, the detached unit journal, port 3773 and that
`ccs-dashboard.service` remained active. Do not run this live check while agent
work that must survive a T3 restart is in progress.

**Antigravity CLI review hold (all three computers).** Account switching works only
with native builds listed in `scripts/antigravity/runtime/release.json`
(`reviewedNatives`), and `agy update` always installs the newest build. Before it
runs, the helper reads the official release manifest for this computer (the fixed
`.../manifests/<os>_<arch>.json` the CLI's own updater and installer use, 16 KB at
most) and compares its version with the reviewed versions the dashboard passes as
`--agy-reviewed` (on Windows they travel in the task request file; a manual Ubuntu
run reads the packaged release file):

- newest build reviewed: the normal update runs;
- newest build already installed: `current`, and `agy update` is not run;
- newest build not reviewed: nothing is installed, the row is `held` with
  `held_for_review` and names that build (`heldVersion`), and the dashboard shows
  "Update held: Antigravity X is waiting for a switching review";
- review list or manifest unreadable: nothing is installed either
  (`held`/`held_unchecked`);
- an update that still lands outside the list (a release published between the
  check and `agy update`) reports `updated_unreviewed`, never silently.

The hold covers Update all only. The CLI also updates itself in the background
during ordinary runs, which no dashboard button can hold; switching then pauses
until a review, as before.

Inventory disables background CLI updates (`MUSE_NO_AUTO_UPDATE=1`,
`AGY_CLI_DISABLE_AUTO_UPDATE=true`, `DISABLE_AUTOUPDATER=1`). This matters because
Muse otherwise checks/updates even during a `--version` invocation.

Restart scope is the **same user's exact app executable/package family**,
validated against PID creation identity. Generic Node/Python/terminal processes
are never selected. Desktop updates preserve existing absolute profile directory
arguments. Mac uses verified atomic bundle replacement; Windows runs in an
interactive task and Add-AppxPackage preserves MSIX LocalState. Desktop apps on
Mac and Windows for Codex and Claude are **never quit, closed, restarted or killed**: the updater
does not even ask them to quit. A running app with a newer version reports the
actionable `action_required`/`quit_first` row ("Quit Codex Desktop to finish
its update") within seconds, before any package download: Windows reads only
the published MSIX manifest over HTTP ranges (three requests, under 1 MB),
Claude on Mac reads its release feed, and Codex on Mac remembers the version of
the last verified DMG by its HEAD fingerprint. Only when that version is
unknown does the full download decide, shown live on the page as
"Downloading" with its elapsed time. The app is checked again right before
the install, so one opened during the download is left alone (Ubuntu desktops
keep the previous bounded terminate-and-relaunch flow). CLI forced stops stay
bounded, app-family-only and counted.
A successful result verifies
that replacement processes exist.

Updated interactive CLIs open new idle terminal instances. Ubuntu uses a private
`tmux -L ccs-updates-...` server; Mac uses Terminal; Windows uses Windows Terminal.
An explicitly selected valid session UUID may be resumed. Original prompts,
print/exec arguments, stdin and commands are never replayed. Existing cwd and
private auth environment stay in memory; Mac uses an owner-private one-shot Unix socket; Windows uses a local named
pipe restricted to the signed-in user SID to enter the new terminal. Original terminal scrollback and the old
conversation display do not migrate. Safe result `restartTargets` identifies new
terminal/tmux sessions; authentication environment is never serialized.

Ubuntu Codex updates additionally serialize against account switching via the
existing `.ccs-activation.lock` and use the established idle/startup-lock runtime
for the shared daemon and desktop app server. Busy Codex work is waited on for
at most 60 seconds, then reported as `codex_busy` (see above). SSH proxies use verified native client reconnect behavior:
Mac/Windows desktop transports recreate `exec codex app-server proxy` after exit.
The updater stops only captured proxies with SSH ancestry, then verifies old PIDs
are gone and replacement proxies execute the updated active binary. Unknown
unsupervised proxies yield an explicit restart failure. A private **version-only**
pending-restart marker permits a subsequent explicit click to retry after timeout.
No account authentication is changed.

On a Linux host without AAC installed, the bridge loads
`app_update_codex_runtime.cjs` from beside itself: the same stop/start runtime
bundled with `ws` and `proper-lockfile`, generated into `dist/app-updates` by
`bun run build:server` and synced with the helpers (a tree without it fails the
sync instead of sending a partial set). It carries no account activation code;
`scripts/verify-bundle.js` checks that. The VM keeps loading the runtime of its
installed package.

The native client reconnect behavior was checked read-only in installed app.asar
bundles on all three hosts and the [version-pinned proxy](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/stdio-to-uds/src/lib.rs)
and [startup lock](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/app-server-transport/src/transport/unix_socket.rs)
implementations. Updating Codex can restart its coupled local desktop/server
family even when only one Codex package changed.

## Deployment and setup

The server syncs every `app_update_*.py`, `app_updates.py`, and
`app_update_codex.cjs` from its own build into `~/.ccs/app-updates` on
Mac/Windows before each run, checksum-gated so up-to-date hosts only answer one
hash query; a sync failure leaves the deployed helpers untouched. Python must be
available. On Windows register `install-windows-task.ps1` once as the signed-in
user: its fixed `CCS App Updates` InteractiveToken/Limited task runs the private
helper. Registration does **not** run an update. The helper queues that task
from SSH session zero, so the user's interactive session must be signed in.
Existing CCS Bar tasks are untouched.

The Mac sync extracts with `/bin/mkdir`, `/bin/chmod` and `/usr/bin/tar`
(macOS has no `/usr/bin/chmod`; that path silently broke every Mac sync until
2026-10-06, so Mac kept running its Oct-2 helpers).

Results are `updated`, `current`, `not_installed`, `failed`, `restart_failed`,
`skipped`, `unknown`, or `action_required`, with bounded versions, fixed message
codes, restart counts/forced-stop counts and safe terminal target metadata. Raw
package command output, process argv, environments, installer response bodies and
credentials are never returned. Host/helper execution locks prevent overlapping
work. Package operation timeout or a relaunch refusal is reported honestly and
does not prevent other apps/hosts from producing their own results. Status reads
never retry an interrupted job.

`action_required` is never a failure: the job completes and the row tells the
user exactly what to do. Codex and Claude desktops on Mac and Windows that are running report
`quit_first` (they are never asked to quit): nothing is swapped or
deployed while anything runs, and the next click after the user quits updates
cleanly. MSIX deployments rejected for apps that need closing also report
`quit_first`. Codex/Claude desktop downloads refused by a bot challenge (HTTP 403 or a
challenge page) report `check_in_app` after bounded retries. Desktop downloads
allow 2 GiB and 10-minute timeouts; Codex/Claude Mac and Windows desktops are never
force-stopped (Ubuntu desktops keep the previous terminate-and-relaunch flow).
Claude's Mac update reads the publisher's own `RELEASES.json` feed on
downloads.claude.ai and checks its version before any package download, so a
current app fetches nothing; its ZIP is extracted with `ditto` and passes the
same codesign/TeamID verification as the DMG flow.
Windows npm updates first ask the registry whether anything is newer, then run
`node npm-cli.js` directly (never through `cmd /s /c`, which mangles spaced
paths) after stopping mapped instances first, since Windows cannot replace a
running npm tree.

## Fixture verification

`python3 -m unittest discover -s tests/unit/app-updates -p '*_test.py' -v`
uses disposable files and a compiled native stand-in only. The PTY fixture
atomically replaces fake version 1 with version 2, confirms the original PID exited,
new version 2 runs in a real tmux PTY with no original prompt, private environment
is preserved, and an unrelated process remains running. No real app update runs.

Disposable native v1→v2 fixtures also passed through Mac Terminal and Windows
Terminal. Windows read-only InteractiveLimited probing confirmed the running
OMP mapped-drive cwd and 45 private environment keys are readable in Session1.
Its restart fixture verified console attachment, zero work arguments, retained
private environment, old PID exit and preservation of an unrelated process.
No real installed app was updated or restarted during these proofs.

Bun tests in `tests/unit/app-updates` and the app-update web-server test files
cover safe normalization, fixed invocations, authentication/origin/body checks,
async job coalescing, cross-process persistence/locks, queued Codex idle waits,
proxy reconnect coordination and retry markers. Real package replacement and real
app restarts are intentionally exercised only after the user clicks the button.
