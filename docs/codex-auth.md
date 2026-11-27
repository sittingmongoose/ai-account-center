# AI Account Center Codex Account Contract

The retained CLI surface is `ai-account-center codex-auth` with create, login,
import-default, show and remove operations. See the current
help owner rather than copying every option.
`ccs codex-auth` remains compatible; `ccsx` and the other retired runtime bins
only give migration guidance. The former shell-export/use/switch/runtime-launch
flows and `activate` are retired: AI Account Center reads each saved login's usage
and no longer switches the shared login ([retirement note](activate-in-place.md)).

## Existing credentials and storage

Import preserves the native `~/.codex/auth.json` source. Profile credentials
remain in private `~/.ccs/codex-instances/<name>/` with the existing registry,
shared-resource links/fallbacks and private atomic-write/backup behavior.
Preserve profile-local content, native sessions and shared config/resources.

The profile resource helper,
configuration link helper and
plugin-cache helper own those
compatibility boundaries. Existing profile names, session aliases and home
overrides are not renamed with the product.

Removing a saved login shares the Codex login lock and rechecks the native login
before changing profile files. Every saved alias matching the current native
account stays protected, including with `--yes` or `--force`. Unreadable native
authentication or an unverifiable saved
identity also keeps the profile while a native login exists. `--force` only
overrides the saved-default selection. Confirmation, backup and rollback remain
part of removal.

## Idle saved-login renewal

The server renews idle saved logins itself. About four days before a saved
login's access token expires (the token lasts 10 days), AAC refreshes it with
OpenAI. The check runs three to five minutes after the server starts, then every
six hours, plus or minus 30 minutes. A busy sign-in or removal makes the check
retry shortly.

AAC leaves these logins alone: the login Codex is using (Codex renews it itself),
a login that a running Codex app uses directly, and a login that another Codex
home on this machine shares (the same refresh token, session id or sign-in time).
Renewing one copy would sign the other out, so Sign in again on one of the copies
to separate them. Renewal shares the Codex login lock and never waits for it.
Renewed tokens are written atomically, only back into the same profile folder.

The dashboard and `GET /api/codex/profiles/quotas` report a login that OpenAI
rejected as needing Sign in again, and Sign in again restores it. Set `CCS_CODEX_RENEWAL=0` to turn renewal
off; a rejected login still shows Sign in again.

Usage requests keep their timeout through response-body reads. A stalled body
times out and retries once, so later refreshes can retrieve a new reading rather
than staying attached to a stuck request. Temporary failures retain the last
successful reading as cached and do not change saved credentials. Banked resets
are totaled across all expiration groups for that account; Details retains each
group's own expiration. Reading usage never redeems a reset.

## The live login

The account in the shared `~/.codex/auth.json` is shown as **Active on Ubuntu**.
It is a fact the dashboard reads; AI Account Center never moves it to another
account. Sign in again on the live profile renews both the saved profile and
`~/.codex/auth.json` with the same account, both or neither; a sign-in to a
different account is refused (`identity_mismatch`), and Remove still refuses the
live profile (`account_active`). Restart a Codex app that is open on Ubuntu so it
reads the renewed login.

## Central Logins step 3: T3 logins only

Step 3 retires AAC's saved Codex copies without deleting them. On the dashboard
computer, `aac-agent --logins-quarantine --codex --claude-renewal-host <name>`
records each saved profile's account identity (a digest of the workspace and
person binding, never a token) in `logins-step3.json`, then moves every
`codex-instances/<name>/auth.json` into `disabled-rollback/` (0700, neutral file
names, so no reader or refresher finds it). From then on each Codex row is read
only through a login a tool owns on that computer, a T3 instance's own home or
`~/.codex`, whose identity is the recorded one. A login of another account is
never used, and neither is a saved copy or the local session files. A profile
with no such login shows **unavailable** with the reason; nothing renews the T3
logins (Codex does that itself).

The same switch cuts Claude renewal to that computer's sign-ins, one per
account. The name must be a computer in the Claude renewal settings, or the
command refuses (a wrong name would pause renewal everywhere). The dry run and
the switch-on list the profiles that stay renewed there and every second
profile of an account, which is not renewed; switch-on is refused until each
such profile runs on a static token or is listed with `--claude` in the same
run (only on the renewal computer itself). On every other computer,
`aac-agent --logins-quarantine --claude <profiles>` moves the Claude sign-in
files of profiles that already run on a static `claude setup-token` token. A
profile is refused, and named, unless that computer holds a delivered Claude
token for its account that reads cleanly, and, where the static logins
registry lives, the registry shows it delivered there with one clean read. The
account is `claude:<profile>` or the Claude account signed in with the same
email; name it as `<profile>=<account>` when neither fits (such as
`default=jmtashjian`). `aac-agent --logins-status` shows what is
held, and `aac-agent --logins-restore` (the exact command is printed by the
quarantine) puts every file back, never over a newer one, and turns the switch
off. A purge of `disabled-rollback/` is a separate, later action.

The retained account tests, dashboard service and native clients own validation.
Use offline fixtures and isolated private paths; signing in or live provider
requests are separately authorized actions.
