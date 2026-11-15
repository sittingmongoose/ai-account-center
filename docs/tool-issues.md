# Test tool issues

R10 ttl-issuer recheck on AlexisGamingPC: the existing Windows helper-sync ACL
inspection still exits 1 with a 20-second timeout (nine other helper-sync tests
pass). Broad Python discovery again reaches Windows-incompatible T3 fixtures
and ends with KeyboardInterrupt in the existing scheduler fixture. Scoped RPC,
session and ACP fixtures pass separately. No new false pass or product bug is
demonstrated; these remain the existing tool-batch items below.

Non-blocking tool findings are collected for the six-hour batch under the
shared test-tool stopping bar. A reproduced false pass on a real check or a
product bug stops the lane. No such false pass was demonstrated below.

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
| Extra Mac parity coverage | M1 scan wave-24 fallback checks passed Rust workspace/clippy/fmt, but APFS rejects a legacy sign-in fixture containing a raw invalid-byte filename. With system Python 3.9, discovery records 67 exact agent/scanner comparisons before the unchanged local TS zcode WAL collector test fails; its zstd fixture also requires Python 3.14. Quota replay records 45 equal / 42 explicit fallback / 0 wrong ordinary replies, then fails its native-success-per-provider coverage assertion. | No false pass observed | Preserve the fixtures. Use Linux for the complete gate; investigate the existing Mac local collector and quota native-acceptance limits separately. No full Mac parity pass is claimed. |
| Agent debug artifact size | Scan-usage cheap review L1 reproduced a 67,598,608-byte default debug agent against the existing 64 MiB artifact guard. | No | Fixed: workspace dev/test profiles disable debug symbols reproducibly; the artifact cap and release profile are unchanged. Default Cargo test commands are rerun on Nas1. |
| Status share send-budget fixture | M2 part 4 VM full slow run: unchanged `status-server-review.test.ts:706` observed a recorded entry whose share state was not `failed` in the 60 ms hung-share fixture (201 other checks passed). The unchanged single-case rerun passes. No status source or assertion was changed. | No real-input false pass shown; product cause unconfirmed | Open for tool batch; full unchanged 53-file slow rerun passes, including this fixture. Investigate the fixture restoration/publication ordering without weakening the deadline checks. |
| Rust transport concurrency timing fixture | `clones_limit_a_computer_to_four_concurrent_channels` expects four overlapping 100 ms fake processes. On busy Nas1 during M1 scan-usage validation it observed three, with all 12 calls successful and no excess concurrency. This does not demonstrate a broken four-channel limit. | No | Open for tool batch; use a readiness barrier rather than depending on process startup timing. Rerun the unchanged fixture with tests serialized. |
| Windows helper-sync ACL fixture | `tests/unit/web-server/app-update-helper-sync.test.ts:262`: the PowerShell ACL inspection exits 1. Reproduced using both the test and helper implementation from unchanged `bdffa10e` on AlexisGamingPC. The new helper publishes successfully; this fixture's inspection fails. | No | Open for tool batch; inspect captured PowerShell stderr and fix the check or product if demonstrated. |
| Helper-sync test timeout | The corrupt-archive fixture exceeded Bun's default 5 seconds on the busy Windows CPU and left its killed fixture's stage directory. It passed unchanged with `--timeout 30000`. | No | Open for tool batch; set a realistic fixture timeout. |
| Signal subprocess timing fixture | New signal tests initially counted Python startup against the termination grace and failed under load. | No | Fixed within the required bounded-signal coverage: measure from a child ready marker at termination; all 19 RPC tests complete, with one Unix-only skip on Windows. |
| Broad Python discovery on Windows | `python -m unittest discover -s tests/unit/app-updates -p '*_test.py'` reported several failures/errors and ended with `KeyboardInterrupt` inside existing `T3Fixtures.test_t3_is_last_after_independent_updates_and_shared_dependencies`. No completed suite summary. Independently reproduced `T3Fixtures.test_state_directory_is_explicit_and_defaults_to_home_ccs` at `app_update_t3_test.py:77`: hardcoded `--platform ubuntu` triggers the unchanged platform guard on Windows. That test and `app_updates.py` are unchanged from `bdffa10e`. | No | Open for tool batch; audit remaining Windows failures and the interrupt. The changed ACP/RPC/common-cleanup fixtures pass independently. |

