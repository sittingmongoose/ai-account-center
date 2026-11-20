"""One-app installer stand-in used only by spawned agent tests."""
import argparse
import json
import os
import pathlib
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--platform", required=True)
parser.add_argument("--agent-capabilities", action="store_true")
parser.add_argument("--apply", action="store_true")
parser.add_argument("--agent-managed", action="store_true")
parser.add_argument("--app")
parser.add_argument("--worker-seconds", type=int)
parser.add_argument("--t3-restart-pending", action="store_true")
args = parser.parse_args()
if args.agent_capabilities:
    print('{"appWorker":1}', flush=True)
    raise SystemExit(0)

root = pathlib.Path(os.environ["AAC_UPDATE_STATE_DIR"])
assert (root / "fake-python-installer-root").is_file()
assert os.environ["CCS_HOME"] == str(root)
assert args.apply and args.agent_managed and 0 < args.worker_seconds <= 1800

def emit(data):
    print(json.dumps(data, separators=(",", ":")), flush=True)

status, code = "not_installed", "not_installed"
extra = {}
if args.app == "codex-desktop":
    for phase in ("checking", "updating", "downloading"):
        emit({"event": "app", "appId": args.app, "phase": phase})
    if (root / "hold-fixture-worker").exists():
        if sys.stdin.readline().strip() == "cancel":
            (root / "fixture-cancel-observed").touch()
        status, code = "skipped", "skipped_cancelled"
    else:
        status, code = "updated", "updated"
        extra = {"previousVersion":"1.2.3","version":"1.2.4","manager":"msix"}
elif args.app == "antigravity-cli":
    status, code = "held", "held_for_review"
    extra = {"heldVersion":"1.2.4","version":"1.2.3","manager":"native",
             "diagnostics":{"cause":"none","tail":"fixture leak sampler","command":"fixture command"}}
elif args.app == "t3-acp-adapters":
    status, code = "action_required", "in_use"
    extra = {"parts":[{"name":"muse-acp","previousVersion":"1.2.3","version":"1.2.3","inUse":True}]}
emit({"results":[{"appId": args.app, "platform":args.platform, "status":status,"messageCode":code,**extra}]})
