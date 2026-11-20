# M0 behaviour contracts

Run from the repository root, with no provider refresh. After the server-only build,
copy the prebuilt native fixture modules described under M3 part 2 below; on/shadow
also require the prebuilt service. The runner never compiles Rust locally:

```sh
export AAC_NO_LOCAL_RUST_BUILD=1 TMPDIR="$HOME/aac-tmp"
bun run build:server
node tests/spec/run.mjs
bun test tests/integration/web-server/m0-spec.test.ts
```

The integration test runs each checker in an isolated process. Do not import
`capture.ts` into another test process: its filesystem, process and network mocks
are deliberately global. No Rust build, UI build, install or real home is needed.

To regenerate, review the diff before committing:

```sh
node scripts/foundation/generate-id-vectors.mjs
PYTHONDONTWRITEBYTECODE=1 python3 tests/spec/parsers.py --regenerate
node tests/spec/parser-ts.mjs --regenerate
node tests/spec/inventory.mjs --regenerate
bun tests/spec/capture.ts --regenerate
node tests/spec/run.mjs
node tests/spec/run.mjs
```

`routes.json` records the registrations made by the actual router factories,
including computed reset-claim paths and the Status regex. `roles` lists the
caller matrix, not an authorization grant. A session and a bar use real logins;
a device is paired through the actual device store; a hub presents a synthetic
key whose hash is seeded on disk. Signed-out calls have neither credential.
The bar is a session with the native probe nonce, not a separate server principal.
`ALL` is exercised with POST to a missing hub endpoint. GET registrations also
implicitly answer HEAD; automatic HEAD/OPTIONS and static assets are not separate
registrations. The middleware-only deliverable and WebSocket upgrade are explicit
inventory entries. Source fingerprints make changes to top-level and middleware
routing fail, even when no router registration is added.

The capture starts the real TS `startServer` on loopback with a temporary
`CCS_HOME`. `fixture.json` is copied there and supplies store/read-service seams.
Background maintenance, native store opening, Status/Scoreboard readers, process
broker, update executor, auto-switch and renewal are stubbed. Real route handlers
and authentication run. Process creation and external fetch/HTTPS/sockets are
blocked; filesystem mocks reject private and share paths outside the fixture and
repository. `os.homedir` is mocked without changing HOME. Guard probes run before
product imports. An early experimental capture exposed a legacy reader ignoring
CODEX_HOME; that output was discarded before committing any goldens.

JSON keys are sorted, arrays preserve their order, indentation is two spaces and
every file ends in LF. Only observed dynamic forms are replaced: the temporary
home and loopback origin, ISO UTC timestamps, UUIDs, `dev_` device ids and synthetic
`aacd_` tokens. Build-id fields, ETags and bar proofs use named placeholders;
session cookies retain attributes while their value and expiry are replaced.
Numbers and error messages are preserved. Selected headers include content type,
cache policy, location, retry, validators, cookies, bar proof and security policy;
transport Date, content length and encoding are excluded. WebSocket cases record
handshake status. Goldens compare bytes; unexpected files also fail the check.

Coverage limits: each route/role has one request, not every valid/error input.
Writes generally use `{}` and nonexistent resource ids. Dashboard and Analytics
reads deliberately use an invalid query to stop before collection. The store,
Status and Scoreboard responses use synthetic service results, so these pin wire
handling rather than native storage or view construction. No packaged WASM/static
asset bytes, WebSocket event stream, provider result, SSH action, installer,
interactive sign-in or successful reset claim is exercised. The fixture preserves
the Status-file 500 and reset-claim 503 outcomes. Full existing safety, HTTP and
provider fixture tests remain necessary.

`parser-vectors.json` pins 214 actual Python evaluations at a fixed clock across
Claude, desktop, plan, Muse, failover and Cursor parsers. TS has no corresponding
HTTP Retry-After parser in those helper paths; only comparable scalar boundaries
are annotated with actual TS results. Two plan numeric strings are accepted by
Python and rejected by the TS scalar boundary. `id-python-parity.json` also records
that Python returns null for an empty raw session id while TS hashes it. Neither
behaviour is changed. Shared vectors and generator are byte-identical to M1's
identity fixture. Event ids originate in native Rust; the TS checker pins its
source format and verifies u64 formatting with BigInt, without compiling Rust.

