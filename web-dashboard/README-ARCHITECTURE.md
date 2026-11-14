# Dashboard UI architecture (Leptos 0.9, Daylight Atlas)

This is the map of the web dashboard: one Leptos app, the app frame, client-side rendered and compiled to
WebAssembly, served by the TypeScript server at every page address, and the tested JavaScript data layer it loads.
The design is Daylight Atlas (`~/PM-Experiments/ccs-accounts-20260930/redesign-concepts-20261001/c-daylight-atlas/`)
as redesigned for the frame (the Leptos plan, `share/ai-account-center/2026-11-08-leptos-redesign/`, and its
DESIGN-SYSTEM.md). Until Leptos finish (step 9, 2026-11-14) a Slint 1.18.1 canvas app drew Home, Analytics and
Accounts; it and its bridge (bridge.js) are retired, and the git history holds them.

## Layers

```
leptos/src/lib.rs           the entry: which tab an address opens (page_from_location), ?embed=1 content-only pages
leptos/src/frame.rs         the app frame: the five tabs, routing and Back, toasts, the offline copy, new-build notice
leptos/src/session.rs       the sign-in page (a real HTML form), drawn from public/frame-session.mjs's view
leptos/src/home.rs, details.rs, activation.rs   Home, Details and the switch approval, from the dashboard's views
leptos/src/accounts/        Accounts & Settings
leptos/src/analytics/       Analytics (feed.rs talks to public/frame-analytics.mjs)
leptos/src/status.rs, scoreboard.rs, cards.rs, pins.rs, resets.rs, ...   Status and the Model Scoreboard
leptos/src/ui/              components, chrome (header, tab bar, Duo rail and band), device tiers, charts, meters, icons
leptos/style/app.css        every style and the theme tokens (light and dark)
public/frame-session.mjs    the sign-in session: check, login, setup, limiter, ended and offline states
public/frame-dashboard.mjs  Home and Accounts' controller: the dashboard reads, timers, accounts controller, switch
                            approvals, Update apps, Claude Open and the Back entries
public/frame-analytics.mjs  Analytics' controller: its reads, poll cadence and page state
public/frame-seam.mjs       PAGE_ACTIONS, the action kinds a page may run
public/view-model.mjs       DTO -> view model v2 for Home, Details, header, Update apps (pure, tested)
public/analytics-*.mjs, model-rates.mjs   the Analytics page's numbers and chart geometry (pure, tested)
public/accounts-view.mjs    DTO -> Accounts & Settings view model v3 (pure, tested)
public/accounts-controller.mjs  the Accounts & Settings state machine with injected I/O (tested with a fake server)
public/account-actions.mjs  each action's request and each error code's words (pure, tested)
public/auth-view.mjs, transport.mjs   the sign-in page's words and rules: strength, setup checks, limiter headers
public/claude-open.mjs      Claude Open progress: the 202 poller (injected I/O) and the row's progress line
public/*-data.mjs, *-confirmation.mjs, visible-usage.mjs, device.mjs   truthfulness, switching and device rules
public/sw.js, sw-route.js   the service worker (offline copy) and its pure routing rules
```

Rules that keep it truthful: every number, label and "unavailable" decision is made in `public/*.mjs` and covered by
`web-dashboard/tests/*.test.mjs`. The Leptos pages only lay out and animate what they are given. Never compute a
reading in a page, never add or average across accounts, never turn a missing value into 0.

## The data seam

- **Views go to the pages as window events**: each controller builds the same versioned JSON the classic app drew
  and dispatches it: `aac-session` (the sign-in view, `session.rs`), `aac-dash` (`{key, json}`: `dashboard`,
  `accounts`, `accounts-strength`, `details`, `activation`, `update`, `busy`, `refresh-interval`, `chrome`;
  `dash.rs` keeps the latest of each as a signal) and `aac-analytics` (`analytics/feed.rs`). A page draws its
  loading state until the first view, never zeros.
