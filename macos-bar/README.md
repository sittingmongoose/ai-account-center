# CCS Bar — accounts edition for macOS

A native SwiftUI menu bar app for the accounts dashboard. It uses the original
[CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar) branding and
packaging, narrowed to the requested account controls. The Windows WPF version
matches its 760-point navy panel, individual Claude/Codex rows, compact provider rows, account detail popovers, blue quota bars, and color palette.

The panel shows Claude, Codex, Antigravity, Muse Code, Cursor, Kimi Code, Qwen,
Z.ai, and OpenCode Go usage returned by CCS. Unknown usage stays unavailable;
reset and separate expiration labels use actual server timestamps in the computer's local time. Every reported hourly, rolling, weekly, and monthly window is shown, with extra usage, numeric remaining balances, and explicit unlimited/enabled states. Cached
rows remain marked. Provider credentials never enter the app. All four Claude and all three Codex accounts are visible directly under their section labels. Each compact row shows actual quota percentages and reset times, alongside Mac/Windows Claude launch buttons or Codex activation controls. The active Codex account has a green check, highlighted row, and its email in the panel header. The seven other providers use compact usage rows. Info buttons expose every reported window, balance, and expiration; no disclosure chevrons or account expansion is required. The panel grows up to the available workarea and scrolls only when the content exceeds it. Actual reported usage above 100% remains visible; only quota bar widths are capped at 100%.

Controls are limited to opening configured Claude profiles on the Mac, safely
activating native Codex profiles on the shared Ubuntu runtime, toggling Codex
automatic switching, and setting its confirmed threshold, with poll/idle settings. Seven added
providers have usage display only. Footer controls open the dashboard, refresh,
configure the connection and sign-in startup, or quit. Spend charts, pool routing,
tier locking, profile editing, alerts, and upstream automatic updates are absent.

## Build, verify, package, install

macOS 14+, Swift 5.9+; Command Line Tools or Xcode are sufficient. No npm/CCS CLI
is needed on the Mac because this edition connects to the existing dashboard.

```sh
swift build -c release
swift run ccs-bar-check
./Scripts/package_app.sh
./Scripts/install_user.sh --launch
```

The installer puts `CCS Bar.app` in `~/Applications`, verifies its ad-hoc
signature, preserves Gatekeeper metadata, and creates a per-user sign-in launch agent. With `--launch` it loads that agent into the active graphical session and starts the app through it. If an
older copy exists it first saves a backup under
`~/Library/Application Support/CCS Bar/Backups`. It preserves connection data.

## Private connection

Connection settings save `~/.ccs/bar/accounts-connection.json` in a private
directory (0700) with file mode 0600. The file is not part of the source or app
bundle. Its keys are `baseURL`, `username`, and `password`; configure them in the
native connection window or deploy the file privately. Existing saved connection files are preserved during app upgrades. The client signs in to
`POST /api/auth/login`, retains cookies only in an ephemeral session, and sends
the dashboard's exact Origin on mutations. It does not disable dashboard
authentication. Failed login polling backs off for 15 minutes.

The accounts contract is `GET /api/accounts/dashboard?platform=mac&refresh=true`
and schema version 1. Menu opening forces a server-backed refresh; background
reads use the dashboard’s saved usage refresh interval (60 seconds by default).
Adjust it in dashboard Settings; manual Refresh bypasses the normal sample cache. The dashboard service owns Codex automatic switching,
so closing this app does not disable it.

When manual activation finds running Codex programs, the native confirmation dialog lists their names, PIDs and roles, and warns that stopping them interrupts active work. Only Yes — Stop, Switch, Restart sends the server's one-use token. Cancel makes no further request. Expired or stale offers require a fresh Activate to review the current programs; confirmed requests are never replayed automatically after an authentication failure. Automatic switching still waits for idle.

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
Provider PNG marks were rendered from the fork's existing SVG assets, with Cursor's icon taken from the installed Mac app. The blue stacked CCS mark and Muse/OpenCode fallback marks follow the supplied concept.
All accounts models, authenticated client, grouped views, and user installer are
the accounts-edition implementation.
