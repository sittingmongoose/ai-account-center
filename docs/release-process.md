# AI Account Center Packaging and Automation

The supported build packages this repository's own source. It does not install
or publish the original upstream CCS npm package or image. The product keeps
repository history and [CHANGELOG.md](../CHANGELOG.md), but the original
development/stable npm, Docker promotion, Cloudflare and notification workflows
are retired.

## Local outputs

- `bun run build:all` compiles TypeScript, builds the locked Slint 1.18.1 WASM
  dashboard and verifies `dist/ui/`.
- [dev-install.sh](../scripts/dev-install.sh) builds and validates this checkout,
  creates a local package tarball and installs it on explicit invocation. It
  keeps the tarball for local reinstall/rollback.
- [dev-symlink.sh](../scripts/dev-symlink.sh) temporarily points an existing
  command at the local build and retains/restores its backup.
- [Docker](../docker/README.md) builds the local dashboard-only image.
- [Mac packaging workflow](../.github/workflows/bar-release.yml) runs manually
  against the repository's current default branch and uploads a source-built
  app artifact. It does not write a GitHub release. The lane is restricted to
  the default branch; use the local Mac build if that branch does not yet contain
  the reviewed product source.

## Validation and publication boundary

[CI](../.github/workflows/ci.yml) and
[Push CI](../.github/workflows/push-ci.yml) validate current product source and
pinned Slint inputs. Trusted-PR checks and read-only token restrictions remain;
build/unit jobs use GitHub-hosted Ubuntu runners. Linux runs source/unit checks,
while dashboard browser verification is scoped to Mac and Windows.
Cache inputs are root `bun.lock` and `web-dashboard/Cargo.lock`.

There is no active semantic-release or npm publication lane. The compatibility
[dev-release.sh](../scripts/dev-release.sh) entry prints retirement guidance and
performs no release mutation. Publishing packages, registry images or external
releases requires separate review/authorization and an actual product target.

## Migration and recovery

Keep existing private `~/.ccs/` state and session aliases. Record the prior local
package/image/app and private configuration backup before changing deployment.
Use the preserved artifact/state for rollback; do not rewrite history or delete
volumes to reverse a program upgrade. Native installers retain their platform
migration safeguards.

Original authored tags, changelog entries and upstream credits remain historical
records. Do not reassign them to the renamed product or restore their old publish
targets as current tooling.