- **Intent comes back through the controller**: a page calls `aacFrameDashboard.act(kind, value)` or
  `aacFrameAnalytics.act(kind, value)` (through `dash.rs` and `feed.rs`, with the controller's id, so a page only
  drives the controller it installed). Only the kinds in `PAGE_ACTIONS` (`public/frame-seam.mjs`) run: Details and
  rows (`details`, `details-closed`, `launch`), switching (`activate`, `antigravity-activate`,
  `activation-confirm`, `activation-cancel`, `automatic`, `threshold`, `antigravity-automatic`,
  `antigravity-threshold`, `antigravity-pool`, `antigravity-cooldown`), the Accounts & Settings kinds of
  accounts-controller.mjs, Settings (`refresh-interval`, `update-cancel`, `install-app`) and the Analytics
  controls. The header's Refresh and Update apps are the controller's own commands (`command`); sign-in, theme and
  Sign out are the frame's. Add new kinds; never repurpose one.
- **Versioned JSON**: `VIEW_MODEL_VERSION = 2` (view-model.mjs, `leptos/src/home.rs`), `ANALYTICS_VIEW_VERSION = 7`
  (analytics-data.mjs, `leptos/src/analytics/view.rs`) and `ACCOUNTS_VIEW_VERSION = 3` (accounts-view.mjs,
  `leptos/src/accounts/model.rs`) must match: a page refuses another version in words instead of drawing half of
  it. Bump the pair together when a shape changes.
- **What bridge.js did** is pinned by `tests/frame-dashboard-golden.test.mjs`: its server calls, views and toasts on
  the differential scripts were recorded from the real bridge.js before it was deleted, and the controller must
  give the same ones.
- **Empty cells**: a `MeterView` with `key == ""` is an intentionally empty slot (a Pro account in the Fable
  column, a Codex account without a reported 5-hour window). A meter with `has-value == false` is a real
  window with no reading: it draws the dashed "unavailable" track and `na-text` ("Unavailable", "Not reported
  yet").

### View model v2 (Home)

`dashboardViewModel(data, ctx)` returns `{ version, sections, cards, registry, chrome }`.
`ctx.includeHidden` keeps every account and provider in the action model for Accounts & Settings and Details;
Home leaves it off. The original dashboard and tray visibility choices stay in the DTO and registry.

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
    "Checked 1m ago · every 1 min"). Active and candidate accounts include hidden rows, labelled "(hidden)";
    a server-reported waiting outcome and candidate take precedence over the forecast.
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
- `public/analytics-data.mjs` `analyticsSlintModel(view, page)` (the name is historical): the version 7 JSON the
  Analytics page draws (`ANALYTICS_VIEW_VERSION = 7`, the same constant in `leptos/src/analytics/view.rs`, which
  refuses another version). It keeps the earlier `head`, `kpis` and `quotaGroups` and adds `state`, `usage`,
  `quota` and `agenda`.

`public/frame-analytics.mjs` reads `/api/accounts/analytics` (and the dashboard beside it), builds that view and
sends it to the page (`leptos/src/analytics/`, as `aac-analytics` events). Page state lives in
`public/analytics-page.mjs` (`createAnalyticsPage`): range, custom days, provider, the trend toggles, donut and
heatmap modes, the Cost by model sort, the open donut legend groups, open focus rows, compare and collapsed groups.
Kinds:
`analytics-range` (24h|7d|30d|month|all), `analytics-custom` ("YYYY-MM-DD,YYYY-MM-DD", local days),
`analytics-provider` (all, or a provider id from the picker), `analytics-split`, `analytics-cache` ("true"|"false"),
`analytics-donut`, `analytics-heat`, `analytics-cbm-sort` (cost|tokens), `analytics-donut-open` (`_other` or
`_undrawn`, toggles that legend group), `analytics-focus` and `analytics-compare` (account id), `analytics-group`
(provider), `analytics-refresh`, and `analytics-layout` ("kind,width,height": the trend, daily, heat and focus
boxes report their size, and the controller lays those charts out in pixels). The backend accepts 24h, 7d and 30d:
Month, All and custom ranges request the covering window (`apiRangeFor`) and are cut in the browser; when the
fetched window differs from the range, cost by model, the donut and session stats say which logs they cover.

