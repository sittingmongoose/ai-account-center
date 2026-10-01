#!/usr/bin/env python3
"""Install this source-only normal Brave/Chrome native messaging helper."""
import json
import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import sys

ID = "nkabpjmpmbdklhknjnjgmapmcjcmmlop"
HOST = "com.ccs.opencode_usage_bridge"


def atomic_json(path, value):
    if path.is_symlink():
        raise ValueError()
    temporary = path.with_name(path.name + ".install-tmp")
    with temporary.open("x", encoding="utf-8") as file:
        os.chmod(temporary, 0o600)
        json.dump(value, file, indent=2)
        file.write("\n")
    os.replace(temporary, path)


def install():
    if sys.platform != "darwin":
        raise ValueError()
    home = Path.home()
    source = Path(__file__).resolve().parent
    interpreter = home / ".ccs/claude-session-migration/venv/bin/python3"
    dependency = subprocess.run([str(interpreter), "-c", "import curl_cffi; print(curl_cffi.__version__)"],
                                capture_output=True, text=True, timeout=10, check=True)
    if dependency.stdout.strip() != "0.16.3":
        raise ValueError()
    root = home / ".ccs/opencode-usage-bridge"
    if root.is_symlink():
        raise ValueError()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    for directory, filenames in {
        "extension": ["manifest.json", "bridge-core.mjs", "service-worker.mjs", "popup.html", "popup.css", "popup.mjs"],
        "native-host": ["opencode_console.py", "muse_console.py", "host.py"],
    }.items():
        target = root / directory
        if target.is_symlink():
            raise ValueError()
        target.mkdir(exist_ok=True, mode=0o700)
        os.chmod(target, 0o700)
        for filename in filenames:
            path = target / filename
            if path.is_symlink():
                raise ValueError()
            shutil.copyfile(source / directory / filename, path)
            os.chmod(path, 0o600)
    muse_source = source / "native-host/muse_console.py"
    launcher = root / "native-host/launch-host.sh"
    if launcher.is_symlink():
        raise ValueError()
    launcher.write_text("#!/bin/sh\nexec " + shlex.quote(str(interpreter)) + " " +
                        shlex.quote(str(root / "native-host/host.py")) + ' "$@"\n')
    os.chmod(launcher, 0o700)
    capsule_directory = home / ".ccs/account-usage"
    if capsule_directory.is_symlink():
        raise ValueError()
    capsule_directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(capsule_directory, 0o700)
    module = capsule_directory / "muse_console.py"
    if module.is_symlink():
        raise ValueError()
    shutil.copyfile(muse_source, module)
    os.chmod(module, 0o600)
    manifest = {"name": HOST, "description": "CCS read-only OpenCode and Muse browser usage",
                "path": str(launcher), "type": "stdio", "allowed_origins": ["chrome-extension://" + ID + "/"]}
    installed = []
    for browser, relative in (("Brave", "Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
                              ("Chrome", "Library/Application Support/Google/Chrome/NativeMessagingHosts")):
        path = home / relative
        if path.is_symlink():
            raise ValueError()
        path.mkdir(parents=True, exist_ok=True)
        atomic_json(path / (HOST + ".json"), manifest)
        installed.append(browser)
    return {"installed": True, "browsers": installed, "extensionId": ID,
            "extensionDirectory": str(root / "extension"), "browserPoliciesChanged": False,
            "extensionLoadedAutomatically": False, "dependenciesVersionVerified": "curl_cffi 0.16.3",
            "authenticationWritten": False, "capsulePresent": (capsule_directory / "opencode-console-session.json").is_file()}


if __name__ == "__main__":
    try:
        print(json.dumps(install(), separators=(",", ":")))
    except Exception:
        print('{"installed":false,"status":"installation_unavailable"}')
        raise SystemExit(1)
