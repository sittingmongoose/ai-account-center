# AI Account Center Product Overview

AI Account Center manages existing accounts and observed subscription usage
through a Slint dashboard and native desktop bars. This is the renamed continuing
CCS fork, retaining original authorship, license, private state and history.

## Retained product

- Authenticated account dashboard (Home, Analytics, Accounts & Settings) for
  Claude, Codex and the configured additional provider collectors.
- Existing profile/account management, per-provider and per-account "Show on
  dashboard" and "Show in tray" switches, server-driven add, sign-in again,
  replace key, remove and restore, and Claude Mac/Windows launcher controls.
- Guarded Codex activation on Ubuntu, explicit busy review, rollback and idle-only
  automatic switching.
- Observed quota Analytics with range presets, a Claude Code / Codex filter,
  Included usage sources and complete reset/expiration details.
- Authentic CLI activity (Claude Code, Codex, OMP, Muse, zcode) from Ubuntu, Mac,
  Windows and Nas1 with clearly labeled logged, partial and not-logged costs;
  estimates are not charges.
- Dashboard sign-in with password change, paired tray devices and the trusted
  local network setting.
- Server-confirmed refresh settings and explicitly started allowlisted app-update
  jobs with Cancel, covering four computers (Ubuntu, Mac, Windows and Nas1) and
  32 result rows.
- Nas1, a second Ubuntu computer reached over a fixed SSH alias, as the fourth
  computer for app updates and Analytics. It runs no dashboard and holds no
  account state; its quota sources stay where they are unless one is saved for it.
- Native Mac and Windows clients of the same authenticated account APIs, paired
  with their own device keys.
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
Nas1 never has account switching, activation, key storage, sign-in or Claude
desktop flows: the dashboard computer runs only fixed update, hash, extract and
read-only analytics commands there (and usage collectors for a source saved for
it). What an older package ignores after a rollback is listed in the
[README](../README.md#update-and-recovery).

Implementation detail belongs in the [source map](codebase-summary.md) and
[architecture](system-architecture/index.md). Historical upstream releases remain
in [CHANGELOG.md](../CHANGELOG.md); they are not a current feature inventory.
