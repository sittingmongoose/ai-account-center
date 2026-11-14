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
counts. Existing role/session/origin checks stay real. The signed-out health
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
export AAC_STATUS_MODULE="$PWD/dist/native/aac-status.linux-x64.node"
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
