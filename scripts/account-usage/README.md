The desktop collectors read existing accounts on the computer that owns them.
They emit normalized account usage only and never write account tokens, change
the selected account, or send a model prompt.

Antigravity refresh uses the native application's OAuth metadata from
`~/.ccs/account-usage/antigravity-oauth-client.json`. Provision that file outside
source control using the metadata shipped by the installed official application.
Its exact schema is `schemaVersion: 1`, `clientId: string`, and
`clientSecret: string`. These application values are separate from an account's
access and refresh tokens; neither belongs in a public code bundle.

The file must be owned by the current user with mode `0600` on Linux/macOS.
On Windows, the reader requires a protected ACL allowing only the current user
and SYSTEM. Missing, malformed or insecure app metadata produces an unavailable
usage result, without asking the user to sign in or inventing a quota reading.
Google access-token renewal remains in memory and uses the fixed official token
endpoint. An explicit collector home also selects its own metadata file, so test
accounts cannot read metadata from the real user's home.

`plan_usage.py` also accepts one registry v2 account:
`--account <provider>:usage|<provider>:acct:<8 hex> --credential discover|aac-key|browser-capsule`
with `--key-id <8 hex>` (Kimi Code, Z.ai, OpenCode Go) or `--capsule-id default|<8 hex>`
(Qwen). Only ids arrive; paths come from fixed folders. An `aac-key` account reads only
`~/.ccs/account-usage/keys/<provider>-<keyId>.json` (Windows: the `.dpapi` file, DPAPI
CurrentUser with entropy `AAC/account-key/v1`), in a 0700 folder at mode 0600, never a
symlink, and never falls back to OMP, OpenCode or environment keys. A missing or unsafe
key gives an unavailable result. A `browser-capsule` account reads only
`qwen-console-session.json` (`default`) or `qwen-console-session-<id>.json`. Any other
combination exits with status 2, which the dashboard reports as an outdated helper.

Exit status 2 is reserved for usage errors: argparse, the account argument check above,
or Python failing to open a missing helper file. A collection outcome always prints one
JSON row and exits 0; any other failure exits 1. The dashboard reads exit status 2 after
it sent account arguments as "Update the usage helper", so no helper may use 2 for
anything else. Which credential kinds each helper reads is pinned for both the server
(`COLLECTED_CREDENTIAL_KINDS`) and the helpers by one shared fixture,
`tests/fixtures/account-usage/collected-credential-kinds.json`.

Key file format, which the key store writer must produce exactly:
- Ubuntu and Mac: `<provider>-<keyId>.json` is the UTF-8 JSON record
  `{"version":1,"provider":"zai","keyId":"9f2c41d0","secret":"...","fingerprint":"sha256:<16 hex>","last4":"...","createdAt":"..."}`
  (`fingerprint`, `last4` and `createdAt` optional; no other key), at most 4 KB.
- Windows: `<provider>-<keyId>.dpapi` is the raw binary output of `CryptProtectData`
  (CurrentUser scope, entropy `AAC/account-key/v1`) over the UTF-8 bytes of that same JSON
  record, at most 16 KB. It is not base64 and not wrapped in JSON, unlike the Qwen
  capsule. The folder gets no extra ACL check: DPAPI CurrentUser already limits decryption
  to the same Windows user, and a linked file is still refused.

