# AI Account Center user guide

The detailed reference behind the [README](../README.md): every page, control, sign-in flow,
security setting and operating note, kept in one place. The README has the overview and
quick start; come here when you need the exact behavior.

AI Account Center runs its dashboard on a Linux computer and can reach remote computers over
SSH: a Mac and a Windows computer (Claude desktop profiles, collectors, Analytics, app updates
and the native trays), and optionally a second Linux computer used only for Update apps,
Analytics and verified usage fallback. The remote computers and their SSH aliases are fixed
in source, not set in Settings ([system architecture](system-architecture/index.md)).

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
  and custom ranges, a provider picker, KPI cards, tokens by provider, the
  usage-trends chart, cost by model, the model donut, sessions, token
  breakdown, cache efficiency, a weekday x hour heatmap, daily cost by
  provider, quota history and the resets agenda, all in local time. CLI logs
  on every computer provide token and activity charts with API-equivalent cost
  estimates; those estimates are not subscription charges or billing
  attribution.
- **Providers in Analytics.** Usage counts under the dashboard provider whose
  subscription or route served it, not under the tool that logged it. Claude
  Code, Codex, the Muse Code CLI and Antigravity are their own provider's
  usage. OMP and zcode log a route per call (for example a Z.ai coding plan or
  Kimi Code), so their usage counts under that route's provider (Claude, Codex,
  Antigravity, Muse Code, Cursor, Kimi Code, Qwen, Z.ai or OpenCode Go). A
  route no provider claims (a local model server, OpenRouter) counts as
  "Other", never a guess. The provider picker lists every provider with usage
  in the range; picking one shows its usage by model, and the session rows and
  daily cost chart follow the same split.
- **Included usage** in the Analytics header lists every CLI log source behind
  the Usage totals and each source's state and last scan, per computer.
  Sources are tools on a computer (for example Claude Code, Codex, OMP, Muse
  Code, zcode or Antigravity); they are not providers. Cost with no logged
  amount and no listed rate reads "Not logged", never $0.00, and totals that
  leave it out say "partial". Settings can add extra usage-log roots per
  computer.
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
(`CCS_CLAUDE_HOST_LIFECYCLE=on` for that server process). They are off by
default, so those controls read "coming" until you turn them on. The default-profile
refusal (409 `account_protected`) holds either way. A just-added pending
Claude profile cannot be opened from its row; open "Claude (\<id\>)" from
Applications or the Start menu.

None of these controls, and no account switching, activation, key storage,
sign-in or Claude desktop flow, targets the optional second Linux computer: it
holds no AI Account Center account state.

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

The [Mac guide](../macos-bar/README.md) and [Windows guide](../windows-bar/README.md)
describe private storage, startup preferences, compatibility aliases and
rollback.

## Update apps and Cancel

**Update apps** (the header button, with results in Accounts & Settings)
starts one allowlisted, asynchronous job for ten installed apps: Antigravity
CLI, Muse Code, OMP, Codex CLI, Codex Desktop, Claude Code, Claude Desktop,
ZCode, the T3 ACP adapters and T3 Code with its installed server runtime. The
dashboard computer runs its helper locally; each remote computer runs its own
synced helper over SSH. Every run reports one result row per app on every
computer (ten rows per computer). Helper deployment and, on Windows, the
existing interactive task are required for remote updates. Absent apps are
skipped; per-app readiness checks tell unknown from failure, and an
unreachable computer reports Unknown rows. Only CLI instances running in a
terminal are stopped and reopened idle; background sessions (T3's agents, a
`claude -p` from a script, services) are never stopped and keep their version
until they start again, except Windows Codex CLI: when a newer npm version is
verified, Update apps forces its package processes to stop so locked files can
be replaced. Background work is not replayed; its result says to start those
sessions again. The Antigravity CLI is held at the newest reviewed
build, so switching never meets an unreviewed version. No update runs merely
by opening the dashboard or reading its status. See
[app update setup and behavior](../scripts/app-updates/README.md).

**Computers run at the same time.** Each computer runs up to four independent
updates together, with live progress. Shared installers and Codex runtime work
stay serialized; Codex Desktop follows its CLI, and T3 waits for every other
app. A computer that does not finish within its time limit (33 minutes)
reports its remaining apps as Unknown, so
one stuck computer never holds the others back.

