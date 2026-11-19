# Agent verb parity

## Scan usage

The workspace dev/test profiles disable debug symbols, so default Cargo test
commands stay below the existing 64 MiB artifact guard without environment
overrides. Release settings and the artifact guard are unchanged.

`quota-read`, `signin-expiry` and `scan-usage` share the same verified transport
and `agentVerbs` switch. Each verb retains its own reply validation and fallback
boundary; quota results still bind provider, platform and account before use.

`aac-agent` uses the existing `native/aac-usage` library through a path dependency.
The verb's `args` are the standalone scanner request. Its terminal `value` is the
standalone reply, including discovery, fingerprints, identity and messages. No
scanner code, text/numeric parser or discovery rule is copied. The shared scanner
parser defines this already-shipped grammar; M2's separate conservative Python
grammar and ASCII guards remain unchanged for sign-in/inventory.

Build both binaries on Nas1 or Mac, then run the existing M2 harness:

```
python3 tests/spec/agent/run.py target/debug/aac-agent --scan-bin native/aac-usage/target/debug/aac-usage-scan
```

This also runs the unchanged discovery and Antigravity helper fixture suites.
Each standalone scan in their existing parity hook now invokes the agent and
compares the raw JSON bytes of its result value with the standalone scanner's
stdout. Unlike the older Python comparison, this new comparison removes no
fields, reorders nothing and rounds no floats. Exit success, terminal type,
protocol and correlation id are checked. The historical 45-fixture suite has
grown; the gate reports the current tests and number of exact byte comparisons.
`_scanFixture` injects only a temporary home and environment into fixture builds;
production rejects it. The agent process HOME is never changed by this hook.

Agent scan frames retain discovery's 8 MiB cap; other verbs retain 1 MiB. Scan
work runs off the async executor, with the scanner's existing watchdog budget
and stalled reply, while agent cancellation/deadlines remain active. The entry
point exits after its terminal event, ending any blocked scan thread.

TS local and remote scans select the hash-pinned agent only with `agentVerbs: on`
(default off), falling back on any execution/protocol/validation failure to the
existing scanner/helper path with the log label `fallback: aac-usage-scan`.
Existing Python/TypeScript fallback labels and reader/cache identities stay intact.
The artifact catalog remains empty; no release binaries are shipped by this task.
Transport upload/verify/rename/handshake tests use fake SSH/SCP only, including
scan frames over 1 MiB, upload-once, corruption and handshake rejection.

Run the unchanged Python suite while capturing its real `collect`/`fix_command`
calls: `python3 tests/spec/agent/capture-signin.py`. The table lists all 90
source tests, including helper/safety checks that still run in Python; 96 calls
become agent requests. Synthetic credentials only.

`bun tests/spec/agent/capture-inventory.ts` extracts literal machine probe
requests from the status collector tests and evaluates the real TS
parser. `bun tests/spec/agent/inventory.ts /path/to/aac-agent` can re-evaluate
these TS expectations against a fixture-enabled binary. Inventory has no prior
Python helper; its reference is `status-collectors.ts`.

Build **on Nas1 or Mac only** with `cargo build -p aac-agent --features
parity-fixtures`. Run `python3 tests/spec/agent/run.py target/debug/aac-agent`
there. This re-executes the original Python helper for every sign-in request and
compares Python, the committed golden reply, and the agent reply; any difference
fails and prints all three. It also compares inventory against the TS goldens.

Normalization is limited to JSON object key order, relocation of fixture roots
(including settings text, symlinks, working directories and recomputed Keychain
service hashes), and Python lone-surrogate filesystem spellings rendered as
U+FFFD at Rust's UTF-8 wire boundary. Array order, fields, nulls, states, repair
commands, errors and millisecond timestamps are compared. The clock is fixed.
No discrepancy is suppressed. Original Python replies also equal the golden
reply before surrogate normalization.

Only OS boundaries mocked by the source tests are replayed: file read/stat
outcomes, injected discovery pairs/failure counters and Keychain service replies.
Ordinary discovery still enumerates the recreated fixture files. The production
binary refuses these injection arguments. `home` always disables real Keychain
and inherited selector reads in the production verb. No fixture process reads
real private homes or contacts a provider.

