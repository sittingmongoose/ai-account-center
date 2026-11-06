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

**Reports folder.** At startup AAC creates `<reports>/v1/`, `v1/writers/` (mode 0777) and `v1/answers/`,
`v1/keys/`, `v1/schemas/`, `v1/catalog/` when missing, but never the reports root itself (an unmounted share must not
grow a local folder). Writers are `<machine>.<pm-mail name>`, or `<machine>.t3-<first 8 hex of sha256(T3 thread
id)>` for a T3 thread with no pm-mail name. Runs carry `seq` like every other document; their `cost.estimate` (`{
source, method, credits, quota, tokens: { in, out, cached } }`) is kept apart from measured cost. The Status server
does not read runs yet (they feed the Scoreboard).

**Reading rules.** For each writer the reader lists the folder (and `cards/`) first and then reads what the listing
names: a Mac rename-over through SMB is a delete then a rename, and NFS caches a missed lookup for about 20 s unless
the folder is listed. ENOENT and ESTALE (Node: "Unknown system error -116") on a listed file, or on listing a writer
folder, keep the last good copy and retry on the next scan; they never count as the thread going away. A file absent
from its folder's listing keeps its last good copy for 2 minutes before it counts as removed. A file containing a NUL
byte is refused (Mac SMB appends wrote NULs into shared files). The fresh card read behind every answer also lists
`cards/` first.

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
- `POST /api/status/answers`: the owner's answers and notes, sent together from the deck's review screen.
- `GET /api/status/answers?limit=N`: the read-only answer history (answers and notes), newest first: `{ answers: [ {
  id, card: { id, label, seq, title, project, stream, askedBy, options: [ { key, label } ], recommendation, cardSha256
  }, answer: { kind, choice, note, via, answeredAt, device, delivery } } ] }`.
- `GET /api/status/files/<writer>/<path>`: an image or video a card names, from that writer's own folder only (no
  `..`, no links out of the folder). The type comes from the content, never the name: images PNG, JPEG or WebP up to
  2 MB; videos MP4 (an `ftyp` box; still-image brands such as HEIC/AVIF refused) or WebM (an EBML header naming
  `webm`) up to 50 MB. Every file answers byte ranges (`Accept-Ranges: bytes`, one `Range: bytes=` range gives
  `206 Partial Content` with `Content-Range`, a range past the end gives `416`): iOS Safari plays no video without
  them. `HEAD` gives the headers only. `Cache-Control: private, max-age=300`, `X-Content-Type-Options: nosniff`.
  Refusals: `404 not_found` (missing, outside the folder, not a writer, bad path), `413 too_large`, `415 not_media`,
  `416 range_not_satisfiable`.
- `/ws` sends `{ "type": "status-changed" }` to browser-session sockets when a source changes (at most one every 5 s);
  the page re-reads `/api/status`. The page also
  re-reads every `refresh.pollSeconds` while visible, so a missed hint only delays.

All routes: a browser session (`authKind(req) === 'session'`; no session `401 auth_required`, a device token
`403 device_scope`), `Cache-Control: no-store`, errors `{ error, code }` with fixed sentences that never echo input.
`GET /api/status` takes no query (`400 unexpected_query`); the history takes only `limit` (1 to 500, default 50,
else `400 invalid_limit`). POSTs: no query string (`400 unexpected_query`), the dashboard Origin
(`403 origin_required`), `application/json` (`415 json_required`), at most 64 KB (`413 body_too_large`) and exactly
the documented keys (`400 invalid_body`). The files route sets `Cache-Control: private, max-age=300` instead of
`no-store`.

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

**Card** (open): the card file's fields after validation (`id`, `label`, `writer`, `seq`, `project`, `stream`,
`askedBy`, `kind`, `title`, `now`, `nowAsOf`, `oldAnswer`, `whatChanged`, `options[]`, `recommendation`, `multiple`,
`urgency`, `blocks`, `sections[]`, `batch`, `evidence[]`, `replies[]`, `neededBy`, `exception`, `reversible`,
`askedAt`) plus `ageSeconds`, `askerState` (the asking thread's state,
`"gone"` when its thread is stale, `"unknown"` when the writer has no `thread.json`), `cardSha256` (SHA-256 of the card
file's bytes), `notes[]` (the owner's notes already sent on this card, oldest first, each `{ note, at, delivery }`),
`problem` (the file's current problem, or null) and `answerable` (false while the shown copy is the last good one of a
file that no longer reads). Sorted by urgency, then age (oldest first). A card version that has an answer leaves
`open`; a card rewritten with a higher `seq` after its answer is open again (the old answer stays in the history,
naming the version it answered). Card fields (from the earlier card decks, research in `CARDS-RESEARCH.md` on the
share):
- `label`: a short id the owner can quote anywhere ("F-Q3"); never reused or relabelled. A file without one shows its
  local id (the part after `--`).
- `title`: the question itself. `now`: the situation today in plain words, no internal ids in its first sentence,
  with `nowAsOf` (when the counts in it were taken). The reader accepts `context` as an alias of `now`.
