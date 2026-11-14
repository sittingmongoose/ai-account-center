# AI Account Center Codebase Summary

AI Account Center is a Leptos 0.9 account dashboard with a TypeScript backend
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
| Explicit app updates | [app-update service](../src/web-server/services/app-update-service.ts), [host invocation and helper sync](../src/web-server/services/app-update-hosts.ts), [update helpers](../scripts/app-updates/) |
| Leptos app frame | [lib.rs](../web-dashboard/leptos/src/lib.rs), [frame.rs](../web-dashboard/leptos/src/frame.rs) and the pages beside them; map of pages, controllers, view models and rules: [UI architecture](../web-dashboard/README-ARCHITECTURE.md) |
| Browser controllers | [frame-dashboard.mjs](../web-dashboard/public/frame-dashboard.mjs), [frame-session.mjs](../web-dashboard/public/frame-session.mjs), [frame-analytics.mjs](../web-dashboard/public/frame-analytics.mjs) |
| Native clients | [macos-bar](../macos-bar/), [windows-bar](../windows-bar/) |
| Existing-host usage | [Python collectors](../scripts/account-usage/), [browser bridges](../browser-bridge/), [remote analytics scan](../src/web-server/services/analytics-remote-transport.ts) |
| Private paths | [config-manager](../src/utils/config-manager.ts) |

`ai-account-center dashboard` and compatible `ccs config` reach the same
authenticated dashboard. Retired runtime bins provide guidance rather than
profile dispatch. Existing stored profile names, session aliases, provider IDs,
native messaging identities and `CCS_*` environment names remain compatible.

Five computers are fixed in source: the Ubuntu computer that runs the dashboard
and the Mac, Windows, Nas1 and Alexis PC reached over fixed SSH aliases. The single
package-owned table is `web-dashboard/public/computers.json`. Nas1 is a second
Ubuntu computer (its helpers run with `--platform ubuntu`) that takes part in
app updates (50 result rows across the five computers) and the Analytics scan
only. It runs no dashboard and holds no account state, and a quota source runs
there only when saved as `platform: "ubuntu"` plus the Nas1 `sshHost`. The
[computers table](system-architecture/index.md#computers) lists what runs where
and names the alias.

The root TypeScript build compiles `src/` into `dist/` and
[bundles](../scripts/build-codex-update-runtime.js) the Codex stop/start runtime
into `dist/app-updates/`; the update helpers carry that bundle to Linux hosts
without AAC, such as Nas1. The
[UI builder](../scripts/build-ui.js) fingerprints the Leptos crate's manifest/lock,
sources, styles and fonts, builds locked WASM and stages `dist/ui/` with the public browser files.
The [validator](../scripts/validate-ui.js) and
[bundle verifier](../scripts/verify-bundle.js) bind source and packaged runtime.
There is no React/Vite production input.

Tests use Bun for TypeScript and Node for dashboard helpers. Native/bridge
guides describe their independent offline checks. Source-built
[Docker](../docker/README.md) runs the dashboard only; it does not start CLIProxy
or import host sessions. Publication is outside the normal build.

Fork cleanup: unused session-launch and Claude config-linking code is removed. CLIProxy is held for central-logins research. See [migration notes](user-guide.md#existing-ccs-installations) and [approval tokens](approval-tokens.md).
