# AI Account Center for Windows

Native C#/.NET 10 WPF tray companion for the [AI Account Center](https://github.com/sittingmongoose/ai-account-center)
dashboard, with a WinForms `NotifyIcon` in the notification area. It follows the official
[macOS CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar) and the approved Daylight Atlas tray design
(the Windows view of the tray polish concept): today's layout and order, in the dashboard's palette, meters and type.

The panel is 760 logical pixels wide (850 tall), clamped to the monitor's work area, and opens above the taskbar:

- **Header:** the Apex Soft mark, "AI Account Center" and one status line ("9 of 9 reporting · cached · updated
  3:15 PM"; hover for the relative time and any providers hidden in the trays). Before pairing it says "Not paired",
  then "Pairing", "Securing this tray" or "Signed out".
- **Claude, Codex, Antigravity:** one card each with column captions over the meters. Claude shows 5-hour, Weekly and,
  for Max plans only, Fable (`seven_day_fable`; "Not reported yet" when a Max account has no Fable window; Pro accounts
  get no Fable cell). Each Claude row ends with the Open on Mac / Open on Windows pair.
- **Selected row:** the active Codex or Antigravity account's row is a softly accent-tinted surface with a hairline
  accent outline, and its address turns semibold. Its action slot shows a filled check and "Active" with "on Ubuntu"
  under it, as plain text with no box. The check sits in the Activate button's 24 px leading space and "Active" starts
  on the "Activate" label's x, so both line up with the buttons above and below. On a switch the highlight glides to
  the new row and the check draws in. With one Antigravity account and none reported active, the slot says
  "Not reported / as active".
- **Other providers** (Cursor, Muse Code, Kimi Code, Qwen, Z.ai, OpenCode Go, and any provider the server adds later,
  with a neutral mark): one card per provider with up to three labelled meters; Qwen's packs button lists every pack.
- **Footer:** Dashboard; Refresh; Settings. The Codex auto-switch toggle, its `at 95%` threshold menu and an
  info popover sit in the Codex section header, above its accounts, styled like Antigravity's.

Click any row to expand its details (every window, balance, expiry and the account's actions); nested buttons keep
their own action. Hover tints a row, brightens its tracks and shows a chevron.

## Data rules

The tray is a thin client of `GET /api/accounts/dashboard?platform=windows`. Unknown percentages and reset times stay
unknown ("Unavailable", never 0), nothing is summed or averaged across accounts, and displayed fractions use at most two
decimals (the raw values keep their precision). Codex Chat pass windows, Qwen subscription metadata, empty Z.ai pack
summaries and an unreported Codex 5-hour window are not shown. Retained (cached) readings keep their original sample
time in the tooltip and details. A compact meter's reset takes the longest form that fits beside its value ("5:15 AM",
then "5h 15m", then "5h"); the exact time is always in its tooltip. Once a window's `resetAt` has passed, a reading
sampled before it (the window's own `sampledAt`, else the account's; or no sample time at all) is not shown: the meter
is drawn as unavailable with no number and reads "Reset at 5:15 AM · new reading pending" (shorter forms in a narrow
cell, "Pending" beside a label), never 0%. A window the dashboard marks `resetPassed: true` (F6) is shown the same way
even when this computer's clock is behind. The refresh timer re-checks this on every tick, so a window flips when its
reset passes while the panel is open. The trays follow only the dashboard's per-provider "Show in tray" switch
(`providers[].trayVisible`, or `settings.trayHiddenProviders`); "Show on dashboard" (`providers[].visible`) never hides
anything here. An account is left out when its own "Show in tray" is off (`accounts[].trayHidden`); the dashboard's
per-account "Show on dashboard" (`accounts[].hidden`) never hides it here, so all four combinations work. Provider ids, Codex profiles,
Claude profile ids and Antigravity profile ids come from the data and are accepted only when they are URI and path
safe (`[A-Za-z0-9][A-Za-z0-9_-]{0,63}`); there is no allowlist of account names.

Claude "Open on Mac" and "Open on Windows" both POST `/api/claude/desktop-profiles/:id/open` with
`Prefer: respond-async`. The tray never starts a `ccs-claude://` URI itself, so a guarded history copy cannot be
bypassed; when the dashboard cannot be reached the Open says "Can't reach the dashboard. Try again." and launches
nothing. A 200 is today's finished Open. A 202 turns into a read-poll of `GET /api/claude/desktop-profiles` once a
second (every five seconds after two minutes, giving up after three), and the account row's secondary line shows the
reported `openOperation` in its own style: "Copying history 3 of 18", then "Opening", then "Opened". `failed` and
`blocked_uncertain` show the server's fixed sentence, or "History copy could not be confirmed. Claude was not
opened.", which is also what a 409 `history_unconfirmed` says; three minutes without a terminal state says "Still
working on the dashboard. Check again shortly." Those sentences go to the footer status, which is where every other
error already shows. Both platform buttons rest for the whole Open, so a second one never starts for the same account,
and the POST is never repeated: not for a poll, not after an expired session, and never resumed after a restart. A read
that fails while polling never ends the Open; the poll just tries again.

## Look and motion

- **Type:** Instrument Sans, embedded as static WPF font resources (Regular, Medium, SemiBold, Bold and SemiCondensed
  SemiBold for meter values) with tabular figures frozen into every face; "%" is part of the same text run as its
  number. See `CCSBar/Resources/Fonts/README.md`.
- **Light / Dark / Auto:** Settings, live. Auto follows the Windows app mode (`AppsUseLightTheme`) and re-applies on
  `SystemEvents.UserPreferenceChanged`; colours cross-fade over 300 ms.
- **Icons:** the Apex Soft notification-area icon, light or dark taskbar art picked from `SystemUsesLightTheme` and
  swapped when the taskbar theme changes; the plated Apex Soft app icon (the Mac's art, built by
  `scripts/build-app-icon.py`) for the executable, window and shortcuts, legible on any wallpaper or theme; official
  provider marks (`marks-v2`, light-surface variants in Light, never recoloured);
  filled Apple and Windows glyphs; Lucide UI icons drawn as vectors. No glyph-font or emoji icons.
- **Motion:** the panel fades and rises on open with a short stagger; bars sweep from 0 on the first open of a session
  and later only changed readings move. Meter widths and numbers ease out (cubic) and never pass the reading; the
  severity gradient cross-fades. Springs are used only on controls: the toggle knob, the segmented thumb and the gear's
  60 degree turn. Rows tint on hover (140 ms), details expand with an animated height, Settings slides over the list,
  Refresh spins and eases to rest. Windows' "show animations" setting off turns motion off.
- **Tray menu:** right-click shows a restyled menu (Open accounts, Open dashboard, Refresh now, Settings, Quit) in the
  panel's palette and font, with Windows 11 rounded corners. The tooltip carries usage: "AI Account Center · Codex:
  codex-2, 91% weekly left".

## Sign-in and pairing

The tray signs in once and then keeps its own device key (CONTRACT-auth-devices sections 5 to 10). The sign-in screen
replaces the list inside the same panel, as the trays concept draws it: a 372 px Atlas paper card on a graticule, the
atlas motif behind it (nine summit contours, the Apex mark, 25/50/75 figures and a survey scale bar), the dashboard's
type, focus halo, three-layer primary button, error shake and success hand-off. It covers every state of the brief:

1. **First run:** the dashboard address (empty, placeholder `http://`; a missing scheme gets `http://`), then the
   username and password, "Pairs as Windows tray on <this PC>".
2. **Setup code:** a dashboard with no sign-in yet: username, password with a strength meter, confirm, and the one-time
   `XXXX-XXXX` code from the server's terminal; "Create sign-in and pair".
3. **Pairing:** four steps tick through (password checked, device key issued, saving the key, forgetting the password).
4. **Not on your local network:** plain HTTP carries the password only to a local address, so a public, CGNAT,
   link-local or unknown address (the tray resolves the name and checks every address) is refused before anything is
   sent, with "Use the dashboard's local address" and "Or connect through your home VPN first". The same screen shows
   when the dashboard itself does not trust this computer ("The dashboard sees this PC at ...").
5. **Pairing turned off:** the dashboard's "Trust this local network" is off; Try again asks once more. A tray without
   a device key (new, signed out, or still on its version 1 password) is also offered "Use password": the password
   sign-in it always had, verified before it is saved, so a changed dashboard password can be entered while pairing
   waits. A paired tray is never offered it.
6. **Wrong password** with the tries left, and 7. **rate limited** with a countdown and the button rested.
8. **Unreachable** or **wrong address** (answered, but not as AI Account Center); the saved connection is untouched.
9. **Securing this tray:** a stored version 1 password is traded for a key by itself (below).
10. **Signed out:** revoked from the dashboard, Sign out all devices (with who, when the dashboard says so), or not
    used for 90 days; the username is filled in and Pair again pairs anew. The tray never retries on its own.
11. **Success:** the button turns calm with its check, the bar fills, and the list loads in underneath.

Pairing is `POST /api/auth/devices/pair` with the password, this PC's name, `platform: windows` and an install id. The
new key must work once (`GET /api/auth/devices/me` with `Authorization: Bearer`) before anything is saved, and a
working connection is never replaced by one that has not. The new key's first check is asked again after a network
error (twice, after 1 and 3 seconds). From the moment the pair request is sent until its answer is finished, Cancel and
Escape rest and Settings' Re-pair and Disconnect wait: the dashboard replaces this install's record (and so revokes a
Re-pair's current key) as soon as it answers, before the new key is ever used, so the answer is always finished. When
a Re-pair's new key still gets no answer, the tray asks whether its current key survived: if it did (another dashboard
answered the pair) nothing is saved; if it did not, the new key is the only one that can work, so it is saved and used
and the next refresh proves it. A pair request that got no answer is handled the same way: never sent (nothing
listening), the connection is unchanged; cut off after the dashboard replaced the key, the tray is signed out and says
so; no answer either, the screen says the current key may have been replaced. The key is stored with DPAPI in the same file, scope and
entropy as before (`%LOCALAPPDATA%\CCS Bar\connection.dpapi`), as version 2 JSON with no password:
`{ version: 2, baseURL, username, deviceId, deviceToken, installId, pairedAt, rotateAfter }`. Every tray route then
carries the key as a bearer token and the cookie sign-in is never used. A dashboard that predates pairing (no
trusted-local-network rule, or no pair route) keeps today's password sign-in, verified before it is saved.

- **Rotation:** once `rotateAfter` has passed the tray calls `POST /api/auth/devices/me/rotate` on a good poll (at
  most hourly, re-reading `devices/me` every six hours) and writes the new key atomically before its first use. A
  rotation the dashboard defers (this connection not trusted right now) keeps the current key.
- **Signed out remotely:** any 401 `device_revoked`, `device_expired` or `invalid_token` deletes the key (the file
  keeps the address and username and why), stops polling and shows the signed-out screen; the tooltip says
  "Signed out". A 503 `auth_store_unavailable` or a network error is not a sign-out.
- **Upgrade from version 1 (section 8):** on launch, at most once per launch and every 24 hours, when the dashboard can
  pair this computer: the version 1 file is copied to `connection.v1-rollback.dpapi` (same bytes, with its time set to
  now, because a Windows copy keeps the source's time and the 24 hours count from the migration), the tray pairs with
  the stored credentials, writes version 2 and calls `devices/me`. On 200 the rollback is deleted; on a 401 device
  code version 1 comes back from the rollback and the cookie sign-in stays; on a network error both are kept and the
  check runs again on the next poll (the rollback never lives longer than 24 hours).

## Settings

Settings opens inside the panel with a visible X; the gear toggles it (and stays pressed while it is open) and Escape
closes it before a second Escape hides the panel. The gear also works on the sign-in screen: Settings slides over it,
and X or Escape return to it. It holds Appearance (Light / Dark / Auto), Connection, Start with Windows (the
installer's logon task), Keyboard shortcut, read-only facts from the dashboard (refresh interval, Codex and Antigravity
auto-switch, providers hidden in the trays, address) and About (version, third-party notices, Quit).

**Connection** reads "Paired as Windows tray, last synced 20s ago", then this PC and the saved address, then "This
connection: 192.168.50.31, trusted local network" (or "not trusted"), as the dashboard sees this computer right now
(`GET /api/auth/check`, read when Settings opens), so you can confirm once that the home VPN counts. **Re-pair** opens
the password step with Cancel: the current key keeps working until Pair is pressed, and pairing again with the same
install id revokes the old key. **Disconnect** asks inline, then calls `DELETE /api/auth/devices/me`, forgets the
key and shows the first-run screen with "Disconnected at ..." and the last address filled in; when the dashboard cannot
be reached nothing changes. A tray still on a version 1 password says so and offers Pair, whose screen reads "Sign in to
pair this tray": the saved password keeps working until pairing finishes, and Cancel changes nothing.

The version 1 password sign-in (a dashboard without pairing) verifies before it saves: a temporary client signs in
(`POST /api/auth/login`) and reads `GET /api/accounts/settings`; only when both work is the connection written and the
running client replaced. A blank password keeps the saved one only for the same dashboard; it is never sent to a new
address.

## Reopening the tray

- **Shortcuts:** the installer creates "AI Account Center" in the Start menu and on the Desktop (the plated Apex Soft
  app icon). Launching it starts the tray with its panel open; if the tray is
  already running, the new launch hands over to it (single instance) and the running tray shows its panel.
- **Keyboard:** Ctrl+Alt+A opens or hides the panel from anywhere while the tray runs. It is optional (Settings >
  Keyboard shortcut). If another app owns the combination, Settings says so and nothing is overridden.
- **At sign-in:** the logon task starts the tray hidden in the notification area (`--background`). A background start
  while a tray already runs (the task again, or its `CCS Bar` alias) exits and leaves the running panel alone.

## Codex and Antigravity switching

Activate switches the shared VM's saved Codex account (`/api/codex/profiles/{profile}/activate`) or Antigravity account
(`/api/antigravity/profiles/{id}/activate`, Ubuntu host, from commit 9cf75fbe). If running programs need to stop, a
native confirmation lists them; only "Stop, switch, restart" sends the server's short-lived token (Antigravity:
`/confirm`). Cancel changes nothing, and an expired or changed confirmation needs a fresh Activate. Antigravity's own
auto-switch (`/api/antigravity/auto-switch`, `thresholdUsedPercent` as % used) appears in its section header once two
Antigravity accounts are signed in; Codex's auto-switch (`thresholdPercent`, % remaining, shown as % used) sits in
its section header, above its accounts.
Failed refreshes keep the last sample, label it stale and disable account controls until a fresh read.

## Build and install

On Windows x64, install the **.NET 10 SDK** using Microsoft's
[.NET 10 downloads](https://dotnet.microsoft.com/en-us/download/dotnet/10.0). Run these from `windows-bar` as the target
Windows user; the tray installs per user without elevation:

```powershell
./scripts/Build.ps1
./scripts/Check.ps1
./scripts/Install.ps1
```

Builds are self-contained for Windows x64. Build and Check use an explicit `-Dotnet` path when supplied, otherwise
`dotnet` on PATH with a .NET 10 SDK, then the legacy private SDK at `%LOCALAPPDATA%\CCS Bar\build\dotnet\dotnet.exe`.
Build's `-OutputDirectory` and Check's `-PublishDirectory` and `-ReportDirectory` support isolated verification.

`Check.ps1` runs, offline and against an isolated state folder: the check suite (formatting, data rules, injection,
DPAPI, the authenticated HTTP fixture for the Codex and Antigravity routes, sign-in and Change against loopback
fixtures with a temporary store, the reset-pending rule, fonts, icons, easing), the render checks for Light and Dark, and
`Test-InstallScripts.ps1` (the installer parses; the task guard accepts only the two approved logon actions).
`-Live` reads your configured dashboard instead. `scripts/Test-SingleInstance.ps1 -PublishDirectory <dir>` checks the
reopen behaviour against an isolated state folder.

The installed executable is `%LOCALAPPDATA%\AI Account Center\app\AIAccountCenter.exe`. The installer verifies the
staged build first, backs up both app locations, both tasks and the three shortcuts, and restores them if anything
fails. Its scheduled logon task runs with the interactive user token, so launching from SSH does not strand the tray
icon in session 0. `-NoStart` defers launching until connection deployment.

## Private deployment and verification

For unattended setup, pipe a private JSON object containing `baseURL`, `username` and `password` into:

```text
dotnet CCSBar.dll --configure-stdin
```

It pairs at once and stores only the device key, as the install id already stored there when there is one (so a
reinstall replaces its own device record instead of leaving one behind); when the dashboard cannot pair this computer (no pairing yet, local
network trust off, not reachable) it stores the version 1 password sign-in as before. Input is never echoed and only
DPAPI ciphertext is written to `%LOCALAPPDATA%\CCS Bar\connection.dpapi`. Do not place this JSON in the source tree or
in command-line arguments. HTTP is accepted for local network origins; other origins require HTTPS.

- `--check <report>`: offline checks; the report holds no account emails or authentication data. Its windows never
  write the real `preferences.json` (the pairing checks keep theirs in their temporary folder), with or without
  `AAC_TRAY_STATE_DIR`.
- `--check-live <report>`: reads the configured dashboard without mutation (provider counts, id validation).
- `--render-fixture <dir> [light|dark]`: renders the real panel from the bundled sanitized fixture
  (`CCSBar/Fixtures/dashboard.json`, example.com identities) to PNGs, with Settings, details, a switch, two Antigravity
  accounts, a hidden provider and an unknown provider, and measures the selected-row rule (check and "Active" on the
  Activate button's left edge and label x within 0.5 px, before and after a switch), the platter's glide, one x for
  every section's name and first meter, whole compact resets, real gear and X clicks, and two runtime rules: refreshes
  (hidden or shown) leave no theme handlers behind, and an idle panel, shown or hidden, uses no CPU. It never connects
  anywhere.
- `--render-proof <png>`: renders the live panel against the configured dashboard without a second tray instance.
- `--e2e <step> <dir>`: one live end-to-end step against a sandbox dashboard (pair, wrong-password, pairing-off,
  reads, rotate, revoked, pair-again, disconnect, migrate, migrate-aged, repair-held, password-off, configure-twice),
  driven through the sign-in screen's own fields and button.
  It refuses to run unless `AAC_TRAY_STATE_DIR` names an isolated folder; credentials arrive on stdin and reports carry
  states, titles, store key names and status codes only.

The render checks also show each of the sign-in screen's states directly (light and dark) and check its title, which
regions it opens, that the card fits above the footer, that no single-line text is cut off and that the primary's label
is centred. The pairing checks drive the screen against a loopback fixture dashboard: every state and flow above,
verify-before-save, rotation, remote sign-out, Re-pair (Cancel and Escape held once Pair is pressed, a new key's check
asked again, a new key that never answers, a pair request never sent, an answer lost), Disconnect (and a rotation still
out when it lands), the version 1 upgrade and its rollback (including a version 1 file weeks old), Pair from Settings on
a version 1 tray, the password option while pairing is off, setup with the code, the fallback for a dashboard without
pairing, and `--configure-stdin` (run twice, one device record). The render checks also draw the paired Settings ›
Connection card and Disconnect's inline confirm.

No test activates a real Codex, Antigravity or Claude account, or pairs with a real dashboard. The Claude Open checks run the tray's own flow against a
loopback fixture dashboard with the connection in an isolated temporary store, and prove that no shell launch happens
when the dashboard cannot be reached.

## Branding compatibility

The primary launcher, shortcuts, window title and task are AI Account Center. The `CCS Bar` task remains a manual
compatibility alias pointing to the new launcher, and the old `%LOCALAPPDATA%\CCS Bar\app\CCSBar.exe` launcher still
opens the current product; both launchers share the singleton and show event. The `%LOCALAPPDATA%\CCS Bar` state folder
(DPAPI connection, `preferences.json` with the theme and shortcut choice) and the DPAPI purpose are intentionally
retained. Provider artwork provenance is in `CCSBar/Resources/Providers/PROVIDER-SOURCES.md` and `sources.json`;
notices in `THIRD-PARTY-NOTICES.txt`. The original MIT license and upstream attribution are preserved.
