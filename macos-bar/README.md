# AI Account Center for macOS

The macOS menu bar app in [AI Account Center](https://github.com/sittingmongoose/ai-account-center), renamed from the CCS Bar accounts edition. Its packaging originated in
[CCS Bar](https://github.com/kaitranntt/ccs/tree/main/macos-bar); upstream credit, license and compatibility identifiers are preserved. The Windows WPF version
matches its 760-point navy panel, individual Claude/Codex rows, compact provider rows, account detail popovers, blue quota bars, and color palette.

The panel shows Claude, Codex, Antigravity, Muse Code, Cursor, Kimi Code, Qwen,
Z.ai, and OpenCode Go usage returned by AI Account Center. Unknown usage stays unavailable;
reset and separate expiration labels use actual server timestamps in the computer's local time. Supported visible hourly, rolling, weekly, and monthly windows are shown, with extra usage, numeric remaining balances, and explicit unlimited/enabled states. Cached
rows remain marked. Provider credentials never enter the app. All four Claude and all three Codex accounts are visible directly under their section labels. Each compact row shows actual quota percentages and reset times, alongside Mac/Windows Claude launch buttons or Codex activation controls. The active Codex account has a green check, highlighted row, and its email in the panel header. The seven other providers use compact usage rows. Info buttons expose the supported visible windows, balances, and expiration; raw provider fields remain intact. Codex Chat pass and unknown Pro five-hour windows, Qwen plan subscription, and empty Z.ai reset-pack summaries are hidden; actual Lex five-hour usage and real extra balances remain visible. Fractional display values use at most two decimal places. Native AppKit tooltip labels name each info and footer action. The footer places Codex auto-switch and its threshold beside Dashboard, Refresh and Settings; no disclosure chevrons or account expansion is required. The panel grows up to the available workarea and scrolls only when the content exceeds it. Actual reported usage above 100% remains visible; only quota bar widths are capped at 100%.

Controls are limited to opening configured Claude profiles on Mac or Windows, safely
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

The installer stages and verifies `AI Account Center.app` before replacing the installed app in `~/Applications`. It preserves Gatekeeper metadata and the existing sign-in startup preference. Existing owned apps are saved in unique backups under `~/Library/Application Support/CCS Bar/Backups`; unrelated apps or foreign symlinks at either install path are rejected. With `--launch`, it starts only this app through the enabled GUI launch agent, or opens it directly if sign-in startup is disabled.

### Rename compatibility

`~/Applications/CCS Bar.app` becomes a relative symlink to `AI Account Center.app`, so existing launch paths continue to resolve. The app retains its executable name `CCSBar`, bundle identifier and launch-agent label `party.sittingmongoose.ccs.accounts-bar`, and private configuration directory `~/.ccs/bar`. The launch-agent program path moves to the new canonical app path. No provider identity, authenticated dashboard setting, API route or native-host identifier is renamed. Existing connection data, account selection, threshold and refresh cadence stay unchanged. The original MIT license is bundled in the app and linked to upstream attribution in About.

`python3 Scripts/migration_check.py` exercises migration, repeat install, alias safety and rollback using temporary fixture apps; it makes no live app or account changes.

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
