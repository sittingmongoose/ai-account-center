# Test tool issues

Non-blocking tool findings are collected for the six-hour batch under the
shared test-tool stopping bar. A reproduced false pass on a real check or a
product bug stops the lane. No such false pass was demonstrated below.

| Tool | Issue and evidence | Real false pass? | Status |
| --- | --- | --- | --- |
| Windows helper-sync ACL fixture | `tests/unit/web-server/app-update-helper-sync.test.ts:262`: the PowerShell ACL inspection exits 1. Reproduced using both the test and helper implementation from unchanged `bdffa10e` on AlexisGamingPC. The new helper publishes successfully; this fixture's inspection fails. | No | Open for tool batch; inspect captured PowerShell stderr and fix the check or product if demonstrated. |
| Helper-sync test timeout | The corrupt-archive fixture exceeded Bun's default 5 seconds on the busy Windows CPU and left its killed fixture's stage directory. It passed unchanged with `--timeout 30000`. | No | Open for tool batch; set a realistic fixture timeout. |
| Broad Python discovery on Windows | `python -m unittest discover -s tests/unit/app-updates -p '*_test.py'` reported several failures/errors and ended with `KeyboardInterrupt` inside existing `T3Fixtures.test_t3_is_last_after_independent_updates_and_shared_dependencies`. No completed suite summary. Independently reproduced `T3Fixtures.test_state_directory_is_explicit_and_defaults_to_home_ccs` at `app_update_t3_test.py:77`: hardcoded `--platform ubuntu` triggers the unchanged platform guard on Windows. That test and `app_updates.py` are unchanged from `bdffa10e`. | No | Open for tool batch; audit remaining Windows failures and the interrupt. The changed ACP/RPC/common-cleanup fixtures pass independently. |

R4 recheck on AlexisGamingPC: the unchanged Windows ACL fixture still exits 1
with a 30-second timeout. Broad Python discovery again ended in the existing
T3 fixture's `KeyboardInterrupt`; direct `app_updates_test.py` discovery cannot
import Unix `termios` on Windows. The scoped RPC and ACP fixtures run separately;
Unix real-signal coverage keeps its Windows skip.
