#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ensure_root_deps() {
  echo "[deps] Syncing root dependencies"
  (cd "$ROOT_DIR" && bun install --frozen-lockfile)
}

ensure_root_deps