R4 recheck on AlexisGamingPC: the unchanged Windows ACL fixture still exits 1
with a 30-second timeout. Broad Python discovery again ended in the existing
T3 fixture's `KeyboardInterrupt`; direct `app_updates_test.py` discovery cannot
import Unix `termios` on Windows. The scoped RPC and ACP fixtures run separately;
Unix real-signal coverage keeps its Windows skip.

Short-session redesign recheck on AlexisGamingPC: the same unchanged Windows
ACL inspection still exits 1 (16 other tests pass in the Cursor/ACP + helper-sync
run). The under-8191 command check passes. No new tool finding or demonstrated
false pass; the ACL issue remains queued for the tool batch.

The redesign's optional R2 Python run has 24 passing tests and three existing
Windows failures: `test_mac_inventory_filters_user_and_rejects_oversize` needs
`os.getuid`; `test_lock_identity_names_start_and_boot` expects the Unix process
identity; `test_main_keeps_streamed_rows_after_an_unexpected_coordinator_failure`
hardcodes Ubuntu. All three reproduce from untouched `f00551bd` source and tests
in a separate fixture. They remain part of the broad Python portability issue
above; the changed-module and scheduler suites pass on Windows.

R6 rotation recheck on AlexisGamingPC: the helper-sync corrupt-archive check
passes with a 30-second timeout; the same Windows ACL inspection still exits 1
(nine helper-sync tests pass). The unchanged T3 Python suite again ends in
KeyboardInterrupt in the existing scheduling fixture after Windows portability
failures. These remain the existing tool-batch items; no new false pass or
product bug was demonstrated by these checks. The ten other relevant Bun files
pass (124 tests, three platform skips).

R7 single-session validation on AlexisGamingPC: scoped session/RPC/ACP/scheduler
Python suites pass (69 tests, two platform skips). Broad discovery still exits
in the existing Windows-incompatible fixtures. Separate unchanged ZCode tests
have 44 passes, three errors (Unix platform selection and os.getuid on Windows)
and one Mac GUI race failure (failed/update_failed instead of quit_first).
No ZCode implementation or fixture changed in R7. These join the existing
Windows portability batch. Bun update checks have 133 passes, three skips and
the existing ACL inspection failure; the final contract subset has 37 passes.

R8 issuance recheck on AlexisGamingPC: the same Windows ACL inspection exits 1.
Three Codex bridge launch fixtures also fail because their executable fixtures
are Unix shell scripts. All four reproduce with unchanged `92874d7e` sources
and tests (52 passes, four failures). No real false pass or new product bug was
demonstrated; retain these in the Windows portability tool batch. Broad Python
discovery again ends in existing Windows-incompatible T3 fixtures without a
completed summary. Scoped issuance/RPC/ACP tests pass separately.

R9 inherited Windows test-tool issues (not product bugs):

- `computer-registry.test.ts:58` assumes Alexis is a remote host when the test
  runs on Alexis itself.
- `integration/computer-registry.test.ts:155` hard-codes `python3`, which Windows
  does not have.

R9 stopping bar, before review: a tool finding stops work only when demonstrated
on real inputs to make a real check falsely pass or reveal a product bug.
Other findings stay in this list for the six-hour tool batch; none are dropped.

M3 service skeleton optional documentation check (2026-11-14):
`node tests/docs/documentation-freshness.js` falsely treats the inline Markdown
example on `docs/secret-policy.md:57` as a relative link to a file named `url`.
The example is byte-identical on base `c5c2763db`; no real link or product failure
was demonstrated. Queue the parser fix for the six-hour tool batch. Required
M3 runtime/spec checks remain independent of this extra check.

M3 cheap-review L1 (2026-11-14), nonblocking portability follow-up: the runtime
route attribution used an unanchored stack regex that could consume a checkout
folder named `src` or ending in `-src`. Fix round 1 anchors the frame to the
actual checkout's source folder and extracts a checkout-relative path. The
review also found clean-copy fixture HTML 404s on `/` and the wildcard hub POST.
Diagnosis: Nas1's shared dependency tree has Express 4.22.1; package.json pins
5.3.0, as used by the VM task checkout. Copying that already-existing pinned
tree into the isolated Nas1 copy makes all 410 on/shadow goldens pass without
any checker/golden change. The obsolete shared tree is unchanged and stays a
tool/environment follow-up: source copies must use the pinned dependencies.
No golden or checker is weakened or skipped.

