# AI Account Center System Architecture

```text
Slint browser / native Mac or Windows bar
                 |
       authenticated account APIs
                 |
       TypeScript dashboard services
        /            |             \
 native profiles   observed history  existing-host collectors
 guarded switching   Analytics      Python / browser bridge
```

The web dashboard is Slint 1.18.1 compiled through Rust to WebAssembly. The
backend remains TypeScript; native bars remain Swift/WPF. The primary CLI command
is `ai-account-center dashboard`, with `ccs config` compatibility.

## API and data ownership

[Server middleware](../../src/web-server/index.ts) owns session/origin/auth
boundaries and the served `dist/ui/` runtime. The
[browser bridge](../../web-dashboard/public/bridge.js) makes same-origin requests
and routes explicit user actions. It does not collect credentials or render a
second UI framework.

[Account service](../../src/web-server/services/account-dashboard-service.ts)
consolidates existing account sources. [Analytics](../../src/web-server/services/account-analytics-service.ts)
owns authentic history and source activity; helper models preserve unknown,
zero, signed balances, overages, units, resets and separate expiration. Quota
snapshots are not additive token totals.

`GET /api/accounts/analytics` takes `range` (`24h`, `7d`, `30d`, `month`, `all`,
or `custom` with inclusive local `from`/`to` days, at most 31 retained days) and
an IANA `tz` for local-day buckets ([ranges](../../src/web-server/services/account-analytics-range.ts)).
The [activity projection](../../src/web-server/services/account-analytics-projection.ts)
adds list-rate cost by token type with a reconciliation flag (none for a model
priced only by the unknown-model fallback), per-day model rows, request counts
(null when a snapshot lacks them), a path-free sample of the sessions active in
range with hashed keys and whole-session totals, and the original CCS anomaly
rules. The provider filter is validated against the server's provider table;
errors carry a stable `code`.

Usage activity covers Claude Code, Codex, OMP, Muse, zcode and Antigravity
(`scope: 'multi-host-cli'`), grouped under the dashboard provider that served
each call ([attribution](../../src/web-server/services/account-analytics-attribution.ts)):
Claude Code, Codex, the Muse CLI and Antigravity are their own provider, OMP and zcode
record a route per call (Qwen, Z.ai, Kimi Code, OpenCode Go, Cursor, Muse Code,
Antigravity), and a route no provider claims is `other`. Every model with usage
is published. Ubuntu logs are parsed in bounded worker
scans with per-file checkpoints; the Mac and Windows contribute per-model,
per-hour aggregates through one packaged Python helper streamed over the
existing ssh channel ([remote transport](../../src/web-server/services/analytics-remote-transport.ts)),
never raw events. Beside its hourly rows each answer carries per-session
aggregates whose key is a digest derived on the host that read the log — the
same key the server's own readers derive at ingest — so sessions from all three
hosts count in Session stats and Recent sessions while no session id, path or
directory ever leaves that host.
OMP rows use the logged cost when nonzero, else list rates
under the provider that served the call (a logged 0 is "not logged"); logged
and unlogged events never share a compact row. Muse and zcode input exclude
cache reads. A resumed OMP session copied into a second root counts once. A
remote scan that does not answer keeps the last remote aggregates in the
totals. `activity.sources` lists each tool and host as `ok`, `cached`,
`unavailable` or `not_installed`, each measured by that host's own scan; only
Cursor is a fixed "no local usage log" entry, being the one tool that keeps no
local usage log on any host. Antigravity is read by the same packaged helper on
every host, the VM included ([reader](../../src/web-server/usage/antigravity-native-usage-collector.ts)):
a port of T3 Code's reader over the conversation databases under `~/.gemini`,
`~/.config/antigravity` and T3 Code's Antigravity instance folders, usage
fields only, each record once across databases and copies. A live database
opens `mode=ro` (a WAL reader updates its read marks in the existing `-shm`,
nothing else); a WAL database closed cleanly, with no `-wal` file, opens
immutable, so the read never creates `-wal` or `-shm` files, and a read that a
writer overlapped is dropped and done again on the next scan.

[Codex account summaries](../../src/codex-auth/codex-auth-dashboard-service.ts),
[guarded activation/rollback](../../src/codex-auth/activate-codex-profile.ts) and idle-only
[auto-switch](../../src/web-server/services/codex-auto-switch-service.ts).
[App updates](../../src/web-server/services/app-update-service.ts) are allowlisted
jobs triggered explicitly, never from startup or routine polling.

The dashboard response lists every supported provider in `providers[]` (labels,
order, sign-in kind, live availability and capabilities) from one
[server table](../../src/web-server/services/dashboard-provider-registry.ts), and marks
each account `hidden` (dashboard) and `trayHidden` (trays) from the
[visibility store](../../src/web-server/services/account-visibility.ts).
`GET`/`PUT /api/accounts/visibility` read and update that store (a `PUT` names any of
its four lists and leaves the others as they are); a saved change
sends `{"type":"accounts-changed"}` to the signed-in `/ws` clients as a hint to re-read.

