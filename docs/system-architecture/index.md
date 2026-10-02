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

[Codex account summaries](../../src/codex-auth/codex-auth-dashboard-service.ts),
[guarded activation/rollback](../../src/codex-auth/activate-codex-profile.ts) and idle-only
[auto-switch](../../src/web-server/services/codex-auto-switch-service.ts).
[App updates](../../src/web-server/services/app-update-service.ts) are allowlisted
jobs triggered explicitly, never from startup or routine polling.

The dashboard response lists every supported provider in `providers[]` (labels,
order, sign-in kind, live availability and capabilities) from one
[server table](../../src/web-server/services/dashboard-provider-registry.ts), and marks
each account `hidden` from the [visibility store](../../src/web-server/services/account-visibility.ts).
`GET`/`PUT /api/accounts/visibility` read and replace that store; a saved change
sends `{"type":"accounts-changed"}` to the signed-in `/ws` clients as a hint to re-read.

The [lifecycle routes](../../src/web-server/routes/account-lifecycle-routes.ts) under
`/api/accounts` add, sign in again, replace keys, remove, re-check, relabel, open
Cursor, list the 30-day trash and restore from it. They need a signed-in browser
session (a device token gets `device_scope`), the dashboard origin, JSON and a strict
body; keys and device codes need a secure transport (HTTPS, a trusted local TLS proxy
or a loopback tunnel, [`isSecureTransport`](../../src/web-server/middleware/secure-transport.ts)).
Destructive actions take a one-use [confirmation token](../../src/web-server/services/account-confirmations.ts)
bound to the session and to the reviewed state. Codex sign-ins run as in-memory
[jobs](../../src/web-server/services/signin-jobs.ts) whose state is pushed to `/ws` as
`{"type":"signin-job"}` (without the code for sockets on a plain transport) and polled.

A Claude Open whose profile has a verified [history policy](../claude-history-sync.md)
is tracked as an operation, and the profile list reports its progress as
`openOperation` (counts and fixed sentences only). A request that sends
`Prefer: respond-async` gets 202 at once and polls; any other request waits and
gets the old 200 or refusal. [Operations](../../src/web-server/services/claude-open-operations.ts)
live in memory, so a restart never resumes one, and a repeated click joins the
running one. Without a policy the Open answers 200 as before.

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
