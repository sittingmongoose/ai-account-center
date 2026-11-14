"""Offline parser case-table generator/checker; imports never call collectors."""
import importlib.util
import json
import pathlib
import sys
import time
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
sys.path.insert(0, str(ROOT / 'scripts/analytics-remote'))
NOW = 1767225600.25
MODULES = {
    'claude_usage': ('retry_after_seconds', 'reset_at', 'number'),
    'desktop_helpers': ('retry_after_seconds', 'reset_at', 'number'),
    'plan_common': ('retry_after_seconds', 'reset_at', 'number'),
    'muse_console': ('retry_seconds', None, None),
    'failover_core': ('_retry_after', None, '_number'),
    'failover_special': ('_retry_after', None, None),
    'failover_usage': (None, None, '_number'),
    'cursor_usage': (None, '_milliseconds_reset', None),
}
PROVIDERS = {
    'claude_usage': ['claude'], 'desktop_helpers': ['antigravity', 'muse-native'],
    'plan_common': ['kimi', 'zai', 'minimax', 'qwen'], 'muse_console': ['muse'],
    'failover_core': ['claude-failover'], 'failover_special': ['antigravity-failover'],
    'failover_usage': ['codex-failover'], 'cursor_usage': ['cursor'],
}
INPUTS = {
    'retry': [None, True, 0, '', 'garbage', '-1', '0', '1', ' 31 ', '86401', '9999999999999', 'Thu, 01 Jan 2026 00:00:01 GMT', 'Wed, 31 Dec 2025 23:59:59 GMT', 'x' * 129],
    'reset': [None, True, 0, -1, 1767225600, 1767225600000, '1767225600', '', 'bad', '2026-01-01T00:00:00Z', '2026-01-01T01:00:00+01:00', '2026-01-01T00:00:00.123Z', '2026-01-01T00:00:00', '2101-01-01T00:00:00Z', '2201-01-01T00:00:00Z'],
    'number': [None, True, False, 0, -1, 1.25, '1.25', ' 2 ', '', 'NaN', 'Infinity', '1e309', [], {}],
}

def load(name):
    source = ROOT / 'scripts/account-usage' / (name + '.py')
    spec = importlib.util.spec_from_file_location(name, source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

def evaluate():
    rows = []
    with patch.object(time, 'time', return_value=NOW), patch('socket.socket', side_effect=AssertionError('Network forbidden')):
        for name, functions in MODULES.items():
            module = load(name)
            for kind, function in zip(('retry', 'reset', 'number'), functions):
                if function is None:
                    continue
                for value in INPUTS[kind]:
                    kwargs = {}
                    if name == 'failover_special' and kind == 'retry':
                        import datetime
                        kwargs['now'] = datetime.datetime.fromtimestamp(NOW, datetime.timezone.utc)
                    result = getattr(module, function)(value, **kwargs)
                    rows.append(dict(module=name, providers=PROVIDERS[name], function=function, kind=kind, input=value, python=result))
    return dict(schemaVersion=1, nowUnixSeconds=NOW, cases=rows)

def main():
    file = ROOT / 'tests/spec/parser-vectors.json'
    actual = evaluate()
    if '--regenerate' in sys.argv:
        file.write_text(json.dumps(actual, ensure_ascii=False, indent=2) + '\n')
    else:
        expected = json.loads(file.read_text())
        # TS annotations are checked independently by parser-ts.mjs.
        for row in expected['cases']:
            row.pop('typescript', None)
            row.pop('disagreement', None)
            row.pop('typescriptSource', None)
        assert actual == expected, 'Python parser contract changed; regenerate and review'
    # Python implements only the analytics session key among the identity vectors.
    import analytics_usage_remote as remote
    vectors = json.loads((ROOT / 'tests/spec/id-vectors.json').read_text())
    identities = [dict(tool=row['tool'], sessionId=row['sessionId'], typescript=row['sessionKey'], python=remote._session_key(row['tool'], row['sessionId']), disagreement=remote._session_key(row['tool'], row['sessionId']) != row['sessionKey']) for row in vectors['sessions']]
    identity_file = ROOT / 'tests/spec/id-python-parity.json'
    if '--regenerate' in sys.argv:
        identity_file.write_text(json.dumps(identities, ensure_ascii=False, indent=2) + '\n')
    else:
        assert identities == json.loads(identity_file.read_text())
    print(str(len(actual['cases'])) + ' parser cases; 8 shared Python session vectors passed')

if __name__ == '__main__':
    main()
