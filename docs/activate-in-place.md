# Activate a Codex account in the shared Ubuntu home

Use `ai-account-center codex-auth activate <name>` or the dashboard's Codex
**Activate** control. Both reach the retained activation transaction.
`ccs codex-auth activate` remains compatible; retired `ccsx` does not launch
or activate accounts.

Activation retains one shared `~/.codex` and changes its login without substituting
a profile directory for the native home. Existing sessions, configuration,
automations, attachments and project links remain there. The transaction locks
activation/daemon startup, checks active work, retains the outgoing login in its
matching profile, installs the target login atomically and validates the result.
Failures use the existing rollback path.

When programs would be affected, dashboard activation opens a review containing
the server's warning and blocking programs. Confirm submits the server-issued,
target-bound approval; cancel sends no mutation. Expired/stale approvals require
a fresh review. Automatic switching never treats that review as consent.

The active identity is explicitly **Active on Ubuntu**, distinct from saved
profile metadata. Account mutations require authenticated same-origin JSON
requests; credentials/tokens never enter dashboard responses. See
[Codex account contract](codex-auth.md) and its source links.

## Automatic switching

Settings displays the server-confirmed enabled state, used-percent threshold
and refresh interval. The server monitor continues while the dashboard server
runs, even if the browser is closed. It only considers fresh provider quota and
validates identity again before activation. Unknown, stale or locally estimated
usage cannot select a new account.

Automatic switching waits while Codex is doing work. Disabling cancels queued
decisions; a transaction already started must finish or roll back. It does not
continue an interrupted request under another account. Claude account launchers
remain manually selected.

The [auto-switch owner](../src/web-server/services/codex-auto-switch-service.ts)
and [activation transaction](../src/codex-auth/activate-codex-profile.ts)
define settings and transaction behavior. Provider IDs, private homes and
session aliases stay compatible with existing installations.