Usage sources (activity `scope: multi-host-cli`): the server reads Claude Code and Codex on Ubuntu plus OMP,
Muse Code and zcode on Ubuntu, Mac, Windows, Nas1 (a second Ubuntu computer) and Alexis PC (a second Windows computer), and groups every row under the
dashboard provider that served it (`src/web-server/services/account-analytics-attribution.ts`): Claude Code,
Codex and the Muse Code CLI are their own provider; OMP and zcode record a route per call (`alibaba-token-plan`
is Qwen, `zai` and zcode's
`builtin:zai-coding-plan` are Z.ai, `kimi-code`, `opencode-go` and `opencode-zen` (OpenCode Go), `cursor`,
`muse-code` (Muse Code), `google-antigravity` (Antigravity), `anthropic` and `openai`). A route no provider
claims (a local vLLM server, OpenRouter) is `other`, never a guess. So every activity `provider` is a dashboard
provider id or `other`; tools appear only in `activity.sources`, `providers[].tools` and `models[].tools`. The
page labels providers with the response's provider table (`payload.providers`), marks each with its official
mark and hue, and merges one model's rows across providers by name (`pageModels`; a model that is no one
provider's alone has `provider: ""`, no mark and the neutral `Theme.h-model` family). Cost: `fallbackCostUsd` is
the part of `estimatedCostUsd` the server priced only at its unknown-model fallback (no logged cost, no listed
rate); for every provider, including Claude and Codex, the page leaves it out, shows "Not logged" where nothing
is logged (never $0.00) and marks totals that leave it out "partial" (`notLoggedPart`). A model with no known
rate is never priced at a guess. "Included usage" in the header (`includedView`,
from `activity.sources`) is a disclosure with one sentence and a tool x computer grid of each source's state and
last scan: a 660 px popover on D whose registered computer columns never elide; below D it
is as wide as the content allows, and a body too narrow for 120 px columns (always on P) lists the cells grouped by
tool instead. The states are honest end to end (FW4-N10): `ok` (read, with its age), `no usage in range` (scanned,
nothing logged in the window), `cached` (the last scan failed or was cut short; older numbers are shown, with
their age and the reason), `scanning` (a scan is working on the tool, or ran out of time before it reached or
finished it - a truncated cold scan never says "unavailable"), `not installed` (the host was scanned and the
tool is absent) and `unavailable` (a real failure, with a short plain reason in the tip).

