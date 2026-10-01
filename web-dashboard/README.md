# CCS account dashboard (Slint 1.18.1)

The production account dashboard is a real Slint UI compiled from
`ui/dashboard.slint` to Rust and WebAssembly. `public/bridge.js` supplies browser
session authentication, same-origin API requests and asynchronous updates; it
does not render the dashboard. No React production assets are required.
The browser probes WebGL on a separate disposable canvas before starting Slint's
supported FemtoVG WebAssembly renderer. If WebGL is denied or unavailable,
the page displays a readable request to enable browser hardware acceleration.
The bridge handles only winit 0.30.13's exact browser event-loop handoff signal
around the Slint start call, then continues session loading and UI bindings.
Other startup errors still reach the visible failure state.
Browser viewport dimensions and device pixel ratio are supplied before Slint's
first render, so the renderer and scene use the same physical size on
Retina displays. The dashboard keeps CSS dimensions in logical pixels.

## Build

Rust 1.92 or newer with `wasm32-unknown-unknown`, and wasm-pack:

```sh
wasm-pack build web-dashboard --release --target web --out-dir pkg --out-name ccs_account_dashboard -- --locked
```

Both `slint` and `slint-build` are pinned to **=1.18.1** in Cargo.toml and locked
in Cargo.lock. wasm-pack's old bundled optimizer cannot validate instructions
produced by the installed Rust 1.98, so its optional wasm-opt pass is disabled.
Rust release optimization and LTO remain enabled.

The repository UI builder stages all `public/` files with `pkg/` at the same
relative location into `ui/dist/`, mirrored into `dist/ui/`. The public entry
point is index.html; bridge.js imports `./pkg/ccs_account_dashboard.js`.
Use the existing CCS server and session-cookie authentication. Do not serve this
page as file:// or configure a cross-origin API URL.

## Data and controls

- GET `/api/accounts/dashboard` supplies consolidated provider accounts.
- Claude and Codex table summaries show provider-reported 5-hour and weekly
  percentages. They never invent token denominators or reset times.
- Detail views retain every returned quota window, credit balance, numeric
  amount, unlimited/enabled state, reset date and separate expiration date.
- Provider cards preview up to three actual windows. Their Details control
  shows all windows from every returned account.
- Claude launch icons POST the configured profile ID plus Mac/Windows choice.
- Codex activation and auto-switch settings reuse authenticated guarded APIs.
  The threshold is shown in percent used and sent in percent remaining.
  Busy activation opens a Slint review dialog listing the blocking programs.
  Yes submits the server-issued, target-bound approval token; Cancel sends no
  mutation. Expired or stale approvals require a fresh Activate review.
  The active Codex identity appears in both the page and Codex headers, with
  its table row highlighted and marked Active.
- Update apps starts the allowlisted asynchronous update job only after the
  user presses the button; it is never run while the dashboard starts.
- Usage refreshes at the server-confirmed interval configured in Settings
  (30–3600 seconds, initially 60 seconds). Dashboard and Analytics Refresh
  controls request fresh usage before reloading their views.
  Failed refreshes retain received samples and
  display their failure; unavailable values are never replaced with zero.
- Analytics is a dashboard navigation tab with all provider/account filters,
  observed quota history, reset/expiration details and authentic local CLI
  activity. History gaps stay empty; estimated API-equivalent costs are labeled.
- Reported usage can exceed 100%. Amounts and percentage labels preserve the
  overage; only visual progress-bar widths are bounded by their tracks.

## Validation

```sh
node --test web-dashboard/tests/*.test.mjs
```

The Slint compiler validates UI bindings during the actual WASM release build.
The data tests cover unknown versus genuine zero, credit units/expiration,
additional counters, confirmed auto-switch thresholds and complete details.

Official API documentation: https://docs.slint.dev/latest/docs/rust/slint/

## Dependencies and attribution

The dashboard application source follows the parent CCS project's MIT license.
Slint 1.18.1 is used under its
[Royalty-free license](https://github.com/slint-ui/slint/blob/v1.18.1/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md).
The standard unmodified `AboutSlint` widget is available at the bottom of the
Settings dialog, accessible from the dashboard's top-level Settings menu.
See [Slint's licensing FAQ](https://github.com/slint-ui/slint/blob/master/FAQ.md#what-obligations-do-i-need-to-fulfil-to-use-the-royalty-free-license).
