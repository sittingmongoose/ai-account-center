# Gap G14 — Analytics writers (Rust-owned)

Branch `rust/gap-G14`, stacked on `rust/d-analytics-owned` (base `1be9ae7dfc`), merged with
`origin/main` wave67 and `rust/e-agent-shipped`. Bundle: `landing/rust-gap-G14.bundle`
(base `1be9ae7dfc..rust/gap-G14`).

Closes the CUT-PLAN G14 row: quota-history sampling and activity collection move into the Rust
service so the Sunday cut can delete the TypeScript writers. Both store write paths are Rust-owned
and parity-proven against the TS behavior; the front door feeds the inputs and stands its own
writers down under a new group switch.

## What works

### Store ports (`crates/aac-store`), parity-proven on Windows

| Port | TS source | Parity |
| --- | --- | --- |
| `observe` | `usage-observations.ts`, `account-analytics-history.ts` | `tests/observe.rs` 5/5 — 46 windows, 24 accounts, 8 records, 12 texts, 7 timestamps |
| `report` | `store/activity-reports.ts` | `tests/report.rs` 3/3 — 11 passes, 5 rows reports, ingestor day/session trimming |
| `sampler` | `account-analytics-service.ts` (`sample`/`recordInStore`), `store/usage-backfill.ts` | `tests/sampler.rs` 1/1 — **end-to-end store equality**: merged order, backfill report, idempotent re-backfill, `historyRecords` byte-equal to the TS writer |
| `collect` | `analytics-remote-sources.ts` (`toWorkerResult`), `usage/activity-cells.ts`, `usage/account-activity-collector.ts` | `tests/collect.rs` 3/3 — 5 scan cases at the store's nano cost precision, provider aliases, session digests |

Regenerate the vectors with `bun tests/spec/analytics-writers/generate-{vectors,report-vectors,sampler-vectors,collect-vectors}.mjs`.
The sampler generator runs the **real TS writer against the in-process NAPI store** (the same Rust
Engine the port uses), so its `historyRecords` are the genuine TS answer, not a re-implementation.
Run the ports: `cargo test -p aac-store --no-default-features --test observe --test report --test sampler --test collect`.

- `observe`: the writer's whitelist and observation shapes (`analyticsWindow`, `storeWindow`,
  `storeObservationFromAccount/Record`, `decode`, `recordsFromText`, `providerFor`). Retention cutoff
  saturates so the live sampler's infinite retention keeps every reading.
- `sampler`: both-platform dedupe (available wins, then newer `sampledAt ?? fetchedAt`), 64-account
  cap, once-only hash-marked backfill (oldest-first 2000 batches, `backfill_report`), live record.
- `report`: `cellsFromHourly`, `pastCells`, `sessionsOf`, `reportState`, `buildActivityReports`,
  `rowsReport`, and the `ActivityIngestor` day/session digest trimming (`sameDays`). Shaped reports
  are the TS `UsageReport` JSON exactly; `contract_reports` deserializes them for ingest.
- `collect`: the scanner's compact rows → priced hour/session/route/model cells and session rows.
  `normalize_provider`, `session_key`/`session_hash`. Pricing is a **fed-facts seam** (`Pricing`
  trait): resolution stays with the front door, the arithmetic (logged cost wins, list rates,
  fallback part) matches the collector. Cost floats compare at the nano the store persists
  (`contract::nano`); sub-nano IEEE ordering noise is not consumer-visible.

### Service job (`crates/aac-http/src/analytics_writers.rs`)

`analytics_jobs` runs beside the store jobs when the store is hosted. Each tick, when the front door
feeds `analytics-writers: {enabled: true}`:

- **Quota sampler** reads the fed `analytics-dashboard` (`{dashboards:[mac,windows]}`), imports the
  JSON history once (archive walk depth ≤3 / ≤2000 files, `.gz` gunzip ≤64 MiB, plus
  `quota-history-v1.json`), and records on the account-refresh cadence (clamped 30s–1h).
- **Activity scheduler** reads the fed `analytics-plan` (computers/kinds/roots) and
  `analytics-pricing` (resolved facts), calls lane E's `scan-usage` verb per computer via a new
  in-process `Agents::scan_usage` (local + configured remotes), validates the v2 answer
  (reader name + kinds set), shapes reports and ingests with the trimmer on the converge cadence
  (10-min window, 60s converge interval). Source states are kept for the owned read.

The std mutex guard never crosses an await (the job future stays `Send`); a newer generation
supersedes a running pass. Cadence/validation/trim helpers carry unit tests.

### Front door wiring (`src/web-server`)

- `rust-service-groups.ts`: `analytics-writers` group + `rustAnalyticsWritersOn()` — requires the
  `analytics` read group on **and** the store hosted (`AAC_RUST_GROUP_ANALYTICS_WRITERS` wins over
  `rustService.groups['analytics-writers']`). Off by default.
- `analytics-writer-feeds.ts`: background producers push the control switch, coalesced dashboards,
  the activity plan and resolved pricing facts on the refresh cadence (content-keyed generations).
- `startAccountAnalyticsSampling(rustService)` stands the TS sampler **and** activity background
  down when the writers own collection; `index.ts` passes the service. With the switch off every TS
  path runs unchanged.
- `analytics-remote-sources.ts`: `analyticsWriterPlan()` builds the fed plan (overflow kinds stay
  with the collector); `toWorkerResult` exported for the parity generator.

`tsc --noEmit` clean.

## Gate

`rustService.groups['analytics-writers']: on` (or `AAC_RUST_GROUP_ANALYTICS_WRITERS=on`), with
`rustService.groups.analytics: on` and `rustService.store: on`. The TS writers stand down only when
all three hold, so nothing is left unwritten and nothing writes twice.

## Residuals (fed from TS until their own ports land — not silent narrowing)

- **Plan worker port**: root discovery (`extraRoots`/`experimentRoots`) and fingerprints are computed
  by TS (`analyticsWriterPlan`) and fed to the scheduler; the scanner discovers its own roots.
- **Pricing resolution**: the models.dev registry / CCS static / policy overrides stay TS-owned; the
  facts feed carries resolved rates. Unknown models use the fallback rate (as TS does).
- **Main history file strict reader**: the job reads `quota-history-v1.json` records via
  `recordsFromText`; the TS `FileAccountAnalyticsHistoryStore` validation is not yet ported.
- **Cursor account reads, large-codex reads, mirror election / freshness refinements, persisted
  snapshot**: still TS; the store answers reads now (lane D), so the snapshot is only for the TS
  in-memory path.
- **Owned-read live states**: the scheduler keeps source states (`source_states`); wiring them into
  lane D's owned-read envelope (replacing the TS-fed `live`) is the remaining read-side hookup.

## Verification

- Windows-native: the four port suites (12 tests) green; `tsc --noEmit` clean.
- Linux service build: `cargo check -p aac-http` on Nas1 under
  `flock /home/sittingmongoose/aac-nas1-heavy.lock` + `nice -n 19 -j 2` (aac-http is Unix-only).
- aac-files has 5 pre-existing Windows unit-test failures (Unix-centric, identical on the lane E
  base) — not regressions.

## Build / bundle

```
cargo test -p aac-store --no-default-features --test observe --test report --test sampler --test collect
bun run typecheck
git bundle create landing/rust-gap-G14.bundle 1be9ae7dfc..rust/gap-G14
```