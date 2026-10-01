# AI Account Center for Windows

Native C#/.NET 8 WPF tray companion for the [AI Account Center](https://github.com/sittingmongoose/ai-account-center) dashboard. It follows
the official [macOS CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar)
and Jared's supplied design: navy surfaces, a blue stacked app mark, blue usage
bars and compact account rows. Width is 760 logical pixels, clamped to
the monitor's work area. All four Claude and all three Codex accounts are
visible immediately inside one Claude box and one Codex box, with usage, reset
times and direct controls. Other
providers follow below; click an account row to reveal every window and balance.

Only accounts and their available usage windows are shown. Controls open the
four configured Claude profiles, switch the shared VM's saved Codex account,
and enable or disable its existing automatic switch monitor. Additional
providers are usage-only. Unknown percentages and reset times remain unknown.
Every provider window is retained, including hourly/rolling, weekly and monthly
limits when reported. Extra usage, numerical remaining balances, unlimited or
disabled states and reported credit expiration dates are displayed separately.
Entitlement expiration is never relabeled as a quota reset, and missing currency
or units are never inferred.
Failed refreshes label the last sample as stale and disable its account controls.
Retained optional balances have their own Cached label and original full local
sample time, including the time zone. A fresh core reading never replaces that
older sample time; a missing sample time remains unavailable.

The app is a thin client of the VM dashboard. It uses an authenticated cookie
session and the exact server Origin for writes. Provider credentials never
enter the app. The dashboard connection is saved in
`%LOCALAPPDATA%\CCS Bar\connection.dpapi` with current-user Windows DPAPI;
session cookies remain in memory. HTTP is accepted for local network origins;
other origins require HTTPS. No authentication material is included in source.

## Build and install

On Windows x64, install the **.NET 8 SDK** using Microsoft's
[.NET 8 downloads](https://dotnet.microsoft.com/en-us/download/dotnet/8.0) or
[Windows installation guide](https://learn.microsoft.com/en-us/dotnet/core/install/windows).
The SDK includes the desktop runtime needed for WPF; a runtime-only install does
not build the app. Reopen PowerShell after SDK installation.

Run these commands from `windows-bar` as the target Windows user. The bar installs
per user without elevation:

```powershell
./scripts/Build.ps1
./scripts/Check.ps1
./scripts/Install.ps1
```

Builds are self-contained for Windows x64. Build and Check use an explicit
`-Dotnet` executable path when supplied, otherwise an installed `dotnet` on PATH
with a .NET 8 SDK, then the existing legacy private SDK at
`%LOCALAPPDATA%\CCS Bar\build\dotnet\dotnet.exe`. They do not install an SDK.
For a custom location, pass the same path to both commands:

```powershell
./scripts/Build.ps1 -Dotnet 'C:\Tools\dotnet\dotnet.exe'
./scripts/Check.ps1 -Dotnet 'C:\Tools\dotnet\dotnet.exe'
```

Build's optional `-OutputDirectory` and Check's `-PublishDirectory` and
`-ReportDirectory` support isolated verification. The installer consumes the
default `publish` directory. Check runs offline fixtures by default; `-Live`
reads your configured dashboard.
The installed executable lives at `%LOCALAPPDATA%\AI Account Center\app\AIAccountCenter.exe`.
The installer adds an AI Account Center Start-menu shortcut and scheduled logon task
using the interactive user token, so launching from SSH does not strand the
tray icon in session 0. `-NoStart` defers launching until connection deployment.

Click the tray icon to open accounts. Right-click offers Open accounts and Quit.
Connection changes only the dashboard URL and dashboard login. The regular
read refresh uses the server-confirmed interval (30–3,600 seconds, default 60 seconds); opening or Refresh requests live data subject
to the server's coalescing. Reset timestamps display in Windows local time.
The confirmed threshold picker displays used percentages (85/90/95/98) and
sends the exact complementary remaining threshold (15/10/5/2) to the dashboard. The
current saved threshold remains selected until a successful server response.
The active Codex account is highlighted in its row with an Active badge. If activation finds running programs, a native confirmation
lists each program and warns that active work will be interrupted. Only
"Yes — Stop, Switch, Restart" sends the server's short-lived confirmation.
Cancel changes nothing. Expired or changed confirmations require a fresh
Activate gesture and a new warning; the tray never retries approval automatically.

The Windows Claude launcher uses only these existing, configured protocol URIs:
`ccs-claude://launch/platyr`, `/gmail`, `/party`, and `/me`. The Apple icon
requests the server's existing Mac launcher; the Windows icon opens locally. A new Claude desktop
profile still needs its own valid session; the tray does not copy credentials.

## Private deployment and verification

For unattended setup, pipe a private JSON object containing `baseURL`,
`username`, and `password` into:

```text
dotnet CCSBar.dll --configure-stdin
```

Input is never echoed and only DPAPI ciphertext is written. Do not place this
JSON in the source tree or in command-line arguments.

`--check <report-path>` runs formatting, injection, unknown-quota, DPAPI, and
authenticated HTTP fixture checks. `--check-live <report-path>` reads the
configured dashboard without mutation and records provider counts, capability
validation, and the monitor's enabled state. Reports contain no account emails
or authentication data. `--render-proof <png-path>` renders the actual native
WPF window against the live API for visual inspection without a second tray
instance. No test activates an actual Codex or Claude account.

Provider marks are shared with the Mac build from official provider assets,
MIT-licensed CCS SVG assets and installed provider app icons. Source URLs and
hashes for the corrected marks are recorded in
`CCSBar/Resources/Providers/sources.json`; the blue stacked mark follows Jared's concept.
See
LICENSE. No analytics, spend graphs, pools, proxy settings, logs, account
editing, provider switching, or update management are included.

The compact footer groups confirmed Codex auto-switch controls with Dashboard and
icon-only Refresh and connection settings. High-contrast native tooltips identify
account actions and usage details. Displayed fractions use up to two decimals;
the underlying usage values retain their precision. Chat pass windows, unreported
Pro 5-hour windows, Qwen subscription metadata, and empty Z.ai pack summaries are
omitted. Reported quotas, individual pack details, and the separate Go wallet
remain available by clicking each account row. Qwen’s clickable extra-pack
chip opens a native list of every reported individual pack, with actual amounts,
status and expiration. Muse’s compact row shows usage percentages; its details
retain the reported weighted-token counts. The compact footer places the actual
`at 95%`-style threshold immediately beside Auto-switch and its information icon.

## Branding compatibility

The primary launcher, shortcut, window title and task are AI Account Center.
The installer stages and verifies the renamed apphost before replacement, saves
rollback copies of both owned app locations and task definitions, and preserves
the existing startup task’s enabled state and triggers.
The existing `CCS Bar` task remains a manual compatibility alias pointing to the
new launcher, with no duplicate logon trigger. The old
`%LOCALAPPDATA%\CCS Bar\app\CCSBar.exe` launcher and Start-menu shortcut still
open the current product. Both launchers use the same singleton and show event,
so a legacy launch opens the existing window instead of creating a second tray.

The existing `%LOCALAPPDATA%\CCS Bar\connection.dpapi` location and DPAPI
purpose are intentionally retained. Branding changes do not rewrite private
connections, sessions, account settings, or history. Internal assembly/resource
identifiers, provider action URIs and API paths remain compatible. The original
MIT license and upstream attribution are preserved.
