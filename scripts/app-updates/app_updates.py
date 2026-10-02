#!/usr/bin/env python3
"""Fixed installed-app updater. Default inventory is read-only.

Only --apply (the authenticated dashboard's one-button action) installs or
restarts anything. There are no configurable app IDs, commands, hosts or URLs.
"""

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time
import uuid

from app_update_common import (
    APP_LABELS, Install, UpdateFailure, cli_version, command, download, execution_lock,
    powershell, private_temporary, ps_quote, result, version_text, write_private_json,
)
from app_update_desktop import detect_desktop, update_desktop
from app_update_processes import cli_contexts, family, scan, terminate_cli
from app_update_terminal import check_terminal, restart_cli

CLI_NAMES = {"antigravity-cli": "agy", "muse-code": "muse", "omp": "omp", "codex-cli": "codex", "claude-code": "claude"}


def _candidates(name, platform):
    home = pathlib.Path.home()
    if platform == "windows":
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(home / "AppData/Local")))
        roaming = pathlib.Path(os.environ.get("APPDATA", str(home / "AppData/Roaming")))
        paths = [home / ".local/bin" / (name + ".exe"), local / name / "bin" / (name + ".exe"), local / name / (name + ".exe"),
                 local / "Programs" / name / (name + ".exe"), local / "Programs" / name / (name + ".ps1"), roaming / "npm" / (name + ".cmd")]
    else:
        paths = [home / ".local/bin" / name, home / ".bun/bin" / name, pathlib.Path("/opt/homebrew/bin") / name, pathlib.Path("/usr/local/bin") / name]
    found = shutil.which(name)
    if found:
        located = pathlib.Path(found)
        managed = home / '.local/share/ai-account-center/antigravity-runtime/bin/agy'
        # Process census/update must name the native executable, never the PATH
        # launcher shim; native version/help/update controls remain pass-through.
        if not (name == 'agy' and platform == 'ubuntu' and located == managed):
            paths.insert(0, located)
    return paths


def detect_cli(app_id, platform):
    name = CLI_NAMES[app_id]
    path = next((path for path in _candidates(name, platform) if path.is_file()), None)
    if path is None:
        return None
    manager, root = "native", None
    if platform == "windows" and path.suffix.lower() == ".cmd":
        if app_id != "codex-cli":
            return Install(app_id, platform, path, manager="unsupported")
        root = path.parent / "node_modules/@openai/codex"
        try:
            info = json.loads((root / "package.json").read_text(encoding="utf-8"))
            if info.get("name") != "@openai/codex":
                return None
            version = version_text(info.get("version"))
        except (OSError, ValueError):
            return None
        return Install(app_id, platform, path, version, "npm", package_root=root)
    version = cli_version(path)
    resolved = path.resolve()
    home = pathlib.Path.home()
    if app_id == "codex-cli" and str(home / ".codex/packages/standalone/releases") in str(resolved):
        root = home / ".codex/packages/standalone/releases"
    elif app_id == "claude-code" and str(home / ".local/share/claude/versions") in str(resolved):
        root = home / ".local/share/claude/versions"
    return Install(app_id, platform, path, version, manager, package_root=root)


def detect(platform):
    return {app_id: detect_desktop(app_id, platform) if app_id.endswith("-desktop") else detect_cli(app_id, platform) for app_id in APP_LABELS}


def perform_cli_update(install):
    path = install.path
    if install.app_id == "muse-code":
        expected = pathlib.Path.home() / ".local/bin/muse"
        if install.platform != "windows" and path.resolve() != expected.resolve():
            raise UpdateFailure("unsupported")
        with private_temporary() as temporary:
            target = temporary / ("muse-install.ps1" if install.platform == "windows" else "muse-install.sh")
            download("https://dev.meta.ai/install.ps1" if install.platform == "windows" else "https://dev.meta.ai/install.sh", target, maximum=512 * 1024, timeout=60)
            env = {"MUSE_UPGRADE_MODE": "1", "MUSE_NO_MODIFY_PATH": "1"}
            if install.platform == "windows":
                command(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", target], timeout=180, env=env)
            else:
                command(["/bin/sh", target], timeout=180, env=env)
        return
    if install.manager == "npm":
        candidates = [pathlib.Path(os.environ.get("ProgramFiles", "C:\\Program Files")) / "nodejs/npm.cmd", install.path.parent / "npm.cmd"]
        located = shutil.which("npm.cmd")
        if located:
            candidates.insert(0, pathlib.Path(located))
        npm = next((item for item in candidates if item.is_file()), None)
        if npm is None:
            raise UpdateFailure("unsupported")
        argv = [str(npm), "install", "--global", "--prefix", str(install.path.parent), "@openai/codex@latest"]
        command([os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "System32", "cmd.exe"), "/d", "/s", "/c", subprocess.list2cmdline(argv)], timeout=180)
        return
    if install.manager != "native":
        raise UpdateFailure("unsupported")
    command([path, "update"], timeout=180, env={"PATH": str(path.parent) + os.pathsep + os.environ.get("PATH", "")})


