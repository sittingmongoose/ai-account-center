#!/usr/bin/env bash
# Portable-crate check using aac-winbuild's shared slots and per-checkout Windows mirrors.
# aac-winbuild itself exposes only Slint/WASM build/check, so use its slot recipe for M1.
set -euo pipefail
TASK_ROOT=$(git rev-parse --show-toplevel)
TASK_NAME=$(basename "$TASK_ROOT")
TASK_HOST=${AAC_WINBUILD_HOST:?Set AAC_WINBUILD_HOST to an existing build-host SSH entry}
[[ "$TASK_NAME" =~ ^[A-Za-z0-9_-]+$ && "$TASK_HOST" =~ ^[A-Za-z0-9_.-]+$ ]] || exit 2
TASK_LOCKS="${XDG_CACHE_HOME:-$HOME/.cache}/aac-win"
mkdir -p "$TASK_LOCKS"
TASK_SLOT=''
for slot in 1 2 3; do
  exec 8>"$TASK_LOCKS/build-slot-$slot.lock"
  if flock -n 8; then TASK_SLOT=$slot; break; fi
done
if [[ -z "$TASK_SLOT" ]]; then
  echo 'All aac-winbuild slots are busy; portable check skipped.' >&2
  exit 75
fi
TASK_TEMP=$(mktemp -d)
TASK_SSH=(-o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30)
cleanup() {
  ssh "${TASK_SSH[@]}" "$TASK_HOST" "powershell -NoProfile -ExecutionPolicy Bypass -File aac-foundation-$TASK_NAME.ps1 -Name $TASK_NAME -Cleanup" || true
  rm -f "$TASK_TEMP/source.tgz"
  rmdir "$TASK_TEMP"
}
trap cleanup EXIT
cd "$TASK_ROOT"
git ls-files -co --exclude-standard -z | tar --null -T - -czf "$TASK_TEMP/source.tgz"
scp -q "${TASK_SSH[@]}" "$TASK_TEMP/source.tgz" "$TASK_HOST:aac-foundation-$TASK_NAME.tgz"
scp -q "${TASK_SSH[@]}" scripts/foundation/check-windows.ps1 "$TASK_HOST:aac-foundation-$TASK_NAME.ps1"
ssh "${TASK_SSH[@]}" "$TASK_HOST" "powershell -NoProfile -ExecutionPolicy Bypass -File aac-foundation-$TASK_NAME.ps1 -Name $TASK_NAME"
