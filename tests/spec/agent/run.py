#!/usr/bin/env python3
"""Replay captured helper requests in isolated homes; compare Python, golden, and Rust."""
import base64
import contextlib
import json
import hashlib
import unicodedata
import os
import pathlib
import re
import subprocess
import sys
import tempfile
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
import signin_expiry as helper


def python_reply(a):
    env = a.get('environment', {})
    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch.dict(os.environ, env, clear=True))
        stack.enter_context(mock.patch.object(sys, 'platform', a['platform']))
        if a.get('op') == 'fix':
            return helper.fix_command(a['path'], pathlib.Path(a['home']), storage_var=a['storageVar'], spelling=a['spelling'])
        io = a.get('io', {})
        states, reads = io.get('states', {}), io.get('reads', {})
        old_state, old_read = helper._dir_state, helper._read_regular_bytes
        stack.enter_context(mock.patch.object(helper, '_dir_state', side_effect=lambda p: tuple(states[str(p)]) if str(p) in states else old_state(p)))
        stack.enter_context(mock.patch.object(helper, '_read_regular_bytes', side_effect=lambda p, limit=65536: (base64.b64decode(reads[str(p)]) if reads[str(p)] is not None else None) if str(p) in reads else old_read(p, limit)))
        if 'savedRoots' in io:
            saved = io['savedRoots']
            stack.enter_context(mock.patch.object(helper, '_saved_roots', return_value=None if saved is None else [(pathlib.Path(p), selectors) for p, selectors in saved]))
        if io.get('discoveryIncomplete'):
            import failover_core
            old_saved = helper._saved_roots
            def incomplete(home, ccs):
                answer = old_saved(home, ccs)
                failover_core.SETTINGS_HOME_FAILURES[0] += 1
                return answer
            stack.enter_context(mock.patch.object(helper, '_saved_roots', side_effect=incomplete))
        def key(service=helper.KEYCHAIN_PLAIN_SERVICE):
            item = a.get('keychain', {}).get(service)
            if item is None:
                return None
            return {'locked': helper.KEYCHAIN_LOCKED, 'indeterminate': helper.KEYCHAIN_INDETERMINATE, 'oversized': helper.KEYCHAIN_ANSWERED_UNREADABLE}.get(item['status'], item.get('text'))
        stack.enter_context(mock.patch.object(helper, '_mac_keychain_text', side_effect=key))
        return helper.collect(pathlib.Path(a['home']), a['nowMs'], a['includeKeychain'], pathlib.Path(a['ccsDir']))


def normalize(value):
    if isinstance(value, str):
        return re.sub(r"[\ud800-\udfff]", "\ufffd", value)
    if isinstance(value, list):
        return [normalize(v) for v in value]
    if isinstance(value, dict):
        return {normalize(k): normalize(v) for k,v in value.items()}
    return value


def terminal(result):
    event = json.loads(result.stdout)
    assert result.returncode == 0 and event.get("type") == "result" and event.get("protocol") == 1 and event.get("id") == "parity", (result.returncode, event)
    return event["value"]


def main():
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    count = 0
    failures = 0
    production_only = "--production-only" in sys.argv
    witness = "--witness-baseline" in sys.argv
    for row in json.loads((ROOT / 'tests/spec/agent/signin-expiry.json').read_text())['cases']:
        if production_only and not row.get('production'): continue
        if witness and not row['source'].startswith('review-'): continue
        with tempfile.TemporaryDirectory(prefix='aac-parity-') as tmp:
            text = re.sub(r'\$\{FIXTURE_(\d+)\}', lambda m: tmp + '/root-' + m[1], json.dumps(row))
            case = json.loads(text)
            a = case['args']
            for old, (spelling, suffix) in a.pop('services', {}).items():
                service = 'Claude Code' + suffix + '-credentials-' + hashlib.sha256(unicodedata.normalize('NFC', spelling).encode()).hexdigest()[:8]
                if old in a['keychain']:
                    a['keychain'][service] = a['keychain'].pop(old)
            pathlib.Path(a['home']).mkdir(parents=True, exist_ok=True)
            for item in case['files']:
                p = pathlib.Path(item['path'])
                if not p.is_relative_to(tmp):
                    raise ValueError('Fixture write escaped temporary root')
                p.parent.mkdir(parents=True, exist_ok=True)
                if 'link' in item:
                    p.symlink_to(item['link'])
                elif item.get('directory'):
                    p.mkdir(exist_ok=True)
                elif item.get('fifo'):
                    os.mkfifo(p)
                else:
                    p.write_bytes(b' '*item['repeatSpaces'] if 'repeatSpaces' in item else item['text'].encode() if 'text' in item else base64.b64decode(item['bytes']))
                if 'link' not in item:
                    p.chmod(item['mode'])
            cwd = a.pop('cwd', a['home'])
            previous = os.getcwd()
            os.chdir(cwd)
            try:
                expected = python_reply(a)
            finally:
                os.chdir(previous)
            wire = {k:v for k,v in a.items() if k in ['home','ccsDir','nowMs']} if case.get('production') else a
            request = dict(protocol=1, verb=case['verb'], id='parity', deadlineMs=15000, args=normalize(wire))
            result = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, timeout=20, cwd=cwd)
            actual = terminal(result)
            target = {'fallback':'python'} if case.get('fallback') else normalize(expected)
            if target != actual or expected != case['expected']:
                print(json.dumps(dict(source=case['source'], python=expected, rust=actual, golden=case['expected']), indent=2))
                failures += 1
                if not witness: raise SystemExit(1)
                print("BASELINE RED:",case["source"])
            count += 1
    print(f'signin-expiry: {count} cases, {failures} differences')
    if production_only: return
    cases = json.loads((ROOT / 'tests/spec/agent/inventory.json').read_text())['cases']
    for case in cases:
        if witness and not case['source'].startswith('review-'):continue
        request = dict(protocol=1, verb='inventory', id='parity', deadlineMs=1000, args=case['args'])
        result = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, timeout=5)
        actual = terminal(result)
        if actual != ({'fallback':'python'} if case.get('fallback') else case['expected']):
            print(json.dumps(dict(source=case['source'], ts=case['expected'], rust=actual), indent=2))
            failures += 1
            if not witness: raise SystemExit(1)
            print('BASELINE RED:',case['source'],case['args']['probe'].strip())
    if failures: raise SystemExit(f'{failures} baseline regression cases fail as expected')
    print(f'PASS inventory: {len(cases)} TS golden/Rust cases')

if __name__ == '__main__':
    main()
