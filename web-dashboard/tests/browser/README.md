# AAC dashboard browser checks

These are browser checks for the web dashboard (Slint 1.18 compiled to WebAssembly, drawn into one `<canvas id="canvas">`), run with browser-check (`browser-check guide`, or `Agent-Guides/browser-check.md` on the Cursor share).

```sh
browser-check run web-dashboard/tests/browser/checks                  # signed out, desktop, widths, phone (~90 s)
browser-check run web-dashboard/tests/browser/checks --grep widths
browser-check run web-dashboard/tests/browser/checks --set home=/tmp/fixture-home   # a prepared CCS_HOME (copied)
```

If `browser-check` is not on PATH, run `node <share>/Tools/browser-check/bin/browser-check.mjs`.

## What a run does

- **Build needed.** It serves this checkout's `dist/ccs.js` and `dist/ui`, so run `bun run build:server` first. Build `dist/ui` with the project recipe (`aac-winbuild`), or for a first look copy the installed package's `dist/ui`.
- **Isolated home.** The server starts with `CCS_HOME` and every home-like variable pointing at a temporary folder: `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, the XDG folders, `CODEX_HOME` and `CLAUDE_CONFIG_DIR`. Some collectors read the real home, so with only `CCS_HOME` set, real account emails and usage appear. Never point `home=` at a real home.
- **Real sign-in.** `t.app.signIn()` goes through the real loopback setup (`POST /api/auth/setup`), or logs in once setup has happened. It runs as a `fetch` inside the page, so the session cookie lands in that page's jar.
- **Probes.** Pages open with `?e2e`, so each `ProbeMark` reports its rectangle (`__aacProbeNow()` and `__aacProbe`). Pages are reached by URL (`?view=analytics`, `?view=accounts`): desktop has no tab probes, which exist only in the narrow tab bar.

## Checks (`checks/smoke.check.mjs`)

| Test | What it asserts |
|---|---|
| `signed out` | Sign-in fields reported inside the viewport; no probed control past the edge. |
| `desktop` | Signed in at 1920x1080. Home, Analytics and Accounts paint, with no probed control past the edge. Lists one full frame (Home). |
| `widths` | The same at 1360, 1430, 1600 and 2200 px, the widths where header and quota-table overflow bugs showed up. |
| `phone` | 390x844, touch, DPR 1. The tab bar probes are inside the viewport, every page paints, no overflow. |

Frames that aren't listed are saved as snaps for `browser-check diff <base run> <candidate run>`.

## Writing checks for a change

```js
export default async function (t) {
	await t.app.stub('/api/accounts/dashboard', fixture);   // synthetic data, shaped like src/web-server/services/*-types.ts
	await t.app.open({ page: 'analytics' });
	const r = await t.app.rect('quota-head');               // a control's rectangle from its ProbeMark
	await t.crop('quota-head', r, 'quota header at 1360 px: refresh button not cut off');
	await t.app.noProbeOverflow();
}
```

- **Helpers:**
  - `t.app.open({ page, signIn, query })`
  - `t.app.signIn()`
  - `t.app.probes()` returns every probed rectangle.
  - `t.app.rect(id)`
  - `t.app.click(id)` clicks the center of a probed control.
  - `t.app.probeVisible(id)`
  - `t.app.noProbeOverflow()`
  - `t.app.stub(path, json)`
- **A probe reports even while its control is hidden.** A reported rectangle proves layout, not visibility. Judge visibility from pixels (`t.crop` at the rectangle, `t.painted`) or a frame.
- **Synthetic data only:** made-up names and emails, never real account data.
- **DPR:** Slint under Playwright's DPR emulation has rendered at double scale. Judge HiDPI on a real device, or treat a DPR above 1 as unverified.
