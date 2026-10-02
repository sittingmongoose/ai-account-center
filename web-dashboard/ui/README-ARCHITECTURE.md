# Dashboard UI architecture (Slint 1.18.1, Daylight Atlas)

This is the map for the agents who build the pages on top of the W1 foundations. The approved design is
`~/PM-Experiments/ccs-accounts-20260930/redesign-concepts-20261001/c-daylight-atlas/` (index.html, styles.css,
analytics.css, app*.js). Jared's rules are in that folder's `BRIEF.md`, `ROUND2.md` (later sections win),
`TRAYS-AND-SIGNIN.md` and `ACCEPTANCE.md`.

## Layers

```
public/bridge.js          network, session, timers, URL state; the only code that talks to the API
public/view-model.mjs     DTO -> view model v2 for Home, Details, header, Update apps (pure, tested)
public/analytics-data.mjs DTO -> analytics view + analyticsSlintModel() v3 (pure, tested)
public/analytics-usage.mjs, analytics-quota.mjs, model-rates.mjs   the Analytics page's numbers and chart geometry
public/accounts-view.mjs  DTO -> Accounts & Settings view model v1 (pure, tested); LIVE lists what the server can do
public/auth-view.mjs      the sign-in page's words and rules: strength, setup checks, limiter headers (pure, tested)
public/claude-open.mjs    Claude Open progress: the 202 poller (injected I/O) and the row's progress line (tested)
public/*-data.mjs, *-confirmation.mjs, visible-usage.mjs   truthfulness and switching rules (tested)
src/lib.rs                wasm entry points; JSON -> Slint structs; persistent models updated in place
src/sync.rs               sync_rows() and Nested<T>: diff by id with set_row_data
src/analytics.rs          analytics v3 JSON -> the AxData global (persistent models)
src/accounts.rs           accounts v1 JSON -> the AcData global (persistent models)
ui/dashboard.slint        the shell: header, routing, overlays, sign-in layer; re-exports models.slint
ui/models.slint           every view-model struct (the contract between Rust and the pages)
ui/theme.slint            Theme / Motion / Type / Breakpoints globals and the embedded fonts
ui/components/*.slint     the component library
ui/pages/*.slint          Home, Analytics, Accounts & Settings, Details
ui/shell/signin.slint     the sign-in layer
```

Rules that keep it truthful: every number, label and "unavailable" decision is made in `public/*.mjs` and
covered by `web-dashboard/tests/*.test.mjs`. Slint only lays out and animates what it is given. Never compute a
reading in Slint, never add or average across accounts, never turn a missing value into 0.

## The data seam

- **Intent goes out through one callback**: `action(kind, value)` in Slint -> `window.ccsDashboardAction(kind,
  value)` in bridge.js. Kinds in use: `navigate` (home|analytics|accounts), `navigate-dashboard`,
  `navigate-analytics`, `theme` (light|dark|auto), `refresh`, `update-apps`, `logout`, `login`
  ("user\npassword"), `details` (account id), `details-closed`, `launch` ("profile:mac|windows"), `activate`
  (Codex profile), `antigravity-activate` (profile id), `automatic`, `threshold` ("95%"),
  `antigravity-automatic`, `antigravity-threshold`, `antigravity-pool`, `refresh-interval` ("1 min"),
  `activation-confirm`, `activation-cancel`, `accounts-show` ("provider:show|hide"), `antigravity-cooldown`
  (seconds), `setup` ("user\npassword\nconfirm\ncode"), `setup-typing` ("password\nconfirm"), `auth-recheck`,
  `login-limit-over`, the Analytics kinds listed under Analytics v3,
  `analytics-metric-key` (quota-history key), and the older `analytics-provider|account|metric|account-id|
  activity-interval`. Add new kinds; never repurpose one.
- **State comes in through wasm exports** (src/lib.rs): `set_dashboard(json)` (view model v2),
  `set_chrome(json)`, `set_auth(authenticated, json)`, `set_busy`, `set_theme_mode(0 auto|1 light|2 dark)`,
  `set_system_dark`, `set_reduced_motion`, `push_toast(kind, title, body, ms)`, `show_details(json)`,
  `close_details`, `set_update_status(json)`, `show_activation_confirmation(json)`,
  `close_activation_confirmation`, `set_analytics(json)`, `set_analytics_head(json)`,
  `set_analytics_trend_paths(json)` (one morph frame), `set_analytics_loading`, `set_current_page`,
  `set_refresh_interval`, `set_accounts(json)` (Accounts & Settings v1), `set_signin_strength(json)`.
