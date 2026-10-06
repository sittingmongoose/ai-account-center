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
import sys
import time
import uuid

from app_update_common import (
    APP_LABELS, Install, UpdateFailure, cli_probe, command, download, execution_lock,
    powershell, private_temporary, ps_quote, result, version_text, write_private_json,
)
from app_update_desktop import detect_desktop, update_desktop
from app_update_processes import cli_contexts, family, scan, terminate_cli
from app_update_terminal import check_terminal, restart_cli

CLI_NAMES = {"antigravity-cli": "agy", "muse-code": "muse", "omp": "omp", "codex-cli": "codex", "claude-code": "claude"}
# The Codex bridge's whole budget: lock (30 s) + update (180 s) + idle wait
# (60 s) + proxy restart checks. It never waits hours for a busy Codex.
CODEX_BRIDGE_SECONDS = 420


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
    version, probe = cli_probe(path)
    if probe == "timeout":
        return Install(app_id, platform, path, None, "native", probe="timeout")
    resolved = path.resolve()
    home = pathlib.Path.home()
    if app_id == "codex-cli" and str(home / ".codex/packages/standalone/releases") in str(resolved):
        root = home / ".codex/packages/standalone/releases"
    elif app_id == "claude-code" and str(home / ".local/share/claude/versions") in str(resolved):
        root = home / ".local/share/claude/versions"
    return Install(app_id, platform, path, version, manager, package_root=root)


def _detect_one(app_id, platform):
    return detect_desktop(app_id, platform) if app_id.endswith("-desktop") else detect_cli(app_id, platform)


def detect(platform):
    """Read-only version probes for every app, side by side.

    Each probe has its own timeout, so the slowest one bounds the whole check
    instead of all seven adding up. Nothing here installs or stops anything.
    """
    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=len(APP_LABELS)) as pool:
        futures = {app_id: pool.submit(_detect_one, app_id, platform) for app_id in APP_LABELS}
    found = {}
    for app_id, future in futures.items():
        try:
            found[app_id] = future.result()
        except Exception:
            found[app_id] = Install(app_id, platform, None, probe="failed")
    return found


def resolve_muse_shell(platform):
    """The Meta installer is bash-only ([[ ]], arrays); /bin/sh is dash on
    Ubuntu and fails immediately. Resolve bash first so a host without one
    reports unsupported without downloading anything."""
    if platform == "windows":
        return None
    shell = shutil.which("bash")
    if shell is None and pathlib.Path("/bin/bash").is_file():
        shell = "/bin/bash"
    if shell is None:
        raise UpdateFailure("unsupported")
    return shell


def resolve_npm(prefix):
    """Locate node.exe plus its npm-cli.js so npm runs without any shell.

    cmd.exe /d /s /c mangles a quoted executable containing spaces, so the
    previous '"C:\\Program Files\\nodejs\\npm.cmd" install ...' line died with
    "not recognized" before npm ever started. Returns (node, cli) or None.
    """
    roots = []
    located = shutil.which("npm.cmd")
    if located:
        roots.append(pathlib.Path(located).parent)
    roots.append(pathlib.Path(os.environ.get("ProgramFiles", "C:\\Program Files")) / "nodejs")
    roots.append(prefix)
    for root in roots:
        node, cli = root / "node.exe", root / "node_modules/npm/bin/npm-cli.js"
        if node.is_file() and cli.is_file():
            return node, cli
    located_node = shutil.which("node.exe") or shutil.which("node")
    if located_node:
        root = pathlib.Path(located_node).parent
        cli = root / "node_modules/npm/bin/npm-cli.js"
        if cli.is_file():
            return pathlib.Path(located_node), cli
    return None


def _clamp(seconds, deadline):
    if deadline is None:
        return seconds
    return max(30, min(seconds, int(deadline - time.monotonic())))


def npm_view_latest(install):
    """Read-only registry check for the npm path. None when unknowable."""
    resolved = resolve_npm(install.path.parent)
    if resolved is None:
        return None
    node, cli = resolved
    try:
        return command([str(node), str(cli), "view", "@openai/codex", "version"], timeout=60, capture=True).strip() or None
    except UpdateFailure:
        return None


