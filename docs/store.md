# AAC store: design

Status: **phases 0 and 1 built** (usage readings; section 11 records what changed from the design and
what was measured); phases 2 to 5 are design. This document is the contract for the store. Code that
disagrees with it is a bug in one of the two; fix whichever is wrong and update this document in the same
change.

AAC keeps its history in one embedded store per dashboard machine: an append-only **event log** is the
record of what happened, and **redb** tables are projections of that log that answer queries. It follows
Puppet Master's storage plan (`Plans/storage-plan.md`: seglog, projectors, redb), so AAC is that plan's
first working trial. Section 2.7 lists what is mirrored and what is left out.

**Boundary.** The store is a storage library: a small Rust crate (`native/aac-store/`) loaded by the
TypeScript server as a native Node module (napi-rs). It appends, projects, queries and maintains, and
nothing else. Routes, authentication, collectors, provider parsing, switching, signing and every product
rule stay in TypeScript. The crate makes no network or provider calls. This is not a Rust backend
rewrite; AGENTS.md's rule still holds.

## 1. Scope

### 1.1 What moves in, in phase order

| Phase | Data | Today | In the store |
|---|---|---|---|
| 1 | Usage readings (every account window) | `account-analytics/quota-history-v1.json`: one JSON file, 30 days, thinned (last hour in full, 5-minute buckets to a day, hourly after), at most 24,000 records and 32 MiB, rewritten whole on every changed sample | every changed reading and a 5-minute "still seen" mark, 90 days; hourly and daily rollups and reset periods kept for good; long-period burn rates |
| 2 | quota-watch attribution | quota-watch's private `ledger.jsonl` (8 days), share `quota/report.json` (latest pass only) | per-session tokens by model and effort, estimated quota per session and meter, rollups by model and effort |
| 2 | Token activity hours | `cache/account-activity-v1/analytics-activity-snapshot-v1.json`: recomputed from the AI programs' logs, 31-day window | every hourly activity row kept after the program logs are gone |
| 3 | Agent-report runs | share `agent-reports/v1/writers/*/runs/<date>/*.json`, read on demand | every run, its reviews and its cost, indexed for the Model Scoreboard |
| 4 | Status answers log | `status/answers.jsonl` (append, branch `t3/ce359811`, not on `main` yet) | answer, send and delivery events with barrier durability |
| 5 | Switch history (new) | not kept: Codex `lastSwitchedAt` is in memory, Antigravity keeps only its latest switch, the rest is in `logs/current.jsonl` for 7 days | every automatic and manual switch with its reason and outcome |
| 5 | App-update job history | `app-updates/dashboard-job.json` holds only the current job | every finished job's summary |

There is no `quota-history` archive in the tree: the one file is thinned in place. Backfill (section 6)
imports that file.

### 1.2 What stays a file

| File(s) | Why |
|---|---|
| `auth/*`, `.session-secret`, `account-usage/keys/*`, `status/answer-key.pem`, `account-usage/*` helper capsules | secrets or credential material; they stay in their private 0600 files and never enter the log |
| `account-usage-accounts.json`, `account-visibility.json`, `account-refresh-settings.json`, `dashboard-preferences.json`, `codex-auto-switch.json`, `agent-usage-snapshot.json`, Claude profile stores | small configuration edited whole; the CLI and Python helpers read them; nothing to query |
| `bar.json`, `bar/*` | read by the Swift and WPF bars |
| `deployed-build.json` | read by the package `postinstall` gate |
| `app-updates/*` from the Python helpers, `codex-renewal-status.json`, `antigravity-switching/*` | live coordination state shared with Python or other processes; history from them is recorded as events (phase 5), the state files stay |
| `cache/*`, `account-usage-failover/*`, `claude-desktop-live-cache/*`, `models-dev-registry-cache.json`, `opencode-console-wallet-*.json` | caches with TTLs, rebuilt from their sources; losing one costs a refresh, not history |
| `logs/current.jsonl`, `logs/archive/*` | diagnostics, already append-only with rotation |
| share files (`aac-usage/*`, `agent-reports/*`) | the cross-machine interface; the store exports to them and imports from them (section 8) |

## 2. Event log

### 2.1 Layout

All under `<getCcsDir()>/store/` (folder 0700, files 0600; section 4).

```
store/
  store.lock                      OS lock held by the one writer process
  aac.redb                        redb projections (rebuildable from the logs)
  log/readings/                   high-volume log, segments expire (section 5)
    manifest.v1.msgpack
    seg-000001-00000000000000000001.active
    seg-000001-00000000000000000001-00000000000000007311.seglog.zst
  log/ledger/                     low-volume log, kept for good
    manifest.v1.msgpack
    seg-...
  intents/                        crash-idempotent maintenance intents
  backups/                        verified local backups (section 4.7)
  quarantine/                     exact bytes of anything invalid, with a manifest
  tmp/                            same-filesystem temporary files
```

Two logs, split by retention class, so expiry deletes whole segments and never rewrites one. Each log has
its own sequence space, manifest and projector checkpoints.

### 2.2 Record types

Every event has the envelope in 2.3. `durability`: `group` is acknowledged at the next group commit,
`barrier` forces an immediate commit (2.5). The idempotency key is hashed into the `idem` table (3.2); an
append whose key is already there returns the original sequence and writes nothing.

| `event_type` | Log | Durability | Idempotency key | Payload fields |
|---|---|---|---|---|
| `usage.sampled` | readings | group | the projector's per-account rule (11.1) | `observations[]`, one per changed account of a sampling tick: `account {identity, provider, account_id, plan, platform, source, status, is_active}`, `sampled_at_ms`, `observed_at_ms`, `windows[] {identity, key, label, kind, unit, used_pct, remaining_pct, used, limit, remaining, reset_at_ms, expires_at_ms, window_minutes, unlimited, enabled, cached_sampled_at_ms, pool_id, pool_id_source, pool_label, model_ids[]}`, `import_ref` (backfill only) |
| `usage.seen` | readings | group | the projector's per-account rule (11.1) | `accounts[]`, one per unchanged account of a sampling tick: `account_identity`, `sampled_at_ms`, `observed_at_ms`, `import_ref` |
| `usage.day_closed` | ledger | group | `day` | `day` (UTC), `series[] {account meta, window meta, hours[24] rollup, day rollup, periods[] touched}` |
| `activity.hour` | ledger | group | `tool:host:hour_ms:row_sha256` | `tool`, `host`, `provider`, `profile`, `hour_ms`, `models[] {model, provider, input, output, cache_create, cache_read, requests, cost_usd, fallback_cost_usd}`. Appended once per source and hour, 2 hours after the hour ends; a later correction appends a new row and the latest wins. The live current hour stays in the activity service's memory |
| `attribution.pass` | ledger | group | `writer:seq` | `writer`, `seq`, `start_ms`, `end_ms`, `generated_at_ms`, `method_id`, `rates_sha256`, `machines[] {name, scanned, error}` |
| `attribution.tokens` | readings | group | `writer:seq:session:model:effort` | `pass {writer, seq}`, the session row of 7.5 |
| `attribution.share` | readings | group | `writer:seq:account:meter` | `pass`, the share row of 7.5 |
| `attribution.day_closed` | ledger | group | `day` | `day`, rows `{account, meter, plan, model, effort, stream, tokens by class, list_usd, est_points, solo_points, solo_hours, sessions}` |
| `run.recorded` | ledger | group | `run_id:seq` | `doc` (the `agent-reports/run@1` document, canonical), `path` (relative to the share), `sha256`, `first_seen_ms`, `import_ref` |
| `answer.recorded` | ledger | barrier | `id` | today's `type: answer` record: `id`, `send_id`, `answer` (signed), `card` |
| `answer.sent` | ledger | barrier | `send_id` | `send_id`, `at_ms`, `device`, `entry_ids[]` |
| `answer.delivery` | ledger | group | `id:step:attempts` | `id`, `step` (`share`, `doorbell`, `summary`), `state`, `at_ms`, `detail`, `attempts` |
| `switch.recorded` | ledger | barrier | `provider:at_ms:to` | `provider`, `from_account`, `to_account`, `trigger` (`auto`, `manual`, `cli`), `reason`, `threshold_pct`, `outcome`, `approval_ref` (no secrets) |
| `update.job_finished` | ledger | group | `job_id` | `job_id`, `started_ms`, `finished_ms`, `state`, `hosts[]`, `results[] {app, host, outcome}` |
| `store.*` | ledger | barrier | deterministic | `store.opened`, `store.integrity_detected`, `store.segment_retired`, `store.backup_verified`, `store.import_completed`, `store.migrated`, `store.loss_accepted` |