- `oldAnswer` `{ text, at }` and `whatChanged`: when a question comes back because its premise moved, the earlier
  answer quoted and what changed. Otherwise both null: an answer that still holds is never asked again.
- `options[]`: `{ key: "A", label, gets, costs, media[], sections[] }`, two to four options (at most 6 media and 10
  sections each). `gets`: what the owner sees or gets; `costs`: what it costs or changes elsewhere.
- **Media** (option `media[]` and `media` sections): `{ kind: "image" | "video", path, caption, label: "built" |
  "mockup" | null, poster }`. `path` and `poster` are relative to the writer's own folder (plain segments, no `..`,
  such as `cards/<card>.assets/a.webp`); the view adds `url` and `posterUrl` (`/api/status/files/<writer>/<path>`,
  null without a poster). Every image opens full screen when tapped; every video has a full-screen button.
- `recommendation` `{ key, why, confidence: "low" | "medium" | "high" | null, flip }`: marked on its option and again
  with the one-line reason; `flip` says what would change it (or null).
- `blocks`: what waits on the answer, or null for "nothing waits on this" (a string).
- `sections[]`: the card's content sections, in order, at most 30; only what the question needs. Each is `{ type,
  title | null, fold }` (`fold: true` starts collapsed) plus its type's fields: `text { body }` (a blank line starts a
  paragraph; no markup), `list { items, ordered }`, `facts { rows: [ { label, value } ] }`, `table { columns, rows }`
  (every row as wide as `columns`; cells may be empty), `compare { items: [ { name, does, problem, link: { label, url }
  | null, version } ] }`, `notCopying { items: [ { what, why } ] }`, `unknowns { items }`, `terms { items: [ { term,
  meaning } ] }`, `callout { tone: "info" | "warn" | "crit", body }`, `links { items: [ { label, url } ] }` (an http(s)
  URL opens in a new tab; a share path is shown to copy; anything else is refused) and `media { items: [ Media ] }`.
  Known types are validated strictly (a bad one makes the card file unreadable, by name); a type this server does not
  know arrives as `{ type, title, fold, unknown: true }` and the page says it has a section it cannot show yet. The
  card's 64 KB cap includes its sections.
- `replies[]` `{ at, text }`: the agent's replies to the owner's notes, in the card file (a new `seq`).
- `kind`: `question` (the default; two to four options, answered in the deck), `objection` (a call the agent already
  made, "tell me if you object": shown in a quiet list with the decision, the reason and an Object button; no options
  or two to four; silence is not an answer, nothing is signed unless the owner objects, and the card stays listed) or
  `action` (something only the owner can do, such as signing in or changing a NAS setting: a Done button, no
  options).
- `neededBy` `{ at: ISO | null, ifNoAnswer }` | null: when an answer is needed and what happens or starts costing
  without one. `exception` `{ rule, text }` | null: the owner's rule this card loosens and how; shown before anything
  else. `reversible` `{ yes: true | false, how }` | null.
- A withdrawn card may carry `reason` ("Settled by your note"). There is no `allowNote`: the note box is always there.
- A count or premise that changes after a card was shown: the agent supersedes the card; the new card's `whatChanged`
  gives old and new values and `oldAnswer` quotes any earlier answer.

**AnsweredCard**: `{ card: Card, answer: { kind: "answer" | "object" | "done", choice, note, via, answeredAt, device, delivery: { state:
"rang" | "failed" | "pending", at, detail, share: Step, doorbell: Step } } }`, newest first, at most 50 (the full
history, notes included, is `GET /api/status/answers`). **Step**: `{ state: "pending" | "done" | "failed" |
"skipped", at, detail, attempts }`. `failed` covers a share write or doorbell still being retried, one that gave up
after 20 attempts, and a card with no pm-mail recipient (`skipped` doorbell); `detail` says which.

**Batch**: `{ id, label, cardIds: [...] }`: open cards that share `batch`, kept together in the deck.

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
2 minutes. A report is `stale` after its `ttlSeconds` or while its file is unreadable. `ttlSeconds` may be null only
when `state` is `waiting`, `idle`, `done` or `paused` (a thread that ended its turn cannot heartbeat): such a thread
shows its age and is never stale for lack of a heartbeat (null with `working` or `blocked` makes the file unreadable). pm-mail rows (registry threads
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

The owner answers in a deck: one card at a time with Back and Next, a row of card labels to jump, and nothing is sent
until the review screen at the end ("Send N answers"). "Send current selections" jumps to that review screen early,
for a long deck. "Use the recommendations" fills every card still empty with its recommendation; the owner can change
any of them before sending. Unsent choices and notes are kept on this device (browser storage) so leaving the page
loses nothing; they are drafts, never answers.

`POST /api/status/answers` body: `{ "answers": [ { "cardId", "cardSeq", "cardSha256", "choice": ["A"] | [], "note": "...",
"via": "deck" | "recommended", "kind"?: "answer" | "note" | "object" | "done" } ] }`, 1 to 50 entries, at most 64 KB. Each entry is validated and recorded on its
own, in order, one set at a time; the response is `{ "results": [ { cardId, outcome: "recorded" | "card_changed" |
"refused", code, error } ] }`, one per entry in request order (HTTP 200 when the request itself is valid, even if some
entries are refused; `code` and `error` are null for `recorded`; `cardId` is null when the entry did not name a valid
card id). Entry codes: `card_changed`, `invalid_entry` (not the strict entry shape), `invalid_choice`, `empty_entry`
(no choice and no note), `invalid_note`, `invalid_via`, `invalid_kind` (the entry's kind does not fit the card's
kind), `words_required` (an objection without the owner's words), `answer_not_saved` (the private store could not be
written; nothing else happened for that entry).
- Record kinds: `answer` (a choice, `question` cards only), `note` (a note alone, any card; the card stays open),
  `object` (`objection` cards: no choice, the owner's words required) and `done` (`action` cards: no choice, words
  optional). Without `kind`, an entry with a choice is an `answer` and one with only a note is a `note`; `object` and
  `done` must be sent as `kind`. `answer`, `object` and `done` close the card version; `note` does not.
- The card must be open on disk at that `seq` and hash (read fresh, not from the last scan) and that version not yet
  answered. Otherwise that entry is `card_changed` and the page reloads the card.
- `choice` keys must be the card's option keys, without repeats (one unless `multiple`).
- One note box serves a different answer, a comment and a question. With a choice, the record is `kind: "answer"` and
  the card closes. With only a note, the record is `kind: "note"`: it is signed and delivered the same way, but the
  card stays open, showing the note (`notes[]`); the agent replies on the card (`replies[]`) or withdraws the card
  with `reason` (for example "Settled by your note"). Agents never write an answer.
- `via: "recommended"` marks an answer filled by "Use the recommendations" and not changed by the owner; the server
  checks the choice is exactly the card's recommendation (`invalid_via` otherwise).
- The note is stored exactly as typed (trimmed of surrounding whitespace only), at most 4,000 characters, no control
  characters other than tab and newlines.
- Stored in `<ccsDir>/status/answers.jsonl` (append, local disk, this server only), then written signed to the share
  (temporary file and rename), then the asker is rung through pm-mail. An answer goes to
  `<reports>/v1/answers/<card>.json` (a later answer to a newer version of the card replaces it); each note gets its
  own file, `<reports>/v1/answers/<card>.note-<12 hex>.json`, so no note overwrites another or an answer; `object` and
  `done` go to `<card>.json` like an answer. Each step's
  result is kept in the history; a failed share write or doorbell is retried and shown, never hidden.
- The answer file: `{ schema: "agent-reports/answer@1", cardId, cardSeq, cardSha256, kind: "answer" | "note" |
  "object" | "done", choice, note, via,
  answeredAt, answeredBy: "owner", device, signature }`. `device` is a word from the browser (`iPhone`, `iPad`,
  `Android`, `Mac`, `Windows`, `Linux`, `browser`).
- Signature: Ed25519 over the UTF-8 bytes of the canonical JSON (RFC 8785 style: object keys sorted by UTF-16 code
  unit at every level, no whitespace, strings and numbers as `JSON.stringify` writes them) of the answer with the
  top-level `signature` removed; `signature.value` is standard base64 of the 64-byte signature. The key pair lives in
  `<ccsDir>/status/answer-key.pem` (PKCS#8, 0600), created on first use; the public key (SPKI PEM) is published at
  both `<reports>/v1/keys/aac-answers.pub` and `<reports>/v1/keys/<keyId>.pub` (verifiers try `<keyId>.pub` first), with
  `keyId` = first 16 hex characters of the SHA-256 of the public key's DER (SPKI) bytes. `canonicalJson(value)` and
  `verifySignedAnswer(fileText, publicKeyPem)` in `src/web-server/status/answer-signing.ts` are what a reporter
  mirrors.
- The doorbell rings only the asker (`replyTo.pmMail`): `pm-mail --as aac-answers send <asker> "Answer for <card>" |
  "Note on <card>" | "Objection to <card>" | "Done: <card>" -m "<body>"`, only after the signed copy is on the share.
  The body carries the card id and seq, the record kind, the owner's exact choice (keys and labels) and words, the
  UTC time and the card's SHA-256, and says the signed file is the authority (naming it and the key file).
- No agent path exists: the routes take a browser session only (not a device token, not localhost without auth).

`cards.batches` is only a grouping (id, label, cardIds) that orders the deck; there is no approve-all endpoint.

## Web UI move

The owner approved moving the web UI from Slint to Leptos on 2026-11-06 (Leptos 0.9.0-rc1, client-side rendered, served by
this TypeScript server). The Status and Scoreboard pages are the first Leptos pages, shown inside the Slint shell by an
iframe until the shell itself moves. The JavaScript view-model layer (`web-dashboard/public/*.mjs`) stays.
