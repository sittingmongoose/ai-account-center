# Agent usage snapshot

Coding agents on every computer (in T3 or not) can see how much quota each AI account has left, how
fast it is being used and when it resets. The dashboard writes its own readings into a shared
folder after every refresh; agents read the files. There is no key, no network route and no client
library: any agent that can read the network share can read the snapshot.

Source: `src/web-server/agent-usage/`. Command: `src/commands/usage-snapshot-command.ts`. Reader
and folder guide: `scripts/agent-usage/` (`aac-usage`, `aac-usage.cmd`, `README.md`).

## Turn it on

```
ai-account-center dashboard usage-snapshot set /mnt/share/aac-usage \
  --location "Linux=/mnt/share/aac-usage" \
  --location "the Mac=/Volumes/share/aac-usage" \
  --location "Windows=\\nas\share\aac-usage"
ai-account-center dashboard usage-snapshot status
ai-account-center dashboard usage-snapshot off
```

The folder (absolute, created when its parent exists) is saved in
`~/.ccs/agent-usage-snapshot.json` (0600). The running dashboard reads it after each refresh, so no
restart is needed. `off` stops writing; files already written stay.

Each `--location <where>=<path>` (at most 8) says where a group of computers finds the same folder.
They are saved with the folder, published as `locations.json`, and listed in the rules block that
`aac-usage rules` prints, so an agent on any computer knows where to look. Your own share names and
mount points stay in this private setting and the shared folder, never in the package. `set`
without `--location` keeps the saved locations.

## What it writes

After each analytics sample (the dashboard refresh interval, one minute by default), from the
readings that sample just recorded:

- `usage.json`: every account with its identity (`id`, provider, `label`, `email`, AAC `profile`,
  `source`, `host`, `plan`, `active`, `hidden`), status and sample age, and each window's reading
  with its `burn` (percentage points per hour over the last 5 and 15 minutes, 1, 6 and 24 hours,
  the `pace` used for the projection, `projectedFullAt` and `runsOutBeforeReset`);
- `usage.md`: the same in plain words;
- `history.json`: the measured readings of every window over the last 7 days;
- `locations.json`: the saved locations, rewritten only when they change.

Of the three data files `usage.json` is written last, so its `generatedAt` says that sample's
`history.json` and `usage.md` are in place. `locations.json` follows when the locations changed; a
failed write of it never fails the sample and is retried at the next one.
Readers treat a snapshot older than `staleAfterSeconds` (three refreshes, at least 5 minutes) as
stale. On the first write after a dashboard start, the dashboard also copies the packaged reader
and guide into the folder when their bytes differ, so the reader always matches the format.

## Readings, pace and privacy

The snapshot never starts a provider read: it publishes what the dashboard's own refresh read.
Every account and window field is copied from an allowlist (`agent-usage-projection.ts`): messages,
file paths, raw provider answers, credentials and tokens never reach the folder. A missing
percentage, amount or reset stays missing.

Pace comes from the quota history (every changed reading of the last hour, 5-minute buckets to a
day, hourly beyond) plus the one-minute readings the dashboard saw in the last hour, kept in
memory, so a burst is measured within a few minutes. A rate needs two real readings of the same
window in the same reset period: a fall in use or a moved reset time starts a new period, and
nothing is extrapolated across a reset. `pace` is the shortest span with enough readings.

One write runs at a time. On a hard network mount, if the file server stalls, one write waits and
later samples are skipped; the dashboard itself is never blocked. A failed write is
logged once per folder (`agent_usage_snapshot.write_failed`).

## Telling agents about it

`aac-usage install-rules` writes a short managed block (`<!-- aac-usage rules: start ... -->`) into
every agent rules file on that computer: `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
`~/.omp/agent/AGENTS.md`, `~/.gemini/config/GEMINI.md`, `~/.zcode/AGENTS.md`,
`~/.claude-t3/CLAUDE.md` and the T3 account files that exist (`~/.claude-t3/*/CLAUDE.md`,
`~/.codex-t3/AGENTS.override.md`, `~/.codex-t3/*/AGENTS.override.md`). It rewrites the block in
place, so hard-linked files stay linked. Cursor keeps user rules in its settings:
`aac-usage rules` prints the block to paste there.
