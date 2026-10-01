# AI Account Center test guide

Root TypeScript and JavaScript suites use Bun's test runner. Tests cover the
account dashboard, saved Codex login controls, native bar boundaries, analytics,
application-update contracts, authentication, and package compatibility.

## Running tests

Run commands from the repository root with the root dependencies already
available. The authoritative commands are in [package.json](../package.json).

```bash
bun run test:fast
bun run test:slow
bun run test:all
bun run test:unit
bun run test:npm
```

[test bucket selection](../scripts/run-test-bucket.js) discovers suites under
`tests/unit`, `tests/integration`, `tests/npm`, and `src`. The slow bucket includes
package lifecycle tests, subprocess tests, local-server tests, and suites that
read built output. `test:all` runs the root buckets; `test:ci` invokes the same
command. CLI process boundaries are covered by the retained command and package
fixtures in those buckets.

Some package checks need current `dist` artifacts. `bun run test`
builds the TypeScript server and Slint dashboard before running `test:all`.
Direct `test:all` and `test:npm` invocations do not imply a fresh
build. See the [build guide](../CONTRIBUTING.md) for the required toolchain.

For source checks and the fast bucket:

```bash
bun run typecheck
bun run lint
bun run format:check
bun run validate
```

The full contributor sequence is defined by
[the CI-parity gate](../scripts/ci-parity-gate.sh) and invoked with
`bun run validate:ci-parity`. Use the smallest relevant suite first.

## Where coverage lives

| Area | Paths | Contracts |
| --- | --- | --- |
| Commands and package | [command tests](./unit/commands/), [package guide](./npm/README.md) | Product help/version, dashboard aliases, saved-login commands, migration guidance, private-state preservation, package metadata |
| Dashboard and controls | [web-server tests](./unit/web-server/), [integration tests](./integration/) | Account projections, guarded activation, automatic switching, refresh settings, update jobs, and local HTTP boundaries |
| Analytics | [account analytics tests](./unit/web-server/account-analytics-service.test.ts), [history tests](./unit/web-server/account-analytics-history.test.ts), [activity tests](./unit/web-server/account-activity-collector.test.ts) | Observed quotas, unknown values, identity separation, bounded history, and authentic local activity |
| Slint browser helpers | [dashboard tests](../web-dashboard/tests/) | Data fidelity, activation review, analytics views, and renderer startup helpers |
| Shared support | [fixtures and isolation helpers](./shared/), [mocks](./mocks/) | Synthetic state and bounded test doubles |
| Native bars | [Mac guide](../macos-bar/README.md), [Windows guide](../windows-bar/README.md) | Platform-specific build and verification instructions |

Focused source tests may also live beside their module in `src/**/__tests__/`.
Directory presence alone does not establish that an old upstream feature remains
part of the current product; use the retained command and API contracts.

## Security and private state

Keep fixture tests offline. Mock provider requests, SSH calls, process launch,
and update execution at the boundary under test. Never use an actual login,
real account switch, installed-app update, or contributor credential as a fixture.

[Bun's configuration](../bunfig.toml) preloads the
[test environment helper](./shared/fixtures/test-environment.js). That helper
aligns `CCS_HOME`, home-directory variables, and XDG paths with temporary storage.
Child processes must inherit the isolated environment. Clear or explicitly set
`CCS_DIR` when testing `CCS_HOME`; isolate `CODEX_HOME` where applicable, restore
environment overrides, and remove temporary directories after the case.

Use [getCcsDir()](../src/utils/config-manager.ts) for application storage paths.
Do not read or modify real `~/.ccs`, `~/.claude`, `~/.codex`, SSH keys, or desktop
credential stores. Preserve existing fixture bytes and permissions when testing
help, version, migration messages, or lifecycle scripts.

Retained security coverage includes
[session middleware](./unit/web-server/auth-middleware.test.ts),
[remote access checks](./unit/web-server/auth-check-remote-access.test.ts),
[remote write guards](./unit/web-server/api-routes-remote-write-guard.test.ts),
[bar access guards](./unit/web-server/api-routes-bar-local-access-guard.test.ts),
[activation review](./unit/web-server/codex-activation-routes.test.ts), and the
[current API surface](./unit/web-server/api-product-surface.test.ts).
See [SECURITY.md](../SECURITY.md) for reporting guidance.

## Slint validation

Browser helper tests run with Node's test runner:

```bash
node --test web-dashboard/tests/*.test.mjs
```

`bun run ui:build` compiles the pinned Rust/Slint dashboard into `dist/ui`.
`bun run ui:validate` checks Rust formatting, helper tests, bridge syntax, and
that the bundle matches the current source fingerprint. Helper tests do not
establish a successful browser render, provider request, or actual account switch.
See the [Slint guide](../web-dashboard/README.md),
[builder](../scripts/build-ui.js), and [validator](../scripts/validate-ui.js).
