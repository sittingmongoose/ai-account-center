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

## URLs
- `/status` and `/scoreboard`: the Slint shell with that tab selected (page routes, like `/analytics`).
- `/app/status` and `/app/scoreboard`: the Leptos pages themselves. `?embed=1` hides the page's own header when the
  shell shows it in its iframe. Both need the same signed-in session as the dashboard.
- `GET /api/status`: the whole view below. `Cache-Control: no-store`. The service worker never touches `/api`.
- `POST /api/status/answers`: the owner's answers and notes, sent together from the deck's review screen.
- `GET /api/status/answers?limit=N`: the read-only answer history, newest first.
- `GET /api/status/files/<writer>/<path>`: an image a card names, from that writer's own folder only (no `..`, no
  links out of the folder, PNG/JPEG/WebP by content, at most 2 MB, `Cache-Control: private, max-age=300`).
- `/ws` sends `{ "type": "status-changed" }` when a source changes; the page re-reads `/api/status`. The page also
  re-reads every `refresh.pollSeconds` while visible, so a missed hint only delays.

All routes: a browser session (`authKind(req) === 'session'`), no query string on POSTs, the dashboard Origin,
`application/json`, a strict body of at most 16 KB, errors `{ error, code }` with fixed sentences that never echo input.

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

**Card** (open): the card file's fields after validation (`id`, `label`, `seq`, `project`, `stream`, `askedBy`,
`title`, `now`, `nowAsOf`, `oldAnswer`, `whatChanged`, `options[]`, `recommendation`, `multiple`, `urgency`, `blocks`,
`batch`, `evidence[]`, `replies[]`, `askedAt`) plus `ageSeconds`, `askerState` (the asking thread's state, or
`"gone"` when its thread is stale), `cardSha256` and `notes[]` (the owner's notes already sent on this card, each
`{ note, at, delivery }`). Sorted by urgency, then age. Card fields (from the earlier card decks, research in
`CARDS-RESEARCH.md` on the share):
- `label`: a short id the owner can quote anywhere ("F-Q3"); never reused or relabelled.
- `title`: the question itself. `now`: the situation today in plain words, no internal ids in its first sentence,
  with `nowAsOf` (when the counts in it were taken).
- `oldAnswer` `{ text, at }` and `whatChanged`: when a question comes back because its premise moved, the earlier
  answer quoted and what changed. Otherwise both null: an answer that still holds is never asked again.
- `options[]`: `{ key: "A", label, gets, costs, image: { url, kind: "built" | "mockup" } | null }`, two or three
  options. `gets`: what the owner sees or gets; `costs`: what it costs or changes elsewhere. `image.url` is
  `/api/status/files/<writer>/<path>` for an image the agent put in its own folder (PNG, JPEG or WebP, at most 2 MB),
  shown under the option.
- `recommendation` `{ key, why }`: marked on its option and again with the one-line reason.
- `blocks`: what waits on the answer, or null for "nothing waits on this".
- `replies[]` `{ at, text }`: the agent's replies to the owner's notes, in the card file (a new `seq`).

**AnsweredCard**: `{ card: Card, answer: { kind: "answer", choice, note, via, answeredAt, delivery: { state: "rang" |
"failed" | "pending", at, detail } } }`, newest first, at most 50 (the full history is `GET /api/status/answers`).

**Batch**: `{ id, label, cardIds: [...] }`: open cards that share `batch`, kept together in the deck.

**Project**: `{ id, label, streams: [ Stream ], counts }`. **Stream**: `{ id, label, owner, goal, board: { path,
ageSeconds } | null, source: "tasks" | "board-only", asOf, stale, counts: { total, landed, done, ready, review,
fixing, working, blocked, todo, dropped }, completion: { done: N, of: M }, items: [ Item ] }`. **Item**: the
`tasks.json` item plus `ageSeconds`. A stream that has only a board (no `tasks.json` yet) shows its board link and
age, and `completion: null`.

