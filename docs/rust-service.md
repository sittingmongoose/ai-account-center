# Optional Rust service (M3 read routes)

The TypeScript dashboard remains the public HTTP server. `ServerOptions.rustService`
(or `AAC_RUST_SERVICE`) selects `off`, `shadow` or `on`; the default is `off`.
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

## Boundary and lifecycle

The service binds no TCP port. It accepts only its Unix socket, mode 0600, in a
0700 random boot directory under the canonical AAC private directory's `run`
folder. TS refuses symlinked/nonprivate run directories. The Rust listener sets
0600 before it accepts requests and refuses existing socket paths.

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

Requests have a 500 ms total deadline, the response caps above and a maximum of 16
in flight per front door. TS owns HTTP encoding, cookies, caching, compression,
ETags and all existing auth/origin/session checks. On Windows, where this service
is not built, the TS fallback remains available. A failed boot is retried on the
next dashboard start; there is no restart loop or provider activity.

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
