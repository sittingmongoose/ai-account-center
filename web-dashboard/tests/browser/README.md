# AAC dashboard browser checks

These are browser checks for the web dashboard (the Leptos app frame, compiled to WebAssembly), run with browser-check (`browser-check guide`, or `Agent-Guides/browser-check.md` on the Cursor share).

```sh
browser-check run web-dashboard/tests/browser/checks                  # the whole kit
browser-check run web-dashboard/tests/browser/checks --grep smoke
browser-check run web-dashboard/tests/browser/checks --set home=/tmp/fixture-home   # a prepared CCS_HOME (copied)
```

## What a run does

- **Build needed.** It serves this checkout's `dist/ccs.js` and `dist/ui` (it needs `dist/ui/app/index.html`), so run `bun install` and `bun run build:server` first. Build `dist/ui` with `bun run ui:build` on a build host (the VM refuses: `AAC_NO_LOCAL_RUST_BUILD=1`), or for a first look copy the installed package's `dist/ui`.
- **Isolated home.** The server starts with `CCS_HOME` and every home-like variable pointing at a temporary folder: `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, the XDG folders, `CODEX_HOME` and `CLAUDE_CONFIG_DIR`. Some collectors read the real home, so with only `CCS_HOME` set, real account emails and usage appear. Never point `home=` at a real home.
- **Real sign-in.** `t.app.signIn()` goes through the real loopback setup (`POST /api/auth/setup`), or logs in once setup has happened. It runs as a `fetch` inside the page, so the session cookie lands in that page's jar.
- **Pages.** `t.app.open({ page })` opens `/app/<page>?e2e` and waits for the frame (or, with `signIn: false`, the sign-in page). With `?e2e` the controllers keep their last views and toasts on `window` (`__aacLastAccounts`, `__aacLastAnalytics`, `__aacToasts`) and controls carry `data-probe` names; checks judge the DOM, so `t.audit` checks every control itself.

## Checks

| Check | What it asserts |
|---|---|
| `checks/smoke.check.mjs` | Signed out (the sign-in form), desktop at 1920, the widths where overflow bugs showed up (1360, 1430, 1600, 2200) and a 390 px phone: Home, Analytics and Accounts audited, no sideways scroll. |
| `checks/accounts-clicks.check.mjs` | Every account row's click (Details) and the provider header switches keep the page on screen at ten widths and on a phone; the Antigravity policy box keeps its choices inside it. |
| `checks/claude-signins.check.mjs` | Claude sign-in renewal: the notice once, the header and rows, and Details from the Accounts rows. |
| `checks/frame.check.mjs` | The frame's tabs, address and Back, the Duo rail and band, the new-build notice, `/` serving the frame and every classic address landing on its tab, and the offline start. |
| `checks/page-load.check.mjs` | No tab loads or waits on the retired Slint dashboard; `load timing:` records each tab's cold and warm load. |

The other files cover Home, Accounts & Settings, Analytics, Status, the Scoreboard, sign-in, overlays, reset claims and the pages together; each says what it asserts at its top.

**Status on the real pins (opt-in).** `checks/status-real-pins.check.mjs` writes every pinned thread's roster row and sheet, plus the page's words, to `real-pins-<format>.json` in the run folder. Two builds can then be compared line by line, for example the tree in `--set repo=<other tree>` against this one:

```sh
browser-check run web-dashboard/tests/browser/checks --grep 'status real pins' \
  --set realPins=1 --set reports=<copy of the reports share> --set registry=<copy of the pm-mail registry> [--set shoot=<title>]
