# AI Account Center Agent Guide

This is the continuing fork of [CCS](https://github.com/kaitranntt/ccs), renamed to
[AI Account Center](https://github.com/sittingmongoose/ai-account-center).
Preserve history, original author metadata, copyright notices and LICENSE.

## Product scope

The product owns account usage and limits, reset claims, sign-in health,
dashboard authentication, existing profiles (add, remove, sign in again),
Analytics, explicit app-update jobs and native Mac/Windows bars. It does not
switch accounts: account switching (Codex activation and auto-switch,
Antigravity activation and auto-switch) was removed on 2026-11-14, because the
agents pick their own accounts. Rust owns the public dashboard listener, CLI, collection,
Status, history Store and Analytics. The installed dashboard starts its native owners directly.
The TypeScript server (`src/`) was deleted on 2026-11-27; build tooling and some tests retain Node.
The web UI is Leptos **0.9** compiled to WebAssembly. Native bars remain Swift and WPF.

The primary command is `ai-account-center dashboard`; `ccs config` is compatible.
Retired `ccsx`, `ccs-codex`, `ccsd`, `ccs-droid` and `ccsxp` bins give migration
guidance. Do not restore the removed runtime dispatcher, React/Vite portal,
or API routing product.

## Working rules

- Source, tests, package scripts and workflow inputs define implemented behavior.
  Current guides are [README.md](README.md), [CONTRIBUTING.md](CONTRIBUTING.md)
  and [docs/README.md](docs/README.md).
- Work on a task branch, change assigned files and preserve other agents' edits.
- Never test against real `~/.ccs/` or `~/.claude/` data. Use `getCcsDir()` with
  a temporary `CCS_HOME`; never repurpose HOME, except for the browser-check kit's
  child server, whose collectors read the real home. Keep credentials out of logs,
  fixtures and screenshots.
- Keep private storage paths, profile/provider IDs, session aliases, native
  messaging identities and existing `CCS_*` settings compatible.
- Preserve authentication/origin/session checks, removal and reset-claim
  approvals, provider identity guards, migrations and rollback.
- Failed providers remain unavailable or honestly cached. Never invent zero
  usage, reset times, quota denominators or account attribution.
- Provider refresh, software installation, remote writes, publication, new
  credentials and deployment require explicit authorization.
- Keep terminal output ASCII and honor `NO_COLOR`/TTY behavior.

## Source map

| Area | Inputs |
| --- | --- |
| Dashboard listener, CLI and native owners | `crates/`, `config/` |
| Dashboard | `web-dashboard/leptos/`, `web-dashboard/public/` |
| Build/package verification | `scripts/build-ui.js`, `scripts/validate-ui.js`, `scripts/verify-bundle.js` |
| Native clients | `macos-bar/`, `windows-bar/` |
| Collectors and updates | `crates/aac-agent/`, `crates/aac-http/`, `crates/aac-providers/` |
| Container | `docker/Dockerfile`, `docker/compose.yaml` |

`dist/` and `dist/ui/` are generated. Keep the Leptos and wasm-bindgen manifest/lock pins
and the third-party credits (public/assets/THIRD-PARTY-NOTICES.txt). Internal generated
module filenames may retain CCS names.

## Validation

Use checks relevant to the change, without live provider refresh:

```bash
bun run test:fast
node --test web-dashboard/tests/*.test.mjs
cargo test -p <touched crate>
```

Authorized dashboard changes need `bun run build` and `bun run ui:validate` to
verify the actual Leptos runtime/source fingerprint. Native/bridge guides describe
their offline checks. Report precisely what ran and any limits.

## Release and documentation

A fix is done only when it is merged into `main` and pushed. Order: commit,
merge into `main`, push, build, install, restart. Never install a branch build:
the next install from `main` removes it. The package `postinstall` refuses a
build that is not on `origin/main`, has uncommitted changes, or lacks the
commit the dashboard runs (`deployed-build.json`); merge instead of setting
the override. See CONTRIBUTING.md, Deployment lineage.

The repository URL is `sittingmongoose/ai-account-center`. Do not publish to
upstream npm packages, GHCR images, Cloudflare routes, webhooks or project boards.
Local source packaging is the development path; external publication requires
separate authorization.

Keep current guides focused on account management. Label upstream feature history
and third-party credits as attribution. Update affected help, tests and migration
notes alongside behavior changes.

<!-- browser-check: start -->
## Browser checks

Run `browser-check run web-dashboard/tests/browser/checks` from a checkout with `dist/` built. Kit facts and helpers: [web-dashboard/tests/browser/README.md](web-dashboard/tests/browser/README.md). Tool rules: `browser-check guide`. Free browser and recording slots on all machines: `browser-check slots`.
<!-- browser-check: end -->
