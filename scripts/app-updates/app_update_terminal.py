"""Fresh idle CLI relaunches in real terminal contexts; no prompt replay."""

import json
import os
import pathlib
import re
import shlex
import shutil
import socket
import subprocess
import sys
import threading
import time
import uuid

from app_update_common import NO_AUTO_UPDATE, UpdateFailure, command
from app_update_processes import family, scan


UUID = re.compile(r"^[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$")


def resume_arguments(app_id, old_args):
    """An explicit already selected UUID may be reopened, never a prompt."""
    forms = {
        "antigravity-cli": ("--conversation",), "muse-code": ("resume",),
        "omp": ("--resume",), "codex-cli": ("resume",), "claude-code": ("--resume",),
    }
    for index, value in enumerate(old_args):
        if value in forms.get(app_id, ()) and index + 1 < len(old_args) and UUID.fullmatch(old_args[index + 1]):
            return [value, old_args[index + 1]]
    return []


def restart_environment(context):
    values = dict(context.env or os.environ)
    values.update({key: value for key, value in NO_AUTO_UPDATE.items() if key != "CODEX_NON_INTERACTIVE"})
    values.pop("MUSE_LAUNCHER_INSTALL", None)
    values.pop("MUSE_UPGRADE_MODE", None)
    return values


def check_terminal(platform, contexts):
    if not contexts:
        return
    if platform == "ubuntu" and not shutil.which("tmux"):
        raise UpdateFailure("restart_context")
    if platform == "windows":
        import ctypes
        session = ctypes.c_ulong()
        if not ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(session)) or any(item.session != session.value for item in contexts):
            raise UpdateFailure("restart_context")
        if not shutil.which("wt.exe"):
            raise UpdateFailure("restart_context")
    if platform == "mac" and not pathlib.Path("/System/Applications/Utilities/Terminal.app").is_dir():
        raise UpdateFailure("restart_context")


def _broker_terminal(install, context, args):
    root = pathlib.Path.home() / ".ccs/app-updates"
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    nonce = uuid.uuid4().hex
    endpoint = root / ("terminal-" + nonce + ".sock")
    windows_pipe = None
    if install.platform == "windows":
        from app_update_pipe import PrivatePipe
        windows_pipe = PrivatePipe(nonce)
        endpoint = windows_pipe.endpoint
        server = windows_pipe
    else:
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(endpoint))
        endpoint.chmod(0o600)
        server.listen(1)
        server.settimeout(25)
    packet = {"argv": [str(install.path), *args], "cwd": context.cwd, "env": restart_environment(context)}
    complete, failures = threading.Event(), []
    def serve():
        try:
            if windows_pipe:
                windows_pipe.send(json.dumps(packet, ensure_ascii=True).encode("utf-8") + b"\n")
            else:
                connection, _ = server.accept()
                with connection:
                    # Socket permissions are owner-only; the nonce is never returned
                    # through HTTP/logs. Environment data never touches a file.
                    connection.sendall(json.dumps(packet, ensure_ascii=True).encode("utf-8") + b"\n")
                    connection.shutdown(socket.SHUT_WR)
        except Exception:
            failures.append(True)
        finally:
            complete.set()
    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    child = pathlib.Path(__file__).with_name("app_update_terminal_child.py")
    script = None
    try:
        if install.platform == "mac":
            script = root / ("terminal-" + nonce + ".command")
            script.write_text("#!/bin/sh\nexec " + shlex.join(["/usr/bin/python3", str(child), "--socket", str(endpoint)]) + "\n", encoding="utf-8")
            script.chmod(0o700)
            command(["/usr/bin/open", "-a", "Terminal", str(script)], timeout=15)
        else:
            # Task Scheduler launches pythonw without a console. The terminal
            # child must use python.exe so the reopened CLI receives WT's PTY.
            console_python = pathlib.Path(sys.executable).with_name("python.exe")
            if not console_python.is_file():
                raise UpdateFailure("restart_context")
            command([shutil.which("wt.exe"), "-w", "0", "new-tab", "-d", context.cwd, "--", str(console_python), str(child), "--pipe", str(endpoint)], timeout=15)
        if not complete.wait(25) or failures:
            raise UpdateFailure("restart_failed")
    finally:
        server.close()
        if not windows_pipe:
            try: endpoint.unlink()
            except OSError: pass
        if script:
            try: script.unlink()
            except OSError: pass


def restart_cli(install, contexts):
    sessions = []
    check_terminal(install.platform, contexts)
    for index, context in enumerate(contexts):
        args = resume_arguments(install.app_id, context.args)
        if install.platform == "ubuntu":
            server = "ccs-updates-" + uuid.uuid4().hex[:12]
            session = "ccs-updated-" + install.app_id + "-" + str(index + 1)
            command([shutil.which("tmux"), "-L", server, "new-session", "-d", "-s", session,
                     "-c", context.cwd, shlex.join([str(install.path), *args])], timeout=15,
                    env=restart_environment(context), preserve_env=True)
            sessions.append({"kind": "tmux", "server": server, "session": session})
        else:
            _broker_terminal(install, context, args)
            sessions.append({"kind": "windows-terminal" if install.platform == "windows" else "terminal"})
    if contexts:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            live = family(install, scan(install.platform))
            ids = {item.pid for item in live}
            if len([item for item in live if item.ppid not in ids]) >= len(contexts):
                return sessions
            time.sleep(.2)
        raise UpdateFailure("restart_failed")
    return sessions
