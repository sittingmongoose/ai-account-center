# Optional Rust service (M3 read routes)

The TypeScript dashboard remains the public HTTP server. `ServerOptions.rustService`
(or `AAC_RUST_SERVICE`, or `rustService.mode` in `config.yaml`) selects `off`, `shadow` or
`on`; the default is `off`, and an absent or invalid value stays off.
`ServerOptions.rustServiceBinary` (or `AAC_RUST_SERVICE_BINARY`) can name an existing
`aac-http` binary. The packaged default location is `dist/native/aac-http`.
Starting the dashboard never builds, downloads or installs this binary.

| Mode | Answer and failure behavior |
| --- | --- |
| `off` | All public routes use TypeScript. No Rust process starts unless the independent `agentTransport` switch is on. |
| `shadow` | TypeScript answers first. A bounded background request compares status and canonical JSON; differences are logged without response bodies. |
| `on` | The owned read routes below use Rust after their existing TS guards. Missing/unhealthy service, deadline, response cap or invalid DTO uses the TS handler and logs `rust.service.fallback`. |

The one ownership table is [`rust-service-routes.json`](../src/web-server/rust-service-routes.json),
imported by TS and embedded by Rust. GET `/api/health`, `/api/accounts/settings`
and `/api/diag` are `rust`. The table also names `/api/store/status`, `/api/status`,
`/api/scoreboard`, `/api/status/files/*` and `/api/status/deliverable` as conservative
read candidates. Its final wildcard marks all other routes `ts-fallback`.
HEAD uses the same guarded read handlers. The table's final wildcard keeps writes and
other routes with TS.

Status and Scoreboard receive immutable accepted reader states. In on mode, Rust
composes their panels, cards, source explanations, spend, summary strip, measures,
rankings and existing native by-model/group fields without building the TS view.
TS retains source collection, store queries/ingest, shared model-identity normalization
and Intl date/collation facts. Missing facts and unsupported numeric domains request
TS fallback. Shadow mode serves the TS result and compares the Rust result in the
background; off mode does not prepare the Rust input.

Store status comes through the sole existing native writer's `status` call. The service
validates that immutable snapshot through its aac-store dependency. Transient cache,
lock and transaction information cannot be reconstructed by another reader. No second
Engine, redb writer, log writer or store lock is opened. The existing native crates stay
as path dependencies under `native/`.

File reads in on mode use an independently confined read-only descriptor before the
TS filesystem path is opened. Rust applies the existing MIME, size and range policy;
the front door validates the bounded reply and retains its original authentication,
origin, query, response encoding and security headers. File HEAD reads metadata only.
Bodies over 1 MiB and unsupported secure-descriptor hosts use logged TS streaming
fallback. Shadow compares small byte snapshots through the original TS file path.

Read inputs are capped at 4 MiB and replies at 2 MiB. The part 1 routes without an input
body retain their 64 KiB reply cap. Missing/unhealthy service, unproven shape, invalid
reply, deadline or cap always leaves the original lazy TS handler available. Raw read
views preserve JS field order and ETags; the writer snapshot and shadow byte readers
retain their per-read equality check.

`/api/health` remains public liveness only: `{"status":"ok"}`. Settings keeps its
existing signed-in-session and query checks, and reads the bounded, existing
`account-refresh-settings.json`. Before any DTO parse, a shared raw JSON gate
requires an object with exactly the expected unique keys. Arrays (including
single-element arrays), scalars, null, missing/extra keys and duplicates take
the logged TS fallback. Original bytes are retained for the strict DTO parse;
no Value conversion can erase duplicates or numeric spelling. Rust answers only a single DTO whose JSON value
is an integer token in 30..3600; these values are exactly representable in JS.
Decimal/exponent forms, strings, duplicate fields, out-of-range or unreadable
files, and any other unsupported input return a private 503 so the bridge logs
and serves the existing TS reader's result. Rust never invents a default from
an unproven parse. TS remains that file's only writer. Diagnostics
requires a signed-in dashboard session, including when dashboard auth is disabled.
It reports no Rust jobs/computers yet, scheduler `unavailable` / `jobs_not_migrated`,
and store `unavailable` / `store_owned_by_ts`. It does not open the native store or
refresh any provider.

## Account reads (M3 part 3)

GET `/api/accounts/visibility`, `/preferences`, `/registry` and `/analytics` are also
`rust` in the ownership table. Each is forwarded only from inside its existing TS
handler, after that handler's session, query and origin checks; Rust repeats the
signed-in session check. Every route answers only what is proven identical to the TS
reader or composer and returns a private 503 otherwise, which the bridge logs as
`rust.service.fallback` before running the original TS handler once.

| Route | What Rust does | TS keeps |
| --- | --- | --- |
| visibility | Reads `account-visibility.json` under the boot root (0600, 32 KiB, no symlink, folder bits, `parseVisibilityFile`) and returns the four lists plus the folder's one-time group-writable line, which TS logs through the same once-per-folder warning. | Writes; every unavailable/500 case; a symlinked or other folder. |
| preferences | Reads `dashboard-preferences.json` and the extra-sources companion with the same validation, key order and merge. Zones are admitted only from a fixed list that the spec proves the running TS accepts. | Writes; every case TS logs (corrupt, migrated remote JSONL); any zone outside the list. |
| registry | Composes the listing (`composeRegistryListing`) from the facts TS read: dashboard rows, removal refusals, sign-in availability, key metadata, jobs and trash. | Every owner read; rows whose JavaScript meaning is not proven. |
| analytics | Composes the answer around the activity block (`analyticsAvailableRange` + `composeAccountAnalytics`): range, history, summary, providers and quota series. It recomputes the range and refuses unless it equals the one TS used for its history read. | Dashboard, history-store records, activity (local scans and remote caches), saved views and the `stale`/`refreshing`/`cached` read state; TS appends `activity`. |

These routes use the same pieces as the M3 part 2 reads: the one ownership table,
`RustService.compose()` / `composeWithRust` for the two compositions (its shadow check
compares serialized bytes, key order included), the `read_routes` dispatch with its
single private 503, the raw object gate on every input envelope, and serde_json values
with `preserve_order` and `float_roundtrip`. Engine facts come from the shared
presentation envelope (`rustReadInput` → `aac_status::read_context::Context`): Analytics
asks for `Date.parse` epochs, ISO words and provider-id `localeCompare` ranks only
(`clockAndDay: false`, `rankAll: false`), and a missing fact refuses the candidate.
Objects the composer spreads (dashboard accounts and windows) travel as ordered entry
lists inside that envelope, so an own member whose value is undefined keeps its position.
The account identity reuses `aac_core::AccountKey` and admits ASCII addresses only.
File reads parse with serde_json, which gives a duplicate key JSON.parse's answer (the
last value at the first position); lone surrogates and invalid UTF-8 use the TS reader.
`RustService.owned()` serves the file reads: Rust reads its own canonical boot root,
never applies the folder warning in shadow mode, and every TS reader or composer runs
exactly once per request. Requests with bodies keep part 2's 4 MiB input and 2 MiB
reply caps; larger answers use the TS fallback.

## Owned Analytics (R5 + A2)

`rustService.groups.analytics: on` (`AAC_RUST_GROUP_ANALYTICS` overrides), with the service `on`
and the store hosted (`store: on`), lets Rust answer the whole `/api/accounts/analytics` read
from the hosted Engine in process: the quota history (`historyRecords`, thinned to the range's
buckets), the oldest stored quota sample and activity hour, the activity range and sources
(`activityRange`/`activitySources`), and the composition of both blocks. The TS computation
remains the fallback for every request Rust declines; off, shadow, or an unhosted store keeps
today's fed composition (part 3) unchanged.

