# Claude history copy: uncertain append hold, microbatches and what stays behind

## Uncertain append hold

Before the existing SSH append call, the service creates and fsyncs one owned 0600 nonce marker inside an owned 0700 `claude-history-pending` directory in the existing private CCS root. This file contains only fixed profile/platform IDs, a nonce and timestamps. It contains no account credentials, descriptor content, transcript or identity UUID.

The fixed helper acknowledges `writerQuiescent:true` only after its direct Node subprocess has finished or Python has killed **and waited for** that child on its own timeout. A successful SSH transport response alone is insufficient: the append reply must also be canonical, well-formed and explicitly acknowledged. A lost/malformed reply retains the pending marker. Open checks the durable marker before optional-policy lookup and again after synchronization. A second click, missing policy, changed policy or backend restart never clears it.

Completion updates only the held, revalidated inode to `finished`; it does not unlink or overwrite a replacement path. An operation never finishes another operation's marker. Unknown, pending, nonprivate, linked, empty or malformed state holds Open. The implementation is intended for the current POSIX backend; it does not claim Windows ACL verification for the backend marker directory.

This is a conservative hold for an uncertain metadata writer. It is not a distributed conversation-writer lease, a profile close, a queue, a restart, rollback proof or zero-write claim. It does not prevent the owner from deliberately deleting or replacing private state.

Manual recovery requires separately reviewed, authoritative proof that the exact remote append mutator and its creating helper can no longer write. Local SSH exit, elapsed time, a closed Claude profile, a removed policy or service restart is not that proof. First preserve the pending file's private bytes, nonce and device/inode binding, plus any remote private transaction snapshots; keep private data out of public receipts. Only after that proof may a separately authorized recovery mark the exact still-owned marker finished through its held/revalidated inode. Foreign replacements remain untouched. No clearing endpoint, automatic deletion, process stop or retry was added.

## Microbatches: one record per transaction, one marker per Open

A full 18-record append overran the 25-second Node deadline, so the copy runs one record per transaction (`MICROBATCH_RECORDS = 1`). Each transaction keeps every guard check and the same deadlines (25 s for Node, 30 s for SSH). Between transactions, a fresh read of the target must show exactly the original rows plus the rows confirmed so far.

One Open uses one marker file. It is armed before the first batch, finished by each batch's trusted terminal receipt, and returned to pending (`rearm()`) right before the next batch. It is pending only while an append may be in flight. A rearm interrupted after it truncated the file leaves an empty file, which holds Open like any malformed marker.

## The per-Open bound

One Open copies for at most `OPEN_COPY_BUDGET_MS` (45 s, counted from the start of the Open's history work) and at most `MAX_BATCHES_PER_OPEN` (50) batches. The bound is checked only between batches, after the last batch's receipt finished the marker, so stopping holds nothing. The copy then returns `status: 'partial'`, `reason: 'open_copy_budget_reached'`, the confirmed `createdCount` and a `remainingCount`. Claude opens as usual, and `openOperation` ends `opened` with `confirmedCount` below `totalCount`. The next Open plans only the records still missing. In-process callers may tighten the bound (tests do), never loosen it. An unreadable clock counts as an exhausted budget.

Each one-record transaction costs four helper calls over SSH: a closed check, a protected check, the append, then a fresh read of the target. The real Mac and Windows costs are unmeasured; only a local synthetic run exists (4.6-4.8 s per transaction against the 25 s deadline).

## Clean stops before an append

These stops happen before a batch's marker is armed or re-armed, and before any append runs. They return a clean refusal (`recoveryRequired: false`), keep the count of records already confirmed, log one line and let Claude open.

| Reason | When | Log event |
|---|---|---|
| `history_policy_changed` | The private policy was changed or removed during the copy, for example to stop a long copy. | `claude.history.copy_stopped` (info) |
| `history_marker_store_full` | The marker store already holds `MARKER_LIMIT` (256) files. Nothing is copied. | `claude.history.marker_store_full` (warn) |
| `history_marker_unavailable` | The private marker storage cannot be verified. | `claude.history.copy_stopped` (info) |

A stop at the per-Open bound logs `claude.history.copy_budget_reached` (info). Log lines carry the platform, counts and the fixed reason only, never profile ids, paths, titles or errors.

## What a copy leaves behind (and retention)

**Snapshot folders on the target computer.** The pinned atomic writer (`history_index_transaction_v1.cjs`, sha256 `0b3b4cbb...`) creates one `.history-index-snapshot-<uuid>/` folder in the target profile root for every transaction and never removes it on success. The folder holds private copies of the profile's `config.json`, `ssh_configs.json` and `ssh-remote-server-state.json` (`protected-<n>.bin`) and a `snapshot-manifest.json`.
- Each record is its own transaction, so **a copy of N records leaves N snapshot folders**: 18 for a first Gmail copy and 32 for a first Platyr copy.
- Claude Desktop's `config.json` can hold its encrypted sign-in token cache, so each folder is one more copy of it. The folders are private through 0700/0600 modes on the Mac and through the inherited folder ACL on Windows.
- A test in `microbatch-sync.test.cjs` pins the count. The per-Open bound caps how many one Open can add (50).

**Retention removes them through the pinned helper path.** The fixed helper's `snapshot-cleanup` mode (transport allow-list, same fixed profile IDs and interpreters, no paths from HTTP) keeps the newest 3 snapshot folders per profile and deletes older ones the writer created:
- only folders named exactly `.history-index-snapshot-<32 hex>` are candidates, and only when every entry inside is the manifest or a `protected-<n>.bin` regular file (single link, never a symlink); anything else is skipped and reported, never deleted;
- it re-checks the names right before deleting, unlinks the verified files and removes the folder with a plain `rmdir`, which fails safely if anything else appeared;
- retention runs automatically after each copy that created records unless Settings turns it off, and on demand from Settings' "Clean up now" (`POST /api/claude/history-snapshots/cleanup`), which covers every known profile on each of its platforms;
- the writer itself is untouched, so its sha256 pin stays `0b3b4cbb...`; the helper stays under its 128 KB transport check.

A measured, platform-qualified batch size above 1 would also cut the count.

**Finished markers.** Finished marker files are never removed. With one marker per Open, the 256 slots count Opens that copied, not copied records. When the store is full, every later copy stops with `history_marker_store_full` and a warning log line; Open still works. A cleanup needs a separately reviewed rule: remove only this server's own `finished` markers older than a retention window, through the held or revalidated inode, and never a pending, malformed or foreign file.

## Test scope

All fixtures are offline and synthetic. The delayed-preparation/lost-reply scenario was constructed with an owned local Node child, not observed on the user's SSH endpoints. Existing native/source confirmation guards remain unchanged. The initial 50 project/SSH descriptors are coverage, not a guaranteed successful transfer; 28 other entries remain outside this mapping, and actual SSH/NTFS/ACL/timing capacity still needs separate acceptance.
