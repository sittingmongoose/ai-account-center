# Hardening Inventory Report

Scope: `src/**/*.{ts,tsx,js,jsx,mjs,cjs}`

## Summary

| Metric | Value |
|---|---:|
| Sync fs occurrences (all) | 1399 |
| Sync fs files affected (all) | 140 |
| Sync fs occurrences (runtime hotpaths) | 740 |
| Sync fs files affected (runtime hotpaths) | 81 |
| Legacy shim markers | 239 |
| Legacy shim files affected | 79 |

## Top Runtime Hotpath Sync fs Files

| File | Sync Calls | API Names |
|---|---:|---|
| `src/web-server/usage/account-activity-collector.ts` | 120 | chmodSync, closeSync, copyFileSync, existsSync, mkdirSync, opendirSync, openSync, readFileSync, readSync, realpathSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync |
| `src/management/shared-manager/diverged-file-adopter.ts` | 29 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, openSync, readdirSync, readFileSync, readlinkSync, renameSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/usage/analytics-request-plan.ts` | 28 | existsSync, realpathSync, statSync |
| `src/management/shared-manager/migrations.ts` | 25 | copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, readdirSync, symlinkSync, unlinkSync, writeFileSync |
| `src/web-server/services/app-update-service.ts` | 20 | chmodSync, closeSync, mkdirSync, openSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/services/dashboard-preferences.ts` | 19 | closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, renameSync, unlinkSync, writeFileSync |
| `src/web-server/services/usage-failover-cache.ts` | 19 | closeSync, fstatSync, lstatSync, mkdirSync, opendirSync, openSync, readSync, renameSync, unlinkSync, writeFileSync |
| `src/commands/bar/install-subcommand.ts` | 18 | cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync |
| `src/management/shared-manager/plugin-layout-internals.ts` | 18 | copyFileSync, existsSync, lstatSync, mkdirSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync |
| `src/web-server/status/answer-signing.ts` | 18 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync |

## Top Legacy Shim Marker Files

| File | Marker Count |
|---|---:|
| `src/commands/bar/__tests__/native-app-paths.test.ts` | 24 |
| `src/web-server/usage/account-activity-collector.ts` | 20 |
| `src/auth/profile-detector.ts` | 18 |
| `src/utils/config-manager.ts` | 13 |
| `src/config/schemas/websearch.ts` | 10 |
| `src/commands/bar/native-app-paths.ts` | 8 |
| `src/web-server/services/app-update-service.ts` | 8 |
| `src/auth/profile-registry.ts` | 5 |
| `src/codex-auth/commands/import-default-command.ts` | 5 |
| `src/commands/bar/launch-descriptor.ts` | 5 |

## Explicit Shim/Re-export Files

- `src/bin/compat-cli.ts`
- `src/cliproxy/__tests__/model-catalog-compat.test.ts`
- `src/cliproxy/types/__tests__/types-backward-compat.test.ts`
- `src/utils/profile-compat.ts`
## Maintainability Metrics

| Metric | Value |
|---|---:|
| typed-error adoption (typed/total throws) | 29.4% (222/754) |
| typed-error adoption (P4 locked subdomains) | 100.0% (15/15), target 40% |
| hotpath console.error/warn occurrences | 72 (279 total, 207 CLI-UX exempt) |
| hotpath console.error/warn files | 18 |
| files with createLogger | 46/513 |
| subdomains with zero createLogger | 22 (antigravity, api, bin, channels, cliproxy, cliproxy/accounts, cliproxy/ai-providers, cliproxy/auth, cliproxy/binary, cliproxy/config, cliproxy/proxy, cliproxy/services, cliproxy/types, config, copilot, cursor, delegation, management, shared, status-native, targets, types) |
| files > 400 LOC | 70 |
| files > 600 LOC | 28 |

### Top Hotpath console.error/warn Files

| File | console.error/warn |
|---|---:|
| `src/errors/error-handler.ts` | 11 |
| `src/utils/prompt.ts` | 11 |
| `src/config/unified-config-loader.ts` | 7 |
| `src/utils/shell-executor.ts` | 7 |
| `src/antigravity/registry.ts` | 5 |
| `src/bin/compat-cli.ts` | 5 |
| `src/targets/codex-detector.ts` | 5 |
| `src/utils/platform-commands.ts` | 4 |
| `src/auth/profile-detector.ts` | 3 |
| `src/ccs.ts` | 2 |
| `src/cliproxy/accounts/registry.ts` | 2 |
| `src/config/loader/config-getters.ts` | 2 |
| `src/utils/helpers.ts` | 2 |
| `src/web-server/status/readers-check.ts` | 2 |
| `src/config/loader/normalizers.ts` | 1 |

### Files > 400 LOC (top 15)

| File | LOC |
|---|---:|
| `src/web-server/usage/account-activity-collector.ts` | 3906 |
| `src/web-server/services/account-analytics-activity.ts` | 1802 |
| `src/web-server/status/answers.ts` | 1744 |
| `src/codex-auth/codex-activation-runtime.ts` | 1340 |
| `src/antigravity/registry.ts` | 1104 |
| `src/web-server/model-pricing-data.ts` | 1023 |
| `src/web-server/services/usage-failover-service.ts` | 999 |
| `src/cliproxy/quota/quota-fetcher-codex.ts` | 990 |
| `src/web-server/status/status-collectors.ts` | 986 |
| `src/web-server/services/analytics-remote-sources.ts` | 958 |
| `src/web-server/services/account-analytics-service.ts` | 957 |
| `src/web-server/services/account-dashboard-service.ts` | 941 |
| `src/cliproxy/model-catalog.ts` | 895 |
| `src/cliproxy/accounts/registry.ts` | 871 |
| `src/web-server/status/status-view.ts` | 870 |

