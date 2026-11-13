# AAC store: design

Status: **phases 0 and 1 built** (usage readings; section 11 records what changed from the design and
what was measured), and **Analytics usage history built** (section 12); the rest of phases 2 to 5 is
design. This document is the contract for the store. Code that
disagrees with it is a bug in one of the two; fix whichever is wrong and update this document in the same
change.

AAC keeps its history in one embedded store per dashboard machine: an append-only **event log** is the
record of what happened, and **redb** tables are projections of that log that answer queries. It follows
Puppet Master's storage plan (`Plans/storage-plan.md`: seglog, projectors, redb), so AAC is that plan's
first working trial. Section 2.7 lists what is mirrored and what is left out.

**Boundary.** The store is a storage library: a small Rust crate (`native/aac-store/`) loaded by the
TypeScript server as a native Node module (napi-rs). It appends, projects, queries and maintains, and
nothing else. Routes, authentication, collectors, provider parsing, switching, signing and every product
rule stay in TypeScript. The crate makes no network or provider calls. The TypeScript server
stays; a new server piece is a native module like this one, and existing TypeScript is not ported
unless the owner asks (AGENTS.md).

## Current storage policy (2026-11-13)

The owner's permanent-capture direction supersedes the earlier age-retention design below.
Raw normalized quota observations and their authoritative event logs are kept forever by default:
`retentionDays: 0` means unlimited. An explicitly positive value retains the legacy 7–3650-day
policy. The dashboard's recent query horizon is independent of physical retention. Old data already
pruned by an earlier installation cannot be recreated from rollups.

Schema **2** adds a 30-byte `account_state` value: version byte 2; one status/platform/active byte;
a big-endian source dictionary id (`KIND_SOURCE`); `observed_ms`, `last_seen`, `last_observed` as
three signed big-endian i64 values. Existing v1 MessagePack states remain readable. Maintenance
converts them in 1,000-row transactions, preserving keys and every field. Migration completion is
recorded only after the last batch. Logs retain their previous event schemas. Older builds refuse
schema 2 before opening redb for writing. Do not lower the admission marker without converting values.

For rollback, first stop every store writer and preserve the entire store folder. On an offline
copy, run the new crate's `state-format --rollback-v1 <offline-store-directory>` example. It converts
v2 states back to v1 and changes the marker only after conversion completes; all other data and logs
remain. Alternatively restore a complete pre-change store backup. Keep a copy of the upgraded
store too. Additive projector data stays on disk if older software cannot display it.

Accepted agent-report thread/status/tasks/card revisions now append `capture.report` to the permanent
ledger. The independent `capture` projector owns `capture_head`, `capture_revision`, `capture_content`
and `capture_by_time`; it does not share usage dictionaries. Document contents are canonical
MessagePack, zstd level 3 only when smaller, with a declared length and verified SHA-256. Source sequence,
source SHA, first observation, ingest time and known correlation identities are kept with each revision.
Source object identity, revision identity and content identity are distinct. Repeated scans/restarts
are idempotent; distinct revisions, including A→B→A, survive. The Status reader captures without page
demand. Source versions overwritten between scans still require an immutable producer outbox; an AAC
storage failure is logged and current revisions are retried. This is not a claim that every producer
transition or the missing native-session binding is already captured.

Native methods `captureReports`, `captureHistory` and `status.capture` support ingestion, inspection
and byte/count/last-ingest health. `status.storageHealth` reports raw policy, hot file bytes, an advisory
2 GiB budget and warning/exceeded flags. Crossing it never deletes data. Yearly growth is null until
measured. Complete cold-tier automation, model/session/task bindings and other capture gaps remain
separate work; `registry.json` describes the implemented families.

History reconstruction now chooses surviving timestamps before constructing JSON and uses binary
searches over sorted spans. This leaves queries and ordering unchanged while avoiding discarded
serialization. Explicit retention drains all expired state batches, even if no reading-day batch is
full. Legacy quota writers remove only their own failed temporary files. Orphan removal is an
offline maintenance operation after every quota-file writer has stopped: writes and backfill never
reap another temporary file, because age cannot establish that a suspended writer has exited.
Source JSON and archives remain intact for rollback.

Every module can use the [shared loaders and reusable fixture](secret-policy.md).
Report readers and native capture share `src/shared/secret-policy.json`: refusal
patterns, credential keys and embedded-text work budgets. Native batch preflight rejects decoded
keys/strings and embedded JSON before ledger append; its replay projector checks the same policy.
The policy is a union of the reader and legacy native protections: authorization-family keys,
Basic/Token/Digest credentials, URL userinfo (including hosts without dots), dashboard tokens,
credential assignments and the legacy token thresholds are preserved. Basic credentials with a
short Base64-encoded `user:password` are refused while ordinary prose remains allowed. The shared
fixture suite and a generated legacy-shape check prevent losing native-only protections.

Storage status also reports `storageHealth.quotaTemporaries`: matching regular UUID temporary-file
count and apparent bytes, scan completeness/errors and an offline-maintenance diagnostic. All ages
are included, since age cannot distinguish a live suspended writer from an orphan. Metadata-only
observation neither reads file contents nor changes/deletes files; links are excluded. An incomplete
scan reports only the observed lower bound with a warning. Stop every writer using that CCS_HOME
before offline removal of confirmed orphan files.

