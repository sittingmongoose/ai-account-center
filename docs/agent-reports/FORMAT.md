# Agent reports: the format (v1)

**Owner:** aac-status-lead (AI Account Center). It owns this file, the schemas and the catalog.
**Writer tool:** `agent-report` (tools-lead). Agents write with the command, never by hand.
**Readers:** AI Account Center's Status and Model Scoreboard tabs.
**Source:** AGENT-REPORTS-FORMAT.md (agreed 2026-11-06), PINNED-THREADS.md section 4, and the six schemas in
`v1/schemas/` as of 2026-11-11. Where this file and a schema differ, the schema wins: tell aac-status-lead.

How to report, for agents: `/mnt/Cursor/Agent-Guides/agent-reports.md`. This file is the format itself.

## Goals

1. **Every agent reports.** Every agent on the VM, the Mac, Windows and Nas1 reports its status, task progress,
   questions for the owner and finished runs. Writers on different machines can never corrupt each other.
2. **The owner's answers are theirs alone.** Only AAC writes them, signed, and any agent can check them.
3. **New things are data.** New projects, streams, models, measures and task types need no dashboard rebuild.
4. **No secrets anywhere** in these files.

## Where

`/mnt/Cursor/agent-reports/` on the VM and Nas1, `/Volumes/Cursor/agent-reports/` on the Mac,
`\\TRUENAS\Cursor\agent-reports\` on Windows.

```
agent-reports/
  FORMAT.md                         this file
  v1/
    schemas/<kind>.json             JSON Schemas (AAC): thread, tasks, card, answer, run, status
    catalog/                        AAC: models.json, task-types.json, measures.json, uses.json, benchmarks.json
    keys/aac-answers.pub            the public key that signs the owner's answers
    writers/<writer>/               only <writer> writes inside its own folder
      thread.json                   what the thread is doing (replaced whole)
      status.json                   the thread's own status update (replaced whole)
      tasks.json                    a stream lead's board (replaced whole; leads only)
      cards/<card>.json             one question for the owner per file
      runs/<YYYY-MM-DD>/<run>.json  one finished run or review per file, written once
    answers/<card>.json             written ONLY by AAC, signed
    answers/<card>.note-<12 hex>.json signed note history, written ONLY by AAC
    answers/<card>.object-<12 hex>.json signed objection history, written ONLY by AAC
    readers/aac/<writer>.json       written ONLY by AAC: what it did with that writer's files
    quota/                          quota-watch's share copy (quota-report@1)
