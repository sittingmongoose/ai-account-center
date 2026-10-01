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

[Docker](../../docker/README.md) builds the same source and starts only the
dashboard on port 3000. Retired routing/runtime/tooling workflows are not part of
the current product. Keep the [MIT license](../../LICENSE), upstream credit and
Slint AboutSlint while changing visible branding.
