# AI Account Center for Windows

Native C#/.NET 8 WPF tray companion for the [AI Account Center](https://github.com/sittingmongoose/ai-account-center)
dashboard, with a WinForms `NotifyIcon` in the notification area. It follows the official
[macOS CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar) and the approved Daylight Atlas tray design
(the Windows view of the tray polish concept): today's layout and order, in the dashboard's palette, meters and type.

The panel is 760 logical pixels wide (850 tall), clamped to the monitor's work area, and opens above the taskbar:

- **Header:** the Apex Soft mark, "AI Account Center" and one status line ("9 of 9 reporting · cached · updated
  3:15 PM"; hover for the relative time and any providers hidden on the dashboard).
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
- **Footer:** the Codex auto-switch toggle, its `at 95%` threshold menu and an info popover; Dashboard; Refresh;
  Settings.

Click any row to expand its details (every window, balance, expiry and the account's actions); nested buttons keep
their own action. Hover tints a row, brightens its tracks and shows a chevron.

## Data rules

The tray is a thin client of `GET /api/accounts/dashboard?platform=windows`. Unknown percentages and reset times stay
unknown ("Unavailable", never 0), nothing is summed or averaged across accounts, and displayed fractions use at most two
decimals (the raw values keep their precision). Codex Chat pass windows, Qwen subscription metadata, empty Z.ai pack
summaries and an unreported Codex 5-hour window are not shown. Retained (cached) readings keep their original sample
time in the tooltip and details. Providers listed in `hiddenProviders` (top level, or in `settings`) are left out
when the dashboard reports them; until then Settings says the server does not report them. Provider ids, Codex profiles,
Claude profile ids and Antigravity profile ids come from the data and are accepted only when they are URI and path
safe (`[A-Za-z0-9][A-Za-z0-9_-]{0,63}`); there is no allowlist of account names.

## Look and motion

- **Type:** Instrument Sans, embedded as static WPF font resources (Regular, Medium, SemiBold, Bold and SemiCondensed
  SemiBold for meter values) with tabular figures frozen into every face; "%" is part of the same text run as its
  number. See `CCSBar/Resources/Fonts/README.md`.
- **Light / Dark / Auto:** Settings, live. Auto follows the Windows app mode (`AppsUseLightTheme`) and re-applies on
  `SystemEvents.UserPreferenceChanged`; colours cross-fade over 300 ms.
- **Icons:** the Apex Soft notification-area icon and app icon, light or dark taskbar art picked from
  `SystemUsesLightTheme`; official provider marks (`marks-v2`, light-surface variants in Light, never recoloured);
  filled Apple and Windows glyphs; Lucide UI icons drawn as vectors. No glyph-font or emoji icons.
- **Motion:** the panel fades and rises on open with a short stagger; bars sweep from 0 on the first open of a session
  and later only changed readings move. Meter widths and numbers ease out (cubic) and never pass the reading; the
  severity gradient cross-fades. Springs are used only on controls: the toggle knob, the segmented thumb and the gear's
  60 degree turn. Rows tint on hover (140 ms), details expand with an animated height, Settings slides over the list,
  Refresh spins and eases to rest. Windows' "show animations" setting off turns motion off.
- **Tray menu:** right-click shows a restyled menu (Open accounts, Open dashboard, Refresh now, Settings, Quit) in the
  panel's palette and font, with Windows 11 rounded corners. The tooltip carries usage: "AI Account Center · Codex
  codex-2: 91% weekly left".

## Settings

Settings opens inside the panel with a visible X; the gear toggles it (and stays pressed while it is open) and Escape
closes it before a second Escape hides the panel. It holds Appearance (Light / Dark / Auto), Connection (who is signed
in, the dashboard address and Change), Start with Windows (the installer's logon task), Keyboard shortcut, read-only
facts from the dashboard (refresh interval, Codex and Antigravity auto-switch, hidden providers, address) and About
(version, third-party notices, Quit).

## Reopening the tray

- **Shortcuts:** the installer creates "AI Account Center" in the Start menu and on the Desktop (Apex Soft icon,
  matched to the taskbar theme at install time). Launching it starts the tray with its panel open; if the tray is
  already running, the new launch hands over to it (single instance) and the running tray shows its panel.
- **Keyboard:** Ctrl+Alt+A opens or hides the panel from anywhere while the tray runs. It is optional (Settings >
  Keyboard shortcut). If another app owns the combination, Settings says so and nothing is overridden.
- **At sign-in:** the logon task starts the tray hidden in the notification area (`--background`).

## Codex and Antigravity switching

Activate switches the shared VM's saved Codex account (`/api/codex/profiles/{profile}/activate`) or Antigravity account
(`/api/antigravity/profiles/{id}/activate`, Ubuntu host, from commit 9cf75fbe). If running programs need to stop, a
native confirmation lists them; only "Stop, switch, restart" sends the server's short-lived token (Antigravity:
`/confirm`). Cancel changes nothing, and an expired or changed confirmation needs a fresh Activate. Antigravity's own
auto-switch (`/api/antigravity/auto-switch`, `thresholdUsedPercent` as % used) appears in its section header once two
Antigravity accounts are signed in; Codex keeps its footer control (`thresholdPercent`, % remaining, shown as % used).
Failed refreshes keep the last sample, label it stale and disable account controls until a fresh read.

## Build and install

On Windows x64, install the **.NET 8 SDK** using Microsoft's
[.NET 8 downloads](https://dotnet.microsoft.com/en-us/download/dotnet/8.0). Run these from `windows-bar` as the target
Windows user; the tray installs per user without elevation:

```powershell
./scripts/Build.ps1
./scripts/Check.ps1
./scripts/Install.ps1
```

Builds are self-contained for Windows x64. Build and Check use an explicit `-Dotnet` path when supplied, otherwise
`dotnet` on PATH with a .NET 8 SDK, then the legacy private SDK at `%LOCALAPPDATA%\CCS Bar\build\dotnet\dotnet.exe`.
Build's `-OutputDirectory` and Check's `-PublishDirectory` and `-ReportDirectory` support isolated verification.

`Check.ps1` runs, offline: the check suite (formatting, data rules, injection, DPAPI, the authenticated HTTP fixture
for the Codex and Antigravity routes, fonts, icons, easing), the render checks for Light and Dark, and
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

Input is never echoed and only DPAPI ciphertext is written to `%LOCALAPPDATA%\CCS Bar\connection.dpapi`. Do not place
this JSON in the source tree or in command-line arguments. Session cookies stay in memory. HTTP is accepted for local
network origins; other origins require HTTPS. Device pairing (a revocable device key instead of the password) waits for
the dashboard's pairing routes; until then a dashboard password change needs a new sign-in in Settings.

- `--check <report>`: offline checks; the report holds no account emails or authentication data.
- `--check-live <report>`: reads the configured dashboard without mutation (provider counts, id validation).
- `--render-fixture <dir> [light|dark]`: renders the real panel from the bundled sanitized fixture
  (`CCSBar/Fixtures/dashboard.json`, example.com identities) to PNGs, with Settings, details, a switch, two Antigravity
  accounts, a hidden provider and an unknown provider, and measures the selected-row rule (check and "Active" on the
  Activate button's left edge and label x within 0.5 px, before and after a switch) and the platter's glide. It never
  connects anywhere.
- `--render-proof <png>`: renders the live panel against the configured dashboard without a second tray instance.

No test activates a real Codex, Antigravity or Claude account.

## Branding compatibility

The primary launcher, shortcuts, window title and task are AI Account Center. The `CCS Bar` task remains a manual
compatibility alias pointing to the new launcher, and the old `%LOCALAPPDATA%\CCS Bar\app\CCSBar.exe` launcher still
opens the current product; both launchers share the singleton and show event. The `%LOCALAPPDATA%\CCS Bar` state folder
(DPAPI connection, `preferences.json` with the theme and shortcut choice) and the DPAPI purpose are intentionally
retained. Provider artwork provenance is in `CCSBar/Resources/Providers/PROVIDER-SOURCES.md` and `sources.json`;
notices in `THIRD-PARTY-NOTICES.txt`. The original MIT license and upstream attribution are preserved.
