# Status tab: contract (v1)

The Status tab answers "how are we looking?" without asking an agent: task progress for every project and stream,
open questions for the owner as decision cards, threads, machines, browsers and recordings, Simlock devices and account
usage. It is the first page of the Leptos web UI (see "Web UI move" below).

Agents report through the files described in
`/mnt/Cursor/share/ai-account-center/2026-11-06-status-tabs/AGENT-REPORTS-FORMAT.md` (published as
`<reports>/FORMAT.md`). This document is the contract between the AAC server and the page.

## Pieces

| Piece | Where | Owns |
|---|---|---|
| Agent reports reader | `src/web-server/status/` | reading `<reports>/v1/**`, last good copy, staleness |
| Collectors | `src/web-server/status/` | pm-mail registry, machines (ssh), `browser-check slots --json`, `simlock status`, usage |
| Status view | `src/web-server/status/status-view.ts` | every number, word, state and warning the page shows (pure, tested) |
| Answers | `src/web-server/status/answers.ts` | the owner's answers: validation, signing, private store, share copy, pm-mail doorbell |
| Routes | `src/web-server/routes/status-routes.ts` | `/api/status`, `/api/status/answers*` |
| Page | `web-dashboard/leptos/` (Leptos 0.9.0-rc1, client-side rendered) | drawing only: never computes a reading |
| Shell tab | `web-dashboard/ui/shell/`, `web-dashboard/public/bridge.js` | the Status and Scoreboard tabs in the Slint shell, the iframe over the canvas |

The reports root is `/mnt/Cursor/agent-reports` unless `AAC_AGENT_REPORTS_DIR` names another (tests and the phone test
server always set it to a temporary fixture folder). The private store is `<ccsDir>/status/` (`getCcsDir()`).
Test servers also set `AAC_PM_MAIL_REGISTRY_DIR` (default `/mnt/Cursor/Inbox/registry`, read-only) and
`AAC_STATUS_DOORBELL=off` (answers are stored and signed, nobody is rung). `node scripts/status-fixture.mjs <dir>
--ccs-dir <dir>` writes a realistic fixture and prints that environment.

**Collectors.** Agent reports: rescanned every 10 s (stat every file, read only changed ones; the share has no change
notification). pm-mail registry: every 30 s. Machines (one short read-only command per host over the app-update ssh
aliases; the VM from `/proc` and `statfs`), `browser-check slots --json` and `simlock status --json`: once a minute
while `GET /api/status` was called in the last 15 minutes, and at once on the first read after a quiet spell (they
start processes and ssh sessions; nobody watching, nothing runs). Usage: every dashboard analytics sample, never a
provider refresh. quota-watch: `unconfigured` until its report format is agreed.

## URLs
- `/status` and `/scoreboard`: the Slint shell with that tab selected (page routes, like `/analytics`).
- `/app/status` and `/app/scoreboard`: the Leptos pages themselves. `?embed=1` hides the page's own header when the
  shell shows it in its iframe. Both need the same signed-in session as the dashboard.
- `GET /api/status`: the whole view below. `Cache-Control: no-store`. The service worker never touches `/api`.
- `POST /api/status/answers`: one answer. `POST /api/status/answers/batch`: "approve all, except ...".
- `GET /api/status/answers?limit=N`: the read-only answer history, newest first: `{ answers: [ { id, card: { id, seq,
  title, project, stream, askedBy, options, recommendation, cardSha256 }, answer: { choice, note, via, batchAnswer,
  answeredAt, device, delivery } } ] }`.
- `/ws` sends `{ "type": "status-changed" }` to browser-session sockets when a source changes (at most one every 5 s);
  the page re-reads `/api/status`. The page also
  re-reads every `refresh.pollSeconds` while visible, so a missed hint only delays.

All routes: a browser session (`authKind(req) === 'session'`; no session `401 auth_required`, a device token
`403 device_scope`), `Cache-Control: no-store`, errors `{ error, code }` with fixed sentences that never echo input.
`GET /api/status` takes no query (`400 unexpected_query`); the history takes only `limit` (1 to 500, default 50,
else `400 invalid_limit`). POSTs: no query string (`400 unexpected_query`), the dashboard Origin
(`403 origin_required`), `application/json` (`415 json_required`), at most 16 KB (`413 body_too_large`) and exactly
the documented keys (`400 invalid_body`).

## GET /api/status (view v1)

```jsonc
{
  "version": 1,
  "generatedAt": "2026-11-06T16:30:00.000Z",
  "refresh": { "pollSeconds": 30 },
  "headline": {
    "tone": "good" | "warn" | "bad",
    "runs": [ { "text": "3 questions", "strong": true, "tone": "warn" }, { "text": " for you · 41 of 88 items landed" } ],
    "counts": { "openCards": 3, "blockedItems": 2, "workingThreads": 9, "staleSources": 1 }
  },
  "sources": [ Source ],
  "cards": { "source": "agent-reports", "open": [ Card ], "answered": [ AnsweredCard ], "batches": [ Batch ] },
  "projects": [ Project ],
  "threads": { "source": "...", "rows": [ Thread ] },
  "machines": { "source": "...", "rows": [ Machine ] },
  "simlock": { "source": "...", "capacity": [ Capacity ], "leases": [ Lease ] },
  "usage": { "source": "...", "warnings": [ UsageWarning ], "accounts": [ UsageAccount ] }
}
```

