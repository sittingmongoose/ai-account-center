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
