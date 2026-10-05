# Fixed installed-app updates

The dashboard's authenticated `POST /api/app-updates/start` accepts exactly `{}`.
It starts one asynchronous, persisted job and returns immediately. Authenticated
`GET /api/app-updates/status` reads state only. Duplicate active jobs are rejected.
The server invokes only Ubuntu locally and the fixed `jared-mac`/`jared-windows`
SSH hosts; callers cannot supply hosts, commands, app IDs, paths or download URLs.

The Python helper defaults to **read-only inventory**. Only `--apply` updates or
restarts apps. It detects the active installation, skips absent apps, and returns
one bounded, whitelist result for each of the seven fixed apps. It does not copy,
modify or export account credentials/configuration.

| App | Detected installation and supported update |
| --- | --- |
| Antigravity CLI | Active native `agy update` |
| Muse Code | Active user launcher, fixed Meta installer run with bash (`set -o pipefail`, `[[ ]]`) with `MUSE_UPGRADE_MODE=1` and no PATH modification |
| OMP | Active standalone `omp update`, installation directory first in PATH |
| Codex CLI | Active native `codex update`; Windows active npm installation uses `@openai/codex@latest` with its existing global prefix |
| Claude Code | Active native `claude update` |
| Codex Desktop | Ubuntu signed-repository `chatgpt` package only; Mac verified OpenAI DMG; Windows same-publisher/same-identity MSIX |
| Claude Desktop | Mac verified Anthropic ZIP from the publisher's own `RELEASES.json` feed (the old claude.ai redirect answers 403 to non-browser clients); Windows same-publisher/same-identity MSIX; absent Ubuntu installations are skipped |

Official methods: [Antigravity installer](https://antigravity.google/cli/install.sh),
[Meta installer](https://dev.meta.ai/install.sh),
[OMP updater](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/cli/update-cli.ts),
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[Claude Code setup](https://code.claude.com/docs/en/setup),
[OpenAI Linux package](https://learn.chatgpt.com/docs/linux/linux-app),
[OpenAI app update management](https://learn.chatgpt.com/docs/manage-app-updates).

Inventory disables background CLI updates (`MUSE_NO_AUTO_UPDATE=1`,
`AGY_CLI_DISABLE_AUTO_UPDATE=true`, `DISABLE_AUTOUPDATER=1`). This matters because
Muse otherwise checks/updates even during a `--version` invocation.

Restart scope is the **same user's exact app executable/package family**,
validated against PID creation identity. Generic Node/Python/terminal processes
are never selected. Desktop updates preserve existing absolute profile directory
arguments. Mac uses its native application quit API and verified atomic bundle
replacement; Windows uses an interactive task to close/reopen windows and preserve
MSIX LocalState. Desktop apps are **never force-stopped** on Mac and Windows:
a graceful quit is requested first, and nothing is swapped or deployed while
anything runs — a refusal, or instances that cannot be captured for a safe
relaunch, reports the actionable `action_required`/`quit_first` row instead of
failing, because swapping files under a running app would mix old and new
versions (Ubuntu desktops keep the previous bounded terminate-and-relaunch
flow). CLI forced stops stay bounded, app-family-only and counted.
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
for the shared daemon and desktop app server. Busy Codex work is queued until the
bounded deadline. SSH proxies use verified native client reconnect behavior:
Mac/Windows desktop transports recreate `exec codex app-server proxy` after exit.
The updater stops only captured proxies with SSH ancestry, then verifies old PIDs
are gone and replacement proxies execute the updated active binary. Unknown
unsupervised proxies yield an explicit restart failure. A private **version-only**
pending-restart marker permits a subsequent explicit click to retry after timeout.
No account authentication is changed.

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

Results are `updated`, `current`, `not_installed`, `failed`, `restart_failed`,
`skipped`, `unknown`, or `action_required`, with bounded versions, fixed message
codes, restart counts/forced-stop counts and safe terminal target metadata. Raw
package command output, process argv, environments, installer response bodies and
credentials are never returned. Host/helper execution locks prevent overlapping
work. Package operation timeout or a relaunch refusal is reported honestly and
does not prevent other apps/hosts from producing their own results. Status reads
never retry an interrupted job.

`action_required` is never a failure: the job completes and the row tells the
user exactly what to do. Mac and Windows desktops whose running instances will
not quit (or cannot be mapped) report `quit_first`: nothing is swapped or
deployed while anything runs, and the next click after the user quits updates
cleanly. MSIX deployments rejected for apps that need closing also report
`quit_first`. Desktop downloads refused by a bot challenge (HTTP 403 or a
challenge page) report `check_in_app` after bounded retries. Desktop downloads
allow 2 GiB and 10-minute timeouts; Mac and Windows desktops are never
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

Bun tests in `tests/unit/app-updates` and the two app-update web-server test files
cover safe normalization, fixed invocations, authentication/origin/body checks,
async job coalescing, cross-process persistence/locks, queued Codex idle waits,
proxy reconnect coordination and retry markers. Real package replacement and real
app restarts are intentionally exercised only after the user clicks the button.