`agentVerbs: on` in config.yaml (or `AAC_AGENT_VERBS=on` for isolated checks)
enables the TS adapter. Absent/invalid values stay off. The trusted package
catalog `src/web-server/services/agent-artifacts.json` deliberately has no
artifacts until an authorized packaging release supplies reviewed per-target
hashes and binaries; this task neither ships nor installs them. A missing or
hash-mismatched binary keeps the labelled `fallback: python`; an executing
agent's failure remains a failure. Existing foundation fake-transport tests
exercise upload, verification and handshake without contacting real peers.

## Fix round 1: conservative delegation

The current contract supersedes the original failure behavior above: **every
agent failure** (spawn, exit, verification, timeout, protocol or explicit
fallback result) invokes the labelled Python helper through the same collector
boundary for local and SSH hosts.

`proof.rs` centralizes the grammar boundary. A verb returns exactly
`{"fallback":"python"}` in its normal terminal result whenever it cannot prove
an input belongs to the supported subset. It never publishes a partially parsed
answer. JSON must parse under serde's bounded nesting, contain only exact-range
integers as numbers, and have no non-ASCII numeric characters in strings. Invalid
UTF-8, lone JSON surrogate escapes, nonstandard constants, fractions/exponents,
overflowing values and inputs beyond their file cap delegate. Expiry strings use
ASCII digits/ordinary ASCII spacing. Inventory accepts plain finite decimal
numbers in the exact-integer range; hex, exponent, signed-plus, Unicode numeric
and excessive numeric forms delegate. It deliberately does not implement all
Python JSON or JS Number quirks. Credential documents use 65,536 bytes; settings
use their own 1,048,576-byte cap and optional UTF-8 BOM.

Regenerate with the original capture scripts followed by
`python3 tests/spec/agent/review-cases.py`. The latter records explicit delegation
expectations independently of the agent and adds real-file review cases. Compare
105 sign-in cases (including 9 direct-file regressions) and the inventory cases
(including 6 numeric-boundary regressions). A fallback case must return the exact
fixed value, while Python still must equal the recorded reference output.

For a separate production-path witness, build without `parity-fixtures`, then run
`python3 tests/spec/agent/run.py target/debug/aac-agent --production-only`. These
9 cases recreate real credential/settings files without injecting IO. The
harness validates exit 0 plus terminal type, protocol and correlation id before
comparing any value. `python3 tests/spec/agent/harness_test.py` rejects a matching
value with wrong terminal metadata or a nonzero process exit.

Coverage limits: the original 96 captures inject credential read outcomes and
therefore do **not** prove production credential opens, races or permission
behavior. The direct-file suite covers the review's document grammar and cap
cases, not every filesystem race or hung mount. Inventory parser cases do **not**
exercise real tool presence, subprocess probes or native Mac/Windows behavior.
Keychain cases use injected responses; no real Keychain is tested. Transport
failures use fakes, not real SSH hosts. These are explicit limits, not claims of
exhaustive Python/TS parity. Unsupported input classes delegate to the reference.

## Fix round 2: ASCII before semantic parsing

The shared text allowlist is printable ASCII (`0x20..0x7e`) plus exactly HT, LF,
VT, FF and CR (`0x09..0x0d`). Those five characters plus SP are the only trim/split
characters. Numeric fields additionally require the decimal grammar described
above; expiry strings retain their narrower numeric allowlist. JSON string keys
and values, paths, selectors and raw inventory text are checked before semantic
processing. Unknown inventory keys receive the same raw guard. Unsupported text
returns the whole-verb fixed fallback. Unicode-aware trimming, splitting and NFC
normalization have been removed from verb parsing; lexical ASCII path resolution
is retained. Non-ASCII profile names and paths now delegate to Python as well.

`whitespace-controls.json` freezes Unicode whitespace plus Cc/Cf control points
(254 points), positions, fields and allowlists. Regenerate with
`python3 tests/spec/agent/make-whitespace-table.py`. It is a compact Cartesian
property table rather than tens of thousands of duplicated golden objects.

Run on the VM (synthetic text only):

```
python3 tests/spec/agent/whitespace.py capture /tmp/aac-whitespace-input.json
bun tests/spec/agent/whitespace-reference.ts /tmp/aac-whitespace-input.json /tmp/aac-whitespace-reference.json
```

Copy the generated reference file to the build host and run there:

```
python3 tests/spec/agent/whitespace.py /path/to/fixture-agent /path/to/aac-whitespace-reference.json
python3 tests/spec/agent/whitespace.py /path/to/production-agent --signin-only --production
```

