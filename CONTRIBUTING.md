# Contributing to AI Account Center

Product changes belong in
[sittingmongoose/ai-account-center](https://github.com/sittingmongoose/ai-account-center),
the continuing fork of [CCS](https://github.com/kaitranntt/ccs). Original
authorship, copyright, license and repository history are retained.

## Source setup

Use Node.js 22, matching CI, Bun, Rust 1.92 or newer with
`wasm32-unknown-unknown`, and wasm-pack. Slint/runtime compiler are pinned to
**1.18.1** in the dashboard manifest and lockfile.

```bash
git clone --branch feat/activate-in-place https://github.com/sittingmongoose/ai-account-center.git
cd ai-account-center
bun install --frozen-lockfile
bun run build
node dist/ccs.js dashboard
```

The installed command is `ai-account-center dashboard`; `ccs config` remains
compatible. Source builds do not require the upstream npm package.
`scripts/dev-install.sh` packages this checkout and installs its local tarball.
`dev:symlink`/`dev:unlink` temporarily replace and restore an existing local
`ai-account-center` command for development.

## Source map

| Area | Paths |
| --- | --- |
| CLI/account/backend | `src/` |
| Slint browser dashboard | `web-dashboard/` |
| Native bars | `macos-bar/`, `windows-bar/` |
| Usage collectors | `scripts/account-usage/`, `browser-bridge/` |
| Explicit app updates | `scripts/app-updates/` |
| Source-built container | `docker/Dockerfile`, `docker/compose.yaml` |
| Maintainer guides | [docs/README.md](docs/README.md) |

The root builder produces the dashboard at `dist/ui/`. There is no React/Vite
workspace to install or run. The backend remains TypeScript; native clients
retain their platform toolchains.

## Preserve account state

Use temporary `CCS_HOME` fixtures and `getCcsDir()` in tests. Do not read or change
a contributor's real private configuration, provider credentials or remote
computer. Keep fixtures sanitized and offline.

The rename preserves `~/.ccs/`, session aliases, profile/provider IDs, environment
settings and native messaging identities. Preserve authentication, origin/session
guards, target-bound switch reviews, idle-only auto-switch, truthful Analytics,
migrations and rollback.

Retired `ccsx`, `ccs-codex`, `ccsd`, `ccs-droid` and `ccsxp` bins give migration
guidance. Use provider CLIs directly for sessions and AI Account Center for
account management.

## Validation

```bash
bun run typecheck
bun run lint
bun run format:check
bun run test:fast
```

Dashboard changes also need the actual WASM build and its verification:

```bash
bun run build
bun run ui:validate
```

The UI gate verifies Slint pins, Rust formatting, offline browser helpers, bridge
syntax, source fingerprint and packaged WASM. `build:server` only compiles
TypeScript; it does not refresh dashboard assets.

For documentation:

```bash
bash tests/docs/quickstart-parity.sh
node tests/docs/documentation-freshness.js
```

Native/bridge guides describe their offline checks. Broaden tests when warranted
and state what was verified. Live refresh, app installation, remote writes and
deployment require separate authorization; fixtures do not prove live responses.

## Review and release

Use a focused task branch from `feat/activate-in-place`, conventional commit
messages and a PR against this fork. Describe behavior, validation and
compatibility effects. Coordinate overlapping files and preserve others' edits.

CI checks the TypeScript backend and pinned Slint inputs. Retired upstream
npm/container/Cloudflare lanes are not AI Account Center publication instructions.
Local packaging is supported; package, release or image publication requires
separate authorization. Do not restore upstream targets or add automation grants.

Report product issues at
[AI Account Center issues](https://github.com/sittingmongoose/ai-account-center/issues).
Keep suspected vulnerabilities and private data out of public issues.

## Attribution

Keep original package author **Tam Nhu Tran (Kai)**, the **CCS Contributors**
copyright and unchanged [MIT license](LICENSE), together with relevant third-party
credits and Slint's license/AboutSlint obligation. Authored historical issue
references retain their original identity; they do not define current commands.
