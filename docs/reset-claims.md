# Reset inventory and guarded claims

Owner decisions R1–R6 (2026-11-11) govern this backend. No UI, automatic claim,
switch-triggered claim, provider refresh during tests, deployment or reset spend
is part of this change. First real spend requires the owner's explicit
confirmation in released AAC. The reset UI is a later consumer of this API.

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
