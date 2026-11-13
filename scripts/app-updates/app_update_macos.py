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

from app_update_common import NO_AUTO_UPDATE, UpdateFailure, command, diagnostics, private_temporary


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
        # launchctl calls are plumbing: the row records the app's own command instead.
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise UpdateFailure("timeout")
        return command(argv, timeout=min(maximum, remaining), capture=capture, errors=errors, quiet=True)

    shown = {"muse-warm": "muse exec (GUI session)", "cursor-update": "cursor-agent update (GUI session)"}[action]

    domain = "gui/" + str(os.getuid())
    try:
        run(["/bin/launchctl", "print", domain])
    except UpdateFailure:
        raise UpdateFailure("mac_gui_unavailable") from None
    label = "com.ccs.app-updates." + action + "." + uuid.uuid4().hex
    service = domain + "/" + label
    with private_temporary() as temporary:
        error_path = temporary / "stderr"
        output_path = temporary / "stdout"
        plist = temporary / "agent.plist"
        plist.write_bytes(plistlib.dumps({
            "Label": label, "ProgramArguments": actions[action],
            "WorkingDirectory": str(temporary), "RunAtLoad": True,
            "ProcessType": "Interactive", "KeepAlive": False,
            "AbandonProcessGroup": False, "ExitTimeOut": 1,
            "StandardOutPath": str(output_path), "StandardErrorPath": str(error_path),
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
                    code = int(exited[1])
                    if code == 0:
                        diagnostics().ran(actions[action], 0, label=shown)
                        return
                    # The app's own words and exit code, so a refusal (Cursor's 401, a locked
                    # Keychain) is classified on the Mac as it is on the other computers.
                    text = "\n".join(part for part in (_tail(output_path), _tail(error_path)) if part)
                    diagnostics().ran(actions[action], code, text, label=shown)
                    lowered = text.lower()
                    if "keychain" in lowered or "errsecinteractionnotallowed" in lowered:
                        raise UpdateFailure("mac_keychain_locked", text, code)
                    raise UpdateFailure("update_failed", text, code)
                time.sleep(min(.2, max(0, deadline - time.monotonic())))
        finally:
            # launchd owns the process group. Bootout stops it and its children,
            # including a timed-out Muse warm, before its private cwd is removed.
            # Also attempt cleanup when bootstrap returned an ambiguous failure.
            try:
                command(["/bin/launchctl", "bootout", service], timeout=10, quiet=True)
            except UpdateFailure:
                # An exited/removed service has nothing left to stop. If it is
                # still registered, cleanup failed and the row must say so.
                try:
                    command(["/bin/launchctl", "print", service], timeout=5, quiet=True)
                except UpdateFailure:
                    pass
                else:
                    raise UpdateFailure("mac_gui_cleanup_failed") from None


def _tail(path, limit=65536):
    """The last bytes one of the agent's private output files holds, as text."""
    try:
        with pathlib.Path(path).open("rb") as stream:
            size = stream.seek(0, 2)
            stream.seek(max(0, size - limit))
            return stream.read(limit).decode("utf-8", "replace")
    except OSError:
        return ""
