# AI Account Center test guide

Root TypeScript and JavaScript suites use Bun's test runner; product code is Rust, with its tests in each crate. Tests cover the
account dashboard, saved Codex login controls, native bar boundaries, analytics,
application-update contracts, authentication, and package compatibility.

## Running tests

Run commands from the repository root with the root dependencies already
available. The authoritative commands are in [package.json](../package.json).

Run tests with `TMPDIR` on disk, not `/tmp` (which is a RAM tmpfs and fills quickly
with test homes and tool caches). Create a disk-backed temp dir and point `TMPDIR`
at it; the test fixture also sweeps stale `ccs-test-*` leftovers there.

```bash
mkdir -p ~/aac-tmp
TMPDIR=~/aac-tmp bun run test:fast
TMPDIR=~/aac-tmp bun run test:slow
TMPDIR=~/aac-tmp bun run test:all
TMPDIR=~/aac-tmp bun run test:unit
TMPDIR=~/aac-tmp bun run test:npm
```

[test bucket selection](../scripts/run-test-bucket.js) discovers suites under
`tests/unit`, `tests/integration` and `tests/npm`. The slow bucket includes
package lifecycle tests, subprocess tests, local-server tests, and suites that
read built output. `test:all` runs the root buckets; `test:ci` invokes the same
command. CLI process boundaries are covered by the retained command and package
fixtures in those buckets.

Some package checks need current `dist` artifacts. `bun run test`
builds the native binaries and Leptos dashboard before running `test:all`.
Direct `test:all` and `test:npm` invocations do not imply a fresh
build. See the [build guide](../CONTRIBUTING.md) for the required toolchain.

For source checks and the fast bucket:

```bash
bun run validate
```

The full contributor sequence is defined by
[the CI-parity gate](../scripts/ci-parity-gate.sh) and invoked with
`bun run validate:ci-parity`. Use the smallest relevant suite first.

## Where coverage lives

| Area | Paths | Contracts |
| --- | --- | --- |
| Commands and package | command tests, [package guide](./npm/README.md) | Product help/version, dashboard aliases, saved-login commands, migration guidance, private-state preservation, package metadata |
| Dashboard and controls | [aac-http tests](../crates/aac-http/tests/), [integration tests](./integration/) | Account projections, add, sign-in again and remove (the live login signs in again in place), refresh settings, update jobs, and local HTTP boundaries |
| Analytics | [native Analytics contract tests](../crates/aac-http/src/public_auth/tests/analytics.rs), history tests, activity tests | Observed quotas, unknown values, identity separation, bounded history, and authentic local activity |
| Dashboard data layer | [dashboard tests](../web-dashboard/tests/) | Data fidelity, account actions, analytics views, the frame's controllers and sign-in session |
| Shared support | [fixtures and isolation helpers](./shared/), mocks | Synthetic state and bounded test doubles |
| Native bars | [Mac guide](../macos-bar/README.md), [Windows guide](../windows-bar/README.md) | Platform-specific build and verification instructions |

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

Use getCcsDir() for application storage paths.
Do not read or modify real `~/.ccs`, `~/.claude`, `~/.codex`, SSH keys, or desktop
credential stores. Preserve existing fixture bytes and permissions when testing
help, version, migration messages, or lifecycle scripts.

Retained security coverage includes
session middleware,
remote access checks,
remote write guards,
bar access guards, and the
current API surface.
See [SECURITY.md](../SECURITY.md) for reporting guidance.

## Dashboard validation

Browser helper tests run with Node's test runner:

```bash
node --test web-dashboard/tests/*.test.mjs
```

`bun run ui:build` compiles the pinned Leptos dashboard into `dist/ui`.
`bun run ui:validate` checks Rust formatting, helper tests, and that the bundle
matches the current source and shell fingerprints. Helper tests do not
establish a successful browser render, provider request, or actual account switch.
See the [dashboard guide](../web-dashboard/README.md),
[builder](../scripts/build-ui.js), and [validator](../scripts/validate-ui.js).
