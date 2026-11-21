#!/usr/bin/env python3
"""Restage an exact landed native build and rehearse its normal install in a temp prefix."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request


def smoke(binary, scratch, env, ledger, expected):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    flags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
    command = [str(binary), "dashboard", "--host", "127.0.0.1", "--port", str(port), "--no-open"]
    with (scratch / "dashboard.log").open("wb") as log:
        proc = subprocess.Popen(command, cwd=scratch, env=env, stdout=log, stderr=log,
                                creationflags=flags)
        try:
            for _ in range(120):
                if proc.poll() is not None:
                    raise RuntimeError(f"Dashboard exited {proc.returncode}. See {log.name}")
                try:
                    with client.open(base + "/api/health", timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    pass
                time.sleep(0.25)
            else:
                raise RuntimeError(f"Dashboard failed to become ready. See {log.name}")
            with client.open(base + "/app/home", timeout=5) as response:
                if response.status != 200 or b"<html" not in response.read().lower():
                    raise RuntimeError("Dashboard failed to serve the packaged Leptos page")
            for _ in range(40):
                deployed = json.loads(ledger.read_text(encoding="utf-8"))
                if deployed["commit"] == expected:
                    break
                time.sleep(0.1)
            else:
                raise RuntimeError("The installed dashboard did not advance the deployment ledger")
        finally:
            if proc.poll() is None:
                if os.name == "nt":
                    proc.send_signal(signal.CTRL_BREAK_EVENT)
                else:
                    proc.terminate()
                try:
                    proc.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
    if proc.returncode != 0:
        raise RuntimeError(f"Dashboard stopped with {proc.returncode}. See {log.name}")
    print(f"Health 200, Leptos 200, ledger {expected}, clean stop")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--wave84-ledger", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--npm-cache", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    output = args.output.resolve(strict=True)
    if output.is_relative_to(root):
        parser.error("--output must be outside the source checkout")
    scratch = Path(tempfile.mkdtemp(prefix="landed-package-", dir=output)).resolve()
    print(f"Release verification receipt directory {scratch}", flush=True)

    previous = args.wave84_ledger.read_bytes()
    record = json.loads(previous)
    if (record.get("version") != 1
            or record.get("commit") != "e85a32edcf731557e635d3e689499946d558fd34"
            or record.get("dirty") is True):
        parser.error("--wave84-ledger must be a clean Wave 84 deployment ledger")

    env = os.environ.copy()
    home = scratch / "home"
    for name in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME",
                 "CCS_HOME", "CCS_DIR", "CODEX_HOME", "CLAUDE_CONFIG_DIR",
                 "AAC_AGENT_REPORTS_DIR", "AAC_PM_MAIL_REGISTRY_DIR"):
        env[name] = str(home / name)
    env.pop("AAC_ALLOW_DEPLOY_REGRESSION", None)
    env.pop("AAC_TS_SIDECAR", None)
    env.pop("SSH_AUTH_SOCK", None)
    for name in ("AAC_AGENT_VERBS", "AAC_AGENT_TRANSPORT", "AAC_STATUS_DOORBELL",
                 "AAC_STORE_SHARE_BACKUP_DIR", "AAC_RUST_GROUP_COLLECTION", "AAC_RUST_STORE"):
        env[name] = "off"
    env["npm_config_userconfig"] = str(home / "absent-npmrc")
    env["npm_config_cache"] = str(args.npm_cache.resolve())
    ccs = Path(env["CCS_DIR"])
    ccs.mkdir(parents=True, mode=0o700)
    ledger = ccs / "deployed-build.json"
    ledger.write_bytes(previous)

    node = shutil.which("node")
    npm = shutil.which("npm.cmd" if os.name == "nt" else "npm")
    if not node or not npm:
        parser.error("Node and npm must be on PATH")
    stage = scratch / "stage"
    stage_script = root / "scripts/stage-dashboard-package.js"
    subprocess.run([node, str(stage_script), "--stage-landed-release", args.commit, str(stage)],
                   cwd=root, env=env, check=True)
    raw = subprocess.check_output([npm, "pack", "--ignore-scripts", "--json",
                                   "--pack-destination", str(scratch)], cwd=stage, env=env, text=True)
    [packed] = json.loads(raw)
    archive = scratch / packed["filename"]
    prefix = scratch / "prefix"
    install = [npm, "install", "--no-audit", "--no-fund", "--foreground-scripts",
               "--ignore-scripts=false", "--prefix", str(prefix), str(archive)]
    if args.offline:
        install.insert(2, "--offline")
    subprocess.run(install, cwd=scratch, env=env, check=True)
    installed = prefix / "node_modules/@sittingmongoose/ai-account-center"
    subprocess.run([node, str(stage_script), "--verify-release-artifacts", str(installed)],
                   cwd=scratch, env=env, check=True)
    metadata = json.loads((installed / "dist/build-info.json").read_text(encoding="utf-8"))
    if (metadata["commit"] != args.commit or metadata["dirty"] is not False
            or metadata["onMain"] is not True or record["commit"] not in metadata["lineage"]):
        raise RuntimeError("Installed package has the wrong release lineage")
    if ledger.read_bytes() != previous:
        raise RuntimeError("Postinstall altered the previous deployment ledger")
    suffix = ".exe" if os.name == "nt" else ""
    smoke(installed / f"dist/native/ai-account-center{suffix}", scratch, env, ledger, args.commit)
    with archive.open("rb") as file:
        print(f"{hashlib.file_digest(file, 'sha256').hexdigest()}  {archive}")


if __name__ == "__main__":
    main()