The first run checks all three inventory platforms, all 15 reading keys/values,
and 12 sign-in/settings/credential/argument/selector fields at start/middle/end.
The real TS parser computes inventory references; the original Python helper
computes sign-in references on real fixture files. Every response must be the
reference answer or exactly `{"fallback":"python"}`; a differing ordinary value
or invalid terminal envelope fails. The second run omits the two fixture-only
selector injections and checks the other ten fields without parity features.
Inventory cases use bounded batches of 256 through a fixture-only adapter to
the same parser; production refuses this argument. Sign-in cases run one process
at a time against fixture files, with one Tokio worker per process. `--baseline` selects the recheck witnesses from this same
table to demonstrate failure at `b23d68d38` without repeating the full corpus.
These checks retain the filesystem/native/transport coverage limits above.

## M2 plan quotas

`python3 tests/spec/agent/quota.py /path/to/fixture-enabled/aac-agent` starts one
loopback HTTP fake, then runs the unchanged Python `plan_usage.collect` and the
agent against the same response queue. All homes and tokens are synthetic.
It checks complete DTO equality or the exact whole-verb fallback, terminal
protocol/id/type/exit, request headers and Qwen GET/POST sequence. JSON numeric
integer/float spellings compare by numeric value. No fields or states are dropped.

The table includes all four providers, HTTP 401/403/429/5xx, integer/date/invalid
Retry-After, timeout, malformed/oversized bodies, fractional and Unicode grammar,
Qwen international console requests, and Unix AAC-key account selection. The Rust
unit test also consumes all 43 `plan_common` M0 vectors, explicitly delegating the
two frozen Python/TS disagreements. Neither reference implementation is edited.

The supported grammar is deliberately conservative. OMP SQLite, DPAPI and Safari
stores, Qwen CN/CSRF sessions and optional Qwen payload layouts, Kimi booster or
overlapping legacy/modern readings delegate. JSON fractions/exponents, non-ASCII
text, decimal or padded numeric strings, unsupported reset spellings, and large
Z.ai count key spellings delegate too. No fallback means an empty/zero quota.
The production binary refuses fixture endpoint/clock overrides. The endpoint
injection is feature-gated and permits only literal `http://127.0.0.1` with an
explicit fixture home. Native Mac/Windows execution and live TLS providers are
not covered. The artifact catalog stays empty pending authorized packaging.

### Quota fix round 1

`quota-review.json` records the review's provider objects and the exact six-slice,
one-second-interval HTTP stream schedule. Run
`python3 tests/spec/agent/quota-review.py /path/to/fixture-agent` on the build host.
The original Python helper and the full quota verb use the same loopback replies
and synthetic fixture credential store. Every case validates the process exit
and terminal envelope, compares the complete reference DTO or required exact
fallback, and rejects reflected dummy credential text. `--baseline` expects the
regressions to fail at `4e3e77c62`; it never uses real credentials/providers.

Qwen unwrapping now uses a typed Result instead of provider-controlled status
keys to distinguish internal errors. Unused upstream status/message fields are
ignored. Every published textual DTO value passes `aac_core::scrub`; modified,
redacted or uncheckable text is dropped in favor of the fixed word `error`.
All public DTO keys are constructed locally, not copied from provider objects.
The boundary has a dedicated regression for nested fields and scrub markers.

The proved ISO string subset is exactly `YYYY-MM-DDTHH:MM:SSZ`, with seconds
00..59. Other string shapes, including lowercase/leap-second/offset forms,
delegate rather than extending Chrono to emulate Python. Existing numeric
seconds/milliseconds retain their proved grammar. Boolean Z.ai response codes
in either quota or pack envelopes delegate. Any uncertain HTTP request, body or
deadline failure delegates; normal known HTTP status replies retain their fixed
messages. The four-second reqwest total timeout is not claimed to equal Python's
per-socket timeout: a continuously progressing five-second response is explicitly
a whole-verb fallback, after which TS uses Python.

## M2 part 3: pins, keys and Claude history

Run the part 1 harness with `--part3` against a fixture-enabled Nas1/Mac build:

```sh
python3 tests/spec/agent/run.py target/debug/aac-agent --part3
python3 tests/spec/agent/part3.py /path/to/production/aac-agent --production
```

