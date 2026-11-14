# Account activation (retired)

AI Account Center no longer switches accounts. Codex activation and automatic
switching, and Antigravity activation and automatic switching, were removed on
2026-11-14: the agents pick their own accounts by burn rate and availability.

- **What is gone:** the dashboard and tray Activate controls, the switch review
  dialog, the auto-switch settings, `ai-account-center codex-auth activate` and
  `ai-account-center antigravity activate` (both now print a retired-command
  message), and the routes `POST /api/codex/profiles/:name/activate`,
  `GET`/`PUT /api/codex/profiles/auto-switch`,
  `POST /api/antigravity/profiles/:id/activate`, `.../confirm`,
  `GET`/`PUT /api/antigravity/auto-switch` and `POST /api/antigravity/recover`.
- **What stays:** usage, limits and resets for every saved login, the live login
  on Ubuntu marked **Active on Ubuntu**, saved-login renewal, sign-in health,
  add/remove and Sign in again. `ai-account-center antigravity recover` stays
  for one release to finish a switch an older version or a managed update left
  unfinished.
- **Files left behind:** `~/.ccs/codex-auto-switch.json` and
  `~/.ccs/antigravity-switching/antigravity-auto-switch.json` are no longer read
  or written. They are never deleted automatically; an older install can still
  use them.

The dashboard response still carries a fixed `codexAutoSwitch` (always off) for
the trays installed before the removal. See [Codex account contract](codex-auth.md).