M2 quota Nas1 check (2026-11-14): unchanged
`clones_limit_a_computer_to_four_concurrent_channels` occasionally observes a
maximum of three child processes instead of four under parallel test/compile
load (the fixture sleeps only 100 ms). This is a false failure, also observed
in M2 part 1. Workspace tests pass with `--test-threads=1`; no transport code,
assertion or concurrency cap was changed. Queue a deterministic synchronization
fixture for the six-hour test-tool batch. HTTPS debug symbols also make the
unstripped agent exceed the existing 64 MiB artifact cap. The workspace dev/test
profiles now disable debug symbols by default, so ordinary Cargo test commands
need no environment overrides. The cap is unchanged; production artifact size
still needs release-packaging checks.

## Rewrite foundation: max review follow-up

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
| Rust transport four-channel fixture | `crates/aac-transport/tests/transport.rs`, `clones_limit_a_computer_to_four_concurrent_channels`: uncoordinated 100 ms children reached only three-way overlap in two unchanged workspace runs. [Astra max review L2](/mnt/Cursor/share/ai-account-center/2026-11-14-parallel/MAX-REWRITE-ASTRA.md). | No; false failure only, no excess concurrency demonstrated. | Open for six-hour tool batch: arrange four child-ready signals, hold a barrier, assert fifth blocked, then release. Do not weaken the limit assertion to hide the race. |

## Analytics search-bound validation (2026-11-15)

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
| OMP cached-root fixture | The unchanged `worker/sessions` 512-root revalidation case in `tests/unit/web-server/usage/omp-session-root-discovery.test.ts` exceeded Bun's default 5-second test timeout once (5.91 s) during the full fast bucket. All six cases passed in isolation (2.59 s total; the case took 1.48 s). [Timeout evidence](/mnt/Cursor/share/ai-account-center/2026-11-15-analytics-search-bound/fast-timeout.txt), [isolated recheck](/mnt/Cursor/share/ai-account-center/2026-11-15-analytics-search-bound/omp-revalidation.txt). | No; timing failure only, no product failure demonstrated. | Open for the six-hour tool batch: remove dependence on shared-runner scheduling while preserving the root-cap and rediscovery assertions. No timeout or assertion was weakened here. |

## Big review fix round 1 (2026-11-15)

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
| File-read shadow count | `tests/spec/read-file-parity.ts:211-213` waits at most 1 s (200 x 5 ms) for eight background shadow comparisons. One full `AAC_RUST_SERVICE=shadow node tests/spec/run.mjs` on the VM at load average 31 failed `shadowed >= 8`; two isolated reruns and the next full shadow run passed with 11 comparisons and 0 differences, on the same binary. | No; a timing failure only, no difference or product failure shown. | Open for the six-hour tool batch: await the shadow comparisons' own settlement instead of a fixed sleep budget. No assertion was weakened here. |

## L3 numeric layer (2026-11-15)

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
| Spec harness binary default | `tests/spec/settings-parity.ts:10` defaults to `target/debug/aac-http`, while every other `run.mjs` harness defaults to `dist/native/aac-http`. With only `dist/native/aac-http` present, `AAC_RUST_SERVICE=on node tests/spec/run.mjs` stops at settings-parity with "Build the real Rust service on Nas1 or the Mac first"; setting `AAC_RUST_SERVICE_BINARY` to the dist binary passes. | No; a false failure only. | Open for the six-hour tool batch: give settings-parity the same `dist/native` default as the other harnesses. |
| aac-http profile-read socket test | `crates/aac-http/tests/profile_reads.rs` `profile_reads_keep_the_ts_scopes` failed once with `BrokenPipe` in a full `cargo test --workspace` on Nas1 at load average about 16: the service answers 401 before the test has written its request body. Five isolated reruns passed (4 of 4 each), on the same source. | No; a false failure only, no product failure shown. | Open for the six-hour tool batch: read the reply even when the body write fails, instead of failing on EPIPE. |
| Stray Windows-path folder | A full `test:slow` run on Linux leaves an empty folder named `C:\Users\x` in the checkout root (seen in this and other worktrees). The suites that use that path are `claude-account-lifecycle.test.ts`, `claude-host-transport.test.ts` and `status-collectors.test.ts`; on Linux the string is a relative name. | No; checkout litter only. | Open for the six-hour tool batch: create such paths under the test's temporary root. |
