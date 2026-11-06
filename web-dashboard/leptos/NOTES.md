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
  card without volatile fields (ages), key lists by id + version, and set a textarea's value once (`prop:value` with a
  plain string), never reactively while the owner types.
- **Drafts in browser storage** (`localStorage`, wrapped so a refusal only loses persistence) let the owner leave the
  page mid-deck.
