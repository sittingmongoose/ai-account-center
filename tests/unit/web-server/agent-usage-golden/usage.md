# AI Account Center usage

Snapshot 2026-10-15 12:00 UTC. The AI Account Center dashboard rewrites it about every 1m; if it is more than 5m old, the dashboard has stopped updating it. Percentages and amounts are the providers' own; a missing number was not reported, not zero. Burn rates come from measured readings since the window last reset. Machine-readable: usage.json, 7-day readings: history.json, or run aac-usage in this folder.

## Codex

- **Gmail** (person@example.com, profile gmail), Pro, Ubuntu VM, active: current, sampled just now
  - 5-hour: 40% used, 60% left, resets 2026-10-15 13:30 UTC (in 1h 30m), burning 30%/h over the last 1 h, full in 2h 0m (after the reset)
  - Weekly spend: 14 of 50 USD left, resets 2026-10-19 00:00 UTC (in 3d 12h), expires 2026-10-20 00:00 UTC

## Claude

- **Party** (party@example.com, profile party), Ubuntu VM: cached earlier reading, sampled 30m ago
  - 5-hour: 55% used, 45% left, resets 2026-10-15 16:00 UTC (in 4h 0m), earlier reading from 30m ago

## Z.ai

- **GLM coding plan** (dev@example.com), Coding Plan, Mac: needs sign-in, sampled just now


## Sign-in health

Claude Code sign-ins renew by themselves (runner on mac, last run 10m ago).

- **NEEDS YOU · party@example.com**: Run the sign-in fix on the Windows bar (since 2026-10-15 08:00 UTC)

- **claude · windows/desk**: already logged out. sign-in ended 2026-10-14 09:00 UTC; re-login: `claude /login:windows`
- **claude · nas1/nas**: ends 2026-10-15 12:30 UTC. re-login before then: `claude-login --profile nas`
- **claude · vm/vm**: ends 2026-10-15 18:00 UTC. re-login before then: `claude-login --profile vm`
- **claude · mac-2/lab**: not readable on this machine. (ssh: connect refused (timed out))
- **muse · vm/build**: no expiry published. no login end time is published by muse
- **claude · mac/default** (mac@example.com): healthy. sign-in ends 2026-11-12 12:00 UTC; renews by itself from 2026-10-20 00:00 UTC; last renewed 2026-10-15 06:00 UTC
- **claude · vm/second**: healthy. refresh expires 2026-11-01 09:00 UTC