Seeing every provider's usage (models stay the division of the charts): the provider picker (`providerChoices`,
`PickerButton` in the header) lists All and every provider that served usage in the range, in the dashboard's
order with its mark and tokens, Other last; picking one narrows every block to the usage it served. "Tokens by
provider" (`providersView`, `AxProvLines` under the KPI cards) is a summary of each provider's tokens in the range,
largest first, summed from the same hourly rows as Total tokens, packed into lines that fit the reported width.
The server publishes every model with usage (bounded at 500, ranked by cost then tokens), and Cost by model lists
them all, by cost or by tokens (`cbmSort`). The donut's "N smaller models" (`_other`) and, by cost, the models
with no arc (`_undrawn`: not logged or $0.00) are legend groups that open into their models (`donut.legend`: seg,
group and child rows; a child lights its group's arc and opens its model detail). The daily chart stacks Claude,
Codex and the other providers together (a picked provider alone, in its hue), and its readout lists each
provider's cost. With other providers in range the trend readout names each one's tokens in the bucket
(`byProvider`), and a model's detail says who served it and which logs hold it ("Muse Code · OMP and Muse Code
logs"). Sessions under All are the server's distinct total; the remote computers' logs add usage but no sessions.

Motion: sections play their first view when they first scroll on screen. On a range, filter or toggle change the
controller sends the shape to end on with `analytics-motion: morph`, and the page glides the trend from what is on
screen (`leptos/src/analytics/trend.rs`, 520 ms, the same easing as before); count-ups land on the exact text the
view gives, and nothing loops while idle. Analytics arms only on a useful answer (`usage.ready`, FW4-N10): a cold,
still-converging response stays behind the loading screen, which names each computer's scan progress ("Mac: 0 of 5
tools · scanning now", `hostProgress`); once armed, background updates show one calm Updating line in the header,
poll answers render as morphs, and the sources-grid cells are keyed by host, so a state flip updates in place.

### Accounts & Settings v3

The page (W4, bound to the server in B5W) is built like the concept's `app-accounts.js` and the Dashboard sign-in
block of `app-auth.js`.
`accountsViewModel(serverData, ctx)` (public/accounts-view.mjs) returns `{ version, accessMessage, colA, colB, ag, policies,
refresh, update, signin, connection, about }`; the page (`leptos/src/accounts/`, `ACCOUNTS_VIEW_VERSION = 3` in
`model.rs`) draws it with lists keyed by id, so a changed row updates in place.

- **Providers**: every provider of the registry, the ones with their own sign-in flows (`colA`: Claude, Codex,
  Antigravity) before the rest (`colB`), even at 0 accounts (an empty line and its add action). `ACCOUNT_KINDS`
  holds each provider's kind line, source label, slot count and the concept's minimum actions width. Qwen and the
  OpenCode console wallet are browser sessions, never API keys (CONTRACT-registry-lifecycle section 7).
- **Rows**: identity and meta (plan · label), the status word only for an exception (Stale after 30 minutes, or
  the status word), "sampled 1m ago", the source and where; fixed action slots (`actions`, one `AcAction` per
  slot: `switch`, `button`, `icon`, `quiet`, `empty`). Switch state (active, `activeLabel`, `canActivate`, the
  inline confirm past the switch point) comes from the Home view model computed with no provider hidden, so a
  provider hidden on Home still switches here. The full row opens Details; nested actions never do.
- **What is live comes from the server**: a provider's `signIn` and `capabilities` (dashboard `providers[]`) and
  each account's `actions` and `removeRefusal` (`GET /api/accounts/registry`) decide every control (`gate()` in
  accounts-view.mjs). A control is drawn live, off with its reason in plain words (a trusted connection needed,
  the CLI missing, one account only), "refused" (dimmed but clickable: the click opens the reason under the row,
  act `refuse`), or "coming" only when the server has no flow or route for it yet (`not_implemented`, a tray
  toggle before the server stores it, Delete now in the trash). A computer's default Claude profile (`isDefault`,
  or a profile folder named "Claude" in any case; the server applies the same rule) is always refused, whatever
  the server reports. While an account-switch confirmation is pending, the kinds that send a change
  (`MUTATING_ACTIONS` in accounts-controller.mjs) and Sign out wait, with a toast; opening, closing, typing,
  copying and cancelling a sign-in stay live. Sign out is never held by a running save or Claude Open.
- **Flows** (`flowView`, `leptos/src/accounts/flow.rs`): one panel under a section, opened by Add, Sign in
  again, Replace key or a footer Sign in: the step bar, the title and body, one field (profile name, a masked API
  key with an optional label, a pasted code), the verification page and code with Copy and Open, the waiting
  spinner, success or the error in plain words, and its buttons. The panel's height eases open and between steps;
  each step slides in 10 px and fades. A key or code stays in its field until it is submitted, is sent once, and
  the field is cleared at once. Sign-in jobs are polled every 2 s (accounts-controller.mjs). A job still running on
  the server (`GET /api/accounts/registry` `jobs[]`) opens its flow again after a reload or in another browser,
  unless this page closed it; a 409 `job_running` with its `jobId` opens the running job instead of a refusal.