No event holds a secret, token, cookie, email address, raw prompt or local credential path. Session ids are
stored only as their published key (7.5). The writer refuses a payload that matches the existing secret
patterns.

### 2.3 Frame

The frame is Puppet Master's `SeglogFrameV2` profile (Case L-2) with AAC's magic. All prefix integers are
unsigned little-endian; fields are read and written explicitly, never by casting a struct.

| Offset | Bytes | Field | Rule |
|---:|---:|---|---|
| 0 | 8 | `magic` | `41 41 43 53 45 47 32 00` (`AACSEG2\0`) |
| 8 | 1 | `frame_version` | `2` |
| 9 | 2 | `header_length` | at most 4,096 |
| 11 | 4 | `payload_length` | at most 1,048,576 (AAC cap; PM allows 16 MiB) |
| 15 | 1 | `flags` | `0`; frames are never compressed individually |
| 16 | 8 | `segment_generation` | equals the segment's generation |
| 24 | 8 | `sequence_id` | strictly greater than the previous frame's |
| 32 | 4 | `header_crc32` | CRC of the stored header bytes |
| 36 | 4 | `payload_crc32` | CRC of the stored payload bytes |
| 40 | 4 | `prefix_crc32` | CRC of the 48 prefix bytes with bytes 40 to 43 zeroed |
| 44 | 4 | `reserved` | `0` |

- **Checksum:** CRC-32/ISO-HDLC (polynomial `0x04c11db7`, reflected, init and xorout `0xffffffff`;
  `"123456789"` gives `0xcbf43926`), as PM. See L1 at the end for why this is not CRC32C.
- **Header:** one canonical MessagePack array of eight elements, PM's tuple:
  `[1, event_id, event_type, "aac.event.v1", schema_version, payload_schema_id, "application", null]`.
  `event_id` is a ULID string. `payload_schema_id` is `aac.<event_type>.v<major>`.
- **Payload:** one canonical MessagePack map (PM's canonical rules: string keys sorted by UTF-8 bytes,
  shortest encodings, no binary, extension or timestamp tags, no NaN):
  `{event_id, event_type, schema_id, schema_version, payload_schema_id, scope_kind, project_id,
  sequence_id, observed_at_ms, source_at_ms, idempotency_key, payload}`. Header values must equal the
  envelope's.
- **Validation order:** PM's ten steps: magic, prefix CRC before trusting lengths, version/flags/reserved,
  caps and file bounds, generation and sequence, header CRC and tuple, payload CRC, decode, envelope
  checks, duplicate identity.

### 2.4 Segments

| Rule | Value |
|---|---|
| Active name | `seg-{generation:06}-{start_seq:020}.active`, one per log |
| Sealed name | `seg-{generation:06}-{start_seq:020}-{end_seq:020}.seglog`, then `.seglog.zst` |
| Seal when | 8 MiB reached, a UTC day ends (so every sealed readings segment covers at most one day), clean shutdown, maintenance, or a frame or schema generation change |
| Seal steps | finish the pending group, write an intent, `sync_all`, rename `.active` to `.seglog`, sync the folder, open the next `.active`, publish the manifest, remove the intent |
| Compression | a background thread writes `.seglog.zst` (zstd level 19, single frame, content checksum on, window at most 8 MiB), decompresses it again and compares the SHA-256 of the raw bytes with the manifest; only then, under an intent, removes `.seglog`. The decoded bytes are the original frames, unchanged |
| Immutability | sealed segments are never edited; expiry deletes whole files (section 5) |
| Manifest | `manifest.v1.msgpack` per log: generation, segments `{name, start_seq, end_seq, start_ms, end_ms, raw_bytes, raw_sha256, zst_bytes, state: active, sealed, degraded}`, durable watermark `{segment, offset, seq}`, sequence lease high-water mark. Written to `tmp/`, fsynced, renamed, folder fsynced |

Readers stream `.zst` segments with bounded memory (one 8 MiB window).

### 2.5 Append, fsync and acknowledgement

