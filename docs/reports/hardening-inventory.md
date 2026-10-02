# Hardening Inventory Report

Scope: `src/**/*.{ts,tsx,js,jsx,mjs,cjs}`

## Summary

| Metric | Value |
|---|---:|
| Sync fs occurrences (all) | 1164 |
| Sync fs files affected (all) | 113 |
| Sync fs occurrences (runtime hotpaths) | 510 |
| Sync fs files affected (runtime hotpaths) | 64 |
| Legacy shim markers | 225 |
| Legacy shim files affected | 80 |

## Top Runtime Hotpath Sync fs Files

| File | Sync Calls | API Names |
|---|---:|---|
| `src/management/shared-manager/diverged-file-adopter.ts` | 29 | chmodSync, closeSync, fsyncSync, linkSync, lstatSync, openSync, readdirSync, readFileSync, readlinkSync, renameSync, statSync, unlinkSync, writeFileSync |
| `src/management/shared-manager/migrations.ts` | 25 | copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, readdirSync, symlinkSync, unlinkSync, writeFileSync |
| `src/management/recovery-manager.ts` | 23 | copyFileSync, existsSync, lstatSync, mkdirSync, renameSync, statSync, unlinkSync, writeFileSync |
| `src/auth/resume-lane-diagnostics.ts` | 19 | closeSync, fstatSync, lstatSync, openSync, readdirSync, realpathSync |
| `src/commands/bar/install-subcommand.ts` | 18 | cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync |
| `src/management/shared-manager/plugin-layout-internals.ts` | 18 | copyFileSync, existsSync, lstatSync, mkdirSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync |
| `src/cliproxy/accounts/registry.ts` | 17 | existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync |
| `src/management/shared-manager/shared-dir-linker.ts` | 16 | copyFileSync, existsSync, lstatSync, mkdirSync, readlinkSync, rmSync, symlinkSync, unlinkSync, writeFileSync |
| `src/management/instance-manager.ts` | 15 | existsSync, lstatSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync |
| `src/web-server/services/app-update-service.ts` | 15 | chmodSync, closeSync, mkdirSync, openSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync |

## Top Legacy Shim Marker Files

| File | Marker Count |
|---|---:|
| `src/commands/bar/__tests__/native-app-paths.test.ts` | 24 |
| `src/auth/profile-detector.ts` | 18 |
| `src/utils/config-manager.ts` | 13 |
| `src/web-server/usage/native-quota-collector.ts` | 13 |
| `src/config/schemas/websearch.ts` | 10 |
| `src/config/migration-manager.ts` | 9 |
| `src/commands/bar/native-app-paths.ts` | 8 |
| `src/auth/profile-registry.ts` | 5 |
| `src/cliproxy/quota/quota-fetcher-gemini-cli/auth-file-discovery.ts` | 5 |
| `src/codex-auth/commands/import-default-command.ts` | 5 |

## Explicit Shim/Re-export Files

- `src/bin/compat-cli.ts`
- `src/cliproxy/__tests__/model-catalog-compat.test.ts`
- `src/cliproxy/types/__tests__/types-backward-compat.test.ts`
- `src/utils/profile-compat.ts`
## Maintainability Metrics

| Metric | Value |
|---|---:|
| typed-error adoption (typed/total throws) | 29.9% (112/374) |
| typed-error adoption (P4 locked subdomains) | 88.2% (15/17), target 40% |
| hotpath console.error/warn occurrences | 83 (291 total, 208 CLI-UX exempt) |
| hotpath console.error/warn files | 20 |
| files with createLogger | 25/345 |
| subdomains with zero createLogger | 20 (antigravity, api, bin, channels, cliproxy, cliproxy/accounts, cliproxy/ai-providers, cliproxy/auth, cliproxy/binary, cliproxy/config, cliproxy/proxy, cliproxy/services, cliproxy/types, config, copilot, cursor, delegation, shared, targets, types) |
| files > 400 LOC | 34 |
| files > 600 LOC | 13 |

### Top Hotpath console.error/warn Files

| File | console.error/warn |
|---|---:|
| `src/errors/error-handler.ts` | 11 |
| `src/utils/prompt.ts` | 11 |
| `src/cliproxy/accounts/account-safety-cross-lane.ts` | 9 |
| `src/config/unified-config-loader.ts` | 7 |
| `src/utils/shell-executor.ts` | 7 |
| `src/bin/compat-cli.ts` | 5 |
| `src/targets/codex-detector.ts` | 5 |
| `src/utils/claude-detector.ts` | 4 |
| `src/utils/platform-commands.ts` | 4 |
| `src/auth/profile-continuity-inheritance.ts` | 3 |
| `src/auth/profile-detector.ts` | 3 |
| `src/ccs.ts` | 2 |
| `src/cliproxy/accounts/registry.ts` | 2 |
| `src/config/loader/config-getters.ts` | 2 |
| `src/config/migration-manager.ts` | 2 |

### Files > 400 LOC (top 15)

| File | LOC |
|---|---:|
| `src/web-server/usage/native-quota-collector.ts` | 1871 |
| `src/web-server/model-pricing.ts` | 1138 |
| `src/codex-auth/codex-activation-runtime.ts` | 1052 |
| `src/cliproxy/quota/quota-fetcher-codex.ts` | 960 |
| `src/cliproxy/quota/quota-manager.ts` | 954 |
| `src/cliproxy/model-catalog.ts` | 895 |
| `src/cliproxy/accounts/registry.ts` | 871 |
| `src/cliproxy/accounts/account-safety.ts` | 787 |
| `src/web-server/usage/aggregator.ts` | 782 |
| `src/auth/profile-detector.ts` | 767 |
| `src/config/migration-manager.ts` | 646 |
| `src/cliproxy/services/usage-compatibility-transformer.ts` | 632 |
| `src/cliproxy/services/stats-fetcher.ts` | 614 |
| `src/utils/hooks/image-analysis-backend-resolver.ts` | 598 |
| `src/web-server/usage/data-aggregator.ts` | 575 |

