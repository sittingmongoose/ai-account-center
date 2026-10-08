# AI Account Center Provider Usage Flows

The account dashboard consumes existing authenticated account sources rather
than provisioning logins or routing AI prompts. Source boundaries are owned by
the [account service](../../src/web-server/services/account-dashboard-service.ts),
[additional accounts](../../src/web-server/services/additional-account-service.ts)
and [usage transport](../../src/web-server/services/additional-usage-transport.ts).

| Source | Retained owner |
| --- | --- |
| Native Codex quota | [saved-profile quota collector](../../src/web-server/usage/native-quota-collector.ts), [usage collectors](../../src/web-server/usage/) |
| Native Claude quota | [Claude Desktop live usage](../../src/web-server/services/claude-desktop-live-service.ts) with the [Python Claude collector](../../scripts/account-usage/claude_usage.py) |
| Additional desktop/plan usage | [Python collectors](../../scripts/account-usage/) |
| OpenCode/Muse browser session bridge | [bridge source](../../browser-bridge/opencode-muse/) |
| Qwen browser/native host bridge | [bridge source](../../browser-bridge/qwen/) |
| Historical observations | [Analytics service](../../src/web-server/services/account-analytics-service.ts) |

Credentials, browser sessions and private capsules stay on their configured
owner host. Existing approved SSH aliases are transport configuration, not an
invitation to create new grants or move raw credentials to the dashboard.
Browser/native messaging host IDs remain compatible with current installations.

Private `~/.ccs/account-usage-sources.json` (or the selected CCS home) uses a
version-1 manifest with a `sources` array. Each entry names one retained provider,
its `ubuntu`, `mac` or `windows` platform, and optionally an already approved
`sshHost` alias. The service validates a bounded manifest, rejects duplicate or
unsupported provider entries, and does not accept arbitrary commands or paths.
Without a manifest the additional-provider sources default to local Ubuntu;
absence of a local valid login is reported as unavailable. Consult the
[source parser](../../src/web-server/services/account-usage-manifest.ts)
before changing an existing deployment.

Nas1, the fourth computer, is a second Ubuntu computer and needs no new
`platform` value. A source runs on Nas1 when it is saved as `platform: "ubuntu"` with
`sshHost: "nas1-agent"`; the same command a remote Ubuntu already gets
(`/usr/bin/python3 "$HOME/.ccs/account-usage/<helper>" ... --platform 'ubuntu'`)
runs over ssh, and the dashboard derives the display host from that pair, so the
account reads "on Nas1" and "Update the usage helper on Nas1". The persisted
platform stays `ubuntu`, so the version-1 manifest, registry v2, quota history
and every earlier package read and collect such a source unchanged. A quota
reading belongs to a provider account, not to a computer, and Nas1 signs in to
the same provider accounts as the dashboard computer, so by default no source
moves there. Claude quota stays with the Mac and Windows Claude desktop apps (no
Claude desktop exists on Nas1), Codex quota with the dashboard computer's saved
logins and the OpenCode console wallet with the Mac. Lifecycle (add, sign in
again, replace key, remove) is not offered for Nas1: it stores no keys and holds
no sign-in state, and the key store refuses an Ubuntu source that has an
`sshHost`.

Registry v2, the private `account-usage-accounts.json` (0600, at most 64 KB),
lists several accounts per additional provider (at most 16 each, 64 in all).
When it exists and is valid it is the only source; when it exists but is unsafe
or malformed, every additional provider reads "Account list could not be read
safely" and the version 1 defaults are not used; when it is absent the version 1
manifest is read exactly as before. The dashboard never writes version 1: the
first lifecycle write copies the effective version 1 sources into v2 as
`discover` entries with ids `<provider>:usage`, so an older package that only
reads version 1 keeps working. Each account has its own cache and backoff. A
`discover` account keeps today's collector call; any other credential kind adds
only `--account <id> --credential <kind>` and `--key-id`, `--capsule-id` or
`--home-id`, never a path, host or secret. A helper that rejects those arguments
(an older helper) shows the account as unavailable with "Update the usage helper
on <host>". See the [registry](../../src/web-server/services/account-registry-v2.ts).