- **Lines under rows** (`lineView`, `RowLine`): a remove or restore confirmation with the server's effect
  sentences and the one-use token, a refusal with its reason, or a request in flight; a removed row fades while
  it keeps its place. The Claude trash lists each profile with Restore and when it is deleted for good.
- **Show on dashboard / Show in tray**: saved on the server (`PATCH /api/accounts/visibility`; PUT remains for full lists); every browser
  follows. Each save sends hide/show operations for only the changed items, applied under the server file mutex. The saves run one at a time, and the toggles wait on `visPending`, not on the shared busy slot. A choice an older build saved in this browser (`localStorage['aac-hidden-providers']`) moves to the
  server once and is then cleared. Home honours `settings.hiddenProviders` and `settings.hiddenAccountIds`. Show in
  tray is live once the dashboard response carries `settings.trayHiddenProviders`.
- **One account's Dashboard / Tray switches** (`accountSwitches`, the small toggle pair beside each row's actions):
  "Dashboard" reads and saves only `settings.hiddenAccountIds` (action `account-show`), "Tray" only
  `settings.trayHiddenAccountIds` (action `account-tray`), so the two never change each other and all four
  combinations exist. A row hidden from the dashboard stays listed here with its identity dimmed. Both wait on
  `visPending` (`acct-show:` / `acct-tray:`) and redraw the saved state when a save ends, saved or refused. The
  trays follow only `providers[].trayVisible` and `accounts[].trayHidden`.
- **Test hooks**: with `?e2e` in the address the controllers keep the last Accounts view (`window.__aacLastAccounts`),
  the last Analytics view (`window.__aacLastAnalytics`) and the toasts (`window.__aacToasts`) for the browser kit;
  controls carry `data-probe` names. Without `?e2e` nothing is kept.
- **Settings controls**: model-backed segments, selects and cleanup use a pending display shadow that re-syncs on model changes and save settlement. Usage-log fields retain their draft across tool changes; a successful add increments `logSources.nonce` to clear the draft. `accessMessage` replaces the Accounts content when dashboard sign-in is off; protected APIs keep their guards.
- **Settings column**: Sign-in & connection (signed in as, connection, this session and other browsers from
  `GET /api/auth/session` with Sign out other browsers; the trusted local network line and note from
  `GET /api/auth/network` with Turn off, and Turn on only on the dashboard computer (the saved
  `GET /api/auth/check` follows the change, and is read again at every sign-out and ended session, so the
  sign-in page's note says how the password travels now); the password change with
  its strength meter, "Also sign out other browsers" and a toast that names the paired trays that stay signed
  in; paired trays from `GET /api/auth/devices` with Revoke and Sign out all devices, each with an inline
  confirmation; Sign out), Settings (Light/Dark/Auto, the usage refresh slider: any whole number of seconds from 30
  to 3600 on a log scale that snaps within 3.5% of its marks, saved on release; the auto-switch policies in %
  used; the extra usage-log host select lists each tool's hosts by label from `LOG_SOURCE_HOSTS` through
  the view's `logSources` hosts), Update apps results by computer (Mac, Windows, Ubuntu, Nas1; the Nas1 row uses the
  Ubuntu glyph; "N of `expectedResults` done", 40 when a job names no count; "Waiting for its turn" and a running
  bar while a job runs), Connection (read only) and About (the version and the credits).

### Sign-in (session.rs, frame-session.mjs)

The frame's sign-in is a real HTML form (`<form method="post" action="/api/auth/login">`, `autocomplete` names on
every field), so password managers fill and save it. `public/frame-session.mjs` makes the same calls as the classic
page did (`GET /api/auth/check` and `/api/auth/setup`, `POST /api/auth/login`, `/api/auth/setup` and
`/api/auth/logout`) and takes every word from `public/auth-view.mjs`:
- default, connecting, wrong (under the password, "N tries left before sign-in pauses for 15 minutes" from the
  answer's `triesLeft`, else `RateLimit-Remaining`), limited (from a 429: the time sign-in opens again and a mm:ss
  countdown on the button; the fields are off until it ends), expired (a stored sign-in time says whether the
  session ran out after its hours or ended early; `signedOutReason: "revoked"` or a 401 `session_revoked` says
  "Signed out from another browser"; the title asks to sign in again), setup, offline (no answer, or a 5xx: the
  offline card, retried every 15 s while shown and when the browser comes online), success.
