# AI Account Center Agent Guide

This is the continuing fork of [CCS](https://github.com/kaitranntt/ccs), renamed to
[AI Account Center](https://github.com/sittingmongoose/ai-account-center).
Preserve history, original author metadata, copyright notices and LICENSE.

## Product scope

The product owns account usage, dashboard authentication, existing profiles,
guarded Codex activation/auto-switch, Analytics, explicit app-update jobs and
native Mac/Windows bars. The backend remains TypeScript; the web UI is Slint
**1.18.1** compiled to WebAssembly. Native bars remain Swift and WPF.

The primary command is `ai-account-center dashboard`; `ccs config` is compatible.
Retired `ccsx`, `ccs-codex`, `ccsd`, `ccs-droid` and `ccsxp` bins give migration
guidance. Do not restore the removed runtime dispatcher, React/Vite portal,
API routing product or introduce a Rust backend rewrite.

## Working rules

- Source, tests, package scripts and workflow inputs define implemented behavior.
  Current guides are [README.md](README.md), [CONTRIBUTING.md](CONTRIBUTING.md)
  and [docs/README.md](docs/README.md).
- Work on a task branch, change assigned files and preserve other agents' edits.
- Never test against real `~/.ccs/` or `~/.claude/` data. Use `getCcsDir()` with
  a temporary `CCS_HOME`; never repurpose HOME. Keep credentials out of logs,
  fixtures and screenshots.
- Keep private storage paths, profile/provider IDs, session aliases, native
  messaging identities and existing `CCS_*` settings compatible.
- Preserve authentication/origin/session checks, target-bound switch approvals,
  idle-only auto-switch, provider identity guards, migrations and rollback.
- Failed providers remain unavailable or honestly cached. Never invent zero
  usage, reset times, quota denominators or account attribution.
- Provider refresh, software installation, remote writes, publication, new
  credentials and deployment require explicit authorization.
- Keep terminal output ASCII and honor `NO_COLOR`/TTY behavior.

## Source map

| Area | Inputs |
| --- | --- |
| CLI/account/backend | `src/`, `lib/`, `config/` |
| Dashboard | `web-dashboard/src/`, `web-dashboard/ui/`, `web-dashboard/public/` |
| Build/package verification | `scripts/build-ui.js`, `scripts/validate-ui.js`, `scripts/verify-bundle.js` |
| Native clients | `macos-bar/`, `windows-bar/` |
| Collectors and updates | `scripts/account-usage/`, `scripts/app-updates/` |
| Browser bridges | `browser-bridge/` |
| Container | `docker/Dockerfile`, `docker/compose.yaml` |

`dist/` and `dist/ui/` are generated. Keep Slint's manifest/lock pins and standard
AboutSlint attribution. Internal generated module filenames may retain CCS names.

## Validation

Use checks relevant to the change, without live provider refresh:

```bash
bun run typecheck
bun run lint
bun run format:check
bun run test:fast
node --test web-dashboard/tests/*.test.mjs
```

Authorized dashboard changes need `bun run build` and `bun run ui:validate` to
verify the actual Slint runtime/source fingerprint. Native/bridge guides describe
their offline checks. Report precisely what ran and any limits.

## Release and documentation

The repository URL is `sittingmongoose/ai-account-center`. Do not publish to
upstream npm packages, GHCR images, Cloudflare routes, webhooks or project boards.
Local source packaging is the development path; external publication requires
separate authorization.

Keep current guides focused on account management. Label upstream feature history
and third-party credits as attribution. Update affected help, tests and migration
notes alongside behavior changes.