```

quota-watch writes only `v1/quota/`: it has no writer folder and writes no reports under `writers/`.

AAC creates `v1/`, `v1/writers/` (writable by all) and the other folders. The reporter never creates `v1/`, so an
unmounted share can't grow a local folder.

## Writing rule (every file)

- **One writer per folder.** `writer` = `<machine>.<pm-mail name>`, such as `vm.tools-lead`. Machine is `vm`, `mac`,
  `windows` or `nas1`. Lowercase `[a-z0-9.-]`, 64 characters at most, no `--` (it separates a writer from a card or
  run id). A T3 thread with no pm-mail name writes as `<machine>.t3-<first 8 hex of sha256(T3 thread id)>`.
- **Whole files only.** Write `.<name>.tmp-<random>` in the same folder, flush, close, then rename it over the
  target. Never append, never edit in place.
- **Every document** carries `schema`, `writer`, `seq` and `writtenAt`.
  - `seq` rises on every write by that writer, across all its files: the highest in the folder + 1.
  - `writtenAt` is UTC, `YYYY-MM-DDTHH:MM:SS[.fff]Z`, and a real moment: no Feb 30, no hour 24, no leap second.
- **Size caps:** thread 16 KB, status 64 KB, card 64 KB, run 32 KB, tasks 512 KB.
- **No secrets:** no tokens, keys, cookies, passwords, email addresses or raw prompts. Paths, SHAs and thread ids are
  fine. AAC refuses a file that matches its secret patterns, and says which file.
- **Characters AAC refuses** in every text field (`CONTROL` in its `schema-helpers.ts`): control characters other than tab
  and line feed (a carriage return, ESC, DEL, U+0080-U+009F), the bidirectional marks U+061C, U+200E, U+200F, U+202A-U+202E
  and U+2066-U+2069 (common in text copied from right-to-left sources), and half of a UTF-16 pair. Tab and line feed are
  allowed. `agent-report` refuses the same set before it writes, and `validate` says the same. Text lengths count UTF-16
  units, as JavaScript does: an emoji counts twice.
- **T3 thread ids are not secrets:** `mcp:launch:<64 hex>`, `thread:delegated-task:...`, `thread:project:...`,
  `mcp:<uuid>` and plain UUIDs belong in `t3Thread` as T3 gives them. A delegated task's tail (its task's id or name) is
  still text: a token there is a secret like anywhere else.

**Readers (AAC):**
- List the folder, then read the file. A Mac rename over SMB is a delete then a rename, and an NFS client caches the
  miss for about 20 s unless the folder is listed.
- ENOENT and ESTALE mean "keep the last good copy, try again". Reject a file holding NUL bytes.
- A file that fails keeps its last good copy and shows "unreadable since <time>"; it is never dropped silently.
- A `seq` lower than the one already seen is ignored.

## thread.json: what this thread is doing

```json
{
  "schema": "agent-reports/thread@1", "writer": "vm.tools-lead", "seq": 42, "writtenAt": "2026-11-06T16:00:00Z",
  "ttlSeconds": 1800,
  "agent": { "name": "tools-lead", "harness": "claude-code", "model": "anthropic/claude-opus-5-5", "effort": "xhigh",
             "account": "claude_me", "machine": "vm", "t3Thread": "<id or null>", "parent": "pm7-build" },
  "project": "puppet-master", "stream": "TOOLS",
  "state": "working",
  "doing": "Blind Opus pre-check of browser-check 2.1",
  "blockedOn": null,
  "progress": { "done": 12, "total": 18, "unit": "items" },
  "links": [ { "label": "Board", "path": "/mnt/Cursor/share/puppet-master/2026-11-04-program/streams/TOOLS.md" } ]
}
```

- **Detected harness:** `--harness` wins. Otherwise use only this process's live markers: Claude Code's variables or its hook event;
  Codex's `CODEX_THREAD_ID`/`CODEX_SESSION_ID` or live parent process; and T3's provider instance asked in this invocation. Disagreement
  or no live markers writes null. A cached `.t3-asked.json` provider or previous report never supplies a harness, including under
  `AGENT_REPORT_T3=none`. The same detection fills a run's `model.harness`. A pm-mail Claude session file names the writer only when
  the local live harness is Claude Code; stale Claude variables in Codex cannot select another session's writer.
- **`state`:** `working`, `waiting` (on a child, a review or a lock), `blocked`, `paused`, `idle` or `done`.
- **`blockedOn`:** `{ "kind": "owner" | "agent" | "review" | "lock" | "resource" | "quota", "what", "since" }`, or
  null. `owner` means the owner: the page shows it as "Waiting on you".
- **`ttlSeconds`:** with no rewrite within it, the thread shows as stale with its age.
  - Null is allowed for `waiting`, `idle`, `done`, `paused` and `blocked`: a thread that ended its turn can't
    heartbeat. Those show their age, never "stale".
  - **Blocked with null is new (2026-11-11, the Fable review's B2).** A thread blocked on the owner that ends its turn must
    not turn stale and hide the blocker. AAC accepts it from its package S release; an earlier AAC refuses it.
  - `working` always carries a ttl, from 60 s to 7 days.
- **Unknowns stay null:** `project`, `stream`, `doing` and every `agent` field. A hook never guesses them.
- **`agent.model`** is the `model` field of `catalog/models.json`, `<provider>/<model>` such as `anthropic/claude-opus-5-5`, with the
  effort in `agent.effort`, never in the id (aac-status-lead, 2026-11-11). An id the catalog doesn't have is written as given. See
  "Model ids" under the runs below.
- **Pins:** `agent.t3Thread` equal to a pinned T3 thread's id puts this thread on the Status tab's Pinned panel. Its
  `project` and `stream` find the task list.
- **RULES-v2 additions (AAC's wave 3, published 2026-11-11 22:54Z):** `title` (1 to 60), `now`, `next`, `stages`, `deliverables`,
  `readmeQuestions`, and `short`, `until`, `card` on `blockedOn`, all optional. `blockedOn.since` is a timestamp or null: AAC's reader
  parses it as a date (wave 3 briefly published it as a plain string, a regression aac-status-lead fixed the same evening). `agent-report` writes `title` and the
  three `blockedOn` fields here, each only where the published schema names it; the rest go in `status.json` (below).

## status.json: the thread's own status update (`agent-reports/status@1`)

What `thread.json` and `tasks.json` lack: the answer the thread would give if the owner asked "where are you?". A thread
writes it at each milestone, before it ends a turn, and whenever the owner asks for status. One file per writer, replaced
whole. `agent-report update` writes it together with `thread.json`.

```json
{
  "schema": "agent-reports/status@1", "writer": "vm.pm7-build", "seq": 18, "writtenAt": "2026-11-11T03:00:00Z",
  "t3Thread": "<the T3 thread id>",
  "summary": "Sticker block landed; the tray icon is on its third fix round.",
  "percent": 70,
  "done": [ { "id": "F-08", "title": "Sticker block landed" } ],
  "left": [ { "id": "F-07", "title": "Tray icon sharp at 125% scale" } ],
  "blockers": [ { "text": "Pick the reviewers for B09", "waitingOn": "owner" } ],
  "looping": { "state": "unsure", "text": "F-07: third fix round, same finding" }
}
```

| Field | Required | What it holds |
|---|---|---|
| `schema` | yes | `agent-reports/status@1` |
| `writer` | yes | The writer id; it must match the folder |
| `seq` | yes | The writer's next seq, 0 to 2^53-1 |
| `writtenAt` | yes | UTC, as above |
| `t3Thread` | yes | The T3 thread this update belongs to, 1 to 200 characters. AAC matches it against the pinned threads |
| `summary` | yes | One or two plain sentences, 1 to 500 characters, as when the owner asks for a status update |
| `percent` | no | The thread's own estimate, a whole number 0 to 100, or null when it gives none |
| `done` | no | Items landed or done, newest first: `{ "id", "title" }`, at most 200 |
| `left` | no | Items not yet landed or done, the same shape, at most 200 |
| `blockers` | no | What the thread waits on: `{ "text", "waitingOn" }`, at most 50 |
| `looping` | no | The thread's own answer to "is anything looping?", or null |
| `title` | no | RULES-v2 (wave 3): the name of the work, 1 to 60 characters; set when the T3 pin title isn't one |
| `now` | no | RULES-v2: one sentence, 1 to 140 characters: what the thread is doing now. Missing: the page takes the summary's first sentence |
| `next` | no | RULES-v2: the next milestone, 1 to 100 characters |
| `stages` | no | RULES-v2: the thread's own steps, 2 to 7, in order: `{ "label", "state", "by", "waitingOn", "round", "of", "note" }` |
| `deliverables` | no | RULES-v2: everything there is to open, up to 12, each with a version (below) |
| `progress` | no | RULES-v2: a count the thread can prove, `{ "done", "total", "unit" }`, or null |
| `readmeQuestions` | no | RULES-v2 section 5 (wave 3): a boolean. `agent-report` doesn't write it yet |

- **Items:** `id` 1 to 64 characters (such as `F-08`, named as in the stream's task list when it has one); `title` 1
  to 300. An id may appear only once in `done` and once in `left`: AAC refuses a repeat.
- **RULES-v2 fields (AAC's wave 3, published 2026-11-11 22:54Z; RULES-v2 sections 1 to 4).** All optional and additive. `agent-report update`
  writes each only where the published schema names it, and otherwise leaves it out with one line saying so (agent-report 1.5).
  - **`stages[]`:** `label` 1 to 24; `state` `done`, `now`, `waiting`, `next` or `skipped` (at most one `now`; `skipped` isn't drawn);
    `by` `thread` (the default), `owner`, `reviewer` or `other`; `waitingOn`, `round` and `of` (1 to 99, or null), `note` (up to 40, or null).
    A new round is the same stage with `round` raised, never a new stage. `waitingOn` (one of the five, or null) is required when `waiting` and `by` isn't `owner`, and a
    third round or later, in any stage state, sets `looping.state` to `unsure` or `yes`: `agent-report update` refuses either missing.
  - **`deliverables[]`:** `id`, `name` (1 to 48), `kind` (`concept`, `report`, `document`, `build`, `decisions`, `video`, `screenshots`,
    `other`), `version` (1 to 12) and `updatedAt` are required; `path` or `url` (up to 400), `state`, `for`, `want`, `main`, `note`,
    `unlocks`, `cards`, `supersedes` (the version it replaces, or null), `versions` (older versions, newest first, up to 8, each with its own
    file) and `media` (up to 6: `kind` `image` or `video`, `path` or `url`, `poster`, `caption` up to 60, `label` `built` or `mockup`,
    `seconds`). A new version is a new file; the old one moves into `versions`. `agent-report update` refuses a new version without a new path or
    URL, or at a path that names a kept version's file (`.`, `..`, symlinks and hard links resolved; a relative path stored absolute).
    Symlinks resolve before `..`, through the longest existing prefix for missing files; comparison, stored paths and media use the same place.
    `--media 'caption=https://example.com/demo.mp4?a=1&b=2'` separates at the first `=` and keeps the URL unchanged; a bare HTTPS URL also
    stays unchanged. Local media retains `<path>[=caption]`.
  - **`blockers[]`** gain `short` (up to 80), `since` (when the wait began), `until` (when it should end, or null) and `card` (a card id, or
    null), each `string` or null in the published schema.
- **`blockers[].waitingOn`:** `owner` (the owner: shown as "Waiting on you"), `agent`, `review`, `lock`, `resource` or
  `quota`. `text` is 1 to 500 characters.
- **`looping.state`:** `yes`, `no` or `unsure`. Say `unsure` rather than `no` when a fix is on its third try.
  `looping.text` (up to 500, or null) says what repeats and how it shows, when the state says more than `no`.
- **The percent shown:** a task list's count comes first; the thread's own estimate stands beside it.
- **Only top-level T3 threads are pinned.** A delegated child's `t3Thread` never matches a pin. Its status still
  counts in the thread lists.
- **A refused `status.json`** keeps its last good copy, and the pin names the file and the reason.
- **Two files, two writes.** `agent-report update` writes `thread.json` and then `status.json`, each whole. A writer killed
  between the two writes leaves the new `thread.json` beside the previous `status.json`: the new `doing` and state over the old
  summary, lists and blockers. Nothing is torn, each file is ordered by its own `seq`, and the next update mends it.

## tasks.json: a stream's board (stream leads only)

```json
{
  "schema": "agent-reports/tasks@1", "writer": "vm.pm-fo-lead", "seq": 7, "writtenAt": "...",
  "project": "puppet-master", "projectLabel": "Puppet Master",
  "stream": "F", "streamLabel": "PM7 follow-ups", "owner": "pm-fo-lead",
  "goal": "Fix every PM7 follow-up before the owner's QA round",
  "board": "/mnt/Cursor/share/puppet-master/2026-11-04-program/streams/F.md",
  "items": [
    { "id": "FO-40", "title": "Welcome PiP look glitch", "state": "landed", "owner": "fo-welcome",
      "branch": "t3/fix/welcome-pip-look-glitches", "head": "369a1bcdfa",
      "reviews": [ { "by": "gpt-6.1-sol/max", "verdict": "pass", "at": "..." } ],
      "landed": { "sha": "c76d1cf756", "at": "..." }, "blockedOn": null, "updatedAt": "..." }
  ]
}
```

- **Item `state`.** The writer uses `todo`, `working`, `review`, `waiting`, `blocked`, `done` or `dropped`.
  Pinned Status maps old `fixing` to `review`, `landing` and `ready` to `working`, `landed` to `done`,
  `queued` to `todo`, and `superseded` to `dropped`. Ids don't repeat.
- **`needsYou`.** The full ID of an open question, judgement or action card filed by this writer.
  The pinned task row shows the question while it remains open and unanswered. The dashboard checks that
  relationship against its card reader because the task schema only has the ID.
- **Completion** = (`landed` + `done`) / (every item except `dropped`). The page shows the count, never only a
  percentage.
- **`reviews[].verdict`:** write `pass` or `fail` plainly, with any detail elsewhere. The loop rule reads these words.

## cards/<card>.json: a question for the owner

`<card>` = `<writer>--<local id>`, such as `vm.pm-fo-lead--Q3`. The full shape is in `schemas/card.json` and in
`/mnt/Cursor/share/ai-account-center/2026-11-08-leptos-redesign/DECISION-CARDS.md`.

- **`kind`:**
  - `question` (the default): 2 to 4 options, each with what the owner gets and what it costs.
  - `objection`: a call already made ("tell me if you object"). Silence is fine.
  - `action`: something only the owner can do. A Done button, no options.
  - `judgement`: a measurement; no recommendation, and "Can't tell" is a valid option.
  - `sample`: a format sample, never answered.
- **Fixed fields:** `title` is the question itself. `now` gives the situation in plain words, with `nowAsOf` for
  its counts.
- **`state`:** the agent sets only `open`, `withdrawn` (with `reason`) and `superseded` (with `supersededBy`).
  "Answered" comes only from `answers/`.
- **Media:** in the writer's own folder. Images are PNG, JPEG or WebP up to 2 MB; videos are MP4 or WebM up to 50 MB.
- **Sections:** `text`, `list`, `facts`, `table`, `compare`, `notCopying`, `unknowns`, `terms`, `callout`, `links`,
  `media`, `quote` and `planChange`. Ask aac-status-lead before using a new type.

## answers/<card>.json: the owner's answer (AAC only)

- **Signed:** Ed25519 over the canonical JSON (keys sorted at every level, no whitespace, UTF-8) of the answer
  without `signature`. The key is `keys/<keyId>.pub`, then `keys/aac-answers.pub`.
- **Which version:** the answer names the card version it answers: `cardSeq` and `cardSha256`.
- **`kind`:** `answer` (a choice; the card closes), `note` (the owner's words-only **Written response**; the card version answered closes), `object` (the owner's words against an
  objection card) or `done` (an action card).
- **Note and objection history:** a note may be in `<card>.note-<12 lowercase hex>.json`; an objection in `<card>.object-<12 lowercase hex>.json`.
  Only a verified signed record with the matching `kind` (`note` or `object`) and `cardId` equal to the part before the well-formed suffix is accepted there. Old notes remain readable, including notes in plain `<card>.json` files.
  A verified `cardId` matching the entire filename instead names a plain answer, even when the legal local id itself ends in
  `.note-<12 hex>` or `.object-<12 hex>`. Filter it by that literal card id, not by a shorter apparent history prefix.
  Preserve the kind, exact words and history; never infer a chosen option, or write, move or rename a signed record when reading it.
  `agent-report answers --card` includes both histories, shows objections as OBJECTED with exact words, and ends `--wait` on a written response
  even after a later edit or reply. Junk is reported but cannot veto a verified response; signed version/hash problems still exit 1.
  A previously read objection to an older card version does not end a new `--card --wait` after the card is edited; wait for a new
  verified signed record. Current-version objections still end the wait immediately. The older-note closing rule is unchanged.
  A verified signed record copied under multiple filenames shows once. Distinct signed records keep their history, even with identical words.
  With no card, `--wait` ends on a new verified signed record, not a copy of one already shown.
- **Delivery:** AAC rings the asker through pm-mail. The mail is only a doorbell. The signed file is the answer:
  `agent-report answers` verifies it.

## runs/<YYYY-MM-DD>/<run>.json: one finished run or review (the Scoreboard)

```json
{
  "schema": "agent-reports/run@1", "id": "vm.pm-closure-lead--b09", "writer": "vm.pm-closure-lead", "seq": 31,
  "writtenAt": "...", "source": "observed", "project": "puppet-master", "stream": "C",
  "task": { "id": "B09", "type": "audit-read", "title": "Re-audit batch B09", "size": { "records": 40 } },
  "model": { "id": "zai/glm-5.3", "provider": "zcode", "harness": "zcode", "effort": "max" },
  "startedAt": "...", "endedAt": "...", "wallSeconds": 1840, "outcome": "done",
  "cost": { "usd": null, "credits": null, "quota": { "account": "zcode", "window": "5h", "deltaPct": 3.5 },
            "tokens": { "in": null, "out": null, "cached": null }, "estimate": null },
  "scope": { "filesAssigned": 16, "filesRead": 4, "recordsAssigned": 40, "recordsJudged": 40 },
  "review": null, "flags": [], "evidence": []
}
```

- **`task.type`:** from `catalog/task-types.json`: `scout`, `plan`, `worknode-create`, `worknode-build`, `build`,
  `fix`, `review`, `test`, `research`, `mechanical`, `audit-read`, `docs`.
  - **A run with no type is not scored.** `agent-report run` refuses one (2026-11-11, the Fable review's B7).
  - A run the hook records for a finished delegate has `task.type` null until the parent completes it with a type
    (`agent-report run --delegate <taskId> --type ...`): the same run id in the same file, with a higher `seq`. A run that has
    a type is never rewritten.
  - The hook and manual command key that run and its `task.id` on the base task id. Continuation ids name the same task, whichever files first.
- **Model ids (`model.id`, and a thread's `agent.model`):** the catalog's form, `catalog/models.json`'s `model` field, such as
  `openai/gpt-6.1-sol` (aac-status-lead, 2026-11-11; about half the records before it used bare ids). The effort goes in `effort`:
  `agent-report` splits a known suffix off (`gemini-4-argon-high` is `google/gemini-4-argon` with effort `high`) when the rest is in the
  catalog and no other effort was given. This includes ids in `thread --file`, `run --file` and `run --model`: effort is resolved before
  mapping the id (`--effort`, then the file effort, then `CLAUDE_EFFORT`); any explicit effort keeps the id whole. A plain write carries
  the model/effort pair already written: `--model gemini-4-argon-high --effort low` stays that whole id with effort `low`, including
  a hook repeating the same transcript model. A newly given model resolves effort again; carried effort alone does not stop its split.
  An id the catalog doesn't have is written exactly as given, never guessed. `review.by` and a board's
  `reviews[].by` are the reviewer in the agent's words and aren't changed.
- **A review** is a run with `task.type` `review` and
  `review: { "of": "<run id>", "verdict": "pass" | "fail" | "cannot-verify", "findings": { "high", "medium", "low" },
  "overturned": true | false }`.
  - **Unrated findings:** aac-status-lead decided (2026-11-11) that `high`, `medium` and `low` may each be null when the reviewer gave
    no counts. **Published with AAC's wave 3** (2026-11-11 22:54Z). `agent-report run --findings unrated` writes the nulls only where the
    published `run.json` allows them, and otherwise refuses, naming the schema.
  - `overturned` is true when the reviewer found the reviewed work wrong, not merely improvable.
  - **A fail states `overturned` outright.** `agent-report run` refuses a fail without `--overturned yes` or `no`
    (2026-11-11, B5), and a review without `findings`.
  - **Until AAC's package M is live, a fail filed with `overturned: false` counts as passed** on the Scoreboard, which
    today counts every review not overturned as passed and doesn't read `verdict`. Package M reads it.
- **`source`:** `observed` (real work) or `benchmark` (with `"benchmark": "<id>"`). The Scoreboard keeps the two
  apart.
- **A delegate's run, from the parent's hook (agent-report 1.5):** filed only when T3's task status says the task finished: a final status,
  `workState` `result_available` and no `hasPendingChildRuns`. T3's `Delegated task ... [completed]:` line comes at every turn end of the
  child, including one that ends its turn waiting on its own long-job, so the line alone is a heartbeat. `endedAt` is the end of the
  child's last turn. Before 1.5 each turn end filed a run with outcome `done` and a short `endedAt` (TOOLS-AR-20261111-04).
- **`outcome`:** free text. `done` is success; `failed`, `cancelled`, `error`, `timed-out` and the like are failure.
- **`cost.estimate`:** `{ "source": "quota-watch", "method", "credits", "quota", "tokens" }`, kept apart from
  measured values. The Scoreboard labels estimates.
- **`flags[].kind`:** `shortcut`, `cheating`, `incomplete-read`, `fabricated`, `test-weakened`, `scope-cut` or
  `false-claim`, each with evidence.
- **Optional:** `process { id, version, sha256 }`, `metrics { name: number | null }` and `imported { from, at }`.
- **Unknown numbers stay null, never 0.** A run is written once: a second write of the same id is refused, except the
  parent's completion of the hook's untyped run above. AAC keeps the highest `seq` of each run id.

### Run corrections (additive run@1 revision, 2026-11-15)

Run ids and files remain write-once. Fix a filed run by writing a **new id** under the same writer:
`corrects: "<earlier run id>"`. It is a complete replacement, with its own type, model, effort,
review, times, costs and evidence. Nothing is inherited by readers. All ordinary required run
fields still apply. `corrects` is a nonempty run id of at most 194 UTF-16 units.

To retract a duplicate, write `corrects` together with `void: { "reason": "Duplicate copy" }`.
The reason is required, nonempty text of at most 500 UTF-16 units, subject to the ordinary text
and secret rules. A void still carries a complete valid run envelope (the CLI can copy the target
for this purpose). A void is never itself scored.

Readers first deduplicate each id by highest seq, then later writtenAt, then file path, as before.
Over the complete history, before date, source, task or model filters, resolve corrections per
writer. Keep only each chain's leaf: the record no accepted record corrects. If its leaf has void,
count nothing in that chain. A later complete correction of a void can restore the chain.

A review naming any accepted ancestor follows the target chain to its current leaf. Reviews of a
voided target stop counting, including as review runs. This removal also applies to reviews of
those removed reviews. The written review is unchanged on disk; the view projection changes
review.of. Ordinary missing review targets retain their existing ignored-review behavior.

Refuse a correction naming another writer's id, a missing target, or a cycle. Refuse descendants
of a refused correction too. Originals remain eligible when the attempted correction is refused.
Detect cycles before deciding conflicts. Among multiple valid corrections of the same target,
later writtenAt wins, comparing UTC seconds and the full accepted fraction padded to nanoseconds;
equal instants use higher seq, then lexicographically greatest ASCII run id. Historical reader
forms (up to nine fractional digits, optional seconds, and UTC offsets) remain accepted.
Report every loser as a conflict and refuse its descendants. A descendant's timestamp never lets
it bypass a lost branch. Refusals carry fixed reasons in the run Source and reader receipts.

**Compatibility:** this is additive, retaining `agent-reports/run@1`, just as the optional
RULES-v2 fields did. The schema's dated description records this revision. Existing records score
unchanged. Old readers that ignore additive fields cannot safely score correction records:
deploy all correction-aware readers before enabling the CLI flags or publishing corrections.
No claims of correction support are made for an old reader. Schema publication continues through
AAC's existing schema publisher; no writer or CLI implementation lives in this repository.

**CLI contract (tools-lead):**

```sh
agent-report run --corrects <id> [all the usual flags]
agent-report run --corrects <id> --void "<reason>"
```

The first command creates a fresh run id and seq under the current writer. It uses all ordinary
run flags, file input, validation and measured metadata rules; it does not silently merge the old
run's fields. A correction of a review requires its full review fields. The second command needs
no type, outcome, model or review flags: copy the target's complete valid envelope, replace id,
seq and writtenAt, set corrects and void.reason, and leave original evidence/history intact.
Do not measure or invent a new work duration, cost or quota delta for a retraction. Reject void
without corrects and an empty/invalid reason; reject conflicting replacement flags with void.

Resolve the supplied target in the current writer's complete history. Reject cross-writer,
missing, refused and already-superseded targets, and require the user to correct the current leaf
instead. This prevents the CLI from knowingly creating a conflict; readers still handle races.
Use a fresh file, never overwrite a typed original or the correction target, and never reuse the
hook-completion rewrite exception for a correction. Do not attach corrections through --delegate
in a way that rewrites that delegate's filed id. Unknown metadata stays null. Apply the standard
secret, timestamp, size and text checks to the whole new record before atomically writing it.

Probe the published run schema for both properties and void's corrects dependency before writing.
If unsupported, refuse with an actionable schema-support message, without changing any files.
Bad usage exits 2; invalid run/target/reason exits 1; unavailable root, lock or schema exits 3;
success exits 0 and prints the new id and the id it corrects (and 'void' for a retraction).
The normal reader check reports semantic refusals once AAC scans the new file. On a simultaneous
correction conflict, correct the winning leaf; do not delete or rewrite the losing file.

## readers/aac/<writer>.json: what AAC did with a writer's files (AAC only)

AAC writes one per writer after each scan that reads that writer's files. `agent-report check` reads it and tells the
writer what happened: accepted and under which pin, refused and why, or not read yet. **Status:** the shape below was
proposed to aac-status-lead on 2026-11-11 for package S; until AAC confirms it, `check` also reads the short form (one
row at the top level).

```json
{
  "schema": "agent-reports/reader@1", "reader": "aac", "writer": "vm.tools-lead",
  "writtenAt": "2026-11-11T03:00:00Z",
  "files": [
    { "file": "status.json", "seq": 12, "sha256": "<hex of the bytes read>", "readAt": "2026-11-11T02:59:40Z",
      "result": "accepted", "reason": null,
      "pin": { "t3Thread": "<id>", "title": "Build PMConcept7" } },
    { "file": "thread.json", "seq": 11, "sha256": "...", "readAt": "...", "result": "refused",
      "reason": "ttlSeconds is missing or not valid", "pin": null }
  ]
}
```

- **`file`:** the path under `writers/<writer>/`: `thread.json`, `status.json`, `tasks.json`, `cards/<id>.json` or
  `runs/<date>/<id>.json`.
- **`result`:** `accepted` or `refused`. `reason` is the fixed refusal sentence, never the file's text.
- **`pin`:** for `thread.json` and `status.json`:
  - the pinned T3 thread it matched;
  - null when no pinned top-level thread has its id;
  - left out for kinds pins don't apply to.

## What AAC reads besides these files

- the pm-mail registry (threads not reporting yet, labelled "from pm-mail");
- `/mnt/Cursor/aac-usage/usage.json` and AAC's own account data;
- `browser-check slots --json`, `simlock status` and a small machine collector over AAC's ssh hosts;
- T3's own database, read-only, for which threads are pinned (top-level only).

The stream boards stay the human record; the Status tab links them.

## Changes

- **2026-11-06:** v1 agreed (AGENT-REPORTS-FORMAT.md). The same day: card kinds, sections, run `process`,
  `metrics` and `imported`.
- **2026-11-08:** `status@1` added (PINNED-THREADS.md section 4).
- **2026-11-09:** every timestamp must be a real calendar moment (AAC and agent-report, the same rule).
- **2026-11-11:**
  - this file published, with the `status@1` section;
  - `ttlSeconds` null allowed for `blocked` (AAC package S);
  - `agent-report run` requires a type, and `overturned` on a fail;
  - `readers/aac/<writer>.json` proposed;
  - review fix round 1: the characters AAC refuses named (bidirectional marks; tab and line feed allowed); T3 thread
    ids not secrets; a fail with `overturned: false` still scores as passed until AAC package M; the parent completes
    the hook's untyped delegate run; a crash between `update`'s two writes described;
  - agent-report 1.4.1: model ids in the catalog's form with the effort apart (aac-status-lead's request); review findings may be
    null once the published `run.json` allows it (aac-status-lead's decision, not yet published).
  - AAC's wave 3 (22:54Z): the RULES-v2 fields published on `status@1`, `thread@1` and `tasks@1`; `run.json`'s finding counts may be null;
    `blockedOn.since` in `thread.json` and `tasks.json` became a timestamp or null (briefly a plain string at first: a regression, fixed).
  - agent-report 1.5: writes the RULES-v2 fields where the published schema names them (status: title, now, next, stages, deliverables,
    progress, the blocker's short, since, until and card; thread: title and blockedOn's short, until and card); a delegate's run only when
    T3 says the task finished; a new version only at a new file. Identity (writer, name, `t3Thread`, harness, account) is unchanged from
    1.4.1: identity by a session binding T3 confirms comes in 1.6.

- **2026-11-15:** additive run@1 revision: same-writer complete corrections and reasoned voids; chain resolution before filters, review retargeting, and explicit refusals/conflicts. Readers deploy before writers.