def update_cli(install, deadline):
    before = install.version
    attempted = False
    changed = False
    pending = pathlib.Path.home() / ".ccs/app-updates" / (install.app_id + "-pending-restart.json")
    try:
        if not before:
            raise UpdateFailure("version_unknown")
        descriptor = pathlib.Path.home() / '.ccs/antigravity-switching/runtime-installation.json'
        if install.app_id == 'antigravity-cli' and install.platform == 'ubuntu' and (descriptor.exists() or descriptor.is_symlink()):
            # The same account lock and proved resident PTY coordinator owns
            # managed updates. Never move this session into a replacement tmux.
            bridge = pathlib.Path(__file__).parents[2] / 'dist/antigravity/managed-update-command.js'
            if not bridge.is_file(): raise UpdateFailure('unsupported')
            seconds = max(30, min(300, int(deadline - time.monotonic())))
            payload = json.loads(command([shutil.which('node') or '/usr/bin/node', bridge],
                timeout=seconds, capture=True))
            if (type(payload) is not dict or payload.get('appId') != 'antigravity-cli' or
                    payload.get('platform') != 'ubuntu' or payload.get('status') not in
                    {'current', 'updated', 'failed', 'restart_failed'}): raise UpdateFailure()
            return payload
        processes = scan(install.platform)
        # Linux Codex's shared daemon needs the existing startup-lock and idle
        # protocol, not an idle TUI/PTY restart. It is handled by the fixed bridge.
        if install.app_id == "codex-cli" and install.platform == "ubuntu" and any("app-server" in item.args for item in family(install, processes)):
            contexts, targets = cli_contexts(install, [item for item in processes if "app-server" not in item.args])
            check_terminal(install.platform, contexts)
            bridge = pathlib.Path(__file__).with_name("app_update_codex.cjs")
            seconds = max(30, min(900, int(deadline - time.monotonic())))
            payload = json.loads(command([shutil.which("node") or "/usr/bin/node", bridge, "--operation", "cli", "--timeout-seconds", str(seconds)], timeout=seconds + 15, capture=True))
            if payload.get("status") == "updated" and contexts:
                refreshed = detect_cli(install.app_id, install.platform)
                if refreshed is None or not refreshed.version:
                    raise UpdateFailure("version_unknown")
                write_private_json(pending, {"version": refreshed.version})
                try:
                    forced = terminate_cli(install, targets)
                    sessions = restart_cli(refreshed, contexts)
                    payload["restartedProcesses"] = payload.get("restartedProcesses", 0) + len(contexts)
                    payload.update(restartTargets=sessions, forcedStops=forced)
                    pending.unlink(missing_ok=True)
                except (UpdateFailure, OSError):
                    payload.update(status="restart_failed", messageCode="restart_failed")
            return payload
        contexts, targets = cli_contexts(install, processes)
        check_terminal(install.platform, contexts)
        attempted = True
        perform_cli_update(install)
        refreshed = detect_cli(install.app_id, install.platform)
        if refreshed is None or not refreshed.version:
            raise UpdateFailure("version_unknown")
        needs_restart = False
        try:
            marker = json.loads(pending.read_text(encoding="utf-8"))
            needs_restart = marker.get("version") == refreshed.version
        except (OSError, ValueError):
            pass
        if refreshed.version == before and not needs_restart:
            return result(install.app_id, install.platform, "current", before, before, install.manager, attempted=True)
        changed = True
        write_private_json(pending, {"version": refreshed.version})
        forced = terminate_cli(install, targets) if targets else 0
        try:
            sessions = restart_cli(refreshed, contexts)
        except (UpdateFailure, OSError):
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", True)
        value = result(install.app_id, install.platform, "updated", before, refreshed.version, install.manager, attempted=True, restarted=len(contexts))
        value.update(restartTargets=sessions, forcedStops=forced)
        pending.unlink(missing_ok=True)
        return value
    except UpdateFailure as error:
        if changed:
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", attempted)
        return result(install.app_id, install.platform, "failed", before, before, install.manager, error.code, attempted)
    except Exception:
        if changed:
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", attempted)
        return result(install.app_id, install.platform, "failed", before, before, install.manager, "update_failed", attempted)