The owned envelope (`data.owned: true`, `rustAnalyticsOwnedInput`) carries only what stays in TS
until H2: the dashboard accounts (as the part-3 spread entries), the live collector state (the
source grid, `refreshing`, `refreshingRemote` — the same `ownedLive` the activity answer itself
reads, refresh trigger included), the resolved pricing facts over the store dictionary's
model/route superset (`activityModels`, answered by the same `getModelPricingWithSource` lookup),
and the query's zone as proven UTC-offset segments (the server-side form of the page's
`homeZoneFacts`, bounded to the `Zone` proof's 64 segments). It also carries the bounds and
provisional range the TS computation read; Rust re-reads both from the Engine and re-resolves the
range, and any disagreement is a `stale` decline. The presentation facts are empty: the owned
composer builds its own `Date.parse` epochs and `toISOString` words from the store's ISO strings
(`aac-num`, the proven engine-independent forms) and the provider-id ranks from the CLDR root
order (`aac_num::intl::ascii_compare`); a non-ASCII model name or session key is a `collation`
decline, an instant outside the zone window a `zone` decline.

The activity block is the port of the store-fed answer (`src/store/activity-answer.ts`,
`activity-coverage.ts`, the attribution, projection and anomalies services):
`rangeToSources` → attribution → projection → session-count override, with the guarded
selection's fixed unavailable projection and the TS-published snapshot's coverage. A store with
no last read yet declines (`stale`): TS's answer would stamp its own clock, which one request
cannot prove equal. The quota composition reuses the part-3 port (`analyticsAvailableRange` +
`composeAccountAnalytics`) with `stored: true`; the answer memo stays ineffective exactly as in
the TS store path (each activity read is a fresh result), and `get()`'s flights, saved views and
`stale`/`refreshing` read state wrap the owned answer unchanged.

The spec: `tests/spec/analytics-owned-parity.ts` — every range preset, filter and zone against
the same request's pure TS computation, byte-equal, served with no TS composer call; a case Rust
cannot prove falls back and stays equal.

## Profile reads (M3 part 4)

Account switching was removed on 2026-11-13, so these GETs only read and report:
profiles, usage and sign-in health. All six are `rust` in the ownership table, are
forwarded only from inside their existing TS handlers after those handlers' session,
query and device-scope checks, and answer only what is proven identical to TS; anything
else is the private 503 and the logged `rust.service.fallback`. Rust repeats the scope:
a paired device reaches `/api/claude/desktop-profiles` and `/api/antigravity/profiles`
(the TS `/api` guard's device scope), the other four need the signed-in session. A
request that still carries a query (TS consumed `platform` and `refresh`) is refused.

| Route | What Rust does | TS keeps |
| --- | --- | --- |
| `/api/claude/desktop-profiles` | Reads `claude-desktop-profiles.json` under the boot root through the capped descriptor read and ports `listClaudeDesktopProfiles` + `listClaudeDesktopProfileMetadata` (text, email, id and SSH-alias rules, duplicate ids, `canOpen`, `launchUri`). | Each row's `openOperation` (this process's memory), added to Rust's rows exactly as the handler does; a symlinked manifest (TS follows it), one over 1 MiB of UTF-8 (TS counts UTF-16), invalid UTF-8 and every 500. |
| `/api/claude/desktop-profiles/usage` | Composes the answer (`composeClaudeDesktopUsage` / `readCachedSample`) from the manifest identities and the remote history text TS read; `toISOString` for years 1970-9999. | The manifest read, the SSH history reads, the 30-second memo; a history serde_json cannot prove `JSON.parse` refuses, and a sample past year 9999. |
| `/api/codex/profiles` | Composes the summary (`composeCodexAuthProfilesSummary`: `resolveActive` precedence and `path.join` for a normalized instances folder). | The registry YAML, every saved and live login (credentials never leave TS; only display-safe fields are sent), the 5-second memo, and the stale `CCS_CODEX_PROFILE` case TS logs. |
| `/api/codex/profiles/quotas` | Composes `composeCodexProfileQuotas` / `buildQuota` with `readingPredatesReset` over the profiles, collector rows, renewal notes and response clock. | The collector (with its 2.5 s budget and cached rows) and the renewal reader; a date only the engine's legacy parser reads. |
| `/api/antigravity/profiles` | Composes `publicInventory` (`publicRuntimeServiceProblem`, `isNativeVersion`, `displayText`, `email`, `timestamp`). | The runtime's inventory read; every 500. |
| `/api/antigravity/profiles/quotas` | Composes `publicQuotas`: `publicDashboardAccount`, `poolWindow`, `antigravityPlanDisplay` (plan table ported; `known()` lowercasing proven: only U+212A lowercases to ASCII) and `markPassedResets`. | The runtime's accounts read (and `refresh=true`); a `kind` given as an array (TS coerces it), an own `remaining`/`expiresAt` member left undefined; every 500. |

The five compositions use part 2's `compose()` / `composeWithRust` and the ISO-only
presentation envelope (`rustReadInput(..., { rankAll: false, clockAndDay: false })`):
`Date.parse` epochs and `toISOString` words are the engine's facts, and a missing fact
refuses the candidate. Inputs pass `jsonExact` first (finite numbers, plain objects, no
holes), so anything JSON cannot carry exactly never leaves TS. The pure TS references are
split out of the services (`composeClaudeDesktopUsage`, `composeCodexAuthProfilesSummary`,
`composeCodexProfileQuotas`, and `src/antigravity/public-reads.ts`); the memos stay where
they were and now cache the composed answer, whichever side composed it.

**Capped file reads.** Every private file Rust answers from (the desktop manifest, part 3's
visibility and both preference files, the refresh settings) is a row of the shared registry
and is read through `aac_files::read` (L2, below): one `lstat`, one open with
`O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC`, every decision on the opened descriptor plus
its identity against the `lstat`, at most the cap plus one byte read. A FIFO, folder or other
special file swapped in is refused without blocking; a final link and `ENOENT` keep the
owner's meaning per file. The TS owners got the same hardening where practical:
`readPrivateJsonFile` adds `O_NONBLOCK`, and the preference reader reads through one
no-follow, nonblocking descriptor after its `lstat`.

## Home view (D7)

`AAC_RUST_HOME_VIEW=on`, with the service `on`, lets the server build Home's view model
(`crates/aac-views` `home`, view model v3); the default is `off`. Off, `GET /api/views/home` is not
offered: it answers the 404 every unknown endpoint gets, and the dashboard answer is unchanged. On,
the dashboard answer adds `server.views: ["home"]` and the page asks that route instead of its three
reads (the dashboard, `/api/claude/desktop-profiles`, `/api/antigravity/profiles`).

