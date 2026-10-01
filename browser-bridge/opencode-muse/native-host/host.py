#!/usr/bin/env python3
"""Bounded Chrome/Brave native messaging, or safe public --collect output."""
import json
from pathlib import Path
import struct
import sys

# Public installed usage modules; credentials never become command arguments.
sys.path.insert(0, str(Path.home() / ".ccs/account-usage"))

from opencode_console import MAX_BYTES, Unavailable, collect, read_capsule, write_capsule, validate_cookies

EXTENSION_ID = "nkabpjmpmbdklhknjnjgmapmcjcmmlop"
ORIGIN = "chrome-extension://" + EXTENSION_ID + "/"


def read_frame(handle):
    header = handle.read(4)
    if header == b"":
        return None
    if len(header) != 4:
        raise Unavailable("protocol_error")
    size = struct.unpack("<I", header)[0]
    if not 0 < size <= MAX_BYTES:
        raise Unavailable("protocol_error")
    body = handle.read(size)
    if len(body) != size:
        raise Unavailable("protocol_error")
    try:
        return json.loads(body)
    except (ValueError, UnicodeError):
        raise Unavailable("protocol_error")


def write_frame(handle, value):
    body = json.dumps(value, allow_nan=False, separators=(",", ":")).encode()
    if len(body) > MAX_BYTES:
        raise Unavailable("protocol_error")
    handle.write(struct.pack("<I", len(body)))
    handle.write(body)
    handle.flush()


def handle_request(request, root, collector=collect, writer=write_capsule):
    if isinstance(request, dict) and request.get("action") == "museSync":
        from muse_console import MuseError, collect_browser, validate_cookies as muse_cookies, write_capsule as muse_write
        if set(request) - {"schemaVersion", "action", "cookies", "teamId"} or type(request.get("schemaVersion")) is not int or request["schemaVersion"] != 1:
            raise MuseError("invalid_request")
        cookies = muse_cookies(request.get("cookies"))
        team, sample = collect_browser(cookies, request.get("teamId"))
        muse_write(root, cookies, team, sample["email"], sample["plan"])
        return {"schemaVersion": 1, "ok": True, "sample": sample, "teamId": team}
    if (not isinstance(request, dict) or set(request) - {"schemaVersion", "action", "cookies", "workspaceId"}
            or request.get("schemaVersion") != 1 or request.get("action") != "sync"):
        raise Unavailable("invalid_request")
    cookies = validate_cookies(request.get("cookies"))
    workspace, sample = collector(cookies, request.get("workspaceId"))
    writer(root, cookies, workspace)
    return {"schemaVersion": 1, "ok": True, "sample": sample}


def main():
    root = Path.home() / ".ccs/account-usage"
    if sys.argv[1:] == ["--collect"]:
        try:
            cookies, workspace = read_capsule(root)
            _, sample = collect(cookies, workspace)
            print(json.dumps(sample, allow_nan=False, separators=(",", ":")))
        except (Unavailable, OSError, ValueError) as error:
            print(json.dumps({"provider": "opencode-go", "platform": "mac", "status": "unavailable",
                              "code": error.code if isinstance(error, Unavailable) else "local_storage_error", "windows": []}, separators=(",", ":")))
            raise SystemExit(1)
        return
    if not sys.argv[1:] or sys.argv[1] != ORIGIN or len(sys.argv[1:]) > 2:
        raise SystemExit(1)
    for _ in range(10):
        try:
            request = read_frame(sys.stdin.buffer)
            if request is None:
                return
            response = handle_request(request, root)
        except Unavailable as error:
            response = {"schemaVersion": 1, "ok": False, "code": error.code}
        except Exception as error:
            from muse_console import MuseError
            if isinstance(error, MuseError):
                response = {"schemaVersion": 1, "ok": False, "code": error.code}
                if error.code == "choose_team":
                    response["teams"] = error.teams
            else:
                response = {"schemaVersion": 1, "ok": False, "code": "error"}
        write_frame(sys.stdout.buffer, response)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        # Never emit tracebacks or secret-bearing upstream exceptions.
        raise SystemExit(1)
