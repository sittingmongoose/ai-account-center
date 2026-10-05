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
- **Hidden providers and accounts.** The tray follows the dashboard's "Show in tray" switch, which is independent of
  "Show on dashboard": a provider with `providers[].trayVisible: false` (or listed in `settings.trayHiddenProviders`) is
  left out of the panel, a missing field means visible, and a provider hidden only on the dashboard stays in the tray.
  An account whose own "Show in tray" is off (`accounts[].trayHidden`) is left out too; its "Show on dashboard"
  (`accounts[].hidden`) never hides it here, so the two per-account switches stay independent. Settings › From the
  dashboard lists both.
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
  It holds Appearance (Light / Dark / Auto, stored on this Mac), the connection, the Menu bar group (any provider in
  the data, or Nothing for the icon alone; the Claude account; Used or Remaining of the account's 5-hour window, else
  its weekly one; Codex and Antigravity follow the active account), the open shortcut, Launch at login, read-only
  facts from the dashboard and About. Connection reads "Paired as **Mac tray**, last synced ...", the computer and the
  saved address, and "This connection: <address>, trusted local network" (or "not trusted") from the dashboard's
  public `GET /api/auth/check`, read when Settings opens, so the owner can confirm once that the home VPN counts.
  Re-pair opens the password step with Cancel; Disconnect asks inline first.
- **Sign-in screen.** On first run, after a remote sign-out, during Re-pair and after Disconnect, a sign-in screen
  replaces the list inside the same panel (the header and the footer stay; the body keeps one 736 pt height). It is
  the dashboard's sign-in page at panel size: a form card as content on the glass (only its buttons are glass, with
  one `.glassProminent` primary) and the atlas motif behind it (nine summit contours, the Apex mark, a scale bar).
  Every state of the approved concept is built: first run (address, then password), the one-time setup code, pairing
  in progress, "This address isn't on your local network", "Pairing is turned off for remote computers", wrong
  password with tries left, rate limited with a countdown, unreachable and wrong address, securing this tray (the
  upgrade from a stored password), signed out (revoked, Sign out all devices, not used for 90 days) and success with
  the hand-off into the list. Motion: the entrance stagger, contours growing from the summit, the focus halo, the
  7 pt decaying error shake, collapsible regions, the scale-bar sweep while busy, and a success hand-off in which the
  list loads in with the first-open stagger and every meter sweeps from zero. Reduce Motion shows each state settled.

Controls are limited to opening configured Claude profiles on Mac or Windows, safely activating native Codex and
Antigravity profiles on the shared Ubuntu runtime, and the two automatic-switching policies. Footer controls open the
dashboard, refresh and toggle Settings; the header menu offers Settings, About and Quit.

