# Shared AAC secret policy

The canonical packaged resource is `src/shared/secret-policy.json`. All detectors use this data;
Status has compatibility exports and no private pattern list. The policy retains the reader/native
union, Basic/Token/Digest forms, URL userinfo (including undotted/internal hosts), and a generic
`sk-` suffix of **20 or more characters**. More specific legacy prefixes retain their protections.

## TypeScript and JavaScript

Import from `src/shared/secret-policy.ts` in source, or `dist/shared/secret-policy.js` in a built package:

- `loadSecretPolicy(): SecretPolicy` returns an independent copy of the packaged data.
- `findSecret(text)` returns a fixed finding kind or `null`.
- `findSecretIn(source, value)` also checks decoded keys/strings, embedded JSON and credential keys.
- `SECRET_POLICY_PATH` names the canonical source/package resource.

Existing imports from `web-server/status/schema-helpers` re-export the same functions. Consumers
should import the shared module directly. The detector never returns matched credential text.

## Rust

AAC store exports `aac_store::secret_policy::{load_secret_policy, find_secret, find_secret_in,
SECRET_POLICY_PATH}`. `aac_store::report_secret` remains a compatibility alias.

Other native crates can consume the shared module without a database dependency. In
`native/<crate>/src/lib.rs`:

```rust
#[path = "../../shared/secret_policy.rs"]
pub mod secret_policy;
```

The shared implementation is `native/shared/secret_policy.rs`. It needs `serde` with `derive`,
`serde_json`, and `regex` with `std`, `perf` and `unicode-case` (or regex's default features).
`load_secret_policy()` returns immutable cached JSON; both finding functions return a fixed kind
or `None`. Package/build fingerprints must include the shared Rust file and JSON resource.

## Reusable parity corpus

`tests/fixtures/secret-policy-cases.json` holds synthetic `{name, doc, secret}` cases; `secret` is a
fixed expected kind or `null`. The named TypeScript export is `secretPolicyCases` from
`tests/fixtures/secret-policy-cases.ts`. Rust tests can use `include_str!` with the same JSON.
Both files are included in source packaging. Keep expected outcomes independent of the detector.

The corpus covers the reader and legacy native lists, shorter boundaries, authorization families,
embedded/escaped text and ordinary prose. Persistence tests also verify that a rejected batch with
a valid preceding item changes neither ledger bytes, compressed content nor history.

Finding kinds are intentional: an email finding is distinct from a credential finding. When
replacing a legacy boolean wrapper, preserve legitimate field-specific identity handling rather
than dropping attribution. Do not create another token threshold list. Keep the parity corpus when
retiring the legacy implementation used by the generated native comparison.
