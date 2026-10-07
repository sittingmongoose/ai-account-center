# AI Account Center Code Standards

Enforced source configuration and tests take precedence over prose. Use the
existing [ESLint](../eslint.config.mjs), [TypeScript](../tsconfig.json),
[package scripts](../package.json) and [commitlint](../commitlint.config.cjs)
contracts.

## Boundaries

Keep the backend in TypeScript, web presentation in Slint 1.18.1 and native bars
in Swift/WPF. Reuse existing account DTOs, browser helpers and guard services.
Do not introduce a runtime dispatcher or resurrect React/Vite to solve a view
change. Keep imports within the domain that owns the behavior.

Use narrow types at untrusted API/provider boundaries. Keep unknown values null
or unavailable instead of inventing zeros. Preserve genuine signed balances,
overages, reset times, separate expiration and sample provenance. Analytics
quota snapshots are not additive token totals.

## Private state and mutation

Resolve private paths through [config-manager](../src/utils/config-manager.ts).
Test with temporary `CCS_HOME`; preserve `~/.ccs/`, session/profile aliases and
native host identities. Keep tokens, cookies and raw provider payloads out of
logs, fixtures and client models.

Preserve authentication/origin/session checks, target-bound activation approvals,
idle-only auto-switch, identity-bound caches, private atomic writes, migrations
and rollback. Polling and startup must not launch app updates or mutate accounts.

### No personal details in the repository

The repository is public. Docs, comments, fixtures and examples use neutral
placeholders: profile names such as `work`, `personal`, `alt` or `plum`, hosts
such as `mac-host` and `windows-host`, emails such as `user@example.com`,
private addresses such as `192.168.10.x`, and `192.0.2.x` for documentation
addresses. Never use a real account, computer, email, home path or home-network
address.

[personal-detail-guard](../scripts/personal-detail-guard.js) enforces this. It
keeps only SHA-256 hashes of the blocked lowercased words and IPv4 /24 prefixes,
hashes every word and address prefix in each tracked text file (binary files and
`node_modules` are skipped) and fails on a match. It runs in the pre-push CI
parity gate and in CI (`bun run check:personal`, about 2 s). To block another
value, add its hash; never write the value itself. Product code that still
depends on a value is listed in the guard's allowlist with an exact count.

## Verification

Run focused offline tests and the relevant TypeScript gate. Dashboard changes
need the actual [Slint build](../scripts/build-ui.js) and
[UI validation](../scripts/validate-ui.js), including the source fingerprint.
Native/bridge changes use their platform checks. Do not silently substitute
fixtures for live provider evidence.

The release wasm build ends in a multi-GB link, so the builder runs only one
at a time per computer: it locks
`${XDG_CACHE_HOME:-~/.cache}/ai-account-center/ui-build.lock`, waits up to
`AAC_UI_BUILD_LOCK_WAIT_S` seconds (default 600) and then fails with "UI build
lane busy". Unless `CARGO_TARGET_DIR` is set, it compiles into the shared
`ai-account-center/cargo-target-wasm` folder in that cache, so a fresh worktree
reuses earlier work; the output still lands in the worktree's own `pkg` and
`dist/ui`.

The [contributor guide](../CONTRIBUTING.md) defines current commands. Document
what changed, why and what was verified. Packaging, provider refresh, remote
writes and publication are distinct actions requiring the appropriate scope.
Keep original authorship, LICENSE/copyright notices and Slint AboutSlint intact.
