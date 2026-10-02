# AI Account Center for macOS

The macOS menu bar app in [AI Account Center](https://github.com/sittingmongoose/ai-account-center), renamed from the CCS Bar accounts edition. Its packaging originated in
[CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar); upstream credit, license and compatibility identifiers are preserved. The panel is 760 points wide
and follows the AI Account Center tray design shared with the Windows tray: the same order, data rules and active-account indicator.

The menu-bar panel is one macOS 27 Liquid Glass surface: a regular `NSGlassEffectView` (20 pt corners) under
the status item, with content on translucent platters and glass controls (SwiftUI `glassEffect`,
`GlassEffectContainer`, `glassEffectUnion`, a materializing Settings close button). Every provider has its own
platter (Claude, Codex and Antigravity hold their header and rows; each other provider holds its row), with a
hairline edge and 12 pt of panel glass between platters, so the providers read as separate blocks over any
wallpaper. Under Reduce Transparency the system glass turns opaque and the panel's own fills switch to opaque system
colours; Increase Contrast adds borders.
Text is the system font (SF Pro) with tabular digits, and every percent sign is part of its number's run.

The layout and order are today's: header (Apex Soft mark, name, "9 of 9 reporting · cached · updated 3:15 PM", menu);
Claude; Codex; Antigravity; the other providers (Cursor, Muse Code, Kimi Code, Qwen, Z.ai, OpenCode Go) in that order;
and a footer that floats over the list (Codex auto-switch and threshold, Dashboard, Refresh, Settings).

- **Usage is never invented.** A missing reading reads "Unavailable" (or "Not reported yet"), never 0. Values keep at
  most two decimals. Codex Chat pass windows, the Qwen subscription row and empty Z.ai reset-pack summaries stay
  hidden; a Codex account gets a 5-hour cell only when its exact `five_hour` window is reported.
- **A reading from before its reset is not shown.** Once a window's `resetAt` has passed and its reading was sampled
  before it (the window's own `sampledAt`, else the account's, or no sample time at all), the meter is drawn
  unavailable with no number and reads "Reset at 10:15 AM · new reading pending" (shorter forms in a narrow cell,
  "Pending" beside a label), never 0%. The menu bar skips such a reading. The refresh timer re-checks this on every
  tick, so a window flips when its reset passes while the panel is open.
- **Claude Fable** has its own column for Max plans only, read from the provider's `seven_day_fable` window; a Max
  account without one shows "Not reported yet", and Pro accounts have no Fable cell.
- **Active account (Selected row).** In Codex and Antigravity the active row sits on one accent-tinted platter that
  glides to the new row after a switch (ease-out, no overshoot). Its action slot shows a filled check, "Active" and
  "on Ubuntu" with no button chrome, lined up exactly with the Activate capsules above and below (the capsule's label
  inset is the check plus its gap). `--check-native-tooltips` measures this alignment.
- **Antigravity switching** uses the dashboard routes `POST /api/antigravity/profiles/:id/activate`,
  `POST /api/antigravity/profiles/:id/confirm` and `PUT /api/antigravity/auto-switch`. Activate is offered only when the
  dashboard reports `antigravityCanActivate` for the Ubuntu host. Running Antigravity programs produce an inline
  confirmation listing them; only "Stop, switch, restart" sends the one-use token, once. With two accounts the section
  header carries Antigravity's own auto-switch and threshold (percent used); it can be turned on once the shared quota
  pool has been chosen in the dashboard.
- **Hidden providers.** Providers hidden in the dashboard's Accounts & Settings are left out of the panel when the
  dashboard DTO carries them (`hiddenProviders`, top level or inside `settings`); otherwise every provider shows.
- **Meters** are 6 pt tracks with quarter ticks, a severity fill (calm, warning from 80 %, critical from 95 %, and an
  overage cap above 100 %) and the auto-switch notch. Widths and numbers ease out and never pass the reading.
  Reported usage above 100 % stays in the text; only the bar is capped.
- **Motion.** The first open of a session staggers the blocks in and sweeps every meter from zero while the numbers
  count up; later opens animate only readings that changed since the panel last closed. The menu-bar percentage rolls
  to its new value, Refresh spins while it works, the gear turns as Settings slides in, and the active check draws in
  after a switch. Reduce Motion shows the settled state.
- **Details.** Clicking anywhere on an account or provider row opens its details popover (every window with its exact
  reset, balances and expiries, and Activate or Active). Nested buttons act on their own and never open details.
- **Settings** opens inside the panel, sliding over the list: the gear toggles it, the glass X and Escape close it.
  It holds Appearance (Light / Dark / Auto, stored on this Mac), the connection, what the menu bar shows (the active
  Codex or Antigravity account, or the logo only; % left or % used), the open shortcut, Launch at login, read-only
  facts from the dashboard and About.

Controls are limited to opening configured Claude profiles on Mac or Windows, safely activating native Codex and
Antigravity profiles on the shared Ubuntu runtime, and the two automatic-switching policies. Footer controls open the
dashboard, refresh and toggle Settings; the header menu offers Settings, About and Quit.

## Opening the panel

- Click the Apex glyph in the menu bar.
- **Launch the app again** from Spotlight, Launchpad (Apps), Finder or the Dock, or with
  `open -a "AI Account Center"`: while it runs, macOS sends the running copy a reopen event
  (`applicationShouldHandleReopen`) and the panel opens. If it was quit, the same launch starts it and opens the
  panel. The login item starts it quietly in the menu bar.
- **Option-Command-A** opens or closes the panel from any app. The shortcut uses Carbon `RegisterEventHotKey`, which
  needs no Accessibility or Input Monitoring permission. It is on by default and can be turned off in Settings (Open
  shortcut). Finder also uses Option-Command-A for Deselect All; while the shortcut is on, the panel wins. If another
  app has already claimed the combination, Settings says so and the other ways still work.
- The installer puts the app in `~/Applications`, which Spotlight and Launchpad index. New installer backups are kept
  in `.noindex` folders so search never offers an old copy.
- The panel closes when you click outside it, switch apps or Spaces, press Escape, or click the menu-bar glyph
  again. Escape works from any field in the panel and closes the innermost thing first: an open Details, packs or info
  popover, then Settings, then an unanswered switch confirmation, then the panel.
- A shortcut needs the app running. If you quit it, open it again from Spotlight or Launchpad: that starts it and opens
  the panel.

## Build, verify, package, install

macOS 26 or later and Xcode 26 or later (the build Mac uses Xcode 27, Swift 6.4); the package uses
`swift-tools-version:6.2` with the Swift 5 language mode. No npm/CCS CLI is needed on the Mac because this edition
connects to the existing dashboard.

```sh
swift build -c release
swift run ccs-bar-check                                  # offline core and contract checks
export AAC_ASSETS_DIR="$PWD/Resources/Assets"            # unbundled runs read artwork from the source tree
.build/release/CCSBar --check-native-tooltips Tests/Fixtures/tray-concept-preview.json
.build/release/CCSBar --check-native-packs Tests/Fixtures/tray-concept-preview.json /tmp/packs.png
.build/release/CCSBar --self-test Tests/Fixtures/tray-concept-preview.json
.build/release/CCSBar --render-preview Tests/Fixtures/tray-concept-preview.json /tmp/panel.png --dark [--settings]
.build/release/CCSBar --render-preview Tests/Fixtures/tray-concept-preview.json /tmp/rt.png --dark --reduce-transparency
python3 Scripts/migration_check.py
./Scripts/package_app.sh
./Scripts/install_user.sh --launch
```

- `--check-native-tooltips` checks every icon control's help tag, the full-row Details targets, that nested controls
  are never the row, and the Selected-row alignment within 0.5 pt.
- `--self-test` opens the real glass panel from a fixture (no sign-in, no network): status item and reading, the open
  shortcut, the glass panel under the menu bar, gear and Escape for Settings, relaunch reopen, Appearance, a reopen
  during the close fade, full-row Details from the panel and Escape closing it first, the Carbon hot-key event
  toggling the panel, Escape from a focused text field on the connect screen, idle CPU with the panel open and
  closed, and memory over 24 open-close cycles. It shows the panel on screen for about half a minute and leaves no
  preferences behind.
- `--render-preview` draws the panel content offscreen (`--light`, `--dark`, `--settings`, `--details=<account id or
  provider>`, `--connect`, and `--reduce-transparency` or `--increase-contrast`, which simulate those display settings in
  the render only). System glass is composited by the window server and never reaches an offscreen render, so previews
  bake a glass stand-in behind the real content and pin "now" to the fixture's capture time. Offscreen switches draw
  in their inactive grey; the knob's side shows the state.

### Rename compatibility

`~/Applications/CCS Bar.app` becomes a relative symlink to `AI Account Center.app`, so existing launch paths continue to resolve. The app retains its executable name `CCSBar`, bundle identifier and launch-agent label `party.sittingmongoose.ccs.accounts-bar`, and private configuration directory `~/.ccs/bar`. The launch-agent program path moves to the new canonical app path. No provider identity, authenticated dashboard setting, API route or native-host identifier is renamed. Existing connection data, account selection, threshold and refresh cadence stay unchanged. The original MIT license is bundled in the app and linked to upstream attribution in About.

`python3 Scripts/migration_check.py` exercises migration, repeat install, alias safety and rollback using temporary fixture apps; it makes no live app or account changes.

## Private connection

Connection settings save `~/.ccs/bar/accounts-connection.json` in a private
directory (0700) with file mode 0600. The file is not part of the source or app
bundle. Its keys are `baseURL`, `username`, and `password`; configure them in
Settings › Connection (or the first-run connect screen in the panel) or deploy the
file privately. Connect and Change verify before they save: a temporary client signs in with the entered details
and reads `GET /api/accounts/settings` with that session, and only then is the file written (same path, permissions
and JSON, any other members kept) and the live client replaced. A wrong address, a rejected login, a timeout (15 s)
or Cancel (or Escape) while it checks keeps the saved file and the live client, and the reason shows under the form.
Device pairing waits for the dashboard's device-token routes. Existing saved connection files are preserved during app upgrades. The client signs in to
`POST /api/auth/login`, retains cookies only in an ephemeral session, and sends
the dashboard's exact Origin on mutations. It does not disable dashboard
authentication. Failed login polling backs off for 15 minutes.

The accounts contract is `GET /api/accounts/dashboard?platform=mac&refresh=true`
and schema version 1. Menu opening forces a server-backed refresh; background
reads use the dashboard’s saved usage refresh interval (60 seconds by default).
Adjust it in dashboard Settings; manual Refresh bypasses the normal sample cache. The dashboard service owns Codex automatic switching,
so closing this app does not disable it.

When manual activation finds running Codex programs, an inline confirmation under the row lists their names, PIDs and roles, and warns that stopping them interrupts active work. Only "Stop, switch, restart" sends the server's one-use token. Cancel makes no further request. Expired or stale offers require a fresh Activate to review the current programs; confirmed requests are never replayed automatically after an authentication failure. Automatic switching still waits for idle.

The Used-threshold menu shows the confirmed stored setting: 85/90/95/98 percent
used map to 15/10/5/2 percent remaining. Selecting a threshold preserves the
current enabled state. The private `~/.ccs/bar/native-status.json` contains only
connection status, timestamp, provider/account counts and automatic-switch
settings, allowing read-only verification of the installed GUI process itself.

After connection deployment, `.build/release/ccs-bar-check --check-live` performs
one read-only native client check and prints schema/provider/status counts and
the Codex automatic-switch setting. It does not activate accounts or print
credentials. `CCSBar --render-preview <fixture.json> <preview.png>` exports an
isolated SwiftUI layout image from a sanitized dashboard fixture without
capturing the desktop or making network requests.

The launch-agent preference affects subsequent graphical sign-ins; quitting the
current app remains available at any time. The app's bundle identifier is
`party.sittingmongoose.ccs.accounts-bar`, separating its preferences from the
upstream edition.

## Source provenance

The original package layout and ad-hoc packaging script came from
`kaitranntt/ccs`'s `macos-bar` at fork commit
`1a4a68dee71063ecb8b2138d7c55ceed552150d3`, under the included MIT license.
Provider marks are the official artwork listed with sources and SHA-256 in `Resources/Assets/PROVIDER-SOURCES.md`
(light-surface variants where the provider ships one; Kimi Code uses its official app icon). The Apex Soft menu-bar
template, header mark and app icon come from the project's own logo export.
All accounts models, authenticated client, grouped views, and user installer are
the accounts-edition implementation.
