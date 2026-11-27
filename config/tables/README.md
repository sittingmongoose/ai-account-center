# config/tables — the canonical tables and their carried copies

One table per list, per language. TypeScript reads these files through
`src/shared/tables.ts`; Rust generates typed constants from them
(`crates/aac-providers/build.rs`, checked by `crates/aac-providers/tests/drift.rs`);
the drift tests in `tests/spec/tables.test.ts` compare every remaining hand list
(the page's JS, the Python helpers, the native bars and the carried TS lists)
against its projection of the table, so a one-sided edit of either side fails.

| Table | Owns |
| --- | --- |
| `providers.json` | Provider ids, labels, dashboard/card/key-store/quota flags, Analytics order, page registry rows, failover kinds, plan Retry-After policy |
| `tools.json` | Analytics tools, scanner-only kinds, usage-log tools and field mapping |
| `hosts.json` | The frozen legacy storage allowlist |
| `antigravity-columns.json` | Antigravity pool keys, services, full and short labels, plan-reason subsets (`thirdParty`, `weeklyOnly`) |

## Carried lists: owner and retirement criterion

A carried list is a hand literal kept because its runtime cannot read these
files (the browser page, the native bars) or because its module has not been
migrated yet. Each is pinned by `tests/spec/tables.test.ts` (and the bars' own
check programs); the pin is the drift alarm until the retirement criterion is
met. Words and their order belong to the table, never to the copy.

| Carried list | Owner | Retirement criterion |
| --- | --- | --- |
| `antigravity-data.mjs` `ANTIGRAVITY_LABELS` and plan-reason subsets | Dashboard page (Antigravity view) | The page build (`scripts/build-ui.js`) emits a table module the page imports; the literals are then generated, not carried |
| `view-model.mjs` `shortWindowLabel` map, `CARD_PROVIDERS`, `PROVIDER_REGISTRY`, zone constants | Dashboard page (view model) | Same page-build emission; the zone constants stay pinned against `crates/aac-http/src/zone_facts.rs` while both engines exist |
| `TrayModel.swift` `antigravityKeys`, `antigravityLabel` | Mac bar | The bar package script generates Swift from the table; until then `CCSBarCheck` and the drift test pin the CLI subset |
| `MainWindow.xaml.cs` `AntigravityKeys` | Windows bar | The bar build generates C# from the table; until then `Checks.cs` and the drift test pin the CLI subset |
| `plan_common.py` `PROVIDERS`, `KEY_PROVIDERS` | Qwen Windows native-messaging bridge | The bridge still ships this Python collector; the Rust agent owns dashboard quota reads |
| `account-key-store.ts` `KEY_PROVIDERS` | Key-store service | Derive from `providerIds(row => row.keyStore)` in the next touch of the module |
| `agent-usage-projection.ts` and `usage-backfill.ts` provider order | Agent-usage projection; store backfill | Derive from the table's Analytics order (codex and claude first) in the next touch of either module |
| Page provider lists (`account-actions.mjs`, `accounts-controller.mjs`, `analytics-usage.mjs`, `accounts-data.mjs`, `analytics-data.mjs`) | Dashboard page | Same page-build emission as the view model |

## Zone facts

`view-model.mjs` `homeZoneFacts` is the JS producer (the page's own `Intl`; the
server checks every Home word against it). `crates/aac-http/src/zone_facts.rs`
is the Rust producer for the native routes (chrono-tz IANA rules). The former
TS server copy (`src/web-server/views/home-zone.ts`) is deleted; its only
consumer (`tests/spec/bridge-measure.mjs`) reads the page producer. The window
constants (`HOME_ZONE_DAYS`, `ZONE_DAY`, `ZONE_STEP`) are pinned equal across
the two producers by the drift test; behavior parity is the Home spec corpus
(`crates/aac-views/tests/home_spec.rs`, `web-dashboard/tests/home-view-spec.test.mjs`).

## Reset predicate

`crates/aac-views/src/js.rs` `reading_predates_reset` is the one Rust port;
`src/web-server/services/account-window-reset.ts` `readingPredatesReset` is the
live TS reference. The agent-usage projection's variant is pinned equal to the
reference on every reset/sampled shape by
`tests/unit/web-server/account-window-reset.test.ts`.
