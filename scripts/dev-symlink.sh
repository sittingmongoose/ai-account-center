#!/usr/bin/env bash
# Temporarily point an existing AI Account Center command at this local build.
# Usage: ./scripts/dev-symlink.sh [--restore] [--command ai-account-center|ccs]
set -euo pipefail

RESTORE=false
COMMAND_NAME=ai-account-center
while [ "$#" -gt 0 ]; do
  case "$1" in
    --restore) RESTORE=true; shift ;;
    --command)
      if [ "$#" -lt 2 ]; then echo "[X] --command requires a name." >&2; exit 1; fi
      COMMAND_NAME="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: $0 [--restore] [--command ai-account-center|ccs]"
      echo "Replaces one installed command with this local build, retaining its backup."
      exit 0 ;;
    *) echo "[X] Unknown option: $1" >&2; exit 1 ;;
  esac
done
case "$COMMAND_NAME" in
  ai-account-center|ccs) ;;
  *) echo "[X] Only AI Account Center and its ccs compatibility command are supported." >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
GLOBAL_PATH=$(command -v "$COMMAND_NAME" 2>/dev/null || true)
if [ -z "$GLOBAL_PATH" ] && [ "$RESTORE" = true ]; then
  # command -v may omit a dangling link after the local build was removed.
  IFS=: read -r -a SEARCH_DIRS <<< "$PATH"
  for SEARCH_DIR in "${SEARCH_DIRS[@]}"; do
    CANDIDATE="${SEARCH_DIR:-.}/$COMMAND_NAME"
    if [ -L "$CANDIDATE" ] && { [ -f "${CANDIDATE}.backup-dev" ] || [ -L "${CANDIDATE}.backup-dev" ]; }; then
      GLOBAL_PATH="$CANDIDATE"
      break
    fi
  done
fi
if [ -z "$GLOBAL_PATH" ]; then
  echo "[X] No installed $COMMAND_NAME command was found." >&2
  echo "Build/install this checkout with scripts/dev-install.sh first." >&2
  exit 1
fi
BACKUP_PATH="${GLOBAL_PATH}.backup-dev"

# Restore remains available even when the local build was removed.
if [ "$RESTORE" = true ]; then
  if [ ! -f "$BACKUP_PATH" ] && [ ! -L "$BACKUP_PATH" ]; then
    echo "[X] No command backup found at $BACKUP_PATH" >&2
    exit 1
  fi
  if [ ! -L "$GLOBAL_PATH" ] || [ "$(readlink "$GLOBAL_PATH")" != "$(pwd)/dist/ccs.js" ]; then
    echo "[X] The installed command no longer points to this checkout; restore it deliberately from $BACKUP_PATH." >&2
    exit 1
  fi
  rm -f "$GLOBAL_PATH"
  cp -P "$BACKUP_PATH" "$GLOBAL_PATH"
  chmod +x "$GLOBAL_PATH"
  rm -f "$BACKUP_PATH"
  echo "[OK] Restored $COMMAND_NAME."
  exit 0
fi

DEV_PATH="$(pwd)/dist/ccs.js"
if [ ! -f "$DEV_PATH" ]; then
  echo "[X] Local build is missing. Run bun run build first." >&2
  exit 1
fi
if [ -L "$GLOBAL_PATH" ] && [ "$(readlink "$GLOBAL_PATH")" = "$DEV_PATH" ]; then
  echo "[OK] $COMMAND_NAME already points to this build."
  exit 0
fi
if [ ! -f "$BACKUP_PATH" ] && [ ! -L "$BACKUP_PATH" ]; then
  cp -P "$GLOBAL_PATH" "$BACKUP_PATH"
fi
rm -f "$GLOBAL_PATH"
ln -s "$DEV_PATH" "$GLOBAL_PATH"
echo "[OK] $COMMAND_NAME points to this local build."
echo "Open with: $COMMAND_NAME dashboard"
echo "Restore with: $0 --restore --command $COMMAND_NAME"