| Step | Rule |
|---|---|
| Group commit | a group seals at the first of 10 ms, 64 records or 1 MiB (PM's numbers). A sample tick (one frame per changed account) is one or two groups |
| Durable | frames written, active segment `sync_all`, manifest watermark promoted, folder synced. Only then does the append resolve |
| Barrier | `barrier` events (answers, switches, `store.*`) seal the group at once and resolve only after the durable step |
| Sequence ids | leased in blocks of 4,096 and recorded in the manifest before use; ids above the lease are never issued; ids lost to a crash are never reused (a legal gap) |
| Failure | a failed write or fsync closes the writer: no event is held in memory as if saved. The caller gets an error (a sample is retried at the next tick; an answer returns `answer_not_saved`) |

### 2.6 Recovery

Start-up order: lock, manifest, logs, then redb, then projectors. Nothing writes before the logs are
recovered.

| Found | Action | Disclosed as |
|---|---|---|
| Invalid bytes after the durable watermark in an active segment (torn tail) | adopt every whole valid frame, truncate the rest under an intent, sync, publish the watermark | `clean`: these appends were never acknowledged |
| Bad frame at or below the watermark in an active segment | stop, seal the segment `degraded`, resynchronise only at a fully validated frame, record `store.integrity_detected` with the byte range and the bounded sequence range | a gap |
| Sealed segment whose SHA-256 or zstd checksum fails | never modify it; mark it `degraded`; restore that file from a backup with the same name and hash if one exists; otherwise salvage the valid frames on both sides of the hole and project those | a gap with exact or bounded range |
| Loss in the **ledger** log (answers, runs, switches) | as above, and Status answers become read-only until a backup restore or `ai-account-center dashboard store accept-loss` (later) (`store.loss_accepted`) | blocking banner |
| Loss in the **readings** log | as above; writing continues | a history gap (shown as no reading, never as zero) |
| Manifest missing or unreadable, segments present | rebuild the manifest by validating every frame; the watermark becomes the last valid frame | `store.integrity_detected` |
| `aac.redb` fails to open or `check_integrity()` fails | move it to `quarantine/`, create a new file, replay both logs from the start | rebuild in progress |
| Store newer than the code (`store_schema_version` or an unknown frame/header version) | open nothing, change nothing | `blocked_newer_store`: the dashboard runs without history |

### 2.7 Mirrored from Puppet Master, and left out

| PM part | AAC |
|---|---|
| `SeglogFrameV2` prefix, header tuple, canonical MessagePack, CRC order | mirrored (own magic, 1 MiB payload cap) |
| Segment names, deterministic rotation, immutable closed segments | mirrored, plus a UTC-day seal for the readings log |
| Group commit 10 ms / 64 / 1 MiB, barrier class, two-barrier acknowledgement, sequence lease of 4,096 | mirrored |
| Manifest with watermark; intents for every destructive step; folder fsync on create, rename and unlink | mirrored |
| Projector checkpoints `{segment_generation, segment_name, byte_offset, last_sequence_id, last_event_id, survivor_prefix_sha256, projector_schema_version}` committed with the projection | mirrored |
| Store version admission, `blocked_newer_store`, no downgrade in place | mirrored |
| Storage value registry | mirrored as `native/aac-store/registry.json`: one row per table and event type with key shape, value schema, version, producer, consumers, retention and authority class (all AAC tables are `derived_rebuildable`) |
| Uncompressed frames in the first profile | changed: whole sealed segments are compressed (frames inside unchanged) |
| One app-global seglog plus successor-generation compaction for retention | changed: two logs by retention class; expiry deletes whole sealed segments |
| Canonical non-rebuildable redb families, mandatory backup snapshots before mutation | left out: every AAC table is rebuildable from the logs |
| Tantivy, JSONL mirror, blob store, regex index | left out: no search need; the share export is the human-readable copy |
| Project scopes, legal holds, deletion lifecycle, fallback roots and divergence commands, the I/O class taxonomy | left out: one machine, one owner, one root under `getCcsDir()` |
| `AppendReceipt` custody (SP-286) | left out: acknowledgement is the durable append itself |

## 3. redb tables

### 3.1 Conventions

| Rule | Detail |
|---|---|
| Keys | fixed-width `[u8; N]`, fields concatenated big-endian, so byte order is numeric order and every prefix is a range scan |
| Field codes | `a` account u32, `w` window u32, `p` provider u32, `t` time ms u64, `h` hour index u32 (`ms / 3,600,000`), `d` UTC day index u32, `m` model u32, `e` effort u32 (0 = unknown), `s` session u32, `st` stream u32 (0 = none), `r` run u32, `tt` task type u32, `wr` writer u32, `c` card u32 |
| Strings | interned in `dict` per kind; ids are local to one redb file and are never written to the log or exported |
| Index values | `()`: index tables are keys only |
| Values | hot tables (`reading`, `latest`, rollups) use the fixed binary layouts below; other values are positional MessagePack arrays with a leading version byte |
| Transactions | each projector commits its rows and its checkpoint in one write transaction (at most 4,096 events or 1 s per commit) |

`Reading` value (v1): `u8 version`, `u8 status`, `u8 flags` (active, cached window, unlimited, enabled
known, enabled), `u16 present mask`, `u64 last_seen_ms`, then only the present fields: `f32 used_pct`,
`f32 remaining_pct`, `f64 used`, `f64 limit`, `f64 remaining`, `u64 reset_at_ms`, `u64 expires_at_ms`,
`u32 window_minutes`, `u32 source`, `u8 platform`, `u64 seq`. A row covers `[t_first, last_seen_ms]` with
the same values: a repeated reading moves `last_seen_ms`, a change starts a new row.

`Rollup` value (v1): `f32 consumed_pts`, `f64 credits_spent`, `f64 credits_added`, `f32 first_used`,
`f32 last_used`, `f32 min_used`, `f32 max_used`, `u16 covered_min`, `u16 rows`, `u8 resets`,
`u64 last_reset_ms`, `u8 late_rows`.

### 3.2 Tables

**Core**

| Table | Key | Value |
|---|---|---|
| `meta` | `&str` | `store_schema_version`, `instance_id`, `created_ms`, `writer_build`, `clean_shutdown`, `last_integrity_ms`, `last_backup_ms`, `retention_revision` |
| `dict` | `kind u8, id u32` | UTF-8 text (at most 512 bytes) |
| `dict_rev` | `kind u8, text bytes` (`&[u8]`) | `id u32` |
| `checkpoint` | `&str` `<projector>/<log>`, such as `usage/readings` | checkpoint (2.7) |
| `idem` | `log u8, event_type u32 (dict), sha256(key)[0..16]` | `seq u64` |

**Usage (phase 1)**

| Table | Key | Value | Serves |
|---|---|---|---|
| `account` | `a` | `{identity, p, account_id, plan, platform, first_ms, last_ms}` | |
| `window` | `w` | `{identity, key, label, kind, unit, window_minutes, pool_id, pool_label, model_ids}` | |
| `series` | `a, w` | `{first_ms, last_ms, rows}` | list of series; "all accounts at time T" |
| `series_by_provider` | `p, a, w` | `()` | **by provider** |
| `reading` | `a, w, t_first` | `Reading` | **an account and window over a time range**; with `series`, **all accounts at time T** (one reverse seek per series) |
| `reading_by_time` | `t_first, a, w` | `()` | **every change between t1 and t2, all accounts**; age-based pruning |
| `latest` | `a, w` | `t_first` + `Reading` | **the latest reading per account** (prefix `a`) |
| `account_latest` | `a` | `{status, sampled_ms, observed_ms, is_active, source, platform}` | account status now |
| `period` | `a, w, t_start` | `{reset_at_ms, end_ms, start_used, end_used, max_used, consumed_pts, hit_limit_ms, rows}` | reset periods: how each window was used up |
| `rollup_hour` | `a, w, h` | `Rollup` | burn over any period, hourly charts |
| `rollup_day` | `a, w, d` | `Rollup` | long-range charts |

**Attribution and activity (phase 2)**

| Table | Key | Value | Serves |
|---|---|---|---|
| `session` | `s` | `{p, machine, a, account_how, thread, parent s, st, project, first_ms, last_ms}` | |
| `tokens` | `s, m, e, h` | `{input, cache_read, cache_write, output, reasoning, requests, list_usd, passes}` | **attribution by session** |
| `tokens_by_model_effort` | `m, e, h, s` | `()` | **attribution by model and effort over time** |
| `tokens_by_stream` | `st, h, s` | `()` | **attribution by stream** |
| `tokens_by_account` | `a, h, s` | `()` | the split behind each account-hour |
| `share` | `a, w, h, s` | `{est_pts, weight, solo, method}` | estimated quota per session |
| `account_hour_mix` | `a, h` | `{pairs, list_usd, machines_complete, coverage}` | which account-hours are solo (7.1) |
| `rollup_model_effort_hour` | `m, e, a, w, h` | `{tokens by class, list_usd, est_pts, solo_pts, solo, sessions}` | cost model, hourly |
| `rollup_model_effort_day` | `m, e, a, w, d` | as hourly | cost model, long range |
| `activity_hour` | `tool u8, host u32, h, m` | `{p, profile, input, output, cache_create, cache_read, requests, cost_usd, fallback_usd, row_sha256}` | token activity per hour after the program logs are gone |
| `activity_by_provider` | `p, h, tool, host, m` | `()` | activity by provider |
| `model_alias` | `m` | catalog model id u32 | joins log model names to `catalog/models.json` |

**Runs (phase 3)**

| Table | Key | Value | Serves |
|---|---|---|---|
| `run` | `r` | `{doc, sha256, seq, wr, path, first_seen_ms}` | |
| `run_by_task` | `tt, m, e, t_end, r` | `()` | **runs by task type, model and effort** |
| `run_by_model` | `m, e, t_end, r` | `()` | runs by model and effort across task types |
| `run_by_writer` | `wr, t_end, r` | `()` | **runs by writer** |
| `run_by_time` | `t_end, r` | `()` | **runs by time**; recomputing costs when attribution arrives late |
| `run_reviews` | `r_reviewed, r_review` | `()` | quality and overturns |
| `run_cost` | `r` | `{measured {credits, usd, quota_pts, account, window}, reported_estimate, store_estimate {pts per meter, tokens by class, list_usd, coverage, method}, computed_ms}` | Scoreboard cost |
| `rollup_runs_day` | `tt, m, e, d` | `{runs, done, failed, wall_s, units, measured_cost, measured_n, est_cost, est_n}` | Scoreboard totals (medians are computed from the index rows) |

**Answers and operations (phases 4 and 5)**

| Table | Key | Value | Serves |
|---|---|---|---|
| `answer` | `id u32` | `{send, c, card_seq, card_sha256, kind, choice, note, signed answer, card}` | |
| `answer_by_card` | `c, t, id` | `()` | answers to one card |
| `answer_by_time` | `t, id` | `()` | answer history, newest first |
| `send` | `send u32` | `{at_ms, device, entries}` | |
| `delivery` | `id, step u8` | step state | |
| `delivery_pending` | `step u8, t_next, id` | `()` | retry queue |
| `switch_event` | `p, t, seq u64` | `{from a, to a, trigger, reason, threshold_pct, outcome}` | switch history |
| `update_job` | `t_start, job u32` | job summary | update history |

### 3.3 Rollups and projectors

| Projector | Reads | Maintains |
|---|---|---|
| `usage` | `usage.sampled`, `usage.seen`, `usage.day_closed` | `account` to `rollup_day`. A reading updates its hour and day rollup and its period at once. At 02:00 UTC it appends `usage.day_closed` for the previous day; readings for a closed day still enter `reading` but leave its rollups alone (counted in `late_rows`). Replaying a `day_closed` rewrites that day's rollups with identical values, so rollups survive the readings' expiry |
| `attribution` | `attribution.*`, `activity.hour` | session, token, share and model-effort tables; appends `attribution.day_closed` the same way |
| `runs` | `run.recorded`, plus `attribution` checkpoints | run tables; recomputes `run_cost` for runs whose time overlaps newly projected account-hours (found through `run_by_time`) |
| `answers`, `ops` | their events | their tables |

Consumption rules used by every rollup are in 7.1. Each projector's tables can be dropped and rebuilt from
its checkpoint at zero; that is the migration path (4.8).

## 4. Configuration

| Item | Setting | Why |
|---|---|---|
| 4.1 Location | `<getCcsDir()>/store/`, folder 0700, every file 0600, owner checked, no symlinks (same checks as today's history store). `CCS_HOME` moves it with the rest | tests use a temporary `CCS_HOME`, never the real home |
| 4.2 Cache | redb `set_cache_size(32 MiB)` (redb's default is 1 GiB). `AAC_STORE_CACHE_MB`, 8 to 256 | bounded; the working set (latest rows, today's readings, recent rollups) is a few MiB |
| 4.3 Durability | log: group commit (2.5), `barrier` for answers, switches and store events. redb: `Durability::Immediate` with `set_quick_repair(true)`, one commit per projector batch | the log is the durability point; redb commits carry their checkpoint, so a lost redb commit is replayed, never doubled. Volume is about one batch a minute, so an fsync per commit costs nothing and keeps restart replay short |
| 4.4 One writer | the dashboard server process. It holds `store.lock` (`flock` or `LockFileEx`, as PM's `pm.lock`) for its lifetime, and redb runs in `ConcurrencyMode::ExclusiveWriter` (its own file lock). Writes and queries run on store threads; every JS call returns a Promise | a second process gets `lock_held` and reads through the API. redb 4.5's `SingleWriter` mode would let local readers open the file, but needs local file locks; ExclusiveWriter is chosen so nothing reads the file over a network share |
| 4.5 How others read | the CLI on the dashboard machine calls the dashboard API as a paired device (bearer token); the read-only store routes are in the device scope (L6, approved). When the dashboard is stopped, `ai-account-center dashboard store ...` takes the lock and opens the store itself. Agents everywhere read the share export (8.3) | |
| 4.6 Integrity at start | logs: manifest, every frame of each active segment, the watermark and checkpoints. Sealed segments: SHA-256 on first read and in the weekly check. redb: `check_integrity()` at every start while the file is under 512 MiB (about 0.5 s per 100 MB), otherwise after an unclean shutdown and weekly | catches a bad file before anything trusts it |
| 4.7 Backups | daily: copy newly sealed segments (immutable, so incremental) and both manifests to `store/backups/<id>/` with a backup manifest (relative files, SHA-256, sizes, store version, durable boundary per log); weekly: a redb snapshot (projector paused, final commit, file copy, reopen; under a second). Keep 7 daily and 4 weekly. Off-machine copy: decision D3 | logs alone can rebuild everything; the redb snapshot only shortens a restore |
| Restore | `ai-account-center dashboard store restore <backup-dir>` with the dashboard stopped: verify every hash, stage under `tmp/`, move current files to `quarantine/`, promote, then replay from the snapshot's checkpoints. An interrupted restore finishes or rolls back from its intent | |
| 4.8 Versions and migrations | `store_schema_version` u32 in `meta`; frame version 2 and header version 1; each event type has its own `schema_version`; each projector has a version in its checkpoint. A projector change bumps its version and rebuilds its tables from the logs. An event change adds a reader for the new version; old events are read by their own reader, never rewritten. Newer than the code: `blocked_newer_store` | redb tables are all derived, so there are no in-place data migrations |
| 4.9 Compaction | readings: whole-segment expiry (section 5). redb: janitor every 6 hours deletes expired rows in batches of at most 10,000 keys per transaction (PM's numbers), then `compact()` weekly when free pages exceed 25% of the file | |
| 4.10 No native module | the loader catches the failed `require`, and the dashboard keeps the JSON history file path it had before the store; `/api/usage/*` answers 503 `store_unavailable` and `/api/store/status` says why | a machine without the module (the VM until its Linux build) keeps its history |
| 4.11 Bounded memory | redb cache 32 MiB; one open group at most 1 MiB; projector batches at most 4,096 events; query responses at most 20,000 points per page; import caps: a run 32 KB, a quota report 512 KB, a quota ledger file 4 MiB. The 32 MB history file is no longer parsed or rewritten whole on each sample | target: under 64 MiB resident for the module (phase 1 check) |
| Kill switch | `AAC_STORE=off` disables the store: the dashboard behaves as 4.10 | rollback during phase 1 (section 10) |

## 5. Retention

| Data | Kept | How it goes |
|---|---|---|
| Readings (`reading`, readings log `usage.*`) | 90 days | whole sealed readings segments, then redb rows by `reading_by_time` |
| Hourly and daily rollups, reset periods | for good | (ledger `usage.day_closed`) |
| Per-session tokens and shares (`attribution.tokens`, `attribution.share`, `tokens`, `share`) | 365 days | as readings |
| Model and effort rollups | for good | (ledger `attribution.day_closed`) |
| Activity hours | for good | (ledger `activity.hour`) |
| Runs, answers, switches, update jobs, store events | for good | ledger is never pruned |

Retention lengths are decision D1.

**Pruning with the log.** A sealed readings segment is deleted only when all hold: its `end_ms` is older
than the longest retention of any event type in it; every projector checkpoint for that log is past its
`end_seq`; the `*.day_closed` events for its days are durable in the ledger; and, with backups on, it is in
a verified backup. Deletion: intent, unlink, folder fsync, manifest update, intent removed,
`store.segment_retired` in the ledger. The gap below the oldest retained sequence is a legal
`retention_compaction` gap. `idem` rows whose sequence is below that point go with it.

**Size estimate** (estimates for 16 accounts and 59 windows, about 5,000 changed window readings and
4,600 "seen" marks a day; phase 1 measures the real figures):

| Part | Per day | 90 days | Per year |
|---|---:|---:|---:|
| Readings log, sealed and compressed (about 3.3 MB raw a day) | 0.35 MB | 32 MB | (expires) |
| `reading` and `reading_by_time` rows (about 125 B per reading) | 0.6 MB | 56 MB | (expires) |
| Rollups (hourly, daily, periods) | 0.1 MB | 9 MB | 37 MB |
| Ledger `day_closed` events, compressed | 0.01 MB | 1 MB | 4 MB |
| Attribution (about 2,800 rows a day): per-session rows, log and redb | 0.12 MB | 11 MB | 45 MB |
| Attribution rollups by model and effort | 0.03 MB | 3 MB | 10 MB |
| Runs (about 50 a day) | 0.05 MB | 4 MB | 18 MB |
| Activity hours (about 720 rows a day) | 0.09 MB | 8 MB | 32 MB |

Steady state after a year: about 230 MB on disk, holding 90 days of every reading and a permanent hourly
history, then about 100 MB more each year. Today's file holds at most 30 thinned days in up to 32 MB,
about 1.2 KB per stored account sample (about 300 B per window reading). The store spends about 195 B
per window reading (redb row plus its compressed log frame) and keeps every change: the same 30 days
would take about 29 MB with every change kept. Phase 1 must show fewer bytes per reading than today on
the same fixture.

## 6. Backfill

| Source | Import | Check |
|---|---|---|
| `account-analytics/quota-history-v1.json` | decoded by today's `decodeAccountAnalyticsHistory`; each observation becomes `usage.sampled` (or `usage.seen` when unchanged) in `sampledAt` order, with `source_at_ms` set and `import_ref = {file, sha256, index}`. Identities are today's hashes, so live accounts match | count of imported readings equals the file's window readings; `/api/accounts/analytics` returns the same series on the fixture as the old code |
| After a verified import | the files are left untouched (the rollback path reads them); each imported file is marked by its SHA-256, so a later run imports only what changed | |
| Share `aac-usage/history.json` | not imported: it is a 7-day copy of the same readings | |
| Share agent-report runs | scan `agent-reports/v1/writers/*/runs/*/*.json` with the Status tab's reading rules (list first, skip dot files, reject NUL bytes, validate `run@1`, 32 KB cap); each becomes `run.recorded` with `import_ref` | count equals the valid files; rejected files listed with reasons |
| `status/answers.jsonl` (phase 4) | every line in order: `answer` to `answer.recorded`, `send` to `answer.sent`, `delivery` to `answer.delivery`; malformed lines quarantined | the answer history API returns the same list before and after |
| quota-watch | its share ledger files (7.5) for as far back as they exist; earlier passes are gone | |
| Activity snapshot | the 31 days of hourly rows in today's snapshot become `activity.hour` | |

Every import is idempotent (its keys hit `idem`), resumable, and records `store.import_completed`.

## 7. The cost model

### 7.1 Definitions and formulas

| Quantity | Formula | Tables |
|---|---|---|
| Points | percentage points of one usage window on one account (a meter). Credit meters use credits instead | `reading` |
| Consumed points `P(meter, [t0, t1])` | sum over consecutive readings in the same reset period of `max(0, used_b - used_a)`; across a detected reset (the rules of `agent-usage-history.ts`) add the first reading's `used` (use since the reset). Credits: sum of decreases, rises counted as added | `reading`, `period`, `rollup_hour` |
| Coverage | minutes of the interval covered by readings no more than 10 minutes apart, over the interval's minutes | `Rollup.covered_min` |
| Burn rate `B(meter, span)` | `P / hours`, for any span: today's 5 m to 24 h within the period, plus 7 d, 30 d, 90 d (raw) and any span (hourly rollups); reported with coverage | `rollup_hour`, `rollup_day` |
| Per-period use | consumed points per reset period, time to 100%, how early it hit the limit | `period` |
| List-price weight `W` | `sum over class c of tokens_c * price_c(model) / 1e6`, classes input, cache read, cache write, output (reasoning is inside output); prices from the models.dev registry AAC already caches | `tokens.list_usd` |
| Solo account-hour | an account-hour whose tokens all come from one (model, effort), with every machine scanned and coverage at least 0.9 | `account_hour_mix` |
| Session share | `est(s, meter, h) = P(meter, h) * W(s, h) / sum of W over the account's sessions in h` (quota-watch's token share at list price) | `share` |
| Points per list dollar `k(model, effort, meter kind, plan)` | `sum of P / sum of W` over solo hours (measured), and over all hours using `est` (estimate) | `rollup_model_effort_*` |
| Quota per million tokens | `k * W_per_Mtok`, where `W_per_Mtok` is the observed list dollars per million tokens of that model and effort; shown with its token mix | same |
| Tokens per task | `T(run) = sum of tokens of the run's sessions` (thread match, overlapping `[startedAt, endedAt]`), by class; per unit when the task type's unit is countable | `run`, `tokens_by_*` |
| Quota per task | measured: `run.cost.quota.deltaPct` or `credits`; estimate: `sum of est` over the run's sessions and hours, prorated by overlap | `run_cost` |
| Task burn | `T(run) / wallSeconds` (tokens a minute) and quota per task over wall hours | `run`, `run_cost` |
| Effort comparison | `k(model, high) / k(model, xhigh)` and the ratio of median tokens per task, each side shown with its n | |

Meters differ by provider and plan (a percentage point of one plan's weekly window is not another plan's),
so `k` is keyed by meter kind and plan and is never averaged across them.

### 7.2 Measured or estimated

| Number | Kind |
|---|---|
| Readings, consumed points, burn rates, reset periods | measured |
| Tokens per session, model and request | measured (counted from logs by quota-watch or AAC's activity collector) |
| Effort when the log records it per turn (Codex `turn_context`) | measured |
| Effort from the T3 thread's setting (Claude Code) | estimate, labelled `effort_how: t3` |
| `k` and quota per million tokens from solo hours | measured, labelled "measured, solo hours" |
| Anything that uses a session share | estimate, labelled with `method` (`token-share-at-list-price`) |
| Run cost the writer reported in `cost.quota` or `cost.credits` | measured; `cost.estimate` stays an estimate |
| USD at list price | estimate (subscriptions do not bill per token) |
| A missing number | `null`, never zero |

### 7.3 Minimum samples before a value is shown

| Value | Minimum | Below it |
|---|---|---|
| `k` (measured) | 6 solo hours and 20 consumed points (percent meters move in whole points) | "too few solo hours (n)" |
| `k` (estimate) | 24 account-hours and 20 points | "too little data (n)" |
| Tokens and quota per task, per (task type, model, effort) cell | `catalog/measures.json` `minRuns` (5 today) | blank, n shown, not ranked |
| Effort comparison | both sides meet their minimum | the side that falls short is named |
| Burn rate over 7 d and longer | coverage at least 50% | the value with "covers n%" |

### 7.4 Scoreboard

Each Scoreboard cell (task type, model, effort) gets a cost column: measured quota or credits per unit
(median, n) and, apart and labelled, the store's estimate (median, n, method), plus tokens per unit
(median, n). Units come from the task type's `unit` in `catalog/task-types.json`; a run without a usable
size counts per run.

### 7.5 What quota-watch would need to publish

Its local `ledger.jsonl` keeps 8 days, and the share `quota/report.json` holds only the latest pass with
per-thread totals. To keep per-session history after the provider logs are gone, quota-watch would also
write one file per pass, written once (temporary file and rename, as runs), kept at least 30 days:

`agent-reports/v1/quota/ledger/<YYYY-MM-DD>/<writer>--<seq>.json`, schema `quota-ledger@1`:

| Field | Meaning |
|---|---|
| `schema`, `writer`, `seq`, `writtenAt` | as `quota-report@1` |
| `pass` | `{start, end, generatedAt, methodId: "token-share-at-list-price", ratesSha256}` |
| `machines[]` | `{name, scanned, error}` |
| `sessions[]` | `{machine, provider, account, accountHow ("t3", "log", "learned", "folder", null), session, parentSession, thread, stream, project, model, effort, effortHow ("log", "t3", null), first, last, tokens {input, cacheRead, cacheWrite, output, reasoning, requests}}`, one row per session and (model, effort) in the pass |
| `shares[]` | `{account, meter, unit, observed (measured change in the pass, null if unread), known, sessions [{session, estimate, weight}]}` |

`session` is the published key `sha256("qw-session-v1:" + provider + ":" + native id)`, first 16 hex,
never the raw id. `effort` needs two collector changes: Codex rollout logs carry it per turn; for Claude
Code, read the thread's effort from T3's state database (as quota-watch already reads threads) and mark it
`t3`. Runs would join more reliably if `run@1` carried an optional `t3Thread` (today the join goes
through the writer's `thread.json`).

Agreed with quota-watch's owner (2026-11-06); `quota-ledger@1` ships after their scrubber fix:

- **Estimated effort is marked.** A row with `effortHow: "t3"` also carries `estimate: true` on that
  field, so nothing reads an effort T3 recorded as the provider's. The importer keeps that mark and the
  cost model labels such rows as estimates (7.2).
- **Files are write-once, and `seq` has gaps.** A pass whose write fails writes the next `seq`; it never
  retries under the same name. The importer dedupes by `(writer, seq)` and treats a missing `seq` as a
  pass that was not published, never as an error or as zero use.

## 8. API and CLI

### 8.1 Server routes

Same authentication, origin and session checks as the other dashboard routes; `Cache-Control: no-store`;
errors `{error, code}` with fixed sentences.

| Route | Returns |
|---|---|
| `GET /api/store/status` | open state, versions, file sizes, each log's watermark and segments, projector checkpoints and lag, last integrity check and backup, gaps, whether the native module loaded |
| `GET /api/usage/history?account&window&from&to&resolution=raw\|hour\|day` | points or rollups for series, paged |
| `GET /api/usage/at?t=` | every series' reading at time T (or "no reading") |
| `GET /api/usage/burn?account&window&span=1h\|6h\|24h\|7d\|30d\|90d&from&to` | burn rate, coverage, reset periods in the span |
| `GET /api/cost/models?model&effort&provider&plan&days` | `k`, quota per million tokens, n, coverage, measured and estimate apart |
| `GET /api/cost/tasks?taskType&model&effort&days` | tokens, quota and wall time per task and per unit, n |
| `GET /api/attribution?by=model-effort\|stream\|session&from&to` | attributed tokens and estimated points |
| `GET /api/runs?taskType&model&effort&writer&from&to&cursor` | runs with their cost |
| existing `GET /api/accounts/analytics`, `/api/status/answers` | unchanged response shapes, served from the store |

### 8.2 CLI on the dashboard machine

`ai-account-center dashboard store status | check | backup | restore <dir>` (built); `export` and `accept-loss` come later. Through the API
while the dashboard runs (read-only commands, L6; `restore` and `accept-loss` only with the dashboard
stopped); with the lock when it is stopped.

### 8.3 Share export and `aac-usage` for agents

The exporter keeps writing `usage.json`, `usage.md` and `history.json` each sample, and adds, written
whole and atomically:

| File | Content | Written |
|---|---|---|
| `history-hourly.json` | hourly rollups per series, 90 days | each hour |
| `history-daily.json` | daily rollups per series, all days | each day |
| `cost.json` | the cost tables of 7.1 by model, effort, meter kind and plan, and per task type; measured and estimate apart; n and minimums | after each quota-watch import, at most every 10 minutes |

`usage.json` gains `burn.last7Days` and `burn.last30Days` (additive). The `aac-usage` reader gains:

| Command | Reads |
|---|---|
| `aac-usage history --days N [--resolution hour\|day]` | up to 7 days `history.json`, up to 90 `history-hourly.json`, beyond that `history-daily.json`; prints consumed points, burn rate and coverage per series |
| `aac-usage cost --model <id or label> [--effort high] [--task-type build] [--json]` | `cost.json`; prints quota per million tokens per meter and plan, tokens and quota per task, n, measured or estimate, and the model's other efforts for comparison |

## 9. Native module

### 9.1 Crate

```
native/aac-store/
  Cargo.toml, Cargo.lock, rust-toolchain.toml (1.98.0, the web dashboard's pin), build.rs (napi-build)
  registry.json             value registry (2.7)
  src/lib.rs                napi exports only
  src/log/                  frame, canonical msgpack, segment, manifest, writer, reader, recovery, compress
  src/db/                   keys, dict, tables, values
  src/project/              usage, attribution, runs, answers, ops, rollup
  src/query/                usage, burn, cost, runs
  src/maint/                retention, backup, restore, integrity, compaction, lock
  src/import/               quota_history_v1, runs, quota_ledger, answers_jsonl, activity
  tests/                    integration, crash cuts, replay determinism
src/store/                  (TypeScript) native-loader.ts, store-service.ts, types.ts
```

Dependencies, exact pins in `Cargo.lock`: `redb = "=4.5.0"`, `napi` and `napi-derive` 3, `zstd`,
`crc32fast` (ISO-HDLC), a small in-crate canonical MessagePack encoder and strict decoder, `sha2`, `ulid`.
The OS lock uses `std::fs::File::try_lock`. `zstd` builds C code: the VM and the Windows build machine
need a C compiler (checked in phase 0; the pure-Rust `ruzstd` is the fallback).

### 9.2 Build

| Step | Detail |
|---|---|
| `scripts/build-native.js` | `cargo build --release --locked` for the host target; copies the result to `dist/native/aac-store.<platform>-<arch>.node`; records its SHA-256 for `deployed-build.json`. Runs inside `bun run build` and `build:all` |
| Platforms | `linux-x64` first and required on the dashboard VM (built there at deploy, as the WebAssembly UI is). Then `win32-x64` (through the Windows build recipe) and `darwin-arm64`, optional: missing means 4.10 |
| `verify-bundle.js` | fails the build when the host's `.node` is missing or its hash does not match |
| Package | `files` adds `native/aac-store/` sources (as `web-dashboard/` sources are listed) so an install can rebuild |
| Loader | `src/store/native-loader.ts` picks the file by `process.platform` and `process.arch`, checks the export version, and returns `null` on any failure (4.10). Works under Node and Bun (both load N-API modules) |

### 9.3 Tests

| Layer | Tests |
|---|---|
| Frame | golden bytes for each event type; CRC vector `0xcbf43926`; every rejection rule of 2.3 (bad magic, prefix CRC, oversize lengths, nonzero flags, sequence regression, noncanonical MessagePack) |
| Crash cuts | for a recorded segment, truncate at every byte offset and at every intent step of seal, compress, expiry and restore; recovery yields exactly the longest valid prefix, never a reused sequence, and a correct disclosure |
| Corruption | flip bits in active and sealed segments and in `aac.redb`; check 2.6's actions and that projections equal a clean rebuild of the survivors |
| Replay | build redb from the logs twice and from a mid-way checkpoint; table dumps are identical |
| Indexes | each index answers the same as a full scan of the primary table on random data (property tests) |
| Cost | fixed fixtures with known solo and shared hours; `k`, shares and per-task values equal hand-computed results; minimums suppress values |
| TypeScript | loader fallback with the module absent; route auth and fixed errors; `/api/accounts/analytics` equal before and after on fixtures; burn rates equal today's `burnRate` on the same readings |
| Budgets | a 90-day synthetic replay (16 accounts, 59 windows): bytes per reading below today's, module resident memory under 64 MiB |

All tests use a temporary `CCS_HOME`, never the real home.

### 9.4 CI parity and the build lane

`scripts/ci-parity-gate.sh` and `.github/workflows/ci.yml` gain `cargo fmt --check`, `cargo clippy
--locked -D warnings` and `cargo test --locked` for `native/aac-store`, and the build gains the `.node`.
The Rust toolchain is already installed for the WebAssembly UI. Expect a first cold build of about two
minutes (redb, zstd) and seconds after that with `target/` cached; the pre-push gate grows by that much.

## 10. Phases

| Phase | Scope | Acceptance |
|---|---|---|
| 0 | crate, both logs, redb core tables, loader, build and CI | frame, crash-cut and corruption suites pass; CI parity green; `.node` built on the VM; the dashboard runs with the module absent and with `AAC_STORE=off` |
| 1 | usage readings: sampler writes `usage.*`; usage projector; rollups and periods; analytics route, burn rates and share export read the store; backfill; retention; backups | backfill count and analytics output match on the fixture; burn rates match today's for spans under 24 h; 7-day and 30-day burn shown with coverage; bytes per reading below today's; resident memory under 64 MiB over a 90-day replay; kill -9 during sampling loses no acknowledged reading; restore from backup reproduces the same tables |
| 2 | quota-watch ledger import (needs 7.5), activity hours, attribution tables, cost model, `cost.json`, `aac-usage cost` | imports idempotent (re-running adds nothing); cost fixture values exact; estimates labelled; activity rows still served after their source logs are removed |
| 3 | runs import, reviews, `run_cost`, Scoreboard cost column, `aac-usage history --days` | every valid share run imported once; Scoreboard cells match a direct computation; cells under `minRuns` are blank with n |
| 4 | Status answers on the ledger (after `t3/ce359811` lands) | answer history identical before and after; an answer is acknowledged only after its barrier fsync; a crash between append and share copy resumes delivery |
| 5 | switch history, update-job history | every switch path (auto, manual, CLI) writes one `switch.recorded`; history visible in the dashboard |

**Phase 1 cut-over and rollback.** No dual writing. The backfill leaves the JSON files untouched.
Rolling back means `AAC_STORE=off` (or the previous build): the JSON path resumes from the old file,
with a gap for the store's days.

## 11. Implementation (phases 0 and 1)

Phases 0 and 1 are built on branch `t3/aac-store`. Phases 2 to 5 remain design.

### 11.1 Changes from the design above

| Area | Built | Why |
|---|---|---|
| Usage events (2.2) | one `usage.sampled` (`{observations: [...]}`) and one `usage.seen` (`{accounts: [...]}`) per sampling tick, not one per account | a frame's envelope is about 380 bytes; per-account "seen" frames doubled the log |
| Usage dedupe (2.2) | the projector's per-account rule (sample time never goes back, an unchanged sample in the same 5-minute bucket is skipped); `idem` is kept for ledger events from phase 2 | an `idem` row per reading would cost more than the reading |
| Event id (2.3) | `<log>-<sequence hex>` (`r-000000000000002a`) | unique for the store's life because sequences are never reused; no random source needed |
| Intents (2.4, 2.6) | crash steps are reconciled from file presence, names and hashes at open (seal, compress, retire); restore keeps an intent file | each step is provable from the files, so no separate intent is needed |
| Spans (3.1) | a reading row is a span; an unchanged sighting more than 10 minutes after the last one starts a new span, so gaps stay visible; the open span lives in `latest` and its `reading` row is rewritten when it closes | no copy-on-write churn of the readings tree every 5 minutes |
| Row and rollup values (3.1) | `Row`: version, flags, presence mask, `last_seen`, then the present fields; `Rollup`: presence mask, percentages as f32, credits f64 | fewer bytes per row; rollups are summaries shown to two decimals |
| Tables (3.2) | added `account_state` (account status spans) and `day_closed` (closed days) | the history reconstruction and day closing need them |
| Reconstruction | `historyRecords` rebuilds the JSON history's account records from the spans, adding readings on its grid (every 5 minutes within a day, hourly beyond) inside observed spans | the analytics route, burn rates and `history.json` keep their exact inputs |
| zstd (2.4) | level 9 | 50.7x on real segments against 54x at level 19, at 1/75 of the time and far less memory |
| redb integrity (4.6) | `set_check_integrity_on_open` below 512 MiB (every page checksum at open); a read-only version probe first, guarded against redb panics on damaged pages | redb may panic on a damaged file opened without the check |
| No module (4.10) | the JSON history file path keeps running (as before the store) | a machine without the module, such as the VM until its Linux build, keeps its history |
| Backfill (6) | the JSON file and every readable file under `account-analytics/archive/` (JSON object, array, JSON lines, `.gz`, nested folders); unreadable files and records are counted and skipped; sources are left untouched; each imported file is marked by its SHA-256 | the rollback path reads the JSON file; the archive's format is not fixed |
| Backups (4.7) | `<root>/pool/<log>/<segment>` plus `<root>/sets/<id>/backup.json`; local root `store/backups/`; share root `share/ai-account-center/store-backups/<instance>` (or `AAC_STORE_SHARE_BACKUP_DIR`), used only when `share/ai-account-center` exists beside the usage snapshot folder | an unmounted share never grows a local folder |
| CLI (8.2) | `ai-account-center dashboard store status|check|backup|restore <dir>` | it follows `dashboard usage-snapshot`; status uses `AAC_DASHBOARD_URL` and `AAC_DEVICE_TOKEN` while the dashboard runs |
| Analytics (8.1) | `/api/accounts/analytics` keeps its 31-day range; the 90 days are served by `/api/usage/*` | no Slint range change in this phase |
| Build (9.2) | `bun run store:build` compiles into `native/aac-store/prebuilt/` (gitignored, with a `.json` note of its sources) and copies into `dist/native/`; every `build` copies an existing prebuilt module; `verify-bundle` warns when it is absent | `build:all` and `prepack` delete `dist/`; the Linux module cannot be built yet |
| CI gate (9.4) | `bun run store:check` and `store:build` in the parity gate and GitHub CI; `AAC_STORE_RUST=off` skips them | machines that may not run Rust builds |
| MessagePack | the decoder caps nesting at 64 (PM: iterative, no cap) | AAC payloads are shallow |
| Folder fsync | Unix fsyncs folders; Windows cannot, and relies on NTFS metadata journaling | |

### 11.2 Owner decisions (answered 2026-11-06)

| | Answer |
|---|---|
| D1 Retention | A: readings 90 days, per-session tokens 365 days, hourly rollups for good |
| D2 Phase 1 | A: usage readings only |
| D3 Off-machine backups | A: daily new sealed segments and a weekly redb snapshot to the share |
| D4 Claude effort | A: from the T3 thread's setting, labelled an estimate (phase 2) |
| D5 Switch history | A: record it (phase 5) |
| L6 Device scope | approved and built: paired device tokens may `GET /api/store/status`, `/api/usage/*` and `/api/cost/*`, and nothing else new |

### 11.3 Acceptance

| Check | Result |
|---|---|
| Frame, crash-cut (a cut at every byte offset of a torn tail and of the acknowledged region), corruption (bit flips in sealed segments, a damaged redb, a damaged manifest) | pass (`bun run store:test`: 23 Rust tests) |
| Sequences never reused; a newer store left byte-for-byte untouched; a second writer refused | pass |
| Replay from nothing and from a mid-way checkpoint gives identical tables | pass |
| Analytics output and burn rates equal to the JSON history on the same samples | pass (`tests/unit/store/store-usage.test.ts`) |
| Backfill of the JSON file and a mixed archive folder; re-run imports nothing | pass |
| Closed days keep rollups and final periods after their readings retire and redb is rebuilt | pass |
| Backup to a local root and a mirror, restore from the mirror reproduces the tables; a tampered backup is refused | pass |
| Kill during sampling loses no acknowledged reading | pass (`tests/unit/store/store-kill.test.ts`, 5 of 5 runs) |
| Device token reaches only the new GET routes | pass (`tests/unit/store/store-routes.test.ts`) |
| Dashboard without the module, and with `AAC_STORE=off` | pass (JSON path) |
| Memory: a store-only process over 10 days (16 accounts, minute samples, daily maintenance) | 43 MiB peak working set: pass |
| Bytes per reading below today's | **fails**: see 11.4 |
| `.node` built on the dashboard VM | not done: Rust builds are not allowed on the VM now; see 11.5 |

### 11.4 Measured size (Windows, synthetic busy workload)

16 accounts with 4 windows each, a sample every minute for 30 days, daily maintenance and a final
compaction:

| | JSON history (today) | Store |
|---|---:|---:|
| Kept | 30 days, thinned (hourly beyond a day): 15,780 records, 62,952 window readings | every change for 90 days: 67,691 reading spans, 46,080 hourly rollups, 2,848 periods |
| Bytes | 17.5 MB | redb 30.6 MB (7.2 MB of rows in 15.3 MB of pages) and the readings log about 6 MB (117 MB raw, 50x smaller sealed, plus at most a day raw) |
| Per stored reading | 277 B | about 585 B |

The store holds more (every change, rollups kept for good), but it does not yet meet "less space than
today's files". Two causes: redb leaves B-tree leaves half full, and while a database fits in its
first allocation region redb doubles the file to grow it (its region size is not configurable outside
redb's own tests). Proposed follow-up, estimated to bring the same 30 days near 15 MB: pack each
series' readings and its hourly rollups into one value per series and day (fewer, fuller pages), and
serve "changes between two times" by per-series scans instead of the `reading_by_time` index.

### 11.5 Linux

The dashboard runs on Linux and needs `aac-store.linux-x64.node`. One command, on the VM or on Nas1 (same
or older glibc than the VM):

```bash
bun run store:build
```

It needs cargo with rustup (the crate's `rust-toolchain.toml` pins 1.98.0 and installs it), a C
compiler for zstd (`build-essential`), and glibc no newer than the VM's. No napi CLI is needed: cargo
builds the cdylib and the script copies it as `.node`. Copy `native/aac-store/prebuilt/` to the VM's
checkout when built elsewhere; the next `bun run build` puts it in `dist/native/`. Until then the VM's
dashboard keeps its JSON history.

Not yet run on Linux: the Rust suites, the kill test, folder fsync, and the first backfill of the VM's
real history and archive.

## Notes for the lead

- **L1. CRC32 rather than CRC32C.** The brief named CRC32C; Puppet Master's `SeglogFrameV2` uses
  CRC-32/ISO-HDLC. This design follows PM so the frame code can later serve PM unchanged. At AAC's volume
  the speed difference does not matter.
- **L2. Compression.** PM's first profile stores frames uncompressed. AAC compresses whole sealed
  segments with zstd and verifies the decoded bytes against the manifest hash; frames inside are
  unchanged (`flags = 0`).
- **L3. Two logs.** PM uses one app-global log and successor-generation compaction for retention. AAC
  splits logs by retention class so expiry is deleting whole files.
- **L4. Inputs that differ from the brief.** There is no quota-history archive. Switch history is not kept
  today. `status/answers.jsonl` exists only on `origin/t3/ce359811`. `origin/t3/8ba2e5d2` and
  `docs/scoreboard.md` are not on origin; the run contract here comes from the share's `run@1` schema,
  `catalog/`, and the agent-reports format in the 2026-11-06 status-tabs share folder.
- **L5. Memory.** Today every changed sample parses and rewrites the whole history file (up to 32 MB).
  Phase 1 removes that; the memory-growth investigation may want to know.
- **L6. Device scope (approved).** Paired device tokens may also `GET /api/store/status`,
  `/api/usage/*` and `/api/cost/*` (`api-request-guard.ts`); every write and every other route stays
  refused.
