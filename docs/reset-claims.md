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

A cached inventory can be reused only while it matches the account's applied
snapshot. Learning a later revision through another alias invalidates that reuse.

A native refusal includes its current recovery `evidence` when storage can be read;
otherwise it includes null. The same revision rules apply to these snapshots.
Only an explicit `sent: false` acknowledges a no-send refusal. An unreadable or
missing sent field leaves the full unanswered-send obligation open. Unversioned
recovery-required errors additionally require a read started after the error was
received; neither a delayed earlier read nor a check receipt can retire that local
read obligation.
