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

One successful session issuance covers the remaining overall run deadline,
capped at 900 seconds (15 minutes). Scopes remain `providers:manage` and
`orchestration:read`. There is no rotation, renewal or overlapping credential.
Issue timeouts start at 30 seconds and grow on retry, bounded by the remaining
deadline; exponential backoff caps at eight seconds. A lost issuance reply can
leave an unknown session valid until its TTL; retries never renew a known token.

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
workers. Hard termination relies on the accepted TTL, at most 15 minutes from
issuance. No future run is required to expire credentials.

Tokens stay in memory and the loopback Authorization header, never argv, AAC
files, logs, exceptions or row output. Only the ACP row contacts T3; stopped T3
does not delay other rows. AAC never writes the runtime or `active.json`.
Desktop-shim discovery, private JSON cleanup, Mac GUI session-loss reporting,
Cursor rows and the legacy 40-row view retain their offline checks.

## Offline coverage

Fixtures cover one issuance across reconnect, a 29-second issuance, deadline
TTL caps, issuance retry/backoff, confirmed final revocation on success and
exceptions, bounded failed revocation, operation identity on reconnect,
unrelated-operation rejection, ambiguous-start failure, abrupt process kills
and bounded signals with blocked workers. No real T3 calls or live updates.