**Cancel** is offered while a run is going. It is acknowledged at once and
nothing running is killed: each computer finishes its active apps, then
reports every app it has not started as skipped ("Skipped: cancelled"). A
computer still preparing its helper skips all its apps. Cancel never promises
an undo.

## The optional second Linux computer

A second Linux computer can take part without AI Account Center installed on
it and without running a dashboard there. The dashboard reaches it only over
an SSH alias fixed in source; hosts and aliases are not configurable. The
dashboard's user needs that alias (reachable without a prompt) and
`/usr/bin/python3` on that computer.

- **Update apps.** It runs the same fixed helper as the dashboard computer
  with `--platform ubuntu`. The helpers are synced, checksum-gated, to
  `~/.ccs/app-updates/` there (a `sha256sum` hash query and a POSIX `tar`
  extract). The sync also ships a self-contained Codex stop/start runtime, so
  Codex Desktop and a Codex CLI daemon update there without AI Account Center
  installed. A T3 Code update there schedules its own deferred restart of the
  T3 service.
- **Analytics.** It is scanned with the same pinned, read-only helper that
  the dashboard streams over SSH to the Mac and Windows (Claude Code, Codex,
  T3 shadow homes, OMP, Muse Code, zcode and Antigravity), and appears as its
  own computer in Analytics. Extra usage-log roots can be set for it in
  Settings (POSIX paths).
- **Accounts.** A quota source belongs to a provider account. This computer
  can supply a fallback reading from its own existing sign-in when the
  verified account identity matches, without adding a row or moving the
  primary source. An explicit primary source there uses `platform: "ubuntu"`
  with that computer's `sshHost`. The packaged usage helper runs in memory
  over SSH; no helper installation or credential copying is required.
- **Never there.** Account switching, Codex and Antigravity activation, key
  storage, sign-in and Claude desktop flows do not run there. The dashboard
  computer runs exactly these fixed commands on it: the helper hash query,
  the helper extract, the update helper, the read-only analytics helper and
  the read-only usage bundle.

## Antigravity switching

Antigravity account switching is released: Activate and the
`ai-account-center antigravity` commands switch the Antigravity CLI (`agy`)
between saved profiles on the Linux dashboard computer, as a guarded
transaction (prove the identity, swap the login, verify, roll back on any
mismatch). When the CLI is running, a manual switch needs a one-use
confirmation of the exact processes it will stop; busy or unmanaged sessions
stay deferred. It needs:

- the official Antigravity CLI at `~/.local/bin/agy`, at a reviewed build
  (a newer, unreviewed build pauses switching until it is reviewed);
- at least two saved profiles (`ai-account-center antigravity signin <profile>`);
- the switching runtime installed and adopted, and its user service running.

**Automatic switching is opt-in and off by default** (default threshold 95%
used, checked every 60 seconds, no quota pool chosen). Turn it on in the
Antigravity auto-switch policy and choose a quota pool that both accounts
report; it stops only an idle CLI it can prove it owns. `ai-account-center antigravity status` reports every prerequisite
read-only and names the next step. The install, adoption, verify and rollback
steps are in [the Antigravity runtime guide](antigravity-runtime.md).

## Platforms

