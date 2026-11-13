# Hardening Inventory Report

Scope: `src/**/*.{ts,tsx,js,jsx,mjs,cjs}`

## Summary

| Metric | Value |
|---|---:|
| Sync fs occurrences (all) | 1233 |
| Sync fs files affected (all) | 119 |
| Sync fs occurrences (runtime hotpaths) | 541 |
| Sync fs files affected (runtime hotpaths) | 60 |
| Legacy shim markers | 180 |
| Legacy shim files affected | 62 |

## Top Runtime Hotpath Sync fs Files

| File | Sync Calls | API Names |
|---|---:|---|
| `src/web-server/usage/account-activity-collector.ts` | 120 | chmodSync, closeSync, copyFileSync, existsSync, mkdirSync, opendirSync, openSync, readFileSync, readSync, realpathSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/usage/analytics-request-plan.ts` | 28 | existsSync, realpathSync, statSync |
| `src/web-server/services/app-update-service.ts` | 23 | chmodSync, closeSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/services/dashboard-preferences.ts` | 19 | closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, renameSync, unlinkSync, writeFileSync |
| `src/web-server/services/usage-failover-cache.ts` | 19 | closeSync, fstatSync, lstatSync, mkdirSync, opendirSync, openSync, readSync, renameSync, unlinkSync, writeFileSync |
| `src/commands/bar/install-subcommand.ts` | 18 | cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync |
| `src/web-server/status/answer-signing.ts` | 18 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync |
| `src/web-server/status/writer-lock.ts` | 18 | closeSync, fsyncSync, mkdirSync, openSync, readFileSync, rmSync, statSync, unlinkSync, utimesSync, writeFileSync |
| `src/cliproxy/accounts/registry.ts` | 17 | existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync |
| `src/web-server/services/account-refresh-settings.ts` | 15 | closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync |

## Top Legacy Shim Marker Files

| File | Marker Count |
|---|---:|
| `src/commands/bar/__tests__/native-app-paths.test.ts` | 24 |
| `src/web-server/usage/account-activity-collector.ts` | 20 |
| `src/utils/config-manager.ts` | 13 |
| `src/web-server/services/app-update-service.ts` | 9 |
| `src/commands/bar/native-app-paths.ts` | 8 |
| `src/web-server/services/account-analytics-activity.ts` | 7 |
| `src/codex-auth/commands/import-default-command.ts` | 5 |
| `src/cliproxy/ai-providers/model-id-normalizer.ts` | 4 |
| `src/commands/bar/uninstall-subcommand.ts` | 4 |
| `src/config/schemas/auth.ts` | 4 |

## Explicit Shim/Re-export Files

- `src/bin/compat-cli.ts`
- `src/cliproxy/__tests__/model-catalog-compat.test.ts`
- `src/cliproxy/types/__tests__/types-backward-compat.test.ts`
- `src/utils/profile-compat.ts`
## Maintainability Metrics

| Metric | Value |
|---|---:|
| typed-error adoption (typed/total throws) | 29.1% (212/729) |
| typed-error adoption (P4 locked subdomains) | 100.0% (3/3), target 40% |
| hotpath console.error/warn occurrences | 57 (226 total, 169 CLI-UX exempt) |
| hotpath console.error/warn files | 14 |
| files with createLogger | 45/472 |
| subdomains with zero createLogger | 20 (antigravity, auth, bin, cliproxy, cliproxy/accounts, cliproxy/ai-providers, cliproxy/auth, cliproxy/binary, cliproxy/config, cliproxy/proxy, cliproxy/services, cliproxy/types, config, cursor, management, shared, status-native, targets, types, utils) |
| files > 400 LOC | 66 |
| files > 600 LOC | 29 |

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
| `src/cliproxy/accounts/registry.ts` | 2 |
| `src/utils/helpers.ts` | 2 |
| `src/web-server/status/readers-check.ts` | 2 |
| `src/config/loader/normalizers.ts` | 1 |
| `src/errors/cleanup-registry.ts` | 1 |
| `src/web-server/middleware/auth-middleware.ts` | 1 |

### Files > 400 LOC (top 15)

| File | LOC |
|---|---:|
| `src/web-server/usage/account-activity-collector.ts` | 3922 |
| `src/web-server/services/account-analytics-activity.ts` | 2198 |
| `src/web-server/status/answers.ts` | 1744 |
| `src/codex-auth/codex-activation-runtime.ts` | 1340 |
| `src/web-server/services/analytics-remote-sources.ts` | 1156 |
| `src/antigravity/registry.ts` | 1104 |
| `src/web-server/services/account-analytics-service.ts` | 1028 |
| `src/web-server/model-pricing-data.ts` | 1023 |
| `src/web-server/services/usage-failover-service.ts` | 999 |
| `src/cliproxy/quota/quota-fetcher-codex.ts` | 990 |
| `src/web-server/status/status-collectors.ts` | 986 |
| `src/web-server/services/account-dashboard-service.ts` | 941 |
| `src/cliproxy/model-catalog.ts` | 895 |
| `src/cliproxy/accounts/registry.ts` | 871 |
| `src/web-server/status/status-view.ts` | 870 |

