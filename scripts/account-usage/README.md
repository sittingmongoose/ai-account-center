# Account usage browser-bridge collectors

Dashboard account collection runs in the Rust agent. This folder retains only the Python modules
used by the native-messaging browser bridges:

- `muse_console.py`: the Muse browser-session reader installed by
  `browser-bridge/opencode-muse/install-macos.py`.
- `plan_common.py` and `plan_usage.py`: the Qwen reader installed by
  `browser-bridge/qwen/install-native-host.ps1`.

Do not add dashboard fallback helpers here. Provider quota reads belong in `crates/aac-agent` and
`crates/aac-providers`; these bridge modules keep their existing public sample and credential
boundaries.