Every number the page shows is in this view, already rounded and worded. A missing value is `null` with a reason
word, never 0. Times are ISO UTC; the page shows them as relative ages ("2m ago") and Eastern 12-hour on hover/tap.

**Source** (one per input; every panel names the one it shows):
```jsonc
{ "id": "agent-reports" | "pm-mail" | "machines" | "browsers" | "simlock" | "usage" | "quota-watch",
  "label": "Agent reports",
  "state": "ok" | "stale" | "missing" | "unreadable" | "error" | "unconfigured",
  "asOf": "ISO or null", "ageSeconds": 12, "staleAfterSeconds": 300,
  "detail": "Plain sentence, e.g. '2 files unreadable since 4:02 PM: vm.foo/tasks.json, ...'" }
```
A panel whose source is not `ok` draws its last good data greyed with the source's `detail` on top, or only the
detail when there is no data. Nothing is ever shown as live when its source is stale.

**Card** (open): the card file's fields after validation (`id`, `writer`, `seq`, `project`, `stream`, `askedBy`,
`title`, `context`, `whatChanged`, `options[]` (`key, label, detail, pros[], cons[]`), `recommendation`, `allowNote`,
`multiple`, `urgency`, `blocks`, `batch`, `evidence[]`, `askedAt`) plus `ageSeconds`, `askerState` (the asking thread's
state, `"gone"` when its thread is stale, `"unknown"` when the writer has no `thread.json`), `cardSha256` (SHA-256 of the
card file's bytes), `problem` (the file's current problem, or null) and `answerable` (false while the shown copy is the
last good one of a file that no longer reads). Sorted by urgency, then age (oldest first). A card version that has an
answer leaves `open`; a card rewritten with a higher `seq` after its answer is open again (the old answer stays in the
history, naming the version it answered).

**AnsweredCard**: `{ card: Card, answer: { choice, note, via, batchAnswer, answeredAt, device, delivery: { state:
"rang" | "failed" | "pending", at, detail, share: Step, doorbell: Step } } }`, newest first, at most 50 (the full
history is `GET /api/status/answers`). **Step**: `{ state: "pending" | "done" | "failed" | "skipped", at, detail,
attempts }`. `failed` covers a share write or doorbell still being retried, one that gave up after 20 attempts, and a
card with no pm-mail recipient (`skipped` doorbell); `detail` says which.

**Batch**: `{ id, label, cardIds: [...], withRecommendation: N }`: open cards that share `batch`, for the
"Approve all, except ..." control. Only cards that have a recommendation can be approved that way.

**Project**: `{ id, label, streams: [ Stream ], counts, completion }` (sums of its `tasks` streams). **Stream**: `{ id,
label, owner, goal, writer, board: { path, ageSeconds } | null, source: "tasks" | "board-only", asOf, ageSeconds, stale,
problem, counts: { total, landed, done, ready, review, fixing, working, blocked, todo, dropped }, completion: { done: N,
of: M }, items: [ Item ] }`. `completion.done` = landed + done, `of` = every item except dropped; null when that is 0.
**Item**: the `tasks.json` item (`id, title, state, owner, branch, head, reviews[], landed, blockedOn, why,
updatedAt`) plus `ageSeconds`. A stream that has only a board (a reporting thread with a `Board` link and no
`tasks.json` for its project and stream) shows its board link and age, `counts: null` and `completion: null`. A
`tasks` stream is `stale` when its file is unreadable or was not rewritten for a day. Board ages come from the file's
modification time, read only for paths inside the share that holds the reports folder (VM, Mac and Windows spellings
of the share are mapped); elsewhere `ageSeconds` is null.

**Thread**: `{ name, writer, machine, project, stream, state, doing, blockedOn, progress, model, effort, account,
source: "report" | "pm-mail", lastSeen, ageSeconds, stale, clockAheadSeconds, problem }`. Threads quiet for more than
a day are left out unless they report `blocked`. `lastSeen` is the older of the writer's `writtenAt` and when AAC first
saw that version, so a writer whose clock runs ahead cannot look fresh; `clockAheadSeconds` says by how much when over
2 minutes. A report is `stale` after its `ttlSeconds` or while its file is unreadable. pm-mail rows (registry threads
with no report, seen in the last day) have `state: null`, `doing` = their about line, and are stale after 30 minutes.
`blockedOn.kind` is any short slug (documented: `owner`, `agent`, `review`, `lock`, `resource`, `quota`).

