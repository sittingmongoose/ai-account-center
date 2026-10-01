# AI Account Center Provider Usage Flows

The account dashboard consumes existing authenticated account sources rather
than provisioning logins or routing AI prompts. Source boundaries are owned by
the [account service](../../src/web-server/services/account-dashboard-service.ts),
[additional accounts](../../src/web-server/services/additional-account-service.ts)
and [usage transport](../../src/web-server/services/additional-usage-transport.ts).

| Source | Retained owner |
| --- | --- |
| Native Claude/Codex usage | [usage collectors](../../src/web-server/usage/), existing native profile services |
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
[source parser](../../src/web-server/services/additional-account-service.ts)
before changing an existing deployment.

Claude desktop launch mappings remain in `claude-desktop-profiles.json`, using
the existing `platyr`, `gmail`, `party` and `me` profile IDs. The optional
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
