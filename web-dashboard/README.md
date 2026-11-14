# AI Account Center web dashboard (Leptos 0.9)

The production account dashboard is the app frame: a Leptos 0.9 app (client-side rendered, compiled to WebAssembly
from `leptos/`) in the Daylight Atlas design, with Instrument Sans, Martian Mono, Lucide icons, the official provider
marks and the Apex Soft logo. The server answers every page address with it (`/`, `/app/<tab>` and the older
`/analytics`, `/accounts`, `/status`, `/scoreboard` addresses). Its controllers in `public/` (`frame-session.mjs`,
`frame-dashboard.mjs`, `frame-analytics.mjs`) own the network, the session, the timers, switch approvals and every
server call, and build each page's versioned view with the tested pure modules (`view-model.mjs`,
`accounts-view.mjs`, `analytics-*.mjs` and the rest); the pages draw those views and send their action kinds back.
[README-ARCHITECTURE.md](README-ARCHITECTURE.md) is the map. Until 2026-11-14 a Slint 1.18.1 canvas app drew Home,
Analytics and Accounts; it is retired (the git history holds it).

## Build

Rust **1.99.1** with `wasm32-unknown-unknown`, and wasm-pack **0.15.1** (`leptos/rust-toolchain.toml` pins the
compiler, target and formatter):

```sh
cargo install wasm-pack --version 0.15.1 --locked
bun run ui:build      # node scripts/build-ui.js
```

`leptos` is pinned to **=0.9.0-rc1** and the wasm-bindgen crates to the CLI wasm-pack downloads (0.2.130); the
builder checks both locks and the wasm-pack version before it replaces the packaged UI. The release build uses
`opt-level = "z"`, LTO and one codegen unit; the builder strips the wasm's `name` section (about 90% of a release
build) with `leptos/build/strip-names.cjs`. `leptos/.cargo/config.toml` keeps WebAssembly tail calls and SIMD off
(WebKit bug 325447 crashed iOS 27 on tail calls). The builder stages `public/` into `dist/ui/`, the runtime into
`pkg/<buildId>/` (named after the wasm's hash, cached forever), `app/index.html` (the frame page, naming that
runtime and its stylesheet by content), `app/app.css` and the mono font, stamps the service worker with the shell
build id, writes `.br` and `.gz` copies and records it all in `ui-build-manifest.json`. A machine that sets
`AAC_NO_LOCAL_RUST_BUILD=1` (the shared VM) refuses to build; build on a build host. Use the existing AI Account
Center server and session-cookie authentication. Do not serve the page as file:// or configure a cross-origin API
URL.

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
  remaining). Busy activation opens the frame's review dialog listing the blocking
  programs. Yes submits the server-issued, target-bound approval token; Cancel
  sends no mutation. Expired or stale approvals require a fresh Activate review.
  The active account is the selected row, marked Active.
- Update apps is its own header button. It starts the allowlisted asynchronous
  update job only after the user presses it, shows progress in place ("Updating
  3 of 21" with a spinner, then a check) and the result as a toast; it is never
  run while the dashboard starts. The results are listed under Accounts &
  Settings.
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
  from OMP, Muse Code and zcode (Ubuntu, Mac, Windows and Nas1) is grouped under the
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
- Status and Scoreboard are tabs after Accounts (`/app/status`, `/app/scoreboard`), each reading its own data.
  Contract: [docs/status-tab.md](../docs/status-tab.md).
- Reported usage can exceed 100%. Amounts and percentage labels preserve the
  overage; only visual progress-bar widths are bounded by their tracks.
  Displayed fractions use at most two decimal places; source precision is retained.

## Performance

The frame downloads about 1.3 MB on a cold start (1 MB of it the brotli wasm) and draws Home in about half a second on
the kit's test server; the retired Slint app downloaded about 3.9 MB and drew after about 1.5 s (Leptos finish
measurements, `share/ai-account-center/2026-11-14-leptos-finish/`). Each tab reads only its own data, its module loads
on first use, and nothing loops while idle.

## Validation

```sh
node --test web-dashboard/tests/*.test.mjs
bun run ui:validate          # pins, cargo fmt, these tests, and that dist/ui is the build of the current sources
cargo test --locked          # in web-dashboard/leptos, on a build host
```

The data tests cover unknown versus genuine zero, credit units/expiration, additional counters, confirmed
auto-switch thresholds, complete details, the version 2 dashboard view models (sections, Fable, switchable
providers, hidden providers) and the version 7 analytics model (rate mirror parity with `model-pricing.ts`, per-type
cost reconciliation, unavailable activity and cost, ranges and local-time buckets, the provider filter, heatmap gaps,
quota history, focus-chart label placement and the resets agenda), the version 3 Accounts & Settings model, the
Accounts & Settings controller against a fake server, the frame's controllers (`frame-dashboard.test.mjs`, with
`frame-dashboard-golden.test.mjs` replaying what bridge.js did; `frame-session.test.mjs`; `frame-analytics.test.mjs`)
and the sign-in rules (strength hint, first-run checks, tries and pauses, ended and revoked sessions).

Browser checks run with browser-check: see [tests/browser/README.md](tests/browser/README.md).

## Supported sizes and mobile testing

The frame is responsive from a 320 px floor to desktop widths, with its own chrome per format (`leptos/src/ui/device.rs`,
`html[data-chrome]`): the desktop header, the phone's top and tab bars, and on the iPhone Duo the status-strip rail
(folded and open on its side) or the status-band head (open upright). Safe-area insets, the keyboard and orientation
are followed live. The device pass (iPhone 17 Pro, iPhone Duo and Android simulators through Simlock) is
`share/ai-account-center/DEVICE-CHECKLIST.md`; the kit's `?safe=t,r,b,l` gives Chromium the measured Duo insets.

## Dependencies and attribution

The dashboard application source follows the parent CCS project's MIT license. Leptos and wasm-bindgen are used under
their MIT or Apache-2.0 licenses. Instrument Sans and Martian Mono are used under the SIL Open Font License 1.1, the
icons are Lucide (ISC) and the platform glyphs Simple Icons (CC0); see `public/assets/THIRD-PARTY-NOTICES.txt`.
Historical credit: from 2026-10 to 2026-11-14 Home, Analytics and Accounts & Settings were drawn with
[Slint](https://slint.dev) 1.18.1 under its Royalty-free license, which showed the standard AboutSlint widget in
About while Slint shipped.
