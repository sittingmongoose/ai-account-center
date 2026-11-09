# AAC dashboard browser checks

These are browser checks for the web dashboard (Slint 1.18 compiled to WebAssembly, drawn into one `<canvas id="canvas">`), run with browser-check (`browser-check guide`, or `Agent-Guides/browser-check.md` on the Cursor share).

```sh
browser-check run web-dashboard/tests/browser/checks                  # signed out, desktop, widths, phone (~90 s)
browser-check run web-dashboard/tests/browser/checks --grep widths
browser-check run web-dashboard/tests/browser/checks --set home=/tmp/fixture-home   # a prepared CCS_HOME (copied)
```

## What a run does

- **Build needed.** It serves this checkout's `dist/ccs.js` and `dist/ui`, so run `bun install` and `bun run build:server` first. Build `dist/ui` with `bun run ui:build`, or for a first look copy the installed package's `dist/ui`. A wasm build can exhaust a small machine's memory, so use your machine's build lane if it has one.
- **Isolated home.** The server starts with `CCS_HOME` and every home-like variable pointing at a temporary folder: `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, the XDG folders, `CODEX_HOME` and `CLAUDE_CONFIG_DIR`. Some collectors read the real home, so with only `CCS_HOME` set, real account emails and usage appear. Never point `home=` at a real home.
- **Real sign-in.** `t.app.signIn()` goes through the real loopback setup (`POST /api/auth/setup`), or logs in once setup has happened. It runs as a `fetch` inside the page, so the session cookie lands in that page's jar.
- **Probes.** Pages open with `?e2e`, so each `ProbeMark` reports its rectangle (`__aacProbeNow()` and `__aacProbe`). Pages are reached by URL (`?view=analytics`, `?view=accounts`): desktop has no tab probes, which exist only in the narrow tab bar.
- **Canvas controls (browser-check 2.0).** The config's `controls(t)` hands browser-check the probes that are controls (the `CONTROL` list: buttons, fields, toggles, rows, tabs), not headings, hints, bars or panels; signed in, the hidden sign-in panel's probes are left out. So `t.audit` checks each control on screen: inside the left and right edges, not under a DOM element, at least 24 px or spaced as WCAG 2.5.8 allows, and drawn. Controls scrolled below the window or parked past an edge (the closed Details panel) are left out and named in a NOTE. A probe added to the UI that is a control belongs in `CONTROL`.

## Checks (`checks/smoke.check.mjs`)

| Test | What it asserts |
|---|---|
| `signed out` | Sign-in fields reported inside the viewport and usable (`t.audit`); no probed control past the edge. |
| `desktop` | Signed in at 1920x1080. Home, Analytics and Accounts: `t.audit` (canvas painted, each page's controls reported and usable) and no probe, headings included, past the edge. Lists one full frame (Home). |
| `widths` | The same at 1360, 1430, 1600 and 2200 px, the widths where header and quota-table overflow bugs showed up. |
| `phone` | 390x844, touch, DPR 1. The same, with the tab bar required. |

`checks/accounts-clicks.check.mjs` clicks every account row and header toggle at ten widths and on a phone, and asserts nothing is pushed off the page.

`checks/status-page.check.mjs` and `checks/scoreboard-page.check.mjs` cover the Leptos pages: deck flows, the layout in each format (with `?safe=t,r,b,l` giving the measured Duo insets), the iframe drawing no chrome, and honest words when `spendByStream` or `byModel` are missing. `frames/` (its own config) shoots whole-page images for review: `browser-check run web-dashboard/tests/browser/frames --set out=<folder>`, needing only `web-dashboard/leptos/pkg` built.

Frames that aren't listed are saved as snaps for `browser-check diff <base run> <candidate run>`.

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
	const r = await t.app.rect('quota-head:title');             // a control's rectangle from its ProbeMark
	await t.crop('quota-head', r, 'quota header title at 1360 px: not cut off');
	await t.audit('analytics 1360', { controls: ['analytics-provider'] });   // controls: reported, on screen, usable, drawn
	await t.app.noProbeOverflow();
}
```

- **Helpers:**
  - `t.app.open({ page, signIn, query })`
  - `t.app.signIn()`
  - `t.app.probes({ require })` returns every probed rectangle, once the set has held still for two reads and has every id in `require` (up to 4 s).
  - `t.app.rect(id)`
  - `t.app.click(id)` clicks a control through `t.clickControl`: it waits for a late probe, wheels a control above or below the window into view, and refuses one a DOM element covers.
  - `t.scrollToControl(id)` (browser-check) brings a control into view with real wheel turns over the page, not the header, and returns its rectangle.
  - `t.app.probeVisible(id)`
  - `t.app.noProbeOverflow()`: no probe reaches past the left or right edge; a probe parked wholly past it (the closed Details panel) is named in the note, not a failure.
  - `t.app.stub(path, json)`
- **A probe reports even while its control is hidden.** A reported rectangle proves layout, not visibility. Judge visibility from pixels (`t.crop` at the rectangle, `t.painted`) or a frame.
- **Synthetic data only:** made-up names and emails, never real account data.
- **DPR:** Slint under Playwright's DPR emulation has rendered at double scale. Judge HiDPI on a real device, or treat a DPR above 1 as unverified.
