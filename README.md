# AI Account Center

AI Account Center manages existing AI accounts and their subscription usage in
one place: a browser dashboard with Home, Analytics and Accounts & Settings
pages, plus native Mac and Windows trays that show the same accounts. It
switches saved Codex logins on the shared Ubuntu runtime under a guarded
transaction, opens configured Claude profiles on Mac or Windows, and runs
explicit, allowlisted updates of the installed provider apps. The browser
dashboard is Slint 1.18.1 compiled to WebAssembly; the backend remains
TypeScript.

It reaches four computers: the Ubuntu computer that runs the dashboard, a Mac,
a Windows computer and Nas1, a second Ubuntu computer reached over a fixed SSH
alias. Nas1 takes part in Update apps and Analytics only; it runs no dashboard
and holds no account state ([details](#nas1-the-fourth-computer)).

This is the continuing fork of [CCS](https://github.com/kaitranntt/ccs), now at
[sittingmongoose/ai-account-center](https://github.com/sittingmongoose/ai-account-center).
Repository history, original authorship, copyright and the [MIT license](LICENSE)
are preserved. This README documents the round-2 tree on
`integrate/aac-round2-final-20261002`; the installation below checks out that
branch explicitly. The repository default branch is managed independently.

## Dashboard pages

The dashboard serves three pages (`/`, `/analytics`, `/accounts`) with a
sign-in page when its own login is configured.

- **Home** shows Claude, then Codex and Antigravity as switchable sections,
  then one card per account of the other providers (Cursor, Muse Code,
  Kimi Code, Qwen, Z.ai, OpenCode Go). Meters show provider-reported
  percentages with severity colours, the auto-switch threshold notch and
  reported overage. A missing reading is drawn as unavailable, never as zero.
  Fable appears only for Claude Max plans, from the `seven_day_fable` window.
  A window whose reset time has passed while its reading predates the reset
  reads "Reset at 10:15 AM · new reading pending" over a dashed track until a
  newer reading arrives. Clicking a row opens Details with every visible quota
  window, balance, reset and expiration.
- **Analytics** keeps the Usage page: range presets (24H, 7D, 30D, Month, All)
  and custom ranges, a Claude Code / Codex filter, KPI cards, the usage-trends
  chart, cost by model, the model donut, sessions, token breakdown, cache
  efficiency, a weekday x hour heatmap, daily cost by provider, quota history
  and the resets agenda, all in local time. Ubuntu Claude Code and Codex CLI
  logs provide token and activity charts with API-equivalent cost estimates;
  those estimates are not subscription charges or billing attribution.
- **Included usage** in the Analytics header lists every CLI log source behind
  the Usage totals and each source's state and last scan, per computer
  (Ubuntu, Mac, Windows and Nas1). Besides Claude Code and Codex on Ubuntu,
  the totals count OMP, Muse Code and zcode logs from Ubuntu, Mac, Windows and
  Nas1, merged by model only. They are sources, never a provider, filter or
  legend; the per-provider session rows and daily chart stay Claude Code and
  Codex. Cost with no logged amount and no listed rate reads "Not logged",
  never $0.00, and totals that leave it out say "partial". Settings can add
  extra usage-log roots per computer; Nas1 takes POSIX paths.
- **Accounts & Settings** lists every provider with its accounts: status,
  last sample, how it signs in, and fixed action slots. Codex and Antigravity
  Activate, Claude Open on Mac or Windows, the Codex and Antigravity
  auto-switch policies, the usage-refresh interval and the appearance are
  live, and so is every sign-in and sign-out control the server offers. The
  settings column holds the Dashboard sign-in block, Update apps results by
  computer, read-only connection facts and About (with the standard
  `AboutSlint` widget).

## Visibility switches

Each provider and each account has two independent switches on
Accounts & Settings, both saved on the server: **Show on dashboard** and
**Show in tray**. The dashboard follows only `providers[].visible` and
`accounts[].hidden`; the trays follow only `providers[].trayVisible` and
`accounts[].trayHidden`. Each account can therefore be shown in both, on one
only, or in neither. The two lists are stored as four independent lists, so a
dashboard switch never changes a tray list and the reverse holds. Hidden
accounts are still collected and stay auto-switch candidates.

## Account add, sign-in again and remove

What a control may do comes from the server. A control is live, off with its
reason in plain words, refused (the click opens the reason and sends nothing),
or marked "coming" where the server has no flow or route yet.

| Provider | Add | Sign in again | Remove |
| --- | --- | --- | --- |
| Claude | A profile on Mac and Windows, then guided sign-in in the provider app | Guided app sign-in; Open on Mac or Windows from the row | To the 30-day trash, with Restore; a computer's default Claude profile is never offered for removal |
| Codex | Device code on Ubuntu, with the page, code and Cancel | Device code again; the identity must be unchanged | With confirmation; the active, default and last profiles cannot be removed |
| Antigravity | Supervised CLI login on the dashboard host (below) | The same supervised login for a saved, non-live profile | Deletes the saved login; no trash |
| Cursor | Guided app sign-in (one account) | Guided app sign-in, Open on Mac | With confirmation |
| Muse Code | Re-check is live; sign-in reads "coming" | Re-check | With confirmation |
| Kimi Code, Z.ai, OpenCode Go | API key in a masked field; only its last 4 are shown afterwards | Replace key | Deletes the stored key |
| Qwen | Guided browser-extension sign-in on Windows | Guided sign-in, Re-check | With confirmation |
| OpenCode console wallet | Guided browser-extension on Mac | Guided sign-in | Not served |

Antigravity add and sign-in again run the official CLI as a supervised job on
the dashboard host, inside the same isolated sign-in home as the terminal
command: the panel shows the Google sign-in page, you paste back the code
Google gives you, and the account appears once the CLI is stopped and the new
login is verified. Add is live whenever that isolation check passes on the
dashboard host and the connection is trusted.

Where the check cannot run (no `agy`, no bubblewrap, no unprivileged user
namespace) or the connection is not trusted, both answer with the terminal
command on Ubuntu, which the dashboard shows verbatim:

```bash
ai-account-center antigravity signin <profile>
```

Claude add, remove and restore need the server's Claude host steps
(`CCS_CLAUDE_HOST_LIFECYCLE=on` for that server process). The live service
keeps them off, so those controls read "coming" there. The default-profile
refusal (409 `account_protected`) holds either way. A just-added pending
Claude profile cannot be opened from its row; open "Claude (\<id\>)" from
Applications or the Start menu.

None of these controls, and no account switching, activation, key storage,
sign-in or Claude desktop flow, targets Nas1: it holds no AI Account Center
account state.

## Dashboard sign-in, devices and pairing

The dashboard has its own username and password login for browsers, separate
from provider logins. The sign-in page shows tries left after a wrong
password, a countdown while paused, and whether the session ended or was
signed out from another browser.

- **Password change** (Settings > Dashboard sign-in) needs a secure
  transport. It offers "Also sign out other browsers" (on by default) and
  names the paired trays that stay signed in; a password change never signs
  out a paired tray.
- **Devices.** Each tray pairs once with the dashboard password and then uses
  its own revocable device key. The dashboard lists paired trays with Revoke,
  plus Sign out other browsers and Sign out all devices. Keys rotate after
  30 days and expire after 90 days unused.
- **"Trust this local network"** (Settings > Dashboard sign-in) is off by
  default. When it is on, plain HTTP from a peer in the trusted networks
  counts as secure for password change, LAN first-run setup, device pair and
  rotate, key add and replace, sign-in job create and auth-code submit. The
  default trusted networks are 10/8, 172.16/12, 192.168/16 and fc00::/7;
  `dashboard_network.trusted_networks` in config.yaml is a CIDR list that
  replaces them. Turning trust on is allowed only from the dashboard computer
  itself (loopback) or by editing the config file; turning it off is allowed
  from any signed-in browser. The page shows "This connection: \<peer\>,
  trusted local network", "not trusted", "this computer" on loopback,
  "\<peer\>, encrypted" over direct HTTPS, or "\<client\>, encrypted
  through the HTTPS proxy" behind the HTTPS reverse proxy below.
- **Plain HTTP and WireGuard.** The dashboard stays on plain HTTP at its LAN
  address; there is no HTTPS. The risk, stated plainly: when trust is on,
  passwords, keys and sign-in codes cross the home network without
  encryption. It works over a WireGuard VPN when its clients arrive from a
  private range (for example the router's 10.6.0.x client subnet) or
  translated to the router's private address; an unusual VPN subnet can be
  added to `trusted_networks`. Public, link-local, CGNAT and unknown peers
  stay refused unless listed, and proxy headers never carry trust (except
  from the HTTPS reverse proxy below, and even then never LAN trust).
- **HTTPS reverse proxy on another computer.** To reach the dashboard from
  the internet without a VPN, put an HTTPS reverse proxy (for example SWAG
  on a NAS, behind Cloudflare) in front of it and tell the dashboard the
  proxy's exact LAN address, on the dashboard computer:

  ```sh
  ai-account-center dashboard proxy set --address 192.168.1.20 --origin https://aac.example.test
  ai-account-center dashboard proxy status
  ai-account-center dashboard proxy off
  ```

  `set` checks the values, saves the previous config.yaml next to it as
  `config.yaml.bak-proxy-<UTC time>` (0600) and writes `dashboard_tls`
  (`trusted_proxy: lan-https-proxy`, `trusted_proxy_addresses`,
  `public_origin`). The running dashboard applies it to its next request;
  no restart is needed. The address must be one exact private LAN address
  (up to 8, repeat `--address`): `set` refuses a range, loopback,
  `0.0.0.0`/`::`, a public address or one of the dashboard computer's own
  addresses, and if config.yaml holds one anyway the proxy stays off (fail
  closed, with one log line) while the usage hub keeps answering on
  loopback.

  What the dashboard then does with requests from that address:

  - They count as secure only with `X-Forwarded-Proto: https` and a valid IP
    address as the last `X-Forwarded-For` entry, which is taken as the
    client. Entries further left are ignored. They are never LAN-trusted and
    never count as the dashboard computer, and a password is taken only
    over their HTTPS side.
  - Their sign-in limits use their own keys (`proxy:<client>`), so nothing
    on the proxy's computer can spend the budget of the browser on the
    dashboard computer or of a LAN computer, whatever `X-Forwarded-For` it
    sends. All forwarded sign-ins also share 30 refusals per hour; anyone on
    the internet can use those up, but LAN and loopback sign-in never
    depend on them.
  - A browser session or tray key that ever crossed the LAN over plain HTTP
    does not work through the proxy: sign in again there. LAN and tray use
    over plain HTTP work as before.
  - `/v0/management` (the T3 usage hub) answers 404, and anonymous visitors
    are not shown the local network or session settings.

  A SWAG server block for this, with placeholders (`aac.example.test` for
  the public name, `192.168.1.10` for the dashboard computer). It accepts
  only Cloudflare, takes the client from `CF-Connecting-IP`, overwrites
  `X-Forwarded-For` with that one address, sends security headers on every
  answer and blocks the usage hub. It does not include `proxy.conf`, which
  would send a second `Host` header and make every change fail with 403.

  ```nginx
  # The TCP peer must be Cloudflare. $realip_remote_addr is the peer before
  # the real client is restored (allow/deny would see the client instead).
  # One line per range in https://www.cloudflare.com/ips-v4 and /ips-v6.
  geo $realip_remote_addr $aac_peer_is_cloudflare {
      default 0;
      198.51.100.0/24 1;                # placeholder: each Cloudflare range
  }

  server {
      listen 443 ssl;
      listen [::]:443 ssl;
      server_name aac.example.test;
      include /config/nginx/ssl.conf;
      client_max_body_size 4m;

      if ($aac_peer_is_cloudflare = 0) { return 444; }
      set_real_ip_from 198.51.100.0/24; # placeholder: each Cloudflare range
      real_ip_header CF-Connecting-IP;

      add_header Strict-Transport-Security "max-age=31536000" always;
      add_header X-Content-Type-Options "nosniff" always;
      add_header Referrer-Policy "no-referrer" always;

      # The T3 usage hub never faces the internet (any letter case).
      location ~* ^/v0/management { return 404; }

      location /ws {
          proxy_pass http://192.168.1.10:3000;
          proxy_http_version 1.1;
          proxy_set_header Upgrade $http_upgrade;
          proxy_set_header Connection "upgrade";
          proxy_set_header Host $host;
          proxy_set_header X-Forwarded-For $remote_addr;
          proxy_set_header X-Forwarded-Proto https;
          proxy_set_header X-Forwarded-Host "";
          proxy_read_timeout 1h;
      }

      location / {
          proxy_pass http://192.168.1.10:3000;
          proxy_http_version 1.1;
          proxy_set_header Host $host;
          proxy_set_header X-Forwarded-For $remote_addr;
          proxy_set_header X-Forwarded-Proto https;
          proxy_set_header X-Forwarded-Host "";
      }
  }
  ```

  Before going live: check that the request log shows the real public client
  as `remoteAddress` and the proxy as `via` for proxied requests (not a
  Docker gateway, a Cloudflare address or 127.0.0.1), use Cloudflare's SSL
  mode Full (strict), and consider Cloudflare Access in front of the name,
  so the dashboard password is the second lock rather than the only one.

## Use it on your phone

The dashboard is responsive down to small phones and installs as an app:

1. Open the dashboard's HTTPS address on the phone, for example
   `https://dashboard.example.test`, and sign in with the dashboard username
   and password. Password managers (iCloud Keychain, 1Password, Google
   Password Manager) offer the saved login and fill both fields.
2. Install it as an app: on iOS open the Share menu in Safari, then choose
   Add to Home Screen; on Android Chrome use the Install app button
   (Settings > Home screen app, or the browser menu). The installed app
   opens full-screen and follows the same light/dark setting.
3. The installed app on iOS keeps its own sign-in, separate from Safari, so
   sign in once more there; Remember me keeps it signed in.

No usage readings are stored on the phone: with no connection the installed
app shows an Offline card with Try again instead of last-known numbers.

## Native trays

The Mac menu bar app and the Windows tray app show the same accounts and
usage, with Claude launch controls and guarded Codex activation and
auto-switch settings. Both honour only the "Show in tray" switches. Each
tray pairs once over the trusted local network: the password is sent once,
the new key must answer before anything is saved, and the stored password is
then deleted. A password change on the dashboard leaves paired trays signed
in. Re-pair replaces the key only after the new one works and is saved, and
Disconnect revokes the key and returns to the first-run screen.

- **Sign-in screen.** Both trays replace the list with a sign-in screen on
  first run, after a remote sign-out, during Re-pair and after Disconnect.
  It covers the setup code, pairing in progress, "not on your local
  network", "pairing is turned off", wrong password with tries left, rate
  limited with a countdown, unreachable or wrong address, the upgrade from a
  stored password, signed out (revoked, sign out all devices, expired), and
  success with the hand-off into the list. A new tray starts with an empty
  address field (placeholder `http://`).
- **Mac menu-bar setting.** Settings > Menu bar chooses which reading the
  menu bar shows: any provider in the data, or Nothing for the icon alone;
  the Claude account when Claude is chosen; and Used or Remaining of the
  5-hour window, else the weekly one. Codex and Antigravity follow the active
  account. A reset-pending or unavailable reading shows the icon with no
  number, never 0%.
- **Reopening the trays.** On the Mac, click the menu-bar glyph (clicking it
  again closes the panel), relaunch the app from Spotlight, Launchpad,
  Finder or the Dock, or press Option-Command-A from any app (on by default;
  Settings > Open shortcut can turn it off). On Windows, use the Start menu
  or Desktop shortcut, or press Ctrl+Alt+A while the tray runs (optional;
  Settings > Keyboard shortcut). A second launch hands over to the running
  tray (single instance) and shows its panel.

The [Mac guide](macos-bar/README.md) and [Windows guide](windows-bar/README.md)
describe private storage, startup preferences, compatibility aliases and
rollback.

## Update apps and Cancel

**Update apps** (the header button, with results in Accounts & Settings)
starts one allowlisted, asynchronous job for the installed provider apps:
Antigravity CLI, Muse Code, OMP, Codex CLI, Claude Code, Codex Desktop,
Claude Desktop and T3 Code with its installed server runtime. Ubuntu runs
locally; the remote targets are the fixed Mac, Windows and Nas1 SSH aliases
in `APP_UPDATE_SSH_HOSTS`, so one run covers four computers x eight apps = 32
result rows. Helper deployment and the
existing Windows interactive task are required for remote updates. Absent
apps are skipped; per-app readiness checks tell unknown from failure, and an
unreachable computer reports Unknown rows. Only CLI instances running in a
terminal are stopped and reopened idle; background sessions (T3's agents, a
`claude -p` from a script, services) are never stopped and keep their version
until they start again. No update runs merely by opening
the dashboard or reading its status. See
[app update setup and behavior](scripts/app-updates/README.md).

**Cancel** is offered while a run is going. It is acknowledged at once: the
running computer's batch always finishes (installers are never killed) and
the queued computers are skipped ("Skipped: cancelled"). Cancel granularity
is one computer's batch, and Cancel never promises an undo.

## Nas1, the fourth computer

Nas1 is a second Ubuntu computer. AI Account Center reaches it only over the
fixed SSH alias `nas1-agent` and labels it "Nas1"; hosts and aliases are not
configurable. The dashboard's user needs that alias (reachable without a
prompt) and `/usr/bin/python3` on Nas1. AI Account Center is not installed on
Nas1 and no dashboard runs there.

- **Update apps.** Nas1 runs the same fixed helper as Ubuntu with
  `--platform ubuntu`. The helpers are synced, checksum-gated, to
  `~/.ccs/app-updates/` on Nas1 (a `sha256sum` hash query and a POSIX `tar`
  extract). The sync also ships a self-contained Codex stop/start runtime, so
  Codex Desktop and a Codex CLI daemon update on Nas1 without AI Account Center
  installed there. A T3 update on Nas1 schedules its own deferred restart of
  `t3code.service`.
- **Analytics.** Nas1 is scanned with the same pinned, read-only helper that
  the dashboard streams over SSH to the Mac and Windows (Claude Code, Codex,
  T3 shadow homes, OMP, Muse Code, zcode and Antigravity), and appears as a
  fourth computer beside Ubuntu, Mac and Windows. Extra usage-log roots can be
  set for Nas1 in Settings (POSIX paths).
- **Accounts.** A quota source belongs to a provider account, not to a
  computer. It runs on Nas1 only when it is saved as `platform: "ubuntu"` with
  `sshHost: "nas1-agent"`; its row then reads "on Nas1". By default no source
  moves there: Nas1 signs in to the same provider accounts as the dashboard
  computer, so a second reading would repeat the first.
- **Never on Nas1.** Account switching, Codex and Antigravity activation, key
  storage, sign-in and Claude desktop flows do not run there. The dashboard
  computer runs exactly these fixed commands on Nas1: the helper hash query,
  the helper extract, the update helper, the read-only analytics helper and,
  only for a source moved to Nas1, the usage collectors.

## Antigravity switching

Antigravity usage rows and saved-profile management are live, but account
switching is not enabled in this tree. Four independent gates hold it
closed: the dashboard release flag, the runtime release file, the runtime
installation and adoption, and the auto-switch setting (off by default).
`ai-account-center antigravity status` reports each gate read-only and names
the next step. Switching is released only after the live test (idle Ubuntu
`agy` switches to the party account and back); any other result keeps
switching off. The exact release, verify, live-test and rollback steps are in
[the Antigravity runtime guide](docs/antigravity-runtime.md).

## Dashboard previews

These screenshots show the actual compiled Slint 1.18.1 UI in a Mac browser,
using validated local fixture responses. Account identities are sanitized;
the quota readings and observed history retain their source values from the
2026-10-01 15:15 UTC snapshot. That earlier snapshot has no Fable Max readings,
so those slots remain unavailable. These previews show the compiled interface;
they do not prove current live-provider availability or deployed-backend behavior.

![AI Account Center dashboard with sanitized example account identities](assets/screenshots/ai-account-center-dashboard.png)

Dashboard preview: nine providers, Claude/Codex controls and complete usage cards.
Muse preserves the original sample time for its cached reading. No real account
emails or credentials appear.

![AI Account Center Analytics with sanitized example account identities](assets/screenshots/ai-account-center-analytics.png)

Analytics preview: actual CLI token/activity charts, API-equivalent estimates,
model plots, a combined provider/account/source overview and separate observed
histories. All 15 fixture accounts contribute 69 available histories; gaps are
preserved. See
[screenshot provenance](assets/screenshots/README.md).

## Platforms

| Component | Platform and requirements |
| --- | --- |
| Dashboard server / CLI | Node.js 22, matching CI, on Linux, macOS or Windows; local source builds also need Bun and the Slint toolchain below |
| Browser dashboard | Mac or Windows browser with WebGL2 and hardware acceleration; served by the dashboard server |
| Mac menu bar | macOS 26+, Xcode 26+ for source installation |
| Windows tray | Native .NET 10 WPF, Windows x64; source builds require an existing .NET 10 SDK |
| Shared Codex activation / auto-switch | Existing configured Ubuntu Codex runtime and saved logins |
| Usage collectors / app updates | Existing credentials and installed apps on the configured owner host; Python and approved SSH transport where needed |
| Nas1 (fourth computer) | A second Ubuntu computer with `/usr/bin/python3`, reached over its fixed SSH alias for app updates and Analytics only; no AI Account Center install, dashboard or account state |

Cross-platform source support does not make every account action available on
every host. The dashboard reports available capabilities. A container does not
provide another computer's provider sessions or native apps.

## Install from current source

There is no published npm package or registry image required for this installation.
Use the documented branch explicitly:

```bash
git clone --branch integrate/aac-round2-final-20261002 https://github.com/sittingmongoose/ai-account-center.git
cd ai-account-center
bun install --frozen-lockfile
```

The source build needs Rust **1.92+**, the `wasm32-unknown-unknown` target and
`wasm-pack`, in addition to Node.js 22, npm and Bun. Both `slint` and `slint-build`
are locked to **=1.18.1**. Install the tools using their official guides:
[Node.js and npm](https://nodejs.org/en/download),
[Bun](https://bun.sh/docs/installation),
[Rust/rustup](https://rust-lang.org/tools/install/) and
[wasm-pack](https://github.com/wasm-bindgen/wasm-pack#installation).
The repository's package-manager declaration is Bun 1.3.9. With those tools installed:

```bash
rustup target add wasm32-unknown-unknown
bun run build:all
bun run validate
bun run ui:validate
node dist/ccs.js dashboard --host localhost --port 3000 --no-open
```

Open [http://localhost:3000](http://localhost:3000). Running from the checkout
is enough to use the dashboard. For a server on a separate Ubuntu computer,
use its existing authenticated dashboard address or approved port forward from
your Mac/Windows browser; localhost refers to the computer running the browser.
To install the primary command globally from
this same checkout's local package:

```bash
./scripts/dev-install.sh --npm
ai-account-center dashboard
```

The installer builds and validates the source, creates a tarball and installs
that local tarball. It keeps the artifact for reinstall/rollback. `--bun` selects
Bun's global package directory. Use a user-owned global prefix; the Windows server
can be run directly from the built checkout, or the same local tarball can be
installed with npm. This repository does not fetch the upstream CCS npm package
as its installer.

See [Contributing](CONTRIBUTING.md) and the
[Slint dashboard guide](web-dashboard/README.md) for build ownership and checks.

## Set up and run

The default dashboard bind is localhost. Specify a port when a fixed address is
needed; otherwise the command selects an available port. Linux does not open a
browser automatically. Configure the dashboard's own login before exposing it
to other computers:

```bash
ai-account-center dashboard auth setup
ai-account-center dashboard auth show
ai-account-center dashboard --host localhost --port 3000 --no-open
```

`auth setup` prompts for dashboard credentials; these are separate from provider
logins. Existing `CCS_DASHBOARD_AUTH_ENABLED`, `CCS_DASHBOARD_USERNAME` and
`CCS_DASHBOARD_PASSWORD_HASH` overrides remain compatible. Keep credentials and
hashes private. Browser requests use the same server origin and authenticated
session. Each tray pairs once with the dashboard password and then keeps its
own private device key; see the tray guides and the pairing section above.

Existing account/profile data remains under `~/.ccs/`. `--config-dir` and
`CCS_DIR` select that directory directly; legacy `CCS_HOME` selects an alternate
home and appends `.ccs`. Claude/Codex account services use existing profiles and
native login data. To save a current Codex native login without changing its source:

```bash
ai-account-center codex-auth import-default personal
ai-account-center codex-auth show
```

Additional provider collectors run on the host that owns the corresponding login.
Their source selection is the private `account-usage-sources.json` configuration,
or the newer `account-usage-accounts.json` (registry v2, several accounts per
provider) when that file exists; existing approved SSH aliases select remote
Mac/Windows collectors, and a source saved as `platform: "ubuntu"` with
`sshHost: "nas1-agent"` runs on Nas1 and reads "on Nas1" (see
[Nas1](#nas1-the-fourth-computer)). Missing
credentials or collector setup yields unavailable usage. Follow the
[provider boundaries](docs/system-architecture/provider-flows.md),
[collector requirements](scripts/account-usage/README.md),
[OpenCode/Muse bridge](browser-bridge/opencode-muse/README.md) and
[Qwen bridge](browser-bridge/qwen/README.md) for the applicable existing source.
Do not copy account secrets into this repository or container images.

Claude desktop launch mappings use the private `claude-desktop-profiles.json`
configuration and the existing profile IDs. The optional
`opencode-console-wallet-source.json` adds a distinct workspace wallet source.
These are existing deployment contracts, not generic account creation forms;
another computer needs its own valid sessions and configured launch mappings.

For native companions, build/install as the target user:

```bash
# macOS, from this checkout
cd macos-bar
swift build -c release
swift run ccs-bar-check
./Scripts/package_app.sh
./Scripts/install_user.sh --launch
```

```powershell
# Windows PowerShell, from this checkout
cd windows-bar
./scripts/Build.ps1
./scripts/Check.ps1
./scripts/Install.ps1
```

On first run the tray asks for the dashboard address, then pairs with the
dashboard login; the stored password is deleted once the device key works.
Mac installs to `~/Applications/AI Account Center.app`; Windows installs to
`%LOCALAPPDATA%\AI Account Center\app\AIAccountCenter.exe`.
The [Mac guide](macos-bar/README.md) and [Windows guide](windows-bar/README.md)
describe private storage, startup preferences, compatibility aliases and rollback.
Windows source setup requires the [.NET 10 SDK](https://dotnet.microsoft.com/en-us/download/dotnet/10.0),
not only the runtime. Build/Check use an explicit `-Dotnet` path, an existing SDK
on PATH, or the existing private per-user SDK; installation stages the built app.
On Mac, `ai-account-center bar install --launch` can also build/install the native
sources included in the local package; `ai-account-center bar` starts or reuses
its local dashboard and opens the installed app.

## Current commands

| Command | Purpose |
| --- | --- |
| `ai-account-center dashboard` | Run the Slint dashboard; options `--host/-H`, `--port/-p`, `--no-open` |
| `ai-account-center dashboard auth setup/show/disable` | Manage dashboard login; `status` aliases `show` |
| `ai-account-center codex-auth create/login/import-default` | Manage saved Codex logins; native login uses the provider's own CLI |
| `ai-account-center codex-auth show [name]` | Inspect saved logins; `list` and `status` are aliases |
| `ai-account-center codex-auth activate <name>` | Guarded activation of the shared native login |
| `ai-account-center codex-auth remove <name>` | Remove an inactive saved login with confirmation; `--force` overrides only saved-default protection |
| `ai-account-center antigravity signin <profile>` | Sign in an Antigravity profile from a terminal on Ubuntu |
| `ai-account-center antigravity status` | Read-only report of the Antigravity switching gates |
| `ai-account-center bar` | Mac local dashboard/app launch; `serve`, `stop`, `status`, `install`, `uninstall`, `version` are available |
| `ai-account-center help` / `version` | Current command help / product version |

`--config-dir PATH` selects the existing account configuration directory.
Use `ai-account-center help dashboard`, `help codex-auth`, `help antigravity` or `help bar` for options.
`ccs` runs the same retained commands; **`ccs config` remains the dashboard alias**.
The [Codex guide](docs/codex-auth.md) documents import/resource compatibility and
[activation review](docs/activate-in-place.md).

## Update and recovery

To update AI Account Center, keep a private configuration backup and the prior
local tarball/app, then update your clean product checkout on
`integrate/aac-round2-final-20261002`, install its locked dependencies and rerun the same
local installer:

```bash
git pull --ff-only
bun install --frozen-lockfile
./scripts/dev-install.sh --npm
```

Rebuild/reinstall native apps with their commands above. Rebuild a Docker
installation with its existing Compose command. Existing private account data,
native connections and startup preferences are retained; keep prior artifacts
and volumes for rollback. A visibility file written by this build adds
`trayHiddenAccountIds`; an older rolled-back build reads it as "could not be
read safely" until its next full save, so save the visibility once from the
rolled-back page if that matters. Local changes should be reviewed before pulling.
`ai-account-center update` is retired and cannot update from the upstream package.

Adding Nas1 touches no account data. A rollback to a build without it loses
display data, and Settings only if a Nas1 extra usage-log root was saved. An
older build never opens the new analytics cache
`cache/analytics-remote-v1/nas1.json`. It does not restore a saved update job
(`app-updates/dashboard-job.json`) with more than 24 result rows (a finished
four-computer run has 32), so the last result is not shown and no update is
replayed. It treats a `dashboard-preferences.json` that holds a Nas1 extra
usage-log root as invalid as a whole: the defaults apply in memory, with a
warning and without overwriting the file, until the next save of Settings. The
usage source and registry files (`account-usage-sources.json`,
`account-usage-accounts.json`) keep their format, because a Nas1 source is
stored as `ubuntu` plus `sshHost`, which every earlier build reads and collects
the same way.

## Existing CCS installations

Preserve `~/.ccs/`, `CCS_HOME`, profile/provider IDs, session aliases, native
messaging identities and account capsules. Adopting the new visible name does
not require renaming private directories or copying credentials.

`ccsx`, `ccs-codex`, `ccsd`, `ccs-droid` and `ccsxp` are retained retirement-guidance
stubs. They no longer launch or route AI runtimes. `codex-auth use/switch` no
longer prints shell exports. Use `codex-auth activate` for shared login selection,
and a provider's own CLI for an AI session. Old routing/API-profile/proxy examples
remain upstream history rather than current setup instructions.

<!-- quickstart-snippet-start -->
## Quick Start (Docker)

From a checkout of [AI Account Center](https://github.com/sittingmongoose/ai-account-center):

```bash
docker compose -f docker/compose.yaml up -d --build
```

Open [the dashboard](http://localhost:3000). This builds the local source image;
it does not download the upstream CCS package or start a CLIProxy service.
Existing host credentials and usage-collector connections require explicit
configuration; see the Docker deployment guide in this checkout.
<!-- quickstart-snippet-end -->

See the [Docker guide](docker/README.md) for loopback access, authentication,
private volumes, explicit host-collector configuration and rollback. Container
instructions build the local dashboard image; they do not deploy native apps.

## Verification and development

```bash
bun run validate
bun run test:all
bun run ui:validate
```

The actual WASM build compiles Slint bindings and packages the UI at `dist/ui/`.
Offline fixture tests validate contracts and guards; they do not prove live
provider availability, successful app replacement or access to another computer.
Native checks and any live verification have their own scopes in the linked
guides. Current validation results belong to the tested revision and artifact.
Release verification is still in progress on this product branch.
Antigravity account switching is not enabled in this tree; its gates, release
steps and the required live test are documented in
[the Antigravity runtime guide](docs/antigravity-runtime.md).

Preserving an actual native conversation through switching still requires
verification. Vendor update/restart checks use controlled fixtures and do not
establish that every installed vendor app has undergone a live update.
The [test guide](tests/README.md) and [maintainer docs](docs/README.md) describe
retained checks and source owners.

Report product issues at
[AI Account Center issues](https://github.com/sittingmongoose/ai-account-center/issues).
Keep tokens, cookies, real account details and private configuration out of public
issues and screenshots. See [Security](SECURITY.md) for reporting boundaries.

## Known limits

- Cursor usage is not available locally: it is server-side only, and reads as
  a fixed "no local usage log" entry in Included usage.
- Antigravity usage (T3 Code's Antigravity instances included) is read from
  its conversation databases on every computer, but only as far back as those
  databases keep it, and its model names are what Antigravity stored: a model
  with no listed rate (for example one known only by a numeric id) shows
  "Not logged" for cost, never a guessed price.
- Update apps Cancel granularity is one computer's batch: the running
  computer's apps finish and the queued computers are skipped.
- Nas1 adds no quota rows by default and has no add, sign-in, key or switching
  control. Claude quota stays with the Mac and Windows Claude desktop apps,
  Codex quota with the dashboard computer's saved logins and the OpenCode
  console wallet with the Mac. A usage collector runs on Nas1 only for a source
  saved for it, and the usage helpers are copied to a computer by hand, not
  synced like the update helpers.
- Claude add, remove and restore read "coming" while the server's Claude host
  steps are off (the live service keeps them off); the default-profile
  refusal holds either way.
- Antigravity add and sign-in again run the supervised CLI login on the
  dashboard host, which needs Ubuntu there with the official `agy` CLI,
  bubblewrap and an unprivileged user namespace, plus a trusted connection
  for the pasted code; without them the dashboard shows the terminal command
  instead. Muse Code sign-in reads "coming" (Re-check is live).

## Attribution

The original CCS project was created by **Tam Nhu Tran (Kai)** and the **CCS
Contributors**. Original package author metadata and
`Copyright (c) 2025 CCS Contributors` remain under the unchanged
[MIT license](LICENSE). Upstream source/history remain at
[kaitranntt/ccs](https://github.com/kaitranntt/ccs).

Slint 1.18.1 uses its royalty-free license; the dashboard retains the standard
AboutSlint widget. See [dashboard licensing](web-dashboard/README.md#dependencies-and-attribution).
Provider artwork retains its [source and attribution notices](web-dashboard/public/assets/THIRD-PARTY-NOTICES.txt).
Original CCS routing work credited
[claude-code-router](https://github.com/musistudio/claude-code-router), and the
original project used [ClaudeKit](https://claudekit.cc).

## Community Projects

[opencode-ccs-sync](https://github.com/JasonLandbridge/opencode-ccs-sync), by
[@JasonLandbridge](https://github.com/JasonLandbridge), serves the original CCS
provider configuration. This preserves its original project credit and does not
imply current dashboard integration.

## Star History

[AI Account Center repository](https://github.com/sittingmongoose/ai-account-center)
and [original CCS history](https://star-history.com/#kaitranntt/ccs&Date).
