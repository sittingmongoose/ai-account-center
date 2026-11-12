# Reset inventory and guarded claims

Owner decisions R1–R6 (2026-11-11) govern reset claims. Automatic claims,
switch-triggered claims and provider refresh during tests are outside this feature.
Real spend requires the owner's explicit confirmation in released AAC.

Public inventory names individual opaque `grantKey` targets and reports kind,
refill sentence, remaining count, expiry, sample time/age and a blocked reason.
Exact provider IDs, organization, profile and credential host stay private.
Unknown kinds remain read-only; Qwen has no supported reset operation and Zai
claiming is not implemented. R4 permits observation separately, not guessing its
transport or sending a claim.

All routes are under `/api/accounts/:accountId/reset-claims`, protected by the
existing dashboard API authentication and origin guards. Signed-in phone/web
sessions are allowed. Paired native device tokens cannot prepare or claim.
The current router also requires sign-in for localhost (no anonymous claims).

| Method and suffix | Input | Result |
| --- | --- | --- |
| GET root | none | inventory plus permanent secret-free history |
| POST `/prepare` | `{grantKey}` | 120-second caller/account/grant/state-bound one-shot confirmation |
| POST root | `{grantKey,confirmationToken}` | attempt key, outcome, verification, providerSuccess |
| POST `/check` | `{attemptKey}` | read-only verification; never sends a provider claim |

For the Details UI the inventory also carries `providerWords` (an unmapped kind's own words, sanitised to at most
48 printable characters; absent for every mapped kind) and `pendingAttempt` (the account's unresolved attempt key,
or null, so any signed-in session can Check again after a reload). Inventory also returns an opaque
`canonicalAccountKey`: saved profile aliases for the same provider account and organization share
pending discovery, history and the UI's spend lock. Check again still requires evidence matching
the attempt's original profile, host and source binding to confirm settlement; another alias's
evidence leaves it unconfirmed. A confirmed claim or check answers `windows`:
each refilled window's id and used percent from the verification read (empty unless confirmed). A refused prepare
or claim answers 409 `{code, action, reason, sent, evidence}`: `reason` is one of a fixed allowlist of native codes (never an
identity) or null, and `sent` says whether the provider write was reached, in which case the UI blocks another
spend until Check again settles it.

Tokens and target keys are 64 hex characters. Request objects have exact fields;
raw provider grant IDs, host paths, organization IDs and executable names cannot
be provided by the browser. Responses and errors never echo private input.
`claimed` requires a fresh same-identity read with the exact grant decrement,
no affected window increasing and at least one affected percentage falling.
The confirmation must render prepare's authoritative `account.label`, never a
cached account card label. Provider `reset` without proof is
`claimed-unconfirmed`; `providerSuccess` distinguishes this from a timeout.
Missing spent grants remain unconfirmed rather than inferring a zero count.

The native module (`native/aac-status/src/reset*.rs`) owns policy, normalization,
confirmation consumption, limits, durable account serialization and audit.
The TypeScript router only delegates I/O and native calls. The private
`reset-source-adapter` resolves accounts from the existing registry/manifest;
public balance rows never establish claim identity.

Claude initially uses the manifest's existing Windows profile and credentials,
locally or through the existing SSH-streamed Python helper. It verifies the same
account and organization through the existing OAuth profile read. Its private
collector seam keeps credentials inside that host process and rechecks the exact
identity and inventory binding before POST. Mac-only logins remain view-only.
**Queued after first usable landing:** the Mac Keychain credential path (R1).

Codex uses a short-lived `codex app-server` with `CODEX_HOME` set to the exact
saved registry profile. It verifies the saved login, account/read identity and
rate-limit response accountId. Missing account attribution or per-credit IDs
makes the source unavailable. Only the pinned `codexRateLimits` kind is mapped.
Both private provider readers recheck the saved login after their read finishes
and immediately before accepting its snapshot. A changed or removed login makes
the read unavailable. Its observation time belongs to the identity verification
before the usage RPC, so a slow response cannot acquire a newer observation time.
Before every private provider read, the reset router asks the Store to commit a
`readTicket` from its independent durable logical clock. The private snapshot
carries that ticket as a decimal sequence and route hash; provider-returned
metadata cannot override it. Issuance changes no account revision, alias, token
or audit state. A failed ticket commit prevents provider I/O.

