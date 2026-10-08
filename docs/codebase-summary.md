# AI Account Center Codebase Summary

AI Account Center is a Slint 1.18.1 account dashboard with a TypeScript backend
and native Swift/WPF clients. It retains existing account/private-state
contracts from the original CCS fork.

| Surface | Current source |
| --- | --- |
| Primary CLI/bootstrap | [src/ccs.ts](../src/ccs.ts), [command router](../src/commands/root-command-router.ts) |
| Installed command and compatibility bins | [package.json](../package.json) |
| Dashboard HTTP/session boundary | [web server](../src/web-server/index.ts), [auth middleware](../src/web-server/middleware/auth-middleware.ts) |
| Account projection | [account dashboard service](../src/web-server/services/account-dashboard-service.ts) |
| Codex controls | [account service](../src/codex-auth/codex-auth-dashboard-service.ts), [activation](../src/codex-auth/activate-codex-profile.ts), [auto-switch](../src/web-server/services/codex-auto-switch-service.ts) |
| Analytics | [analytics service](../src/web-server/services/account-analytics-service.ts) |
| Explicit app updates | [app-update service](../src/web-server/services/app-update-service.ts) |
| Slint scenes/bindings | [dashboard.slint](../web-dashboard/ui/dashboard.slint), [analytics.slint](../web-dashboard/ui/analytics.slint), [lib.rs](../web-dashboard/src/lib.rs); map of pages, components, view models and rendering rules: [UI architecture](../web-dashboard/ui/README-ARCHITECTURE.md) |
| Browser API bridge | [bridge.js](../web-dashboard/public/bridge.js) |
| Native clients | [macos-bar](../macos-bar/), [windows-bar](../windows-bar/) |
| Existing-host usage | [Python collectors](../scripts/account-usage/), [browser bridges](../browser-bridge/) |
| Private paths | [config-manager](../src/utils/config-manager.ts) |

`ai-account-center dashboard` and compatible `ccs config` reach the same
authenticated dashboard. Retired runtime bins provide guidance rather than
profile dispatch. Existing stored profile names, session aliases, provider IDs,
native messaging identities and `CCS_*` environment names remain compatible.

The root TypeScript build compiles `src/` into `dist/` and
[bundles](../scripts/build-codex-update-runtime.js) the Codex stop/start runtime
that the update helpers carry to Linux hosts without AAC into `dist/app-updates/`. The
[Slint builder](../scripts/build-ui.js) fingerprints the crate manifest/lock,
Rust, Slint and public browser inputs, builds locked WASM and stages `dist/ui/`.
The [validator](../scripts/validate-ui.js) and
[bundle verifier](../scripts/verify-bundle.js) bind source and packaged runtime.
There is no React/Vite production input.

Tests use Bun for TypeScript and Node for dashboard helpers. Native/bridge
guides describe their independent offline checks. Source-built
[Docker](../docker/README.md) runs the dashboard only; it does not start CLIProxy
or import host sessions. Publication is outside the normal build.
