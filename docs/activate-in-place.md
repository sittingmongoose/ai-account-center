# Activate a Codex account in the shared Linux home

Run `ccs codex-auth activate <name>`, or open the dashboard's Codex page and
click **Activate on VM** beside a saved account. Both use the same transaction.
`ccsx auth activate <name>` also works.

Activation keeps one shared `~/.codex` and changes only its `auth.json`.
Sessions, configuration, automations, attachments, and project links stay in
that home. Profile directories hold saved logins; they are not substituted for
the shared home during activation. Unset a per-profile `CODEX_HOME` before
activating.

The transaction locks activation and daemon startup, checks for active work,
stops the shared app-server and the VM's Codex desktop app, waits for credential
writers to exit, saves the live login to its matching profile by decoded email,
atomically installs the requested login, and restarts both on their original
display and environment. It then checks the installed account. A failed switch
attempts to restore the original login. Active work produces a busy error
without changing credentials or interrupting a thread; activate after it ends.

The dashboard shows **Active on VM** separately from the default profile used
by CCS's existing per-process Codex launch commands. Its activate endpoint
requires JSON and a same-origin request; remote access requires the existing
dashboard authentication. No token values are returned.

Import existing credentials before activation, or create a profile and sign in
once with `ccs codex-auth login <name>`. The activation feature currently
supports the Linux VM layout with `/usr/lib/chatgpt/ChatGPT`; Claude desktop
account profiles are configured separately.

## Automatic switching for the shared VM login

The **Codex accounts** dashboard page has an **Automatic switching** control.
It is disabled by default. Enabling it writes private settings to
`~/.ccs/codex-auto-switch.json` and starts a monitor in the dashboard server.
The server must remain running; the browser can be closed.

The monitor checks every minute. When a reported 5-hour or weekly limit has
5% or less remaining, it selects another authenticated saved Codex account
with more than 5% remaining and the greatest remaining percentage across its
reported limits. Only fresh provider responses can make this decision;
missing usage, old samples and local session-log estimates cannot trigger a
switch. The account shown as active in the shared VM home is checked again
before activation.

Automatic switching uses the same activation transaction as the button and
CLI. If Codex is doing work, it waits and tries again later. It does not
continue an interrupted request under another account or restart an active
thread. Disabling cancels a queued decision; an activation that has already
started must finish its existing transaction or rollback.

`GET /api/codex/profiles/auto-switch` returns the setting and current status.
`PUT` at the same path accepts exactly `{ "enabled": true }` or
`{ "enabled": false }`, with dashboard authentication and a same-origin JSON
request. The threshold and interval are fixed rather than supplied by callers.

This monitor covers native Codex profiles and the shared VM daemon/desktop.
CCS's existing CLIProxy rotation is a separate feature with its own account
pool. Native credentials are not merged into that pool. Claude desktop
profiles remain manually selected through their launchers.