**Thread**: `{ name, machine, project, stream, state, doing, blockedOn, progress, model, effort, account, source:
"report" | "pm-mail", lastSeen, ageSeconds, stale }`. Threads quiet for more than a day are left out unless they
report `blocked`.

**Machine**: `{ id: "vm" | "mac" | "windows" | "nas1", label, state: "ok" | "stale" | "unreachable", asOf, cpu: {
cores, load1, busyPct }, memory: { availableGb, totalGb }, disk: [ { mount, freeGb, totalGb } ], browsers: { running,
max }, recordings: { running, max }, gpuPct, warnings: [ "Disk 17.7 GB free, under the 50 GB floor" ] }`. Floors:
50 GB disk, 6 GB memory (the shared machine rules).

**Capacity** (Simlock): `{ platform, running, max, reserved, overLimit }`. **Lease**: `{ device, platform, holder,
since, expiresAt }`. Device ids and holders are shown as Simlock reports them; tokens never.

**UsageAccount**: `{ provider, label, window, usedPct, resetsAt, projectedFullAt, runsOutBeforeReset, atLimit,
stale }` from AAC's own dashboard data (no extra provider refresh). **UsageWarning**: `{ tone, text }`, such as
"claude jmtashjian: full in 1d 11h, before its Nov 12 reset". Codex credits come from the same data.

## Answers

The owner answers in a deck: one card at a time with Back and Next, a row of card labels to jump, and nothing is sent
until the review screen at the end ("Send N answers"). "Send current selections" jumps to that review screen early,
for a long deck. "Use the recommendations" fills every card still empty with its recommendation; the owner can change
any of them before sending. Unsent choices and notes are kept on this device (browser storage) so leaving the page
loses nothing; they are drafts, never answers.

`POST /api/status/answers` body: `{ "answers": [ { "cardId", "cardSeq", "cardSha256", "choice": ["A"] | [], "note": "...",
"via": "deck" | "recommended" } ] }`, 1 to 50 entries, at most 64 KB. Each entry is validated and recorded on its
own; the response is `{ "results": [ { cardId, outcome: "recorded" | "card_changed" | "refused", code, error } ] }`, one per entry in order
(HTTP 200 when the request itself is valid, even if some entries are refused).
- The card must be open, at that `seq` and hash. Otherwise that entry is `card_changed` and the page reloads the card.
- `choice` keys must be the card's option keys (one unless `multiple`).
- One note box serves a different answer, a comment and a question. With a choice, the record is `kind: "answer"` and
  the card closes. With only a note, the record is `kind: "note"`: it is signed and delivered the same way, but the
  card stays open, showing the note; the agent replies on the card (`replies[]`) or withdraws the card with
  `reason` (for example "Settled by your note"). Agents never write an answer.
- `via: "recommended"` marks an answer filled by "Use the recommendations" and not changed by the owner.
- The note is stored exactly as typed (trimmed of surrounding whitespace only), at most 4,000 characters.
- Stored in `<ccsDir>/status/answers.jsonl` (append, local disk, this server only), then written signed to
  `<reports>/v1/answers/<card>.json` (temporary file and rename), then the asker is rung through pm-mail. Each step's
  result is kept in the history; a failed share write or doorbell is retried and shown, never hidden.
- Signature: Ed25519 over the canonical JSON of the answer without `signature`. The key pair lives in
  `<ccsDir>/status/answer-key.pem` (0600), created on first use; the public key is published at
  `<reports>/v1/keys/aac-answers.pub` with `keyId` = first 16 hex characters of the SHA-256 of the public key.
- No agent path exists: the routes take a browser session only (not a device token, not localhost without auth).

`cards.batches` is only a grouping (id, label, cardIds) that orders the deck; there is no approve-all endpoint.

## Web UI move

The owner approved moving the web UI from Slint to Leptos on 2026-11-06 (Leptos 0.9.0-rc1, client-side rendered, served by
this TypeScript server). The Status and Scoreboard pages are the first Leptos pages, shown inside the Slint shell by an
iframe until the shell itself moves. The JavaScript view-model layer (`web-dashboard/public/*.mjs`) stays.
