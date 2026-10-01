#!/usr/bin/env bash
# Compatibility entry for the retired upstream dev publication lane.
# Intentionally performs no npm, GitHub, webhook or git mutation.
set -euo pipefail
echo "[X] The upstream CCS development publication lane is retired." >&2
echo "[i] AI Account Center is maintained at https://github.com/sittingmongoose/ai-account-center." >&2
echo "[i] Use scripts/dev-install.sh to build and install this checkout locally." >&2
echo "[i] External package, release and image publication require separate authorization." >&2
exit 1
