# Test tool issues

Non-blocking tool findings are collected for the six-hour batch under the
shared test-tool stopping bar. A reproduced false pass on a real check or a
product bug stops the lane. No such false pass was demonstrated below.

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
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
