# AI Account Center package tests

The `npm` directory contains package lifecycle, CLI, and metadata tests. They
run with Bun against local source and synthetic private state; running these
suites does not install a package or contact a provider.

## Running

Run from the repository root:

```bash
bun run test:npm
bun test ./tests/npm/cli.test.js
bun test ./tests/npm/postinstall.test.js
bun test ./tests/npm/cross-platform.test.js
```

The [package script](../../package.json) invokes `bun test tests/npm/`. The
[slow bucket](../../scripts/run-test-bucket.js) also includes this directory.
`cross-platform.test.js` checks that `dist/ccs.js` exists, so build artifacts must
already be available for the full package suite. `bun run test` builds first,
then runs the root buckets. The CLI fixture suite itself executes
[src/ccs.ts](../../src/ccs.ts) with Bun, rather than assuming a packaged executable
was rebuilt.

## Suites

| File | Current coverage |
| --- | --- |
| [cli.test.js](./cli.test.js) | Nine CLI cases: help, version, dashboard/config aliases, config-directory selection, invalid paths, and retired-command/bin migration behavior |
| [postinstall.test.js](./postinstall.test.js) | Direct execution of local install/uninstall lifecycle scripts, private-directory creation, idempotence, and preservation of existing files and permissions |
| [cross-platform.test.js](./cross-platform.test.js) | Path handling, Node subprocess availability, built executable presence, binary mappings, and the `prepack` contract |

## The nine CLI cases

| Case | Assertion |
| --- | --- |
| Root help | Empty invocation and help aliases show AI Account Center's retained commands; `NO_COLOR=1` produces help without ANSI escapes. |
| Version | Version aliases read the current package version and report the selected account configuration path. |
| Dashboard help | `dashboard`, compatible `config`, and their help forms return usage without starting the server. |
| Config-directory selection | A valid `--config-dir` takes precedence in reported configuration paths. |
| Invalid directory | A missing global config directory fails before command execution. |
| Retired commands | Old profile, provider, Docker, API, install/uninstall, and doctor invocations fail with product migration guidance. |
| Retired Codex shell commands | `codex-auth use` and `switch` emit no shell exports and point to `codex-auth activate <saved-login>`. |
| Five retired launcher bins | `ccsx`, `ccs-codex`, `ccsd`, `ccs-droid`, and `ccsxp` map to the migration-only stub; its invocation fails explicitly and writes no shell output. |
| Retired self-update | Old self-update forms return migration guidance without invoking package installation; installed-application updates remain a separate product control. |

Every CLI case checks that four synthetic files retain their exact bytes:
`.ccs/config.yaml` with dashboard authentication settings,
`.ccs/codex-profiles.yaml`, `.ccs/profiles.json`, and `.codex/auth.json`.
The supported `ai-account-center` bin and compatible `ccs` alias both map to
`dist/ccs.js`; the five retired launchers map to `dist/bin/compat-cli.js`.
See [the migration stub](../../src/bin/compat-cli.ts) and
[command routing tests](../unit/commands/root-command-router.test.ts).

## Lifecycle preservation

The lifecycle suite calls [postinstall.js](../../scripts/postinstall.js) and
[postuninstall.js](../../scripts/postuninstall.js) directly with a temporary
`CCS_HOME`. A fresh install creates only an empty private `.ccs` directory
(mode `0700` on Unix). An explicit `CCS_DIR` selects its own directory.

Repeated install and uninstall calls preserve existing configuration, saved
account files, Claude settings, old hooks, completion files, and migration
markers byte for byte, including their file modes. A file or dangling symlink at
the account-directory path fails without replacement or repair. Uninstall alone
creates or removes no user storage. These checks protect existing state during
migration; they do not install profiles or authorize accounts.

## Isolation and security

Keep these tests offline and use only synthetic data. Isolate `CCS_HOME` and
`CODEX_HOME`, clear or explicitly set inherited `CCS_DIR`, and restore temporary
state after each case. Never read or mutate a real account directory, Claude or
Codex login, SSH key, or desktop credential store.

[Bun's preload](../../bunfig.toml) uses the
[shared isolation helper](../shared/fixtures/test-environment.js). See the
[main test guide](../README.md), [storage path resolver](../../src/utils/config-manager.ts),
[dashboard authentication tests](../unit/web-server/auth-middleware.test.ts), and
[security reporting guide](../../SECURITY.md).
