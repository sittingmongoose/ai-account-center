"""Optional pre-deletion parity check against the retained Python helper; fixture home only."""
import importlib.util, json, pathlib, sys
from unittest.mock import patch
root=pathlib.Path(__file__).resolve().parents[3]
spec=importlib.util.spec_from_file_location('history_oracle',root/'scripts/claude-history/history_index_remote_v2.py')
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
request=json.load(sys.stdin);fixture=request['_historyFixture']
with patch.object(h.Path,'home',lambda:pathlib.Path(fixture['home'])),patch.object(h,'local_closed',lambda *args:fixture['closed']),patch.object(h,'native_guard',lambda *args:fixture['native']):
    bound=h.BoundProfile(request['profileId'],request['platform'],request['policy'])
    print(json.dumps(bound.collect(),ensure_ascii=False))
