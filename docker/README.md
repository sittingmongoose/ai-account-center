# AI Account Center in Docker

The canonical image builds this checkout's TypeScript backend and pinned
Slint 1.18.1 dashboard. It starts the dashboard only, with no upstream npm
installation, AI runtime installation or CLIProxy process.

These instructions use the reviewed `feat/activate-in-place` product source
branch explicitly:

```bash
git clone --branch feat/activate-in-place https://github.com/sittingmongoose/ai-account-center.git
cd ai-account-center
```

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

Configure authentication inside the container that owns its state:

```bash
docker compose -f docker/compose.yaml exec ccs ai-account-center dashboard auth setup
```

Running that command on the outer host configures the host's private directory,
not the container volume. Dashboard cookie/session/origin checks also apply
inside Docker.

## Existing host accounts

The image includes Python and an SSH client for existing configured usage
collectors. It does not import host credentials, logins, SSH keys, browser
sessions or desktop applications. A new container cannot see those sessions
by itself.

If account sources already use approved SSH aliases, supply the corresponding
existing private configuration and necessary narrowly scoped mounts through
your own Compose override. Review mount permissions and the container user
before connecting private files. Do not mount a whole host home directory,
add credential grants or assume every provider is supported by the image.

Claude/Codex activity analytics refers to the configured Ubuntu source logs;
container activity is not a substitute for those host logs. Native bars connect
to the authenticated dashboard endpoint independently.

## Operations

```bash
docker compose -f docker/compose.yaml ps
docker compose -f docker/compose.yaml logs --follow ccs
docker compose -f docker/compose.yaml stop
docker compose -f docker/compose.yaml up -d --build
```

Health checks request the actual dashboard entry at `/` on port 3000. They do
not probe port 8317 or retired health APIs. Use the dashboard's authenticated
account views to assess provider availability; an HTTP healthcheck does not
prove live provider collection.

The default image tag is `ai-account-center:local`. It is built from source;
these instructions do not claim a published registry image. `CCS_IMAGE` may
select a separately reviewed local tag.

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
4. Build the current source image and confirm authentication, account identity,
   cached/unknown usage and guarded switching before replacing the prior setup.

The legacy `docker-compose.yml` and `docker-compose.integrated.yml` filenames
remain compatibility entry points to the same source-built dashboard. Former
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
and upstream history are retained. Slint's licensing/attribution details are in
[the dashboard guide](../web-dashboard/README.md#dependencies-and-attribution).
