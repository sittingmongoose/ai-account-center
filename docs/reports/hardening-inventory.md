# Hardening Inventory Report

Scope: `src/**/*.{ts,tsx,js,jsx,mjs,cjs}`

## Summary

| Metric | Value |
|---|---:|
| Sync fs occurrences (all) | 1083 |
| Sync fs files affected (all) | 107 |
| Sync fs occurrences (runtime hotpaths) | 486 |
| Sync fs files affected (runtime hotpaths) | 54 |
| Legacy shim markers | 184 |
| Legacy shim files affected | 56 |

## Top Runtime Hotpath Sync fs Files

| File | Sync Calls | API Names |
|---|---:|---|
| `src/web-server/usage/account-activity-collector.ts` | 123 | chmodSync, closeSync, copyFileSync, existsSync, mkdirSync, opendirSync, openSync, readFileSync, readSync, realpathSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/usage/analytics-request-plan.ts` | 28 | existsSync, realpathSync, statSync |
| `src/web-server/services/dashboard-preferences.ts` | 22 | closeSync, fstatSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readSync, realpathSync, renameSync, unlinkSync, writeFileSync |
| `src/web-server/services/usage-failover-cache.ts` | 19 | closeSync, fstatSync, lstatSync, mkdirSync, opendirSync, openSync, readSync, renameSync, unlinkSync, writeFileSync |
| `src/commands/bar/install-subcommand.ts` | 18 | cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync |
| `src/web-server/status/answer-signing.ts` | 18 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync |
| `src/web-server/status/writer-lock.ts` | 18 | closeSync, fsyncSync, mkdirSync, openSync, readFileSync, rmSync, statSync, unlinkSync, utimesSync, writeFileSync |
| `src/web-server/services/account-refresh-settings.ts` | 15 | closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync |
| `src/web-server/usage/t3-usage-roots.ts` | 14 | readdirSync, readFileSync, readlinkSync, realpathSync, statSync |
| `src/web-server/services/cursor-account-reader.ts` | 13 | mkdirSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync |

## Top Legacy Shim Marker Files

| File | Marker Count |
|---|---:|
| `src/web-server/rust-service.ts` | 29 |
| `src/commands/bar/__tests__/native-app-paths.test.ts` | 24 |
| `src/web-server/usage/account-activity-collector.ts` | 20 |
| `src/utils/config-manager.ts` | 13 |
| `src/commands/bar/native-app-paths.ts` | 8 |
| `src/codex-auth/commands/import-default-command.ts` | 5 |
| `src/web-server/routes/account-visibility-routes.ts` | 5 |
| `src/commands/bar/uninstall-subcommand.ts` | 4 |
| `src/config/schemas/auth.ts` | 4 |
| `src/web-server/services/account-visibility.ts` | 4 |

## Explicit Shim/Re-export Files

- _none_
## Maintainability Metrics

| Metric | Value |
|---|---:|
| typed-error adoption (typed/total throws) | 33.6% (240/714) |
| typed-error adoption (P4 locked subdomains) | 0.0% (0/0), target 40% |
| hotpath console.error/warn occurrences | 51 (221 total, 170 CLI-UX exempt) |
| hotpath console.error/warn files | 13 |
| files with createLogger | 39/438 |
| subdomains with zero createLogger | 8 (antigravity, config, management, shared, status-native, targets, types, utils) |
| files > 400 LOC | 66 |
| files > 600 LOC | 27 |

### Top Hotpath console.error/warn Files

| File | console.error/warn |
|---|---:|
| `src/errors/error-handler.ts` | 11 |
| `src/utils/prompt.ts` | 11 |
| `src/antigravity/registry.ts` | 5 |
| `src/config/unified-config-loader.ts` | 5 |
| `src/targets/codex-detector.ts` | 5 |
| `src/utils/platform-commands.ts` | 4 |
| `src/ccs.ts` | 2 |
| `src/utils/helpers.ts` | 2 |
| `src/web-server/status/readers-check.ts` | 2 |
| `src/antigravity/terminal-signin-entry.ts` | 1 |
| `src/errors/cleanup-registry.ts` | 1 |
| `src/web-server/middleware/auth-middleware.ts` | 1 |
| `src/web-server/services/ssh-policy.ts` | 1 |

### Files > 400 LOC (top 15)

| File | LOC |
|---|---:|
| `src/web-server/usage/account-activity-collector.ts` | 4030 |
| `src/web-server/services/analytics-remote-sources.ts` | 2429 |
| `src/web-server/status/answers.ts` | 2069 |
| `src/web-server/services/analytics-remote-transport.ts` | 1791 |
| `src/codex-auth/codex-activation-runtime.ts` | 1337 |
| `src/web-server/services/usage-failover-service.ts` | 1244 |
| `src/antigravity/registry.ts` | 1206 |
| `src/web-server/status/status-collectors.ts` | 1203 |
| `src/web-server/services/account-dashboard-service.ts` | 1174 |
| `src/web-server/rust-service-transport.ts` | 1124 |
| `src/web-server/rust-service.ts` | 1035 |
| `src/web-server/model-pricing-data.ts` | 1023 |
| `src/web-server/services/additional-account-service.ts` | 938 |
| `src/web-server/status/status-view.ts` | 903 |
| `src/web-server/usage/native-quota-collector.ts` | 815 |

