#!/bin/bash
# DEVICE-RECIPE step 3: test dashboard on 127.0.0.1:18861 from a branch build, temporary home with the Status and
# Scoreboard fixtures, sign-in on (setup on first visit or by curl).
set -e
REPO=${REPO:-$HOME/aac-frame-device-2}
H=${H:-/tmp/aac-device-home}
cd "$REPO"; mkdir -p "$H"; R="$H/agent-reports"
if [ ! -d "$R/v1/writers" ]; then node --input-type=module -e "
  const { writeStatusFixture } = await import('./scripts/status-fixture.mjs'); writeStatusFixture('$R', { ccsDir: '$H/.ccs', withKinds: true });
  const { writeScoreboardFixture } = await import('./scripts/scoreboard-fixture.mjs'); writeScoreboardFixture('$R');"; fi
export CCS_HOME="$H" HOME="$H" USERPROFILE="$H" APPDATA="$H/AppData/Roaming" LOCALAPPDATA="$H/AppData/Local" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share" XDG_STATE_HOME="$H/.local/state" XDG_CACHE_HOME="$H/.cache" CODEX_HOME="$H/.codex" CLAUDE_CONFIG_DIR="$H/.claude" AAC_AGENT_REPORTS_DIR="$R" AAC_PM_MAIL_REGISTRY_DIR="$R/pm-mail-registry" AAC_STATUS_DOORBELL=off CCS_DASHBOARD_AUTH_ENABLED=1
exec node dist/ccs.js dashboard --no-open --host 127.0.0.1 --port 18861