Inventory accepts only a route-bound ticket newer than that route's last accepted
ticket. Older reads and repeated tickets are rejected before any revision, alias,
target or expiry changes, even when the identity is unchanged. Counter issuance
and acceptance survive restart; wall-clock corrections cannot lower or strand the
route's authority. The old timestamp fence is retired on upgrade. Unticketed old
in-flight inventories fail closed, while retained targets and attempts stay
readable. A new ticketed read can verify an attempt from before the upgrade.
Ticket metadata is excluded from the confirmation fingerprint: a new read of
unchanged account/grant state still matches its confirmation.

Verification also orders ticketed before/after reads by sequence. Observation
wall times remain display/freshness data, not an identity or verification-order
fence; the existing fresh-sample policy remains. Token/grant expiry and execution
still use the trusted post-lock clock from the execution transaction.
The consume request always includes the exact `creditId` and `idempotencyKey`;
no default or next-credit substitution exists. Protocol evidence is the local
`t3code-ref` generated V2 schema: ConsumeAccountRateLimitResetCreditParams,
RateLimitResetCredit and GetAccountRateLimitsResponse. No HTTPS consume is used.

Private state is `getCcsDir()/account-usage/reset-claims/claims.sqlite` in an
owner-only directory, using SQLite FULL synchronous transactions. The durable
pending reservation and pre-write audit commit before provider I/O. Account
aliases share one reservation by provider/account/organization. A crash or
ambiguous result keeps that reservation, exact selection and idempotency key;
opening another confirmation cannot spend another grant. Check again is the
supported recovery path, with no automatic write retry. Prepare is limited to
6/minute per provider account, sends to 3/minute and 10/hour; provider cooldowns
and 429 holds persist across restarts. An unresolved attempt intentionally
blocks later claims until a fresh read proves the result.

Audit records are append-only, retained forever, with provider/kind, hashed
grant/caller/idempotency identifiers, outcome and before/after counts/windows.
Never copy the database into share exports: it also holds private snapshots.
The history API returns only secret-free audit fields. There are no raw
provider bodies, credentials, cookies, email addresses or raw grant IDs in it.

Offline coverage includes fake Claude endpoint calls, fake Codex app-server
processes, identity/grant drift, caller/device/origin denial, one-shot expiry,
account alias serialization, durable ambiguous recovery, cooldowns, exact
selection and fresh verification. Tests use temporary homes and synthetic
protocol material; they do not invoke real provider refresh or writes.

The real Node binding also has a completely offline scenario:
`node native/aac-status/tests/reset_abi.cjs /absolute/path/to/test-module.node`.
Use a native artifact built in an isolated target directory. It exercises only
`resetClaims` with generated nonces and synthetic snapshots, never source or
provider methods. It verifies exact selection, one-shot confirmation, durable
provider-success state, read-only verification and audit privacy.

## Details UI