`test-inventory.json` is a conservative static classification of every discovered
test file (including Rust files with inline tests and browser checks). Evidence
labels may overlap, but each file has one primary kind. An internal test is marked
`uncovered` until a behaviour-level mapping is demonstrated; classification is
not a claim of behavioural coverage.

## M3 service parity

The M3 skeleton adds signed-in diagnostics (five new goldens). All original 405
request/role documents retain their bytes; numeric filenames after the insertion
shift because the runtime inventory remains sorted.

After `bun run build:server`, copy an `aac-http` binary built on Nas1 into
`dist/native/aac-http`. Never build Rust on the VM. Run the same full spec with
`AAC_RUST_SERVICE=off` and `AAC_RUST_SERVICE=on`, plus `AAC_RUST_SERVICE=shadow bun
tests/spec/capture.ts`. The on/shadow fixture allows only that exact binary, with
no arguments, through the closed process seam and requires actual Rust reply
counts. Existing role/session/origin checks stay real. After pair-2 M1, the M0
capture requires 27 Rust-served answers in on mode and 18 comparisons in shadow
mode. The twelve admitted standalone profile calls instead require explicit
`private_input` declines and zero Rust comparisons/serves for those routes. Their
HTTP response goldens stay the same: TS still answers them. The signed-out health
request carries an internal public-only envelope; it confers no private access.

`rust-proxy.ts` adds real fixture-socket fault injection: boot mismatch, invalid
JSON, oversized replies, non-200 status, total deadline, header replacement and
shadow divergence. Capture also verifies attachment, broken-socket fallback and
unchanged read/write guards. Source fingerprints now include the three TS bridge
modules and the shared ownership table. See [service details](../../docs/rust-service.md).

### M3 fix round 1: conservative numeric parsing

`settings-numeric-forms.json` is a 42-form shared regression table. The Rust
socket test requires either an exact integer DTO matching its pinned TS result
or an explicit 503 fallback. `settings-parity.ts` also executes the actual TS
file reader and production Rust bridge in off/on/shadow modes. It verifies
identical final answers, exact TS callback counts, and Rust reply/fallback
counters, including all six concrete cheap-review examples. It failed against
ce9d3eb61 on the lower-bound decimal (TS 60, Rust 30) before the fix.

The complete table runs from `run.mjs` in on/shadow mode on a Rust host; off
mode uses its `--ts-only` check. Run the actual table directly on Nas1 with
`bun tests/spec/settings-parity.ts` after `cargo build --locked -p aac-http`.
Rust accepts one typed integer DTO in 30..3600; all other file inputs delegate
to the TS owner rather than guess a default. The main fixture writes its default
setting through the real TS writer so ownership counters still prove Rust work.

For clean-copy checks, use the pinned dependency tree (Express 5.3.0), not
Nas1's obsolete shared Express 4 tree. The latter cannot implement the Express
5 wildcard spellings and returns HTML 404s for the page/hub fixture paths.
A source copy also needs its own Git index for `inventory.mjs`; a temporary
`git init` and `git add -A` supplies that index without altering shared Git data.
Route-source attribution is now explicitly relative to this checkout's `src/`,
so checkout names containing `src` cannot become handler paths.

### M3 fix round 2: raw object shape before DTO parsing

The shared numeric/document table now has 65 forms. Added top-level integer
arrays (including 30/60/90/3600), nested/multi-element arrays, scalar/null/boolean
roots, wrong-key objects, nested values, extra keys and escaped duplicate keys
all require raw 503 and exact TS callback/fallback accounting. Empty object and
extra-key controls remain included. The extended real TS/Rust table fails at
7ad39e524 on `[90]` (TS 60, Rust 90), before any fix is applied.

Every parsed service object DTO now passes a map-only, exact-unique-key gate
before its DTO deserializer is called. Original bytes are parsed again only
after that gate succeeds, preserving number spelling and duplicate rejection.
This covers settings, boot input, principal headers and raw route-table entries;
the route list is intentionally an array. Helper tests prove bad object
envelopes never invoke the DTO deserializer, and actual socket tests refuse
sequence/unknown/duplicate principal envelopes with 401.

## M3 part 2 checkpoint

