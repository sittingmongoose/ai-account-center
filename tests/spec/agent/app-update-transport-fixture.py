#!/usr/bin/python3
"""L5 recorder and agent stand-in for the Rust app-update service test.

The fake SSH client verifies that L5 reuses the control path. It opens no
socket and invokes no installer.
"""
import base64
import json
import os
from pathlib import Path
import re
import shlex
import sys
import time

ROOT = Path(__file__).resolve().parent
HOSTS = {
    "ubuntu": ("ubuntu", "x86_64-unknown-linux-gnu"),
    "jared-mac": ("mac", "aarch64-apple-darwin"),
    "jared-windows": ("windows", "x86_64-pc-windows-msvc"),
    "nas1-agent": ("ubuntu", "x86_64-unknown-linux-gnu"),
    "alexis-windows": ("windows", "x86_64-pc-windows-msvc"),
    "odroid": ("ubuntu", "x86_64-unknown-linux-gnu"),
}
APPS = (
    "antigravity-cli", "muse-code", "omp", "codex-cli", "codex-desktop", "claude-code",
    "claude-desktop", "zcode", "t3-acp-adapters", "t3-code", "cursor-cli", "antigravity-acp",
)


def trace(host, stage, warm=False):
    entry = {"host": host, "stage": stage, "warm": warm, "at": time.monotonic_ns()}
    with (ROOT / f"trace-{host}.jsonl").open("a", encoding="ascii") as out:
        out.write(json.dumps(entry, separators=(",", ":")) + "\n")


def messages(host, request):
    os_name = HOSTS[host][0]
    token = request["args"]["jobId"]
    marker = ROOT / f"{host}-{token}.active"
    flag = ROOT / f"{host}-{token}.cancel"

    def emit(kind, **fields):
        record = {"type": kind, "protocol": 1, "id": request["id"], **fields}
        print(json.dumps(record, separators=(",", ":")), flush=True)

    action = request["args"]["action"]
    if action == "cancel":
        active = marker.exists()
        if active:
            flag.touch()
        emit("result", value={"cancelled": active})
        return

    marker.touch()
    emit("progress", data={"event": "app", "appId": "codex-desktop", "phase": "downloading"})
    if (ROOT / "hold").exists():
        until = time.monotonic() + 8
        while not flag.exists() and time.monotonic() < until:
            time.sleep(0.01)

    rows = []
    for app in APPS:
        status, code, extra = "not_installed", "not_installed", {}
        if app == "codex-desktop":
            status, code = "updated", "updated"
            manager = {"ubuntu": "apt", "mac": "official-download", "windows": "msix"}[os_name]
            extra = {"version":"3.4.5", "manager":manager, "updateAttempted":True}
        elif app == "antigravity-cli":
            status, code = "held", "held_for_review"
            extra = {"heldVersion":"1.5.9", "version":"1.5.8"}
        elif app == "t3-acp-adapters":
            status, code = "action_required", "in_use"
            extra = {"parts":[{"name":"muse-acp","version":"0.21.0","inUse":True}]}
        elif flag.exists() and app == "t3-code":
            status, code = "skipped", "skipped_cancelled"
        row = {"appId":app,"platform":os_name,"status":status,"messageCode":code,**extra}
        rows.append(row)
        emit("progress", data={"event":"result","result":row})
    emit("result", value={"results": rows})
    marker.unlink(missing_ok=True)


def main():
    if Path(sys.argv[0]).name == "ssh":
        if "-G" in sys.argv[1:]:
            return
        assert "--" in sys.argv
        host, command = sys.argv[-2:]
        assert host in HOSTS and host != "ubuntu", host
        control = next((arg for arg in sys.argv if arg.startswith("ControlPath=")), None)
        assert "ControlMaster=auto" in sys.argv and control, sys.argv
        # Distinct host master and same control path for its subsequent channels.
        marker = ROOT / f"master-{host}"
        warm = marker.exists()
        if not warm:
            marker.write_text(control, encoding="ascii")
        else:
            assert marker.read_text(encoding="ascii") == control
        if command.startswith("powershell.exe"):
            command = base64.b64decode(command.rsplit(" ", 1)[-1]).decode("utf-16-le")
            wrapped = re.search(r"'-EncodedCommand' '([A-Za-z0-9+/=]+)'", command)
            assert wrapped, "missing Windows fixture launcher"
            command = base64.b64decode(wrapped[1]).decode("utf-16-le")
            stage = re.search(r"\$mode='(probe|run)'", command)
        else:
            stage = re.search(r"case '(probe|run)'", shlex.split(command)[-1])
        assert stage, command
        stage = stage.group(1)
        trace(host, stage, warm)
    else:
        host = "ubuntu"
        stage = "probe" if sys.argv[1:] == ["--version"] else "run"
        trace(host, stage)
    if stage == "probe":
        if host == "jared-windows" and (ROOT / "missing-windows-agent").exists():
            sys.exit(87)
        print(json.dumps({"name":"aac-agent","version":"0.1.0","protocol":1,"target":HOSTS[host][1]}))
    else:
        if host == "jared-windows" and (ROOT / "hold-windows-connection").exists():
            trace(host, "connecting", True)
            until = time.monotonic() + 5
            while not (ROOT / "release-windows-connection").exists() and time.monotonic() < until:
                time.sleep(0.01)
        request = json.loads(sys.stdin.buffer.readline())
        assert request["verb"] == "apps" and request["args"]["platform"] == HOSTS[host][0]
        if host == "ubuntu":
            assert Path(os.environ["AAC_APP_UPDATE_STATE_DIR"]) == ROOT / "app-updates"
        else:
            assert "AAC_APP_UPDATE_STATE_DIR" not in os.environ
        start = request["args"]["action"]
        if start == "prepare-update":
            if host == "jared-windows" and (ROOT / "old-windows-agent").exists():
                trace(host, "legacy-refusal", True)
                print(json.dumps({"type":"failure","protocol":1,"id":request["id"],
                                  "failure":{"cause":"refused","reason":None,
                                             "detail":"invalid apps action","retry":{"kind":"never"},"at":None}}))
                sys.exit(1)
            print(json.dumps({"type":"progress","protocol":1,"id":request["id"],
                              "data":{"event":"admission"}}), flush=True)
            grant = sys.stdin.buffer.readline()
            if not grant:
                return
            assert json.loads(grant) == {"admit": True}, grant
            start = request["args"]["action"] = "update"
        trace(host, start, True)
        messages(host, request)
        trace(host, start + "-end", True)


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, ValueError) as error:
        with (ROOT / "fixture-errors").open("a",encoding="utf-8") as out:
            out.write(repr(error) + "\n")
        raise
