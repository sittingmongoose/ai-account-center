# Gap G16c: the Status feed's producers in Rust

**Branch** `rust/gap-G16c` · **base** G16b `170b49429` (origin/main `7f28c3940` + lane C `c796b5d79`).
Machine: AlexisGamingPC (Windows-native cargo and Bun); Linux checks on Nas1 under the heavy lock.
**No lane C file was edited** (`answer_signing.rs`, `validate.rs`, the readers, `config/private-files.json`).

## What moved

The TypeScript `StatusService` built every fed Status generation itself. The service now builds
them, under the existing owned Status group (`groups.status: on` + the store host), and the front
door stops its producers:

| Item | Where | What |
| --- | --- | --- |
| Facts | `crates/aac-status/src/produce/facts.rs` | `readFacts` + `localeRanks` + `dateWordsIn`: every string as a word (and its part after the first `.`), every ISO-shaped string's `Date.parse`, every safe-integer instant, the ASCII collation ranks and the display zone's clock/day words. The zone's UTC offsets cross as facts (the front door's `Intl`, `serverZoneFacts`); every word is written in Rust from them (`aac_num::intl::Zone`, the layer Home's fed path uses). |
| Browser-check | `produce/browsers.rs` | `parseBrowserSlots`, `mergeBrowserRows`, `sessionUrl`, `shortText` (the one packaged secret policy), phones as null/absent, notes and problems. |
| Simlock | `produce/simlock.rs` | `parseSimlockStatus`: capacity, leases (no ids/tokens), starting/failed refusals, disconnected workers and last-known leases. |
| Machines | `produce/machines.rs` | `parseMachineProbe`, `busyBetween`, `machineProbeCommand` (linux/mac/windows, the same bytes), `linuxCpuTimes`, `localSample` (injectable reads), the Mac memory probe. |
| Pinned threads | `produce/pinned_input.rs`, `produce/answered.rs` | The `composePinned` input the collector built: `reportV2` projection, `activeBlocker`/`resolvedCardWaits`, model labels from the packaged catalog, every `Date.parse` once. |
| Answers in the feed | `produce/answers_feed.rs`, `answers_owned.rs` `feed` | `mergeSharedAnswers` over the store's history and verified share copies; the share scan now feeds the Rust store (G16b item 4). |
| Six generations | `produce/sources.rs` | The exact source texts the service pushes (`status-reports|quota|pinned|collected|answers|facts`), `spendByStream` composed with the same composer, and the facts over all six. |
| Service producers | `crates/aac-http/src/status_produce.rs` | The 60 s collect (demand-gated, one pass at a time), the pinned re-read and composition (10 s, or on the request after a send), the shared-answer scan on each 10 s report pass, the six pushes with unchanged texts keeping their generation, and the 5 s `status-changed` hint. |
| Command runner | `crates/aac-http/src/status_run.rs` | One spawn without a shell, deadline, output cap, missing program named; the ssh isolation options and keepalives the front door's policy sets; the dashboard's environment allowlist. |
| Request | `Handler::Status` + `x-aac-owned`/`x-aac-now`/`x-aac-zone` | The service composes its own fed generations and answers with the existing `status_fed` composer, cache and ETag. |
| Usage | `PUT /internal/status/usage`, `produce/usage.rs` | The dashboard's analytics sample's built input (`{accounts, sampledAt, staleAfterSeconds}`) is held for the feed. The sample and the snapshot projection stay with the Analytics owner (G18); this gap consumes the input. |
| TypeScript | `status-service.ts`, `status-feed.ts`, `status-routes.ts` | In owned mode the 60 s collect, the local scans and the owned-pins push do not run; the service's `status-changed` events are rebroadcast to `/ws` (5 s). The first request the service cannot answer restarts this process's scanners and collectors (`handBackProducers`) and answers from them. Sends mark the service's answer history dirty. |

## Parity (fixtures and temporary roots only; no real mail, no provider calls, no real `~/.ccs`)

`tests/spec/status-produce-vectors.ts` records the real TypeScript answers for one fixture input:
the six `answerStatusFed` pushes, the browser rows/merges, the Simlock reading, the three host
probes and busy percents, the shared-answer merge, and the display zone's offsets. Then:

| Gate | Result |
| --- | --- |
| `crates/aac-status/tests/status_produce.rs` (Windows) | 5/5: parsers, merges, probes, busy, answers merge and the six sources' content (JSON equality; this crate's maps are key-sorted) |
| `crates/aac-http/tests/status_produce_bytes.rs` (Nas1) | the six source texts **byte for byte** against the front door's; facts' epochs/ranks exact, dates a subset (an instant outside the zone's proved window keeps no word here, while the reference's `Intl` writes one for any instant) |
| `tests/unit/web-server/status-feed.test.ts` | 8/8 (the owned attempt declines in the stand-in, the fed path answers) |
| `crates/aac-status` unit and integration tests | all but `status_doc.rs`'s `secrets_are_type_scripts_…`, which is lane C's `validate.rs` secret filter (G16 finding 3), not touched here |
| `bun run typecheck` | clean |

## What is left

1. **Lane C's key format and second secret filter** (G16 findings 1 and 3): the service's answer
   key, and `status_doc.rs`'s pinned test, stay C's. Not edited.
2. **The usage sample's producer** stays with the Analytics owner: the service consumes
   `recordUsage`'s built input. When the analytics sampler moves (lane I/D, G14/G15), its sample
   feeds `PUT /internal/status/usage` directly.
3. **The agent transport's `inventory`/`t3-pins` verbs** are not used for the remote reads yet;
   the collectors run the same read-only probes over ssh (the TypeScript fallback path), so the
   agent rollout (lane E) can replace the command runner without changing the output.
4. **Route/source hashes and the test inventory**: the new `PUT /internal/status/usage` row and
   the new test files need the landing train's `tests/spec/capture.ts --regenerate` and
   `tests/spec/inventory.mjs --regenerate` (G16b left the same for its row).
5. **Kill drill and the Status browser kit** on the merged branch once C's key fix lands.
