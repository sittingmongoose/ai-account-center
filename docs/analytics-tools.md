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

A completed read is distinct from current usage coverage. The scanner retains
per-file measured event clocks and compares them with independent log/WAL,
installation and T3 projection activity. Activity more than one five-minute scan
cycle newer than measured data produces a `stale` source with both dates.
Activity is not converted into tokens or cost. An incomplete activity check is
unverified. Old fingerprints are read once to establish their measured clocks.

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
