# AI Account Center Codebase Summary

AI Account Center is a Leptos 0.9 account dashboard with a Rust service
and Swift/WPF clients. It retains
the account and private-state contracts from the original CCS fork.

| Surface | Current source |
| --- | --- |
| Primary CLI/bootstrap | [Native command entry points](../crates/aac-http/src/cli.rs) |
| Installed command and compatibility bins | [package.json](../package.json) |
| Dashboard HTTP/session boundary | [Rust public listener](../crates/aac-http/src/public_auth/mod.rs), [native owner host](../crates/aac-http/src/public_auth/owner_host.rs) |
| Account projection | [native producer](../crates/aac-http/src/account_usage/producer.rs), compatibility collection |
| Codex saved logins | account service, renewal, quota reader |
| Analytics | [native routes](../crates/aac-http/src/public_auth/analytics_routes.rs), [native writers](../crates/aac-http/src/analytics_writers.rs) |
| Explicit app updates | [app-update service](../crates/aac-http/src/app_updates/mod.rs), host invocation and helper sync, [update helpers](../scripts/app-updates/) |
| Leptos app frame | [lib.rs](../web-dashboard/leptos/src/lib.rs), [frame.rs](../web-dashboard/leptos/src/frame.rs) and the pages beside them; map of pages, controllers, view models and rules: [UI architecture](../web-dashboard/README-ARCHITECTURE.md) |
| Browser controllers | [frame-dashboard.mjs](../web-dashboard/public/frame-dashboard.mjs), [frame-session.mjs](../web-dashboard/public/frame-session.mjs), [frame-analytics.mjs](../web-dashboard/public/frame-analytics.mjs) |
| Native clients | [macos-bar](../macos-bar/), [windows-bar](../windows-bar/) |
| Existing-host usage | [Python collectors](../scripts/account-usage/), [browser bridges](../browser-bridge/), remote analytics scan |
| Private paths | config-manager |

`ai-account-center dashboard` and compatible `ccs config` reach the same
authenticated dashboard. Retired runtime bins provide guidance rather than
profile dispatch. Existing stored profile names, session aliases, provider IDs,
native messaging identities and `CCS_*` environment names remain compatible.

Seven computers are fixed in source: the Ubuntu computer that runs the dashboard
and the Mac, Windows, Nas1, Alexis PC, Odroid and 1070ti reached over fixed SSH aliases. The single
package-owned table is `web-dashboard/public/computers.json`. Nas1, Odroid and 1070ti are extra
Ubuntu computers (their helpers run with `--platform ubuntu`) that take part in
app updates (84 result rows across the seven computers), Status and the Analytics scan
only. They run no dashboard and hold no account state, and a quota source runs
there only when saved as `platform: "ubuntu"` plus that computer's `sshHost`. The
[computers table](system-architecture/index.md#computers) lists what runs where
and names the alias.

Providers, Analytics and usage-log tools, and the frozen storage host allowlist
live in [config/tables/](../config/tables/) (`providers.json`, `tools.json`,
`hosts.json`). The Rust crate
[aac-providers](../crates/aac-providers/) generates typed constants from the same
files, and its consumers hold no list literals of their own. A consumer's subset of
the table is a field there too: `providers.json`'s `additionalProviders`,
`snapshotOrder`, `signInSessions`, `usageRoutes` and `pricingAliases`, and
`tools.json`'s `genericUsageLogTool`, `activitySources`, `cliTargets`,
`toolProviders`, `appInventory` and `legacyRebuildThrough`. To add a provider or
tool, edit the JSON. `tests/spec/tables.test.ts` and `cargo test -p aac-providers` then name each hand
list (Python helpers, page JS, Leptos, native modules) that still needs the new row.

The service build stages the native commands, modules and agents in `dist/native/`
and records the build lineage; Antigravity sign-in and the Codex stop/start runtime
are native Rust (the TypeScript server was deleted on 2026-11-27). The
[UI builder](../scripts/build-ui.js) fingerprints the Leptos crate's manifest/lock,
sources, styles and fonts, builds locked WASM and stages `dist/ui/` with the public browser files.
The [validator](../scripts/validate-ui.js) and
[bundle verifier](../scripts/verify-bundle.js) bind source and packaged runtime.
There is no React/Vite production input.

Rust tests live in each crate; Bun runs the package-script tests and Node the dashboard helpers. Native/bridge
guides describe their independent offline checks. Source-built
[Docker](../docker/README.md) runs the dashboard only; it does not start CLIProxy
or import host sessions. Publication is outside the normal build.

Fork cleanup: unused session-launch and Claude config-linking code is removed. The CLIProxy manager was deleted on 2026-11-14 after its Codex quota reader moved into the dashboard server; account switching was removed the same day. See [migration notes](user-guide.md#existing-ccs-installations) and [approval tokens](approval-tokens.md).
