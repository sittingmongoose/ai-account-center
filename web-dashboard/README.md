# AI Account Center web dashboard (Slint 1.18.1)

Source: [AI Account Center](https://github.com/sittingmongoose/ai-account-center),
a fork of [CCS](https://github.com/kaitranntt/ccs).

The production account dashboard is a real Slint UI compiled from
`ui/dashboard.slint` to Rust and WebAssembly, in the Daylight Atlas design
(light, dark and auto themes, embedded Instrument Sans and Martian Mono, Lucide
icons, official provider marks and the Apex Soft logo). `public/bridge.js`
supplies browser session authentication, same-origin API requests, URL state
(the page routes `/`, `/analytics` and `/accounts`, with `?view=` as an alias)
and asynchronous updates; it does not render
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
  `seven_day_fable` window ("Not reported yet" when absent). A window whose
  reset time has passed while its reading was sampled before that reset (or at
  an unknown time) hides the old percent, fill and notch and reads "Reset at
  10:15 AM · new reading pending" over a dashed track until a newer reading
  arrives; it never shows 0%, and it does not count toward a switch point.
- Details (a slide-over opened by clicking any account row or card; closed by a
  click anywhere outside it, including header buttons, nested row actions and
  Analytics cards, or Escape; a click on another row swaps it to that account)
  retain every visible quota window, credit balance,
  numeric amount, unlimited/enabled state, reset date and separate expiration
  date. Product display exclusions remove Codex Chat pass, attach Qwen
  subscription expiration to monthly usage, and hide empty Z.ai reset-pack
  summaries.
- Claude launch buttons POST the configured profile ID plus Mac/Windows choice
  with `Prefer: respond-async`. A 200 is a finished Open, as before. A 202
  (a managed history copy) starts read-only polling of
  `GET /api/claude/desktop-profiles` (every 1 s for 120 s, then every 5 s, for
  at most 10 minutes) and the row's line shows "Copying history 3 of 18",
  "Opening on Mac", "Opened on Mac" or the server's fixed sentence; the result
  also appears as a toast. The POST is never sent again by the poller, a second
  click while an Open runs sends nothing, and nothing resumes after a reload or
  a server restart (`public/claude-open.mjs`, `tests/claude-open.test.mjs`).
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
- Analytics is built around Usage, the original CCS analytics page improved: range
  presets (24H, 7D, 30D, Month, All) and a custom range of whole local days, a
  provider picker, five KPI cards, the usage-trends chart (tokens and
  estimated cost on two axes, token types and cache reads toggles, a crosshair
  readout), cost by model with a model popover, the model donut, session stats,
  token breakdown, cache efficiency, a weekday x hour heatmap, daily cost by
  provider, the compact quota history with focus charts and the upcoming resets
  and expiries, all in local time from the version 3 analytics view model.
  Per-type costs use the rates mirrored from `src/web-server/model-pricing.ts`
  (`public/model-rates.mjs`) and are kept only when they add up to each model's
  logged estimate; otherwise the split shows token shares and says so. Usage
  from OMP, Muse Code and zcode (Ubuntu, Mac and Windows) is grouped under the
  dashboard provider that served it, from the route each log records (Qwen,
  Z.ai, Kimi Code, OpenCode Go, Cursor, Muse Code, Antigravity; a route no
  provider claims is Other); the tools are never providers. The top-right
  picker lists every provider with usage in the range, a "Tokens by provider"
  summary sits under the KPI cards, Cost by model lists every model (sortable
  by cost or tokens), and the donut's "N smaller models" and its not-logged
  models open into their models. "Included usage" in the header lists each
  tool's source state and last scan. Cost with no logged amount and no listed
  rate shows "Not logged", never $0.00, and totals that leave it out say
  "partial"; this holds for every provider, including Claude and Codex. Month,
  All and custom ranges read the covering 24h, 7d or 30d response and are cut in
  the browser; per-model and session data then say which logs they cover.
- Accounts & Settings lists every provider with its accounts: status only for
  an exception, the last sample, how it signs in, and fixed, aligned action
  slots. Codex and Antigravity Activate, Claude Open on Mac or Windows, the
  Codex and Antigravity auto-switch policies, the refresh interval and the
  appearance are live, and so is every sign-in and sign-out control the server
  offers: Add account (Codex device code with the page and code, a Claude
  profile on Mac and Windows, an API key in a masked field that shows only its
  last 4 afterwards), Sign in again, Replace key, Remove with a confirmation
  (Claude into the 30-day trash, with Restore), guided app and browser
  sign-ins with Re-check, and "Show on dashboard" and "Show in tray" saved on
  the server, per provider and per account (each account's two switches are
  independent, so it can be shown in both, on one only, or in neither). What a
  control may do comes from the server (`providers[]` and
  `GET /api/accounts/registry`); "coming" marks only what the server has no
  flow or route for yet. A computer's default Claude profile is never offered
  for removal. The settings column holds the Dashboard sign-in block (password
  change, other browsers, paired trays with Revoke and Sign out all devices,
  the trusted local network with Turn off, Sign out), Update apps results by
  computer, read-only connection facts and About.
- The sign-in page is the login screen: wrong passwords show the tries left, a
  pause shows its countdown from `retryAfterSeconds` or `Retry-After`, a session
  that ran out or was signed out from another browser says so, the network note
  follows the trusted local network (trusted, not on it, or trust off with plain
  guidance), and first-run setup appears only when the server reports setup mode
  (with the setup code when the server asks for one).
- Reported usage can exceed 100%. Amounts and percentage labels preserve the
  overage; only visual progress-bar widths are bounded by their tracks.
  Displayed fractions use at most two decimal places; source precision is retained.

## Rendering performance

The FemtoVG renderer redraws the whole canvas for every frame, so the dashboard
keeps per-frame work small, which matters most on software WebGL (Chrome's
SwiftShader on a machine without a GPU):

- Nothing animates while idle: spinners, the update-progress bar, the sign-in
  sweep and the skeleton shimmer read `animation-tick()` only while they are
  active, and the browser requests no frames once the page settles.
- Static surfaces render into cached layers (`cache-rendering-hint`): the paper
  grid, Home's sections and provider cards, the Accounts & Settings sections and
  settings groups, the Analytics KPI cards and every Analytics card whose
  content does not follow the pointer. The usage-trends card caches its axes,
  gridlines and data paths in their own layer, so the crosshair only blits it.
  A layer re-renders only while something inside it changes.
- Hover lifts fade in a second, static shadow (`HoverShadow`) instead of
  animating a shadow's blur or colour, which would make the renderer blur a new
  shadow texture on every frame.
- Value animations (meter widths, count-ups, chart draws) use the ease-out
  curve only; a filmed refresh moves every meter monotonically to its new value.

Measured on the Ubuntu VM in headless Chrome with SwiftShader at 1440 x 900
(2026-10-02, median per animation): the page's own work per frame (WebAssembly
and JavaScript) is 5-11 ms for scrolling and hovering and about 17 ms while
Details slides in, and frames arrive every 47-90 ms (11-21 fps), a rate set by
SwiftShader's rasterising rather than by the page. Before the cached layers and
static hover shadows, a hover in Analytics took 250-300 ms a frame (4 fps) and
Home's scrolling about 20 ms of page work a frame. Hardware-accelerated browsers
are far faster; the VM figure is the floor.

Every state push from `bridge.js` (`with_ui` and the other entry points in
`src/lib.rs`) asks for a frame and wakes winit's event loop, so a change that
arrives while the loop sleeps is painted at once instead of at the next pointer
event.

## Validation

```sh
node --test web-dashboard/tests/*.test.mjs
```

The Slint compiler validates UI bindings during the actual WASM release build.
The data tests cover unknown versus genuine zero, credit units/expiration,
additional counters, confirmed auto-switch thresholds, complete details, the
version 2 dashboard view models (sections, Fable, switchable providers, hidden
providers) and the version 3 analytics model (rate mirror parity with
`model-pricing.ts`, per-type cost reconciliation, unavailable activity and cost,
ranges and local-time buckets, the provider filter, heatmap gaps, quota history,
focus-chart label placement and the resets agenda), the version 2 Accounts &
Settings model (registry sections, fixed slots, live, refused and coming actions,
flows, lines under rows, the trash, server visibility, the sign-in block, Update
apps results, the refresh scale), the Accounts & Settings controller against a
fake server (every action's request and every error code's words) and the
sign-in rules (strength hint, first-run checks, tries and pauses, ended and
revoked sessions).

Visual checks use the sanitized fixture preview in
`~/PM-Experiments/ccs-accounts-20260930/worktrees/preview/` (see its README):
it serves `dist/ui` with the concept's fixture data and screenshots it in
headless Chrome with SwiftShader WebGL. Compare against the approved Daylight
Atlas concept (`redesign-concepts-20261001/c-daylight-atlas/`) at 1024 x 700,
1440 x 900, 1920 x 1080 and 2560 x 1440, light and dark.

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
