# CCS Bar for Windows

Native C#/.NET 8 WPF tray companion for the CCS accounts dashboard. It follows
the official [macOS CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar)
and Jared's supplied design: navy surfaces, a blue stacked CCS mark, blue usage
bars and compact account rows. Width is 760 logical pixels, clamped to
the monitor's work area. All four Claude and all three Codex accounts are
visible immediately, with usage, reset times and direct controls. Other
providers follow below; info buttons reveal every window and balance.

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

The app is a thin client of the VM dashboard. It uses an authenticated cookie
session and the exact server Origin for writes. Provider credentials never
enter the app. The dashboard connection is saved in
`%LOCALAPPDATA%\CCS Bar\connection.dpapi` with current-user Windows DPAPI;
session cookies remain in memory. HTTP is accepted for local network origins;
other origins require HTTPS. No authentication material is included in source.

## Build and install

Run PowerShell as the target Windows user, without elevation:

```powershell
./scripts/Install-Sdk.ps1
./scripts/Build.ps1
./scripts/Check.ps1
./scripts/Install.ps1
```

Builds are self-contained for Windows x64. The pinned per-user SDK uses
[Microsoft's official install script](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script).
The installed executable lives at `%LOCALAPPDATA%\CCS Bar\app\CCSBar.exe`.
The installer adds a Start-menu shortcut and a `CCS Bar` scheduled logon task
using the interactive user token, so launching from SSH does not strand the
tray icon in session 0. `-NoStart` defers launching until connection deployment.

Click the tray icon to open accounts. Right-click offers Open accounts and Quit.
Connection changes only the dashboard URL and dashboard login. The regular
read refresh is once per minute; opening or Refresh requests live data subject
to the server's coalescing. Reset timestamps display in Windows local time.
The confirmed threshold picker displays used percentages (85/90/95/98) and
sends the exact complementary remaining threshold (15/10/5/2) to CCS. The
current saved threshold remains selected until a successful server response.
The active Codex account is named in the header and highlighted in its row with
an Active badge. If activation finds running programs, a native confirmation
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

Provider marks are shared with the Mac build from MIT-licensed CCS SVG assets
and installed provider app icons; the blue stacked mark follows Jared's concept.
See
LICENSE. No analytics, spend graphs, pools, proxy settings, logs, account
editing, provider switching, or update management are included.
