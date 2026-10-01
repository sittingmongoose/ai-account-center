#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="$ROOT/dist/AI Account Center.app"
if [[ ! -d "$SOURCE" ]]; then
  echo 'Build first with Scripts/package_app.sh.' >&2
  exit 1
fi
if [[ "${1:-}" == '--launch' ]]; then
  exec /usr/bin/python3 "$ROOT/Scripts/install_bundle.py" --source "$SOURCE" --launch
fi
exec /usr/bin/python3 "$ROOT/Scripts/install_bundle.py" --source "$SOURCE"