| Component | Platform and requirements |
| --- | --- |
| Dashboard server / CLI | Node.js 22, matching CI, on Linux, macOS or Windows; local source builds also need Bun and the Slint toolchain below |
| Browser dashboard | Mac or Windows browser with WebGL2 and hardware acceleration; served by the dashboard server |
| Mac menu bar | macOS 26+, Xcode 26+ for source installation |
| Windows tray | Native .NET 10 WPF, Windows x64; source builds require an existing .NET 10 SDK |
| Shared Codex activation / auto-switch | Linux only (activation refuses any other platform: "In-place Codex activation currently requires Linux"); an existing configured Codex runtime and saved logins |
| Antigravity activation / auto-switch | Linux dashboard computer with the official `agy` CLI at a reviewed build and the switching runtime (see [Antigravity switching](#antigravity-switching)) |
| Usage collectors / app updates | Existing credentials and installed apps on the owner computer; Python 3 there (`/usr/bin/python3` on Linux and Mac, `python.exe` on Windows) and SSH from the dashboard computer for remote ones |
| Optional second Linux computer | `/usr/bin/python3`, reached over its fixed SSH alias for app updates, Analytics and verified usage fallback; no AI Account Center install, dashboard or managed account state |

Cross-platform source support does not make every account action available on
every host. The dashboard reports available capabilities. A container does not
provide another computer's provider sessions or native apps.

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
Mac/Windows collectors, and a source saved as `platform: "ubuntu"` with the
second Linux computer's `sshHost` runs there (see
[the optional second Linux computer](#the-optional-second-linux-computer)). If a primary source fails, the shared
coordinator can use an existing sign-in for the exact provider account on another
configured computer. Credentials stay on their owner host, and the packaged
helper runs in memory over SSH. Verified readings keep their original timestamps
in a persistent cache; source outages can show cached usage after restart.
Provider rate limits apply across native collectors and all alternate hosts for
the same identity. Sign-in warnings stay visible alongside recovered quota.
A first-ever unavailable source without a known identity stays unavailable.
Explicit source and lifecycle configuration is unchanged. Follow the
[provider boundaries](system-architecture/provider-flows.md),
[collector requirements](../scripts/account-usage/README.md),
[OpenCode/Muse bridge](../browser-bridge/opencode-muse/README.md) and
[Qwen bridge](../browser-bridge/qwen/README.md) for the applicable existing source.
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
The [Mac guide](../macos-bar/README.md) and [Windows guide](../windows-bar/README.md)
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
The [Codex guide](codex-auth.md) documents import/resource compatibility and
[activation review](activate-in-place.md).

## Update and recovery

To update AI Account Center, keep a private configuration backup and the prior
local tarball/app, then update your clean product checkout on
`main`, install its locked dependencies and rerun the same
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

Rolling back to an older build touches no account data. An older build
ignores newer per-computer analytics caches. It does not restore a saved
update job (`app-updates/dashboard-job.json`) with more result rows than it
expects (a build that knows fewer computers or apps expects fewer), so the
last result is not shown there and no update is replayed. It may treat a
`dashboard-preferences.json` that holds an extra usage-log root for a computer
it does not know as invalid as a whole: the defaults apply in memory, with a
warning and without overwriting the file, until the next save of Settings.
The usage source and registry files (`account-usage-sources.json`,
`account-usage-accounts.json`) keep their format, because a second Linux
computer's source is stored as `ubuntu` plus `sshHost`, which earlier builds
read and collect the same way.

## Existing CCS installations

Preserve `~/.ccs/`, `CCS_HOME`, profile/provider IDs, session aliases, native
messaging identities and account capsules. Adopting the new visible name does
not require renaming private directories or copying credentials.

`ccsx`, `ccs-codex`, `ccsd`, `ccs-droid` and `ccsxp` are retained retirement-guidance
stubs. They no longer launch or route AI runtimes. `codex-auth use/switch` no
longer prints shell exports. Use `codex-auth activate` for shared login selection,
and a provider's own CLI for an AI session. Old routing/API-profile/proxy examples
remain upstream history rather than current setup instructions.

## Known limits

- Cursor usage is not available locally: it is server-side only, and reads as
  a fixed "no local usage log" entry in Included usage.
- Antigravity usage (T3 Code's Antigravity instances included) is read from
  its conversation databases on every computer, but only as far back as those
  databases keep it, and its model names are what Antigravity stored: a model
  with no listed rate (for example one known only by a numeric id) shows
  "Not logged" for cost, never a guessed price.
- Update apps Cancel granularity is one app per computer: each computer
  finishes the app it is on (installers are never killed) and skips the rest.
- The optional second Linux computer adds no quota rows by default and has no
  add, sign-in, key or switching control. It can provide a verified fallback reading from an existing supported
  sign-in or capsule. A missing sign-in, unknown identity or mismatched workspace
  remains unavailable; usage logs do not supply account credentials. Shared
  usage collection streams its helper bundle in memory and needs no manually
  copied helper files.
- Claude add, remove and restore read "coming" while the server's Claude host
  steps are off (the default); the default-profile
  refusal holds either way.
- Antigravity add and sign-in again run the supervised CLI login on the
  dashboard host, which needs Ubuntu there with the official `agy` CLI,
  bubblewrap and an unprivileged user namespace, plus a trusted connection
  for the pasted code; without them the dashboard shows the terminal command
  instead. Muse Code usage comes from the existing `muse login` on the Mac;
  dashboard sign-in for Muse Code is off by default and reads "coming"
  (Re-check is live).
- Cursor, Muse Code and Qwen hold one account each. The other providers take
  up to 16 accounts per provider.