The policy is packaged as a build input and emitted with the TS reader. Native build fingerprints
include it, so policy changes require rebuilding the module. Native parse limits fail closed.

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
| Active file length differs from the acknowledged length at seal, while both ends still decode (a whole interior record removed). A sequence gap that keeps the acknowledged length is not this: a restart may jump the lease, and a failed write already resets the length to the bytes that remain | seal that segment `degraded` with the length and the acknowledged sequence bounds; do not hash it as intact | a gap; status `degraded` and the check is not OK, including after a log-only restore |
| Sealed segment whose SHA-256 or zstd checksum fails | never modify it; mark it `degraded`; restore that file from a backup with the same name and hash if one exists; otherwise salvage the valid frames on both sides of the hole and project those | a gap with exact or bounded range |
| Loss in the **ledger** log (answers, runs, switches) | as above, and Status answers become read-only until a backup restore or `ai-account-center dashboard store accept-loss` (later) (`store.loss_accepted`) | blocking banner |
| Loss in the **readings** log | as above; writing continues | a history gap (shown as no reading, never as zero) |
| Manifest missing or unreadable, segments present | rebuild the manifest by validating every frame; the watermark becomes the last valid frame | `store.integrity_detected` |
| `aac.redb` fails to open for any reason but another holder (`check_integrity()` fails, a file left by a kill while redb was creating it, which has no magic number, an empty or unreadable file) | move it to `quarantine/`, create a new file, replay both logs from the start. A new redb (new store, rebuild, restore without redb) is made under `tmp/aac.redb.<random>` and renamed into place only after its first commit, so a kill mid-creation leaves only that temporary file, which the next open removes | `redb_quarantined`; rebuild in progress |
| A projector checkpoint that does not match the surviving events: an acknowledged event it was built from is gone or no longer validates (for example a bit flip in a sealed segment that is already projected), or the chain over retired events is not recorded | rebuild that projector's tables from the surviving events, then record the loss in the ledger | `projection_missing_events` (or `projection_unverifiable_missing`) with ordered bounds that hold the loss: from the first damaged or missing segment, or past the last surviving event, through the checkpoint (the whole covered range when neither is known); `projector_rebuilt`; the store is `degraded` |
| Any disclosed loss, on a later open | kept in the `finding` table in the same transaction as the rows it explains (loss found by the checkpoint check, with the rebuild it causes), restored from the ledger's `store.integrity_detected` after a rebuild; each is written to the ledger once, in groups of at most a projector batch, each group marking its findings ledgered in its own commit, so a kill between groups leaves the rest for the next open | the same finding, until it is resolved (`accept-loss`, later) |
| Store newer than the code (`store_schema_version`, a higher `store/release`, or an unknown frame/header version) | open nothing, change nothing | `blocked_newer_store`: the dashboard runs without history |
| Store written by one of the branch's unreleased test builds: logs or a redb file but no `store/release` marker, or a marker other than `1` (owner decision 2026-11-07: no upgrade from those builds) | open nothing, change nothing, delete nothing; a backup without `storeRelease: 1` is refused the same way | `unreleased_store`: "This store was written by an unreleased test build; move it aside and AAC will start a new one." The dashboard runs without history until the folder is moved |

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
| Values | hot tables (`reading_day`, `latest`, rollups) use the binary layouts below; other values are MessagePack with a leading version byte |
| Transactions | each projector commits its rows and its checkpoint in one write transaction (at most 4,096 events or 1 s per commit) |

`Row` (v1, the open span in `latest`): `u8 version`, `u8 flags` (cached, unlimited known, unlimited,
enabled known, enabled), `u16 present mask`, `u64 last_seen_ms`, then only the present fields, 8 bytes
each: `used_pct`, `remaining_pct`, `used`, `limit`, `remaining`, `reset_at_ms`, `expires_at_ms`,
`window_minutes`, `cached_sampled_at_ms`. A span covers `[t_first, last_seen_ms]` with the same values: a
repeated reading moves `last_seen_ms`, a change closes the span and starts a new one.

`reading_day` value (v1, `src/pack.rs`): one series' closed spans of one UTC day (the day `t_first` falls
in), oldest first. `u8 version`, varint count, then for each span: varint distance from the span before
(the first from midnight), varint length (`last_seen - t_first`), varint mask of the fields that differ
from the span before, then only those fields. A number is one varint when absent, whole or a hundredth
(`remaining_pct` also when it is `100 - used_pct`), otherwise a marker and 8 raw bytes. A time is a marker
and a zigzag varint distance from its previous value. Every field round-trips bit for bit.

`Rollup` value (v2): varint presence mask, then only the present fields: `consumed`, `credits_spent`,
`credits_added`, the four used percentages (`first`, `last`, `min`, `max`), `covered_ms`, `rows`, `resets`,
`late_rows`, `last_reset_ms`. Percentages and `consumed` keep f32 precision (one varint when whole), credits
keep f64 precision, counts are varints. `rollup_hours` value (v1): `u8 version`, a 24-bit mask of the hours
present, then their rollups in order.

### 3.2 Tables

**Core**

| Table | Key | Value |
|---|---|---|
| `meta` | `&str` | `store_schema_version`, `instance_id`, `created_ms`, `writer_build`, `clean_shutdown`, `last_integrity_ms`, `last_backup_ms`, `retention_revision` |
| `dict` | `kind u8, id u32` | UTF-8 text (at most 512 bytes) |
| `dict_rev` | `kind u8, text bytes` (`&[u8]`) | `id u32` |
| `checkpoint` | `&str` `<projector>/<log>`, such as `usage/readings` | checkpoint (2.7) |
| `idem` | `log u8, event_type u32 (dict), sha256(key)[0..16]` | `seq u64` |
| `finding` | `&str` `kind:start:end` | `{finding, ledgered}`: a disclosed finding kept across opens (2.6) |

**Usage (phase 1)**

