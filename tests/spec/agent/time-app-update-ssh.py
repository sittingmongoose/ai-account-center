#!/usr/bin/env python3
"""Time two OpenSSH calls carrying an app-update fixture, cold then pooled.

Run from a host with the destination already pinned and authenticated.
No BASH/PowerShell remote files or installer are used. Fixture returns are
required byte for byte in every sample. The control socket is owned here.
"""
import argparse
import base64
import json
import pathlib
import statistics
import subprocess
import tempfile
import time

COMPUTERS = json.loads((pathlib.Path(__file__).resolve().parents[3]
                        / "web-dashboard/public/computers.json").read_text(encoding="utf-8"))
HOSTS = {
    "mac": (COMPUTERS["mac"]["sshAlias"], "/usr/bin/python3"),
    "windows": (COMPUTERS["windows"]["sshAlias"], "python.exe"),
    "nas1": ("nas1-agent", "/usr/bin/python3"),
    "alexis": ("alexis-windows", "python.exe"),
    "ubuntu": ("ubuntuserver", "/usr/bin/python3"),
}
OPTIONS = [
    "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
    "-o", "UpdateHostKeys=no", "-o", "ConnectTimeout=4", "-o", "ClearAllForwardings=yes",
    "-o", "PermitLocalCommand=no", "-o", "ForwardAgent=no",
]
APPS = (
    "antigravity-cli", "muse-code", "omp", "codex-cli", "codex-desktop", "claude-code",
    "claude-desktop", "zcode", "t3-acp-adapters", "t3-code", "cursor-cli", "antigravity-acp",
)


def remote_python(binary, code):
    source = base64.b64encode(code.encode("ascii")).decode("ascii")
    return binary + ' -B -c "import base64; exec(base64.b64decode(\'' + source + '\'))"'


def measure(target, rounds):
    alias, binary = HOSTS[target]
    verify = remote_python(binary, "print('aac-fixture-v1')")
    updates = remote_python(binary, f"""
from concurrent.futures import ThreadPoolExecutor
from time import sleep
import json
def installer(app):
    sleep(0.02)
    return {{"appId":app,"status":"current"}}
with ThreadPoolExecutor(max_workers=4) as pool:
    print(json.dumps(list(pool.map(installer,{APPS!r})),separators=(",",":")))
""")
    expected = json.dumps([{"appId": app, "status": "current"} for app in APPS], separators=(",", ":"))

    def call(opts, *args):
        return subprocess.run(["ssh", *OPTIONS, *opts, "--", alias, *args], stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=10, check=True, text=True)

    results = {"legacy": [], "pooled": []}
    for repeat in range(rounds):
        for mode in (["legacy", "pooled"] if repeat % 2 == 0 else ["pooled", "legacy"]):
            with tempfile.TemporaryDirectory(prefix="aac-m4d-", dir="/tmp") as directory:
                socket = str(pathlib.Path(directory) / "s")
                if mode == "pooled":
                    opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=20", "-o", "ControlPath=" + socket]
                else:
                    opts = ["-o", "ControlMaster=no", "-o", "ControlPath=none"]
                try:
                    begin = time.monotonic_ns()
                    assert call(opts, verify).stdout.strip() == "aac-fixture-v1"
                    assert call(opts, updates).stdout.strip() == expected
                    results[mode].append((time.monotonic_ns() - begin) / 1_000_000)
                    if mode == "pooled":
                        assert "Master running" in call([*opts, "-O", "check"]).stderr
                finally:
                    if mode == "pooled" and pathlib.Path(socket).exists():
                        subprocess.run(["ssh", *OPTIONS, *opts, "-O", "exit", "--", alias],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=8)
    return {mode: {"samplesMs": [round(n, 1) for n in values],
                   "medianMs": round(statistics.median(values), 1)} for mode, values in results.items()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("targets", nargs="+", choices=HOSTS)
    parser.add_argument("--rounds", type=int, choices=range(1, 10), default=5)
    args = parser.parse_args()
    for host in args.targets:
        print(json.dumps({"host": host, **measure(host, args.rounds)}, separators=(",", ":")), flush=True)


if __name__ == "__main__":
    main()
