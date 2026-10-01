# AI Account Center Product Overview

AI Account Center manages existing accounts and observed subscription usage
through a Slint dashboard and native desktop bars. This is the renamed continuing
CCS fork, retaining original authorship, license, private state and history.

## Retained product

- Authenticated account dashboard for Claude, Codex and the configured additional
  provider collectors.
- Existing profile/account management and Claude Mac/Windows launcher controls.
- Guarded Codex activation on Ubuntu, explicit busy review, rollback and idle-only
  automatic switching.
- Observed quota Analytics with source/platform/account filters and complete
  reset/expiration details.
- Authentic local Ubuntu Claude Code/Codex activity with clearly labeled estimated
  API-equivalent costs.
- Server-confirmed refresh settings and explicitly started allowlisted app-update
  jobs.
- Native Mac and Windows clients of the same authenticated account APIs.
- Local source packaging and a dashboard-only Docker build.

The primary command is `ai-account-center dashboard`; `ccs config` is compatible.
Retired runtime bins provide guidance. The product does not launch arbitrary
models, route API profiles or deploy a CLIProxy service.

## Data and safety

Unknown usage stays unknown; real zero, signed balances and overages stay real.
Missing history is not interpolated into fabricated activity. Account details
retain all applicable counters, units, resets, expiration and provenance.

Credentials stay with the host/session that owns them. Authentication,
origin/session guards, identity-bound caches, target-bound switch approvals,
migrations and rollback are part of the product behavior. A visible rename does
not rename `~/.ccs/`, session aliases, provider/profile IDs or native host names.

Implementation detail belongs in the [source map](codebase-summary.md) and
[architecture](system-architecture/index.md). Historical upstream releases remain
in [CHANGELOG.md](../CHANGELOG.md); they are not a current feature inventory.
