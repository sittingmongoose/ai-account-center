The desktop collectors read existing accounts on the computer that owns them.
They emit normalized account usage only and never write account tokens, change
the selected account, or send a model prompt.

A collector first uses the computer and credential named by its configured
source. The shared usage coordinator can then read the same verified provider
account on the dashboard computer or a configured Mac, Windows, Ubuntu or Nas1
source. It matches provider account identity, workspace/organization or key
fingerprint as applicable; an email or matching display label alone cannot
authorize another login. A first-ever unavailable source with no saved identity
remains unavailable because a peer login cannot establish the original account.

The coordinator streams the packaged `failover_usage.py` bundle over SSH stdin
and imports its modules in memory. No helper installation or credential copying
is required on alternate computers. Each helper reads that computer's existing
supported sign-ins, capsules and keys, verifies the selected identity and local
credential revision before and after collection, and returns normalized usage
and nonsecret identity metadata. Native application requirements, such as
Antigravity OAuth application metadata, still apply on each source computer.

Verified readings and original sample times persist under the dashboard's private
`account-usage-failover/` directory. A temporary failure can show the last
successful reading as cached across dashboard restarts; changed identities and
hard identity failures cannot inherit that reading. `identity_unavailable` (the
provider did not name the signed-in account this time) discards the primary's
reading but keeps the verified identity, so its exact or linked logins on the
other computers continue to supply quota. Renewal of a credential for
the same verified account can retain dated core quota; Claude browser extras
still require the original credential and capsule binding. Primary sign-in
warnings remain visible when a fallback supplies quota. Observation storage
retains at most 256 recognized private cache files, keeping recent readings.
HTTP 429 and `Retry-After` set a shared cooldown for the provider account across
native collectors and alternate computers, so changing hosts does not bypass a
rate limit. Muse's 300-second refresh budget is checked before requesting an
inference key; a healthy cached reading does not trigger peer refreshes.
An in-flight credential change rejects its quota but preserves any 429 hold for
the original account. Newer verified quota is never replaced by an older source
cache. Claude optional balances expire after 24 hours even when an entire source
is offline. Muse plan changes require dated native verification; an outdated
capsule label cannot discard a newer reading. Its shared browser cache keeps the
existing v1 format, with verification provenance in a separate private sidecar.
The sidecar separately records whether the latest attempt verified the native
identity, so a throttled identity lookup cannot restore a browser reading that
an earlier sign-in failure discarded.

Muse native installations can also be explicitly linked to an established
dashboard account. This is needed when the primary has a browser team binding
but another computer has only the CLI sign-in. The private grant is
`account-usage-failover/<source fingerprint>.muse-peers.json` under the selected
CCS home, beside the account's existing `<source fingerprint>.json` observation.
It has `version: 1`, the same `key` fingerprint, the primary `identity`, and a
`peers` array of `{host, identity}` entries. Hosts are the fixed `ubuntu`, `mac`,
`windows`, or `nas1` computers; peer identities must be exact
`muse-native-credential` fingerprints from the packaged helper's inventory.
Create this owner-only file only after the user explicitly associates those
existing native sign-ins with that subscription and live collection verifies
each account. Email or plan equality alone never creates a link. No credential
or browser capsule is copied.

A linked fallback replaces the complete quota reading with the native endpoint's
actual percentages, reset times, and observation time. It never fills missing
native counters or denominators from the portal cache. Source text identifies
native subscription quota. The regular account cooldown and each native helper's
five-minute budget still apply. Links survive restart, but changing the primary
account/source/team or a peer credential invalidates its link; explicitly link a
replacement credential again. Removing the grant revokes cached fallback quota
on the next coordinator read, including an in-flight result and any plan-only
metadata that peer supplied. Missing, malformed, symlinked or nonprivate grant
files authorize nothing.

Cursor reads the current subscription from its authenticated membership endpoint,
so changing plans does not depend on the desktop application's cached plan name.
Legacy Cursor plans retain their existing included, Cursor-model and other-model
usage fields. X Plus, Premium, Super and Ultra use the shared Grok pool; the new
Ultra code (`unified_ultra`) remains distinct from legacy Cursor Ultra (`ultra`).
Cards, account details and analytics choose their labels and meters from each
reported plan and its current usage windows. Additional usage spend stays separate
from included quota.

The shared pool uses `GetCurrentPeriodUsage.planUsage.totalPercentUsed` from
Cursor's DashboardService. Its cadence comes from
`GetPlanInfo.planInfo.includedUsagePeriod`; weekly resets use
`includedUsageResetsAt`. An explicitly monthly period uses its reported billing
cycle end. A missing period or reset stays unknown, and prices and legacy dollar
allowances never supply a new quota denominator.
Weekly, monthly and unspecified shared periods have different history keys from
the legacy pools. Changing plans therefore cannot turn old monthly model usage
into a new shared-pool forecast. A newly verified plan also invalidates cached
quota from the previous plan when usage itself is unavailable.

`cursor_usage.py` is shared by the standalone desktop helper and the streamed
failover bundle. Standalone installations must update it together with
`desktop_usage.py`, `desktop_helpers.py` and `plan_common.py`. The collector uses
the fixed first-party DashboardService routes with Connect JSON and a bounded
request budget; missing fields or an unsupported response remain unavailable.
The endpoint and field definitions come from Cursor's shipped client. Offline
fixtures validate parsing and transport shape; they do not establish live API
availability for a particular account.

The transport reuses the existing private
`claude-session-migration/venv` interpreter when present. OpenCode wallet live
collection needs its existing `curl_cffi` dependency there; no dependency is
installed by fallback. Private Claude Desktop profile paths are forwarded only
to the configured Windows source and validated before discovery.

Explicit source configuration and lifecycle ownership are unchanged. Nas1 is
still represented as `platform: "ubuntu"` with `sshHost: "nas1-agent"` and reads
only its own existing sign-ins; fallback adds no account row, key storage,
activation or sign-in control there. Standalone legacy collector commands below
remain available and require their helper files when invoked directly.

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

OpenCode Go's usage endpoint reports rejected credentials with HTTP 401. HTTP 403
does not establish a sign-in failure: denied plan access or an upstream access
restriction leaves usage unavailable, or retains a dated verified sample through
failover, without labeling the account signed out. Error response bodies stay private.

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

The writer is `key_store.py put|delete --provider <kimi-code|zai|opencode-go> --key-id
<8 hex>`. The key arrives on stdin only (one trailing newline allowed), never in argv or
the environment. It creates the folder at 0700 (tightening a looser one, refusing a link),
writes the record atomically at 0600 and prints only
`{"ok":true,"fingerprint":"sha256:...","last4":"..."}` (`{"ok":true}` for delete). A
refused key or a write failure prints nothing and exits 1; usage errors exit 2. The
dashboard writes keys for its own host in-process in the same format and uses this
helper over ssh only for a key kept on another host. Nas1 is never that host: it
stores no keys, so a source saved for Nas1 relies on `discover`.