`web-dashboard/leptos/src/resets.rs` draws the "Resets and balances" fact group: one table row per grant (State /
Refills / Expires) with Use reset only on rows the server marks claimable, used grants grey at the bottom, unknown
kinds in the provider's words with "AAC doesn't know what this refills yet; it can't be used from here". Opening
the dialog calls only `prepare`; it shows prepare's `account.label`, the grant, what it refills, its expiry and
"You have N; this spends one and can't be undone", and counts down the confirmation (ending 10 s early so the
server's fresh read fits inside its 120 s). Only "Use this reset" calls the claim route. The result line comes from
the server snapshot's newest attempt, then the group reads the inventory again; nothing is shown as used until a read says so. Z.ai
cards are listed with "Use in Z.ai"; Qwen says its plan has none. Paired native bars never call these routes.

Inventory, pending recovery, prepare, claim and check responses carry `evidence`:
`{canonicalAccountKey, revision, latestAttempt, pendingAttempts}`. Revisions and
attempt orders are decimal strings, preserving integer order in JavaScript. The
private store advances a durable logical clock in the same immediate transaction
as account observations and audit mutations. Each response reads its complete
account snapshot in that transaction. Inventory and pending recovery also include
`bindingRevision`, `history` and `pendingAttempt` from that snapshot; the HTTP
adapter does not combine separate native reads. Replaying a settled receipt keeps
its outcome/windows immutable while returning the account's current evidence.

The Details UI applies the highest revision per canonical account. Lower revisions
can add historical receipts/events but cannot change the pending map or current
result. Current result text comes only from the snapshot's newest attempt; older
receipts say "Older claim" without presenting old percentages as current limits.
A saved profile changing accounts binds the view using its ordered binding revision
and explicitly reports the change. Each account retains its own state and history.

Before transport starts, each local claim creates a separate unanswered-send
obligation. An accepted receipt for that request acknowledges only that obligation.
A lost or malformed answer stays blocked until an accepted inventory/recovery read
starts after the full 120-second send interval. Observing or settling another
attempt cannot shorten that interval. A refusal proving no provider send cancels
only its originating obligation and requires a fresh account snapshot. Responses
without valid ordering metadata cannot enable another claim. Read-only checks do
not acknowledge local sends.

Opening reset details always re-reads inventory. Use reset performs another current
server read before prepare; a cached observation alone cannot open a confirmation.

A native refusal includes its current recovery `evidence` when storage can be read;
otherwise it includes null. The same revision rules apply to these snapshots.
Only an explicit `sent: false` acknowledges a no-send refusal. An unreadable or
missing sent field leaves the full unanswered-send obligation open. Unversioned
recovery-required errors additionally require a read started after the error was
received; neither a delayed earlier read nor a check receipt can retire that local
read obligation.

Outstanding prepare confirmations are durable canonical-account obligations too.
Every ordered `evidence` snapshot includes `claimTokens: {untilMs, tokenKeys}`:
`untilMs` is the latest execution-time expiry of an unconsumed token, or null;
`tokenKeys` contains only opaque hashes, never usable confirmation secrets.
Prepare commits this record with the confirmation. Begin consumes it and reserves
the attempt in one transaction; even invalid confirmations are burned durably.
Expiry removes the record and advances the account revision using the server
clock. Upgrade recovers issued tokens from their original immutable targets.

A fresh page, tab or device blocks Use reset while any token remains and says
that a claim from this or another page may still arrive, with its expiry time.
Check again reads server evidence; a client clock alone cannot lift the block.
The originating dialog may consume its own sole token, identified by prepare's
`confirmationKey`. Other outstanding tokens block that dialog as well. The local
unanswered-send interval remains independent: another client's settlement or a
server token expiry cannot acknowledge that local transport prematurely.


Only one unconsumed, unexpired confirmation may be issued per canonical account.
Prepare checks this inside its immediate transaction, including across sibling
aliases and devices, and refuses with `claim-token-outstanding` if one exists.
The originating token may still reach begin. Legacy multiple-token records remain
visible and independently expire or burn; they never authorize another prepare.

Execution samples the trusted server clock **after** acquiring its immediate
transaction, including waits during open/migration and during begin itself.
Pruning, token validation, fingerprint checks, limits and reservation all use that
sample. Pending recovery follows the same clock rule; a queued request timestamp
cannot extend or prematurely expire a token. Direct Store tests retain explicit
injectable clocks; the production ABI always selects the trusted wall clock.

Prepare, begin and settlement notify signed-in dashboard browser sockets through
the existing `/ws` server with a data-free `reset-claims-changed` hint and opaque event id. Paired
devices and anonymous sockets receive none. Pages also publish same-origin hints
through BroadcastChannel and storage events. Server hints are relayed to browser
peers, and event ids suppress duplicate WebSocket/channel/storage deliveries. The reset view invalidates its cache
and blocks both new Use actions and an open confirmation until a read in the
current invalidation generation arrives. A delayed read from an earlier generation
cannot discharge that requirement, even when millisecond timestamps are equal.
Revisions still govern attempt/token/result state, independently of that local
freshness fence and the unanswered-send interval. Reads overtaken by another hint
or newer response are repeated; prepares and provider writes are never retried.
Focus, visible-again, online and socket reconnection also invalidate and re-read.
Socket/listener ownership ends with the signed-in view. Hints authorize nothing;
server exclusion and transaction-time validation protect even a missed hint or
a delayed pre-prepare response on an already-open page.
