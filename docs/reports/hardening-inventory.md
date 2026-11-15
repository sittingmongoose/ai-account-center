# Hardening Inventory Report

Scope: `src/**/*.{ts,tsx,js,jsx,mjs,cjs}`

## Summary

| Metric | Value |
|---|---:|
| Sync fs occurrences (all) | 1112 |
| Sync fs files affected (all) | 110 |
| Sync fs occurrences (runtime hotpaths) | 538 |
| Sync fs files affected (runtime hotpaths) | 60 |
| Legacy shim markers | 201 |
| Legacy shim files affected | 60 |

## Top Runtime Hotpath Sync fs Files

| File | Sync Calls | API Names |
|---|---:|---|
| `src/web-server/usage/account-activity-collector.ts` | 123 | chmodSync, closeSync, copyFileSync, existsSync, mkdirSync, opendirSync, openSync, readFileSync, readSync, realpathSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/usage/analytics-request-plan.ts` | 28 | existsSync, realpathSync, statSync |
| `src/web-server/services/app-update-service.ts` | 23 | chmodSync, closeSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/services/dashboard-preferences.ts` | 21 | closeSync, fstatSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readSync, realpathSync, renameSync, unlinkSync, writeFileSync |
| `src/web-server/services/usage-failover-cache.ts` | 19 | closeSync, fstatSync, lstatSync, mkdirSync, opendirSync, openSync, readSync, renameSync, unlinkSync, writeFileSync |
| `src/commands/bar/install-subcommand.ts` | 18 | cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync |
| `src/web-server/status/answer-signing.ts` | 18 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync |
| `src/web-server/status/writer-lock.ts` | 18 | closeSync, fsyncSync, mkdirSync, openSync, readFileSync, rmSync, statSync, unlinkSync, utimesSync, writeFileSync |
| `src/web-server/rust-service-lifecycle.ts` | 15 | existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, renameSync, rmSync, unlinkSync, writeFileSync |
| `src/web-server/services/account-refresh-settings.ts` | 15 | closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync |

## Top Legacy Shim Marker Files

| File | Marker Count |
|---|---:|
| `src/commands/bar/__tests__/native-app-paths.test.ts` | 24 |
| `src/web-server/rust-service.ts` | 21 |
| `src/web-server/usage/account-activity-collector.ts` | 20 |
| `src/utils/config-manager.ts` | 13 |
| `src/web-server/services/app-update-service.ts` | 9 |
| `src/commands/bar/native-app-paths.ts` | 8 |
| `src/web-server/scoreboard/scoreboard-service.ts` | 7 |
| `src/web-server/services/account-analytics-activity.ts` | 7 |
| `src/codex-auth/commands/import-default-command.ts` | 5 |
| `src/commands/bar/uninstall-subcommand.ts` | 4 |

## Explicit Shim/Re-export Files

- `src/bin/compat-cli.ts`
- `src/utils/profile-compat.ts`
## Maintainability Metrics

| Metric | Value |
|---|---:|
| typed-error adoption (typed/total throws) | 32.3% (212/656) |
| typed-error adoption (P4 locked subdomains) | 0.0% (0/0), target 40% |
| hotpath console.error/warn occurrences | 54 (223 total, 169 CLI-UX exempt) |
| hotpath console.error/warn files | 12 |
| files with createLogger | 44/442 |
| subdomains with zero createLogger | 10 (antigravity, bin, config, cursor, management, shared, status-native, targets, types, utils) |
| files > 400 LOC | 61 |
| files > 600 LOC | 23 |

### Top Hotpath console.error/warn Files

| File | console.error/warn |
|---|---:|
| `src/errors/error-handler.ts` | 11 |
| `src/utils/prompt.ts` | 11 |
| `src/antigravity/registry.ts` | 5 |
| `src/bin/compat-cli.ts` | 5 |
| `src/config/unified-config-loader.ts` | 5 |
| `src/targets/codex-detector.ts` | 5 |
| `src/utils/platform-commands.ts` | 4 |
| `src/ccs.ts` | 2 |
| `src/utils/helpers.ts` | 2 |
| `src/web-server/status/readers-check.ts` | 2 |
| `src/errors/cleanup-registry.ts` | 1 |
| `src/web-server/middleware/auth-middleware.ts` | 1 |

### Files > 400 LOC (top 15)

| File | LOC |
|---|---:|
| `src/web-server/usage/account-activity-collector.ts` | 4028 |
| `src/web-server/services/account-analytics-activity.ts` | 2400 |
| `src/web-server/services/analytics-remote-sources.ts` | 2221 |
| `src/web-server/status/answers.ts` | 1744 |
| `src/web-server/services/analytics-remote-transport.ts` | 1634 |
| `src/codex-auth/codex-activation-runtime.ts` | 1337 |
| `src/antigravity/registry.ts` | 1104 |
| `src/web-server/services/usage-failover-service.ts` | 1053 |
| `src/web-server/model-pricing-data.ts` | 1023 |
| `src/web-server/status/status-collectors.ts` | 973 |
| `src/web-server/services/account-dashboard-service.ts` | 958 |
| `src/web-server/services/account-analytics-service.ts` | 887 |
| `src/web-server/status/status-view.ts` | 870 |
| `src/web-server/usage/native-quota-collector.ts` | 815 |
| `src/web-server/usage/codex-quota/quota-fetcher-codex.ts` | 799 |

