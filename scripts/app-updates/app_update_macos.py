"""Short-lived, fixed LaunchAgents in the owner's GUI bootstrap session.

An SSH process, including `launchctl asuser`, can retain its SSH security
session. Launching through gui/<uid> gives Keychain clients the login session.
No credential files or Keychain settings are read, copied or changed here.
"""

import os
import pathlib
import plistlib
import re
import time
import uuid

from app_update_common import NO_AUTO_UPDATE, UpdateFailure, command, private_temporary


MUSE_WARM_PROMPT = "Reply with the word: ok"


def gui_command(action, timeout):
    """Only these two package-owned operations can run; no supplied argv or path."""
    home = pathlib.Path.home()
    actions = {
        "muse-warm": [str(home / ".local/bin/muse"), "exec", "--contributor-mode", "off", MUSE_WARM_PROMPT],
        "cursor-update": [str(home / ".local/bin/cursor-agent"), "update"],
    }
    if action not in actions:
        raise UpdateFailure("unsupported")
    deadline = time.monotonic() + timeout

    def run(argv, maximum=5, capture=False, errors=False):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise UpdateFailure("timeout")
        return command(argv, timeout=min(maximum, remaining), capture=capture, errors=errors)

    domain = "gui/" + str(os.getuid())
    try:
        run(["/bin/launchctl", "print", domain])
    except UpdateFailure:
        raise UpdateFailure("mac_gui_unavailable") from None
    label = "com.ccs.app-updates." + action + "." + uuid.uuid4().hex
    service = domain + "/" + label
    with private_temporary() as temporary:
        error_path = temporary / "stderr"
        plist = temporary / "agent.plist"
        plist.write_bytes(plistlib.dumps({
            "Label": label, "ProgramArguments": actions[action],
            "WorkingDirectory": str(temporary), "RunAtLoad": True,
            "ProcessType": "Interactive", "KeepAlive": False,
            "AbandonProcessGroup": False, "ExitTimeOut": 1,
            "StandardOutPath": "/dev/null", "StandardErrorPath": str(error_path),
            "EnvironmentVariables": dict(NO_AUTO_UPDATE),
        }))
        plist.chmod(0o600)
        try:
            try:
                run(["/bin/launchctl", "bootstrap", domain, str(plist)])
            except UpdateFailure:
                raise UpdateFailure("mac_gui_unavailable") from None
            while True:
                try:
                    output = run(["/bin/launchctl", "print", service], capture=True)
                except UpdateFailure as error:
                    if error.code == "timeout":
                        raise
                    # A vanished service alone is not evidence of logout.
                    # Probe the GUI domain to distinguish session loss.
                    try:
                        run(["/bin/launchctl", "print", domain], errors=True)
                    except UpdateFailure as domain_error:
                        if domain_error.code == "timeout":
                            raise
                        if "could not find domain" in domain_error.output.lower():
                            raise UpdateFailure("mac_gui_unavailable") from None
                        raise error
                    raise error
                state = re.search(r"^\s*state = (.+)$", output, re.M)
                exited = re.search(r"^\s*last exit code = (\d+)", output, re.M)
                if state and state[1].strip() in ("not running", "exited") and exited:
                    if int(exited[1]) == 0:
                        return
                    try:
                        with error_path.open("rb") as stream:
                            diagnostic = stream.read(65536).decode("utf-8", "replace").lower()
                    except OSError:
                        diagnostic = ""
                    if "keychain" in diagnostic or "errsecinteractionnotallowed" in diagnostic:
                        raise UpdateFailure("mac_keychain_locked")
                    raise UpdateFailure()
                time.sleep(min(.2, max(0, deadline - time.monotonic())))
        finally:
            # launchd owns the process group. Bootout stops it and its children,
            # including a timed-out Muse warm, before its private cwd is removed.
            # Also attempt cleanup when bootstrap returned an ambiguous failure.
            try:
                command(["/bin/launchctl", "bootout", service], timeout=10)
            except UpdateFailure:
                # An exited/removed service has nothing left to stop. If it is
                # still registered, cleanup failed and the row must say so.
                try:
                    command(["/bin/launchctl", "print", service], timeout=5)
                except UpdateFailure:
                    pass
                else:
                    raise UpdateFailure("mac_gui_cleanup_failed") from None