`read-route-parity.ts` builds populated Status and Scoreboard views through the real readers,
plus bounded media and deliverable file inputs, and pins them in `contract/m3-read-snapshots.json`.
The full runner checks this document in addition to the original 410 role documents. In off
mode it uses `--ts-only`; on/shadow require the Nas1-built binary and assert actual candidate
reply counts, exact serialized bytes, mismatch refusal and explicit fallback for unsupported
inputs. This is a transitional snapshot protocol: TS still builds the reference for each read,
and only equal Rust candidates are admitted. Large views/files keep TS ownership.

An explicit `AAC_STATUS_MODULE` enables an eighth real-native-field parity case. It must
point at a compatible module already built on Nas1 or the Mac; the harness fails if it
cannot load or compose. The committed seven-case golden remains independent of optional
native module availability.


## M3 part 2 read views

The full runner also checks real Status/Scoreboard reader states, exact JS number
rounding vectors, guarded HTTP media/deliverable reads and an actual native fixture
store. Copy prebuilt `aac-http`, `aac-store.linux-x64.node` and
`aac-status.linux-x64.node` into `dist/native/` after the server-only build. Rust
builds run on Nas1 or the Mac, never the VM. No real store/share or provider is used.

```sh
export AAC_RUST_SERVICE_BINARY="$PWD/dist/native/aac-http"
AAC_RUST_SERVICE=off node tests/spec/run.mjs
AAC_RUST_SERVICE=on node tests/spec/run.mjs
AAC_RUST_SERVICE=shadow node tests/spec/run.mjs
```

The four `contract/m3-read-*.json` corpora supplement the M0 stubs. Raw views check
39 state/timezone combinations and serialized field order. Files check 29 HTTP
cases, including ranges, HEAD, MIME/security headers, role/origin/query refusals,
links and large-file fallback. Native HTTP checks all five routes with the same
writer kept open, by-model/group composition, ETag revalidation and no TS view
builder calls in on mode. Only observed fixture paths, timestamps, generated IDs
and hashes are normalized. The original role goldens retain their bytes.

For repeatable fixture measurements, use the release `aac-http` binary and run:

```sh
AAC_M3_BUILD_PROFILE=release AAC_M3_MEASURE_OUTPUT=/absolute/result.json \
  bun tests/spec/read-http-parity.ts --measure
```

Each fresh mode worker takes 40 warm HTTP samples per route after five warmups,
including full body consumption, and records TS plus its own Rust child RSS.
A separate Scoreboard case clears only the view cache before each request to
measure uncached composition with the existing ingested fixture store. Collector
scheduling, store ownership and TS front-door overhead remain in the measurement.
These are fixture measurements, not production capacity claims.


### M3 part 2 rounding regression

`read-numbers.json` now carries 120,992 Node-generated reference forms (checked against `aac_num::view`
by `crates/aac-num/tests/corpus.rs` since L3). It retains
the original precision/fixed cases and adds both neighboring doubles and the
center at every terminal x5 digit for decimal magnitudes 1e-23 through 1e12,
plus integer prefixes and the 2^51/2^52/2^53 transitions. Both signs, signed zero,
subnormals, scaled overflow, and the exact operation order of all seven rounding
expressions are included. Math.round rows retain input, operand and result bits.
The Rust test compares those bits, including negative zero, rather than applying
a tolerance. Fixed strings and toPrecision numeric results also require equality;
unsupported domains must refuse.

`read-view-parity.ts` invokes `read-rounding-parity.ts` on the accepted complex
fixture. It sends table-driven boundary batches through production
`RustService.compose` and `isRustStatusView`, requiring exact public serialized
bytes plus real served/legacy/fallback counters. The reviewed CPU/usage value is
checked in three timezones; GiB, quota values/words, coverage and budget percent
wording are also exercised. An unproven integer word requires explicit logged TS
fallback. The same table-driven test was run against unchanged bc3c1f496 Rust
sources and failed with 0.1 != 0 before the fix.


Keep global `AAC_STATUS_MODULE`/`AAC_STORE_MODULE` overrides unset for the full
runner. The real-native HTTP harness selects `dist/native/` modules explicitly;
M0 keeps its saved unavailable reset-backend fixture. Scope `AAC_STATUS_MODULE`
to the optional native-field command rather than exporting it into M0 capture.

## M3 part 3: account reads