```

- **The reports.** `reports=` and `registry=` are copied into the run's temporary home, because the server writes reader receipts beside the reports. Pass a copy, never the share itself.
- **The pins.** `realPins=1` reads this machine's T3 pins read-only (`AAC_T3_STATE_DB`). It links the real `~/.ssh` into the temporary home, so ssh's `Include ~/...` lines resolve for the other machines' pins. Teardown unlinks it before removing the home.
- **Nothing else is real.** HOME and every other home-like variable stay temporary, the doorbell is off, and nothing is answered.

`checks/status-page.check.mjs` and `checks/scoreboard-page.check.mjs` cover the Status and Scoreboard pages: deck flows, the layout in each format (with `?safe=t,r,b,l` giving the measured Duo insets), the `?embed=1` page drawing no chrome, and honest words when `spendByStream` or `byModel` are missing. `frames/` (its own config) shoots whole-page images for review: `browser-check run web-dashboard/tests/browser/frames --set out=<folder>`, needing only `web-dashboard/leptos/pkg` built.

Frames that aren't listed are saved as snaps for `browser-check diff <base run> <candidate run>`.

`frames/duo-poll.check.mjs` covers ordinary ten-second status polls while a pin sheet is being read or another
overlay is above it. It requires stable sheet/media DOM, retained internal scroll and focus, newest-overlay
paint/Tab/Escape ownership, and safe removal of a lower pin. A changed task note, deliverable description and
media caption must update in place while disclosure state, media playback position and focused controls survive.
The media fixture uses the development server's HTTP Range support and waits for a completed seek before
polling. It uses only synthetic status responses and media.
`frames/duo-fixes.check.mjs` also swaps left/right and top/bottom insets after the final settle pass, with unchanged
viewport and combined probe dimensions. For those edge regressions, add `--set rawProbe=1` to exercise the
computed-padding seam. Each edge must update safe properties and content clearance without a viewport event.

## Recording, machines and parallel runs

- **Parallel runs:** each gets its own browser, temporary home and reserved port. Use a worktree per candidate or `--set repo=<tree>`: rebuilding `dist/` during a run breaks that run. `--set port=N` fixes the port, so don't use it in parallel.
- **Recording:** `--record` works only with `--grep desktop`, because `signed out` and `phone` use `t.newContext` and `widths` uses `t.viewport`. It fails on the Mac, where tab capture stops pixel checks.
- **Other machines:** `--host` doesn't work, because the server listens on 127.0.0.1 and setup is loopback-only. Run from a checkout on that machine instead.
- **Other settings:** `--set repo=<tree>` tests another tree, `keep=1` keeps the temporary home, `serverTimeout=<ms>` extends the 240 s start limit.
- **Motion:** the page turns motion off in HeadlessChrome. For `t.moving`, open with `{ query: 'motion' }`.
- **`browser-check diff`:** freeze the clock and override `matchMedia` so `(hover: hover)` is true: headless answers false, and CDP emulation ignores it. Capture the base twice; a whole-frame 1 px shift in both is scroll noise.

## Writing checks for a change

```js
export default async function (t) {
	await t.app.stub('/api/accounts/dashboard', fixture);   // synthetic data, shaped like src/web-server/services/*-types.ts
	await t.app.open({ page: 'analytics' });
	await t.page.waitForSelector('.ax-page .kpi');
	await t.crop('quota-head', '.ax-page .quota-head', 'quota header title at 1360 px: not cut off');
	await t.audit('analytics 1360');                            // covered controls, overflow, clipped text, console
}
```

- **Helpers:**
  - `t.app.open({ page, signIn, query })` opens `/app/<page>?e2e` (and `query`), signed in unless `signIn: false`.
  - `t.app.signIn()`
  - `t.app.stub(path, json)`
  - `t.app.noResets()` answers every account's reset inventory as `unavailable`, so Details on synthetic accounts reads no 503 where the server has no native reset backend (the VM). Not for checks about resets.
- **Selectors:** controls carry `data-probe` names (`[data-probe="show:claude"]`); each tab is a `.route`, and the one on screen is `.route:not([hidden])`.
- **Synthetic data only:** made-up names and emails, never real account data.
- **DPR:** judge HiDPI on a real device; Playwright's DPR emulation is not the same as a phone's screen.

`frames/duo-fixes.check.mjs` reproduces delayed Duo insets and scrolled pin sheets with the synthetic fixture server
(`browser-check run web-dashboard/tests/browser/frames --grep 'duo regression'`). `?safe=t,r,b,l` initializes the
same padding probe that reads `env()` on devices; only with that override the probe is marked `data-safe-probe`.
Changing its inline padding after load simulates delayed insets without a resize. It affects display only.
The startup/lifecycle checks suppress only that probe's observer to exercise the 1.5-second settle fallback.
`--grep 'duo interactions'` checks Escape, focus return, scrim tap, fling and fresh sheet state on reopen.

`frames/duo-focus.check.mjs` (`--grep duo-focus`) checks actual keyboard navigation in the body overlay, including
native video playback, seek and mute in both directions through Chromium's accessibility tree. Complete forward
and reverse tours must visit the same stops in opposite order, including every native media button/slider, summary,
editing hosts, positive tabindex ordering and multi-control same-origin/opaque iframe content. Separate iframe
boundary tests reset the frame's remembered focus to its first control, then require reverse entry at its last control.
`--set packaged=1` runs the same cases against this checkout's fingerprinted `dist/ui` runtime. The `keyboard-controls.mp4`
fixture copies the synthetic option-b video stream and adds silent AAC audio so the mute control is enabled:

```sh
ffmpeg -i option-b.mp4 -f lavfi -i anullsrc=r=44100:cl=stereo -map 0:v:0 -map 1:a:0 -c:v copy -c:a aac -shortest -movflags +faststart keyboard-controls.mp4
```

`frames/standalone.check.mjs` (`--grep 'standalone status'`) verifies static `black-translucent` metadata,
actual GPU background pixels behind white system glyphs (minimum 7:1), dark-band labels (4.5:1), and safe
spacing in light/dark themes on measured Duo and iPhone 17 Pro poses. It includes first-paint/offline pages,
startup failure, open sheets and the embedded-page opt-out. The development server's `/__fixture/*` routes
serve the real source documents over HTTP; routing HTML through DevTools changes its private-network
security context and prevents its styles/fonts loading. These routes exist only in the fixture server.

Chromium uses the kit's `safe=` override; it cannot verify iOS Home Screen installation geometry. The device
pass must remove the old AAC Home Screen icon and add the new served build again: the status-bar setting can
be captured when the icon is added. Reloading the old icon is insufficient. Confirm the band is physically
beside the clock/signal symbols, and record the viewport/insets rather than assuming the meta was honored.

`frames/duo-reorder.check.mjs` (`--grep duo-reorder`) exercises ordinary timestamp-driven deliverable
sorting with a focused playing video: sibling reorder, up/down/first/last and a matched metadata control.
While a deliverable row owns focus, the list defers its sorted order so the keyed renderer cannot move the
focused subtree; data and text stay live. The latest order applies when focus leaves the deliverable list,
including opening a viewer, and a fresh sheet starts in the latest order. It requires the same keyed
row/player/control, no focused-subtree move, retained playback/focus and working Enter. Removal variants
choose the next row’s same action (previous for the last row), a replaced action restores its row key/control
role, and covered-sheet updates must preserve top-viewer focus, including return after its trigger was removed.

`frames/duo-intent.check.mjs` (`--grep duo-intent`) guards deliberate focus abandonment. Real reading-text
and sheet-background clicks, explicit blur, Tab-away, delayed fetches and abandoned reorders must not re-arm
an action. A focused reorder still recovers. Fixture-only one-shot DOM insertion interception injects consumed
pointer/key/focus events after capture during a real focused-media removal, verifying cancellation even when a child
stops bubbling. These timing injections are isolated to the fixture page; no product test hook is exposed.

`frames/duo-native-poll.check.mjs` (`--grep duo-native-poll`) selects native Mute with trusted Tab, records its
actual AX backend node, and compares it after a metadata control and a timestamp poll. Space must keep
toggling mute while paused. It also observes that no focused row is removed/reinserted, checks live captions,
Open-action deferral over repeated polls, latest order after blur/reopen, native-focused row removal recovery,
and a neighbouring removal plus new row in one poll without moving the surviving native focus scope.

`frames/duo-lists.check.mjs` (`--grep duo-lists`) covers the shared `ui/focus_order.rs` policy across nested
media, display limits, older versions, task rows, stages and task groups; the existing native/reorder suites
cover deliverables. It ports the two-media Sol witness with real HTTP polls, native Mute AX identity and
Space behavior. Leaving inner media for the deliverable's Open action must release only the inner order;
leaving deliverables for Close releases the outer order. Captions and row data update while ordering holds.
Task/stage rows have no intrinsic Tab stops: those two cases explicitly add fixture-only tabindex to test
focus/identity without adding product controls. The task-group case uses its actual Fewer button. All
collection roots bind the id returned by the same `hold_order` helper; keys remain separate from live data.

`frames/duo-portals.check.mjs` (`--grep duo-portals`) covers live folded/unfolded popover placement,
reactive modal policy, delayed inset-only changes, browser Back/Forward and keyboard route departure,
and overlay-local exact-token hover/touch tips. It also exercises pose and route lifetimes for pins,
viewers, Details, activation dialogs and Scoreboard model sheets. Routes keep their page state while
open body portals retire on departure; Forward cannot resurrect a retired opening. The fixture server
serves the same committed computers.json as the product, without a reviewer-only routing wrapper.

`frames/duo-polish.check.mjs` (`--grep 'duo polish'`) covers owner report 2. In the landscape rail (folded and open,
with the strip on either side, light and dark), a one-pixel column inside the rail and the rail's edge line are each
one colour from the top edge to the bottom edge. Under an open sheet's scrim they may shade but must not step.
White status glyphs keep 7:1 on the rail's top. Open upright (band), every text line in the band clears the 55 pt
rounded top corners by 12 pt on every page, including an owner-length Status summary. A one-line summary keeps
the band at 82 pt, and the account menu and toasts start below a taller band.

`frames/duo-portals.check.mjs` also has the max round 6 cases:
- A switch approval made busy with the real `activation-confirmation.mjs` controller, then Back, then the request
  failing: no modal returns over Home. The busy update keeps the same dialog element and focus.
- A nonmodal Provider with page focus (reached by trusted Tab, or focused directly) moves the focus into the sheet
  when a fold makes it modal. Enter can't open Custom range behind it, and unfolding doesn't strand focus.
- An open exact-token explanation follows a live model update.

The fixture server answers `/` with the start page, which the frame warms 4 s after it starts.