The additional runner executes the unchanged key-store and history Python tests.
It intercepts their real fixture-home requests and compares the Rust terminal
reply on those same files. Key writes additionally restore the initial fixture
and compare every resulting file byte, mode and symlink, including absence of
atomic-write scratch. The clock is fixed. Python exceptions match a classified
agent refusal or exact whole-verb fallback, never an ordinary success. The
production run repeats key/pin cases without fixture features and checks that
history fixture injection delegates. It does not claim native Mac/Windows proof.

Pins use `crates/aac-status` as a path dependency without its Node feature:
one read-only SQLite cursor, five-second busy timeout, 1,000 rows and 1 MiB of
pin data. Its remote entry point preserves all nine original SQL columns,
including child rows, scalar types, and absent versus explicit-null type facts.
The existing local read and remote entry point share that cursor; neither SQL
nor parser is copied into the agent. Parity compares complete JSON rows with
the exact TS SQL query on the same fixture database. It drops no fields, sorts
no arrays and rewrites no metadata. Tests verify no database bytes change or
sidecar files appear. The existing native parser and machine merge stay in
place on the TS side. Native reader error words are preserved in
`Failure.reason`; an unclassified `io` is not guessed to mean a lock or a
permission error.

Unix keys preserve the Python record path/format, key-directory ownership and
0700 mode, 0600 file, exclusive temporary file, file fsync and atomic replacement.
Only the response fingerprint and last four characters leave the verb. Windows
DPAPI is explicitly unqualified and delegates before reading or writing a path.
The dashboard's in-process local key store stays as-is; the remote store selects
the verified agent through the existing `agentVerbs` switch.

History supports bounded index collection, closed checks and protected checks
without new candidates. Native Mac collection requires the exact existing app
version and ASAR member digest, with no active Claude process. Apple's fixed
`plutil` reads the checked Info.plist bytes over stdin; it receives no path from a
request. Active processes (whose argv/alias guard is not ported), Windows probes,
unproven inputs, candidate-bearing checks, transcript verification, append and
snapshot cleanup delegate as a whole. The SHA-pinned Python/Node transaction and
all durable pending-marker/receipt rules stay authoritative for writes. No native
history result claims writer quiescence or acknowledges an append. The TypeScript
adapter delegates write modes before launching an agent.

`_historyFixture` is a feature-only seam for a synthetic home, closed state and
native guard receipt. All index/protection files still use real bounded,
no-follow filesystem reads, directory identities, hashes and stable snapshots.
Source-test mocked IO/races delegate explicitly. Original transaction, bridge,
microbatch and pending-marker tests continue to exercise the unchanged writer.
The proven-grammar and ASCII-only boundary applies before text processing.
Additional real-file witnesses cover VT/FF/CR SSH directives and a dangling
`waiting-input` symlink. No private home, provider or T3 database is used.

All three TS adapters run the current helper after any unavailable artifact,
explicit delegation, timeout, exit, malformed envelope or invalid result.
Fallbacks are labelled `fallback: python` (`fallback: sqlite3` for pins).
The artifact catalog remains empty; this change does not ship or install agents.

## M2 part 4: failover inventory

`python3 tests/spec/agent/run.py target/debug/aac-agent --part3 --part4` adds
`failover-inventory` same-home tests to the existing M2 harness. Run quota's
`quota.py` and `quota-review.py` separately as before. Build with
`--features parity-fixtures` after any `cargo test`; the latter rebuilds the
agent without the fixture feature. `part4.py` also runs unchanged against a
production binary, with no injected filesystem, clock, platform or native receipt.

The agent accepts the existing inventory request on stdin, reconstructs only
locator/selectors/identity/revision/email/plan, and makes no network calls.
Linux Codex/Claude CLI default, saved and T3/settings homes; Muse native/config
homes; OpenCode plan keys, environment keys, private AAC keys; and Qwen cookie
session identities have a proved subset. Original length limits apply before
trimming, and the public dispatcher validates display metadata again. Core files
and settings use 1 MiB, additional auth JSON 64 KiB, and AAC key records 4096
bytes. Core directory discovery bounds 1024 raw entries, 64 homes and 256 roots;
additional slot listing bounds 512 entries and 128 candidates. JSON numbers use
the shared exact-integer grammar. Arrays and every hash/field/null keep their
reference meaning and order; no reference field is dropped for comparison.

