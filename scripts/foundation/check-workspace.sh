#!/usr/bin/env bash
# Run on Nas1 or the Mac only, under nice -n 19 with CARGO_BUILD_JOBS=2.
set -euo pipefail
if [[ "${AAC_NO_LOCAL_RUST_BUILD:-}" == 1 ]]; then
  echo 'Rust builds and checks are disabled on this machine.' >&2
  exit 2
fi
if [[ -r /proc/meminfo ]]; then
  awk '/MemAvailable:/ { if ($2 < 8 * 1024 * 1024) exit 1 }' /proc/meminfo || {
    echo 'Need at least 8 GiB available before a foundation check.' >&2
    exit 75
  }
fi
export CARGO_BUILD_JOBS=2
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
cargo check --locked --target wasm32-unknown-unknown -p aac-core -p aac-protocol