Claude "Open on Mac" and "Open on Windows" both POST `/api/claude/desktop-profiles/:id/open` with
`Prefer: respond-async`. A 200 is today's finished Open. A 202 turns into a read-poll of
`GET /api/claude/desktop-profiles` once a second (every five seconds after two minutes, giving up after three), and
the account row's secondary line shows the reported `openOperation` in its own style: "Copying history 3 of 18", then
"Opening", then "Opened" ("Opened · copied 3 of 18" when a bounded copy opened Claude before every record was across;
the next Open copies the rest). The profile id comes from the dashboard's own data (`capabilities.claudeProfileId`):
the tray keeps no list of ids, and only checks that one is a plain identifier. `failed` and `blocked_uncertain` show the server's fixed sentence, or "History copy could
not be confirmed. Claude was not opened.", which is also what a 409 `history_unconfirmed` says; three minutes without
a terminal state says "Still working on the dashboard. Check again shortly." Those sentences go to the panel's warning
banner, which is where every other error already shows. Both platform glyphs rest for the whole Open, so a second one
never starts for the same account, and the POST is never repeated: not for a poll, not after an expired session, and
never resumed after a restart. A read that fails while polling never ends the Open; the poll just tries again.
`swift run ccs-bar-check` covers every one of these paths against mock transports, including the poll's exact cadence
and deadline.

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
.build/release/CCSBar --check-meter-geometry Tests/Fixtures/tray-concept-preview.json
.build/release/CCSBar --check-hover-occlusion Tests/Fixtures/hover-occlusion.json
.build/release/CCSBar --check-menu-bar-prefs
.build/release/CCSBar --self-test Tests/Fixtures/tray-concept-preview.json
.build/release/CCSBar --toggle-test Tests/Fixtures/tray-concept-preview.json
.build/release/CCSBar --check-signin                     # every sign-in state read back on screen, then every flow
.build/release/CCSBar --render-preview Tests/Fixtures/tray-concept-preview.json /tmp/signin.png --signin=wrong-password
.build/release/CCSBar --render-preview Tests/Fixtures/tray-concept-preview.json /tmp/panel.png --dark [--settings]
.build/release/CCSBar --render-preview Tests/Fixtures/tray-concept-preview.json /tmp/rt.png --dark --reduce-transparency
python3 Scripts/migration_check.py
./Scripts/package_app.sh
./Scripts/install_user.sh --launch
```

- `--check-native-tooltips` checks every icon control's help tag, the full-row Details targets, that nested controls
  are never the row, and the Selected-row alignment within 0.5 pt at its expected x, with every slot's content fitting
  the slot.
- `--check-meter-geometry` checks every rendered meter against its own track's laid-out width: the fill is the reading
  within 0.5 pt, ticks sit at 25/50/75, and the notch sits at the switch threshold.
- `--check-hover-occlusion` checks which hover tag actually presents under the pointer, with the real panel hosted
  offscreen and a stand-in pointer (the real pointer never moves and no event reaches the system). With Settings open,
  each Settings control (the X, Show, Claude account and Value) presents exactly its own tag with the expected text,
  and no point of a grid over the panel presents a tag from the account list hidden underneath. It also walks the
  pointer across rows and Settings controls (each stop shows its own tag, nothing stale), checks both Settings
  transitions and the panel's close, and checks the rows still tag after Settings closes and after a reopen. A
  height-limited, scrolled panel follows: a footer button over an Activate or Open button (and the gear over Settings'
  Value) presents only the footer's tag with no row highlight, nothing scrolled up under the header presents, a row
  that scrolls away from under a still pointer drops its tag, and a row under a still pointer tags once Settings
  closes or the panel opens. The fixture has three Codex accounts with full emails, the first not active.
- `--check-menu-bar-prefs` checks the menu-bar pickers persist on this Mac, earlier stored values migrate, and fresh
  defaults keep today's behaviour. It uses a throwaway suite and leaves no preferences behind.
- `--self-test` opens the real glass panel from a fixture (no sign-in, no network): status item and reading, the open
  shortcut, the glass panel under the menu bar, gear and Escape for Settings, relaunch reopen, Appearance, a reopen
  during the close fade, full-row Details from the panel and Escape closing it first, the Carbon hot-key event
  toggling the panel, Escape from a focused text field on the connect screen, idle CPU with the panel open and
  closed, and memory over 24 open-close cycles. It shows the panel on screen for about half a minute and leaves no
  preferences behind.
- `--toggle-test` clicks the menu-bar icon with real synthetic mouse events from a fixture (no sign-in, no network):
  open, close, open again, then click-outside closes, Escape closes, and the Carbon hot-key event toggles. It posts
  its clicks through a `--click-at` helper child process and leaves no preferences behind.
- `--check-signin` renders all 15 sign-in states in light and dark at 760 x 736 and reads each back with on-device text
  recognition, then drives every flow (address refusals, pairing off, wrong password, pairing and hand-off, Re-pair and
  Cancel, revoked, expired and Sign out all devices, Pair again, Disconnect, rate limit, setup code, the migration
  from a stored password) through the real view models and `ConnectionSession` against an in-process fake dashboard.
  No network, no real connection file, no screenshot.
- `--e2e <sandbox address> <phase>` runs the same flows over real HTTP against a sandbox dashboard only (a port from
  3901 to 3999, never 3000), with its tray state in `AAC_TRAY_STATE_DIR` (refused under `~/.ccs`) and the sandbox's test
  login in `AAC_E2E_USER`, `AAC_E2E_PASSWORD` and `AAC_E2E_NEW_PASSWORD`. Phases: `trust-off`, `not-trusted`, `main`
  (first pairing, bearer reads, a password change on the dashboard, revoke, Pair again, Re-pair, Re-pair cancelled
  during the pair call, This connection, Disconnect), `migrate` (a fake version 1 password file) and `names` (run from
  the packaged app's own binary against a dotted host name for the sandbox, such as `<lan address>.nip.io`: the
  numeric-address message, before anything is sent and for a saved password connection).
- `--render-preview` draws the panel content offscreen (`--light`, `--dark`, `--settings`, `--details=<account id or
  provider>`, `--signin=<state>`, and `--reduce-transparency` or `--increase-contrast`, which simulate those display settings in
  the render only). System glass is composited by the window server and never reaches an offscreen render, so previews
  bake a glass stand-in behind the real content and pin "now" to the fixture's capture time. Offscreen switches draw
  in their inactive grey; the knob's side shows the state.

### Rename compatibility

`~/Applications/CCS Bar.app` becomes a relative symlink to `AI Account Center.app`, so existing launch paths continue to resolve. The app retains its executable name `CCSBar`, bundle identifier and launch-agent label `party.sittingmongoose.ccs.accounts-bar`, and private configuration directory `~/.ccs/bar`. The launch-agent program path moves to the new canonical app path. No provider identity, authenticated dashboard setting, API route or native-host identifier is renamed. Existing connection data, account selection, threshold and refresh cadence stay unchanged. The original MIT license is bundled in the app and linked to upstream attribution in About.

`python3 Scripts/migration_check.py` exercises migration, repeat install, alias safety and rollback using temporary fixture apps; it makes no live app or account changes.

## Private connection

The connection lives in `~/.ccs/bar/accounts-connection.json`, in a 0700 directory with file mode 0600, outside the
source and app bundle; the tray refuses the file if any group or other permission bit is set. Every write creates its
temporary file at 0600 in the same directory before anything is written to it, then renames it over the old one and
reads it back. `AAC_TRAY_STATE_DIR` moves the whole state folder for checks and isolated end-to-end runs; the
installed tray never sets it.

**Version 2: a device key, never a password** (CONTRACT-auth-devices sections 5 to 9). The tray pairs once with
`POST /api/auth/devices/pair` (username, password, the Mac's name, `platform: "mac"`, an install id it keeps, the app
version), proves the new key with one `GET /api/auth/devices/me`, and only then saves
`{ version: 2, baseURL, username, deviceId, deviceToken, installId, pairedAt }`; the password is dropped and never
written. From then on every request carries `Authorization: Bearer <key>` and the tray never logs in; it calls only the
dashboard's tray routes. A password change on the dashboard does nothing to the key, so the tray stays signed in.

- **Plain HTTP addresses.** The app's App Transport Security allows local networking only (`NSAllowsLocalNetworking`,
  no per-address exception), which on macOS 27.2 lets plain HTTP reach a numeric address (IPv4 or IPv6), a `.local`
  name or a one-word name, and refuses every other host name before a request leaves the Mac. The address step
  therefore asks for the dashboard's numeric address (such as `http://192.168.1.20:3000`) or its `.local` name when
  given another name such as `dash.home.arpa`, and the Mac's own refusal is shown with that message, never as "Can't
  reach that address".
