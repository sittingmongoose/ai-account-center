# Agent reports: schemas and examples

JSON Schemas (draft 2020-12) for agent-reports v1: thread,
tasks, card, answer, run. `schemas/` holds the five schemas;
`examples/` holds one valid and two invalid examples per kind.

The same five schema files are published on the share at
/mnt/Cursor/agent-reports/v1/schemas/ for writers and readers.

Source of truth is the written spec on the share
(see the 2026-11-06-status-tabs folder); these files copy it.

Timestamps carry `"format": "date-time"` beside their pattern. A 2020-12
validator treats `format` as an annotation, so the pattern stays the hard
check of shape, and AAC (and agent-report, by the same rule) also requires a
real calendar moment: a real month and day with leap years counted, hour
00-23, minute and second 00-59 (no leap second, no 24:00), and an offset
within +-14:00. `2026-02-30T10:00:00Z` is refused, not read as March 2.