- **Versioned JSON**: `VIEW_MODEL_VERSION = 2` (view-model.mjs) must equal `VIEW_MODEL_VERSION` in lib.rs,
  and `ANALYTICS_VIEW_VERSION = 3` (analytics-data.mjs) must equal `ANALYTICS_VIEW_VERSION` in analytics.rs;
  and `ACCOUNTS_VIEW_VERSION = 1` (accounts-view.mjs) must equal `ACCOUNTS_VIEW_VERSION` in accounts.rs;
  a mismatch is refused, not half-rendered. Bump the pair together when its structs change shape.
- **In-place updates**: lib.rs owns one `Rc<VecModel<T>>` per list (sections, cards, registry, toasts, Details
  meters/amounts/facts, processes, KPIs, quota groups) and a `Nested<T>` per nested list (a section's rows, a
  row's cells, a card's meters and amounts, a group's quota rows), keyed by id. `sync_rows` keeps rows whose
  key and data are unchanged, `set_row_data`s changed rows, inserts new ones and removes vanished ones. Slint
  repeaters keep their instances when the nested `ModelRc` is the same pointer, so a changed reading animates
  from its old value. Keep ids stable: rows by account id, cells by `"<account id>|<window key>"`.
- **Empty cells**: a `MeterView` with `key == ""` is an intentionally empty slot (a Pro account in the Fable
  column, a Codex account without a reported 5-hour window). A meter with `has-value == false` is a real
  window with no reading: it draws the dashed "unavailable" track and `na-text` ("Unavailable", "Not reported
  yet").

### View model v2 (Home)

`dashboardViewModel(data, ctx)` returns `{ version, sections, cards, registry, chrome }`.

- `sections`: Claude (`kind: "claude"`), then the switchable providers Codex and Antigravity (`kind:
  "switchable"`, `switchable: true`). Each has `columns` and `rows`; every row has one `cells` entry per column,
  plus `active`, `activeLabel` ("on Ubuntu"), `canActivate`, `activateKind` (`activate` |
  `antigravity-activate`), `profile`, `amountsLine` (Codex credits and banked resets), and `auto` (the
  section's auto-switch state, always "% used").
  - Lines that mix weights travel as runs, `{ text, strong, tone }` (`RunView`; tone `good` is the active
    account's name, `warn` a warning figure): `metaRuns` ("**4** accounts · desktop profiles", "**3** accounts ·
    **codex-2** active"), a row's `amountsRuns` ("**62.5K** credits · **1** banked"), `foot.runs`,
    `auto.offRuns` and `confirmRuns`.
  - `canSwitch`: more than one account to switch between; rows show Activate only then.
  - `auto.shown`: the header draws the toggle and the % used stepper (`auto.min`..`auto.max`, `auto.pool`);
    otherwise `auto.offRuns` ("Auto-switch **off** · needs a second account").
  - `foot` (Codex): `{ shown, warn, runs, when }`, what auto-switch does about the active account (the
    backend's own message while it is below the point; a warning naming the next account when it is above;
    "Checked 1m ago · every 1 min").
  - A row's `confirm`/`confirmRuns`: activating an account already at or past the section's switch point
    asks inline first ("**99% used**, above the 95% switch point. ... Activate anyway?"); only when the
    threshold is known. "Activate anyway" then runs the existing guarded activation.
  - Claude columns: 5-hour, Weekly, and Fable only when a Max account exists; Fable comes from the
    `seven_day_fable` window; absent is "Not reported yet"; Pro rows get an empty cell.
  - Codex: no 5-hour column unless an account reports the canonical `five_hour` window; Chat pass hidden;
    the auto-switch notch is drawn at the threshold when auto-switch is on, faint on inactive rows.
  - Antigravity: columns are every reported rate-limit window (Gemini before Claude and GPT, 5-hour before
    weekly); rows come from the dashboard DTO and become activatable only when the native inventory
    (`/api/antigravity/profiles`, commit 9cf75fbe) binds and verifies them (antigravity-data.mjs).
- `cards`: one per account of the other providers (Cursor, Muse Code, Kimi Code, Qwen, Z.ai, OpenCode Go), in
  registry order; up to three meters (a window with no reading is an unavailable meter), everything else as
  `AmountView`s (packs, balances, credits, spend). The plan drops the provider's own name ("High Usage"); the
  footer is `flag` ("Stale" after 30 minutes, or a status word like "Sign-in needed"), `sampled` and
  `platform`; `planNote` is the Qwen "Plan subscription ends ..." line (visible-usage.mjs marks the window with
  `planExpiry`) and `packsNote` says "All 3 packs expire ..." once instead of on every pack.
- `registry`: the provider registry derived client-side (`PROVIDER_REGISTRY`) with counts and `visible`.
  Providers listed in `data.settings.hiddenProviders` (when the backend sends it) are left out of sections and
  cards and marked `visible: false`.
- `chrome`: the header status line ("Updated **2m ago** · cached readings") plus username and host.

`detailsViewModel(data, id, ctx)`: every visible window of one account as a labelled meter (notches copied
from its row), amounts, facts (status, sampled, fetched, source, platform, profile, note) and the action state:
`subLead` ("Codex · Pro"), `canSwitch`, `activeLabel`, `activateHint`, `platform`, `confirm` and `confirmRuns`,
so Details shows the row's own action slot and asks the same inline question.

### Analytics v3

The Analytics page (W3) is built like the concept's `app-analytics.js`. Four pure modules make every number:

- `public/analytics-usage.mjs` `usageView(payload, state, { now, sizes })`: the header, KPI row, usage trend,
  cost by model, donut, session stats, token breakdown, cache efficiency, heatmap, daily cost and the custom
  range calendar. It buckets the response's UTC hours in local time (24H hourly, 7D four-hourly, 30D/All
  daily, Month and custom by span), validates and de-duplicates rows, and splits each provider's logged cost
  by type at its models' blended rates so the parts add up to the logged total (exact for the whole window).
  A missing cost estimate makes every cost unavailable; unavailable activity makes every block unavailable.
- `public/model-rates.mjs`: the rate table, aliases and fallback mirrored from `src/web-server/model-pricing.ts`
  (read only; `tests/model-rates.test.mjs` fails when they drift), the models.dev rates CCS resolved for the
  logged Codex models, and `reconcile(row)`: a model's split is kept only when its four parts add up to its
  logged `estimatedCostUsd`, otherwise it shows token shares and the card footnote says so.
- `public/analytics-quota.mjs`: `quotaView()` (one row per account by identity, the main window never Fable,
  step sparklines coloured by each run's value in a 600 x 24 viewbox, gaps and resets never bridged),
  `focusChart()` (shapes, dots, placed labels and hover stops in pixels; a label takes the first free spot or
  is dropped, so labels never overlap; Martian Mono is 7.15 px a character at 11 px, so label boxes are exact)
  and `agendaView()` (resets at the same minute merged, expiries with what is left, spent packs dropped, two
  balanced columns of whole days). Current readings come from the dashboard response, history from analytics.
- `public/analytics-data.mjs` `analyticsSlintModel(view, page)`: the version 3 JSON for `src/analytics.rs`
  (`ANALYTICS_VIEW_VERSION = 3`, its own version, checked there). It keeps the earlier `head`, `kpis` and
  `quotaGroups` and adds `state`, `usage`, `quota` and `agenda`.

`src/analytics.rs` writes it into the `AxData` global (`ui/pages/analytics/ax-data.slint`), with persistent
models for everything that animates (KPIs, model bars, donut arcs, stats, token bars, heat cells, daily bars,
quota groups and rows). Page state lives in bridge.js (`analyticsPage`): range, custom days, provider, the
trend toggles, donut and heatmap modes, open focus rows, compare and collapsed groups. Kinds:
`analytics-range` (24h|7d|30d|month|all), `analytics-custom` ("YYYY-MM-DD,YYYY-MM-DD", local days),
`analytics-provider` (all|claude|codex), `analytics-split`, `analytics-cache` ("true"|"false"),
`analytics-donut`, `analytics-heat`, `analytics-focus` and `analytics-compare` (account id), `analytics-group`
(provider), `analytics-refresh`, and `analytics-layout` ("kind,width,height": the trend, daily, heat and focus
boxes report their size, and bridge.js lays those charts out in pixels). The backend accepts 24h, 7d and 30d:
Month, All and custom ranges request the covering window (`apiRangeFor`) and are cut in the browser; when the
fetched window differs from the range, cost by model, the donut and session stats say which logs they cover.

Usage sources (activity `scope: multi-host-cli`): the server reads Claude Code and Codex on Ubuntu plus OMP,
Muse Code and zcode on Ubuntu, Mac and Windows. Only Claude Code and Codex are providers on this page (the
filter, the session rows, the daily chart, the model marks); `TOOLS` rows of the other tools count in the
totals, the trend, the heatmap and the model views under All only, merged by model name (`pageModels`; a model
that is not one provider's alone has `provider: ""`, no mark and the neutral `Theme.h-model` family). Cost:
`fallbackCostUsd` is the part of `estimatedCostUsd` the server priced only at its unknown-model fallback (no
logged cost, no listed rate); the page leaves it out, shows "Not logged" where nothing is logged (never $0.00)
and marks totals that leave it out "partial" (`notLoggedPart`). "Included usage" in the header
(`includedView`, from `activity.sources`) is a disclosure with one sentence and a tool x computer grid of each
source's state and last scan.

Motion: sections play their first view when they first scroll on screen (`AxReveal` reads its absolute
position against the page scroll). The trend draws on through a clip; on a range, filter or toggle change
bridge.js interpolates the 360-sample point arrays every frame and pushes only the eleven path strings
(`set_analytics_trend_paths`), while the axes cross-fade. Count-ups (`AxNum`) roll on one ease-out and land on
the exact text from bridge.js; the donut sweeps and morphs, the gauge arc and its number share one ease-out,
bars grow in sequence, rows of the quota history and the agenda rise in with a stagger. Nothing loops while
idle; the recent-sessions skeleton shimmers twice on first view.

### Accounts & Settings v1

The page (W4) is built like the concept's `app-accounts.js` and the Dashboard sign-in block of `app-auth.js`.
`accountsViewModel(serverData, ctx)` (public/accounts-view.mjs) returns `{ version, colA, colB, ag, policies,
refresh, update, signin, connection, about }`; src/accounts.rs writes it into the `AcData` global
(ui/pages/accounts/ac-data.slint) with persistent models (sections, rows by id, each row's actions, footers,
policies, Update apps hosts and their lines).

- **Providers**: every provider of the registry, the ones with their own sign-in flows (`colA`: Claude, Codex,
  Antigravity) before the rest (`colB`), even at 0 accounts (an empty line and its add action). `ACCOUNT_KINDS`
  holds each provider's kind line, source label, slot count and the concept's minimum actions width. Qwen and the
  OpenCode console wallet are browser sessions, never API keys (CONTRACT-registry-lifecycle section 7).
- **Rows**: identity and meta (plan · label), the status word only for an exception (Stale after 30 minutes, or
  the status word), "sampled 1m ago", the source and where; fixed action slots (`actions`, one `AcAction` per
  slot: `switch`, `button`, `icon`, `quiet`, `empty`). Switch state (active, `activeLabel`, `canActivate`, the
  inline confirm past the switch point) comes from the Home view model computed with no provider hidden, so a
  provider hidden on Home still switches here. The full row opens Details; nested actions never do.
- **What is live**: `LIVE` in accounts-view.mjs. Activate (Codex, Antigravity), Claude Open on Mac or Windows, the
  Codex and Antigravity policies (toggle, threshold, pool, cooldown through the existing PUT routes), the
  refresh interval and the theme are live. Add account, Sign in again, Replace key, Remove, app Sign in,
  Re-check, server-side visibility, Change password, other browsers and paired trays are drawn in their places,
  dimmed, with a `coming` caption and a tip that says what they will do; worktrees/status/W4.md lists the
  contracts. When a route lands: flip its `LIVE` entry and add its handler in bridge.js.
- **Show on dashboard**: saved in this browser (`localStorage['aac-hidden-providers']`) until the server stores
  visibility. bridge.js keeps the response as `serverData` and renders everything else from `data`, the same
  response with this browser's hidden providers added to `settings.hiddenProviders`, so Home, Details and
  Analytics honour it. A section hidden here says "Hidden in this browser"; one hidden by the server "Hidden
  everywhere".
- **Settings column**: Dashboard sign-in (signed in as, connection, this session from the stored sign-in time,
  and the coming items), Settings (Light/Dark/Auto, the usage refresh slider: any whole number of seconds from 30
  to 3600 on a log scale that snaps within 3.5% of its marks, saved on release; the auto-switch policies in %
  used), Update apps results by computer (Mac, Windows, Ubuntu; "Waiting for its turn" and a running bar while a
  job runs), Connection (read only) and About with `AboutSlint`.

### Sign-in (shell/signin.slint)

The real login screen. bridge.js builds `AuthView` with the words from public/auth-view.mjs:
- default, connecting (the scale bar sweeps), wrong (inline under the password, "N tries left before sign-in
  pauses for 15 minutes" from `RateLimit-Remaining` and `RateLimit-Policy`; the fields shake), limited (from a 429:
  "Try again in 15 minutes", the clock time from `Retry-After`, a mm:ss countdown and a draining bar; the rings
  dim), expired (a stored sign-in time says whether the session ran out after its hours or ended early),
  setup (only when `/api/auth/check` reports `accessMode: "setup"`), success (the button turns into a check, the
  bar fills with the Apex ramp, then the layer cross-fades into the dashboard load-in);
- the first-run form (username, password with the strength meter, confirmation with "Matches", setup code) shows
  only when `GET /api/auth/setup` reports `setupCodeRequired`, the signal that comes with `POST /api/auth/setup`
  (CONTRACT-auth-devices section 4); until then the setup state shows the command that sets sign-in up on the
  server and a Check again button;
- the network note says whether this address is plain HTTP, HTTPS or the dashboard host itself.

### URL state

The page routes `/`, `/analytics` and `/accounts` (also `/accounts/<provider>`; the server sends `/settings` to
`/accounts`), read and written by public/page-route.mjs. `?view=home|analytics|accounts` still works as an alias.
bridge.js pushes the path on navigation and follows `popstate`.

## Theme

`Theme` (theme.slint) holds every Daylight Atlas colour as a light/dark pair mixed by `Theme.mix`. Globals cannot
animate, so the Dashboard window animates one float (`theme-mix`, 300 ms in-out) and copies it into `Theme.mix`
on every frame: the whole UI cross-fades. `Theme.mode` is 0 Auto, 1 Light, 2 Dark; Auto follows
`Theme.system-dark`, which bridge.js keeps live from `prefers-color-scheme`. The choice is stored in
`localStorage['aac-theme']` (the old `ccs-slint-theme` is migrated).

Tokens: surfaces (`bg`, `paper`, `paper-2/3`), ink (`ink`, `ink-2..4`), rules, meters (`track`,
`track-hover`, `track-tick`, `fill-wash`), rows (`row-hover`, `row-sel`, `flash`, `sel-fill`, `sel-line`),
accent, navigation, the severity ramp (`calm|warn|crit|over` fills and `*-text`), shadows, scrim, tooltip,
heatmap, provider hues (charts only) and the chart palette (`chart-tokens|input|output|cache-write|
cache-read|cost`). Helpers: `Theme.severity(has-value, used)` (-1 unavailable, 0 calm < 80, 1 warning, 2
critical >= 95, 3 over > 100), `sev-fill`, `sev-text`, `provider-hue`.

`Breakpoints.width` is the window width; `compact < 1280 <= regular < 1600 <= wide < 2200 <= ultra`, with
`gutter` and `max-content` (2304 px). Desktop only (1024 to 2560+); no phone layouts.

## Motion

`Motion` durations: fast 140, med 260, slow 520, draw 900, stagger 32, theme 300, meter 1050 ms.
`Motion.reduced` (prefers-reduced-motion, and every headless capture unless the URL has `?motion`) makes
`Motion.on` false; every `animate` reads `Motion.on ? ... : 0ms`.

Easings are written inline (Slint has no easing-typed properties):
- `cubic-bezier(0.2, 0.8, 0.2, 1)` ease-out: every value-bearing animation (meter widths, count-ups, notch
  position, chart draws). **Values never overshoot.**
- `cubic-bezier(0.65, 0, 0.35, 1)` in-out: colour and opacity cross-fades.
- `cubic-bezier(0.34, 1.45, 0.64, 1)` spring: hover lifts, the nav indicator, presses, chevrons, panels only.
- `cubic-bezier(0.4, 0, 1, 1)` ease-in: quick exits.
- `cubic-bezier(0.32, 1.12, 0.52, 1)`: the slide-over (its 28 px bleed hides the overshoot).

Colour animations on elements (hover tints, flashes, toggles) must use `duration: Motion.tint(...)`: it is 0 ms
while the theme cross-fades (`Motion.theme-shift`), so every colour moves with `Theme.mix` in step instead of
lagging behind it with its own animation.

Load-in: the header slides down 8 px, sections rise 12 px one stagger apart (`Reveal`), meters sweep from 0
with their colour climbing the severity ramp, numbers roll up (`Meter.played`, `delay`). Page change: the
outgoing page fades and lifts 8 px (190 ms), the incoming page's sections rise in. Nothing loops while idle:
spinners and skeleton shimmer read `animation-tick()` only while they are active.

## Fonts and icons

- `ui/fonts`: Instrument Sans (default family), "Instrument Sans Tab" (tnum frozen; use `Type.tab` for every
  number that updates or aligns) and Martian Mono 87.5% width (`Type.mono`, the `Caps` component). OFL texts
  and `build_fonts.py` (fonttools + opentype-feature-freezer) sit next to them.
- `ui/icons`: curated Lucide 0.460.0 SVGs, stroke 1.75, tinted with `colorize`. Use `Icon { name: "refresh"; }`
  or `Icons.<name>`; to add one, drop the SVG in and add it to `components/icon.slint` (and `icons.json`).
- `ui/marks`: official provider marks (marks-v2, provenance and optical scales in `sources.json`) and the
  Simple Icons platform glyphs. `ProviderMark { provider: "codex"; size: 22px; }` applies the optical scale,
  cross-fades the light/dark artwork with the theme and never recolours a mark. Kimi Code is its official app
  icon with its plate (drawn 14% smaller). `PlatformGlyph { platform: "apple"; }` is tinted.
- `components/logo.slint`: Apex Soft as Slint Paths (`AppLogo { size: 26px; }` picks the pixel-tuned variant).
  `public/assets/favicon.svg` and `aac-logo.svg` serve the browser tab and the loading screen.

## Components (ui/components)

| file | component | notes |
|---|---|---|
| icon.slint | `Icon`, `Icons` | |
| mark.slint | `ProviderMark`, `PlatformGlyph`, `Marks` | |
| logo.slint | `AppLogo`, `AacLogoApex*` | |
| text.slint | `Caps`, `ValueUnit` | the % or unit is the same family and weight at 88% on the number's baseline |
| button.slint | `Button` | kinds default, primary, ink, accent-line, danger, ghost; `small`; `icon` or `platform`; exposes `pad-left`, `icon-size`, `icon-gap` for alignment |
| icon-button.slint | `IconButton` | 34/28 px, `bare`, `chevron`, `pressed-look`, `spinning` (eases out to rest) |
| segmented.slint | `Segmented`, `SegItem` | one sliding indicator |
| toggle.slint | `Toggle` | spring thumb; disabled draws off and flat |
| select.slint | `Select` | PopupWindow menu |
| menu.slint | `MenuPanel`, `MenuHead`, `MenuItem`, `MenuSeparator` | for PopupWindow menus |
| meter.slint | `Meter`, `MeterBar` | gradient fill, severity, ticks, threshold notch, overage segment, dashed unavailable track, no-overshoot sweep, count-up, hover card, `clicked` forwards to the row |
| card.slint | `Plate`, `Card`, `HoverShadow` | Plate's content and Card's body are cached layers; Card lifts 2 px and `HoverShadow` (a static large shadow) fades in behind it on hover; its body sits inside its TouchArea |
| reveal.slint | `Reveal` | load-in rise |
| hover-card.slint | `Hover` (global), `HoverLayer` | custom hover card and tooltips, 90 ms intent, glides between targets |
| skeleton.slint | `Skeleton` | shimmer only while `active` |
| toast.slint | `ToastStack` | spring up, countdown bar, fade, slot closes, then `toast-dismissed(id)` removes the row |
| slide-over.slint | `SlideOver` | the scrim only tints and lets clicks through (as in the concept): a row click swaps the content (`swap()`), any other click closes it (the window's background TouchAreas, and bridge.js `closeDetailsOnOutsideClicks` for clicks a button or card takes), Escape closes it |
| dialog.slint | `Dialog` | scrim + rising card; `dismissable` |
| field.slint | `Field` | text and password fields with show/hide |
| nav.slint | `NavBar` | the indicator glides between the three items |

Gotchas found while building W1 (Slint 1.18.1):
- `row`, `col`, `colspan`, `rowspan` are reserved (grid cells); `color` on a component inheriting Rectangle
  clashes with the deprecated alias; `focus` is reserved on components (use another function name).
- An element with a fixed size and no `x`/`y`, outside a layout, is centred in its parent: give overlays
  explicit `x: 0; y: 0`.
- `width` and `min-width` cannot both be set; a TouchArea placed inside a layout becomes a layout item.
- A TouchArea's `has-hover` stays true over its descendants, not over siblings drawn above it: put content
  that must keep the hover (a card's meters) inside the TouchArea.
- Globals cannot hold `animate`; animate in a component and copy into the global (`changed`).
- Flickable `viewport-*` are deprecated: use `content-*`.
- A `Path` keeps its viewbox aspect ratio by default (centred); charts and sparklines need `fit: fill`.
- Children passed through `@children` resolve `parent` at the use site: do not size them with
  `parent.width` (the Details panel fills the slide-over without an explicit size).
- An element created together with its data has no "previous value" to animate from: `Meter` waits one frame
  (`mounted`) before sweeping; do the same for anything that must animate in on first load.
- Layout items stretch by default: give fixed header items `horizontal-stretch: 0` so only the spacer grows.

Gotchas found while building Home (W2):
- **A GridLayout whose repeater starts empty and fills later can panic** in i-slint-core `layout.rs`
  (`index out of bounds: the len is 4 but the index is 4`); it showed in dark mode, where the theme switch at
  boot re-lays out between the model change and the repeater update. Create every repeated GridLayout only
  once its model has rows (`if model.length > 0: GridLayout { for ... }`). Later count changes (a provider
  hidden, 7 to 6 cards) were tested and are fine.
- `row`, `col` and `colspan` of repeated grid items can be runtime expressions; Home uses that so cards and
  sections re-flow on resize without being re-created (their meters keep their values).
- `ValueUnit` (and any element that centres its own child) needs an explicit `height` when placed outside a
  layout, or it centres twice.
- Slint Text has no line-height: give Texts the concept's line box as `min-height` (13.5 px body is 19.5 px,
  12 px meta 17.5 px, 18 px titles 26 px) with `vertical-alignment: center`, or rows come out shorter.
- `changed` callbacks fire for properties with bindings, so a Meter reports `value-changed` when its reading
  moves after the load-in and the row flashes.

Gotchas found while building Analytics (W3):
- `TouchArea.moved` fires only while a button is pressed: hover tracking uses `changed mouse-x` and
  `changed mouse-y` (with `has-hover`).
- A Text with `wrap: word-wrap` inside a HorizontalLayout reports a one-line preferred height; give it an
  explicit width (or put it directly in a VerticalLayout) before measuring a popover or card from it.
- A PopupWindow does not size itself: give it `height: <body>.preferred-height`.
- A TouchArea used for a tooltip consumes clicks; forward them (`clicked => { ... }`) when it sits on a row.
- Widths that depend on a card's own width inside its layout make a binding loop; derive them from
  `Breakpoints.width` instead.
- Slint has no exponent literals (`1e9`) and no dashed strokes: dashes are generated as subpaths in JS.
- `gen` is a reserved word in Rust 2024: do not name a struct field `gen`.
- `StyledText` with `@markdown("<font color='\{Theme.ink-2}'>**\{lead}**</font>\{rest}")` mixes weights in
  a wrapping line (the KPI subtitles); interpolated strings are escaped.

Gotchas found while building Accounts & Settings and sign-in (W4):
- `row` is reserved on components too (`in property <AcRow> row` fails): name it `item`.
- A wrapping Text given an explicit `width` from its container inside a layout makes a binding loop through
  every ancestor's min-width (Reveal included). Put the Text directly in a VerticalLayout with padding for its
  icon or label and place the icon or label absolutely beside it (`IconNote`, `FactLine`).
- A callback cannot share a name with a property (`setup` the bool and `setup(...)` the callback).
- A layout child cannot set `y`; use `transform-scale` or a wrapper for a small leave motion.
- A Toggle assigns its own `on` when clicked, which ends a plain binding: re-assign it from a `changed` handler
  on the model value (`ProviderSection.shown`, `PolicyRow.on`).
- `StyledText` with `@markdown` mixes weights in a wrapping line (the pairing note, the limited banner's time).

Gotchas found in the W5 review (concept comparison and performance):
- **Line boxes.** The concept's CSS sets line-height 1.45 (numbers in meters 1); Slint Text uses the font's own
  1.22 em box. Rows, heads and footers therefore set `min-height` on their Texts (13.5 px 19.5, 12.5 px 18,
  12 px 17.5, 16 px titles 23, 18 px 26, 11 px captions 15-16) and padding that includes the 1 px hairline the
  CSS border adds outside the padding. `ValueUnit.box-h` gives a number its line-height-1 box, and `Meter.top-h`
  is the concept's measured value line (20.7, inline 19.2, compact 17.5). Accounts rows are 61 px apart, Home
  rows 90, as in the concept.
- **A layout's paddings are already in its preferred size.** `Card` used to add them again, which made every
  provider card 28 px taller and spread the extra into its meters.
- **`opacity` and `cache-rendering-hint` are ignored on a component's root element** (the compiler only warns).
  Put them on an inner element (Plate wraps its children in `content`) or set them where the component is used.
- **Cached layers** (`cache-rendering-hint`) reuse their texture only while their size is unchanged. A size
  change allocates a new texture, and the framebuffer check that follows waits for the GPU: 0.1-0.3 s a frame on
  SwiftShader. Keep a cached layer's bounds stable: an AxCard whose hover readout (with its shadow) moves with
  the pointer sets `cached: false`, and the trend card caches only its plot (`chart-layer`).
- **Never animate `drop-shadow-*`.** Each change re-blurs a new shadow texture (same cost as above); fade a
  static second shadow instead (`HoverShadow`).
- **Content-sized columns.** The concept's agenda grid sizes TIME to its widest line and ACCOUNT to its widest
  (at most 300 px); each agenda line measures its own texts with hidden Text elements and raises the widths in
  `AxAgendaCols`, the way Accounts rows report their slot widths.
- **Opacity 0 still draws.** Give faded-out groups `visible: self.opacity > 0.005` (the trend's hidden token
  bands were tessellated every frame).
- **Missed redraws.** A state change pushed from JavaScript could stay unpainted until the next pointer event
  (seen at 1920 px and wider: the sign-in page kept "Connecting to AI Account Center…"). The push arrives
  outside winit's event loop; Slint's redraw request starts its frame-throttle timer, but a sleeping loop with
  no timer scheduled never runs it. `wake()` in lib.rs (called by `with_ui` and the other entry points)
  requests a redraw and posts an empty `invoke_from_event_loop` so the loop wakes and schedules the timer.

## Pages

- `pages/home.slint` (W2, built like the concept's Home):
  - `HomePage`: the sections in a GridLayout (stacked; Claude and Codex side by side at 7:5 from 1600 px,
    4:3 from 2200 px, the same height), "Other providers" with its count, and the cards: as many columns as
    fit at 300 px, spread over the fewest rows with the longer rows first, equal widths within a row (a grid
    over the rows' least common multiple), heights following content.
  - `AccountSection`: header (mark, title, meta runs; Claude's "?" legend popup and History; a switchable
    section's auto-switch toggle and % used `Stepper`, debounced 600 ms, the notch sliding first; or the off
    line), column heads, rows, the Codex footer. Column fitting: rows report `ident-natural` (mark, gap, the
    widest of email at the active weight, meta and credits, slack; clamped 150 to 320 px) and `slot-natural`;
    the section keeps the largest; meter columns share the rest equally (min 108 px). The selected-row
    highlight is one Rectangle per section placed from the active row's y and height: instant on re-layout,
    a 520 ms ease-out glide on a switch; it deepens on hover and flashes with its row.
  - `AccountRow`: the full row opens Details, nested buttons act on their own; `SwitchSlot` stacks the
    Activate button and `ActiveMark` (check-circle `CheckMark` drawn in by a clip, "Active", "on Ubuntu") on
    the button's grid (`pad-left`, `icon-size`, `icon-gap`), measured equal within 0.11 px at 1024, 1440 and
    2560 in both themes before and after a switch; `ConfirmLine` opens under the row (and `ConfirmBox` in
    Details).
  - `ProviderCard`: header with plan, inline meters (side by side from 560 px), plan note, amounts (two
    columns from 700 px, dashed separators), packs note, footer; a soft flash when a reading changes.
- `pages/analytics.slint` (W3): the page; `pages/analytics/` holds `ax-data.slint` (structs and the `AxData`
  global), `ax-common.slint` (colours, count-ups, card, switch, segmented control with marks, readout rows,
  tips, the first-view reveal), `ax-head.slint` (header, scope disclosure, custom range picker, KPI row),
  `ax-trend.slint`, `ax-models.slint` (cost by model, model popover, donut), `ax-stats.slint` (sessions, token
  breakdown, cache gauge), `ax-time.slint` (heatmap, daily cost), `ax-quota.slint` (quota history and focus
  charts) and `ax-agenda.slint`. Chart rows reflow 3 -> 2 columns at 1600 px.
- `pages/accounts.slint` (W4): the page layout by width (below 1280 px the settings sit under the providers in two
  columns; from 1280 a 380 / 420 / 440 px settings column beside them, the source column folding into the
  identity line from 1280 to 1439; from 2200 two provider columns). `pages/accounts/` holds `ac-data.slint`
  (structs and the `AcData` global), `ac-provider.slint` (the provider section, rows with fixed slots measured
  across the section, the selected-row highlight, the Antigravity policy box, the footer, `TipArea`,
  `ComingWord`, `IconNote`) and `ac-settings.slint` (the settings groups, `RefreshSlider`, `AboutSlint`).
- `pages/details.slint`: the Details content for the slide-over.
- `shell/signin.slint`: the sign-in page (see Sign-in above) with the contour motif.

## Visual check

`~/PM-Experiments/ccs-accounts-20260930/worktrees/preview/` serves `dist/ui` with sanitized fixtures and
screenshots it in headless Chrome with SwiftShader WebGL; see its README. `POST /__preview/state` changes the
data at runtime (hidden providers, example readings) and `shoot.mjs` steps `post`, `eval` and `film`
(a CDP screencast, optionally clicking or evaluating once it runs) cover refresh, switch and load-in motion.