- **Trusted local network.** The dashboard stays on plain HTTP at its LAN address. Pairing works when its owner has
  turned on "Trust this local network" and this Mac's address is in its trusted networks (the home network or the home
  VPN). Before any password is sent, the tray resolves the address itself and refuses a public, CGNAT, link-local or
  unknown one ("This address isn't on your local network"); then `GET /api/auth/check` decides between the password
  step, "Pairing is turned off for remote computers" (`trustedLocalNetwork: false`) and the dashboard's own refusal
  ("The dashboard sees this Mac at <address>").
- **Verify before saving.** Address checks save nothing. Re-pair pairs under a new install id, because the dashboard
  revokes an install id's old key the moment it pairs that id again. The working key and file therefore stay valid
  until the new key has answered `devices/me` with 200 and is saved; only then is the old key revoked with itself
  (`DELETE /api/auth/devices/me`, tried again by maintenance if the dashboard does not answer). A key the dashboard
  issued that is never saved (no confirmation, a failed save, or Cancel or Escape while the pair call runs) is revoked
  at once instead of being kept, and a failed save restores the file exactly. Attempts in one sign-in session share
  one install id, so a retry replaces any key that could not be revoked; after a Cancel the next attempt takes a new
  one.
- **Migration from version 1.** A stored password is traded for a key by itself on launch ("Securing this tray"): the
  version 1 file is copied to `accounts-connection.v1-rollback.json` (0600), version 2 replaces it, and the first 200
  from `devices/me` deletes the copy. A 401 device code puts version 1 back; no answer keeps both and checks again on the
  next poll, for at most 24 hours. A dashboard without pairing (404/405), one that refuses it (403, trust off) or no
  answer keeps today's verified password login; the refresh path asks again after an hour, so turning on "Trust this
  local network" later needs no restart. A key the dashboard refuses right after a migration puts version 1 back and
  waits a day. A stored password the dashboard rejects opens the pairing screen with the username filled in.
- **Rotation.** `devices/me` is read every six hours; once its `rotateAfter` has passed, the tray calls
  `POST /api/auth/devices/me/rotate`, saves the new key before its first use and switches to it. A refusal keeps the
  current key and waits an hour.
- **Signed out.** A 401 `device_revoked`, `device_expired` or `invalid_token` deletes the key (the address, username and
  install id stay), stops polling and shows "This tray was signed out" with the reason, and who and when if the
  dashboard sends `revokedBy` and `revokedAt`. The menu bar shows the logo with no percentage and the help tag "Signed
  out". Nothing retries on its own; a network error is never a sign-out. A 503 `auth_store_unavailable` keeps the key.
- **Disconnect** calls `DELETE /api/auth/devices/me`, forgets the key and shows the first-run screen with
  "Disconnected at ..." and the last address filled in.
- **Older dashboards.** Without pairing, Connect falls back to today's version 1 login, verified before it is saved
  (`POST /api/auth/login`, then `GET /api/accounts/settings` with that session; cookies only in an ephemeral session;
  failed logins back off for 15 minutes).

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
