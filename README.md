# AI Account Center

AI Account Center brings existing AI accounts, subscription usage, safe Codex
switching and Analytics into one dashboard, with native Mac and Windows bars.
The browser dashboard uses **Slint 1.18.1**; the backend remains TypeScript.

This is the continuing fork of [CCS](https://github.com/kaitranntt/ccs), now at
[sittingmongoose/ai-account-center](https://github.com/sittingmongoose/ai-account-center).
Repository history, original authorship, copyright and the [MIT license](LICENSE)
are preserved. Current product source is on
[`feat/activate-in-place`](https://github.com/sittingmongoose/ai-account-center/tree/feat/activate-in-place).
The installation below uses that reviewed product branch explicitly.

## Features

- **Accounts and usage:** Claude and Codex tables plus Cursor, Muse Code,
  Antigravity CLI, Kimi Code, Qwen token plan, Z.ai coding plan and OpenCode Go
  cards. Details retain supported reported quota windows, balances, overages, resets
  and separate expiration dates. Missing values stay unknown; stale samples
  remain labeled.
- **Claude controls:** open existing configured Claude profiles on Mac or
  Windows. Credentials remain with those profiles; Claude switching stays manual.
  Fable Max quota readings appear only when that window is explicitly reported;
  missing readings remain unavailable.
- **Codex controls:** identify and activate a saved login in the shared Ubuntu
  environment. Busy activation lists affected programs and requires an explicit,
  short-lived approval. The transaction preserves backup, verification and
  rollback. Automatic switching uses fresh provider usage and waits until idle.
- **Settings:** server-confirmed usage refresh interval (30–3600 seconds,
  initially 60 seconds) and Codex used-threshold
  controls. Refresh requests new usage before reloading the view. Thresholds
  retain their stored value until the server confirms a change.
- **Analytics:** a compact provider/account/source overview, with every usable
  account/window history shown by default. Select one history without combining
  unrelated quotas. Ubuntu Claude Code and Codex CLI logs provide daily/hourly
  input, output and cache-token charts, actual session/entry counts, separate
  estimated API-equivalent USD charts and model cost/share plots. Expand model
  details for token/cache columns. Activity spans accounts; estimates are not
  subscription charges or account billing attribution. Missing samples, cached
  observations and unfinished log tails remain explicit.
- **Native companions:** macOS menu bar and Windows tray apps show the same
  accounts, full usage details, configured Claude launch controls and guarded
  Codex activation/auto-switch settings. Analytics and app updates are in the
  browser dashboard.
- **Explicit app updates:** a dashboard button starts one allowlisted,
  asynchronous job for installed provider apps. Status reads do not start an
  update or restart a program; see the update details below.

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
| Mac menu bar | macOS 14+, Swift 5.9+, Xcode or Command Line Tools for source installation |
| Windows tray | Native .NET 8 WPF, Windows x64; source builds require an existing .NET 8 SDK |
| Shared Codex activation / auto-switch | Existing configured Ubuntu Codex runtime and saved logins |
| Usage collectors / app updates | Existing credentials and installed apps on the configured owner host; Python and approved SSH transport where needed |

Cross-platform source support does not make every account action available on
every host. The dashboard reports available capabilities. A container does not
provide another computer's provider sessions or native apps.

## Install from current source

There is no published npm package or registry image required for this installation.
Use the product branch explicitly:

```bash
git clone --branch feat/activate-in-place https://github.com/sittingmongoose/ai-account-center.git
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
session; native clients save their own private dashboard connection settings.

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
Mac/Windows collectors. Missing
credentials or collector setup yields unavailable usage. Follow the
[provider boundaries](docs/system-architecture/provider-flows.md),
[collector requirements](scripts/account-usage/README.md),
[OpenCode/Muse bridge](browser-bridge/opencode-muse/README.md) and
[Qwen bridge](browser-bridge/qwen/README.md) for the applicable existing source.
Do not copy account secrets into this repository or container images.

Claude desktop launch mappings use the private `claude-desktop-profiles.json`
configuration and the existing `platyr`, `gmail`, `party` and `me` profile IDs.
The optional `opencode-console-wallet-source.json` adds a distinct workspace
wallet source. These are existing deployment contracts, not generic account
creation forms; another computer needs its own valid sessions and configured
launch mappings. The other seven providers are usage-only in the dashboard.

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

Configure the dashboard URL and dashboard login in the native connection window.
Mac installs to `~/Applications/AI Account Center.app`; Windows installs to
`%LOCALAPPDATA%\AI Account Center\app\AIAccountCenter.exe`.
The [Mac guide](macos-bar/README.md) and [Windows guide](windows-bar/README.md)
describe private storage, startup preferences, compatibility aliases and rollback.
Windows source setup requires the [.NET 8 SDK](https://dotnet.microsoft.com/en-us/download/dotnet/8.0),
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
| `ai-account-center bar` | Mac local dashboard/app launch; `serve`, `stop`, `status`, `install`, `uninstall`, `version` are available |
| `ai-account-center help` / `version` | Current command help / product version |

`--config-dir PATH` selects the existing account configuration directory.
Use `ai-account-center help dashboard`, `help codex-auth` or `help bar` for options.
`ccs` runs the same retained commands; **`ccs config` remains the dashboard alias**.
The [Codex guide](docs/codex-auth.md) documents import/resource compatibility and
[activation review](docs/activate-in-place.md).

## Update and recovery

To update AI Account Center, keep a private configuration backup and the prior
local tarball/app, then update your clean product checkout on
`feat/activate-in-place`, install its locked dependencies and rerun the same
local installer:

```bash
git pull --ff-only
bun install --frozen-lockfile
./scripts/dev-install.sh --npm
```

Rebuild/reinstall native apps with their commands above. Rebuild a Docker
installation with its existing Compose command. Existing private account data,
native connections and startup preferences are retained; keep prior artifacts
and volumes for rollback. Local changes should be reviewed before pulling.
`ai-account-center update` is retired and cannot update from the upstream package.

The dashboard's **Update apps** action is different: it updates installed
Antigravity CLI, Muse Code, OMP, Codex CLI, Claude Code, Codex Desktop and Claude
Desktop using fixed supported methods. It may gracefully close/restart the
selected app family. Absent apps are skipped; failures and restart failures are
reported. Ubuntu runs locally; the current remote targets are the existing
`jared-mac` and `jared-windows` SSH aliases. Helper deployment and the existing
Windows interactive task are required for remote updates. This is not a generic
remote updater; see [app update setup and behavior](scripts/app-updates/README.md).
No update runs merely by opening the dashboard or reading its status.

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
Release verification is still in progress on this product branch. The latest
frontend and newly installed backend have been checked from Mac and Windows.
The running Mac bar's account Details, hover help, Qwen packs and Settings/Cancel
controls have been checked; these interaction checks do not establish pixel
layout on every display. Browser DPI fixtures need matching browser and context
scaling; see the [dashboard validation guide](web-dashboard/README.md#validation).
Antigravity account switching is not enabled in this checkpoint;
its disabled Ubuntu runtime, setup/rollback and update limits are documented in
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
