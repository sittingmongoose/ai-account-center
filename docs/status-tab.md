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
id)>` for a T3 thread with no pm-mail name. After creating the folders AAC publishes the JSON Schemas it ships
(`docs/agent-reports/schemas/<kind>.json`: answer, card, run, status, tasks, thread; packaged with the server) into
`v1/schemas/`, replacing a file only when it is missing or different (temporary file and rename).

**Unknowns stay null.** Thread, run and card `project` and `stream`; thread `doing` and every `agent.*` field; run
`startedAt`, `task.type` and `model.id|provider|harness|effort|account`; card `replyTo.pmMail`. A hook never guesses
them, and the view shows null as unknown.

**Runs** (`writers/<writer>/runs/<YYYY-MM-DD>/<run>.json`, 32 KB cap) carry `seq` like every other document. The
validator (`src/web-server/status/run-schema.ts`) accepts `model.account`, `process { id, version, sha256 }`,
`metrics { name: number | null }`, `imported { from, at }`, `review.by`, `review.disputed` and `cost.estimate` (`{
source, method, credits, quota, tokens: { in, out, cached } }`, kept apart from measured cost). The Status server does
not read runs; they feed the Model Scoreboard (`docs/scoreboard.md`).

**Reading rules.** For each writer the reader lists the folder (and `cards/`) first and then reads what the listing
names: a Mac rename-over through SMB is a delete then a rename, and NFS caches a missed lookup for about 20 s unless
the folder is listed. ENOENT and ESTALE (Node: "Unknown system error -116") on a listed file, or on listing a writer
folder, keep the last good copy and retry on the next scan; they never count as the thread going away. Once is the
share's rename-over race and says nothing; the same error on a listed file on the next scan too makes it a named
problem ("could not be read on two scans in a row ..."), since its first failure, so the source is not `ok`. A writer
or `cards/` folder whose own listing fails the same way twice in a row is named too ("folder could not be listed on
two scans in a row ..."), and so is one whose listing fails with any other error, at once; either way every row under
it carries that problem ("its folder could not be listed ...") and shows as its last good reading, never as current,
until the folder lists again. A pm-mail registry file that is listed but gone at the stat (pm-mail's rename-over)
keeps its row for that scan; listed and gone on the next scan too, it is a named problem. The Scoreboard's run and
catalog files, its writer, `runs/` and date folders, and the quota-watch report follow the same rule. A file absent
from its folder's listing keeps its last good copy for 2 minutes before it counts as removed. A file containing a NUL
byte is refused (Mac SMB appends wrote NULs into shared files). The fresh card read behind every answer also lists
`cards/` first. Every share file is opened non-blocking and without following a link, and the open handle must be a
regular file before a byte is read: a FIFO or a link where a report belongs is named as a problem and can never block
a read; in `cards/` it also makes `cards.complete` false. A lower `seq` than the one already read keeps the higher copy and names a problem until the writer writes a
higher one; a card in that state has `answerable: false`, because an answer to it could only come back `card_changed`.
A writer name cannot contain `--` (a card id is split on the first one); such a folder is named as a problem.

**Scan limits.** One scan reads at most 200 writer folders (over that, the most recently changed), 500 card files and
64 MB in total. A file over its own size cap is never read, so it takes nothing from the 64 MB. Anything past a limit is not read and is named in the Source (`limits/writers`, `limits/cards`,
`limits/bytes`), never dropped silently. The Scoreboard has its own limits (`docs/scoreboard.md`, Reading runs).

**Collectors.** Agent reports: rescanned every 10 s (stat every file, read only changed ones; the share has no change
notification). pm-mail registry: every 30 s. Machines (one short read-only command per host over the app-update ssh
aliases; this computer, whichever of the four it is, from `/proc` and `statfs`), `browser-check slots --json` and `simlock status --json`: once a minute
while `GET /api/status` was called in the last 15 minutes, and at once on the first read after a quiet spell (they
start processes and ssh sessions; nobody watching, nothing runs). Usage: every dashboard analytics sample, never a
provider refresh. quota-watch: its share report `<reports>/v1/quota/report.json`, read with the agent reports scan
(every 10 s, by the same reading rules; see Spend).

## URLs
- `/status` and `/scoreboard`: the Slint shell with that tab selected (page routes, like `/analytics`).
- `/app/status` and `/app/scoreboard`: the Leptos pages themselves. `?embed=1` hides the page's own header when the
  shell shows it in its iframe. Both need the same signed-in session as the dashboard.
- `GET /api/status`: the whole view below. `Cache-Control: no-store`. The service worker never touches `/api`.
- `POST /api/status/answers`: the owner's answers and notes, sent together from the deck's review screen.
- `GET /api/status/answers?limit=N`: the read-only answer history (answers and notes), newest first: `{ answers: [ {
  id, card: { id, label, seq, title, project, stream, askedBy, options: [ { key, label } ], recommendation, cardSha256
  }, answer: { kind, choice, note, via, answeredAt, device, delivery } } ] }`.
  An unreadable private journal returns `503 answers_unavailable`, "The answer history can't be read right now.".
- `GET /api/status/files/<writer>/<path>`: an image or video a card names, from that writer's own folder only (no
  `..`, no links out of the folder, checked again on the opened file before a byte is read: on Linux by what the
  descriptor really holds, so a parent folder swapped for a link after the path check is refused; without `/proc` (macOS,
  Windows) by looking at every path segment again, which leaves one race there, a folder swapped out for the open and back
  before that look). The type comes from the content, never the name: images PNG, JPEG or WebP up to
  2 MB; videos MP4 (an `ftyp` box; still-image brands such as HEIC/AVIF refused) or WebM (an EBML header naming
  `webm`) up to 50 MB. Every file answers byte ranges (`Accept-Ranges: bytes`, one `Range: bytes=` range gives
  `206 Partial Content` with `Content-Range`, a range past the end gives `416`): iOS Safari plays no video without
  them. `HEAD` gives the headers only. `Cache-Control: private, max-age=300`, `X-Content-Type-Options: nosniff`.
  Refusals: `404 not_found` (missing, outside the folder, not a writer, bad path), `413 too_large`, `415 not_media`,
  `416 range_not_satisfiable` (a JSON body, `no-store`). A writer folder that is itself a link is `404`. The file is
  checked and served from one open handle, so a swap after the check cannot change what is sent; a file that shrinks
  mid-send ends in a broken connection rather than a short file under the promised `Content-Length`. A client that takes
  no bytes for 2 minutes loses its connection and the open file with it; the browser asks again by range. A client that
  has left is never written to.
- `/ws` sends `{ "type": "status-changed" }` to browser-session sockets when a source changes (at most one every 5 s);
  the page re-reads `/api/status`. The page also
  re-reads every `refresh.pollSeconds` while visible, so a missed hint only delays.

All routes: a browser session (`authKind(req) === 'session'`; no session `401 auth_required`, a device token
`403 device_scope`), `Cache-Control: no-store`, errors `{ error, code }` with fixed sentences that never echo input.
`GET /api/status` takes no query (`400 unexpected_query`); the history takes only `limit` (1 to 500, default 50,
else `400 invalid_limit`). POSTs: no query string (`400 unexpected_query`), the dashboard Origin
(`403 origin_required`), `application/json` (`415 json_required`), at most 64 KB (`413 body_too_large`) and exactly
the documented keys (`400 invalid_body`). Before routing, a body that is not JSON gets `400 invalid_json` and one over
100 KB `413 body_too_large`, each `{ error, code }` that never echoes input. The files route sets `Cache-Control: private, max-age=300` instead of
`no-store`.

## Shell iframe

The Slint shell (`web-dashboard/ui/`, `web-dashboard/public/bridge.js`, `web-dashboard/public/shell-frame.mjs`) shows
Status and Scoreboard as tabs after Accounts (desktop header, the F/T and short NavBar, the phone tab bar) and lays
one iframe per page over the canvas, as the sign-in layer lays its HTML inputs.

- **Lifetime**: the iframe (`/app/status?embed=1`, `/app/scoreboard?embed=1`) is created on the first visit and kept
  while hidden, so returning is instant. It is visible only while its tab is current, the user is signed in, no
  shell overlay covers the content area and the page has sent `aac-ready`. Sign-out (or an ended session) removes
  both. Same origin, no `sandbox`, no `allow`. The Slint page under it draws only the paper background.
- **Geometry**: the shell sends the `content-rect` action, `"x,y,w,h"` in CSS px of the window: below the header,
  above the phone tab bar, the on-screen keyboard and the bottom safe area, inside the side safe areas. It is sent
  once at start and whenever it changes (resize, rotation, fold, safe areas, keyboard, header height). The bridge
  rounds the edges to whole pixels. `content-cover` says what the canvas draws over that area: `"all"` while a
  header menu, sheet, popover, dialog or Details is open (the iframe hides, so a click outside the overlay reaches
  the canvas and closes it), otherwise the toast stack and the hover card as `"x,y,w,h"` rects joined by `;`,
  which are cut out of the iframe with `clip-path` (they stay visible and clickable), or `""`.
- **Messages** (`window.postMessage`, target origin = the dashboard origin; the shell checks `event.origin` and
  that `event.source` is one of its own iframes, the page checks `event.origin` and that `event.source` is its
  parent, and each ignores anything else):
  - shell -> page `{ "type": "aac-theme", "mode": "light" | "dark" }`: the effective theme (Auto resolved), sent
    on the iframe's `load`, on `aac-ready` and whenever it changes. The page draws in it and ignores its own
    `prefers-color-scheme` while embedded.
  - shell -> page `{ "type": "aac-visible", "visible": true | false }`: whether the page is the current tab, sent
    after each `aac-ready` and whenever that changes. A hidden iframe's document still reports itself visible, so
    the page relies on this: while hidden it stops polling and ignores `/ws` hints (so the server's collectors can
    idle), and on becoming visible it reads at once.
  - page -> shell `{ "type": "aac-ready" }`: the page is mounted and drawing (send it once per document load).
    The shell shows the iframe and sends the theme and `aac-visible` again.
  - page -> shell `{ "type": "aac-navigate", "page": "home" | "analytics" | "accounts" | "status" | "scoreboard" }`:
    switch the shell's tab (a link from one page to another tab).
- **Unavailable**: every `load` of a frame starts it over (hidden until it says `aac-ready` again). A frame that
  fires `error`, finishes loading anywhere but `/app/status` or `/app/scoreboard` (a redirect, the shell itself),
  or sends no `aac-ready` within 20 seconds is replaced by the shell's own note
  ("Status page unavailable", Try again) in the content area; until ready the note reads "Loading Status...". A
  failed page gets a new iframe on Try again, when its tab is opened again and when the browser comes back online.
  Stale data inside a page that did load is the page's own business (Sources above).
- **Service worker**: `/app/*` is cached network-first like the shell, but offline it falls back only to its own
  cached copy, is never mirrored to the start URL and never answers another navigation as "any shell"
  (`public/sw-route.js` `isAppPage`). A shell that finds itself inside its own iframe does not start.

## GET /api/status (view v1)

```jsonc
{
  "version": 1,
  "generatedAt": "2026-11-06T16:30:00.000Z",
  "refresh": { "pollSeconds": 30 },
  "headline": {
    "tone": "good" | "warn" | "bad",
    "runs": [ { "text": "3 questions", "strong": true, "tone": "warn" }, { "text": " for you · 41 of 88 items landed" } ],
    // `openCards` counts every open card; `questions` only question, judgement and action
    // cards ("questions for you"), `decisions` the objection cards ("decisions to look at").
    // Sample cards are in `openCards` and in neither of the other two.
    "counts": { "openCards": 3, "questions": 3, "decisions": 0, "blockedItems": 2, "workingThreads": 9, "staleSources": 1 }
  },
  "sources": [ Source ],
  "cards": { "source": "agent-reports", "open": [ Card ], "answered": [ AnsweredCard ], "batches": [ Batch ],
             "complete": true },
  "pinned": PinnedPanel,
  "projects": [ Project ],
  "threads": { "source": "...", "also": [ "pm-mail" ], "rows": [ Thread ] },
  "machines": { "source": "...", "also": [ "browsers" ], "rows": [ Machine ] },
  "simlock": { "source": "...", "capacity": [ Capacity ], "leases": [ Lease ] },
  "usage": { "source": "...", "warnings": [ UsageWarning ], "accounts": [ UsageAccount ] },
  "spend": Spend
}
```

If the private answer journal cannot be read, the rest of the view still returns normally. `cards.answered` is
`null`, and `cards.answerHistoryProblem` reads "The answer history can't be read right now." (otherwise null).
Cards remain visible but cannot be answered until history recovers; `cards.complete` is false so drafts stay safe.
The page shows this problem beside the questions. A later successful read restores history automatically.

A panel's `also` names its secondary sources. The page shows each one's problem on that panel next to the main
source's, and the rows they supplied carry that source's own state (`SourceRef`: `{ id, label, state, asOf,
ageSeconds, detail }`), so a healthy main source never makes a failed secondary reading look live. A failed secondary
source keeps its last good readings, labelled cached with their age.

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

A partial read is never a clean success. A machine read whose system disk failed (local `statfs` throws, or a remote
probe answers without its disk lines) keeps CPU and memory current, keeps the earlier disk reading and its low-disk
warning, and marks the machine `stale` with "Disk: The system disk could not be read (since 10:01 AM). Showing the
disk reading from 10:00 AM." (or "No earlier disk reading."), so Machines is not `ok` until a disk read works again.
A remote answer without its memory lines is handled the same way ("Memory: The machine did not report its memory
..."), keeping the earlier memory reading and its low-memory warning, and so is the CPU busy figure ("CPU: The machine
did not report its CPU ..." when a probe has no CPU line; locally on Linux "CPU: The CPU counters could not be read"
when `/proc/stat` fails). The Mac probe prints `cpuPct` only when `ps` succeeds, and a Mac answer without its core
count has no busy figure (the summed `ps` cannot be divided); Windows prints one `cpuLoad=` per processor and any
processor without a `LoadPercentage` makes the figure unknown (PowerShell's `Measure-Object` would count it as 0), so a
failed command is never a measured 0% or 100%.
When the folder listing or the command worked but named parts of it failed,
the source is not `ok` and its `detail` names each part with its reason and since when, then "The rest read <time>.":
a pm-mail registry file that can't be read, isn't JSON, is over 64 KB or isn't a regular file makes the registry
`unreadable` ("1 registry file unreadable since 10:01 AM: registry-worker.json (could not be read). ..."); a host
`browser-check slots --json` answers with `state: { error }` (exit 0) makes `browsers` `error` ("Nas1: session list
failed: ... (since 10:01 AM)."); a host browser-check did read but flags makes `browsers` `error` and names that
host, with its counts current: `display_problem` ("browsers cannot start here: ..."), `snapshot` (its own words, such
as "backend busy (...): counted from its session records"; it carries no time, so the reading time stays this read's),
and `phones: null` on the VM, Mac or Nas1, which run a Simlock worker ("reported no phone count ..."; phones are
unknown, never zero; Windows has no worker, so its null is normal); a Simlock gateway worker that is `disconnected` or `incompatible` makes `simlock`
`error`. Each failed part is tried again on the next read and the problem clears on its first good one. A Simlock that
answers `daemon.health: "starting"` (no devices, leases or capacity yet) or `"failed"` is a failed read, so its last
good reading stays, shown cached, never an empty fleet.

**`cards.complete`**: whether a card missing from this view is known to be gone. The page prunes the owner's local
drafts of closed cards only while it is true. True only when the latest scan listed `v1/writers` and every writer and
`cards/` folder, read every file it tried and reached no scan limit. A file refused for its content (bad JSON, NUL
bytes, a secret, a schema miss) was read: it is a named problem, and leaves this true while the card's last good copy
is still listed; a refused card file with no good copy (after a restart, say) makes it false, since that card may be
open but isn't listed. False before the first full scan
after start; while the Source is `missing`, `error` or `stale`; when any listing failed (EIO, EACCES, ETIMEDOUT, ESTALE
and the like, including on one writer's folder); when a file could not be read (a Source of `unreadable` for that
reason); and when a scan limit was reached.

**Card** (open): the card file's fields after validation (`id`, `label`, `writer`, `seq`, `project`, `stream`,
`askedBy`, `kind`, `title`, `now`, `nowAsOf`, `oldAnswer`, `whatChanged`, `options[]`, `recommendation`, `multiple`,
`urgency`, `blocks`, `sections[]`, `batch`, `evidence[]`, `replies[]`, `neededBy`, `exception`, `reversible`,
`askedAt`) plus `ageSeconds`, `askerState` (the asking thread's state,
`"gone"` when its thread is stale, `"unknown"` when the writer has no `thread.json`), `cardSha256` (SHA-256 of the card
file's bytes), `notes[]` (the owner's notes and objections already sent on this card, oldest first, each `{ kind:
"note" | "object", note, at, delivery }`),
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
  URL opens in a new tab; a share path is shown to copy; anything else is refused), `media { items: [ Media ] }`,
  `quote { items: [ { text, where, context: [], source, link: { label, url } | null, version } ] }` (verbatim words
  copied by code, never reworded; `where` is the heading path, `context` nearby status notes such as "Superseded
  2026-10-09") and `planChange { doc, unit, now, proposed }`. Order on a phone: title, now, options, recommendation,
  then the folded sections.
  Known types are validated strictly (a bad one makes the card file unreadable, by name); a type this server does not
  know arrives as `{ type, title, fold, unknown: true }` and the page says it has a section it cannot show yet. The
  card's 64 KB cap includes its sections.
- `replies[]` `{ at, text }`: the agent's replies to the owner's notes, in the card file (a new `seq`).
- `kind`: `question` (the default; two to four options, answered in the deck), `objection` (a call the agent already
  made, "tell me if you object": shown in a quiet list with the decision, the reason and an Object button; no options
  or two to four; silence is not an answer, nothing is signed unless the owner objects, and the card stays listed
  until the agent replies or withdraws it), `action` (something only the owner can do, such as signing in or changing
  a NAS setting: a Done button, no options), `judgement` (a measurement card: two to four options, "Can't tell" is a
  valid one; a judgement card that carries a `recommendation` is shown with `problem` saying so and `answerable:
  false`, so the owner is never anchored) or `sample` (a format sample: shown with "no answer wanted",
  `answerable: false`, never sent; no options or two to four).
- Passed through for the page: `verdict` (a badge such as adopt / adapt / keep ours / reject; the deck can filter on
  it), `group` (a group within a batch, with its own "use the recommendations for this group"), `agreement { same:
  true | false | null, note }` (did the judges agree), `doubt`, `appliesTo { count | null, description }` (a rule
  card that settles a whole class), `changesCanon [ { ref, what } ]`, `related [ card ids ]` and option `stance`
  (`keep` = the plan as written, `borrow` = take the outside approach, `own` = our own fix, or null).
- `neededBy` `{ at: ISO | null, ifNoAnswer }` | null: when an answer is needed and what happens or starts costing
  without one. `exception` `{ rule, text }` | null: the owner's rule this card loosens and how; shown before anything
  else. `reversible` `{ yes: true | false, how }` | null.
- A withdrawn card may carry `reason` ("Settled by your note"). There is no `allowNote`: the note box is always there.
- A count or premise that changes after a card was shown: the agent supersedes the card; the new card's `whatChanged`
  gives old and new values and `oldAnswer` quotes any earlier answer.

**AnsweredCard**: `{ card: Card, answer: { kind: "answer" | "done", choice, note, via, answeredAt, device, delivery: { state:
"rang" | "failed" | "pending", at, detail, share: Step, doorbell: Step, summary: Step | null, summaryWarning } } }`,
newest first, at most 50 (the full
history, notes included, is `GET /api/status/answers`). **Step**: `{ state: "pending" | "done" | "failed" |
"skipped", at, detail, attempts }`. `failed` covers a share write or doorbell still being retried, one that gave up
after 20 attempts, a doorbell turned off on a test server (`skipped`), or an answer a newer answer to that card replaced
before its signed copy reached the share (both steps `skipped`, `state: "failed"`, never written or rung, so the older
answer cannot land over the newer one); `detail` says which. A card with no
pm-mail name is delivered once its signed file is on the share: `state: "rang"` with `detail` "No mail name; the agent
reads its answers file." (doorbell step `skipped`). While a share write or ring is retried, `detail` gives its attempt
("attempt N of 20"); once it gives up it says "no more retries".
`summary` is the step of the record's send summary (null for a record from before sends), shown beside `state`
without changing it: `summaryWarning` is null once it is written, else says it is not on the share yet, is being
retried ("attempt N of 20"), or gave up after 20 attempts. The same `delivery` is on each `notes` entry and in
`GET /api/status/answers`; the page shows a note's or objection's own failed `detail` on its open card, beside any
`summaryWarning`, since those records never appear in `answered`.

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
source: "report" | "pm-mail", lastSeen, ageSeconds, stale, clockAheadSeconds, problem, registrySource }`. Threads quiet for more than
a day are left out unless they report `blocked`. `lastSeen` is the older of the writer's `writtenAt` and when AAC first
saw that version, so a writer whose clock runs ahead cannot look fresh; `clockAheadSeconds` says by how much when over
2 minutes. A report is `stale` after its `ttlSeconds` or while its file is unreadable. `ttlSeconds` may be null for any
state (a thread that ended its turn cannot heartbeat, whatever state it ended in): such a thread
shows its age and is never stale for lack of a heartbeat. pm-mail rows (registry threads
with no report, seen in the last day) have `state: null`, `doing` = their about line, and are stale after 30 minutes.
Their `registrySource` is the pm-mail registry's `SourceRef` (null on report rows): while the registry fails, its rows
are the last good scan, shown as cached with the registry's problem. While only some registry files fail, each row
carries its own file's state: a row whose file fails keeps its last good reading with `registrySource.state:
"unreadable"`, `asOf` = when that file last read well, `stale: true` and `problem` naming the file ("registry-worker.json:
could not be read (since 10:01 AM). Showing its reading from 10:00 AM."); the other rows are `ok` at this scan. A file
with no good reading yet has no row; the registry's `detail` names it.
`blockedOn.kind` is any short slug (documented: `owner`, `agent`, `review`, `lock`, `resource`, `quota`).

**Machine**: `{ id: "vm" | "mac" | "windows" | "nas1", label, state: "ok" | "stale" | "unreachable", asOf, ageSeconds,
detail, cpu: { cores, load1, busyPct }, memory: { availableGb, totalGb }, disk: [ { mount, freeGb, totalGb } ],
browsers: { running, max }, recordings: { running, max }, gpuPct, sessions: [ { id, url, owner, recording,
ageSeconds } ], slotsSource, warnings: [ "Disk 17.7 GB free, under the 50 GB floor" ] }`. Floors: 50 GB disk, 6 GB memory (the
shared machine rules). GB here is GiB (as `free -g` and `df -h`). `unreachable` keeps the last good reading with its
time in `detail`. Windows has no load average (`load1: null`). Session URLs are host and path only (a file URL shows its
path); query strings and fragments are never shown. `browsers`, `recordings`, `gpuPct` and `sessions` come from
`browser-check` (null or empty when it has no row for that machine); `slotsSource` is that source's `SourceRef`, apart
from the machine's own `state`, so a failed `browser-check` shows its cached readings with their age and problem while
CPU, memory and disk stay current. While `browser-check` answers, each machine's `slotsSource` is that host's own:
`ok` with its read time; `error` when its host failed, keeping the last good counts with `asOf` = their read time and
the host's reason ("Nas1: ... (since 10:01 AM). Showing its reading from 10:00 AM."), or with unknown (null) counts and
"No reading yet." when it never answered; `missing` (`asOf: null`) when `browser-check` has no row for it.

**Capacity** (Simlock): `{ platform, running, max, reserved, overLimit }` (one row per platform Simlock lists,
including `global`). **Lease**: `{ device, deviceLabel, platform, holder, since, expiresAt, lastKnown }`
(`lastKnown`: the lease is on a gateway worker that is not connected, so it is the last known, not a current lease). Device ids and holders
are shown as Simlock reports them (`holder` is the worker label, else the requester id); lease ids, owner ids and tokens
never.

**UsageAccount**: `{ id, provider, label, host, active, window, kind, usedPct, remaining, limit, unit, resetsAt,
projectedFullAt, runsOutBeforeReset, atLimit, stale, resetPassed }`, one row per rate-limit window and per balance (Codex credits:
`kind: "balance"` with `remaining`/`unit`; the page shows a balance as its amount left with that unit, "12 credits
left", or "12 of 50 credits left" only when `limit` is given, and "-" while `remaining` is null; it never turns a
balance into a percent), from AAC's own dashboard samples and the same pace math as
`aac-usage` (no extra provider refresh). Accounts hidden on the dashboard are left out. Every view rechecks each
reading against its reset with the dashboard's rule (the reset is due and the reading predates it, or its time is
unknown), not only when the sample was taken: such a row has `resetPassed: true` and `stale: true`, keeps its measured
`usedPct` as history (never an assumed 0), and has no `atLimit`, projection or warning. **UsageWarning**: `{ tone,
text }`, worst first, only from current readings: `bad` "claude main (7-day): at its limit until its Nov 12 4:00 PM
reset", `warn` "claude main (7-day): full in 1d 11h, before its Nov 12 reset".

**Pinned** (`pinned`): the threads pinned in T3, each as the status update its owner would get by asking
"status?" (PINNED-THREADS.md section 1). Composed in the `aac-status` native module from every machine's T3 pins
(the VM's database read locally, the Mac's, Windows' and Nas1's fetched with one short read-only `sqlite3 -json`
command over the dashboard's ssh aliases) plus the agent reports above and the `agent-reports/status@1` documents
(`writers/<writer>/status.json`, read with the agent-reports rules). TypeScript checks status reports for secrets
(`jsonFromBytes`, the same check as every other agent report); the native module only reads reports TypeScript
accepted — one secret filter, the TypeScript one. The TypeScript side passes the module's answer
through. A machine that does not answer keeps its last good pins for one scan, then the panel names it in
`machineProblems` while the other machines' pins still show. Every word is the module's; a missing piece is said in
words or left null for the page to word ("No task lists yet." when there is no task list;
"Can't tell." when nothing proves looping either way), never invented.
```jsonc
{ "asOfMs": 1762608600000, "head": "5 pinned in T3 · 1 looping · 2 waiting on you",
  "counts": { "pinned": 5, "looping": 1, "waitingOnYou": 2 },
  "machineProblems": [ { "machine": "mac", "label": "the Mac", "text": "Pinned threads unavailable on the Mac: it did not answer." } ],
  "updates": [ { "id": "<t3 thread id>", "title": "Build PMConcept7 and Background Activity", "machine": "mac",
                 "who": "Puppet Master · pm7-build · Opus 5.5 xhigh" | null,
                 "reportState": "fresh" | "stale" | "unreadable" | "none",
                 "percent": { "text": "58%", "done": 14, "of": 24 } | { "text": "70%", "done": null, "of": null } | null,
                 "percentNote": "About 70%, by its own estimate." | "Its task list says 14 of 24 (58%); the thread estimates 70%." | null,
                 "now": "Windows 10 sweep for stream F, batch 9." | null,
                 "blocked": { "text": "Waiting on you: pick the reviewers for B09.", "waitingOnYou": true } | null,
                 "looping": { "answer": "yes" | "no" | "cantTell", "text": "Yes. F-07 has gone back for fixes 5 times without a passing review.", "task": "F-07" | null, "rounds": 5 },
                 "left": [ { "id": "F-07", "title": "Tray icon sharp at 125% scale" } ] | null,
                 "done": [ { "id": "F-08", "title": "Sticker block landed" } ] | null,
                 "updated": { "text": "Updated 6 min ago", "at": "ISO", "ageSeconds": 360 } | null,
                 "reportNote": "This thread doesn't report its status yet." | null,
                 "staleNote": "No status update for 25 days. If it is finished, unpin it in T3." | null,
                 "unreadableNote": "Could not be read since 1:12 PM." | null,
                 "summary": "One or two sentences." | null } ] }
```
Without the native module (or before the first read) the field is `{ "unavailable": "the aac-status native module
is disabled" }` style, with the reason. `status@1` documents are optional: a pinned thread without one still gets
its update from `thread@1` and `tasks.json`.

**Spend** (source `quota-watch`): quota-watch's per-stream estimates of how much of each account meter a stream used,
from `<reports>/v1/quota/report.json` (`quota-report@1`, one writer, rewritten whole every 10 minutes, 512 KB cap;
field table: quota-watch README, "Share copy"). It is read with the agent-reports rules: the `quota/` folder is listed
before the file is read; ENOENT and ESTALE keep the last good copy and retry (on two scans in a row, a named
problem); a file gone from the listing keeps its
last good copy for 2 minutes; NUL bytes, over 512 KB, a secret, a schema other than `quota-report@1`, `estimate` not
`true` or a field that does not validate make it unreadable by name (the last good copy stays, marked); a lower
`seq` than one already accepted is ignored.
```jsonc
{ "source": "quota-watch", "estimate": true, "asOf", "generatedAt", "writer", "methodId", "method",
  "periods": [ { "id": "day" | "week" | "window", "label": "Today" | "This week" | "Last 6 hours", "period", "start",
                 "end", "coverage", "coverageText": "covers 9% of the week" | null,
                 "streams": [ { "stream", "label", "bucket", "rows": [ { "account", "meter", "meterLabel", "unit",
                                "value", "valueText", "estimate" } ] } ] } ],
  "budgets": [ { "stream", "streamLabel", "desc", "accounts", "unit", "meter", "meterLabel", "per", "limit", "use",
                 "known", "percent", "period", "coverage", "coverageText", "overLimit", "estimate", "text" } ],
  "unattributed": [ { "account", "meter", "meterLabel", "unit", "amount", "amountText", "reason", "why", "estimate" } ],
  "machinesNotScanned": [ { "name", "thisPass", "error", "runsMissed", "runsInWindow" } ],
  "overLimitBudgets": 0 }
```
- Every number here is an estimate (`estimate: true` on the section and every row; a row the report marks measured,
  such as unattributed `(no session seen)`, says `estimate: false`). `method` is the report's method in words.
- `unit` is `credits` or `percent` (percentage points of the meter's window). Credits and points are never added
  together and the view makes no total across units or accounts. `value` is rounded (credits to 0.01, points to 0.1)
  and `valueText` words it: "≈ 17.77 credits", "≈ 4.7 points of the 7-day window", "≈ under 0.01 credits". An unknown
  stays null and reads "not read", never 0.
- `label`: the stream's label from the agent reports when exactly one tasks stream has that id (two different labels:
  the id); buckets such as "(no stream)" keep their words (`bucket: true`, listed last).
- `coverageText` is set when coverage is under 0.5 ("covers 9% of the week") or unknown ("coverage of the week not
  known"); otherwise null.
- Budgets pass `limit`, `use` and `percent` through as the report gives them (null stays null); `known: false` means
  `use` is a lower bound ("at least ≈ …"). `overLimit` is true when `percent` ≥ 100, or `use` ≥ `limit` with both
  known. `text` words the row ("≈ 18.28 credits today; no limit set.", "at least ≈ 5.3 of 20.0 points of the
  7-day window this week (27%).").
- The headline adds "N budget(s) at or over its limit (estimate)" (tone `bad`) only while the source is `ok`.
- Source states: `missing` (no `quota/` folder or no report, or before the first scan), `ok` (with its age), `stale`
  (no new report for 30 minutes, three missed passes; the age is the older of `writtenAt` and when AAC first saw that
  version), `unreadable` (the reason, and the time of the last good copy if any), `error` (the folder could not be
  listed). Without a report the section is empty (`periods: []`).

**Spend by stream** (`spendByStream`, a sibling of `pinned`): the same estimates as a per-stream table, every word
decided in `native/aac-status/src/spend.rs` over the `spend` view above plus `nowMs`, the dashboard's UTC offset and
the usage snapshot's account order (CONTRACT-spend-bymodel.md). One period per accepted report period: columns are the
account meters the streams use (Claude weekly, Claude 5-hour, other providers), rows are streams by their largest
share of a weekly limit with the bucket row ("Not tied to a stream") last, and `lead`/`summary`/`foot` word it. Cells
are shares of one account's limit (`"42%"`, `"3.2×"` for full 5-hour windows, `"not read"` with a null share for an
unread one, `"≥…"` where a merged bucket part was unread); credits stay per account. When there is no report, no
native module or an input the module refuses, the field is the contract's fixed unavailable shape
(`state: "unavailable"`, one sentence, `periods: []`), never a guessed figure.

## Answers

The owner answers in a deck: one card at a time with Back and Next, a row of card labels to jump, and nothing is sent
until the review screen at the end ("Send N answers"). "Send current selections" jumps to that review screen early,
for a long deck. "Use the recommendations" fills every plain question still empty with its recommendation (never a
judgement, objection or action card); the owner can change any of them before sending. Unsent choices and notes are
kept on this device (browser storage) so leaving the page loses nothing; they are drafts, never answers.

`POST /api/status/answers` body: `{ "sendId"?: "<id>", "answers": [ { "entryId"?: "<32 hex>", "cardId", "cardSeq", "cardSha256", "choice": ["A"] | [], "note": "...",
"via": "deck" | "recommended", "kind"?: "answer" | "note" | "object" | "done" } ] }`, 1 to 50 entries, at most 64 KB. Each entry is validated and recorded on its
own, in order, one set at a time; the response is `{ "results": [ { cardId, outcome: "recorded" | "card_changed" |
"refused", code, error } ] }`, one per entry in request order (HTTP 200 when the request itself is valid, even if some
entries are refused; `code` and `error` are null for `recorded`; `cardId` is null when the entry did not name a valid
card id). Entry codes: `card_changed`, `invalid_entry` (not the strict entry shape, or an `entryId` already used with other content), `invalid_choice`, `empty_entry`
(no choice and no note), `invalid_note`, `note_secret` (the note looks like it holds a key, token, password or email
address, including one typed as prose such as `password: ...` with a digit in it: it would be written to the share,
so it is refused; the refusal names the kind, `It looks like an API key.`, and quotes none of it), `invalid_via`, `invalid_kind` (the entry's kind does not fit the card's
kind), `words_required` (an objection without the owner's words), `sample_card` (a format sample takes no answer),
`judgement_anchored` (a judgement card that carries a recommendation), `answer_not_saved` (the private store could
not be written; nothing else happened for that entry), `answer_save_unclear` (the store refused the record and could not
cancel it either: this process never delivers it, but a restart before the cancel lands may deliver it once; wait
before sending it again), `store_busy` (another dashboard process on this computer holds
the private store; nothing was written or sent), `share_unavailable` (the share failed or did not answer in time, so the
card could not be checked; nothing was recorded for that entry, and it can be sent again). `card_changed` means the share
answered: the card is not there, or is not that version. If the send cannot finish at
all the answer is `500 answers_failed`: some entries may have been recorded.
- **Retry key:** the optional request `sendId` (16 to 64 characters of `[A-Za-z0-9_-]`, else `400 invalid_body`) is the
  client's retry key. The page sends a content hash of the entries, so a retry of the same entries carries the same id,
  even after a reload, and any edit makes a new one. A repeat with the same entries answers with the first send's
  per-entry results and records, writes and rings nothing; one that arrives while the first is still running waits its
  turn and answers the same way. The same id with different entries (draft ids included) is a new send. The last 200 ids
  are kept for 24 hours, in the private store, so this survives a restart. Each answer line carries the retry key too
  (the id, the entries hash, its place in the request and the results of the entries before it since the send's previous
  record), so a send whose own `send` line failed to append is still recognised after a restart: the known results
  stand and only entries after its last stored record, none of which was stored, are judged again. A send that recorded nothing is not remembered,
  since a retry may yet succeed. It is only a dedupe key: it is never the summary's `sendId` below, which the server makes
  and signs, and it never reaches the share or the mail.
- **Draft key:** the optional entry `entryId` (32 lowercase hex; anything else, or the same id twice in one body, is
  `400 invalid_body`) is the real guard against recording twice. The page makes it from a random per-draft nonce plus the
  entry's content, so a retry of one draft carries the same id, even alone after the rest of its send closed, and a new
  draft with identical words gets a new one. For `note` and `object` the page's id leaves out the card's version (`cardSeq`
  and `cardSha256`); for `answer` and `done` it includes it. An entry whose id is already recorded answers `recorded` as
  it did the first time and is not recorded, signed, written or rung again, nor counted in the new send's ring or summary.
  The same id with other content is refused `invalid_entry`. The server binds the same content as the page: card, choice,
  note, via and kind, plus the card's version for `answer` and `done` (without `kind`, a choice makes an `answer` and no
  choice a `note`). So an agent's reply on a card between a lost response and the retry does not record a note or an
  objection again, and an answer's id replayed on a reopened card is refused, not reported as recorded while the new
  version stays unanswered.
  Ids are kept with their records in the private store: all from the last 24 hours and the newest 1,000 whatever their
  age. An entry that was refused is not remembered and may be sent again. The id never reaches the share or the mail.
- **Time limits:** one card read has 5 s, one signed file 10 s, one ring 40 s, one whole send 30 s and one retry pass 45 s,
  so a hung share cannot hold the answers queue. A record already stored when time runs out stays pending and the retry
  writes it. A write that outlives its deadline is dropped before its rename.
- Record kinds: `answer` (a choice, `question` and `judgement` cards), `note` (a note alone, any answerable card),
  `object` (`objection` cards: no choice, the owner's words required) and `done` (`action` cards: no choice, words
  optional). Without `kind`, an entry with a choice is an `answer` and one with only a note is a `note`; `object` and
  `done` must be sent as `kind`. `answer` and `done` close the card version; `note` and `object` leave it open (the
  agent replies on the card or withdraws it), and both show on the open card in `notes[]`.
- The card must not be older than a `seq` the reader has already read or any record the store holds for that card (an
  older copy can land on the share and read fresh; it is `card_changed`, and the card shows `answerable: false`, also
  after a restart). Publishing checks again: `<card>.json` is never renamed over a file with this server's signature and
  a higher `cardSeq` (that record then reads as superseded). Only this server's own closing answer to the same card
  counts: one signed by another key, a signed file copied from another card, a note, or a file the server can't read
  does not, so a file dropped on the share cannot block a real answer.
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
- Stored in `<ccsDir>/status/answers.jsonl` (append-only, local disk, this server only. Each line carries its own id;
  each record starts on a line of its own and is written in one piece or refused; a record that fails to be written
  whole is cancelled by a `void` line naming it, never cut, so no writer can remove another's record. A record counts
  once its fsync succeeds: a later close error is logged and changes nothing, so a durable record is never answered
  `answer_not_saved` and its draft id still guards a retry. If the disk refuses that `void` line too, the entry is
  answered `answer_save_unclear` instead: the process holds the record as cancelled, never lists, shares or rings it,
  and writes the `void` line again on each gain, retry pass and send and on close and exit until it lands. Limit: if
  the process ends before any of those `void` lines lands (killed, or the disk still refusing at exit), a restart
  counts the record and may share and ring it once; if that send's line was lost too, its summary is rebuilt with that record
  and re-signed with other contents (the one exception to the stable summary below). The process that records answers holds `<ccsDir>/status/answers.lock` while it runs, refreshing it
  each minute. Only ENOENT means an empty journal; other journal read errors refuse store-dependent decisions
  (answers return `answer_not_saved`) and mark history unavailable even after a warm cache; a later good authoritative read recovers. A second dashboard on the same folder is refused with `store_busy`, writing nothing. A lock is taken
  over when its holder on this computer has ended, or on Linux was written in an earlier boot or names a pid since
  reused, or, written by another
  host such as a replaced container, when no one has refreshed it for 10 minutes; nothing is judged by the clock for
  a holder on this computer. Deleting the file frees it at once), then written signed to the share
  (temporary file and rename), then a signed send summary, then the askers are rung through pm-mail. An answer or a
  done goes to `<reports>/v1/answers/<card>.json` (a later answer to a newer version of the card replaces it); each
  note gets its own file `<card>.note-<12 hex>.json` and each objection `<card>.object-<12 hex>.json`, so none
  overwrites another or an answer. Each step's result is kept in the history; a failed share write, summary or ring is
  retried every minute (up to 20 times) and shown, never hidden. A retry never writes an answer or a done once a newer
  one for that card exists. Each share attempt checks its writer ownership generation after asynchronous work
  and synchronously immediately before rename. Losing or regaining the lock drops that attempt; reacquisition
  reloads and reconciles history before a later retry decides what still needs delivery. A failed reload keeps the held records and sends attached to in-flight deliveries, leaves ownership unconfirmed,
  and retries reconciliation by id on the next gain. Held objects are kept separately from readable history:
  list and history refuse after a read error until a good authoritative read succeeds. A second dashboard
  may recover by reading without ownership; it still refuses every store-dependent decision with `store_busy`.
  It reads the journal again whenever its size, mtime or inode changed (on each history read and retry pass), so
  its history follows the writer's. Each read replaces that read-only history; it never holds what it read. Only
  this process's own delivery objects stay attached (records it wrote, and records or sends whose share write, ring,
  summary or completion it started) and are merged back by id on a gain. A gain never puts back a record this
  process did not write, or one the fresh read shows voided: a record cancelled by a failed fsync stays cancelled
  even if another dashboard read it in the moment before its void line, so it is never shared or rung.
  Successful delivery effects append their completion lines immediately under the writer lock, even if a
  journal read failed: appending a true, idempotent done fact needs no read. Only a failed append leaves pending
  persistence work. It is retried before ownership reconciliation, on retry, on server close and on process exit
  (including the CLI's existing SIGTERM/SIGINT cleanup), while the lock can be held. No new signal handler changes
  termination behavior. A completion persisted before a lock handoff prevents the next owner repeating it.
  An external lock deletion while a completion is still pending (until the next retry pass, each minute, server
  close or exit writes it; a history read does not) cannot be made safe by this process; pending facts remain
  attached and are retried when it can own the store again.
  **Delivery is at least once.** A crash or kill between a ring and its done line can ring again after restart;
  receivers dedupe because the ring is only a doorbell. Every ring points at signed files whose names and
  signatures (`signature.keyId` + `signature.value`) are stable across retries (except in the void-failure limit above), so reading them
  twice changes nothing. There is one answer file per card; each note or objection has its own `<card>.note-<12 hex>.json`
  or `<card>.object-<12 hex>.json` file. Receivers verify the pinned signing key before accepting the file.
  Persistence repair never rings again. Durable done steps
  stay done, and persistence retries continue even after the delivery attempt limit. Nothing is written when `<reports>/v1` is not
  there: only folders below it are created, so an unmounted share grows no local folder, and the step reads
  "The reports share is not available."
- **Send summary:** every POST that records at least one entry is a send with its own server-made `sendId` (a UUID, not
  the request's retry key; if the server stopped before the send was finished, the retry after a restart rebuilds the
  summary from the stored records; a ring can go out before its summary is written, and the file then follows on a
  retry, up to the retry limit), written
  signed to `<reports>/v1/answers/batches/<sendId>.json`: `{ schema: "agent-reports/send@1", sendId, at, answeredBy:
  "owner", device, entries: [ { cardId, seq, cardSha256, kind, choice, note } ], signature }`, entries in the order
  sent, signed exactly like an answer (`verifySignedDocument(fileText, publicKeyPem, "agent-reports/send@1")`).
  Its `at` is the first record's signed `answeredAt`, the same in the send line, the first summary and a summary
  rebuilt without the send line, so a retry after a lost send line and a restart writes the same bytes (except in the void-failure limit above). A
  summary already on the share that this key signed with the same content but another `at` (from an earlier
  version, which stamped the send's end) is kept, not re-signed. A file there is replaced only once it is known not
  to be that summary: read and found invalid, under another key or with other content, a link or other non-folder entry, or absent
  (ENOENT and a fresh listing of `batches/` without it). Any other read error, or an ENOENT for a file the folder
  still lists, fails that attempt, to be retried, so a read fault never re-signs it. A folder at that path can't be
  replaced (the rename onto it fails): the step fails each pass and stops after 20 tries, shown as such. A record stored before kinds existed is shared
  exactly as it was signed, without `kind`.
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
  mirrors. **A reporter must pin the key id it trusts.** `signature.keyId` comes from the file being checked, so anyone
  able to write to the share could add `keys/<theirs>.pub` and sign with it: looking the key up by that id alone proves
  nothing. Pin the id (or the key) out of band, and treat a new one as a rotation to confirm, not as proof.
- **Doorbell:** one ring per asker (`replyTo.pmMail`) per send, not one per card: `pm-mail --as aac-answers send
  <asker> "<N> answer(s) from the owner (<batch, ...>)"` with the body on stdin (a send of 50 notes is longer than
  one command-line argument may be). It rings only after that asker's signed files are on the share. The body names
  the batch if any, the count and the card ids, then for each card its seq, record kind, the owner's exact choice (keys
  and labels) and words, the UTC time, the card's SHA-256 and its signed file, and says the signed files are the
  authority (naming the send summary and the key file). A card without a pm-mail name is recorded and signed normally
  and not rung. Card text in the ring stays on its one line (newlines and control characters collapse to a space); the
  owner's words stay exact. `replyTo.pmMail` follows pm-mail's own name rule (`^[a-z0-9][a-z0-9_-]{0,63}$`) and is never
  `all`: a card that breaks it is unreadable, by name.
- No agent path exists: the routes take a browser session only (not a device token, not localhost without auth).

`cards.batches` is only a grouping (id, label, cardIds) that orders the deck; there is no approve-all endpoint.

## Web UI move

The owner approved moving the web UI from Slint to Leptos on 2026-11-06 (Leptos 0.9.0-rc1, client-side rendered, served by
this TypeScript server). The Status and Scoreboard pages are the first Leptos pages, shown inside the Slint shell by an
iframe until the shell itself moves. The JavaScript view-model layer (`web-dashboard/public/*.mjs`) stays.