`account-read-parity.ts` (with `account-read-fixtures.ts`) runs GET
`/api/accounts/{visibility,preferences,registry,analytics}` through the real TS
routers over HTTP in off, on and shadow modes. Every row must equal
`contract/m3-part3-account-reads.json` byte for byte in each mode. On mode must
serve each case marked Rust-eligible with no TS reader/composer call and log a TS
fallback for the rest; shadow runs TS exactly once per request, applies no
Rust-side effect and finds no difference. Analytics inputs travel in the shared
`rustReadInput` presentation envelope (ISO facts and provider-id ranks only). It then
compares `composeAccountAnalytics`
and `composeRegistryListing` with the Rust socket over 60 and 48 generated inputs,
and checks that the running TS accepts every zone Rust admits for preferences.

The fixtures cover 24 visibility and 26 preferences file forms (modes, symlinks,
folder bits, sizes, BOM, invalid UTF-8, lone surrogates, duplicate keys (answered as
JSON.parse answers them), migration,
companion merges, zones), five registry states and nine Analytics reads. Analytics
activity comes from the real v8 remote-cache code, in one concurrent four-computer
scan per pass (so it also pins that each computer saves only its own cell notes):
Mac reads fully (read); Windows reads fully, then partially (partial, cached with
the first pass's full-read age); Alexis only partially (no complete read); Nas1's
Claude folder is permission denied; OMP, Devin and others are not installed. The
M0 capture counts only `NNN-role.json` documents; named `m3-*.json` corpora are
checked by their own scripts. `run.mjs` runs the TS side in off mode and all three
modes when `AAC_RUST_SERVICE` is on or shadow. Regenerate with
`bun tests/spec/account-read-parity.ts --regenerate` and review the diff.


## M3 part 4 profile reads

`profile-read-parity.ts` runs GET `/api/claude/desktop-profiles` (and `/usage`),
`/api/codex/profiles` (and `/quotas`) and `/api/antigravity/profiles` (and `/quotas`)
through the real TS routers in off, on and shadow modes against
`contract/m3-part4-profile-reads.json`, plus part 3's visibility/preference files in forms
only a descriptor check can classify (FIFO, folder, oversized or dangling companion). Remote
history reads, the Codex collector and renewal reader, and the Antigravity runtime are
fixture closures; Codex logins are unsigned fixture JWTs in the temporary home. On mode
requires each `rust` case to be served with zero calls of that route's TS reader/composer;
shadow runs TS exactly once, keeps standalone profile facts out of the child (`private_input`),
and preserves the same public bytes. The generated tables below are explicit fixture protocol
tests; they do not change the production shadow admission policy.

Generated tables then compare each TS composer with the Rust socket reply byte for byte:
every plan label and alias parsed from `src/antigravity/plan.ts` with respellings, display
texts and secret patterns, timestamp shapes and offsets, random window/inventory/Codex
quota/summary/usage combinations, and manifest rows. A Rust 200 must equal TS; an input TS
throws on must be refused; `mayRefuse` marks inputs whose JavaScript meaning is TS's
(a legacy date parse, a coerced array, a history serde cannot prove). The M0 capture clears
the Codex summary memo before each request so its Rust reply count does not depend on timing.

```sh
bun tests/spec/profile-read-parity.ts --ts-only      # off mode only
bun tests/spec/profile-read-parity.ts                # off, on, shadow (copied Nas1 binary)
AAC_M3_MEASURE_OUTPUT=/absolute/result.json bun tests/spec/profile-read-parity.ts --measure
```

## L1: service lifecycle and the bridge floor

`rust-lifecycle.mjs` (run last by `run.mjs`) checks the lifecycle on the Nas1-built binary under
Node, on `dist/`: one kept-alive connection, restart after a kill, the build check on attach, a
root too long for a socket path, the sweep, named declines with the
`contract/l1-bridge-diag.json` golden, and feeds and events. With `--ts-only` (off mode) it only
proves no service starts. `--regenerate` rewrites the golden. The Rust side (deadline under a
preloaded slow read, the 8-permit bound, cancel, the identity header, short socket folders, the
service-only internal routes) is `crates/aac-http/tests/lifecycle.rs`.

`bridge-measure.mjs` times a built `dist/` under Node (see `docs/rust-service.md`); it needs no
Rust build and starts its servers inside the capture's closed boundary
(`bridge-measure-server.cjs`). `large-home.mjs <dir>` writes the large synthetic home it uses.
`--home` (R1) measures Home instead: off `GET /api/accounts/dashboard`, D7's Home and the fed Home in
the same rounds, with the design's server-time and RSS bars evaluated in the report.

## L5: agent lifecycle

`tests/spec/agent/lifecycle.mjs` is not in `run.mjs`. It needs the Nas1-built `aac-agent` and
`aac-http` plus a built `dist/`, and it is documented next to `tests/spec/agent/transport.py`:

```sh
node tests/spec/agent/lifecycle.mjs <aac-agent> <aac-http>
```

## M4a/M4b: launch and claude-host (lane F)

`launch-table.ts` and `claude-host-table.ts` generate the checked-in parity tables from the
real reference builders (`app-launcher.ts`'s `buildSystemdRunArgs`/`systemdRunEnvironment`,
`windowsHostScript`, `macOpenCommand`, `windowsOpenCommand`, `windowsAccountListScript`, the
fixed bootstrap and `claudeHostPurgePermit`). `--check` fails on drift; the Rust ports compare
every row byte for byte (`crates/aac-agent/src/launch.rs` and `src/claude_host.rs` table tests).
The launch table covers every app shape on linux/darwin/win32 with no launch; the claude-host
table covers every operation, the wide/José/no-start-menu launcher shapes and the spec's
refusal cases. `crates/aac-agent/src/claude_host/categories.rs` (the SAFE_PATH L/M/N ranges)
is generated from Node by the same claude-host table script.

`cargo test -p aac-agent --test launch_exec --features parity-fixtures` proves the execution
contract with a compiled recorder standing in for systemd-run (argv, environment, exit-code
fail-closed) and a detached marker child (full environment replacement, cwd, pid). No real
app starts.

```sh
python tests/spec/agent/claude_host_parity.py <aac-agent-with-parity-fixtures>
```

runs the reference Mac program and the verb on identical temporary homes (create, double
create, trash/restore byte-identical trees, permitted purge, undo, session markers, path
refusals, the ps needle rule) and the fixture-safe Windows steps (session, state, purge)
against the real `windowsHostScript` run locally behind the reference bootstrap. Purge
without the TS-issued permit, with a forged one and with an expired one is refused; the
shell-folder/task steps are refused in fixture mode and stay covered by the text table.
On POSIX the Mac section also compares modes; on Windows the reference's text writes are
emulated with POSIX newline semantics (its production platform). `tests/unit/web-server/
claude-host-agent.test.ts` checks the permit binding, the adapter reply validation through
the real TS fallback transport with a fake runner, and the labelled fallback with no
artifact. The Mac host-program spec tests (`claude-host-transport.test.ts`) and the loader
trampoline case in `app-launcher.test.ts` need `/bin/sh` and run on POSIX only.

## Native Store, Usage and Home

Run `node tests/spec/run.mjs --cut-rust-only --route-group=store-usage-home` with
`AAC_M6_TARGET_CLI` naming the native dashboard launcher. The group replays five routes across
the five caller roles. `tests/unit/store/store-native-routes.test.ts` exercises HEAD, browser
origin and device scope, validation, disabled-store replies and conditional requests on the
public listener. The historical store-usage test also reads its populated burn series over
that listener. Home browser checks cover the live route and compare the Rust view, DOM and
pixels with the browser's own rendering. `crates/aac-views/tests/home_spec.rs` covers the view
corpus and fuzz seeds.

## R2: Status, fed

Status and Scoreboard now replay through the native cut. Run
`node tests/spec/run.mjs --cut-rust-only --route-group=status` with `AAC_M6_TARGET_CLI`
pointed at the native dashboard binary. The historical view corpus remains in
`read-view-parity.ts`. The workerless browser fixtures cover real report and run
ingest, media ranges and answer submission.


## L3: one numeric layer (`aac-num`)

`crates/aac-num` is the one implementation of the number and date semantics the Rust side reproduces: ECMAScript
`Number#toString`, `JSON.stringify`, `Math.round`, `toFixed`, `toPrecision` and the `reduce` sum; the ISO
`Date.parse` forms, `toISOString` and civil-day math; `Intl.NumberFormat` en-US (two-fraction decimal, USD, short
compact), zone clock and day words, and printable-ASCII collation; CPython `round(x, n)`, `repr`, `json.dumps` float
text and the `datetime.fromtimestamp`/`fromisoformat` forms the helpers use. Each function states its domain and
answers `None` outside it.

The corpora in `num/` are written by the reference runtimes themselves and checked row by row by
`crates/aac-num/tests/corpus.rs`. Both scripts are seeded and deterministic; rerun them after changing them:

```
node tests/spec/num/make-node-corpus.mjs      # ecma.json, intl.json, dates.json, collation.json (records Node and ICU)
python3 tests/spec/num/make-python-corpus.py  # python.json (records the Python version)
```

Values cover signed zero, NaN and the infinities, the extremes and subnormals, both neighbouring doubles of powers of
ten and two, of decimal halves at every place and of the reviewers' witnesses, exact shortest-digit ties, eighth
steps near 2^38 to 2^53, random bit patterns over all of binary64 and human-sized decimals. The Python corpus adds
exact ties at 0 to 60 places, the Kimi and Z.ai `round(x, 8)` witnesses, and the far-future sign-in milliseconds
(8796093022210, 9999999999001 and their neighbours). The test also reproduces each deleted copy and shows where it
disagreed with its reference while aac-num agrees.

## M6 edge contracts

`m6-edge-parity.ts` covers the edges lane M moves: the T3 usage hub (`/v0/management/auth-files`, `api-call`,
`reset-quota`, every refusal), the dashboard's static files and Leptos bundle (frame pages and aliases, HEAD,
`no-cache` vs immutable `pkg/<build-id>/`, Brotli/gzip negotiation, ETag revalidation, `.br`/`.gz` never served
directly, the build manifest never served, `sw.js`/`sw-route.js`, the PWA manifest and icons, the root icon alias,
the `/api` JSON 404 and the unknown-page redirect), and the CLI entry points (help, version, `dashboard --help`,
the `config` alias, the retired self-update and profile guidance, and a retired legacy bin name).

