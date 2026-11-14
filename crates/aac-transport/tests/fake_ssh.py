#!/usr/bin/python3
"""Offline SSH/SCP script. Records argv and simulates a private remote cache in this fixture."""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import sys
import time

root = Path(__file__).parent
scenario = json.loads((root / "scenario.json").read_text())
tool = Path(sys.argv[0]).name
argv = sys.argv[1:]
with (root / "trace.jsonl").open("a") as log:
    log.write(json.dumps({"tool": tool, "argv": argv, "at": time.monotonic(), "pid": os.getpid(), "event": "start"}) + "\n")
mode = scenario.get("mode", "ok")
if mode == "unreachable":
    sys.exit(255)
if mode == "host-key":
    sys.stderr.write("Host key verification failed")
    sys.exit(255)
if mode == "sleep":
    time.sleep(scenario.get("seconds", 5))
elif mode == "flood":
    sys.stdout.write("x" * 65536)
    sys.stdout.flush()
    sys.stderr.write("x" * 65536)
elif mode == "shipping":
    if tool == "scp":
        if scenario.get("uploadFailure"):
            sys.exit(1)
        shutil.copyfile(argv[-2], root / "upload.part")
        if scenario.get("corruptUpload"):
            (root / "upload.part").write_bytes(b"damaged upload")
        sys.exit(0)
    command = shlex.split(argv[-1])
    script = command[-1]
    match = re.search(r"case '(probe|prepare|commit|run)'", script)
    if not match:
        raise RuntimeError("unexpected launcher command")
    stage = match[1]
    if stage == "prepare":
        sys.exit(0)
    if stage == "commit":
        part = root / "upload.part"
        if hashlib.sha256(part.read_bytes()).hexdigest() != scenario["hash"]:
            sys.exit(87)
        shutil.move(str(part), str(root / "installed"))
    if not (root / "installed").exists():
        sys.exit(87)
    if hashlib.sha256((root / "installed").read_bytes()).hexdigest() != scenario["hash"]:
        sys.exit(87)
    if stage == "run":
        request = json.loads(sys.stdin.buffer.readline())
        if scenario.get("wrongEventId"):
            request["id"] = "wrong"
        event = {"type": "result", "protocol": 1, "id": request["id"], "value": {"status": "ok"}}
        print(json.dumps(event))
        if scenario.get("duplicateTerminal"):
            print(json.dumps(event))
        sys.exit(0)
    identity = scenario["handshake"].copy()
    if scenario.get("wrongHandshake"):
        identity[scenario["wrongHandshake"]] = "wrong"
    print(json.dumps(identity))
    sys.exit(0)
else:
    sys.stdout.buffer.write(sys.stdin.buffer.read())
with (root / "trace.jsonl").open("a") as log:
    log.write(json.dumps({"at": time.monotonic(), "pid": os.getpid(), "event": "end"}) + "\n")