The display-only visibility file `account-visibility.json` (0600) holds two
independent pairs of lists: `hiddenProviders` and `hiddenAccountIds` for the
dashboard, `trayHiddenProviders` and `trayHiddenAccountIds` for the Mac and
Windows trays. Changing one pair never changes the other, so an account can be
shown in both, hidden only from the dashboard, hidden only from the trays, or
hidden from both. Hidden accounts are still collected and still auto-switch
candidates; the dashboard marks each row `hidden` (dashboard) and `trayHidden`
(trays) and lists every provider in `providers[]` with `visible` and
`trayVisible`. The dashboard follows only `visible` and `hidden`; the trays
follow only `trayVisible` and `trayHidden`. An unreadable file is never treated as
empty: the dashboard keeps the last good lists, sets `settings.visibilityAvailable`
to false, and shows nothing hidden only when it has never read the file. Every
private store refuses a folder that others can write.

API keys added from the dashboard (Kimi Code, Z.ai, OpenCode Go) are kept by the
[AAC key store](../../src/web-server/services/account-key-store.ts) in
`account-usage/keys/<provider>-<keyId>.json` (0600 in a 0700 folder) on the host that
collects them; another host is written by `scripts/account-usage/key_store.py` with
the key on stdin only. The API returns only the last four characters and a
fingerprint. A new key is checked once by the collector: a rejected key is deleted
again, a network failure keeps it as unverified. Codex accounts are added or signed
in again with `codex login --device-auth` in a private staging folder, never in the
native `~/.codex`; the identity must be new (Add) or unchanged (Sign in again, which
uses the workspace and person rules of `codex-activation-identity.ts` and a fresh read
of the live login), and the active, default and last profiles cannot be removed. Key
files that no account names are swept after an hour. Claude profile creation and
removal into a 30-day trash are implemented against a host transport but stay off
until the Windows launcher and the usage helper read profile ids from the inventory;
`CCS_CLAUDE_HOST_LIFECYCLE=on` in the server's environment turns them on for that
process only (a supervised dry run, or a sandbox with fake hosts). A computer's
default Claude profile (a launcher marked `isDefault`, or one whose data folder is the
app's own `Claude` folder, in any case) is never removed from the dashboard: Remove
answers 409 `account_protected` whether the host steps are on or off.

Claude desktop launch mappings remain in `claude-desktop-profiles.json`, using
the existing profile IDs. The optional
`opencode-console-wallet-source.json` selects a distinct workspace wallet source.
These configuration files contain private deployment state and are not shipped
with the package. Bridges for [Muse/OpenCode](../../browser-bridge/opencode-muse/README.md)
and [Qwen](../../browser-bridge/qwen/README.md) describe their own browser/host setup.
Native companions store dashboard connections as described in the
[Mac](../../macos-bar/README.md) and [Windows](../../windows-bar/README.md) guides.

Displayed accounts preserve provider identity, source/platform provenance,
genuine zero, unknown values, signed balances, overages, every applicable quota
window and separate reset/expiration. Cached samples retain their original
sample times. Muse keeps a prior reading only for explicitly classified temporary
rate limits, provider failures or network failures with the same verified
identity and source. Hard, unknown or unclassified failures clear that reading;
later retries cannot restore it. Identity changes cannot inherit another
account's cache.

Muse refresh/backoff and Qwen/OpenCode projections are tested offline in their
bridge/collector source roots. A fixture proves parser/guard behavior, not live
availability. [Analytics](index.md) uses observed snapshots without invented
points; local Ubuntu Claude/Codex token totals cannot be assigned to the current
active account or treated as subscription charges.

Old API profile routing, proxy rotation and managed web-search/image tools belong
to upstream history. The current dashboard and Docker entrypoint do not start
those runtime services.
