# T3 Antigravity ACP update sessions

AAC uses the running T3 server's installer. T3 alone writes its runtime and
`active.json`. Discovery uses the official Desktop shim or the matching
standalone CLI and the persisted loopback endpoint.

## Authentication contract

Verified against T3 commit `ab8e488e10d883fa880b0ddcf01917b51839ab07`:

- [EnvironmentAuth.ts:722-747](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/auth/EnvironmentAuth.ts#L722-L747)
  verifies the token through SessionStore. The WebSocket bearer path falls
  through to request authentication at lines 1293-1294.
- [SessionStore.ts:803-833](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/auth/SessionStore.ts#L803-L833)
  rejects expired or revoked bearer tokens at authentication.
- [ws.ts:3366-3386](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/ws.ts#L3366-L3386)
  authenticates the upgrade and captures scopes for RPC authorization. At
  [lines 3445-3458](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/ws.ts#L3445-L3458)
  it races the socket against `sessions.awaitInvalidation`.
- [SessionStore.ts:1043-1097](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/auth/SessionStore.ts#L1043-L1097)
  watches revocation and sleeps until expiry. Expiry ends a bearer socket.
  Following a successor requires a DPoP proof key; issuing another ordinary
  bearer does not extend the original socket.

## Session lifetime

Jared, 2026-11-12 21:23Z, relayed by pm7-build: "Yes, accept 15 minutes (Recommended)".

Jared, 2026-11-12 22:01Z, relayed by pm7-build: "Option a for the second thing."
Option A means no issuance retry after a possibly committed timeout, and an honest row otherwise.

One successful session issuance covers the remaining overall run deadline,
capped at 900 seconds (15 minutes). Scopes remain `providers:manage` and
`orchestration:read`. There is no rotation, renewal or overlapping credential.
Each issuance attempt gets the entire remaining overall deadline. Only a proven
failure to start the CLI is retried; exponential backoff caps at eight seconds.
Timeouts, nonzero exits and malformed replies after spawn are never retried.

### Issuance evidence and cleanup

At the pinned T3 commit, [cli/auth.ts:29-47](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/cli/auth.ts#L29-L47)
resolves config and constructs services before the handler;
[lines 169-199](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/cli/auth.ts#L169-L199)
issue before formatting/printing. Argument/schema validation (TTL lines 50-54)
and config/service initialization failures before the handler are pre-commit.
AAC conservatively retries only Popen failure: a generic CLI error does not
prove which stage failed. [SessionStore.ts:643-748](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/auth/SessionStore.ts#L643-L748)
generates a fresh ID, persists at lines 700-712, then emits and returns; errors
from that path can occur after commit.

Each issue has a random per-run nonce in `--label`.
[EnvironmentAuth.ts:1084-1124](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/auth/EnvironmentAuth.ts#L1084-L1124)
preserves the label in client metadata and lists active sessions.
[cli/auth.ts:204-223](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/cli/auth.ts#L204-L223)
and [cliAuthFormat.ts:146-169](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/cliAuthFormat.ts#L146-L169)
expose `sessionId` and `client.label` through `auth session list --json`.
After ambiguity AAC spends up to five seconds on that lookup and revokes the
matching session in finally with the existing confirmed five-second budget.
A failed/empty lookup cannot prove absence (a commit can race cleanup). The row
always fails before installation with `t3_session_issue_unknown`: no install
started; one session may stay valid up to 15 minutes. Even successful cleanup
never turns ambiguous issuance into a successful update.

One connection is used until a transport failure. Reconnection uses the same
token and resumes `provider.install.subscribe`. Start is sent at most once.
Every resumed state must match the operation ID returned by start and its pin.
A different operation reports "Install state unknown, check T3".

## Recovery correlation

[providerInstallation.ts:108-125](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/provider/providerInstallation.ts#L108-L125)
accepts an instance for start and subscription, with no caller correlation key.
[AntigravityInstallation.ts:707-762](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/provider/AntigravityInstallation.ts#L707-L762)
returns an existing running operation at line 711, or generates its ID at line
718, publishes state at line 730 and returns it at line 760. Subscription exposes
the latest instance snapshot, which another caller can replace. Thus a lost
start reply has no provable correlation: AAC fails closed with "Install state
unknown, check T3". It never adopts that snapshot or repeats start. A received
start reply supplies the operation ID used for subsequent recovery.

## Termination and privacy

Finally cleanup confirms CLI revocation with retries within a shared five-second
budget. If confirmation fails, the row reports `t3_session_revoke_failed`, with
fixed text explaining the TTL. No background retry threads or joins are used.
The known session remains available to signal cleanup after a failed revoke.

The main thread handles SIGTERM/SIGINT/SIGHUP on Unix and SIGTERM/SIGINT/SIGBREAK
on Windows where available. Signals share a one-second revoke budget; a
two-second exit watchdog covers process-creation stalls. Neither path waits on
workers. Hard termination after session persistence relies on the accepted TTL,
at most 15 minutes from issuance. No future run is required to expire credentials.

### Accepted residual: a signal during issuance

A signal that arrives after T3 has saved a new session but before
`t3 auth session issue` prints it (T3 `SessionStore.ts` ~709 commits before
`cli/auth.ts` ~184-190 replies) finds no known session to revoke, so the helper
exits and that session stays valid until its TTL, at most 15 minutes. The Sol
6.1 max review of 2f893548 reproduced it offline (`app_update_t3_rpc.py` ~180,
signal cleanup, against ~234, where the issued session is registered). This is
the force-stop risk Jared accepted on decision card ACP-KEY: "If an update is
force-stopped partway, its key stays usable for up to 15 minutes." A full fix
cannot fit the one-second signal budget, because finding the session by its
label means starting another T3 CLI. pm7-build recorded it as an accepted
residual on 2026-11-12 at 22:36Z, with no further fix round.

Tokens stay in memory and the loopback Authorization header, never argv, AAC
files, logs, exceptions or row output. Only the ACP row contacts T3; stopped T3
does not delay other rows. AAC never writes the runtime or `active.json`.
Desktop-shim discovery, private JSON cleanup, Mac GUI session-loss reporting,
Cursor rows and the legacy 40-row view retain their offline checks.

## Offline coverage

Fixtures cover one issuance across reconnect, a 29-second issuance, deadline
TTL caps, pre-spawn retry/backoff, committed-timeout nonce cleanup, confirmed final revocation on success and
exceptions, bounded failed revocation, operation identity on reconnect,
unrelated-operation rejection, ambiguous-start failure, abrupt process kills
and bounded signals with blocked workers. No real T3 calls or live updates.

## Issuer lifetime (R9)

Jared, 2026-11-12 at 23:39Z, relayed by pm7-build, answering the Astra max
orphaned-issuer finding, verbatim: "1 B" (fix it, then a cheap recheck and
Astra max only).

The issuer is tracked before spawning a gated Python launcher. The launcher
cannot execute T3 until the helper releases its stdin gate; helper death before
release closes the pipe and prevents issuance. Signals kill tracked issuer trees
before revoking known sessions, within the existing one-second shared budget,
without waiting on workers. The two-second exit watchdog remains.

On Windows, a noninherited job handle has JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.
The launcher joins the job before gate release; T3 and its descendants inherit
membership. Helper death closes the last handle even under TerminateProcess.
Windows 8+ nested jobs are supported without breakaway; restrictive enclosing
jobs or creation/assignment errors fail closed with failed /
t3_issuer_protection_failed. T3 is never released unprotected.

On Linux, start_new_session creates a process group, killed on signals. A fresh
Python interpreter sets PR_SET_PDEATHSIG(SIGKILL), rechecks its parent PID, then
reads the gate and execs T3. No preexec_fn runs in a threaded parent. Failure to
set the death signal exits before T3. A hard kill kills the issuer itself;
descendants are covered by group termination on the signal path, not PDEATHSIG.

### Accepted residual: macOS hard kill during issuer startup

On macOS, a hard kill of the helper after gate release can leave the issuer
running; it may subsequently create a session that remains valid for up to
15 minutes from issuance, potentially beyond the original run deadline.

macOS has no parent-death signal. A pipe monitored by a wrapper cannot provide
an atomic guarantee between observing EOF and T3 persisting a session without
changing T3's CLI; no such guarantee is claimed. The process group is killed on
the signal path. This hard-kill residual is separate from the unchanged accepted
signal-after-save-before-print residual above.