| Table | Key | Value | Serves |
|---|---|---|---|
| `account` | `a` | `{identity, p, account_id, plan, platform, first_ms, last_ms}` | |
| `window` | `w` | `{identity, key, label, kind, unit, window_minutes, pool_id, pool_label, model_ids}` | |
| `series` | `a, w` | `{first_ms, last_ms, rows}` | list of series; "all accounts at time T" |
| `series_by_provider` | `p, a, w` | `()` | **by provider** |
| `reading_day` | `a, w, d` | packed closed spans | **an account and window over a time range** (one value per day in the range); with `series`, **all accounts at time T** (one reverse seek per series); pruning |
| `latest` | `a, w` | `t_first`, `period_start`, `Row` | **the latest reading per account** (prefix `a`); each series' open span, which is only here |
| `account_latest` | `a` | `{status, sampled_ms, observed_ms, is_active, source, platform}` | account status now |
| `period` | `a, w, t_start` | `{reset_at_ms, end_ms, start_used, end_used, max_used, consumed_pts, hit_limit_ms, rows}` | reset periods: how each window was used up |
| `rollup_hours` | `a, w, d` | packed `Rollup` per hour of the day | burn over any period, hourly charts |
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
| `usage` | `usage.sampled`, `usage.seen`, `usage.day_closed` | `account` to `rollup_day`. A reading updates its hour and day rollup and its period at once. At 02:00 UTC it appends `usage.day_closed` for the previous day; readings for a closed day still enter `reading_day` but leave its rollups alone (counted in `late_rows`). Replaying a `day_closed` replaces that day's hourly value and day rollup for each listed series, so rollups survive the readings' expiry and a rebuild answers as the live store did |
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
| 4.6 Integrity at start | logs: manifest, every frame of each active segment, the watermark and checkpoints. Every surviving event of both logs is read at open and each checkpoint's survivor-prefix chain (`sha256(previous, sequence, event_id)` from the chain over retired events, kept in the manifest) is recomputed; a checkpoint that does not match is rebuilt (2.6). Sealed segments: SHA-256 at every open as part of that read (a matching segment is then walked frame by frame without decoding each event; a mismatch is salvaged frame by frame) and in the weekly check. redb: `check_integrity()` at every start while the file is under 512 MiB (about 0.5 s per 100 MB), otherwise after an unclean shutdown and weekly | catches a bad file before anything trusts it |
| 4.7 Backups | daily: copy newly sealed segments (immutable, so incremental) and both manifests to `store/backups/<id>/` with a backup manifest (relative files, SHA-256, sizes, store version, durable boundary per log); weekly: a redb snapshot (projector paused, final commit, file copy, reopen; under a second). Keep 7 daily and 4 weekly. Off-machine copy: decision D3 | logs alone can rebuild everything; the redb snapshot only shortens a restore |
| Restore | `ai-account-center dashboard store restore <backup-dir>` with the dashboard stopped: validate the whole manifest first (an id and file names of `[A-Za-z0-9._-]`, file paths only `pool/<log>/<name>` or `sets/<id>/<name>`, the folder `<root>/sets/<id>`, every restored file listed with its SHA-256, a regular file inside the backup root and not a link), verify every hash, stage under a new `tmp/restore-<random>` (never a name from the manifest), check each staged copy's hash again, move current files to `quarantine/`, promote, then replay from the snapshot's checkpoints. An interrupted restore finishes or rolls back from its intent, which may name only this store's `tmp/restore-*` and `quarantine/*` folders | a backup manifest is input: nothing it names may reach outside the store and its backup root |
| 4.8 Versions and migrations | The first merged store is release 1: `store/release` holds `1`, written when a store is created, and backups record `storeRelease: 1`; a store or backup without it is refused (2.6). `store_schema_version` 1 in `meta`; the usage projector is version 1; frame version 2 (Puppet Master's `SeglogFrameV2` profile) and header version 1; each event type has its own `schema_version`; each projector has a version in its checkpoint. A projector change bumps its version and rebuilds its tables from the logs. An event change adds a reader for the new version; old events are read by their own reader, never rewritten. Newer than the code: `blocked_newer_store` | redb tables are all derived, so there are no in-place data migrations |
| 4.9 Compaction | readings: whole-segment expiry (section 5). redb: janitor every 6 hours deletes expired rows in batches of at most 10,000 keys per transaction (PM's numbers), then a weekly `compact()` only when the file is over twice its used pages: a trimmed file has no free pages, and below one 4 GiB region redb doubles the file at the next commit | |
| 4.10 No native module | the loader catches the failed `require`, and the dashboard keeps the JSON history file path it had before the store; `/api/usage/*` answers 503 `store_unavailable` and `/api/store/status` says why | a machine without the module (the VM until its Linux build) keeps its history |
| 4.11 Bounded memory | redb cache 32 MiB; one open group at most 1 MiB; projector batches at most 4,096 events; query responses at most 20,000 points per page; import caps: a run 32 KB, a quota report 512 KB, a quota ledger file 4 MiB. The 32 MB history file is no longer parsed or rewritten whole on each sample | target: under 64 MiB resident for the module (phase 1 check) |
| Kill switch | `AAC_STORE=off` disables the store: the dashboard behaves as 4.10 | rollback during phase 1 (section 10) |

## 5. Retention

| Data | Kept | How it goes |
|---|---|---|
| Readings (`reading_day`, readings log `usage.*`) | 90 days | whole sealed readings segments; redb spans by scanning each series' `reading_day` values through the cutoff's day: a value whose spans have all ended before the cutoff goes, one that straddles it is rewritten, the open span stays |
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

**Size estimate** (the design's estimates for 16 accounts and 59 windows, about 5,000 changed window
readings and 4,600 "seen" marks a day, before the packed layout; 11.4 has the measured figures):

| Part | Per day | 90 days | Per year |
|---|---:|---:|---:|
| Readings log, sealed and compressed (about 3.3 MB raw a day) | 0.35 MB | 32 MB | (expires) |
| Reading rows (design: about 125 B per reading; 11.4 measures about 11 B stored in day values) | 0.6 MB | 56 MB | (expires) |
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
| Points | percentage points of one usage window on one account (a meter). Credit meters use credits instead | `reading_day` |
| Consumed points `P(meter, [t0, t1])` | sum over consecutive readings in the same reset period of `max(0, used_b - used_a)`; across a detected reset (the rules of `agent-usage-history.ts`) add the first reading's `used` (use since the reset). Credits: sum of decreases, rises counted as added | `reading_day`, `period`, `rollup_hours` |
| Coverage | minutes of the interval covered by readings no more than 10 minutes apart, over the interval's minutes | `Rollup.covered_min` |
| Burn rate `B(meter, span)` | `P / hours`, for any span: today's 5 m to 24 h within the period, plus 7 d, 30 d, 90 d (raw) and any span (hourly rollups); reported with coverage. With no reading in the span (unknown or lost history), `P` and the rate are null; a series that was read and did not move is 0 | `rollup_hours`, `rollup_day` |
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
  Cargo.toml, Cargo.lock, rust-toolchain.toml (1.99.1, the web dashboard's pin), build.rs (napi-build)
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
| Indexes | each index answers the same as a full scan of the primary table on random data (property tests); day values answer ranges, "at time T" and limits as a filter of every span |
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
| Spans (3.1) | a reading row is a span; an unchanged sighting more than 10 minutes after the last one starts a new span, so gaps stay visible; the open span lives only in `latest` and enters its `reading_day` value when it closes | no copy-on-write churn of the readings tree every 5 minutes |
| Packed day values (3.1, 11.4) | `reading_day` and `rollup_hours` hold one value per series and UTC day | full pages instead of half-full ones: the size acceptance |
| No time index (3.2) | no `reading_by_time`; every series is scanned instead (see 11.4) | the index cost 1.7 MB of pages and served only pruning, the oldest reading and the first day to close |
| Rollup values (3.1) | percentages and `consumed` as f32, credits f64, whole values in a varint | rollups are summaries shown to two decimals |
| Tables (3.2) | added `account_state` (account status spans) and `day_closed` (closed days) | the history reconstruction and day closing need them |
| Reconstruction | `historyRecords` rebuilds the JSON history's account records from the spans, adding readings on its grid (every 5 minutes within a day, hourly beyond) inside observed spans; a grid reading's `observedAt` is its time plus the span's first observation offset, kept between the span's first and last observation | the analytics route, burn rates and `history.json` keep their inputs: the same analytics buckets, values and gaps; times inside a bucket are the grid's, not each sample's minute |
| Credentials (2.2) | every persisted string of an observation (account, window, pool, model ids, import reference), and every TypeScript meta value (`setMeta`), is refused, before the log append, when it matches the dashboard's log-redaction patterns (`sk-ant-`, `sk-`, `aacd_`, `xox?-`, `gh?_`, `github_pat_`, `glpat-`, `AIza`, `AKIA`, JSON web tokens, `api_key=`-style values, private key blocks), narrowed for stored text: a prefix counts only at the start of a token (`sk-` inside `task-` does not), and `Bearer`/`Basic`/`Token` only before a credential-shaped value (16 or more token characters with a digit or both cases), so `Token requests` is stored; `src/secret.rs` | the log is kept for good and copied to backups |
| Burn route (8.1) | `/api/usage/burn` finds its series with `seriesList` (series metadata seen in the span), not from a point query | a capped point query left series out |
| Filtered queries (3.2) | `seriesPoints`, `seriesList` and `historyRecords` choose their series through the indexes: identities through `dict_rev` and the account prefix, a provider through `series_by_provider`, a window key through the window table and then the keys of `series_by_provider`; they read only the selected series' `latest` rows, and the account state open at the range start by one reverse seek. Only an `account_ids` filter reads account rows (few, and `account_id` is not interned) | no metadata row of an unrelated series is read |
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
| Frame, crash-cut (a cut at every byte offset of a torn tail and of the acknowledged region), corruption (bit flips in sealed segments, a damaged redb, a damaged manifest) | pass (`bun run store:test`: 47 Rust tests) |
| A cut at every byte of a tick over the projection: exactly the surviving frames' day values, with redb kept and rebuilt | pass (`tests/store_packed.rs`) |
| Packed values round-trip bit for bit; day values answer as a filter of every span; retention splits a day value | pass (`src/pack.rs`, `tests/store_packed.rs`) |
| Sequences never reused; a newer store left byte-for-byte untouched; a second writer refused | pass |
| Replay from nothing and from a mid-way checkpoint gives identical tables | pass, and now also when samples arrive after their day closed |
| Analytics output and burn rates equal to the JSON history on the same samples | pass (`tests/unit/store/store-usage.test.ts`) |
| Backfill of the JSON file and a mixed archive folder; re-run imports nothing | pass |
| Closed days keep rollups and final periods after their readings retire and redb is rebuilt | pass |
| Backup to a local root and a mirror, restore from the mirror reproduces the tables; a tampered backup is refused | pass |
| Kill during sampling loses no acknowledged reading | pass (`tests/unit/store/store-kill.test.ts`, 5 of 5 runs) |
| Device token reaches only the new GET routes | pass (`tests/unit/store/store-routes.test.ts`) |
| A sealed segment loses an acknowledged frame: the projection is rebuilt to the surviving events and the loss disclosed; retirement is not mistaken for loss | pass (`tests/store_review.rs`) |
| Restore refuses manifests that reach outside (a traversing or absolute id, `..` or absolute file paths, a traversing segment name, a restored file without a hash, a linked file) and leaves the store and everything else unchanged | pass (`tests/store_review.rs`) |
| Credential-shaped strings in every persisted field are refused and never reach the log; ordinary labels such as `Token requests` and `task-0123…cdef` are stored | pass (`tests/store_review.rs`, `src/secret.rs`, synthetic values only) |
| Loss bounds are ordered and hold the lost event (a lost first frame) | pass (`tests/store_review.rs`) |
| Loss findings survive a plain reopen and a rebuild | pass (`tests/store_review.rs`) |
| A store with 20,000 findings not yet ledgered (more than one batch; what a kill between the projection and the ledger leaves) opens, ledgers each once, and a reopen or a rebuild changes nothing; after child processes are killed part way through that open, the next open discloses every one, each ledgered exactly once | pass (`tests/store_review.rs`) |
| A store from an unreleased test build (logs or redb, no marker; another marker) is refused with the owner's message and left byte for byte as it was; a newer marker reads as newer; a backup without `storeRelease: 1` is refused; the CLI shows the message | pass (`tests/store_review.rs`, `tests/unit/store/store-usage.test.ts`) |
| Burn over lost or unknown history is null, a measured flat series is 0 | pass (`tests/store_review.rs`) |
| Filtered queries answer with unrelated series, open spans, accounts and earlier account states unreadable; a window filter with another window's series unreadable | pass (`tests/store_review.rs`) |
| Each restore refusal is made by the guard its case names (asserted by message; each case fails with its guard removed) | pass (`tests/store_review.rs`) |
| Burn finds every series past the 20,000-point cap (11 series, 90 days hourly) | pass (`tests/unit/store/store-usage.test.ts`) |
| Analytics route from the store equals the JSON history: every account, window and point, by analytics bucket, over 28 hours with resets, an error hour, a cached window, a 40-minute gap and a credit balance, for the 24-hour and 7-day ranges | pass (`tests/unit/store/store-usage.test.ts`) |
| Open with the checkpoint check | 309 ms on the 30-day busy store (32 ms before the check; most of it reading and hashing the sealed segments) |
| Dashboard without the module, and with `AAC_STORE=off` | pass (JSON path) |
| Memory: a store-only process over 10 days (16 accounts, minute samples, daily maintenance) | 36 MiB peak working set: pass |
| Bytes per reading below today's, on the 30-day busy run | **pass: 216 B against 277 B** (176 B with a one-day raw tail); also with the snapshot's 5/32 fractional mix, 65 B against 279 B, whose 31.81 MB total is the planned every-change history (section 5): see 11.4 |
| `.node` built on the dashboard VM | not done: Rust builds are not allowed on the VM now; see 11.5 |

### 11.4 Measured size and query cost (Windows, synthetic busy workload)

Run it with `bun run store:size` (`tests/manual/store-size-run.ts`, a temporary folder only): 16 accounts
with 4 windows each, a sample every minute for 30 days, daily maintenance and a final compaction request.
"Before" is the layout with `reading`, `reading_by_time` and `rollup_hour`, on the same workload.

| 30 days | JSON history (today) | Store before | Store now |
|---|---:|---:|---:|
| Kept | thinned (hourly beyond a day): 15,780 records, 62,952 window readings | every change: 67,691 reading spans, 45,312 hourly rollups, 2,848 periods | the same |
| redb | | 30.49 MB (7.14 MB stored) | 5.28 MB (2.05 MB stored, 3.78 MB of pages) |
| Log, sealed and compressed | | 2.36 MB | 2.36 MB |
| Log, raw tail | | 7.00 MB | 7.00 MB |
| Total | **17.45 MB** | 39.85 MB | **14.64 MB** |
| Per stored reading | **277 B** | 589 B | **216 B** |

The reading and hourly-rollup tables went from 3.71 MB and 1.37 MB stored to 0.71 MB (about 10.5 B per
span) and 0.40 MB. A store and a JSON "reading" are not the same thing (a span can cover many samples), so
the totals are the plainer comparison; the store passes on both.

**Why the file is smaller.** Day values fill pages, the time index is gone, and only changed fields are
stored. The other cause was compaction itself: it trims the file to its pages, then the next commit finds no
free page and redb doubles the file (22.3 MB became 30.5 MB in the old layout). Compaction now runs only
when the file is over twice its used pages, so here it does nothing. If it were forced, this run would end
at 7.50 MB of redb, 16.85 MB in total, 249 B.

**The bar is bytes per reading** (section 5): the store keeps every change for 90 days (D1), so its total is
expected to exceed the thinned JSON file; section 5 estimated "about 29 MB with every change kept" for the same
30 days. A larger total with fewer bytes per reading is the plan, not a failure.

**Variants and the steady-state tail.** `STEADY=1` closes and reopens the store before the last day, so every
earlier segment is sealed and compressed and the raw tail is one real day, the most a store sealing at midnight
carries just before it seals. Without it the replay's segments seal on the wall clock, so up to 8 MiB stays raw.
`MIX=1` makes 10 of the 64 series (5/32, the share of fractional windows in the published snapshot) report
4-decimal percentages that rise every minute; the rest are as in the round run. 30 days each. The two tail modes'
answers differ only in the last day's hourly and daily rollups, because the replay records in 100-minute
batches and the reopen moves where a batch ends, so a different number of samples arrives after the last
day closed:

| Run | Raw tail | Store | Per reading | JSON history | JSON per reading | Result |
|---|---|---:|---:|---:|---:|---|
| Round | replay leftover, 7.00 MB | 14.64 MB | 216 B | 17.45 MB | 277 B | pass |
| Round | one day, 4.17 MB | 11.90 MB | 176 B | 17.45 MB | 277 B | pass |
| Mixed, 5/32 fractional | replay leftover, 6.11 MB | 32.35 MB | 66 B | 18.06 MB | 279 B | pass (total is the planned every-change history) |
| Mixed, 5/32 fractional | one day, 5.49 MB | 31.81 MB | 65 B | 18.06 MB | 279 B | pass (total is the planned every-change history) |
| All fractional (known worst case) | replay leftover, 8.57 MB | 22.18 MB | 313 B | 18.87 MB | 300 B | fails |
| All fractional (known worst case) | one day, 4.23 MB | 17.99 MB | 254 B | 18.87 MB | 300 B | pass |

**Why the mixed run's total is larger.** A fractional series changes every sample, so the store keeps 490,493
spans against 67,691 in the round run, and its account's tick is a `usage.sampled` event every minute instead of
a `usage.seen` every five. The JSON history keeps those series thinned to one record an hour beyond the last
day. The store's 31.81 MB is redb 16.84 MB (13.82 MB of pages, of which `reading_day` 7.29 MB stored, about 15 B
per span: a 4-decimal value takes a marker and 8 raw bytes) and the log 9.46 MB compressed plus 5.49 MB raw.
Each reading costs a quarter of the JSON history's while the store keeps seven times as many: the planned
every-change history, close to section 5's 29 MB estimate. Packing `account_state` (0.44 MB stored) and `period`
(0.42 MB) would save at most about 1.5 MB, so it was not done.

**A possible later saving, not built:** store 4-decimal values as scaled integers (ten-thousandths in a varint,
as hundredths are now), about 5 bytes instead of 9 per changed value, a few bytes per span of a fractional
series.

**The doubling.** It is real: redb's region size is not configurable outside its own tests, so in these runs the
file stepped at about 1.3, 2.6, 5.3, 10.6 and 15.8 MB. The round run by day: days 2 to 20 fall from 1,965 B to 316 B (a small store
carrying the replay's raw tail); days 20 to 30 move between 216 B and 329 B with that tail; after day 30 it is
over 277 B only on days 35 to 37 (305 B at the step to 10.6 MB). Over 90 days of every change the round run
takes 27.28 MB (redb 15.83 MB, log 7.46 MB compressed and 3.98 MB raw), 134 B per reading; the old layout took
103.56 MB, 510 B. The all-fractional run's leftover-tail failure is this step (5.47 MB of pages just past the
5.28 MB file) plus the 8.57 MB leftover tail.

**Queries after the change.**

| Query | Table and key | Cost |
|---|---|---|
| A series over a time range (`/api/usage/history`, raw) | `reading_day` range `a, w, day(from)..=day(to)`, then filter; open span from `latest` | one value per day in the range is decoded whole |
| A span before the range, and every series at time T (`/api/usage/at`) | `latest`, then a reverse seek on `reading_day` `a, w, ..=day(t)` | one seek per series, one value decoded |
| Rebuilt history records (analytics, burn rates, `history.json`) | `account_state` by `a, t`; `reading_day` and `latest` as above | per account and window |
| Hourly charts, burn over a span | `rollup_hours` range `a, w, day..=day`, then filter by hour | one value per day |
| Daily charts | `rollup_day` `a, w, d` | unchanged |
| Reset periods | `period` `a, w, t` | unchanged |
| By provider, list of series | `series_by_provider`, `series` | unchanged |
| Every change between two times, all accounts | no index: one range per series | scans `series`; no route asks this of all accounts except unfiltered raw history, which already walked every series |
| Oldest reading, first day to close | one forward seek per series on `reading_day`, or its `latest` | was one index read |
| Pruning | `reading_day` range `a, w, ..=day(cutoff)` per series | was an index range |

Medians on the 30-day store, same workload and machine, one run each, so differences under about 10% are
noise (the cold first call was not reliably different either):

| Query | Before | Now |
|---|---:|---:|
| Raw, 1 hour, 1 series | 0.53 ms | 0.47 ms |
| Raw, 1 hour, 1 account | 0.49 ms | 0.46 ms |
| Raw, 1 hour, all series | 1.03 ms | 1.06 ms |
| Raw, 1 day, 1 series | 0.52 ms | 0.48 ms |
| Raw, 30 days, 1 series | 2.73 ms | 2.31 ms |
| Raw, 30 days, all series (20,000 cap) | 27.1 ms | 23.1 ms |
| Hourly, 1 day, 1 series | 0.49 ms | 0.52 ms |
| Hourly, 30 days, all series | 26.4 ms | 25.1 ms |
| Daily, 30 days, all series | 3.42 ms | 2.96 ms |
| All series at time T | 0.98 ms | 1.06 ms |
| History 24 h, analytics thinning | 140 ms | 139 ms |
| History 31 days, analytics thinning | 2,033 ms | 2,185 ms |
| Burn 7, 30, 90 days, all series | 6.97 ms | 4.35 ms |
| Periods, all series | 14.3 ms | 13.2 ms |
| Oldest reading | 0.03 ms | 0.15 ms |
| One sampling tick written (16 accounts) | 11.0 ms | 9.5 ms |
| Reopen / rebuild redb from the logs | 104 ms / 27.3 s | 32 ms / 26.7 s |

**What packing costs.** A one-hour range does read a whole day's value, but here that is lost in the rest of
the call. The worst case built for it, a series changing every minute with non-round values (1,440 spans in
the value), answers a one-hour raw query in 0.19 to 0.25 ms across four runs, against 0.15 to 0.24 ms before;
no difference this measurement can separate. The clear slowdown is the oldest reading, five times slower and
still under a millisecond, because it lost its index. At 90 days the slowest results moved no more than at
30 (hourly all series 20.0 to 23.4 ms, 31-day history 2,145 to 2,044 ms).

**Same answers.** Each of the 16 queries' answer bytes are identical before and after, on the 30-day, the
non-round and the 90-day runs. A rebuild from the logs now gives the same answers too, except as below;
before, hourly rollups and long burn differed when samples arrived after their day closed, because a rebuild
kept hours the close had not listed. The close now replaces the day's whole hourly value.

**Known limits.**

- A rebuild restores readings past retention while their log segments still exist, until the next maintenance
  prunes them (the one 90-day difference, raw history at the cutoff; the old layout does the same).
- `late_rows` in a closed day's rollup returns to the closed value after a rebuild. A series whose readings on a
  day were all late has no entry in that day's close, so a rebuild keeps rollups for it. Neither case changed here.
- All of these are synthetic runs; the share of non-round percentages in real history was not measured.

### 11.5 Linux

The dashboard runs on Linux and needs `aac-store.linux-x64.node`. One command, on the VM or on Nas1 (same
or older glibc than the VM):

```bash
bun run store:build
```

It needs cargo with rustup (the crate's `rust-toolchain.toml` pins 1.99.1 and installs it), a C
compiler for zstd (`build-essential`), and glibc no newer than the VM's. No napi CLI is needed: cargo
builds the cdylib and the script copies it as `.node`. Copy `native/aac-store/prebuilt/` to the VM's
checkout when built elsewhere; the next `bun run build` puts it in `dist/native/`. Until then the VM's
dashboard keeps its JSON history.

Not yet run on Linux: the Rust suites, the kill test, folder fsync, and the first backfill of the VM's
real history and archive.

## 12. Analytics usage history (built)

Analytics' token and cost activity is kept in the store as well, beside the quota readings, by a second
projector, `activity` (`src/activity.rs`), with its own checkpoints and its own dictionary tables. This
replaces the phase 2 "activity hours" row of 1.1 and the `activity.hour` sketch of 2.2: the readers'
finest grain is kept (session-hour cells), not hourly totals per tool. Design, measurements and the
owner's direction: `share/ai-account-center/2026-11-13-analytics-history/DESIGN.md`.

### 12.1 The usage record contract

`src/contract.rs` (`aac.usage-report.v1`) is the one shape readers hand the store, also for a failed
read (a state and no cells, never zero rows). A **cell** is the absolute totals of one UTC hour for one
computer, tool, session, route, model, effort and account; that tuple is its dedupe key. A **session**
row is keyed by tool and session key; a mirrored log is one session. `loggedCostUsd` (what the tool
logged) is kept apart from `costUsd` and `fallbackCostUsd` (computed by the server). Per-message
records (`messages[]`) are reserved and refused with `not_supported` until their table is built.
On the dashboard's computer the usage scanner (`aac-usage-scan`) owns some tools, and their reports
are built from its rows alone, never the collector's as well: Cursor and Devin always (the collector has
no reader for them, so an empty scanner answer is their measured state), and Codex and the others once
the scanner takes them over (its answer's `local.owned`, or rows it sent); before that the collector
reads them (`buildActivityReports`).
Roots are diagnostics: a root names its `origin` (`default`, `env`, `tool_config`, `t3_instance`,
`ccs_instance`, `extra`, or the usage scanner's `signature` and `experiment`) and its state, which for a
root alone may be `link_unreadable` or `link_unreachable`. `newestMtime` is an ISO UTC time or epoch
milliseconds; a session's folder is at most 1,024 characters, home written as `~`.
Every label a report carries (model, route, account, target, folder) is checked with the shared AAC
secret policy (`aac_store::secret_policy`, [secret-policy.md](secret-policy.md)): a report holding a
credential or an email address is refused whole, before anything reaches the log.
Today's TypeScript collectors are adapted on the TypeScript side (`src/store/activity-reports.ts`,
`src/web-server/usage/activity-cells.ts`).

**Nothing after now.** The store clamps a report's `readAt` to its own clock, and a cell or session
dated more than five minutes after that clock (`FUTURE_SLACK_MS`) is not written; a session that
started before then ends there. Such records are counted, with the reader's own count
(`counts.futureDated`), in the read's scan state (`futureDated` per source), and the page says how
many. The TypeScript side drops and counts them before sending (`cellsFromCompact`,
`buildActivityReports`, `rowsReport`). Without this, one record dated ahead (a clock running fast, a
remote host's clock, a bad log line) moved the source's settle line past now and froze every live
write for that source.

**Quality flags.** A cell or session may carry `quality` words from `QUALITY_FLAGS`, kept with it for
good (cell value format 2; format 1 values read as clean). `outputIncludesReasoningTwice`: the output
count holds the reasoning tokens twice and cannot be split (12.8); input and cached input are exact.
`activityRange` returns the bits as a last row column (rows never mix flagged and clean cells) and
`quality` spans per tool. The page keeps such usage in its totals, leaves it out of Cost by model, the
model share, the model detail and the cost anomalies, and says so in one line for the range.

### 12.2 Tables and events

Tables `act_dict`, `act_dict_rev`, `act_hour` (one packed value per UTC day, computer and tool),
`act_session`, `act_session_by_last` (a covering index: first and last time and an interned
signature, so a range's sessions are counted without reading a row), `act_cover`, `act_source`,
`act_day_closed`, `act_scan`, `act_backfill`, and the volatile `act_live` (minute-precision last read,
written without a log event). Events `activity.cells` (readings, or ledger for a closed day),
`activity.read` (ledger, scan history), `activity.day_closed` (ledger, the whole truth of a UTC day,
74 hours after it ends) and `activity.backfill` (ledger). `registry.json` has every key and value.

### 12.3 Ingest rules

Per computer, tool and UTC hour, decided inside the write transaction:

| Hour | Report | Action |
|---|---|---|
| newer than the source's settle line (72 h before its last complete read), inside a complete report's `[coverFrom, readAt]` | live | **replace**: the hour's cells become exactly the report's (cells it no longer has are removed) |
| unsettled, otherwise | live partial, or any backfill or import | **raise**: the report's hour wins only with more tokens (or as many tied to more sessions); nothing is removed |
| behind the settle line | live | **leave**: a tool that deletes old logs never erases history |
| behind the settle line | backfill or import | raise |

Only changed cells are written. The same report twice writes nothing. The TypeScript ingest queue
leaves out days and sessions it already sent unchanged (`sameDays`). A complete read adds
`[coverFrom, readAt]` to `act_cover`; hours no complete read covered are gaps, shown as such.

### 12.4 Replay

Before a day closes only readings events touch it; its close replaces the whole day; later changes
to it are ledger events after the close. Replay (readings, then ledger) therefore rebuilds the same
tables; session merges are commutative. `retire_segments` also waits for the activity projector's
readings checkpoint and for the oldest open activity day.

### 12.5 Retention

Kept for good by default (DATA-ARCHITECTURE section 4, and the store's raw policy above: with
`retentionDays: 0` readings segments never retire, so the raw `activity.cells` changes stay too).
`AAC_ANALYTICS_RETENTION_DAYS` (90 to 36,500) is an explicit, opt-in retention: maintenance then deletes
day values, sessions (with their index rows), cover spans and scan rows before the cutoff. The ledger
keeps the closed days either way. The `activity` projector owns only `act_*` tables and its own
dictionaries; it shares nothing with `usage` or `capture`.

### 12.6 Queries

`activityRange` (cells summed per bucket, tool, route and model: hourly up to 92 days, else in the
caller's local days; session counts by signature and the newest sessions), `activitySources` (per
computer and tool: stored span, last read, state, backfill, covered spans; per computer the gaps in
the range), `activityOldest`, `activityCounts`, `activityIngest`, `activityBackfill`. The page's
numbers come from the existing projection fed with these rows, so they equal the in-memory answer
on the same data (`tests/unit/store/store-activity.test.ts`).

### 12.7 Measured (two years of real data shape)

The real 31 days in the copied caches (7,396 session-hour cells, 8,873 sessions across 30 computer
and tool results) repeated back to 756 days: 87,600 cells, 106,416 sessions. Store: redb 33.7 MB,
ledger 2.5 MB compressed. Activity part of an Analytics answer (store query plus projection), median
of 5 on the VM: 24H 11 ms, 7D 25 ms, 30D 49 ms, 90D 107 ms, two years 184 ms (store query 49 ms).
A minute pass with ten changed cells: 2.4 ms. Reopen: 0.5 s.

### 12.8 The retired Codex cache

`cache/codex-native-usage-v1.json` (version 1, last written 2026-10-01, no longer read by the code)
holds every Codex usage record the old collector read: on the dashboard VM 491,361 records from
2026-01-29 to 2026-10-01, and 5,150 of its 5,153 rollout files are gone, so it is the only copy. The
backfill's first step imports it once (`activityImportCodexV1`, `src/legacy.rs`): read as a stream one
rollout file at a time, folded into session-hour cells and sessions, raw ids hashed, paths dropped.
The version 1 parser counted two categories twice (fixed and the cache bumped to version 2 at
`6298e60ad`, 2026-10-01):

- **Input** included the cached input (in every one of the 490,042 records with cache reads, input is
  at least the cache reads), so uncached input is `inputTokens - cacheReadTokens - cacheCreationTokens`.
  Exact.
- **Output** is `output_tokens + reasoning_output_tokens`, although Codex's `output_tokens` already
  holds the reasoning: every reasoning token is counted twice. The cache keeps only the sum, so the
  split cannot be recovered (the three surviving rollout files hold no token events). Output is
  imported as it is, an upper bound, with no reasoning split, and every imported cell and session
  carries the quality flag `outputIncludesReasoningTwice` for good (12.1). The page notes "Codex output
  before Oct 2 counts reasoning twice (old cache)" for any range holding such hours and leaves them out
  of per-model cost and efficiency comparisons.

Every record has `target: codex` and no route, and the cache excluded proxy sessions, so all of it is
Codex-subscription usage (route empty, provider Codex). It carries no cost: the cells hold none and the
page prices them at today's rates, so their output cost is an upper bound too. The import is recorded
in the scan history as an `import` read with cause `retired_cache` and the cache's file count, and the
store marks the file's SHA-256 (`ts:activity_import_codex_v1:<first 32 hex>` holds the record count) so
the same file is never imported twice. The import is marked done only after the store committed it;
a failed or refused import is tried again on the next start. Measured on a copy: 4.6 s, 95 MB process
memory, 9,056 cells over 159 days, 4,891 sessions; a second run imports nothing.

**The file is kept.** It is the only copy of that history; nothing in AAC deletes it (not the import,
not the store's maintenance, not the DB audit's cleanup), and it must not be retired.

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

### Final review integrity and read guarantees

Backup publication verifies each live sealed segment's decoded SHA-256 against its manifest
(raw and zstd). Corrupt or missing segments refuse the backup. Existing pooled files, including
mirrors, are immutable: a differing copy refuses publication and leaves older sets restorable.

Long burn uses complete hourly buckets only in the interior of the requested interval. Partial
hours use retained reading spans for consumption and coverage; usage before the interval is
excluded. A change straddling the lower boundary without a measured baseline is unknown, not
an estimated number. Unknown history remains null.

Hourly and daily share exports enumerate all series and fetch bounded pages, checking each
page's truncation flag. A truncated page is split at a bucket boundary. If completeness cannot
be established, export generation fails before publication, preserving the last complete files.

Unreadable segments classify rebuild status as degraded with their sequence loss bounds.
Repeated integrity checks include known loss findings, so a later CLI check cannot print an
all-clear for previously detected corruption.
