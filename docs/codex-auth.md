# AI Account Center Codex Account Contract

The retained CLI surface is `ai-account-center codex-auth` with create, login,
import-default, show, remove and activate operations. See the current
[help owner](../src/codex-auth/codex-auth-help.ts) rather than copying every option.
`ccs codex-auth` remains compatible; `ccsx` and the other retired runtime bins
only give migration guidance. The former shell-export/use/switch/runtime-launch
flows are retired.

## Existing credentials and storage

Import preserves the native `~/.codex/auth.json` source. Profile credentials
remain in private `~/.ccs/codex-instances/<name>/` with the existing registry,
shared-resource links/fallbacks and private atomic-write/backup behavior.
Preserve profile-local content, native sessions and shared config/resources.

The [profile resource helper](../src/codex-auth/codex-profile-resources.ts),
[configuration link helper](../src/codex-auth/codex-config-symlink.ts) and
[plugin-cache helper](../src/codex-auth/codex-profile-plugin-cache.ts) own those
compatibility boundaries. Existing profile names, session aliases and home
overrides are not renamed with the product.

Removing a saved login shares the activation lock and rechecks the native login
before changing profile files. Every saved alias matching the current native
account stays protected, including with `--yes` or `--force`; activate another
saved login first. Unreadable native authentication or an unverifiable saved
identity also keeps the profile while a native login exists. `--force` only
overrides the saved-default selection. Confirmation, backup and rollback remain
part of removal.

## Idle saved-login renewal

The server renews idle saved logins itself. About four days before a saved
login's access token expires (the token lasts 10 days), AAC refreshes it with
OpenAI. The check runs three to five minutes after the server starts, then every
six hours, plus or minus 30 minutes. A busy account switch, sign-in or removal
makes the check retry shortly.

AAC leaves these logins alone: the login Codex is using (Codex renews it itself),
a login that a running Codex app uses directly, and a login that another Codex
home on this machine shares (the same refresh token, session id or sign-in time).
Renewing one copy would sign the other out, so Sign in again on one of the copies
to separate them. Renewal shares the activation lock and never waits for it.
Renewed tokens are written atomically, only back into the same profile folder.

The dashboard and `GET /api/codex/profiles/quotas` report a login that OpenAI
rejected as needing Sign in again, and Sign in again restores it. Automatic
switching never chooses such a login. Set `CCS_CODEX_RENEWAL=0` to turn renewal
off; a rejected login still shows Sign in again.

## Activation and automatic switching

[Activation](activate-in-place.md) changes the selected native login in the
shared Ubuntu home through the existing guarded transaction. Busy work requires
explicit review; target-bound approvals, stale-token rejection and rollback stay
intact. A dashboard refresh cannot approve a switch.

The server's [auto-switch service](../src/web-server/services/codex-auto-switch-service.ts)
uses configured thresholds and fresh provider usage, waits until idle and rechecks
the active identity. Browser Settings displays server-confirmed values; missing
usage or local activity estimates cannot trigger a switch. Claude stays manual.

The retained account tests, dashboard service and native clients own validation.
Use offline fixtures and isolated private paths; signing in or live provider
requests are separately authorized actions.
