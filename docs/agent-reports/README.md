# Agent reports: schemas and examples

JSON Schemas (draft 2020-12) for agent-reports v1: thread,
tasks, card, answer, run, status. `schemas/` holds the six schemas;
`examples/` holds one valid and two invalid examples per kind.

The same six schema files are published on the share at
/mnt/Cursor/agent-reports/v1/schemas/ for writers and readers.

Source of truth is the written spec on the share
(see the 2026-11-06-status-tabs folder); these files copy it.

Timestamps carry `"format": "date-time"` beside their pattern. A 2020-12
validator treats `format` as an annotation, so the pattern stays the hard
check of shape, and AAC (and agent-report, by the same rule) also requires a
real calendar moment: a real month and day with leap years counted, hour
00-23, minute and second 00-59 (no leap second, no 24:00), and an offset
within +-14:00. `2026-02-30T10:00:00Z` is refused, not read as March 2.

Optional Status v2 fields are additive on thread, tasks and status reports:
`title`, `now`, `next`, `stages`, `deliverables` (including versions and media),
`progress`, and `readmeQuestions`. Blockers retain `short`, `since`, `until`
and `card`; task items may carry `note`. The pinned composer retains the new
report data under `updates[].reported` for the forthcoming view; existing
rendered fields continue to use the v1 projection. Card-level media is retained
by the card reader too. This does not scan deliverable files for headings yet.

An unknown task state keeps the board and marks only that task row. A malformed
JSON document still uses the reader's last good copy. Review finding severity
counts may be null (unrated); null must never be displayed as zero. The current
Scoreboard does not draw those severity counts.

The schema files are publication templates. AAC fills machine enums and writer ID
patterns from `web-dashboard/public/computers.json` before publishing them; the
live TypeScript and Rust readers enforce that same fixed computer allowlist.
