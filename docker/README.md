# AI Account Center in Docker

The canonical image packages a gated Linux release tarball — the same verified
package installed on Linux hosts — into a minimal Debian runtime (`debian:trixie-slim`,
glibc 2.41, matching the `GLIBC_2.38`/`GLIBC_2.39` symbols required by the gated
Linux release binaries).
Because the gated tarball already contains the Linux Rust executables, pinned
Leptos 0.9 WebAssembly UI, all three cross-OS pinned host agents
(`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`),
usage-log scanners, Cursor account readers, and `dist/build-info.json`, the
container supports remote agent transport across Linux/Mac/Windows hosts and
keeps `deployed-build.json` current without compiling inside Docker. The runtime
ships no Node, Bun, or Python interpreter and starts no CLIProxy process.

These instructions use the `main` branch:

```bash
git clone --branch main https://github.com/sittingmongoose/ai-account-center.git
cd ai-account-center
```

<!-- quickstart-snippet-start -->
## Quick Start (Docker)

From a checkout of [AI Account Center](https://github.com/sittingmongoose/ai-account-center) with a gated Linux release tarball (`sittingmongoose-ai-account-center-8.10.0.tgz`):

```bash
AAC_RELEASE_DIR=/path/to/release/linux AAC_BUILD_COMMIT=<40-hex-release-sha> docker compose -f docker/compose.yaml up -d --build
```

Open `http://localhost:3000`. This packages the gated release tarball (Rust
dashboard, Leptos UI, pinned cross-OS agents, scanners, Cursor readers, and
`dist/build-info.json`) into a minimal Debian runtime with no Node, Bun, or
Python interpreter; it does not download the upstream CCS package or start a
CLIProxy service. `AAC_BUILD_COMMIT` must match the release tarball's
compiled commit and `dist/build-info.json`.
Existing host credentials and usage-collector connections require explicit
configuration; see the Docker deployment guide in this checkout.
<!-- quickstart-snippet-end -->

## Building and smoke-testing

To build directly with `docker build` (BuildKit named build context `release`):

```bash
docker build --pull \
  --build-context release=/path/to/release/linux \
  --build-arg AAC_BUILD_COMMIT=<40-hex-release-sha> \
  --build-arg AAC_RELEASE_SHA256=<optional-64-hex-tgz-sha256> \
  -f docker/Dockerfile \
  -t ai-account-center:local .
```

The unpack stage verifies `AAC_BUILD_COMMIT` against both
`dist/native/aac-http --build-commit` and `dist/build-info.json` (`dirty: false`,
`onMain: true`), checks the Leptos WASM digest, and verifies SHA-256 and byte
size for all three targets in `dist/agent/`, `dist/native/aac-cursor-account/`,
and `dist/native/aac-usage/` against `config/agent-artifacts.json` and their
manifests.

To run the isolated end-to-end smoke test (builds `aac-d1-smoke:local`, starts
an isolated container on an ephemeral loopback port, checks `/api/health`,
authenticated `/app/home`, `dist/build-info.json`, live `deployed-build.json`,
all three target artifacts inside the container, `stage-dashboard-package.js --verify-release-artifacts`,
and removes all test resources on exit):

```bash
bash docker/smoke-test.sh --release-tgz /path/to/release/linux/sittingmongoose-ai-account-center-8.10.0.tgz
```

## Configuration and storage

Compose publishes port 3000 on `127.0.0.1` by default. `CCS_DASHBOARD_PORT`
changes the host port; `CCS_DOCKER_BIND_HOST` changes the bind address. Keep
loopback binding unless the existing dashboard authentication and network
controls are configured for the intended access.

The service key `ccs`, network `ccs-net` and named volume key `ccs_home` are
retained for existing deployments. The configuration volume mounts at
`/home/node/.ccs`; private on-disk names remain CCS compatibility contracts.
The current process logs to Docker. Old volume declarations remain available
for migration/rollback and are not deleted by the application.

Authentication settings can be passed using the existing
`CCS_DASHBOARD_AUTH_ENABLED`, `CCS_DASHBOARD_USERNAME`,
`CCS_DASHBOARD_PASSWORD_HASH` and `CCS_SESSION_SECRET` names. Unset variables
stay unset so saved authentication is preserved. Keep these values out of
committed Compose files and public logs.

For a fresh container, supply its initial authentication through the private
environment overrides above. The Rust service uses the container's own CCS
state directory and enforces the same cookie, session and origin checks.

## Existing host accounts

The image includes an SSH client and all three pinned cross-OS `aac-agent`
artifacts for existing configured computers. It does not import host
credentials, logins, SSH keys, browser sessions or desktop applications. A new
container cannot see those sessions by itself.

The image sets `AAC_DASHBOARD_COMPUTER=none`: the container is none of the
registered computers, so it reads only its own CCS state in place and reaches
every computer in `computers.json`, the Ubuntu VM included (`ubuntu-vm`), over
SSH through the pinned agent. A dashboard installed on a registered computer
leaves it unset (the VM) or names that computer's id.

If account sources already use approved SSH aliases, supply the corresponding
existing private configuration and necessary narrowly scoped mounts through
your own Compose override. Mount SSH configuration at `/home/node/.ssh` and
review mount permissions and the container's UID 1000 before connecting
private files. Do not mount a whole host home directory, add credential grants
or assume every provider is supported by the image.

Update apps helper synchronization still invokes Python on each managed host
over its admitted transport. Python is a managed-host prerequisite, not a
container runtime dependency.

Claude/Codex activity analytics refers to the configured Ubuntu source logs;
container activity is not a substitute for those host logs. Native bars connect
to the authenticated dashboard endpoint independently.

## Operations

```bash
docker compose -f docker/compose.yaml ps
docker compose -f docker/compose.yaml logs --follow ccs
docker compose -f docker/compose.yaml stop
AAC_RELEASE_DIR=/path/to/release/linux AAC_BUILD_COMMIT=<40-hex-release-sha> docker compose -f docker/compose.yaml up -d --build
```

Health checks use `curl` against the public `/api/health` endpoint on port
3000. They do not probe port 8317 or retired health APIs. Use the dashboard's
authenticated account views to assess provider availability; an HTTP healthcheck
does not prove live provider collection.

The default image tag is `ai-account-center:local`. It is packaged locally from
a gated Linux release tarball; these instructions do not claim a published
registry image. `CCS_IMAGE` may select a separately reviewed local tag.

## Migrating an existing container

1. Save a private backup of the existing configuration and record the actual
   Compose project name, service and volume names. Keep the previous local
   image and files for rollback.
2. Stop the prior dashboard before starting the new one on port 3000. Keep the
   same Compose project name when reusing its `ccs_home` volume; changing the
   working directory or project name can select a different volume.
3. Review the existing volume's ownership and configured user. The new mounted
   path does not rename private account files. Permission changes are a separate
   deliberate migration step, not an automatic credential grant.
4. Build the release image and confirm authentication, account identity,
   `deployed-build.json`, pinned agents, and cached/unknown usage before
   replacing the prior setup.

The legacy `docker-compose.yml` and `docker-compose.integrated.yml` filenames
remain compatibility entry points to the same release-packaged dashboard. Former
service names may remain to preserve deployment identity; they do not start
CLIProxy. The old `ccs docker` runtime-deployment suite and host reconciliation
timers are retired. Do not enable those timers against the dashboard-only image.

Existing hosts may still have `ccs-cliproxy-reconcile.timer`,
`ccs-cliproxy-reconcile.service`, `ccs-cliproxy-update.timer` and
`ccs-cliproxy-update.service` installed independently of this checkout. Record
and review those identities before changing a deployment: their old helpers at
`/opt/cliproxy/ccs-cliproxy-reconcile.sh` and
`/opt/cliproxy/ccs-cliproxy-safe-update.sh` manage the previous CLIProxy stack,
including port 8317. They cannot update or reconcile the current dashboard-only
image. The current source omits that obsolete automation; it does not remove or
alter installed units, private configurations, images or volumes. Keep the
previous deployment checkout/revision and its original unit/helper files with
the recorded prior image and Compose configuration for a deliberate rollback.

For rollback, stop the new container and use the recorded prior image/Compose
configuration with the preserved volume and matching permissions. Avoid
`down --volumes`: it deletes configuration rather than rolling back a program.

## Attribution

This is the renamed continuing fork of
[kaitranntt/ccs](https://github.com/kaitranntt/ccs), originally authored by
Tam Nhu Tran (Kai) and the CCS Contributors. The unchanged [MIT license](../LICENSE)
and upstream history are retained. The dashboard's licensing/attribution details are in
[the dashboard guide](../web-dashboard/README.md#dependencies-and-attribution).
