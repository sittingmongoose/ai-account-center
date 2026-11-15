"""Production transcript transport, only in the owned loopback fixture namespace."""
import importlib.util, os
from pathlib import Path
source = Path(__file__).resolve().parents[3] / 'scripts/claude-history/history_index_remote_v2.py'
spec = importlib.util.spec_from_file_location('aac_history_fixture', source)
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
bound = h.BoundProfile.__new__(h.BoundProfile)
bound.bind = lambda: None
bound.platform = 'mac'
bound.policy = {'project': {'transcriptRoot': '/fixture/transcripts'}, 'ssh': {'mac': {'alias': 'fixture', 'hostname': '127.0.0.1', 'username': 'root', 'port': int(os.environ['AAC_TEST_PORT'])}}}
assert bound.verify_transcripts([])
print('{"connected":true}')