The goldens are recorded from today's TS server in `contract/m6-edge.json` (39 HTTP, 7 CLI cases). Runtime data
(the hub answers, the JSON 404s, the redirects, the empty HEAD/304 bodies, the CLI output) is compared byte for
byte. A built asset is not frozen: 14 cases (the frame pages, the stylesheet, the pkg JS/wasm, `sw.js`,
`sw-route.js`, the PWA manifest, the icons and the root icon alias) require the served bytes to equal the packaged
file under the static root, so any rebuild or source change moves both sides together. Status, the selected headers
and the cache policy stay exact. The server half starts the real `startServer` on loopback on a temporary
`CCS_HOME` with the standard `fixture.json` seams, a real `dist/ui` built by `scripts/build-ui.js` with the fake wasm
pack, and a seeded management key. The hub's account list is the projection of `m6-dashboard.json` (three synthetic
rows), stubbed at `peekAccountDashboard` so no collector runs. The CLI half runs the built `dist/ccs.js` under Node
on a clean temporary home:

```sh
bun run build:server            # the CLI goldens run the shipped entry point
bun tests/spec/m6-edge-parity.ts                 # compare both halves with the goldens
bun tests/spec/m6-edge-parity.ts --regenerate    # re-record from TS, then review the diff
```

To run the same cases against another front door, start it on the same fixture home with the same
`m6-dashboard.json` rows and the same seeded key and set:

```sh
AAC_M6_TARGET_URL=http://127.0.0.1:PORT AAC_M6_STATIC_DIR=/path/to/dist/ui \
  bun tests/spec/m6-edge-parity.ts                                          # HTTP cases against the target
AAC_M6_TARGET_CLI=/path/to/other-cli bun tests/spec/m6-edge-parity.ts       # CLI cases as well
```

The target run discovers the build id from the frame page (`/pkg/<12 hex>/`), sends the same requests, compares the
recorded rows and, for a built asset, the served bytes with the packaged file under `AAC_M6_STATIC_DIR`. It refuses
to combine with `--regenerate`. The Rust CLI gate also invokes a temporary retired bin name.

For an isolated Rust run, let the fixture build the same UI and seed the same
management key and account rows:

```sh
cargo build -p aac-http --locked
AAC_RUST_PUBLIC_BINARY="$PWD/target/debug/aac-http" \
  AAC_M6_TARGET_CLI="$PWD/target/debug/ai-account-center" \
  bun tests/spec/m6-edge-parity.ts --rust-fixture
```

On Windows use the corresponding .exe paths. The full spec runner selects the
isolated Rust HTTP fixture when AAC_RUST_PUBLIC_BINARY is set. The
--regenerate option only records from the TS target.