**Machine**: `{ id: "vm" | "mac" | "windows" | "nas1", label, state: "ok" | "stale" | "unreachable", asOf, ageSeconds,
detail, cpu: { cores, load1, busyPct }, memory: { availableGb, totalGb }, disk: [ { mount, freeGb, totalGb } ],
browsers: { running, max }, recordings: { running, max }, gpuPct, sessions: [ { id, url, owner, recording,
ageSeconds } ], warnings: [ "Disk 17.7 GB free, under the 50 GB floor" ] }`. Floors: 50 GB disk, 6 GB memory (the
shared machine rules). GB here is GiB (as `free -g` and `df -h`). `unreachable` keeps the last good reading with its
time in `detail`. Windows has no load average (`load1: null`). Session URLs are host and path only (a file URL shows its
path); query strings and fragments are never shown. `browsers`, `recordings`, `gpuPct` and `sessions` come from
`browser-check` (null or empty when it has no row for that machine).

**Capacity** (Simlock): `{ platform, running, max, reserved, overLimit }` (one row per platform Simlock lists,
including `global`). **Lease**: `{ device, deviceLabel, platform, holder, since, expiresAt }`. Device ids and holders
are shown as Simlock reports them (`holder` is the worker label, else the requester id); lease ids, owner ids and tokens
never.

**UsageAccount**: `{ id, provider, label, host, active, window, kind, usedPct, remaining, limit, unit, resetsAt,
projectedFullAt, runsOutBeforeReset, atLimit, stale }`, one row per rate-limit window and per balance (Codex credits:
`kind: "balance"` with `remaining`/`unit`), from AAC's own dashboard samples and the same pace math as
`aac-usage` (no extra provider refresh). Accounts hidden on the dashboard are left out. **UsageWarning**: `{ tone,
text }`, worst first, only from current readings: `bad` "claude main (7-day): at its limit until its Nov 12 4:00 PM
reset", `warn` "claude main (7-day): full in 1d 11h, before its Nov 12 reset".

## Answers

`POST /api/status/answers` body: `{ "cardId": "...", "cardSeq": 2, "cardSha256": "...", "choice": ["A"], "note": "..." }`.
- The card must be open on disk at that `seq` and hash (read fresh, not from the last scan) and that version not yet
  answered. Otherwise `409 card_changed` and the page reloads the card. Answers run one at a time.
- `choice` keys must be the card's option keys, without repeats (one unless `multiple`); a note alone is allowed when
  `allowNote`, and a note on a card without `allowNote` is refused. Otherwise `400 invalid_choice`.
- The note is stored exactly as typed (trimmed of surrounding whitespace only), at most 4,000 characters, no control
  characters other than tab and newlines (`400 invalid_note`).
- `200 { answer: { id, card, answer } }`; `500 answer_not_saved` when the private store could not be written (nothing
  else happens then).
- Stored in `<ccsDir>/status/answers.jsonl` (append, local disk, this server only), then written signed to
  `<reports>/v1/answers/<card>.json` (temporary file and rename), then the asker is rung through pm-mail. Each step's
  result is kept in the history; a failed share write or doorbell is retried and shown, never hidden.
- Signature: Ed25519 over the canonical JSON of the answer without `signature` (object keys sorted at every depth, no
  whitespace, values as `JSON.stringify` writes them). The key pair lives in `<ccsDir>/status/answer-key.pem` (PKCS#8,
  0600), created on first use; the public key (SPKI PEM) is published at `<reports>/v1/keys/aac-answers.pub` with
  `keyId` = first 16 hex characters of the SHA-256 of the public key's DER (SPKI) bytes. `answeredBy` is `"owner"`;
  `device` is a word from the browser (`iPhone`, `iPad`, `Android`, `Mac`, `Windows`, `Linux`, `browser`).
  `verifySignedAnswer(fileText, publicKeyPem)` in `src/web-server/status/answer-signing.ts` is the check a reporter
  mirrors.
- The doorbell runs `pm-mail --as aac-answers send <replyTo.pmMail> "Answer for <card>" -m "<body>"` only after the
  signed copy is on the share; the body names the card and the answer file, never the note.
- No agent path exists: the routes take a browser session only (not a device token, not localhost without auth).

`POST /api/status/answers/batch` body: `{ "batch": "id", "cards": [ { "id", "seq", "cardSha256" } ], "except": [ ids ] }`
approves the recommendation of every listed card not in `except`, each as its own answer with
`via: "approve-all-except"` and the same `batchAnswer` id. The exceptions stay open. A card that changed is refused
individually (`card_changed`) and the rest proceed; the result lists each card's outcome: `200 { batchAnswer, results:
[ { id, outcome: "answered" | "excepted" | "card_changed" | "not_in_batch" | "no_recommendation" |
"answer_not_saved", answer? } ] }`. At most 50 cards.

## Web UI move

The owner approved moving the web UI from Slint to Leptos on 2026-11-06 (Leptos 0.9.0-rc1, client-side rendered, served by
this TypeScript server). The Status and Scoreboard pages are the first Leptos pages, shown inside the Slint shell by an
iframe until the shell itself moves. The JavaScript view-model layer (`web-dashboard/public/*.mjs`) stays.
