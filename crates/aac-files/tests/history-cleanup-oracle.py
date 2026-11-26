"""Frozen cleanup policy on a synthetic filesystem only; no host or provider calls."""
import importlib.util, json, pathlib, sys
from types import SimpleNamespace
source=pathlib.Path(__file__).resolve().parents[3]/'scripts/claude-history/history_index_remote_v2.py'
spec=importlib.util.spec_from_file_location('cleanup_oracle',source)
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
bound=SimpleNamespace(root=pathlib.Path(sys.argv[1]),bind=lambda:None)
print(json.dumps(h.BoundProfile._prune_snapshots(bound)))
