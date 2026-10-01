#!/usr/bin/env bash
set -euo pipefail

cd /app
umask 077
ccs_home_dir="${CCS_DIR:-${CCS_HOME_DIR:-${CCS_HOME:-/home/node}/.ccs}}"
export CCS_DIR="$ccs_home_dir"

mkdir -p "$ccs_home_dir"

# Never rewrite ownership or authentication state from an older installation.
if [ ! -r "$ccs_home_dir" ] || [ ! -w "$ccs_home_dir" ]; then
  echo "[X] Account state directory is not readable and writable by runtime UID $(id -u): $ccs_home_dir" >&2
  echo "    Review the existing volume ownership or explicitly select its existing runtime UID." >&2
  exit 1
fi

# Match the image's dashboard default when invoked without a command.
if [ "$#" -eq 0 ]; then
  set -- ai-account-center dashboard --host 0.0.0.0 --port 3000 --no-open
fi

exec "$@"
