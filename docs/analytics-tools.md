# Analytics tool and computer vocabulary

`src/web-server/usage/analytics-tools.ts` owns the wire tool names and the
experiment-discovery subset. Collector kinds, remote requests, remote source
states and worker payloads derive their types from that registry. A new tool
must have a worker payload: the mapped request union enforces that at compile
time. `jsonl` and `experiment-roots` are explicit collection work, not tools;
retired `droid` is not added to the tool list. Source display ordering and saved
folder formats keep their existing values, including `claude-code` in saved
folder preferences.

Computers remain in the existing `dashboard-hosts.ts` registry.
`DashboardPlatform` aliases its `HelperPlatform`, and `helperPlatform(host)`
derives the persisted platform from each computer's OS. Linux hosts retain
`ubuntu`; Mac and Windows retain `mac` and `windows`. No API, DTO or saved-file
value changes. `sourceComputer` resolves the host of existing saved sources.

This is U4 only. Collector/service splitting (U2/U5) and failover restructuring
(U7) are cancelled by the 2026-11-13 decision.

Native scanner coverage includes every existing Analytics tool on the dashboard
computer as well as remote computers. Each local collector remains active until
the scanner completes that tool's first read; binary failure or excess roots
returns ownership to the collector. This preserves totals during takeover.

A completed read is distinct from current usage coverage. Activity is dated only
by entries that carry usage: the newest measured event clock across the kind's
files. A log file, SQLite database or Cursor chat store written or touched
without a usage entry, a tool install or update, a login/token rewrite and a T3
provider-thread record are installation or metadata signals, never activity
dates, so they never make a source look stale. A source with no measured usage
reports no activity date honestly (no usage, not stale). Activity more than one
five-minute scan cycle newer than measured data still produces a `stale` source
with both dates when a caller supplies such evidence; the native scanner itself
keeps both clocks at the newest usage event. Activity is not converted into
tokens or cost. Coverage completeness comes from the usage read itself:

- A partial read, unreadable logs or folders, or a bounded Cursor chat listing
  leaves current coverage unverified.
- A usage-shaped OMP record the reader cannot decode (a torn append, a usage
  envelope with unusable numbers) flags that file's print; the measured date
  stands, the source reports unverified coverage, and the flag is carried
  through unchanged rescans and cleared by a verified read. Header-only probes
  and ordinary metadata records stay clean.
- A write-ahead log the SQLite open could not see (`walUnread`) makes every
  reader that emits the flag unverified on that scan, not merely annotated. A
  database whose last print said `walUnread` is read again even when its bytes
  are unchanged, so a log that became readable is counted.
- Antigravity's untimed step falls back to the database's file time for its
  numeric aggregation only. That estimate never enters the measured freshness
  clock; the numbers are counted and the missing event date is disclosed.

Old fingerprints are read once to establish their measured clocks.

A source's measured numbers can stand while its coverage does not. A
usage-shaped record the reader could not decode is named on the file's
fingerprint (`unreadUsage`): the tokens already measured stay, the source reports
`unknown`, and the page says its tokens are not counted until a later scan reads
it. A number the reader could only date from a file time (Antigravity's untimed
step) stays counted, and only logged event times enter the measured clock; the
source carries `estimatedClock` and the page says those dates are not usage
dates. Both disclosures survive unchanged rescans until a verified read clears
them.

Cursor SDK run stores remain under `.cursor/projects/*/sdk-agent-store/*/index.db`
on all operating systems. Current CLI chat roots include `.cursor/chats`,
`$XDG_CONFIG_HOME/cursor/chats` (default `.config/cursor/chats`) and the native
application-data `cursor/chats` directory. Their blobs/meta schema exposes no
numeric usage columns: discovery reports the files and activity, with their
usage unmeasured. SDK stores retain their measured model and token totals; cost
is absent from the observed SDK usage schema and is not invented.

Desktop app installation is reported separately for Claude desktop, Codex app,
Cursor IDE and Windsurf. Presence does not establish that their logs expose
usage or an account identity. Quota burn, shared tool totals and process/T3
activity cannot prove per-account token attribution.

zcode requests use SQLite `mode=ro`, including committed WAL records, instead of
forcing `immutable=1` and silently reading an older checkpoint. No database or
WAL write queries are issued. SQLite may maintain its shared-memory index, as
in the existing local reader. An explicit immutable scanner request remains
supported and reports its unread WAL; fingerprints distinguish that read from
a later WAL-visible read so unchanged file sizes do not suppress the upgrade.