The route keeps the dashboard route's session, query and failure rules and answers those three answers
plus `view`. It reads each one the way its own route answers it: the desktop profiles through part 4's
capped manifest read (`ownedValue()`, the value form of `owned()`, with TS adding each Open's progress)
and the Antigravity copy through part 4's composition, both with the TS fallback. It uses the shared
pieces: the page's context (clock, display zone, platform, refresh
interval, user, host and a format probe of its `Intl` formatters, sent in `X-AAC-Home-Context`) and the
three answers are the data of the M3 presentation envelope (`homeReadInput` in
`src/web-server/views/home-context.ts`, over `rustReadInput`: this engine's clock and day words in the
page's display zone, ISO epochs, and the ranks of the account ids and sign-in profiles Home sorts by);
`compose()` forwards it, and `read_routes` dispatches it to `aac_views::home::compose`. Rust joins the
words in the style the probe proves (Chrome and Node, U+202F before AM/PM, or iOS Safari's
`Jan 5 at 12:07 AM`), compares ASCII ids by the envelope's ranks, and admits only date strings with an
explicit zone. Anything it cannot prove equal to `web-dashboard/public/view-model.mjs` (another locale,
a local-time or date-only date, a non-ASCII sort pair, a missing fact, a Claude Open in progress, an
unexpected shape) is the shared fallback, and `view` is null: the page then builds its own view, as it
does for an unreadable context, missing profiles, a context that changed while the view was read, and
any route failure but a sign-out. The page remembers the offer, so the first read after a reload asks
too; while the switch is off nothing is stored.

The spec is `tests/spec/views/home` (`cases.json` from the dashboard's own JS tests plus zone, edge,
style and decline cases, each with its envelope facts; `declines.json`; `fuzz.mjs` for seeded runs),
checked by `crates/aac-views/tests/home_spec.rs`, `web-dashboard/tests/home-view-spec.test.mjs` and
`tests/unit/web-server/home-view-routes.test.ts`. `node tests/spec/views/home/generate.mjs [--check]`
rewrites or checks it (Bun computes the envelopes, `envelope.ts`).

## Boundary and lifecycle

The service binds no TCP port. It accepts only its Unix socket, mode 0600, in a
0700 random boot directory `aac-http-<id>` under the canonical AAC private directory's
`run` folder. When that socket path would be longer than 100 bytes (a Unix socket path
must fit 108 bytes on Linux, 104 on macOS), the boot directory goes under a short private
folder bound to the root by the first 12 hex digits of SHA-256 of its canonical path:
`$AAC_RUST_RUN_DIR/aac-<hash12>` (for a container, `/run/aac`), else
`$XDG_RUNTIME_DIR/aac-<hash12>`, else `/tmp/aac-<uid>/<hash12>`. Every folder on the way is
a real 0700 folder of the root's owner (no links), and Rust checks the name, the hash, the
mode and the owner again before it binds. The descriptor stays in `<root>/run`. TS refuses
symlinked/nonprivate run directories. The Rust listener sets 0600 before it accepts requests
and refuses existing socket paths.

TS sends a new 256-bit secret over the child's stdin pipe, never argv, inherited
environment or logs. A 0600 `run/aac-http.json` descriptor under the 0700 run folder
allows another front door using the same private root and binary to attach to a
healthy boot. It includes the boot secret and must be treated as credential data.
An owned exit removes its descriptor and boot directory. Startup replaces stale
metadata using a fresh random directory/secret, without signalling a PID from disk.
An attachment does not terminate the owner on cleanup. The spawning front door
owns the boot; its stdin closing ends that service. Normal shutdown also sends
SIGTERM, with a one-second SIGKILL backstop.

Rust removes its inode-matched socket, empty boot directory and empty shared parent
on shutdown, including when its front door dies. TS serializes its own directory
creation and cleanup and retries ENOENT at most twice when Rust removes a parent
between validation and child creation. Each attempt recreates and revalidates every
layout component (directory type, current owner and 0700 mode, without following a
link at that component) before spawning. Startup only sweeps stale boot children
of the selected root; it does not scan abandoned parents belonging to other roots.

The child's environment is an allowlist (`rust-service-env.ts`): home and profile folders,
the CCS and provider selectors, locale, temp and the Windows system names. The provider keys
and proxy names a Local agent verb reads join only while the agent transport is on (Wonder
L1): shadow reads never spawn a Local verb, so the long-lived service holds no provider key
for them.

The same raw object gate covers the boot input, principal header and each
embedded route-table entry. The route list itself intentionally remains an
array; entries are retained as RawValue bytes and then checked as exact objects.
Each parsed DTO also denies unknown fields. Health and diagnostic DTOs are
output-only in this service and are not parsed from requests.

Every socket request needs both the boot secret and a strict `x-aac-principal`
JSON header (`kind`, `id`, `role`). Duplicate, missing or malformed headers and
impossible role/kind combinations get 401, including on liveness. Incoming kind
and role fields accept string words only; serde enum-object spellings are refused. TS constructs
these headers only inside the guarded handlers; it copies no caller credentials,
origin, principal or secret header. Session ids are hashed before forwarding.
The existing bar authenticates as a session; a nonce alone never creates a bar
principal. No hub authentication is ported. The DTO reserves device/bar/hub kinds
for later ports without granting them settings/diagnostic access.

For signed-out public liveness only, TS asserts the reserved envelope
`{kind:"session",id:"public-health",role:"public"}`. This denotes the verified
public-route decision, not a signed-in session; Rust cannot use it for private
routes. The secret is still mandatory. The socket boundary trusts the OS account
and its private run folder; it does not isolate mutually hostile processes running
as that same account.

Requests have a 500 ms total deadline in TS, the response caps above and a maximum of 16
in flight per front door. TS owns HTTP encoding, cookies, caching, compression,
ETags and all existing auth/origin/session checks. On Windows, where this service
is not built, the TS fallback remains available. No lifecycle step refreshes a provider.

## Lifecycle and bridge floor (L1)

L1 of the M3 completion design (`DESIGN-M3-COMPLETE.md` section 2.1). All of it is
inert while the service is off.

| Piece | Behaviour |
| --- | --- |
| Startup | Public routes and the listener do not await Rust acquisition. Hashing uses asynchronous nonblocking reads of a regular image; one 5 s elapsed budget covers identity, attach, stale cleanup and readiness. Stop cancels acquisition and closes an unadopted child. Requests decline to TS until ready. |
| Connection | One keep-alive `http.Agent` per boot (16 sockets), replaced when the boot changes. A request on a kept-alive connection the service had closed is retried once on a new one, inside the same 500 ms. |
| Route table | `rust-service-routes.json` rows carry `group`, `roles`, `budgetMs` and `handler`. Rust builds its dispatch from them (startup refuses a row without a known handler, a read without a budget, a public route open to the `service` role or an internal route open to any other); TS gates forwarding on the same roles. |
| Deadline | Each route's budget in Rust (400 ms for reads). Blocking work runs on two semaphore pools: 8 permits for reads, 4 for the store group. A full pool is a `busy` decline at once; a task that outlives its budget keeps its permit until it ends, so no more than 8 (or 4) blocking tasks ever exist; work whose request was dropped before it started never runs. The runtime caps blocking threads at 16. |
| Declines | One `Decline` enum (`aac-views`) sent as `x-aac-decline: <word>` on the private 503: `shape`, `number`, `date`, `zone`, `locale`, `collation`, `stale`, `busy`, `deadline`, `unsupported`, `internal`. TS counts every TS answer per (route, reason) — the service's word, or the bridge's own (`unavailable:down`, `unavailable:breaker`, `failed:deadline`, ...) — and bounds `rust.service.route_state` to the first two states per route per minute; the periodic summary counts suppressed comparisons. |
| Breaker | Per route in TS: 3 transport failures or timeouts in a row open it for 30 s, then one probe goes. Declines are replies and never count. Admission only looks; the probe is reserved when the request is really sent, so a request TS keeps (no input, another root) never holds it. A new boot starts with every breaker closed. |
| Restart | An owned service that exits is restarted after 1 s, 5 s, 30 s, 2 min, then 10 min; after 5 restarts in an hour TS logs `rust.service.gave_up` and answers alone until the next dashboard start. An attached boot that stops answering is dropped the same way. A process that accepts but never answers (stopped, deadlocked) is caught by its transport timeouts: after 3 in a row on any route, a health probe (500 ms) decides, and a hung process is retired into the same restart budget. Rust's own `deadline` and `busy` declines are replies and never count. Boot folders older than a minute whose socket refuses a connection are swept at start; no PID read from disk is ever signalled. |
| Build check | Health replies carry `x-aac-service-build: {commit, sha256, protocol, storeSchema}`, with `sha256` the running image's own hash. The descriptor records it. A front door attaches only when the live identity equals the descriptor's, its `sha256` is that of the binary it would start, and the protocol is its own; otherwise it starts its own boot (an in-place upgrade). The public `/api/health` body stays `{"status":"ok"}`. |
| Feeds, events | `PUT /internal/feed/<source>` `{generation, data, facts}` and `GET /internal/feed/<source>`; `GET /internal/events` streams NDJSON (`facts-needed` when a generation arrives without facts). Only the reserved `{kind: session, id: front-door, role: service}` principal reaches them. Since R1 each source holds its latest generation's payload and facts, immutable; see Home, fed (R1). |
| Shadow | At most 2 comparisons in flight per route; the rest are skipped and counted. TS answers first, then Rust is asked, so each comparison times both paths on the same request. On a compose route the paired TS timing includes the per-request input-envelope build: the user waits for it before the TS answer (Wonder L5), and the Rust timing includes it in both modes. |
| Reply floor | `view_reply` in Rust and `sendRustView` in TS: the final bytes, the revalidated-view ETag and `x-aac-view`, written without a parse. Home (R1) is the first route that uses it. |

**Measurement hooks.** `GET /api/diag?bridge=1` (signed-in session) answers the skeleton
diagnostics plus a `bridge` block: the service state, its build, restarts, the child's RSS,
the counters and, per route, the paired timings (TS path and Rust path on the same requests,
from shadow) and Rust-only timings (on). A compose route's paired TS timing carries the
input-envelope build, since shadow serves it before the TS answer; the Rust timing carries it
in shadow and on alike. Every minute with traffic, `rust.service.minute`
logs the same per-route block for that minute, and the event-loop monitor's line adds
`rustMode` and `rustRssBytes`. Response bodies are never logged. A route's `served` counts every
Rust success on it; `fedServed` and `fedDeclined` count only the fed path's own answers and named
declines, so an envelope fallback's successes on the same route are `served - fedServed` (R2b L2).

`tests/spec/bridge-measure.mjs` measures a built `dist/` under Node: off and on servers in
alternating rounds (5 x 40 samples per route) with the load average around each round, a
16-client, 60 s concurrency row with the server's event-loop p99, RSS of TS and the Rust
child, shadow paired timings, and (`--heap`) the heap the Status and Scoreboard owners hold
on the large synthetic home (`tests/spec/large-home.mjs`: 50 profiles, 200 reports, 5,000
runs, 30 accounts x 12 windows x 90 days). The worker server runs inside the same closed
boundary as the contract capture.

## One file policy (L2)

L2 of the M3 completion design (`DESIGN-M3-COMPLETE.md` section 2.2). No answer changes.

- **One read.** `aac_files::read::{read, open}` with a `ReadPolicy`: a byte cap, the leaf rule
  (a final link refused, or read as absent), the ancestor rule (no link below a base), the
  owner rule (this user) and the mode rule (no group or other bit). The outcome names what was
  found (`Bytes`, `Absent`, `Link`, `Foreign`, `BadMode`, `NotRegular`, `TooLarge`,
  `Unreadable`, `Unproved`); each reader maps it to its reference's meaning. Listings use
  `aac_files::read::entries`, which yields at most a count cap and then says so.
  On Unix the ancestor rule pins each folder below the base as a descriptor chain (`openat`,
  `O_NOFOLLOW | O_DIRECTORY`, Wonder L12): the leaf is lstat'd and opened relative to the
  pinned parent, so a folder swapped for a link at any point cannot redirect the read. The
  leaf's own open is `O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC`, with a (dev, ino)
  identity check against its lstat. With `Ancestors::Any` the caller admitted the folders (or
  the reference follows them) and the leaf is opened by path; the identity check still holds
  for the final name. Directory descriptors use `O_PATH | O_DIRECTORY` on Linux/Android and
  `O_SEARCH | O_DIRECTORY` on macOS, FreeBSD, NetBSD, Solaris and illumos, preserving named
  reads through search-only folders. Other Unix platforms keep `O_RDONLY | O_DIRECTORY`;
  a chain's `PermissionDenied` declines the whole agent verb to the labelled Python reference
  (`{"fallback":"python"}`), rather than serving an incomplete inventory. On Windows the open is the
  reference's plain open (owner, mode and identity pass trivially); native qualification is
  an M4 gate item.
- **One path rule set.** `aac_files::path`: `python::{double_root, canonical_home, normpath}`,
  `node::resolve`, `linked_below`, `linked_within`, `hop` (one `_local_path` hop), `safe_name`
  and `foreign`.
- **One writer.** `OwnedStore` stages a 0600 file with `O_EXCL` beside the target, fsyncs it,
  renames it and fsyncs the folder, under a kernel lease (`<name>.lock`, or a lease on the
  folder itself where another implementation counts the folder's entries).
- **One registry,** `config/private-files.json`, read by TS (`src/shared/private-files.ts`) and
  embedded in Rust (`aac_files::registry`): each file's root, path, class, cap, mode, writer
  on the dashboard host, whether the agent writes it on other computers, and the route group
  whose `on` hands it to the service (`movesWith`; none yet). TS resolves the owners at start
  and sends them in the boot JSON (`files`); the service refuses a map that differs from its
  own registry, and a front door attaches only to a boot with its own owners. TS writers
  (`private-json-store.ts`, the key store, the preference and refresh-settings writers, the
  readers check) refuse a row TS does not own.
- **Gate.** `scripts/foundation/one-file-policy.mjs` (in `tests/spec/run.mjs`): outside
  `crates/aac-files`, no Rust source uses `O_NOFOLLOW`, `custom_flags`, `OpenOptions`,
  `File::open`, `File::create`, `fs::read`, `fs::read_to_string`, `fs::write`, `fs::rename`
  or a tempfile persist. The
  allowlist names each exception with its reason (the store engine, the scanner, the Cursor key
  module, the service's own image hash) and fails when an entry no longer matches.

## Offline checks

Build Rust only on Nas1 or the Mac, using an isolated copy, `nice -n 19`, two Cargo
jobs and the host's memory-admission rule. Never compile it on the VM. Workspace
`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and
`cargo check --target wasm32-unknown-unknown -p aac-api` cover the DTO and socket
boundary. The Linux integration test examines the child process's socket inodes
against both TCP tables; unsigned requests are tested over the real Unix socket.

On the VM use `AAC_NO_LOCAL_RUST_BUILD=1` and a temporary `CCS_HOME`. After
`bun run build:server`, copy the Nas1-built binary into the ignored
`dist/native/aac-http` and native fixture modules as described in the contract harness, then run:

```sh
export AAC_RUST_SERVICE_BINARY="$PWD/dist/native/aac-http"
AAC_RUST_SERVICE=off node tests/spec/run.mjs
AAC_RUST_SERVICE=on node tests/spec/run.mjs
AAC_RUST_SERVICE=shadow node tests/spec/run.mjs
```

`run.mjs` ends with `tests/spec/rust-lifecycle.mjs`, which runs under Node on `dist/` and the
real binary: keep-alive (20 requests, one connection), a killed service restarted after its
backoff, an in-place upgrade never attached, a root too long for a socket path, the sweep, the
named declines with the bridge diagnostics golden (`contract/l1-bridge-diag.json`), and a feed
answered by `facts-needed`. The deadline under a stuck read, the pool bound and the cancel are
checked in `crates/aac-http/tests/lifecycle.rs` with an `LD_PRELOAD` slow-read shim (Linux,
needs `cc`), and in `limits.rs`.

`account-read-parity.ts` runs the four account read routes through the real TS
routers in all three modes against `contract/m3-part3-account-reads.json`, then
compares the TS composers with the Rust socket over generated Analytics and
registry tables (see the spec README).

`settings-numeric-forms.json` and `settings-parity.ts` compare 65 numeric/document
forms against the real TS reader and the actual Rust socket, then run them through
the production off/on/shadow bridge with exact TS-call and reply/fallback counters.
The table covers fractional rounding, exponents, huge numbers, negative zero,
strings, duplicate/out-of-domain values, top-level JSON shapes, nested arrays,
extra keys, and the canonical writer integers.
Unsupported shadow responses also log a fallback rather than compare a guessed
value. `bun tests/spec/settings-parity.ts --ts-only` checks the real TS side
without running Rust; the full spec runner selects that on the VM in off mode.

The fixture permits only that exact existing binary to cross its otherwise closed
process boundary. On/shadow checks fail when the binary is missing, and assert
actual forwarding counts so a fallback cannot falsely prove Rust parity. All five
roles retain their golden responses. The same sandbox injects real socket faults,
checks spoofed caller headers, route guards, attachment, unhealthy fallback,
response caps/deadlines and a real shadow difference. No live home or provider is
used. See [the contract harness](../tests/spec/README.md) for capture limits.

`read-route-parity.ts` adds a separate real-reader golden document under `tests/spec/contract/`.
It builds populated Status and Scoreboard fixtures through the real TS readers, reads fixture
media, and checks exact serialized candidate bytes in off/on/shadow modes. It also checks
unsupported-store fallback, candidate mismatch and the input cap. The original M0 stub cases
remain unchanged; the extra fixture does not claim all source states have been ported.

For the optional native-field fixture, point `AAC_STATUS_MODULE` at the aac-status Node
module built on Nas1, then run `bun tests/spec/read-route-parity.ts`. This adds an eighth
case which invokes the real TS native adapters and compares their by-model/group output
with the service's path dependency. No module is built or installed by the harness.


`read-view-parity.ts`, `read-file-parity.ts` and `read-http-parity.ts` add 39 raw
state/timezone cases, 29 guarded file cases and five actual native-store HTTP routes.
The HTTP fixture keeps its sole writer open and asserts zero TS view-builder calls in
on mode. The number corpus checks 120,992 JS precision/fixed/Math.round forms, including
both adjacent binary doubles at decimal half boundaries. All fixtures use temporary private roots; no collector is refreshed
against a live provider. See the harness README for release-binary RSS/latency commands.


Every number and date the Rust side writes for a JavaScript, `Intl` or Python reference comes from one crate,
`crates/aac-num` (L3): ECMAScript text and rounding, `Intl.NumberFormat` en-US, zone words and ASCII collation,
CPython `round`/`repr`/`fromtimestamp`. Each operation states its proven domain and answers `None` outside it, so
the caller declines; the views keep the bounded domain they were proven on (`aac_num::view`). Its corpora are
generated by real Node and CPython (`tests/spec/num/`, see the harness README).

All Math.round sites use one binary64 helper (`aac_num::ecma::math_round`): compare the exact
fraction with 0.5, preserve ties toward positive infinity and negative zero, and
leave already-integral values unchanged. Adding 0.5 before floor is forbidden
because the addition can cross a rounding boundary. toFixed/toPrecision continue
to round the exact binary significand within their admitted domain. Integer words
outside the proven decimal formatting domain request logged TS fallback. The
production bridge/Status-validator regression is driven by the Node-generated
number table; it catches the pre-fix 0.049999999999999996 -> 0.1 wrong answer.

## Agent transport (L5, off by default)

`agentVerbs: off|shadow|on` is the ceiling for the verb adapters; `agentVerbFamilies`
(`AAC_AGENT_VERB_FAMILIES`, JSON) sets one mode per verb under that ceiling. An absent
map keeps the ceiling for every verb (the legacy scalar behavior); a present map turns
on only its listed families, so the read-only families turn on one at a time
(`signin-expiry`, `quota-read`, `failover-inventory`, `t3-pins`, `scan-usage`). A
present but invalid map keeps every family off. `key-store` and `claude-history` write
on the computer, so their shadow mode is off: shadow never duplicates a write.

`shadow` serves the reference (Python/TS/scanner) answer as soon as it settles and runs
the agent alongside; the comparison is logged in the background as `agent.verbs.shadow`
with the verb, outcome (`equal`, `different`, `fallback`, `error`, `reference-failed`,
`reference-fallback`) and both durations. Bodies never enter the log. Object key order
is the only normalization; a scan whose reference is `python-fallback` is
`reference-fallback`, not a difference. `on` keeps the agent-first boundary with the
labelled helper fallback.

Independently, `agentTransport: on` (or `AAC_AGENT_TRANSPORT=on`) sends those calls to
`POST /internal/agent/<verb>` on the private service. It can start the service while
public Rust read routes remain off. The endpoint requires the boot secret and the
reserved front-door service principal; it is never exposed as a public dashboard route.
Shadow read mode does not duplicate agent operations. Absent/invalid settings leave the
original TS behavior intact.

The service owns one four-channel `Computer` and one verified client per configured
computer, loaded through `aac-config` and the packaged `computers.json` seed. Calls
resolve a configured id/SSH alias and OS; an unknown or ambiguous selector refuses.
A successful handshake is cached for that client's lifetime. Hash verification and
fixed launchers live in `aac-transport`; the labelled TS fallback keeps its previous
verification while the service is absent/old. Both use the 64 MiB artifact cap from
`config/tables/agent-transport-policy.json`.

**Shipping is disabled on the service path.** Only an already installed hash-pinned
agent is admitted. Missing, damaged or old agents return the labelled helper fallback;
no prepare, SCP or commit step runs. M3D4 (ship in stages, VM first) and M3D5 (the VM's
current key, AAC-pinned known_hosts) are answered: `bun run agent:build` builds the
agent on each build computer (Nas1, the Mac, aac-winbuild; never the VM) into
`crates/aac-agent/prebuilt/<target>/`, `--copy-only` carries the matching targets into
`dist/agent/` during `build`, and the lead reviews and commits each target's pin into
`src/web-server/services/agent-artifacts.json`. The lead then places the binary on each
remote computer with `scripts/agent-install.sh` (Nas1, Mac) or `scripts/agent-install.ps1`
(Windows): digest check, staged `.part`, re-check, atomic rename, `--version` handshake.
The VM is Local and runs the packaged binary in place. Linux artifacts are glibc until
the musl target is authorized (F11); both are admitted. This implementation creates no
SSH identity and edits no trust or authorization file.

SSH requires an explicit existing identity file (`computers[].transport.key`, or
`agentTransportSsh.identityFile` for seeded computers) and an explicit pinned source
(`agentTransportSsh.knownHostsFile`). `AAC_AGENT_IDENTITY_FILE` and
`AAC_AGENT_KNOWN_HOSTS_FILE` override those global paths for isolated gates. No path is
guessed from a home directory. `StrictHostKeyChecking=yes`, `IdentitiesOnly=yes` and the
shared ControlMaster policy apply; whitespace in option paths is quoted for OpenSSH.
SCP always runs the probed SSH file (`scp -S <that file>`), never scp's built-in or
PATH ssh. Every SSH/SCP launch (TS, Python, Rust and the native reset helper) spawns that
exact file, and its identity is checked again immediately before the spawn (Rust: after any
wait for a channel); a client replaced after argv was built is refused, never launched. A
swap between that check and exec is the accepted residual: it needs someone who can already
replace the user's ssh. A failed probe is cached for 30 s only, so a recovered client is
probed again; a successful probe stays cached for that file identity.
SSH/SCP preserve OpenSSH's connection config: every IdentityFile in order, HostName,
User, Port, Match and jumps. Explicit AAC fields override their corresponding settings;
`-i` and `IdentitiesOnly=yes` apply when AAC specifies a key. There is no synchronous
`ssh -G` alias discovery. Match commands execute asynchronously in the SSH child, inside
that action's existing process timeout.

Command-line overrides enforce `StrictHostKeyChecking=yes`, `UpdateHostKeys=no`,
`ForwardAgent=no`, `ForwardX11=no`, `GlobalKnownHostsFile=none`, `PermitLocalCommand=no`
and `ClearAllForwardings=yes`. The pin is the configured file (legacy TS/Python default:
`~/.ssh/known_hosts`); alternate global trust and `known_hosts2` do not supplement it.
A bounded asynchronous no-connect capability probe enables
`KnownHostsCommand=none` when supported (OpenSSH 8.1 lacks it). Its result is cached
per executable file, not per pathname: canonical path, device, inode, size and
modification/change time (Windows: volume serial and file index in TS/Python, creation
time in Rust). Replacing or retargeting the client re-probes it, and Rust's AAC master
socket name includes that identity. It also probes
`SetEnv=AAC_SSH_POLICY=1`: the harmless single assignment takes precedence over the
config's SetEnv list. Unsupported options are omitted with one compatibility warning.
On a client unable to suppress SetEnv, literal values in its own config can still reach
the host, as before; AAC process secrets remain excluded.

SSH/SCP receive only PATH, HOME, USER, LOGNAME, LANG and SSH_AUTH_SOCK, plus Windows
SYSTEMROOT/USERPROFILE/WINDIR. Local agent authentication remains available; agent
forwarding stays disabled. Local provider verbs retain their existing environment.
TS/Python use `ControlMaster=no` and `ControlPath=none`, because reusing a user master
would bypass the AAC pin. Rust uses private 0700 owner-checked control directories and
short socket paths bound to the pin path, current pin-file identity and policy options.
Changing that identity selects a new master. Long run paths use an AAC-owned short
private directory under /tmp on Unix. No key, trust or SSH config file is created or changed.
Changing configuration or the catalog requires a service restart.

The response is bounded NDJSON: progress, then a result, failure or explicit fallback.
A handled refusal goes to the caller's helper; an absent/old service uses the labelled
TS transport. POSTs are never retried. The caller's existing helper reserve stays in
TS and covers both bridge and legacy attempts. Keys remain stdin/pipe data, never argv.

`POST /internal/agent/<callId>/control` accepts a bounded `{"input":"..."}` or
`{"cancel":true}` frame. Input acknowledgement means queued, not handled by an
interactive verb (M4 adds those). Cancel acknowledgement is true only after the agent's
cancelled terminal frame and the SSH/local process exit. If no acknowledgement arrives
within two seconds, the owned transport is terminated and the response says false;
that is never a claim that a real remote descendant stopped. Closing a result stream
also cancels its owned transport. Fixtures exercise a far-process recorder, not live SSH.

`python3 tests/spec/agent/transport.py <fixture-agent> <aac-http>` runs the unchanged
agent corpora directly and through the private socket/SSH recorder and compares their
counts. It also checks six HTTP hangs opening the three-failure circuit, pooled reuse, breaker, byte/hash caps, no shipping, authentication,
progress/input and acknowledged cancellation. All binaries stay on the build host.

## M4 host action verbs (off by default)

`launch` and `claude-host` move the host work of `src/utils/app-launcher.ts` and the Claude
profile bootstraps (`claude-host-transport.ts`, `claude-host-mac-program.ts`, the Open
commands in `claude-desktop-transport.ts`) into the agent. They ride the same `agentVerbs` /
`agentTransport` switches and the same fallback contract: every unavailable, unproven or
failed agent answer delegates to the labelled reference (the TS launcher, the ssh
bootstrap), which stays in place until the cut. The guards stay in the front door.

- **launch** is a byte-for-byte port of the reference builders: the transient user-service
  argv (`--user --unit=aac-launch-<app>-<pid>-<suffix> --collect --property=KillMode=process`,
  bare `--setenv=KEY` flags, the loader trampoline, `$` doubling), the systemd-run
  environment (the app's values plus the inherited `XDG_RUNTIME_DIR`), the fail-closed
  refusal when systemd-run is missing, and the detached direct spawn on the other platforms.
  Environment values never reach an argv. Non-ASCII spellings delegate to the TS launcher.
- **claude-host** carries `{op, host, profileId?, trashName?, launcher?, accountList?, token?}`
  for create, undo, state, session, trash, restore, purge and open. The Mac backend is a
  native port of the fixed Python program (the same folder rules, the sh launch script,
  plistlib's XML form, the ps needle rule, the sign-in marker read); the Windows backend
  generates the reference's fixed PowerShell byte for byte and runs it locally behind the
  same encoded bootstrap, so the COM shortcuts, scheduled tasks and allowlist keep the
  reference's exact behavior. The host family must match the agent's computer.
- **purge** additionally requires the TS-issued permit: `claudeHostPurgePermit` computes
  `<expiresMs>.<HMAC-SHA256>` over `aac-claude-host-purge\0<host>\0<trashName>\0<expiresMs>`
  with the one shared key in `config/tables/agent-transport-policy.json`
  (`claudeHostPurgeKey`), riding the confirmation's 120-second lifetime. The lifecycle
  issues it after the route consumed its confirmation (and for the due sweep); the verb
  verifies it before any path is touched and refuses without it (`confirmation_invalid` /
  `confirmation_expired`). The key binds the permit to the TS+agent pair; it is not a
  credential against the OS account, which can already read its own trash.

The parity suites (see `tests/spec/README.md`, M4a/M4b): the checked-in tables generated by
the real TS builders (`launch-table.json`, `claude-host-table.json`, the SAFE_PATH category
ranges), the Rust table tests, `launch_exec.rs`'s recorder runs, and
`claude_host_parity.py`'s fixture-home comparisons — Mac operations with byte-identical
trees (trash then restore included), the fixture-safe Windows steps, and the permit
refusals. No real app, shell folder, scheduled task or provider is touched by the suites.

## Sign-in sessions (M4c, off by default)

`agentVerbs: on` with `agentTransport: on` and a live service connection moves the interactive
sign-in CLI session into the `signin-session` agent verb (`crates/aac-agent/src/signin_session/`).
The TypeScript runner stays the fallback and keeps the install commit until M5.

- **One session per agent process.** `start` runs the ported `SignInJobRunner` state machine
  (`signin-jobs.ts`) with the ported output parser (`signin-output.ts`) and the ported process
  layer (`signin-process.ts`): the same fixed Python PTY relay byte for byte, the complete
  environment and nothing else, a new process group, SIGTERM then SIGKILL after the two-second
  grace (the relay's own one-second grace is inside the fixed program). Plain-pipe commands
  (the Antigravity driver shape, Muse over SSH) skip the relay. Every job state change is a
  `progress` event carrying the same job DTO; `{"input":"<code>"}` submits the one supervised
  authorization code; `{"cancel":true}` ends the job cancelled and kills the CLI group. The
  terminal frame is the final job, or the cancelled failure the transport acknowledges.
- **The install boundary.** On CLI exit 0 the session reports `verifying` with
  `installPending` and exits; the front door's `AgentSignInJobRunner`
  (`src/web-server/services/signin-session-agent.ts`) runs the unchanged flow `complete`
  under its identity guards and registry lock, emits the remaining events exactly as the TS
  runner does, and settles the journal. Origin, session and confirmation guards never moved.
- **The journal.** Every state change is written atomically through the one-file registry's
  `signin-jobs` row (0600, `<ccs>/signin-jobs/`). One-shot actions read it: `get` (the journaled
  job, null past the ten-minute retention, or the restarted placeholder for an unknown id),
  `list`, `cancel` (the second call sets the flag the owner polls, kills the relay group and
  the CLI's own session, and verifies the child is gone), and `settle`. A running entry whose
  owner died recovers as `failed/server_restarted` and its stray CLI group is killed; an
  installing entry is owned by the front door's pid, not the exited agent. The runner's limits
  (one running job per provider, three in all) are enforced against the journal at start.
- **Scrub.** Raw CLI output is parsed in memory and discarded; only the allowlisted URL and
  the device user code enter events. The submitted authorization code, credential bytes,
  tokens and emails never enter events, the journal or logs.
- **Deadline.** The verb keeps the CLI's own 15-minute window: `max_deadline_ms("signin-session")`
  is 960 s (the other verbs keep 600 s). The adapter sizes the request deadline past the job
  timeout plus the install margin.

The checks: `cargo test -p aac-agent signin_session` (parser, state machine, journal), and on
the build host `bun tests/spec/agent/signin-session.mjs /path/to/aac-agent`: fake-CLI PTY
scripts per provider (Claude, Codex, Antigravity, Muse) drive the real TS runner and the verb
on the same fixtures and compare the full job event sequences, plus the cancel-kill verified
by a second call, the scrub, the killed-agent journal recovery, the settle, the limits and the
restarted placeholder. Synthetic credentials and temporary roots only; no provider is
contacted. A supervised live sign-in per provider remains the turn-on gate (decision 7).

## Home, fed (R1)

R1 of the M3 completion design (`DESIGN-M3-COMPLETE.md` section 1.1): Home and the accounts dashboard at the
**fed** level. Off by default: `rustService.groups.home: on` in `config.yaml` (or `AAC_RUST_GROUP_HOME=on`),
under `rustService.mode: on`, which stays the ceiling. With the group on, Home is offered as with the D7 switch,
and `GET /api/views/home` is answered from fed generations; with it off, Home is D7's path (or not offered) and
nothing below runs.

- **The owners push only when their data changed** (`views/home-feed.ts`, `PUT /internal/feed/<source>`), with
  a numeric generation per source that only moves forward (a late push of an older one is not taken, and the
  reply names the generation held):
  - `home-dashboard-<platform>-<0|1>`: `AccountDashboardService.readForHome` (the answer before its
    per-response pass) per platform and secure transport, as `JSON.stringify` writes it with `updatedAt: ""`.
    That text is the change key: the request writes it once and sends it only when it differs. Its facts,
    computed once per generation: the `localeCompare` ranks of the account ids and sign-in profiles Home sorts
    by, and the `Date.parse` epochs of every `resetAt`/`sampledAt` string `markPassedResets` reads;
  - `home-opens`: this process's Claude Open progress per profile (`ClaudeOpenOperations.forScope`);
  - `home-antigravity`: the runtime's inventory in the ISO envelope; the service composes it (part 4) when it
    arrives.
- **A request carries only** the principal, the query (`x-aac-feed`: platform, secure transport, the
  generation ids), the clock (`now` of the per-response pass, `updatedAt`) and the page's context as TS read it
  (`x-aac-home-context`). No body.
- **The service** (`crates/aac-http/src/home_fed.rs`, `aac_views::home::fed`) reads the desktop manifest
  itself (part 4's capped read), adds each Open's progress, applies `markPassedResets` with `now`, builds the
  view with clock and day words from the page's own zone offsets and probed style (no server `Intl` per
  request; the ranks are the generation's facts), and answers the final bytes of
  `{schemaVersion, dashboard, profiles, antigravity, view}` with the revalidated-view `ETag` and
  `x-aac-view: home@3`. Every byte is the one Node would write (`aac_views::js_json`: JavaScript's key order,
  string escapes and number text); a dashboard generation is written once as a template whose only per-request
  parts are `updatedAt` and the windows whose reset can pass, and a generation whose text does not round-trip
  byte for byte declines. TS writes the bytes unparsed (`sendRustView`, keeping Home's `no-store`), with a 304
  for a matching `If-None-Match`.
- **Anything unproven declines with a named reason and the original handler answers from the same dashboard
  read:** no service or another root (`root`), a page zone this engine does not agree with (`zone`: the
  server's offsets for the page's zone and day, cached per zone-day, must be the page's; the guard D7's server
  words gave), an input JSON cannot carry exactly or a failed read (`input`), a push that did not land
  (`feed`), a generation the service does not hold as named (`stale`: a push raced, or a new boot), and every
  service decline (a shape `markPassedResets` throws on, a missing fact, any view decline).
- **Generations are per boot:** a restarted service holds none, and the front door pushes again on the next
  request. Generation numbers start from the clock (ms x 1000), and a push that finds a newer one held moves
  past it, so a front door that attaches to a service an earlier process fed is not declined for long.
- **R1b, the clock-independent parts are kept** (`aac_views::home::memo`). For one held dashboard generation and
  one page zone and style, the service keeps the results of the view builder's pure calls that read neither `now`
  nor the per-request inputs: an account's visible windows, a window's labels, exact reset text, amounts and source
  line, a card's plan, platform and plan note, the dashboard's shape check, number texts and zone words. Each call
  site asks the memo with the same arguments at the same point, so the builder runs the same calls in the same order;
  objects are named by their exact address in the held generation (a table built per memo), and the memo is dropped
  with the generation (at most 4 per generation, 65,536 entries each). Per request only the clock-dependent words and
  decisions (relative times, reset-in, reset-due, stale, same-day, expiry passed) and the passed-reset holes are
  computed. The manifest is still read every request; its composition and the profiles' text are reused while its
  bytes and the Open generation are the same.
- **The Home ETag** (R1b, review L2) is the SHA-256 of everything the bytes are a function of: the build, the three
  generations' content digests, the manifest's bytes, the front door's per-request facts and the page's context. An
  equal tag means equal bytes, clock-named members included; the 200 KB answer is not hashed per request.
- **A window the per-response pass reads differently from its JSON** (an own `undefined` member, a `Date`, a
  non-finite number, a non-enumerable or inherited member) is refused per request (`input`, review L1); a push over
  the transport's 4 MiB cap is refused once per change key (`cap`, review L4).
- **Retained memory** (Wonder L8): a prepared dashboard holds its raw text and facts, its parsed
  value and its byte template (flippable windows twice) — about eight times its text for a
  string-heavy dashboard — plus its bounded view memos. Up to four dashboard sources are possible
  (`mac|windows` x `0|1`). Measured on Nas1 with four 3.69 MiB fixture dashboards pushed to the real
  release binary: +118.6 MiB RSS over the 5.8 MiB idle service (8.0x their combined text; the debug
  build nearly the same), so four sources at the 4 MiB cap can retain on the order of 128 MiB. The
  other sources hold their own bounded payloads; at most 16 sources are held. The measurement's
  synthetic opens fixture declines the view (`shape`), so the memos' own cost is not in that number;
  they are bounded per generation. Evidence: `2026-11-13-rust-rewrite/MEASURE-qwen-lane-2-feed-rss.json`.

The checks: `crates/aac-http/tests/lifecycle.rs` (the fed answer and its ETag, a missing, stale or unproven
generation, a late push, a restarted service), `aac_views::home::fed` and `js_json` unit tests,
`crates/aac-views/tests/home_spec.rs` (every Home case and the fuzz seeds through the fed path: equal or
declined), `tests/unit/web-server/home-feed.test.ts`, and `tests/spec/home-read-parity.mjs` (in `run.mjs`):
the fed path against today's handler on the real binary under Node, for every Home case, plus the 304, a push
during a request, a service killed mid-feed and an Open in progress. `tests/spec/bridge-measure.mjs --home`
measures off `GET /api/accounts/dashboard`, D7's Home and the fed Home in the same rounds on the large
synthetic home (whose 30 x 12 dashboard rows reach the real dashboard service through the additional-accounts
snapshot).

## Status, fed (R2)

R2 of the M3 completion design (`DESIGN-M3-COMPLETE.md` section 1.2): Status at the **fed** level. Off by default:
`rustService.groups.status: on` in `config.yaml` (or `AAC_RUST_GROUP_STATUS=on`), under `rustService.mode: on`.
With the group off nothing below runs and `GET /api/status` is today's handler.

- **StatusService keeps reading and collecting.** Its sources are pushed through R1's feed
  (`status/status-feed.ts`), each a JSON object of some of the composer's input members, as the envelope writes
  them: `status-reports` (reports), `status-quota` (quota, spendByStream), `status-pinned` (pinned),
  `status-collected` (registry, machines, browsers, simlock, usage), `status-answers` (answers, sends,
  answerHistoryProblem). `status-facts` holds the time zone and the fixed lists (hiddenThreadNames, machineIds,
  machineLabels) with that generation's `Intl` facts, computed once by `rustReadInput`'s code over the whole
  set: the clock and day words of every time value, the `Date.parse` epochs and the `localeCompare` ranks.
- **Change keys cost no serialization** where the owner replaces what it changes: each member is compared with
  what was pushed by identity, to the depth `readInputs()` builds fresh (the reports snapshot's arrays hold the
  reader's immutable rows; the collected panels are replaced per collection). The answers change in place, so
  their key is their JSON text. A changed member pushes its source and the facts.
- **A request carries only** the clock, now's clock and day words (two cached `Intl` formats) and the six
  generation ids (`x-aac-feed`; no body).
- **The service** (`crates/aac-http/src/status_fed.rs`) parses each generation once when it arrives (proving it
  re-writes to the very text TS sent and holds no `Object.prototype` name), composes with the existing Status
  composer (`aac_status::read_status::compose_fed`: the same code as the envelope's `compose`, over the borrowed
  generations and the facts plus now's date) and writes the bytes as Node does (`aac_views::js_json`) with the
  revalidated-view ETag and `x-aac-view: status@1`. The ETag is the SHA-256 of the identity bytes (everything but
  `generatedAt`, `ageSeconds` and `cacheAgeSeconds`) and is reused only for those same bytes. TS writes the reply
  unparsed (`sendRustView`, `private, no-cache`), with a 304 for a matching `If-None-Match`.
- **R2b keeps the clock-independent bytes** per the six generation ids and the request's clock and day words
  (`status_cache.rs`, at most 8 generation-sets and 4 word-pairs each). A request recomputes the ages, stale and
  due thresholds and the "full in" spans. When those decisions match, only `generatedAt` is filled in and the
  ETag is the one stored with those bytes. A new generation id is a different key. Fed Status reads use their
  own 4 blocking slots and wait up to 8 ms for one; the read pool's 8 slots are unchanged, and the wait is
  cancelled by the request's deadline.
- **Anything unproven declines with a named reason and today's handler answers from the same read:** another
  root (`root`), an input member the front door does not know or a value JSON cannot carry (`input`), a push that
  did not land (`feed`), a generation the service does not hold as named (`stale`; the next request pushes again),
  and every service decline (`shape` for a generation that does not round-trip, `date` for a missing fact or a
  clock `toISOString` cannot write, `unsupported` for a view the composer cannot prove).
- **Stays in TS:** the answers writer and the POST, the process collectors, the pinned-threads collector, the
  usage listener, the WebSocket and the report captures into the store.

The checks: `crates/aac-http/tests/lifecycle.rs` (the fed answer byte-equal to the envelope read with its ETag;
a missing, stale or unproven generation; a late push; a restarted service), `status_fed.rs` unit tests,
`tests/unit/web-server/status-feed.test.ts`, and under `tests/spec`: `read-view-parity.ts` (the 21 raw Status
cases, the 4 board ages and the prototype name through the fed path, plus a negative control),
`read-rounding-parity.ts` (the 37 numeric admissions through the fed path), `status-fuzz.ts` and
`status-read-parity.mjs` (end to end on the real binary and the real owner). `status-measure.mjs` times off, the
M3 envelope path and the fed path under Node on the large home (`read-http-parity.ts --measure` runs it).


## R3: store ownership

`rustService.store: on` (`AAC_RUST_STORE=on` overrides) under `mode: on` moves
the history writer and its periodic jobs into this service. Off is the default;
shadow keeps the NAPI writer. The boot validates the binding's compiled identity
before opening the store. The internal store-method route retains the kernel
single-writer lock and automatically rolls back to NAPI after five crashes.
See [store.md, optional Rust store host](store.md#15-optional-rust-store-host-r3)
for the method contract, caps, shutdown order, fixture drills and turn-on soak.

## M6 dashboard authentication

The Rust public front door is opt-in. Run the `aac-http` binary with `--public`,
an explicit `CCS_DIR` (or `CCS_HOME`), and a numeric
`AAC_RUST_PUBLIC_LISTEN` such as `127.0.0.1:0` for an isolated fixture. The
binary prints the actual bound address. The existing no-argument command still
selects the private service. On a host with a configured
`dashboard_tls.https_listener`, public mode also starts the native HTTPS
listener on its configured port when the certificate and key pass admission.

Public mode owns dashboard sign-in, sessions, setup, password and setting
changes, devices, the bar proof, and the `/ws` upgrade. It reuses the TS
session and device documents, the signed `connect.sid` cookie, and the
private-file registry. Set the same `CCS_SESSION_SECRET` in both processes if
the installation overrides the file secret. Stop the TS server before starting
Rust on the same private root. Rust refuses the TS session-owner marker.
Use separate private roots while comparing the two processes. TS keeps
ownership in its private-socket modes, including `on`.

To integrate the other public routes, supply the composed public router to
`DashboardAuth::protect`. Handlers read `DashboardAccess` from the request
extensions. The public router and the secret-bearing private Unix router have
separate entry points. Rust owners publish through `DashboardAuth::events()`. The standalone entry
point explicitly declines routes outside its ownership. See the producer table
below for the transitions that publish a hint.

The Windows auth fixture uses separate temporary roots, the real TS auth
routers, and the TCP binary. Run the following commands with a fixture binary.

```sh
cargo test --locked -p aac-http --lib
cargo build --locked -p aac-http
bun tests/spec/m6-auth-parity.ts /absolute/path/to/aac-http
```

Set `AAC_M6_OPENSSL` to the fixture's OpenSSL executable to exercise native HTTPS
and setup audit redaction. `AAC_RUST_PUBLIC_BINARY` opts the same HTTP fixture
into `tests/spec/run.mjs`. The parity fixture compares statuses and serialized
body order, plus security, cookie and limiter headers. It normalizes generated
identifiers, timestamps, cookie material and retry timing. It also checks a
TS-issued cookie and paired token across handoff and a Rust restart.


### Public dashboard event producers

| Wire event | Transitional TS producer | Rust owner handoff |
| --- | --- | --- |
| `accounts-changed` | Accepted account-usage collection, visibility and lifecycle writes | Publish `DashboardEvent::AccountsChanged {}` after the new snapshot or write is readable. Lane I owns collection. |
| `status-changed` | The Status service's existing coalesced notification | Lane C publishes `DashboardEvent::StatusChanged {}` on the same invalidation. Scoreboard has no separate TS WebSocket frame. |
| `analytics-changed` | Completed history sample or published activity snapshot | Lane D publishes `DashboardEvent::AnalyticsChanged {}` after publication. |
| `app-update-job` | Update all start, host phases and completed rows, cancellation and terminal state | Lane H supplies `AppUpdateProgress` to `DashboardEvent::AppUpdateJob`. The frame has task counts and host phases. HTTP still serves the full result. |
| `reset-claims-changed` | Existing successful claims and revocations | The owning Rust write calls `DashboardEvents::reset_claims_changed()` after commit. It assigns one id for all recipients. |
| `signin-job` | The existing sign-in runner's state changes | Lane G supplies the public `DashboardSignInJob` to `DashboardEvent::SigninJob`. Rust applies verification and email redaction per connection. |

The transitional launcher can pass an inherited private pipe to Rust. Start the
`--public` child with piped stdin and `AAC_RUST_PUBLIC_EVENT_STDIN=1`, then
attach the child's stdin with `attachDashboardEventFeed` in the producer
process. `startServer({ dashboardEventFeed: child.stdin, ... })` attaches the
same adapter for a TS fallback instance using its own private root.
`DashboardEvents::read_feed` accepts the stream directly in an embedded
composition. Start only one writer per private root, and detach or replace the
old pipe when the child is restarted. This pipe carries sign-in verification
data, so keep it out of logs, disk queues, and public routes.

Frames are newline-delimited JSON with a 64 KiB limit. Rust filters recipients
when sending. Only account hints reach an auth-disabled localhost socket.
Malformed frames are skipped. An oversized or incomplete frame ends the feed,
and a slow or ended pipe can miss hints. Clients can recover current state by
polling. Analytics and Update all previously relied on polling. Their new
events carry invalidation or progress only.

```sh
bun tests/spec/m6-events-parity.ts /absolute/path/to/aac-http
bun tests/spec/m6-public-contract.ts /absolute/path/to/aac-http --require-complete
```

The event fixture drives the TS adapter over the inherited pipe, a fake Update
all runner and the real Rust WebSocket. The public contract replay drives all
M0 route/role rows directly against Rust. Its strict mode fails while any
handler is unmounted or any reply differs. Set `AAC_M6_REQUIRE_COMPLETE=1`
with `AAC_RUST_PUBLIC_BINARY` to enable strict mode from `tests/spec/run.mjs`.

### M6 hub, dashboard assets and CLI

Compose the API with `auth.protect(auth.mount_edges(api, cache, ui_root))`.
Give account collection a clone of `HubCache` and call
`publish_dashboard_cache` with the cached dashboard DTOs in Mac, Windows
order after their normal passed-reset calculation. The facade reads those
published answers without collecting. The standalone front door can also
poll an absolute `AAC_RUST_HUB_CACHE` path at most once per five seconds.
Write a single dashboard or an array of dashboards atomically as owner-only
JSON with an `accounts` array on each dashboard. An unavailable or invalid
read retains the last published answer. The hub key stays in the existing TS
auth store and can be managed with the shipped dashboard usage-hub command.

Set `AAC_RUST_UI_DIR` to a packaged `dist/ui` for development, or set
`AAC_UI_EMBED_DIR` when building the binary for a self-contained install.
Supply the output from `scripts/build-ui.js`, including its build manifest.
An unpackaged development binary also looks for `dist/ui` in the checkout.
The front door uses strong per-encoding ETags for the verified precompressed
variants and keeps unversioned assets, the service worker and the app frame
revalidating. Static files skip session persistence. Page and hub responses
preserve rolling browser cookies.

The native `ai-account-center` and `ccs` binaries share the same command parser.
Set `AAC_RUST_DASHBOARD=1` to opt their dashboard launcher into the public Rust
listener. The shipped TS CLI continues to run dashboard by default and handles
the administrative subcommands and other existing commands. The native CLI
locates the shipped entry point next to the executable or through
`AAC_TS_CLI`. Release packaging can use the same native executable under
the five retired names to keep their guidance.
