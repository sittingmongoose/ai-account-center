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

Thus an authenticated AAC socket cannot safely outlive its token. Renewal
requires a new connection.

## Lifetime and renewal

Each `t3 auth session issue` receives `--ttl 60s` and only `providers:manage`
and `orchestration:read`. Rotation normally starts 30 seconds after issuance
starts. Measured issuance duration advances renewal to reserve startup time
plus 12 seconds for handshake and scheduling. Issue timeouts start at 30 seconds
and increase on retries; exponential backoff caps at eight seconds. The overall
install deadline caps every attempt and wait.

AAC issues and connects the replacement before closing the previous socket.
If renewal crosses expiry, T3 keeps installing and AAC reconnects to recover
the subscription. Normal revocation checks CLI success with a five-second
budget. Failures remain in memory and retry every two seconds until revoked
or expired, including after RPC cleanup. Signal budgets remain unchanged.

Start requires ten seconds of remaining connection lifetime, rotating first
when needed. Its reply can cross the renewal point but cannot outlive the
credential. An ambiguous reply recovers through subscription without replaying
start. The prior completed operation is rejected; a new operation or an already
running operation is followed.
[providerInstallation.ts:108-125](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/provider/providerInstallation.ts#L108-L125)
exposes subscriptions by instance. AAC checks the original operation ID and
pinned version on every subsequent snapshot, including after reconnect.
`provider.install.start` is never replayed, even if its reply is lost.

[AntigravityInstallation.ts:707-762](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/provider/AntigravityInstallation.ts#L707-L762)
returns an existing running operation or creates a new ID and updates state
before returning. Line 758 forks installation in the service scope.
[Lines 869-870](https://github.com/pingdotgg/t3code/blob/ab8e488e10d883fa880b0ddcf01917b51839ab07/apps/server/src/provider/AntigravityInstallation.ts#L869-L870)
expose state and SubscriptionRef changes. Subscription by instance needs no
operation ID and supplies the latest snapshot, including completion while
disconnected.

A killed helper, a lost issue reply or a failed revoke leaves a credential
with at most 60 seconds of validity from issuance. No later apply is needed.
AAC creates no session records or locks and never enumerates old sessions.
Leftover records from previous versions are ignored, including while T3 is
stopped. Only the ACP row discovers/authenticates with T3.

## Termination and privacy

The main thread handles SIGTERM/SIGINT/SIGHUP on Unix and SIGTERM/SIGINT/
SIGBREAK on Windows where signal delivery is available. It directly revokes
the in-memory known session ID with a shared one-second budget, suppresses
all CLI output, then calls `os._exit`. A two-second watchdog also exits if
process creation or revocation stalls. Neither path unwinds or joins a pool
worker. Windows hard termination and Unix SIGKILL rely on the short TTL.
Normal `finally` cleanup also attempts a bounded revoke.
The scheduler's main-thread wait wakes at least every 250 milliseconds to
dispatch Python console signals on Windows while the ACP worker is blocked.

Tokens remain in memory and the loopback Authorization header. They never
enter argv, AAC files, logs, exceptions or row output. Private JSON cleanup,
Mac GUI session-loss reporting, Cursor rows and legacy 40-row views are
unchanged.

## Offline coverage

The RPC fixtures use a fake auth store and abrupt child-process exits before
issue, after commit before reply, after reply before registration, during
connection, after authentication, after start before reply, during subscribe,
before revoke and after renewal. They verify expiry without any future run,
renewal revocation, retained operation identity, no start replay, bounded
signal handling with a blocked executor and a stalled revoke, no credential
output/files, and eleven normal rows plus one ACP error with stopped T3 and
legacy leftovers. The ACP fixtures cover one transport for a short install,
renewal with original-operation verification, T3 as the only runtime writer,
private JSON cleanup and Mac login-session loss.
