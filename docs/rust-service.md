# Optional Rust service (M3 read routes)

The TypeScript dashboard remains the public HTTP server. `ServerOptions.rustService`
(or `AAC_RUST_SERVICE`, or `rustService.mode` in `config.yaml`) selects `off`, `shadow` or
`on`; the default is `off`, and an absent or invalid value stays off.
`ServerOptions.rustServiceBinary` (or `AAC_RUST_SERVICE_BINARY`) can name an existing
`aac-http` binary. The packaged default location is `dist/native/aac-http`.
Starting the dashboard never builds, downloads or installs this binary.

| Mode | Answer and failure behavior |
| --- | --- |
| `off` | All routes use TypeScript. No Rust process is started. |
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
visibility and both preference files) goes through one helper, `private_file::read_capped`:
the name is opened once with `O_NOFOLLOW | O_NONBLOCK | O_NOCTTY`, every decision is made on
the opened descriptor (`fstat`: a regular file, its size), at most the cap plus one byte is
read, and a file that grew past the cap while it was read refuses. A FIFO, folder or other
special file swapped in is refused without blocking; `ELOOP` (a symlinked final name under
the canonical root) and `ENOENT` keep the owner's meaning per file. The TS owners got the
same hardening where practical: `readPrivateJsonFile` adds `O_NONBLOCK`, and the preference
reader reads through one no-follow, nonblocking descriptor after its `lstat`.

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
| Connection | One keep-alive `http.Agent` per boot (16 sockets), replaced when the boot changes. A request on a kept-alive connection the service had closed is retried once on a new one, inside the same 500 ms. |
| Route table | `rust-service-routes.json` rows carry `group`, `roles`, `budgetMs` and `handler`. Rust builds its dispatch from them (startup refuses a row without a known handler, a read without a budget, a public route open to the `service` role or an internal route open to any other); TS gates forwarding on the same roles. |
| Deadline | Each route's budget in Rust (400 ms for reads). Blocking work runs on two semaphore pools: 8 permits for reads, 4 for the store group. A full pool is a `busy` decline at once; a task that outlives its budget keeps its permit until it ends, so no more than 8 (or 4) blocking tasks ever exist; work whose request was dropped before it started never runs. The runtime caps blocking threads at 16. |
| Declines | One `Decline` enum (`aac-views`) sent as `x-aac-decline: <word>` on the private 503: `shape`, `number`, `date`, `zone`, `locale`, `collation`, `stale`, `busy`, `deadline`, `unsupported`, `internal`. TS counts every TS answer per (route, reason) — the service's word, or the bridge's own (`unavailable:down`, `unavailable:breaker`, `failed:deadline`, ...) — and logs `rust.service.route_state` with the route and reason on each change of a route's state. |
| Breaker | Per route in TS: 3 transport failures or timeouts in a row open it for 30 s, then one probe goes. Declines are replies and never count. Admission only looks; the probe is reserved when the request is really sent, so a request TS keeps (no input, another root) never holds it. A new boot starts with every breaker closed. |
| Restart | An owned service that exits is restarted after 1 s, 5 s, 30 s, 2 min, then 10 min; after 5 restarts in an hour TS logs `rust.service.gave_up` and answers alone until the next dashboard start. An attached boot that stops answering is dropped the same way. A process that accepts but never answers (stopped, deadlocked) is caught by its transport timeouts: after 3 in a row on any route, a health probe (500 ms) decides, and a hung process is retired into the same restart budget. Rust's own `deadline` and `busy` declines are replies and never count. Boot folders older than a minute whose socket refuses a connection are swept at start; no PID read from disk is ever signalled. |
| Build check | Health replies carry `x-aac-service-build: {commit, sha256, protocol, storeSchema}`, with `sha256` the running image's own hash. The descriptor records it. A front door attaches only when the live identity equals the descriptor's, its `sha256` is that of the binary it would start, and the protocol is its own; otherwise it starts its own boot (an in-place upgrade). The public `/api/health` body stays `{"status":"ok"}`. |
| Feeds, events | `PUT /internal/feed/<source>` `{generation, data, facts}` and `GET /internal/feed/<source>`; `GET /internal/events` streams NDJSON (`facts-needed` when a generation arrives without facts). Only the reserved `{kind: session, id: front-door, role: service}` principal reaches them. A metadata-only stub until R1: the service keeps the generation, the data's size and whether facts came, not the payload. |
| Shadow | At most 2 comparisons in flight per route; the rest are skipped and counted. TS answers first, then Rust is asked, so each comparison times both paths on the same request. |
| Reply floor | `view_reply` in Rust and `sendRustView` in TS: the final bytes, the revalidated-view ETag and `x-aac-view`, written without a parse. No route uses it yet (R1 onward). |

**Measurement hooks.** `GET /api/diag?bridge=1` (signed-in session) answers the skeleton
diagnostics plus a `bridge` block: the service state, its build, restarts, the child's RSS,
the counters and, per route, the paired timings (TS path and Rust path on the same requests,
from shadow) and Rust-only timings (on). Every minute with traffic, `rust.service.minute`
logs the same per-route block for that minute, and the event-loop monitor's line adds
`rustMode` and `rustRssBytes`. Response bodies are never logged.

`tests/spec/bridge-measure.mjs` measures a built `dist/` under Node: off and on servers in
alternating rounds (5 x 40 samples per route) with the load average around each round, a
16-client, 60 s concurrency row with the server's event-loop p99, RSS of TS and the Rust
child, shadow paired timings, and (`--heap`) the heap the Status and Scoreboard owners hold
on the large synthetic home (`tests/spec/large-home.mjs`: 50 profiles, 200 reports, 5,000
runs, 30 accounts x 12 windows x 90 days). The worker server runs inside the same closed
boundary as the contract capture.

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


All newly ported Math.round sites use one binary64 helper: compare the exact
fraction with 0.5, preserve ties toward positive infinity and negative zero, and
leave already-integral values unchanged. Adding 0.5 before floor is forbidden
because the addition can cross a rounding boundary. toFixed/toPrecision continue
to round the exact binary significand within their admitted domain. Integer words
outside the proven decimal formatting domain request logged TS fallback. The
production bridge/Status-validator regression is driven by the Node-generated
number table; it catches the pre-fix 0.049999999999999996 -> 0.1 wrong answer.