- The first-run form (username with its rule, password with the strength meter, confirmation, setup code with where
  it is printed and saved) shows only when `GET /api/auth/setup` reports `setupCodeRequired`; until then the setup
  state shows the command that sets sign-in up on the server and Check again.
- The network note follows `/api/auth/check` and is read again at every sign-out and ended session; the page also
  says how long a remembered session lasts and that the trays keep their own device tokens.
- A sign-in through the form on Home opens the tab this device showed last (`aac-last-page`, DESIGN-MOBILE.md 4.5.4).

### Addresses

The frame's addresses are `/app/home`, `/app/analytics`, `/app/accounts`, `/app/status` and `/app/scoreboard`. The
server answers every page address with the frame page (`src/web-server/static-ui.ts`): those five and the classic
app's `/`, `/login`, `/analytics`, `/accounts`, `/accounts/<provider>`, `/status` and `/scoreboard`; `/settings` and
`/home` redirect. The page reads its tab from the address (`?view=<page>` first, the classic alias) and writes its own
`/app/<tab>` address, dropping `view`, `classic` and `content`. The installed app starts at `/`. A device that once
chose the classic app (`aac-frame=classic`) lands on the frame like any other. `?embed=1` shows the Status or
Scoreboard content alone (no chrome).

## Theme, tiers and motion

- **Theme**: `style/app.css` holds every Daylight Atlas colour as a token, set for light, for dark by the system
  scheme and for an explicit Dark (its THEME TOKENS block, first generated from the retired `theme.slint`; the
  stylesheet test keeps the three sets complete and alike). The choice is `localStorage['aac-theme']`
  (auto, light, dark; an older build's `ccs-slint-theme` still counts), applied as `html[data-theme]`, and the
  theme-color metas follow it.
- **Tiers and chrome**: `leptos/src/ui/device.rs` picks the chrome (`html[data-chrome]`: the desktop header, the
  phone's top and tab bars, the Duo's status-strip rail or its status-band head) from the window and the safe-area
  insets, and keeps the safe areas, keyboard and orientation current. Phone and Duo layouts are compact (small text,
  columns wherever the width allows).
- **Motion**: values never overshoot (meters sweep and count up on one ease-out); colour and opacity cross-fade; the
  selected-row highlight glides; nothing loops while idle; `prefers-reduced-motion` turns motion off. While a job
  runs, the header's Update apps spins and counts its results ("Updating 3 of 21") and shows a check when it ends.
- **Fonts and icons**: Instrument Sans (`public/assets/InstrumentSans.ttf`) and Martian Mono
  (`leptos/fonts/MartianMono.ttf`, served at `/app/MartianMono.ttf`); Lucide line icons and the official provider
  marks in `leptos/src/ui/icons.rs` (`leptos/src/ui/marks/sources.json` is the marks' provenance record).

## Builds and checks

`node scripts/build-ui.js` (`bun run ui:build`) builds the crate with wasm-pack, strips the wasm's name section,
content-addresses it (`pkg/<buildId>/`), stages `app/index.html`, `app/app.css` and the mono font, stamps the
service worker with the shell build id and writes `ui-build-manifest.json`; `bun run ui:validate` checks the build
is the build of the current sources; `scripts/verify-bundle.js` checks the package. The pages' own logic is tested
with `cargo test` in `leptos/` (on a build host, never the shared VM: `AAC_NO_LOCAL_RUST_BUILD=1`), the data layer
with `node --test web-dashboard/tests/*.test.mjs`, and the pages in a browser with the kit in
`web-dashboard/tests/browser/` (see its README). `leptos/NOTES.md` has the lessons from building them.
