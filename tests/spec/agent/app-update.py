"""Run the Python scheduler against a single fixed desktop installer.

The hook supplies the installed version and performs no package or process IO.
Phase elapsed milliseconds are omitted from comparisons.
"""
import contextlib
import json
import pathlib
import sys
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_updates as updates
import app_update_common as common

platform, scenario = sys.argv[1:3]
manager = {"ubuntu": "apt", "mac": "official-download", "windows": "msix"}[platform]
old, new = "26.1101.9.0", "26.1113.36452.0"
installed = new if scenario == "current" else old
apps = dict.fromkeys(updates.APP_LABELS)
for app in apps:
    if app == "codex-desktop" or scenario == "all":
        owner = ("npm" if app in ("codex-cli", "t3-acp-adapters") else
                 manager if app.endswith("-desktop") else
                 "official-download" if app in ("zcode", "t3-code", "antigravity-acp") else "native")
        apps[app] = common.Install(app, platform, None, installed, owner)
complete = []
cancelled = False


def emit(event):
    global cancelled
    complete.append(event)
    if event.get("phase") == "downloading" and scenario == "cancel":
        cancelled = True


def install(app, deadline, phase=None, **_):
    assert app is apps[app.app_id]
    if installed != new and app.app_id == "codex-desktop":
        phase("downloading")
    status = "current" if installed == new else "updated"
    return common.result(app.app_id, platform, status, installed, new, app.manager, attempted=True)


def diagnostic(row, recorder):
    answer = common.with_diagnostics(row, recorder)
    answer["diagnostics"].pop("phases", None)
    return answer


with contextlib.ExitStack() as stack:
    def patch(obj, field, *args, **kwargs):
        stack.enter_context(mock.patch.object(obj, field, *args, **kwargs))

    patch(common.subprocess, "Popen", side_effect=RuntimeError("fixture process guard"))
    patch(common.urllib.request, "urlopen", side_effect=RuntimeError("fixture network guard"))
    patch(updates, "execution_lock", contextlib.nullcontext)
    patch(updates, "detect", return_value=apps)
    if scenario == "cancel":
        # Pin the point where the fixture cancels: all early result rows have
        # landed, the desktop is downloading and the Ubuntu tail is pending.
        patch(updates, "MAX_PARALLEL_UPDATES", 1)
    patch(updates, "check_readiness", return_value=None)
    patch(updates, "claude_current", return_value=None)
    patch(updates, "ubuntu_unit_busy", return_value=False)
    for name in ("update_cli", "update_desktop", "update_zcode", "update_adapters", "update_t3",
                 "update_cursor", "update_antigravity_acp"):
        patch(updates, name, install)
    patch(updates, "with_diagnostics", diagnostic)
    payload = updates.run_apply(platform, emit, lambda: cancelled)

print(json.dumps({"events": complete, "payload": payload}, ensure_ascii=True, separators=(",", ":")))
