#!/bin/sh
set -eu

# Retired compatibility path. The canonical image runs the dashboard directly.
# No supervisor, provider login, credential copy, or ownership rewrite occurs.
cd /app
exec /usr/local/bin/ai-account-center-entrypoint "$@"
