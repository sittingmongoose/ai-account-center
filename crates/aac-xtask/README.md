# AAC development tasks

Run from the repository with `cargo run -p aac-xtask -- <command>`.
X2 adds these commands without changing production or CI callers:

| Original entry | Rust command |
| --- | --- |
| `scripts/verify-bundle.js` | `release verify-bundle` |
| `scripts/deploy-lineage.js <operation>` | `release lineage <operation>` |
| `scripts/stage-dashboard-package.js <arguments>` | `release stage <arguments>` |
| `scripts/rebuild-landed-dashboard-package.py <arguments>` | `release rebuild <arguments>` |
| `scripts/postinstall.js` core | `release postinstall` |
| `scripts/postuninstall.js` | `release postuninstall` |
| `scripts/check-line-endings.js` | `check line-endings` |
| `scripts/personal-detail-guard.js` | `check personal` |
| `tests/docs/documentation-freshness.js` | `check docs` |
| `tests/docs/quickstart-parity.sh` | `check quickstart` |
| `tests/spec/inventory.mjs` | `check inventory [--regenerate]` |
| `scripts/foundation/one-file-policy.mjs` | `check one-file-policy` |

The `check` commands accept `--root <directory>` for offline fixtures. Release
APIs also accept explicit roots, installation directories and ledger directories.
The existing npm lifecycle hooks remain in place until the lead switches callers.
`ui-validate` uses the Rust bundle verifier.

The frozen hashed personal-detail policy, documentation contracts, one-file
allowlist reasons and counted receipts live beside the Rust modules as JSON.
These are policy data, not runtime imports from the old JavaScript scripts.
Changes to a policy must update its tests and its retained oracle together.

The post-TypeScript release package checks native executables instead of the
retired store/status Node addons and Codex JavaScript bundle (X0 audit section 9).
Staging excludes stale TypeScript output, retired command entrypoints, foreign
platform command binaries and retired native addons. It retains landed-commit,
source fingerprint, pinned artifact, ledger and rollback checks.

The release rehearsal invokes npm only to exercise the actual packaging/install
contract. Its child processes receive isolated home, account and temporary paths,
with provider collection and transports disabled. Receipt directories are retained
for the lead to inspect and clean. Running a rehearsal requires the existing
release artifacts and a clean landed checkout; it does not build or deploy them.

The offline tests compare fixture results against the retained Node oracles.
Before removing those scripts, preserve their fixture answers as reviewed golden
files or retain their source under test-only storage. Do not remove parity checks
as part of deleting production scripts.

Review stopping bar: a finding stops release work only when reproduced on real
inputs as a real check falsely passing, or when it demonstrates a product bug.
Other tool findings stay on the project's tool-issues list for its six-hour batch.
No delegated review is started by an implementation worker.

Bundle budgets use flate2 at the original default compression level, with all
thresholds unchanged. The lead approved regenerating size baselines with the
Rust encoder: the committed minimal fixture measures 2234 bytes, compared with
2229 from Node's Chromium zlib. The manifest and all other oracle checks retain
exact parity. See `tests/fixtures/bundle-size-baseline.json`.
