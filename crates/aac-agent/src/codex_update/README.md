# Codex update runtime

P2 replaces the process-only closure rooted at `codex-update-runtime-entry.ts`.
It provides a Rust API for the Linux updater; W2/W6 wire the updater, and TS3
retires the old source and build references after that integration is proven.
No account activation or authentication-file replacement is included.

```rust,ignore
use aac_agent::codex_update::{Native, Runtime};
let backend = Native::new(codex_home.clone(), updated_cli);
let mut runtime = Runtime::new(codex_home, user_home, backend);
runtime.stop(|plan| record_desktop_obligations(plan)).await?;
// Run the authorized installer here, preserving the runtime in memory.
let restarted = runtime.start().await;
runtime.dispose().await;
restarted?;
```

Record the stop plan in the callback before any signal; callback failures close
nothing. Always call `dispose` when abandoning a transaction. Dropping the native
backend also releases its file locks. A failed installer must still attempt
`start` before disposal. Keep the desktop update marker in the updater: this
runtime returns obligations, but never persists a launch context.

After start, callers can compare `verify_running_daemon` with `restart_state`
by both PID and start ticks. A replaced daemon needs its own readiness check.
`retired_plan` counts desktops that this runtime did not relaunch and see ready.
Fresh terminal CLIs are left to the terminal updater; unowned stdio app servers
refuse the shared daemon update. Private environment snapshots have no Debug or
Serialize implementation and all runtime errors use fixed, sanitized messages.

The Chromium, desktop-detection and SHA-256 fingerprint oracle in `oracle.json`
contains 512 cases generated with the original TypeScript functions at
`fd55e8301`. Tests also exercise the stop/restart transaction, private local
WebSocket RPC fixtures, rollout lifecycle files and native startup flock files.
No live provider, installed app, or real account storage is required.
