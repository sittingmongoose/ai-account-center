# AI account usage for agents

The AI Account Center dashboard writes the quota left on every AI account it tracks into this
folder after each refresh (every minute by default). Any agent on any computer that can read the
folder can use it through the native AI Account Center CLI: no key and no network route. AI Account Center keeps every file
here current; edits are overwritten.

Run the published reader from this folder: `<this folder>/aac-usage` on Linux and macOS,
`<this folder>\aac-usage.cmd` on Windows. The launchers call the installed native
`ai-account-center usage` command; it must be on PATH. The retained source Python reader is
an offline test oracle and is not published. `locations.json` lists where each computer finds this
folder, as the dashboard's owner set it, and `aac-usage rules` includes them.

## Files

- `usage.json`: every account and its current reading. `generatedAt` is when the dashboard wrote it;
  past `staleAfterSeconds` the dashboard has stopped updating it.
- `usage.md`: the same reading in plain words.
- `history.json`: the measured readings of every window over the last 7 days (every changed reading
  in the last hour, 5-minute steps to a day, hourly beyond), each point
  `[time, used %, remaining, reset time]`. Never compute a rate across points whose reset times
  differ; the `burn` values in `usage.json` already respect resets.
- `locations.json`: where each group of computers finds this folder (`where`, `path`), from
  `usage-snapshot set <folder> --location "<where>=<path>"`; empty when none were given.
- `aac-usage`, `aac-usage.cmd`: the reader below.
- `computers.json`: the package-owned computer labels, copied from the shared registry.
- `claude-signins.json`, `claude-signins.md`: when each Claude Code sign-in on each computer ends,
  how its automatic renewal went and anything only a person can do (a CAPTCHA), written by the
  sign-in renewal runner after each run (see `claude-signin/README.md`). `staleAfterSeconds` old
  means the runner stopped.
- `claude-signin/`: the renewal runner itself, kept current by the dashboard; the installed runner
  updates itself from here.

## Which account is which

Each account lists its provider, `label` (its name on the dashboard), `email` (the signed-in
provider account), `profile` (AI Account Center's saved profile name, such as `gmail`), `id` (stable
across snapshots), `host` (the computer the quota reading came from; for Claude it can differ from
`platform`, the computer its desktop app opens on), `plan` and `active` (the account that
provider currently uses there). Match your own login by email; T3 names provider instances after
their account email. Tokens, credentials, file paths and raw provider answers are never written here.

## Readings and pace

`status: "ok"` means the last provider read succeeded and is still within its
refresh interval. Serving that successful sample between scheduled reads does
not make it cached and does not change `sampledAt`. `status: "cached"` means an
earlier verified sample was retained after a failed or skipped read. Its age
always comes from the real sample time, never the feed's `generatedAt`.

Version 1 snapshots may include the optional `cacheReason` on a cached account:
`read_failed`, `rate_limited`, `refresh_pending`, `source_changed`, or
`outcome_unknown`. These are fixed safe codes, never provider response text.
The reader and `usage.md` explain the cause alongside the sample age. Older
snapshots without the field say the previous read outcome was not recorded.
Existing fields and version stay compatible with older agent readers.


Each window carries the provider's own `usedPercent`/`remainingPercent`, amounts (`used`, `limit`,
`remaining`, `unit`) and `resetAt`. A missing number was not reported, not zero. `resetPassed: true`
means the reset time has passed since the reading; a new reading is pending.

`burn` measures percentage points used per hour since the window last reset: `last5Minutes`,
`last15Minutes`, `lastHour`, `last6Hours`, `last24Hours`. `pace` is the shortest span with enough
readings (two readings at least 2 minutes apart for 5 minutes) and, under an hour, a move of at
least 2 points, so a burst shows within a few minutes while a single rounding step does not (a
short-span rate can be set while `pace` comes from a longer span). `projectedFullAt` is when the
window reaches 100% at that pace and `runsOutBeforeReset` says whether that comes first. A rate
needs real readings in the same reset period; nothing is extrapolated across a reset.

## Reader

```
aac-usage                        every account: identity, each window, reset, pace, run-out
aac-usage --provider codex       codex, claude, antigravity, muse, cursor, kimi, qwen, zai, opencode
aac-usage --account gmail        accounts whose label, email or profile contains the text
aac-usage --json                 usage.json after the same filters
aac-usage history --provider claude --hours 6
aac-usage signins [--account me]  when each Claude Code sign-in ends and how its renewal went
aac-usage rules | install-rules  the agent rules block, or write it into this computer's rules files
```

Every view starts with a `NEEDS YOU (Claude sign-in): ...` line when the renewal runner is waiting
for a person; pass that on to them. The reader only reads these files. Switching accounts, sign-in and refreshes stay in the AI Account
Center dashboard. On the dashboard computer, `ai-account-center dashboard usage-snapshot status`
shows where it writes.
