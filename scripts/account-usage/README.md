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

