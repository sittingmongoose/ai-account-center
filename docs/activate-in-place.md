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