All unsupported grammar and uncertain reads delegate the entire inventory with
exactly `{"fallback":"python"}`. This includes symlinks, IO/owner/shape/cap
failures, non-ASCII fields, unproved numbers, SQLite/OMP, Cursor, Antigravity,
wallet/Muse/Qwen browser capsules, Claude desktop context and all native Mac or
Windows execution. The existing Python helper retains its full behavior and
incomplete/sign-in-needed selectors. A failed native read never becomes an empty
ordinary inventory. The TS boundary uses the existing opt-in verified transport,
existing public response parser, provider-kind binding and caller deadline;
collection remains with its current helper. Every failure or fallback is labelled.
The trusted artifact catalog remains empty and the default remains off.

Part 4 fixture rows explicitly declare equality or exact fallback, including
valid nonempty Antigravity and wallet stores that delegate. The runner compares
unchanged Python on the same files, checks file bytes/modes are unchanged,
validates the terminal envelope/exit, and rejects a changed-answer negative
control through the same comparator. These are bounded representative fixtures,
not exhaustive filesystem-race or native-platform proofs. Provider HTTP, real
homes and credential stores, deployment and actual SSH agent execution are not
exercised. The adapter's fault tests use a fake verified SSH transport.

## Big review fix round 1

All file reads of signin-expiry, failover-inventory, quota-read and claude-history go
through `crates/aac-agent/src/files.rs`: one no-follow, nonblocking, identity-checked,
capped descriptor read, and one path rule (a leading `//` delegates; failover normalizes
an absent `homeDir` the way it normalizes an explicit one; the other verbs delegate a home
spelling Python's pathlib would rewrite). Each verb still maps an outcome to its own
reference. Witnesses:

- `quota-review.py`: Kimi rounding ties and their neighbours through the whole collector
  (CPython `round(x, 8)` is half-even on the exact binary value), and plan credentials
  behind a final link, a link below the home or a FIFO (fallback, no request sent).
  `python3 tests/spec/agent/make-round-table.py` rewrites `python-round8.json`, CPython's
  own answers for the Rust unit test.
- `deadline_review.py <aac-agent>` (Linux, needs `cc`): `slow-read.c` is preloaded so the
  first read of one fixture file sleeps 3 s. A 100 ms `deadlineMs` must answer a timeout or
  the fallback within 1.5 s for signin-expiry and quota-read; the long-deadline control
  proves the delay happened. `--baseline` expects the old late answer.
- `part4.py`: `key-twins-*` (Python cuts slot names to 128 before deduplicating) and
  `default-home-*` (the environment's home spelled with `//`, `/.`, `/`, `/..`).
- `inventory.json`: `review-big-L6-cpu-grouping` rows from the real TS parser.

## Big review fix round 2

signin-expiry reads the T3 settings file and its sibling homes by Python's
`failover_core` rules (`_local_path`, `_read_present`, `SAFE_NAME`), through the same
`files.rs` helpers:

- **Settings path:** a link anywhere below the home (`.t3`, `userdata` or
  `settings.json`, dangling, looping, in or out of the home) delegates the verb. A file
  another user owns, a folder, a FIFO or an unreadable file marks discovery incomplete,
  as Python's `read_failed()` does; a missing one is silent.
- **Configured homes:** admitted by spelling alone, as Python does; one whose real path
  leaves the home publishes its own "outside the allowed roots" row.
- **Sibling homes:** names follow `SAFE_NAME` exactly. A linked entry takes one hop of
  `_local_path` (`files::hop`): inside the scope it is a sibling home, outside it joins
  from the collector's own scan, and a second link delegates. A linked `.claude-t3` or
  `instances` folder that leads to a folder delegates.
- **Ambient selectors:** one that is not UTF-8 delegates.

`signin_review.py <aac-agent> [--production] [--foreign-root DIR]` checks 41 real-directory
witnesses, Wonder's five among them (their Python rows are pinned). Each shape must give
Python's answer or the fallback, as listed in `SHAPES`. The two ambient witnesses send no
`home`, so the agent reads HOME and the selector from its own environment. `--foreign-root`
names a folder whose new files have another owner (an NFS export that maps every file to one
user); without it the owner witness is skipped.

## One file policy (L2)

Every verb reads, lists and writes through `aac-files` (`aac_files::read`, `aac_files::path`,
`OwnedStore`); `crates/aac-agent/src/files.rs` keeps only the agent's grouping of a `Read`
outcome (`Regular`) and the decline recorder. Every count above is unchanged.

`key_store_review.py <aac-agent> [--baseline]` runs real key-store puts under `sync-log.c`
(LD_PRELOAD; Linux and `cc`): the staged file is fsynced and, after the rename, the keys folder
too; killed at the data fsync the committed key is whole, killed at the folder fsync the new key
is whole; a held folder lease refuses the write; answers, bytes and modes equal
`key_store.put`, and no lock file joins the folder. `--baseline` expects the base's
behaviour (no folder fsync, no lease).

## Lane E: packaging and per-family turn-on

M3D4 (ship in stages, VM first) and M3D5 (the VM's current key, AAC-pinned known_hosts)
are answered. The trusted catalog `src/web-server/services/agent-artifacts.json` carries
one reviewed pin per built target; `scripts/build-agent.js` (`bun run agent:build`) builds
`aac-agent` on each build computer (Nas1, the Mac, aac-winbuild; never the VM) into
`crates/aac-agent/prebuilt/<target>/` with a source-hash note, `--copy-only` carries the
matching targets into `dist/agent/` during `build`, and `--catalog-entry` prints the entry
the lead reviews and commits. `scripts/verify-bundle.js` checks every shipped agent against
its manifest and every catalog pin present in the build. Linux builds are glibc until the
musl target is authorized (F11); the adapter and the service admit both on Linux.

The service still admits only an already installed agent (`AgentClient::existing`); it never
uploads one. The lead places the binary on each remote computer with `scripts/agent-install.sh`
(Nas1, Mac) or `scripts/agent-install.ps1` (Windows): source digest check, staged `.part`,
re-check, atomic rename into `~/.cache/ai-account-center/bin/aac-agent`
(`%LOCALAPPDATA%\ai-account-center\bin\aac-agent.exe`), then the `--version` handshake for
that OS. The VM is Local: the packaged `dist/agent` binary runs in place, nothing to install.

`agentVerbs: off|shadow|on` is the ceiling; `agentVerbFamilies` (or `AAC_AGENT_VERB_FAMILIES`,
JSON) sets one mode per verb under it. An absent map keeps the scalar behavior; a present map
enables only its listed families; a present but invalid map keeps every family off. The
read-only families turn on one at a time as their parity suites pass: `signin-expiry`,
`quota-read`, `failover-inventory`, `t3-pins`, `scan-usage`. `key-store` and `claude-history`
write on the computer, so their shadow is off (shadow never duplicates a write).

In `shadow`, the reference answer is served as soon as it settles; the agent runs alongside
and the comparison is logged as `agent.verbs.shadow` (verb, outcome, agentMs, referenceMs;
never bodies). Outcomes: `equal`, `different`, `fallback` (agent declined), `error` (agent
attempt failed), `reference-failed`, and `reference-fallback` (a scan whose reference is the
Python helper, not the scanner). Object key order is the only normalization. The live gate
per DESIGN 4.5 stays 72 h with 0 `different` and agent p50 at most half of Python's; the
fixture suites below are the lane gate, not a substitute for that soak.

Per-family fixture gates (fixture-enabled Nas1/Mac debug build, unchanged harnesses):

```sh
cargo build -p aac-agent --features parity-fixtures
python3 tests/spec/agent/run.py target/debug/aac-agent                 # signin-expiry + inventory
python3 tests/spec/agent/run.py target/debug/aac-agent --part3         # + t3-pins, keys, history
python3 tests/spec/agent/run.py target/debug/aac-agent --part3 --part4 # + failover-inventory
python3 tests/spec/agent/quota.py target/debug/aac-agent               # quota-read
python3 tests/spec/agent/quota-review.py target/debug/aac-agent
# scan-usage: build native/aac-usage's scanner, then
python3 tests/spec/agent/run.py target/debug/aac-agent --scan-bin native/aac-usage/target/debug/aac-usage-scan
```

`tests/unit/web-server/agent-verb-families.test.ts` covers the family map, the ceiling cap,
invalid-map safety, the write-verb shadow refusal, glibc admission, and the shadow
comparison outcomes at the collector and scan boundaries with fake closures only.

Limits unchanged: no native Mac/Windows execution is proven by the Linux fixture runs; the
Mac and Windows catalog entries land only after their own builds and native checks; the
install scripts are lead-run, and no live computer is touched by this lane.
