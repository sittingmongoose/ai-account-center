# Fixed installed-app updates

The dashboard's authenticated `POST /api/app-updates/start` accepts exactly `{}`.
It starts one asynchronous, persisted job and returns immediately. Authenticated
`GET /api/app-updates/status` reads state only. Duplicate active jobs are rejected.
The server invokes only Ubuntu locally and the fixed Mac, Windows and Nas1
SSH aliases (`APP_UPDATE_SSH_HOSTS`); callers cannot supply hosts, commands, app IDs, paths or download URLs.
Nas1 is a second Ubuntu computer reached as `nas1-agent` (see [Nas1](#nas1-the-fourth-computer)).

## Every computer at once, and never stuck

Ubuntu, Mac, Windows and Nas1 run **at the same time**; each computer still
updates its own apps one at a time (one installer per host). The job ends when
the slowest computer finishes, so a busy or unreachable host never holds the
others back. Four computers x ten apps give 40 result rows. Per-computer
progress is live: the helper prints one JSON line per event
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
one bounded, whitelist result for each of the ten fixed apps. It does not copy,
modify or export account credentials/configuration.

| App | Detected installation and supported update |
| --- | --- |
| Antigravity CLI | Active native `agy update`, only to a build in the switching review set (held otherwise, see below) |
| Muse Code | Active user launcher, fixed Meta installer with `MUSE_UPGRADE_MODE=1` and no PATH modification; bash on Ubuntu/Nas1/Mac, PowerShell on Windows |
| OMP | Active standalone `omp update`, installation directory first in PATH |
| Codex CLI | Active native `codex update` on its managed standalone install (linked from `~/.local/bin/codex`), which detection picks whatever the PATH order; Windows active npm installation uses `@openai/codex@latest` with its existing global prefix. Other copies are reported as strays, see [Stray copies](#stray-copies-beside-a-managed-install) |
| Claude Code | Active native `claude update` on its managed install (linked from `~/.local/bin/claude`); an install already at the newer of the official `latest` and `stable` pointers reports `current` before any process check (see [CLI instances and T3 sessions](#cli-instances-and-t3-sessions)); other copies are reported as strays |
| Codex Desktop | Ubuntu signed-repository `chatgpt` package only (a turned-off repository reports `source_disabled`); Mac verified OpenAI DMG; Windows same-publisher/same-identity MSIX |
| Claude Desktop | Mac verified Anthropic ZIP from the publisher's own `RELEASES.json` feed (the old claude.ai redirect answers 403 to non-browser clients); Windows same-publisher/same-identity MSIX; absent Ubuntu installations are skipped |
| ZCode | Official release page and that release's CDN `latest.yml` (sha512 and size); Ubuntu/Nas1 through Jared's `t3-acp-update.service`; Mac verified ZIP swap (never quit); Windows same-signer NSIS installer with close, install and reopen (see [ZCode and T3's ACP adapters](#zcode-and-t3s-acp-adapters)) |
| T3 ACP adapters | `@brokkai/muse-acp` and `zcode-acp-server` from npm's `latest`, reported per package in `parts`; Ubuntu/Nas1 through the same unit run, Mac through Jared's `t3-acp-adapters-update`, Windows npm in `%APPDATA%\npm` |
| T3 Code | One `t3-code` row per host covers its nightly desktop/bundled server and any installed standalone runtime; Ubuntu uses the native updater and a detached delayed server restart; Mac verifies SHA512, codesign and notarization before a bundle swap; Windows verifies SHA512 and the T3 Tools Inc Authenticode publisher before the silent NSIS installer |

Nas1 is a second Ubuntu computer: wherever this table or the text below names
Ubuntu for an installation or an update method, Nas1 uses the same one, because
its helper runs with `--platform ubuntu`. [Nas1](#nas1-the-fourth-computer)
below lists what differs.

Official methods: [Antigravity installer](https://antigravity.google/cli/install.sh),
[Meta installer](https://dev.meta.ai/install.sh),
[OMP updater](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/cli/update-cli.ts),
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code setup](https://code.claude.com/docs/en/setup),
[OpenAI Linux package](https://learn.chatgpt.com/docs/linux/linux-app),
[OpenAI app update management](https://learn.chatgpt.com/docs/manage-app-updates).

### Stray copies beside a managed install

On Ubuntu and Mac, a managed Codex CLI or Claude Code install is the release folder
that its `~/.local/bin` link resolves into (`~/.codex/packages/standalone/releases/<version>`
and `~/.local/share/claude/versions/<version>`). Detection always picks that copy, whatever
the PATH order, so an older copy earlier on PATH (such as a stale npm install in
`/usr/local/bin`) cannot mis-report the version. Where no managed install exists, detection
is unchanged: the first copy on PATH is used and no strays are reported. Nas1's npm-installed
Codex under `~/.local` is such a case.

Every other copy that resolves to a different file is a **stray**. Each one gets a single
bounded, read-only `--version` probe, run side by side (at most 20 seconds each), and at most
four are kept, in PATH order. Strays appear in `--inventory` output and on the job's Codex or
Claude row as, for example:

```json
"strays":[{"location":"usr-local","version":"0.145.0","shadows":true}]
```

- `location` is one fixed word: `usr-local`, `homebrew`, `bun`, `user-npm` (an npm package under the
  home folder) or `other`. The folder itself is never reported.
- `version` is the copy's own version, or `null` when its probe could not read one.
- `shadows` is `true` when the copy comes before the managed install on the helper's PATH, or
  when the managed install is not on that PATH at all.

When a shadowing copy is older than the row's version, the row's message gains one fixed sentence:
"An older Codex CLI copy (0.145.0) in /usr/local/bin comes first on some PATHs; remove it so it
never runs instead." A copy that is newer, or the same version, is listed but never named as
older. Strays are never updated, stopped or removed: the update runs only the managed install.
Windows and the other CLIs have no managed layout, so they report no strays.

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
Mac quit-first behavior; Windows Codex and Claude close and reopen as described below.

On Ubuntu, `t3 update <version> --channel nightly` runs with empty, non-TTY stdin
and **without `--yes`**. Its native updater verifies the runtime and rewrites the
service definition while leaving the running server on its old version. The
updater never reloads systemd, so the helper runs `systemctl --user daemon-reload`
only when `NeedDaemonReload` reports `yes`: after a runtime install, on a later run
that finds T3 current (a unit an earlier run left stale), and right before the
detached restart. A reload never restarts a service. T3 is
the last app on each host, and Ubuntu schedules a separate transient user service
that waits for the update helper to exit, every computer in the dashboard job
(Nas1 included) to finish and the
dashboard job lock to be released, then waits 30 seconds. AAC passes its resolved
`app-updates` state directory explicitly, including custom `--config-dir`,
`CCS_DIR` and legacy `CCS_HOME` configuration. Without that argument the helper
uses `~/.ccs/app-updates`. A dashboard worker treats a missing job file as active.
It holds both the per-user `helper-update.lock` and that dashboard lock through
the final check, restart and health verification before releasing them. It
restarts **only `t3code.service`**;
`ccs-dashboard.service` is never stopped or restarted. A new update job delays
the restart again. The detached worker has an 18-minute wait limit and verifies
HTTP health after restarting, then checks that the running server reports the
installed version. The unit's main process is a launcher that starts the server
from the runtime named in `service-state.json`, so the version is read from the
launcher's direct `serve` child (a process from `~/.t3/runtime/versions/<version>/t3`);
a plain server is read directly. Nothing is restarted to read it.

The immediate result says the server restart is **scheduled**, with zero
synchronously restarted processes and a fixed `systemd` restart target; it
does not claim the new server is already running. Restarting T3 disconnects its
active agent threads and clients. A version-only pending marker remains if
scheduling, restart, health verification or the server's version check fails; a
later explicit click retries. Restart intent is written before runtime
installation, so a post-install version probe timeout cannot lose it. On a later
explicit run, a read-only check of the service's main process and server child
clears intent if the running server already uses the installed version; otherwise
the delayed restart is scheduled again.
Inspect `journalctl --user -u 'aac-t3-restart-*'` for the detached outcome.
No status read schedules or retries anything.

After a verified restart the helper prunes old runtimes under
`~/.t3/runtime/versions`, and an Ubuntu run that finds T3 current prunes them too.
It deletes at most 20 real directories per run, oldest first. It keeps the active
version from `service-state.json`, the version `~/.local/bin/t3` resolves to, the
newest older version as one rollback copy, and any version a process of this user
is executing from or names in its command line. Symlinks are never followed or
removed, and nothing is deleted when `service-state.json` is unreadable or names no
exact version. T3's own tool cache (`~/.t3/tools`, for example cloudflared) is not
part of this: T3 downloads the tool version it pins on demand.

Companions run beside T3 on Ubuntu, best effort, and none changes the
`t3-code` row. The Cursor agent CLI behind T3's Cursor provider
(`~/.local/bin/cursor-agent`, only when it resolves inside
`~/.local/share/cursor-agent/versions`) runs its own `cursor-agent update` before
any restart is scheduled. The Muse and ZCode ACP adapters and the extracted ZCode
app belong to their own rows now: the `zcode` and `t3-acp-adapters` rows run
Jared's `t3-acp-update.service` and wait for it earlier in the same job (see
[ZCode and T3's ACP adapters](#zcode-and-t3s-acp-adapters)), so the T3 row no
longer starts it. When a T3 restart is scheduled, the detached worker still starts
the unit, without waiting, right after the verified restart: the restart has just
stopped the running adapters, so an update the unit deferred for them lands then.
The outcome goes to `t3-components.json` in the helper state directory: UTC time,
the Cursor CLI before, after and status, the ACP updater status (`rows` when the
rows ran it in this job, `pending` until the worker starts it, then `started` or
`failed`; `absent` without the unit), the pruned versions and whether a reload ran.
The detached worker also writes one line to its journal.

Hosts still run in parallel. Installers stay sequential within each host:
T3's replacement closes its bundled server/process family, Windows installers
can hold executable/package files, and Codex already shares a daemon with its
desktop. Overlapping those operations would weaken stop/restart and rollback
guarantees.

Windows Muse is detected at `%LOCALAPPDATA%\Programs\muse\muse.cmd`. Meta's installer
writes only that file; a hand-added extensionless `muse` shim beside it is not a
detection target. The name matches in any letter case,
as `shutil.which` returns `muse.CMD` from PATHEXT. Its `.muse-launcher.ps1 --version` is
probed directly with auto-update disabled, retaining the full `1.4.3-R5018.1`
build version. The old generic `.cmd` rejection reported this official install
as unsupported before probing it. Its fixed
[PowerShell installer](https://dev.meta.ai/install.ps1) now runs with
`MUSE_UPGRADE_MODE=1`, `MUSE_NO_MODIFY_PATH=1` and `MUSE_INSTALL_DIR` bound to the
detected official directory. Unrelated Muse shims remain unsupported. AAC never stops or
restarts Windows Muse: T3's muse-acp adapter hosts `muse serve` from the same folder, and a
running session keeps its version until its host restarts it. The launcher's `.muse-update-lock`
and `.muse-update.<pid>.exe` leftovers are left to the launcher, which reclaims a dead lock; while
a live holder keeps the lock, Meta's installer skips the update, so the row fails with the `busy`
code instead of reporting `current`.

### Live verification after deployment

Only the owner's explicitly authorized Update apps click should exercise real
installers. Before that, read-only `--inventory --platform <host>` should show
ten rows, full T3 nightly versions, Windows Muse's native manager/version, ZCode's
version and the adapters' `parts`; Codex CLI or Claude Code also lists `strays` when
another copy sits beside its managed install.
After an authorized click, confirm 40 result rows and simultaneous host progress.
When T3 updates, verify Mac/Windows bundle versions and port 3773 health, then
wait until the completed job's scheduled Ubuntu restart has finished; inspect
`t3 service status`, the detached unit journal, port 3773 and that
`ccs-dashboard.service` remained active. Do not run this live check while agent
work that must survive a T3 restart is in progress. For Nas1 (inventory there
runs with `--platform ubuntu`) confirm its ten rows, that a T3 update there
scheduled the restart of Nas1's own `t3code.service`, that
`~/.ccs/app-updates/` on Nas1 holds the 13 synced helper files, and that Nas1
has no AI Account Center package, command or service and no `~/.ccs` content
beyond `app-updates/`.

**Antigravity CLI review hold (all four computers).** Account switching works only
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
interactive task and Add-AppxPackage preserves MSIX LocalState.

Codex and Claude desktop apps on the **Mac are never quit, closed, restarted or
killed**: the updater does not even ask them to quit. A running Mac app with a
newer version reports the actionable `action_required`/`quit_first` row ("Quit
Codex Desktop to finish its update") within seconds, before any package
download: Claude on Mac reads its release feed, and Codex on Mac remembers the
version of the last verified DMG by its HEAD fingerprint. Only when that version
is unknown does the full download decide, shown live on the page as
"Downloading" with its elapsed time. The app is checked again right before the
swap, so one opened during the download is left alone.

On **Windows**, Update all closes a running Codex or Claude desktop app
gracefully, installs the update and reopens it by itself (`desktop_reopened`:
"Updated: it closed, installed the update and reopened."). The version decision
is unchanged and cheap: Windows reads only the published MSIX manifest over HTTP
ranges (three requests, under 1 MB); Windows Codex first reads the app's own
Microsoft Store feed (`codex-app-prod/windows-store-update.json`, product
`9PLM9XGG6VKS`), because the direct MSIX stopped at 26.930.7945.0, and installs a
newer Store build with `winget install --source msstore`. A current app keeps
running, and an app that is not running installs without opening anything. The
MSIX download and its verification finish while the app keeps running. Right
before closing, the updater captures the running main instances again (the app
may have been opened or closed meanwhile) and plans how to reopen each the way
it was started:

- an instance with a data-directory argument reopens the way AAC's launcher
  starts a Claude profile: the updated package's `app\Claude.exe` by path, its
  folder as the working directory and the same
  `--user-data-dir="%APPDATA%\Claude-<id>"`. Package identity does not change
  this; AAC's named profile on the Windows PC has it too;
- an instance without arguments and with package identity (the Start menu, or
  Claude's default instance launched through `explorer.exe shell:AppsFolder\...`)
  reopens by its AUMID (`OpenAI.Codex_2p2nqsd0c76g0!App`,
  `Claude_pzs8sxrjxfjjc!Claude`); an unpackaged instance without arguments
  reopens by path.

The packages also run session-0 Windows services (`codex-windows-sandbox-service.exe`,
`cowork-svc.exe`, parent `services.exe`). They are not the user's processes: they
are outside the updater's process list, so it never closes them, and the
Store/MSIX deployment handles them.

It still answers `quit_first` and closes nothing when an instance runs in another
Windows session, belongs to another package, is packaged without arguments but has
no AUMID to reopen it by, or cannot be inspected: its start could not be
reproduced. Such an instance is found before the download when the manifest was
readable. Closing posts WM_CLOSE to the
captured instances' windows, waits a bounded 15 s, then stops only
identity-checked survivors of that app family (counted as `forcedStops`); nothing
is force-stopped without that graceful attempt first. After the install every
planned instance starts detached, and the row is `updated` once as many main
processes of the updated package run (45 s at most); otherwise it is
`restart_failed` with the new version. A failed close or install reopens what was
closed from the previous package, which is still registered, and reports the
failure (`quit_first` when the deployment still asks for the app to close).

Ubuntu desktops keep the previous bounded terminate-and-relaunch flow. CLI forced
stops stay bounded, app-family-only and counted. A successful result verifies
that replacement processes exist. An Ubuntu upgrade turns third-party sources off:
when `apt-cache policy` lists no package source for the Codex or Claude desktop
package except the installed status file, the row is `action_required` /
`source_disabled` and nothing is stopped or installed, never a false `current`.

Updated interactive CLIs open new idle terminal instances. Ubuntu uses a private
`tmux -L ccs-updates-...` server; Mac uses Terminal; Windows uses Windows Terminal.
An explicitly selected valid session UUID may be resumed. Original prompts,
print/exec arguments, stdin and commands are never replayed. Existing cwd and
private auth environment stay in memory; Mac uses an owner-private one-shot Unix socket; Windows uses a local named
pipe restricted to the signed-in user SID to enter the new terminal. Original terminal scrollback and the old
conversation display do not migrate. Safe result `restartTargets` identifies new
terminal/tmux sessions; authentication environment is never serialized.

### CLI instances and T3 sessions

A CLI process that T3 started is **never stopped, relaunched or judged** by Update
all. It belongs to T3 when T3's own server or desktop is one of its ancestors in
the process table, recognised only by exact location: the standalone runtime
`~/.t3/runtime/versions/<version>/t3` (Ubuntu, Nas1, the Mac), the Mac's
`/Applications/T3 Code (Nightly).app` and Windows'
`%LOCALAPPDATA%\Programs\t3code` (the Windows scan also lists `cmd.exe` so a CLI
T3 starts through a shell still shows T3 above it). A process merely named `t3`
elsewhere does not count. Its children (a `claude -p` an agent runs, for example)
belong to T3 too. T3 sessions never fail the readiness check, are not counted
when a relaunch is verified, and T3 is only ever restarted by its own `t3-code`
row.

Where the update can replace files a session is running, it proceeds and the
session keeps its old version until T3 starts it again: the row is `updated` /
`t3_sessions_kept` ("Running T3 sessions were left alone and keep the previous
version until T3 starts them again"), and only the user's own terminal instances
are stopped and reopened. That holds for native installs everywhere: Claude Code
on Ubuntu and the Mac writes `~/.local/share/claude/versions/<version>` and
re-points its link, and on Windows its updater moves the running
`~/.local/bin/claude.exe` aside (`claude.exe.old.<ms>.<pid>`) before placing the
new one, as it must for its own running copy. Windows npm cannot replace a tree a
running process holds open, so Codex CLI on Windows with a T3 session reports
`action_required` / `in_use`, installs nothing and stops nothing, not even the
user's own terminal Codex. Windows Muse keeps its own rule (never stopped). The
Ubuntu Codex bridge still decides on its own when a `codex app-server` runs.

A user's own instance that exits between the process scan and the read of its
working directory and environment is dropped with its children (gone from `/proc`,
or its PID now has another start time; the Mac and Windows re-check its start
identity); it is not a `restart_context` failure. One that still runs but cannot
be read, or whose working directory no longer exists, still fails closed with
`restart_context`, because it could not be reopened where it was.

Claude Code reads the official channel pointers its native updater and
`claude.ai/install.sh` use, `https://downloads.claude.ai/claude-code-releases/latest`
and `.../stable` (plain text, at most 64 bytes, 10 s each, a strict `x.y.z`, the
final URL still under that base). When both answer and the installed native
version is at least the newer of the two, the row is `current` with no process
scan and no `claude update`. Anything else (an unreadable pointer, an unusual
installed version, a pending relaunch marker) takes the usual path.

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
bundles on the Ubuntu, Mac and Windows hosts and the [version-pinned proxy](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/stdio-to-uds/src/lib.rs)
and [startup lock](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/app-server-transport/src/transport/unix_socket.rs)
implementations. Updating Codex can restart its coupled local desktop/server
family even when only one Codex package changed.

## ZCode and T3's ACP adapters

Two rows cover what T3's Muse and ZCode providers run: `zcode` (the ZCode desktop
app, manager `official-download`) and `t3-acp-adapters` (the npm packages
`@brokkai/muse-acp` and `zcode-acp-server`, manager `npm`). Neither ever stops a T3
session: when an update would replace files a running session uses, the row is
`action_required` / `in_use` and nothing changes; Update apps finishes it after the
session ends. Both run before the Codex CLI and T3, so T3's restart stays last.

**ZCode versions and packages.** The installed version is read from files, never
by running ZCode: the root `package.json` of `resources/app.asar` (`@zcode/desktop`;
its header lists the whole node_modules, about 7 MB, so this reader allows 16 MiB
where T3's keeps 2 MiB), `Info.plist` on the Mac (`dev.zcode.app`), and on Ubuntu
t3-acp-update's `.installed-version` stamp when the asar is unreadable. It is
detected at `~/.local/opt/zcode/app` (Ubuntu and Nas1, the extracted AppImage),
`/Applications/ZCode.app` and `%LOCALAPPDATA%\Programs\ZCode\ZCode.exe`. The
newest version is the highest one [zcode.z.ai/en](https://zcode.z.ai/en) lists as
`releases/<version>/<platform>/latest.yml` for this computer (`linux-x64`,
`macos-arm64`, `windows-x64` or their other architecture), compared numerically
like t3-acp-update; ZCode's own `app-update.yml` names only a localhost
placeholder. A ZCode at that version is `current` and nothing runs. Packages come
from that release's `https://cdn-zcode.z.ai/zcode/electron/releases/<version>/<platform>/latest.yml`:
its version must match, and the exact asset's sha512 and size bind the download
(800 MiB and 10 minutes at most, within the host budget).

**In use by T3** means a process of this user that is the zcode-acp-server adapter
(`zcode-acp-server/dist/cli.js`), its child whose argv was rewritten to
`zcode-cli` (no path to ZCode is left on that command line), or ZCode started as a
CLI (`resources/glm/zcode.cjs`). Mac and Windows check again right before they
replace anything.

- **Ubuntu and Nas1.** Jared's `t3-acp-update.service` (a oneshot user unit running
  `~/.local/bin/t3-acp-update`) updates both rows: it verifies and swaps ZCode,
  keeping one backup, and installs both adapters with npm into `~/.local`, under
  its own lock, deferrals and log (`~/.local/state/t3-acp-update/update.log`). The
  helper starts it with a blocking `systemctl --user start` (10 minutes at most,
  within the host budget; at the limit only the wait stops and the unit finishes
  on its own) and runs it **once per job**: the first row that needs it runs it and
  the other reads the result. The zcode row runs it when ZCode is behind and no
  session uses ZCode; the adapters row runs it when an adapter is behind, unless
  the zcode row already did or an adapter process (`muse-acp`,
  `zcode-acp-server/dist/cli.js`, anything from either package folder) runs. Each
  row then re-reads its own versions: newer is `updated`; unchanged while
  something still runs from the app folder (ZCode) or an adapter runs is `in_use`,
  because the unit defers then; otherwise the unit's failure code or
  `update_failed`. Without `~/.config/systemd/user/t3-acp-update.service` both
  rows are `failed` / `unsupported`.
- **Mac.** A running ZCode (any process of the bundle that is not a CLI session)
  reports `quit_first` before any download: like Codex and Claude, ZCode on the
  Mac is never quit. A T3 session alone is `in_use`. Otherwise the ZIP is checked
  like T3's: archive paths and symlinks before `ditto -x -k`, strict deep
  `codesign`, the inline requirement `identifier "dev.zcode.app" and anchor apple
  generic and certificate leaf[subject.OU] = "8A5X4JJ39T"`, `spctl` and the bundle
  version. It is copied beside the app, checked again, checked for a ZCode opened
  meanwhile (`quit_first`) and swapped in with an atomic rename; a failed final
  check restores the old bundle. ZCode is not opened afterwards: it was not
  running. The adapters live in `/opt/homebrew/lib/node_modules`; Jared's
  `~/.local/bin/t3-acp-adapters-update` (his daily LaunchAgent runs it too)
  installs both with Homebrew npm and is run and waited for, 5 minutes at most. It
  does not defer, since macOS replaces files under running adapters safely, so an
  unchanged result is `update_failed`; without the script the row is `unsupported`.
- **Windows.** The NSIS installer (`ZCode-<version>-win-x64.exe`, or arm64)
  downloads and is verified while ZCode keeps running: a valid Authenticode
  signature whose signer subject equals that of the installed `ZCode.exe`, which
  must be valid too and carry ZCode's registration number
  `SERIALNUMBER=91110108MA01KP2T5U` (one PowerShell call that prints nothing; a
  mismatch is `signature_failed` and nothing closes). Then it follows Windows Codex
  and Claude: the running main instances are captured (one in another session is
  `quit_first`, also before any download), closed with WM_CLOSE, a bounded 15 s
  wait and an identity-checked stop of only ZCode's survivors (`forcedStops`), a
  private copy of the install folder is kept, `/S` runs with T3's own wait and
  tree kill, the asar version must equal the release, and every closed instance
  reopens by path with its data-directory argument (`desktop_reopened`, restart
  target `desktop`). A ZCode that was not running installs without opening
  anything. A failed install restores the copy and reopens what closed (`failed`
  with its code); a failed reopen after a good install is `restart_failed` with the
  new version. `ZCode.exe` and the adapter's native `muse-acp.exe` are in the helper's Windows
process list for this. The
  adapters live in the global npm prefix `%APPDATA%\npm`. Windows cannot replace
  files a running adapter holds, so a node process naming either package folder,
  or anything running from one, is `in_use`; otherwise npm runs like the Codex
  CLI's, without a shell: `node npm-cli.js install --global --prefix %APPDATA%\npm
  @brokkai/muse-acp@latest zcode-acp-server@latest` (only installed packages are
  named; 5 minutes at most).

**Adapter rows.** The top-level versions stay null; `parts` lists `muse-acp` and
`zcode-acp-server` once each with their previous and current versions (null for a
package that is not installed). Neither package present is `not_installed`. npm's
`latest` documents (`https://registry.npmjs.org/@brokkai%2fmuse-acp/latest`,
`https://registry.npmjs.org/zcode-acp-server/latest`, 1 MiB at most) decide:
every installed package at least that version is `current` and nothing runs. An
`updated` row has at least one package whose version changed.

**Unreadable sources.** When the ZCode page or the npm registry does not answer
clearly, nothing runs and the row is `failed` / `update_failed`, as T3 and Claude
Desktop report an unreadable release feed: nothing can be called current without
it.

## Deployment and setup

The server syncs every `app_update_*.py`, `app_updates.py`, and
`app_update_codex.cjs` from its own build, plus the generated
`app_update_codex_runtime.cjs` from `dist/app-updates`, into `~/.ccs/app-updates` on
Mac, Windows and Nas1 before each run, checksum-gated so up-to-date hosts only answer one
hash query; a sync failure leaves the deployed helpers untouched. Python must be
available. On Windows register `install-windows-task.ps1` once as the signed-in
user: its fixed `CCS App Updates` InteractiveToken/Limited task runs the private
helper. Registration does **not** run an update. The helper queues that task
from SSH session zero, so the user's interactive session must be signed in.
Existing CCS Bar tasks are untouched.

The task runs `pythonw.exe` from the same Python install (registration stops with
an error if it is missing) and is registered hidden. Helper children, such as
PowerShell, winget, CLI probes and the Muse installer, start with
`CREATE_NO_WINDOW`, so none opens a console window of its own. Relaunched apps and
the Windows Terminal tab that reopens a CLI still appear normally. Re-running
`install-windows-task.ps1` applies the hidden setting to a task that is already
registered.

The Mac and Nas1 syncs extract with `/bin/mkdir`, `/bin/chmod` and `/usr/bin/tar`
(macOS has no `/usr/bin/chmod`; that path silently broke every Mac sync until
2026-10-06, so Mac kept running its Oct-2 helpers). Ubuntu's `/bin` is the same
folder as `/usr/bin`, so the three paths work on Nas1 too. The hash query differs
by system: the Mac uses `/usr/bin/shasum -a 256`, Nas1 uses `/usr/bin/sha256sum`.

Results are `updated`, `current`, `not_installed`, `failed`, `restart_failed`,
`skipped`, `unknown`, or `action_required`, with bounded versions, fixed message
codes, restart counts/forced-stop counts and safe terminal target metadata. Raw
package command output, process argv, environments, installer response bodies and
credentials are never returned. Host/helper execution locks prevent overlapping
work. Package operation timeout or a relaunch refusal is reported honestly and
does not prevent other apps/hosts from producing their own results. Status reads
never retry an interrupted job.

`action_required` is never a failure: the job completes and the row tells the
user exactly what to do. ZCode and the ACP adapters report `in_use` while a T3
session uses them: nothing was changed, and Update apps finishes them later; so does
Codex CLI on Windows (npm) while a T3 session runs it. Codex and Claude desktops on the Mac that are running report
`quit_first` (they are never asked to quit): nothing is swapped while anything
runs, and the next click after the user quits updates cleanly. On Windows they
report `quit_first` only when an instance's start cannot be reproduced (see
above) or an MSIX deployment is still rejected for apps that need closing. Codex/Claude desktop downloads refused by a bot challenge (HTTP 403 or a
challenge page) report `check_in_app` after bounded retries. Desktop downloads
allow 2 GiB and 10-minute timeouts; Codex/Claude Mac desktops are never
force-stopped, Windows ones only after the bounded WM_CLOSE wait (Ubuntu desktops
keep the previous terminate-and-relaunch flow).
Claude's Mac update reads the publisher's own `RELEASES.json` feed on
downloads.claude.ai and checks its version before any package download, so a
current app fetches nothing; its ZIP is extracted with `ditto` and passes the
same codesign/TeamID verification as the DMG flow.
Windows npm updates first ask the registry whether anything is newer, then run
`node npm-cli.js` directly (never through `cmd /s /c`, which mangles spaced
paths) after stopping mapped instances first, since Windows cannot replace a
running npm tree.

## Nas1, the fourth computer

Nas1 is a second Ubuntu computer reached over the fixed alias `nas1-agent`
(label "Nas1"). Its rows are filed under their own computer, so one run returns
4 computers x 10 apps = 40 result rows, with Nas1's progress beside the other
three. It follows the Ubuntu rows of this guide except where noted here.

- **Command.** `ssh nas1-agent` runs
  `AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform ubuntu`
  plus `--agy-reviewed '<versions>'` when the reviewed list is readable. The
  helper checks `--platform` against its own operating system, so Nas1 runs as
  `ubuntu` and the dashboard files the rows under Nas1. Nas1 gets neither
  `--state-dir` nor `--dashboard-job`; those belong to the dashboard's own
  Ubuntu run.
- **Helper sync.** The `sha256sum` hash query and POSIX `tar` extract above fill
  `~/.ccs/app-updates` (mode 0700) with 13 files: the 12 source helpers and the
  generated Codex runtime below.
- **Codex runtime.** Nas1 has no AI Account Center, so its Codex bridge loads
  the bundled `app_update_codex_runtime.cjs` that the sync ships beside the
  helpers (described with the Ubuntu Codex updates above). Codex Desktop and a
  Codex CLI daemon therefore update on Nas1 as on Ubuntu. The runtime only
  signals and launches Codex processes and manages the
  `~/.codex/app-server-control/` socket; it never writes `auth.json` or any
  other login.
- **T3.** A T3 update on Nas1 schedules its own deferred restart: a detached
  user service waits for the helper to exit and 30 seconds, reloads Nas1's unit
  only when it is stale, restarts only Nas1's `t3code.service`, checks its health
  and that the server reports the installed version. Nas1 then prunes its old
  runtimes and updates its Cursor CLI; its own `zcode` and `t3-acp-adapters` rows
  run its `t3-acp-update` unit, and the worker starts it again after a T3 restart,
  as the Ubuntu rows above describe, where those are installed. Run without
  `--dashboard-job`, it needs no dashboard job or lock.
- **Antigravity.** Nas1 has no AI Account Center managed Antigravity runtime
  (that update branch needs `~/.ccs/antigravity-switching/runtime-installation.json`,
  which is never created there), so the CLI updates through the generic
  `agy update` behind the same reviewed-version hold.
- **Only fixed commands.** For updates the dashboard runs three fixed commands
  on Nas1: the hash query, the extract and this helper. None reads or writes
  `auth.json`, `.credentials.json` or a saved profile, and restarts reuse each
  process's own environment. Account switching, activation, key storage,
  sign-in and Claude desktop flows never target Nas1.
- **Rollback.** An older package ignores Nas1's rows and progress entry in a
  saved job and does not restore a job with more than 24 result rows, so the
  last result may not show after a rollback. It never replays an update.

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
