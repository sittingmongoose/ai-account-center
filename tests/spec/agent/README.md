# Agent verb parity

Run the unchanged Python suite while capturing its real `collect`/`fix_command`
calls: `python3 tests/spec/agent/capture-signin.py`. The table lists all 90
source tests, including helper/safety checks that still run in Python; 96 calls
become agent requests. Synthetic credentials only.

`bun tests/spec/agent/capture-inventory.ts` extracts the 9 literal machine probe
requests from the unchanged status collector tests and evaluates the real TS
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
105 sign-in cases (including 9 direct-file regressions) and 15 inventory cases
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

The first run checks all three inventory platforms, all 16 reading keys/values,
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