The [lifecycle routes](../../src/web-server/routes/account-lifecycle-routes.ts) under
`/api/accounts` add, sign in again, replace keys, remove, re-check, relabel, open
Cursor, list the 30-day trash and restore from it. They need a signed-in browser
session (a device token gets `device_scope`), the dashboard origin, JSON and a strict
body; keys and device codes need a secure transport (HTTPS, a trusted local TLS proxy,
a loopback tunnel or, when the owner turns it on, the trusted local network,
[`isSecureTransport`](../../src/web-server/middleware/secure-transport.ts)).
Destructive actions take a one-use [confirmation token](../../src/web-server/services/account-confirmations.ts)
bound to the session and to the reviewed state. Codex sign-ins run as in-memory
[jobs](../../src/web-server/services/signin-jobs.ts) whose state is pushed to `/ws` as
`{"type":"signin-job"}` to browser sessions only (without the code or email for sockets
on a plain transport) and polled. Once the login is being installed, a cancel or the
timeout no longer ends the job at once: the install checks the stop under the
activation lock and the job ends cancelled only if nothing was saved.

Dashboard sign-in ([auth routes](../../src/web-server/routes/auth-routes.ts)) keeps
the username and password login for browsers and today's trays, and adds a password
change, a first-run setup (free on loopback, a one-time code printed by the server for
the LAN) and "sign out other browsers" through a session epoch in `~/.ccs/auth/state.json`.
Trays pair once with the password and then use a revocable
[device token](../../src/web-server/services/dashboard-device-store.ts) as
`Authorization: Bearer`; only its SHA-256 is stored, it reaches only the tray routes
([allowlist](../../src/web-server/middleware/api-request-guard.ts)), rotates every 30 days
and expires after 90 days unused. Password, setup, pairing and rotation need a secure
transport; `dashboard_tls` in config.yaml (a trusted TLS proxy - local on loopback, or
`lan-https-proxy` with exact private `trusted_proxy_addresses` on another computer, set with
`ai-account-center dashboard proxy` - an in-process HTTPS listener and the public origin) is off
by default. So is `dashboard_network`: with
`trust_local_network: true`, plain HTTP from a peer in `trusted_networks` (by default
10/8, 172.16/12, 192.168/16 and fc00::/7; [ranges](../../src/web-server/middleware/trusted-networks.ts),
IPv4-mapped addresses normalised) counts as secure when the request carries no proxy
headers. Loopback is never trusted this way (its own rule also checks the Host header),
and public, link-local and CGNAT peers stay refused unless listed. `/api/auth/check` reports the switch and this
connection; [`PUT /api/auth/network`](../../src/web-server/routes/auth-network-routes.ts)
turns it off from any signed-in browser and on only from loopback. Credentials set by environment
variables are read-only. Wrong passwords are limited per address and per server
([limits](../../src/web-server/routes/auth-rate-limits.ts)), keyed independently of the
session id, and a confirmation token offered to one browser or tray is refused for any
other caller.

A Claude Open whose profile has a verified [history policy](../claude-history-sync.md)
is tracked as an operation, and the profile list reports its progress as
`openOperation` (counts and fixed sentences only). A request that sends
`Prefer: respond-async` gets 202 at once and polls; any other request waits and
gets the old 200 or refusal. [Operations](../../src/web-server/services/claude-open-operations.ts)
live in memory, so a restart never resumes one, and a repeated click joins the
running one. One Open copies for a bounded time (45 s or 50 records), then opens
Claude with `confirmedCount` below `totalCount`; the next Open copies the rest.
Without a policy the Open answers 200 as before.

## Credential and persistence boundaries

Private state remains under `~/.ccs/`, resolved by
[config-manager](../../src/utils/config-manager.ts). Preserve `CCS_HOME`,
`CCS_DIR`, session aliases, profile/provider IDs and native host identities.
Native account credentials remain separate from displayed DTOs and history.

[Provider collectors](provider-flows.md) operate on their configured existing
hosts/sessions. Identity-bound caches and bounded refresh/backoff retain truthful
unavailable/cached outcomes. Container packaging supplies no host credentials.

## Build and distribution

The [builder](../../scripts/build-ui.js) fingerprints manifest/lock, Rust,
Slint and public inputs, compiles locked WASM and stages directly into `dist/ui/`.
[Validation](../../scripts/validate-ui.js) and
[verification](../../scripts/verify-bundle.js) bind packaged runtime to current
source. Native and bridge source roots have independent offline checks.

The wasm-pack output is staged in `dist/ui/pkg/<buildId>/`, where `buildId` is the
first 12 hex characters of the wasm SHA-256, and only the packaged `bridge.js`
import is rewritten to that folder. Larger wasm and text files also ship as `.br`
and `.gz` copies listed in `ui-build-manifest.json`; folders are 0755 and files
0644. The [static server](../../src/web-server/static-ui.ts) checks those copies
at startup and negotiates them by `Accept-Encoding`, caches `pkg/<buildId>/**` as
immutable and revalidates everything else, and answers the page routes `/`,
`/login`, `/analytics`, `/accounts` and `/accounts/<provider>` with `index.html`.
API responses are never compressed, an unmatched `/api` request is a JSON 404, and
the build manifest itself is never served.

[Docker](../../docker/README.md) builds the same source and starts only the
dashboard on port 3000. Retired routing/runtime/tooling workflows are not part of
the current product. Keep the [MIT license](../../LICENSE), upstream credit and
Slint AboutSlint while changing visible branding.
