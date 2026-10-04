# Hardening Inventory Report

Scope: `src/**/*.{ts,tsx,js,jsx,mjs,cjs}`

## Summary

| Metric | Value |
|---|---:|
| Sync fs occurrences (all) | 1104 |
| Sync fs files affected (all) | 109 |
| Sync fs occurrences (runtime hotpaths) | 499 |
| Sync fs files affected (runtime hotpaths) | 62 |
| Legacy shim markers | 197 |
| Legacy shim files affected | 74 |

## Top Runtime Hotpath Sync fs Files

| File | Sync Calls | API Names |
|---|---:|---|
| `src/management/shared-manager/diverged-file-adopter.ts` | 29 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, openSync, readdirSync, readFileSync, readlinkSync, renameSync, statSync, unlinkSync, writeFileSync |
| `src/web-server/services/account-analytics-activity.ts` | 28 | chmodSync, existsSync, mkdirSync, readFileSync, realpathSync, renameSync, statSync, writeFileSync |
| `src/management/shared-manager/migrations.ts` | 25 | copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, readdirSync, symlinkSync, unlinkSync, writeFileSync |
| `src/web-server/usage/account-activity-collector.ts` | 23 | chmodSync, closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, renameSync, statSync, writeFileSync |
| `src/commands/bar/install-subcommand.ts` | 18 | cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync |
| `src/management/shared-manager/plugin-layout-internals.ts` | 18 | copyFileSync, existsSync, lstatSync, mkdirSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync |
| `src/cliproxy/accounts/registry.ts` | 17 | existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync |
| `src/management/shared-manager/shared-dir-linker.ts` | 16 | copyFileSync, existsSync, lstatSync, mkdirSync, readlinkSync, rmSync, symlinkSync, unlinkSync, writeFileSync |
| `src/web-server/services/account-refresh-settings.ts` | 15 | closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync |
| `src/web-server/services/app-update-service.ts` | 15 | chmodSync, closeSync, mkdirSync, openSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync |

## Top Legacy Shim Marker Files

| File | Marker Count |
|---|---:|
| `src/commands/bar/__tests__/native-app-paths.test.ts` | 24 |
| `src/auth/profile-detector.ts` | 18 |
| `src/utils/config-manager.ts` | 13 |
| `src/config/schemas/websearch.ts` | 10 |
| `src/commands/bar/native-app-paths.ts` | 8 |
| `src/auth/profile-registry.ts` | 5 |
| `src/codex-auth/commands/import-default-command.ts` | 5 |
| `src/commands/bar/launch-descriptor.ts` | 5 |
| `src/cliproxy/ai-providers/model-id-normalizer.ts` | 4 |
| `src/commands/bar/uninstall-subcommand.ts` | 4 |

## Explicit Shim/Re-export Files

- `src/bin/compat-cli.ts`
- `src/cliproxy/__tests__/model-catalog-compat.test.ts`
- `src/cliproxy/types/__tests__/types-backward-compat.test.ts`
- `src/utils/profile-compat.ts`
## Maintainability Metrics

| Metric | Value |
|---|---:|
| typed-error adoption (typed/total throws) | 22.8% (143/627) |
| typed-error adoption (P4 locked subdomains) | 100.0% (15/15), target 40% |
| hotpath console.error/warn occurrences | 70 (277 total, 207 CLI-UX exempt) |
| hotpath console.error/warn files | 17 |
| files with createLogger | 32/381 |
| subdomains with zero createLogger | 21 (antigravity, api, bin, channels, cliproxy, cliproxy/accounts, cliproxy/ai-providers, cliproxy/auth, cliproxy/binary, cliproxy/config, cliproxy/proxy, cliproxy/services, cliproxy/types, config, copilot, cursor, delegation, management, shared, targets, types) |
| files > 400 LOC | 44 |
| files > 600 LOC | 17 |

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
| `src/config/loader/normalizers.ts` | 1 |
| `src/errors/cleanup-registry.ts` | 1 |

### Files > 400 LOC (top 15)

| File | LOC |
|---|---:|
| `src/web-server/model-pricing.ts` | 1322 |
| `src/antigravity/registry.ts` | 1083 |
| `src/codex-auth/codex-activation-runtime.ts` | 1052 |
| `src/web-server/services/account-analytics-activity.ts` | 1018 |
| `src/cliproxy/quota/quota-fetcher-codex.ts` | 960 |
| `src/web-server/usage/account-activity-collector.ts` | 903 |
| `src/cliproxy/model-catalog.ts` | 895 |
| `src/cliproxy/accounts/registry.ts` | 871 |
| `src/web-server/usage/aggregator.ts` | 782 |
| `src/web-server/usage/native-quota-collector.ts` | 768 |
| `src/auth/profile-detector.ts` | 767 |
| `src/web-server/services/account-analytics-projection.ts` | 682 |
| `src/web-server/services/account-dashboard-service.ts` | 674 |
| `src/web-server/services/additional-account-service.ts` | 636 |
| `src/cliproxy/services/usage-compatibility-transformer.ts` | 632 |

