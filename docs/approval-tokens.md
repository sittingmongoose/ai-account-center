# Approval tokens for new guarded actions

Use `AccountConfirmationStore` in `src/web-server/services/account-confirmations.ts`
for every new guarded action. Extend its action union and use the existing
review/consume pattern in `src/web-server/services/account-lifecycle-removal.ts`.
The routes derive the session key and shape the requests. Do not add another issuer.

Bind each offer to the action, exact subject (account, trash entry or other target), authenticated session key and a
fingerprint of the state shown in the review. Recompute the binding before the
write. Consume once, including failed attempts, and require a new review after an
expired or stale offer. Never log tokens. Keep the existing 120-second lifetime,
32 pending offers and 8 offers per session, with their eviction behavior.

Browser routes obtain the session key from the authenticated request. A new
action exposed to paired devices must bind its authenticated device identity as
well; do not use an absent browser session as a shared identity. Design and test
that caller binding before enabling the device action. Preserve the transport,
origin, authentication and authorization checks at the route boundary.

Existing guarded actions keep their issuers and tests:

| Mechanism | Existing responsibility |
| --- | --- |
| `AccountConfirmationStore` | Account removal and trash restore/purge |
| `codex-activation-confirmation.ts` | Target profile, auth hash and reviewed process identities; 60-second one-use offer |
| `antigravity/confirmation.ts` | Existing Antigravity activation offer and process review |
| `ConfirmationBindings` | Caller overlay for existing activation routes; strict Codex binding and the existing Antigravity pass-through |

`signin-again-active` is reserved in the action union; no current caller issues it.

The overlay's ten-minute retention does not extend an issuer's lifetime: the
issuer still validates expiry and target/state. Do not migrate existing tokens or
change lifetimes, caps, binding modes or idle-only automatic switching as part of
this decision. Reset-claim tokens retain their separate transaction protocol.

Verification stays with the account-confirmations, caller-bound-confirmations,
activation route, lifecycle, reset-claim and idle-switch suites. A future issuer
migration requires a separate change and its full safety checks.
