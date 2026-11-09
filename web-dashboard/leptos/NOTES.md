# Leptos trial notes (AI Account Center, for Puppet Master's web GUI)

Lessons from building AAC's first Leptos pages (Leptos 0.9.0-rc1, client-side rendered, served by AAC's TypeScript
server). Each note says what happened, why, and what to do.

## Size and build
- **Download:** the Status page is about 0.8 MB of WebAssembly, 0.18 MB brotli, against 36.7 MB (3.7 MB brotli) for
  the Slint canvas UI. About 90% of a release build is the `name` custom section (function names): strip it after
  wasm-bindgen (`build/strip-names.cjs`). Don't use `strip = true` in Cargo: it also drops `target_features`, and
  wasm-bindgen then can't tell which WebAssembly features are on.
- **Build time:** about 1.5 min for a clean release build on the VM, 25-75 s incremental. CSS changes need no build.
- **Cargo config is inherited:** a crate under `web-dashboard/` picks up the Slint crate's `.cargo/config.toml`
  rustflags (cargo joins arrays from parent folders). Ours turns `+tail-call` back off (WebKit bug 325447 on iOS 27).

## API changes from 0.8
- `set_interval_with_handle` / `set_timeout_with_handle` are gone: `set_interval` and `set_timeout` return the
  handle (`Result<IntervalHandle, _>`).
- **Timers belong to the current reactive owner** and are cancelled when it is cleaned up. A `set_timeout` started in
  an event handler that changes a signal the surrounding `move ||` view reads is cancelled before it fires, because
  that view's branch is rebuilt and its owner cleaned up. Symptom: the timer is scheduled and never runs, with no
  error. Use a plain `window().set_timeout_with_callback_and_timeout_and_arguments_0` with
  `Closure::once_into_js` for fire-and-forget work, or start the timer from an owner that outlives the change.
  `request_animation_frame` behaves the same way.
- `NodeRef` on an element inside a branch that is rebuilt can be empty when a callback reads it later; query the DOM
  (or keep the ref on a stable parent) for that kind of work.

## Patterns that worked
- **Truth on the server, drawing in Leptos:** the server sends a view with every word and number decided; the page
  only lays it out. Rust types with `#[serde(default)]` keep the page tolerant of added fields.
- **Keep typing safe across refreshes:** a 30 s refresh must not rebuild the card being typed in. Memoize the visible
  card without volatile fields (ages) and key lists by id + version. A textarea's `prop:value` may follow the draft
  reactively (so another tab's change shows): typing writes the draft, which sets the same text back, and setting a
  textarea to its own value keeps the cursor and selection (checked in Chrome, with IME, and in iOS 27.3 Safari).
- **Drafts in browser storage** (`localStorage`, wrapped so a refusal only loses persistence) let the owner leave the
  page mid-deck.
- **`default` is not enough for `null`:** serde's `default` fills a missing field, but a `null` sent for a `String` or
  `Vec` fails the whole view. The server sends unknowns as `null`, so text and list fields read through a small
  `nullable` deserializer (null becomes empty). Numbers stay strict: a null count must not turn into a zero.

## iOS WebKit (iPhone 17 Pro and iPhone Duo simulators, iOS 27.3)
Chrome's checks passed while these were wrong on the phone; test WebKit before calling a page done.
- **Variable fonts ignore `font-weight`:** Instrument Sans and Martian Mono are variable (`wght` axis). WebKit drew
  every weight as the default 400; only `font-variation-settings: 'wght' N` picks the weight. `app.css` gives every
  weight both ways, and sets the default-bold elements (`strong`, `b`, headings, `th`) explicitly.
- **Boolean media attributes set after creation don't act:** the page sets attributes on an element it has already
  created, and `muted` only mutes an element created with it. Use `prop:muted=true` as well, or the video starts with
  sound and phones refuse to autoplay it.
- **Fixed asset URLs stay cached:** `/app/app.css` revalidates (`no-cache`), but Safari kept showing the old file. The
  page links it as `app.css?v=<content hash>` (`{{CSS}}` in `index.html`), so a new build is always read.
- **Media fills its frame:** `width/height: 100%` with `object-fit: contain`, not `max-width/max-height: 100%`, so a
  small screenshot is shown as large as the screen allows in every engine.
- **Testing:** text entry into the Slint canvas sign-in can't be driven through the simulator's automation; a test
  server's sign-in can be done with a fetch from a page on that origin. The Simlock-leased Duo can't change its pose
  (`fold` reports no panel table), so the open panel is covered by Chromium viewports only.

## Status and Scoreboard redesign (2026-11-09)
- **Page chrome is standalone only.** The rail, band, phone bars and desktop header draw only without `?embed=1`; in the
  shell's iframe the shell owns the chrome and the content takes the whole box. `ui/device.rs` chooses (`html[data-chrome]`)
  from the window and `env(safe-area-inset-*)`, which CSS can't branch on.
- **A CSS grid column grows to its widest child.** Give the shell's grid `grid-template-columns: minmax(0, 1fr)`: without
  it the desktop header's nowrap width stretched the column, and a 951 px window laid out as a desktop page.
- **Hidden copies break `url(#id)`.** Marks and the logo are drawn more than once with some copies `display: none`; a
  gradient inside a hidden copy paints nothing for the visible ones. Marks are `<img>` data URLs; the logo uses a unique id.
- **iOS Safari inflates text** in wide, short layouts (text autosizing): set `text-size-adjust: 100%` on `html`.
- **Tokens are generated.** `build/theme-tokens.mjs` writes the colour block of `style/app.css` from `theme.slint`;
  `web-dashboard/tests/leptos-style.test.mjs` fails on drift and on pills, chips, dots, side bars or tinted boxes.
- **Dev data:** `dev/serve.mjs` serves `fixtures/status-design-view.json`, with `spend-by-stream.json` and
  `scoreboard-by-model.json` (the golden output of CONTRACT-spend-bymodel.md's reference scripts) until the server sends them.
- **Whole-page images:** `tests/browser/frames/` shoots both pages in the five formats (`--set out=<folder>`).
