# Spec harnesses

The TypeScript server (`src/`) and its TS-vs-Rust parity harnesses were deleted on 2026-11-27.
What remains checks the shared tables and vectors, and replays the recorded contracts against the
native binary. The recorded goldens (`routes.json`, `fixture.json`, `m6-dashboard.json`,
`contract/*.json`, the vector and corpus JSON files) stay as data: Rust tests read them with
`include_str!` or from disk, and they are no longer regenerated from TypeScript.

```sh
node tests/spec/run.mjs
AAC_RUST_PUBLIC_BINARY=/path/to/aac-http node tests/spec/run.mjs   # adds the native replays
```

`run.mjs` runs, in order:

- `parsers.py`: the offline parser case table (`parser-vectors.json`) against the Python helpers.
- `inventory.mjs`: the deterministic test inventory (`test-inventory.json`). After adding or
  removing a test file, run `node tests/spec/inventory.mjs --regenerate` and review the diff.
- `tables.test.ts`: `config/tables/*.json` against every remaining hand list (the Python helpers,
  the page's JS, the Antigravity column copies and the native bars).
- `scripts/foundation/one-file-policy.mjs`: no raw file mechanics outside `aac-files`.
- `read-number-vectors.mjs`: the shared number formatting vectors.
- With `AAC_RUST_PUBLIC_BINARY`: `m6-tray-parity.ts` and `m6-public-contract.ts`, which replay the
  recorded route contracts (`routes.json`, `contract/`) for every caller role against the binary
  started by `m6-rust-fixture.ts` on a temporary `CCS_HOME`.

`run.mjs --cut-rust-only --route-group=<group>` (with `AAC_M6_TARGET_CLI` naming the native
dashboard launcher) replays one route group through `m6-public-contract.ts`.

## Wire vocabulary

`wire-words.json` is embedded by `crates/aac-core` and is part of the agent's pinned source hash,
so it changes only with an agent rebuild. Rows whose source is the retired `src/` tree are kept as
frozen provenance. `node scripts/foundation/generate-wire-words.mjs --check` regenerates the table
from the native reset route and the Python helpers, carries the retired rows forward, and must
leave `wire-words.json` byte-identical; `wire-source-hashes.json` fingerprints only live sources.

## Standalone harnesses

These run against a built native binary and are not part of `run.mjs`:

- `home-active-open.mjs`, `large-home.mjs`, `native-collection-worker.mjs`,
  `deep3-fx4-analytics-profiles.ts`: native dashboard regressions and measurements on synthetic homes.
- `agent/transport.py <aac-agent> <aac-http>`: the agent fixture transport (real private socket, Rust
  computer pool, no real SSH).
- `views/home/*.mjs`: the Home view corpus and fuzz seeds that `crates/aac-views/tests/home_spec.rs` reads.

## L3: one numeric layer (`aac-num`)

The corpora in `num/` are written by the reference runtimes themselves and checked row by row by
`crates/aac-num/tests/corpus.rs`. Both scripts are seeded and deterministic; rerun them after changing them:

```
node tests/spec/num/make-node-corpus.mjs      # ecma.json, intl.json, dates.json, collation.json (records Node and ICU)
python3 tests/spec/num/make-python-corpus.py  # python.json (records the Python version)
```
