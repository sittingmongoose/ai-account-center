# AI Account Center web dashboard (Slint 1.18.1)

Source: [AI Account Center](https://github.com/sittingmongoose/ai-account-center),
a fork of [CCS](https://github.com/kaitranntt/ccs).

The production account dashboard is a real Slint UI compiled from
`ui/dashboard.slint` to Rust and WebAssembly, in the Daylight Atlas design
(light, dark and auto themes, embedded Instrument Sans and Martian Mono, Lucide
icons, official provider marks and the Apex Soft logo). `public/bridge.js`
supplies browser session authentication, same-origin API requests, URL state
(`?view=home|analytics|accounts`) and asynchronous updates; it does not render
the dashboard. `public/view-model.mjs` turns the API responses into the version 2
view model that `src/lib.rs` writes into persistent Slint models in place, so
changed readings animate. [ui/README-ARCHITECTURE.md](ui/README-ARCHITECTURE.md)
maps the shell, the component library, the theme tokens and the data seam.
No React production assets are required.
The browser probes WebGL on a separate disposable canvas before starting Slint's
supported FemtoVG WebAssembly renderer. If WebGL is denied or unavailable,
the page displays a readable request to enable browser hardware acceleration.
The bridge handles only winit 0.30.13's exact browser event-loop handoff signal
around the Slint start call, then continues session loading and UI bindings.
Other startup errors still reach the visible failure state.
Browser viewport dimensions and device pixel ratio are supplied before Slint's
first render, so the renderer and scene use the same physical size on
Retina displays. The dashboard keeps CSS dimensions in logical pixels.
The root window declares a preferred starting size instead of fixed width/height
constraints; browser resize and fullscreen requests update both layout and canvas.

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
relative location directly into `dist/ui/`. The public entry
point is index.html; bridge.js imports `./pkg/ccs_account_dashboard.js`.
Use the existing AI Account Center server and session-cookie authentication. Do not serve this
page as file:// or configure a cross-origin API URL.

## Data and controls

- GET `/api/accounts/dashboard` supplies consolidated provider accounts.
- Home shows Claude, then Codex and Antigravity as switchable sections, then one
  card per account of the other providers. Meters show provider-reported
  percentages with severity colours (warning from 80%, critical from 95%), the
  auto-switch threshold notch and reported overage; they never invent token
  denominators or reset times, and a missing reading is drawn as unavailable,
  never as zero. Fable appears only for Claude Max plans, from the
  `seven_day_fable` window ("Not reported yet" when absent).
- Details (a slide-over opened by clicking any account row or card; closed by a
  click outside it or Escape) retain every visible quota window, credit balance,
  numeric amount, unlimited/enabled state, reset date and separate expiration
  date. Product display exclusions remove Codex Chat pass, attach Qwen
  subscription expiration to monthly usage, and hide empty Z.ai reset-pack
  summaries.
- Claude launch buttons POST the configured profile ID plus Mac/Windows choice.
- Codex and Antigravity activation and auto-switch settings reuse authenticated
  guarded APIs. Thresholds are shown in percent used (Codex sends percent
  remaining). Busy activation opens a Slint review dialog listing the blocking
  programs. Yes submits the server-issued, target-bound approval token; Cancel
  sends no mutation. Expired or stale approvals require a fresh Activate review.
  The active account is the selected row, marked Active.
- Update apps is its own header button. It starts the allowlisted asynchronous
  update job only after the user presses it, shows progress in place and the
  result as a toast; it is never run while the dashboard starts.
- Usage refreshes at the server-confirmed interval configured in Accounts &
  Settings (30–3600 seconds, initially 60 seconds). Refresh requests fresh usage
  before reloading. Failed refreshes keep the received samples and report the
  failure as a toast; unavailable values are never replaced with zero.
- Analytics shows the KPI row and the quota history (one row per account,
  grouped by provider) from the version 2 analytics view model.
- Reported usage can exceed 100%. Amounts and percentage labels preserve the
  overage; only visual progress-bar widths are bounded by their tracks.
  Displayed fractions use at most two decimal places; source precision is retained.

## Validation

```sh
node --test web-dashboard/tests/*.test.mjs
```

The Slint compiler validates UI bindings during the actual WASM release build.
The data tests cover unknown versus genuine zero, credit units/expiration,
additional counters, confirmed auto-switch thresholds, complete details and the
version 2 view models (sections, Fable, switchable providers, hidden providers,
quota history).

For automated Chromium DPI checks, launch a disposable browser with
`--force-device-scale-factor=N` and a new context with `device_scale_factor=N`
(test N=1 and N=2). Record CSS size, `devicePixelRatio`, ResizeObserver's
`devicePixelContentBoxSize`, canvas backing size and WebGL dimensions at startup
and after resizing. A context-only scale override can leave the physical pixel
box at the browser's original scale, as reported in
[Playwright issue #18591](https://github.com/microsoft/playwright/issues/18591).
Preserve that failing context-only result separately from the browser-scale
comparison. Native-display checks use the display's actual browser scale.

Official API documentation: https://docs.slint.dev/latest/docs/rust/slint/

## Dependencies and attribution

The dashboard application source follows the parent CCS project's MIT license.
Slint 1.18.1 is used under its
[Royalty-free license](https://github.com/slint-ui/slint/blob/v1.18.1/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md).
The standard unmodified `AboutSlint` widget is shown in the About section of the
Accounts & Settings page.
Instrument Sans and Martian Mono are embedded under the SIL Open Font License 1.1
and the icons are Lucide (ISC); see `public/assets/THIRD-PARTY-NOTICES.txt`.
See [Slint's licensing FAQ](https://github.com/slint-ui/slint/blob/master/FAQ.md#what-obligations-do-i-need-to-fulfil-to-use-the-royalty-free-license).
