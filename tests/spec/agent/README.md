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
