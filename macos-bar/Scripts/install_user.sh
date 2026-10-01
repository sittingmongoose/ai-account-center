#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="$ROOT/dist/CCS Bar.app"
TARGET="$HOME/Applications/CCS Bar.app"
if [[ ! -d "$SOURCE" ]]; then
  echo 'Build first with Scripts/package_app.sh.' >&2
  exit 1
fi
/usr/bin/codesign --verify --deep --strict "$SOURCE"
mkdir -p "$HOME/Applications"
LAUNCH_LABEL='party.sittingmongoose.ccs.accounts-bar'
LAUNCH_DOMAIN="gui/$(id -u)"
if /bin/launchctl print "$LAUNCH_DOMAIN/$LAUNCH_LABEL" >/dev/null 2>&1; then
  /bin/launchctl bootout "$LAUNCH_DOMAIN/$LAUNCH_LABEL"
fi
/usr/bin/python3 - "$TARGET" <<'PY'
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
executable = str(Path(sys.argv[1]) / 'Contents/MacOS/CCSBar')
rows = subprocess.check_output(['ps', '-axo', 'pid=,comm='], text=True).splitlines()
for row in rows:
    parts = row.strip().split(None, 1)
    if len(parts) != 2 or parts[1] != executable:
        continue
    pid = int(parts[0])
    os.kill(pid, signal.SIGTERM)
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        raise SystemExit('The current CCS Bar did not stop; installation was left unchanged.')
PY
if [[ -e "$TARGET" ]]; then
  BACKUPS="$HOME/Library/Application Support/CCS Bar/Backups"
  mkdir -p "$BACKUPS"
  /usr/bin/ditto "$TARGET" "$BACKUPS/CCS Bar-$(date -u +%Y%m%dT%H%M%SZ).app"
fi
/usr/bin/ditto "$SOURCE" "$TARGET"
/usr/bin/codesign --verify --deep --strict "$TARGET"

# Start on the user's next graphical sign-in; credentials are never written here.
mkdir -p "$HOME/Library/LaunchAgents"
/usr/bin/python3 - "$TARGET" <<'PY'
from pathlib import Path
import plistlib
import sys
target = Path(sys.argv[1])
agent = Path.home() / 'Library/LaunchAgents/party.sittingmongoose.ccs.accounts-bar.plist'
agent.write_bytes(plistlib.dumps({
    'Label': 'party.sittingmongoose.ccs.accounts-bar',
    'ProgramArguments': [str(target / 'Contents/MacOS/CCSBar')],
    'RunAtLoad': True,
    'ProcessType': 'Interactive',
    'LimitLoadToSessionType': 'Aqua',
}))
PY
echo "Installed: $TARGET"
if [[ "${1:-}" == '--launch' ]]; then
  /bin/launchctl print "$LAUNCH_DOMAIN" >/dev/null
  /bin/launchctl enable "$LAUNCH_DOMAIN/$LAUNCH_LABEL"
  /bin/launchctl bootstrap "$LAUNCH_DOMAIN" "$HOME/Library/LaunchAgents/$LAUNCH_LABEL.plist"
  echo 'Launched CCS Bar through its interactive GUI launch agent.'
fi
