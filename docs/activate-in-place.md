# Account activation (retired)

AI Account Center no longer switches accounts. Codex activation and automatic
switching, and Antigravity activation and automatic switching, were removed on
2026-11-14: the agents pick their own accounts by burn rate and availability.

- **What is gone:** the dashboard and tray Activate controls, the switch review
  dialog, the auto-switch settings, the `codexAutoSwitch` dashboard field, the
  managed Antigravity switching runtime (`antigravity status`,
  `antigravity recover`, `antigravity runtime refresh`),
  `ai-account-center codex-auth activate` and
  `ai-account-center antigravity activate` (both print a retired-command
  message), and the routes `POST /api/codex/profiles/:name/activate`,
  `GET`/`PUT /api/codex/profiles/auto-switch`,
  `POST /api/antigravity/profiles/:id/activate`, `.../confirm`,
  `GET`/`PUT /api/antigravity/auto-switch` and `POST /api/antigravity/recover`.
- **What stays:** usage, limits and resets for every saved login, saved-login
  renewal, sign-in health, add/remove and Sign in again (including renewing the
  live login in place when you re-sign the live account).
- **Files left behind:** `~/.ccs/codex-auto-switch.json` and
  `~/.ccs/antigravity-switching/antigravity-auto-switch.json` are no longer read
  or written. They are never deleted automatically; a legacy stuck Antigravity
  switch transaction in the profile index is cleared on the next registry write.

See [Codex account contract](codex-auth.md).
