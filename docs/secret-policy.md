# Shared AAC secret policy

The canonical packaged resource is `src/shared/secret-policy.json`. Both shared detectors use this data;
Status has compatibility exports and no private pattern list. The policy retains the reader/native
union, Basic/Token/Digest forms, URL userinfo (including undotted/internal hosts), and a generic
`sk-` suffix of **20 or more characters**. Scheme-less `user:password@host[:port]` is also
refused, as are `x-api-key`, `api-key` and `token` keys. More specific legacy prefixes retain their protections.

## TypeScript and JavaScript

Import from `src/shared/secret-policy.ts` in source, or `dist/shared/secret-policy.js` in a built package:

- `loadSecretPolicy(): SecretPolicy` returns an independent copy of the packaged data.
- `findSecret(text)` returns a fixed finding kind or `null`.
- `findSecretIn(source, value)` also checks decoded keys/strings, embedded JSON and credential keys.
- `nativeUnreadable(value)` says whether a value `JSON.parse` returned comes from text Rust's
  parser refuses: a number read as Infinity (`1e400`), a lone surrogate in a string or key, or
  nesting past serde's 127 levels. Every TypeScript reader that parses before `findSecretIn`
  treats such text as not valid JSON (`jsonFromBytes`, the quota reader), and embedded JSON is
  `unparseable embedded JSON`, so both languages refuse the same documents.
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

`tests/fixtures/secret-policy-cases.json` holds synthetic `{name, doc, secret}` cases; `secret` is the
one expected kind or `null` for both languages. There is no per-language exception: the TypeScript
and Rust corpus tests both refuse a case with any other key. The named TypeScript export is `secretPolicyCases` from
`tests/fixtures/secret-policy-cases.ts`. Rust tests can use `include_str!` with the same JSON.
Both files are included in source packaging.

JSON-looking strings (trimmed text starting with `{` or `[`) fail closed if parsing fails, with
`unparseable embedded JSON`. Quoted embedded JSON also fails closed on parse failure. Text
Rust's parser rejects (lone surrogates, overflowing numbers, nesting past 127 levels) is
unparseable in TypeScript too (`nativeUnreadable`), with the same reason. Work-budget exhaustion
also refuses the value. Invalid embedded text never
falls through to persistence. This also refuses leading Markdown such as `[link](url)`; prose
that does not start with an object/array opener is still checked as text. Keep expected outcomes independent of the detector.

The corpus covers the reader and legacy native lists, shorter boundaries, authorization families,
embedded/escaped text and ordinary prose. Persistence tests also verify that a rejected batch with
a valid preceding item changes neither ledger bytes, compressed content nor history.

Finding kinds are intentional: an email finding is distinct from a credential finding. When
replacing a legacy boolean wrapper, preserve legitimate field-specific identity handling rather
than dropping attribution. Do not create another token threshold list. Keep the parity corpus when
retiring the legacy implementation used by the generated native comparison.