def run_apply(platform):
    results = []
    deadline = time.monotonic() + 15 * 60
    with execution_lock():
        installations = detect(platform)
        # Update package apps first; the shared Codex daemon idle wait is last
        # so it cannot delay unrelated already-idle updates on this computer.
        order = [app_id for app_id in APP_LABELS if app_id != "codex-cli"] + ["codex-cli"]
        for app_id in order:
            install = installations[app_id]
            if install is None:
                results.append(result(app_id, platform, "not_installed"))
            elif time.monotonic() >= deadline:
                results.append(result(app_id, platform, "failed", install.version, install.version, install.manager, "timeout"))
            elif app_id.endswith("-desktop"):
                try: results.append(update_desktop(install, deadline))
                except Exception: results.append(result(app_id, platform, "failed", install.version, install.version, install.manager, "update_failed"))
            else:
                results.append(update_cli(install, deadline))
    return {"results": results}


def windows_interactive_apply():
    """The fixed separate InteractiveToken task owns GUI/terminal restarts."""
    root = pathlib.Path.home() / ".ccs/app-updates"
    request_path, result_path = root / "windows-task-request.json", root / "windows-task-result.json"
    with execution_lock("windows-coordinator.lock"):
        powershell("$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}", timeout=15)
        nonce = uuid.uuid4().hex
        write_private_json(request_path, {"nonce": nonce})
        powershell("$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}; Start-ScheduledTask -TaskName 'CCS App Updates'", timeout=15)
        deadline = time.monotonic() + 16 * 60
        while time.monotonic() < deadline:
            try:
                value = json.loads(result_path.read_text(encoding="utf-8"))
                if value.get("nonce") == nonce and isinstance(value.get("results"), list):
                    return {"results": value["results"]}
            except (OSError, ValueError):
                pass
            time.sleep(1)
        raise UpdateFailure("timeout")


def main():
    parser = argparse.ArgumentParser(description="Inventory or explicitly update the fixed installed app set")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--inventory", action="store_true")
    mode.add_argument("--apply", action="store_true")
    parser.add_argument("--platform", choices=("ubuntu", "mac", "windows"), required=True)
    parser.add_argument("--task-child", action="store_true")
    args = parser.parse_args()
    native = "windows" if os.name == "nt" else "mac" if sys.platform == "darwin" else "ubuntu"
    if args.platform != native:
        parser.error("The selected platform must match this computer.")
    task_nonce = None
    if args.task_child and args.apply and args.platform == "windows":
        request_path = pathlib.Path.home() / ".ccs/app-updates/windows-task-request.json"
        try:
            value = json.loads(request_path.read_text(encoding="utf-8")).get("nonce")
            if isinstance(value, str) and len(value) == 32 and all(char in "0123456789abcdef" for char in value):
                task_nonce = value
        except (OSError, ValueError):
            pass
    try:
        if not args.apply:
            installations = detect(args.platform)
            payload = {"inventory": True, "apps": [
                {"appId": app_id, "installed": install is not None, "version": install.version if install else None, "manager": install.manager if install else None}
                for app_id, install in installations.items()
            ]}
        elif args.platform == "windows" and not args.task_child:
            payload = windows_interactive_apply()
        else:
            payload = run_apply(args.platform)
    except UpdateFailure as error:
        payload = {"results": [result(app_id, args.platform, "failed", code=error.code) for app_id in APP_LABELS]}
    except Exception:
        payload = {"results": [result(app_id, args.platform, "failed", code="update_failed") for app_id in APP_LABELS]}
    if args.task_child and args.apply and args.platform == "windows":
        request_path = pathlib.Path.home() / ".ccs/app-updates/windows-task-request.json"
        try:
            if task_nonce:
                write_private_json(request_path.with_name("windows-task-result.json"), {"nonce": task_nonce, **payload})
        except (OSError, ValueError):
            pass
    print(json.dumps(payload, ensure_ascii=True, allow_nan=False, separators=(",", ":")))


if __name__ == "__main__":
    main()