def perform_cli_update(install, deadline=None):
    path = install.path
    if install.app_id == "muse-code":
        expected = pathlib.Path.home() / ".local/bin/muse"
        if install.platform != "windows" and path.resolve() != expected.resolve():
            raise UpdateFailure("unsupported")
        shell = resolve_muse_shell(install.platform)
        with private_temporary() as temporary:
            target = temporary / ("muse-install.ps1" if install.platform == "windows" else "muse-install.sh")
            download("https://dev.meta.ai/install.ps1" if install.platform == "windows" else "https://dev.meta.ai/install.sh", target, maximum=512 * 1024, timeout=60)
            env = {"MUSE_UPGRADE_MODE": "1", "MUSE_NO_MODIFY_PATH": "1"}
            if install.platform == "windows":
                command(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", target], timeout=_clamp(180, deadline), env=env)
            else:
                command([shell, target], timeout=_clamp(600, deadline), env=env)
        return
    if install.manager == "npm":
        resolved = resolve_npm(install.path.parent)
        if resolved is None:
            raise UpdateFailure("unsupported")
        node, cli = resolved
        command([str(node), str(cli), "install", "--global", "--prefix", str(install.path.parent), "@openai/codex@latest"], timeout=_clamp(300, deadline))
        return
    if install.manager != "native":
        raise UpdateFailure("unsupported")
    command([path, "update"], timeout=180, env={"PATH": str(path.parent) + os.pathsep + os.environ.get("PATH", "")})


def update_cli(install, deadline):
    before = install.version
    attempted = False
    changed = False
    pre_stopped = False
    pre_forced = 0
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
            seconds = max(30, min(CODEX_BRIDGE_SECONDS, int(deadline - time.monotonic())))
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
        if install.manager == "npm" and install.platform == "windows":
            if contexts and npm_view_latest(install) == before:
                # Already current: never stop running sessions for a no-op.
                # Anything mapped is running, so no stale marker can matter.
                pending.unlink(missing_ok=True)
                return result(install.app_id, install.platform, "current", before, before, install.manager, attempted=False)
            if contexts:
                # Windows cannot replace a running npm tree (locked files fail
                # the install), so mapped instances stop before npm runs. The
                # pending marker lets a later explicit click retry the restart
                # if this one leaves processes down.
                write_private_json(pending, {"version": before})
                try:
                    pre_forced = terminate_cli(install, targets)
                except UpdateFailure:
                    try:
                        restart_cli(install, contexts)
                        pending.unlink(missing_ok=True)
                    except (UpdateFailure, OSError):
                        pass
                    return result(install.app_id, install.platform, "failed", before, before, install.manager, "restart_context", False)
                pre_stopped = True
        attempted = True
        perform_cli_update(install, deadline)
        refreshed = detect_cli(install.app_id, install.platform)
        if refreshed is None or not refreshed.version:
            raise UpdateFailure("version_unknown")
        marker_version = None
        try:
            marker_version = json.loads(pending.read_text(encoding="utf-8")).get("version")
        except (OSError, ValueError):
            pass
        needs_restart = marker_version == refreshed.version and marker_version != before
        if refreshed.version == before and not needs_restart:
            if pre_stopped:
                # Stopped for the install; relaunch the same version. The
                # marker must go: there is nothing newer to retry towards.
                pending.unlink(missing_ok=True)
                try:
                    sessions = restart_cli(refreshed, contexts)
                except (UpdateFailure, OSError):
                    write_private_json(pending, {"version": refreshed.version})
                    return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", True)
                value = result(install.app_id, install.platform, "current", before, before, install.manager, attempted=True, restarted=len(contexts))
                value.update(restartTargets=sessions, forcedStops=pre_forced)
                return value
            if marker_version == before:
                # Stale pre-stop bookkeeping from an earlier run. Dropping it
                # keeps a later run from reporting "updated" for a version
                # that never changed.
                pending.unlink(missing_ok=True)
            return result(install.app_id, install.platform, "current", before, before, install.manager, attempted=True)
        changed = True
        write_private_json(pending, {"version": refreshed.version})
        forced = (terminate_cli(install, targets) if targets else 0) + pre_forced
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
        if pre_stopped:
            relaunch_after_failed_install(install, contexts, pending)
        code = "update_failed" if error.code == "download_blocked" else error.code
        return result(install.app_id, install.platform, "failed", before, before, install.manager, code, attempted)
    except Exception:
        if changed:
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", attempted)
        if pre_stopped:
            relaunch_after_failed_install(install, contexts, pending)
        return result(install.app_id, install.platform, "failed", before, before, install.manager, "update_failed", attempted)


def relaunch_after_failed_install(install, contexts, pending):
    """Relaunch the old version after a post-stop install failure, best effort.

    The pending marker stays when the relaunch fails, so a later explicit
    click retries the restart through the normal pending path.
    """
    try:
        restart_cli(install, contexts)
        pending.unlink(missing_ok=True)
    except (UpdateFailure, OSError):
        pass


def check_readiness(install):
    """Per-app pre-flight before an update is attempted. Read-only: no installs,
    stops or restarts. Returns None when ready, else (status, code): 'failed'
    when the check ran and said no (no supported updater, or running instances
    that cannot restart safely), 'unknown' when the check itself could not run.
    """
    try:
        if install.manager == "unsupported":
            return ("failed", "unsupported")
        if install.app_id.endswith("-desktop"):
            # Desktop updaters verify before stopping anything; the manager
            # check above is the whole pre-flight.
            return None
        contexts, _targets = cli_contexts(install, scan(install.platform))
        check_terminal(install.platform, contexts)
    except UpdateFailure as error:
        return ("failed", error.code)
    except Exception:
        return ("unknown", "readiness_unknown")
    return None


# One computer's whole run; apps not started by then report a timeout row.
HOST_DEADLINE_SECONDS = 15 * 60


def run_apply(platform, emit=None, cancelled=None):
    """Check and update every app on this computer, one installer at a time.

    emit(event) receives {"event": "app", ...} before each app's check and
    update, and {"event": "result", ...} as soon as its row is known, so the
    dashboard can show live progress. cancelled() is polled between apps: once
    true, every app not yet started is reported as skipped.
    """
    emit = emit or (lambda event: None)
    cancelled = cancelled or (lambda: False)
    results = []
    deadline = time.monotonic() + HOST_DEADLINE_SECONDS

    def report(row):
        results.append(row)
        emit({"event": "result", "result": row})

    with execution_lock():
        emit({"event": "app", "appId": None, "phase": "checking"})
        installations = detect(platform)
        # Update package apps first; the shared Codex daemon idle wait is last
        # so it cannot delay unrelated already-idle updates on this computer.
        order = [app_id for app_id in APP_LABELS if app_id != "codex-cli"] + ["codex-cli"]
        for app_id in order:
            install = installations[app_id]
            if cancelled():
                report(result(app_id, platform, "skipped", code="skipped_cancelled"))
            elif install is None:
                report(result(app_id, platform, "not_installed"))
            elif install.probe == "timeout":
                report(result(app_id, platform, "unknown", None, None, install.manager, "check_timeout"))
            elif install.probe is not None:
                report(result(app_id, platform, "unknown", None, None, None, "readiness_unknown"))
            elif time.monotonic() >= deadline:
                report(result(app_id, platform, "failed", install.version, install.version, install.manager, "timeout"))
            else:
                emit({"event": "app", "appId": app_id, "phase": "checking"})
                gate = check_readiness(install)
                if gate is not None:
                    status, code = gate
                    report(result(app_id, platform, status, install.version, install.version, install.manager, code))
                    continue
                emit({"event": "app", "appId": app_id, "phase": "updating"})
                if app_id.endswith("-desktop"):
                    try: report(update_desktop(install, deadline))
                    except Exception: report(result(app_id, platform, "failed", install.version, install.version, install.manager, "update_failed"))
                else:
                    report(update_cli(install, deadline))
    return {"results": results}


def windows_interactive_apply(emit=None, cancelled=None):
    """The fixed separate InteractiveToken task owns GUI/terminal restarts.

    The task child writes its progress to a private file; this coordinator
    relays new events to emit() and forwards a cancel through a nonce-bound
    cancel file the child polls between apps.
    """
    emit = emit or (lambda event: None)
    cancelled = cancelled or (lambda: False)
    root = pathlib.Path.home() / ".ccs/app-updates"
    request_path, result_path = root / "windows-task-request.json", root / "windows-task-result.json"
    progress_path, cancel_path = root / "windows-task-progress.json", root / "windows-task-cancel.json"
    with execution_lock("windows-coordinator.lock"):
        powershell("$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}", timeout=15)
        nonce = uuid.uuid4().hex
        for stale in (progress_path, cancel_path):
            stale.unlink(missing_ok=True)
        write_private_json(request_path, {"nonce": nonce})
        powershell("$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}; Start-ScheduledTask -TaskName 'CCS App Updates'", timeout=15)
        deadline = time.monotonic() + 16 * 60
        relayed, cancel_sent = 0, False
        while time.monotonic() < deadline:
            if not cancel_sent and cancelled():
                try:
                    write_private_json(cancel_path, {"nonce": nonce})
                    cancel_sent = True
                except OSError:
                    pass
            try:
                value = json.loads(progress_path.read_text(encoding="utf-8"))
                events = value.get("events") if value.get("nonce") == nonce else None
                if isinstance(events, list):
                    for event in events[relayed:]:
                        emit(event)
                    relayed = max(relayed, len(events))
            except (OSError, ValueError, AttributeError):
                pass
            try:
                value = json.loads(result_path.read_text(encoding="utf-8"))
                if value.get("nonce") == nonce and isinstance(value.get("results"), list):
                    return {"results": value["results"]}
            except (OSError, ValueError, AttributeError):
                pass
            time.sleep(1)
        raise UpdateFailure("timeout")


def task_child_progress(nonce):
    """emit/cancelled pair for the Windows task child, bound to its nonce."""
    root = pathlib.Path.home() / ".ccs/app-updates"
    events = []

    def emit(event):
        events.append(event)
        try:
            write_private_json(root / "windows-task-progress.json", {"nonce": nonce, "events": events})
        except (OSError, ValueError):
            pass

    def cancelled():
        try:
            return json.loads((root / "windows-task-cancel.json").read_text(encoding="utf-8")).get("nonce") == nonce
        except (OSError, ValueError, AttributeError):
            return False

    return emit, cancelled


def stream_progress():
    """emit/cancelled pair for a dashboard that asked for live progress.

    Events go to stdout as one JSON object per line, flushed at once; the final
    {"results": [...]} line stays last. A "cancel" line on stdin (the dashboard
    writes it; over ssh it arrives the same way) stops apps not yet started.
    """
    import threading
    stop = threading.Event()

    def listen():
        # Raw reads on the descriptor: a daemon thread parked inside the
        # buffered sys.stdin would hold its lock while the interpreter exits.
        try:
            descriptor = sys.stdin.fileno()
        except (AttributeError, OSError, ValueError):
            return
        seen = b""
        while not stop.is_set():
            try:
                chunk = os.read(descriptor, 256)
            except OSError:
                return
            if not chunk:
                return
            seen = (seen + chunk)[-64:]
            if b"cancel\n" in seen or b"cancel\r\n" in seen:
                stop.set()

    threading.Thread(target=listen, daemon=True).start()

    def emit(event):
        try:
            print(json.dumps(event, ensure_ascii=True, allow_nan=False, separators=(",", ":")), flush=True)
        except (OSError, ValueError):
            pass

    return emit, stop.is_set


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
    emit = cancelled = None
    if args.apply and args.task_child and task_nonce:
        emit, cancelled = task_child_progress(task_nonce)
    elif args.apply and os.environ.get("AAC_UPDATE_PROGRESS") == "1":
        # Only a dashboard that reads line-by-line progress sets this; an
        # older one keeps receiving exactly one JSON document.
        emit, cancelled = stream_progress()
    try:
        if not args.apply:
            installations = detect(args.platform)
            payload = {"inventory": True, "apps": [
                {"appId": app_id, "installed": install is not None, "version": install.version if install else None, "manager": install.manager if install else None}
                for app_id, install in installations.items()
            ]}
        elif args.platform == "windows" and not args.task_child:
            payload = windows_interactive_apply(emit, cancelled)
        else:
            payload = run_apply(args.platform, emit, cancelled)
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
