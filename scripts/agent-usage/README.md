# AI account usage for agents

The AI Account Center dashboard on the Ubuntu VM writes the quota left on every AI account the user
uses into this folder after each refresh (every minute by default). Any agent on any computer can
read it: no key, no network route, nothing to install. AI Account Center keeps every file here
current; edits are overwritten.

| Computer | Folder | Run |
| --- | --- | --- |
| Ubuntu VM, Nas1 | `/mnt/share/aac-usage` | `python3 /mnt/share/aac-usage/aac-usage` |
| Mac | `/Volumes/share/aac-usage` | `python3 /Volumes/share/aac-usage/aac-usage` |
| Windows | `\\NAS\share\aac-usage` | `python \\NAS\share\aac-usage\aac-usage` |

## Files

- `usage.json`: every account and its current reading. `generatedAt` is when the dashboard wrote it;
  past `staleAfterSeconds` the dashboard has stopped updating it.
- `usage.md`: the same reading in plain words.
- `history.json`: the measured readings of every window over the last 7 days (every changed reading
  in the last hour, 5-minute steps to a day, hourly beyond), each point `[time, used %, remaining]`.
- `aac-usage`, `aac-usage.cmd`: the reader below.

## Which account is which

Each account lists its provider, `label` (its name on the dashboard), `email` (the signed-in
provider account), `profile` (AI Account Center's saved profile name, such as `gmail`), `id` (stable
across snapshots), `host` (the computer AAC reads it on), `plan` and `active` (the account that
provider currently uses there). Match your own login by email; T3 names provider instances after
their account email. Tokens, credentials, file paths and raw provider answers are never written here.

## Readings and pace

Each window carries the provider's own `usedPercent`/`remainingPercent`, amounts (`used`, `limit`,
`remaining`, `unit`) and `resetAt`. A missing number was not reported, not zero. `resetPassed: true`
means the reset time has passed since the reading; a new reading is pending.

`burn` measures percentage points used per hour since the window last reset: `last5Minutes`,
`last15Minutes`, `lastHour`, `last6Hours`, `last24Hours`. `pace` is the shortest span with enough
readings (two readings at least 2 minutes apart for 5 minutes), so a burst shows within a few
minutes. `projectedFullAt` is when the window reaches 100% at that pace and `runsOutBeforeReset`
says whether that comes first. A rate needs real readings in the same reset period; nothing is
extrapolated across a reset.

## Reader

```
aac-usage                        every account: identity, each window, reset, pace, run-out
aac-usage --provider codex       codex, claude, antigravity, muse, cursor, kimi, qwen, zai, opencode
aac-usage --account gmail        accounts whose label, email or profile contains the text
aac-usage --json                 usage.json after the same filters
aac-usage history --provider claude --hours 6
aac-usage rules | install-rules  the agent rules block, or write it into this computer's rules files
```

The reader only reads these files. Switching accounts, sign-in and refreshes stay in the AI Account
Center dashboard. On the dashboard computer, `ai-account-center dashboard usage-snapshot status`
shows where it writes.
