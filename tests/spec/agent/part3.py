#!/usr/bin/env python3
"""M2 part 3: original helper tests and same-home, real-file agent parity.

Only test-owned homes are passed to the agent. No environment HOME is changed.
Every terminal envelope is checked. Unsupported inputs must fall back or fail
without publishing a value. Key files and modes are compared after each write.
"""
import base64
import copy
import datetime as dt
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
import key_store as key
from run import terminal

COUNTS = {v: {'equal': 0, 'fallback': 0, 'refusal': 0} for v in ['key-store', 'claude-history', 't3-pins']}
BINARY = None


def call(verb, args, failure=None):
    result = subprocess.run([BINARY], input=json.dumps(dict(protocol=1, verb=verb,
        id='parity', deadlineMs=15000, args=args))+'\n', text=True,
        capture_output=True, timeout=20)
    assert not result.stderr, 'agent emitted diagnostics'
    if result.returncode:
        event = json.loads(result.stdout)
        assert result.returncode == 1 and event['type'] == 'failure' and event['protocol'] == 1 and event['id'] == 'parity'
        assert set(event) == {'type', 'protocol', 'id', 'failure'}
        if failure is not None:
            assert (event['failure']['cause'], event['failure']['reason']) == failure
        return None
    return terminal(result)


def compare(verb, actual, expected, refused=False):
    if actual == {'fallback': 'python'}:
        COUNTS[verb]['fallback'] += 1
    elif refused:
        assert actual is None, (verb, 'refusal became a success')
        COUNTS[verb]['refusal'] += 1
    else:
        # Do not print private-shaped data on mismatch, including fixture keys.
        assert actual == expected, (verb, 'reply differs from reference')
        COUNTS[verb]['equal'] += 1


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def image(home):
    result = {}
    for root, dirs, files in os.walk(home, followlinks=False):
        for name in dirs + files:
            p = Path(root) / name
            meta = p.lstat()
            result[str(p.relative_to(home))] = (stat.S_IMODE(meta.st_mode),
                ('link', os.readlink(p)) if p.is_symlink() else ('dir',) if p.is_dir() else ('file', p.read_bytes()))
    return result


def restore(home, saved):
    for p in home.iterdir():
        if p.is_symlink() or not p.is_dir(): p.unlink()
        else: shutil.rmtree(p)
    for name, (mode, item) in sorted(saved.items(), key=lambda pair: len(Path(pair[0]).parts)):
        p = home / name
        if item[0] == 'dir': p.mkdir(exist_ok=True)
        elif item[0] == 'link': p.symlink_to(item[1])
        else: p.write_bytes(item[1])
        if item[0] != 'link': p.chmod(mode)


def key_parity():
    suite_module = load('key_store_reference', 'tests/unit/account-usage/key_store_test.py')
    original_put, original_delete = key.put, key.delete
    instant = dt.datetime(2026, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc)

    def wrap(action, original):
        def invoke(provider, key_id, *args, **kwargs):
            home = Path(kwargs['home'])
            assert home.name.startswith('aac-key-store-') and home.parent == Path(tempfile.gettempdir())
            before = image(home)
            expected = None
            error = None
            if action == 'put': kwargs['now'] = instant
            try: expected = original(provider, key_id, *args, **kwargs)
            except (key.StoreError, OSError, ValueError) as caught: error = caught
            after = image(home)
            restore(home, before)
            request = dict(action=action, provider=provider, keyId=key_id, home=str(home), nowMs=int(instant.timestamp()*1000))
            if action == 'put': request['secret'] = args[0]
            if kwargs.get('windows'): request['platform'] = 'windows'  # Explicitly unqualified: whole-verb fallback.
            actual = call('key-store', request)
            compare('key-store', actual, expected, error is not None)
            assert image(home) == (before if actual == {'fallback':'python'} else after), 'key file/mode/atomic cleanup parity'
            if actual == {'fallback':'python'}: restore(home, after)
            if error: raise error
            return expected
        return invoke

    with mock.patch.object(key, 'put', wrap('put', original_put)), mock.patch.object(key, 'delete', wrap('delete', original_delete)):
        result = unittest.TextTestRunner(verbosity=0).run(unittest.defaultTestLoader.loadTestsFromModule(suite_module))
    assert result.wasSuccessful()
    # Exercise stdin grammar and boundaries, including the existing source test vectors.
    with tempfile.TemporaryDirectory(prefix='aac-key-store-') as tmp:
        for raw in [b'', b'short', b'has space inside', b'fixture-secret\n\n', b'\xff'*10,
                    b'x'*5000, b'x'*8, b'x'*512, b'x'*513, b'fixture-secret\n', b'fixture-secret\r\n']:
            try:
                secret = key.read_secret(io.BytesIO(raw))
                expected = original_put('zai', '12345678', secret, home=tmp, windows=False, now=instant)
                refused = False
            except key.StoreError: expected, refused = None, True
            # Non-ASCII cannot cross the proved wire grammar and must delegate.
            text = raw.decode('latin1')
            actual = call('key-store', dict(action='put', provider='zai', keyId='12345678', secret=text, home=tmp, nowMs=int(instant.timestamp()*1000)))
            compare('key-store', actual, expected, refused)
            if len(text)>=8: assert text not in json.dumps(actual), 'secret reflected'


def history_parity():
    module = load('history_reference_tests', 'tests/unit/claude-history/test_history_remote.py')
    helper = module.h
    original = helper.BoundProfile.collect
    active = False

    def collect(bound, *args, **kwargs):
        nonlocal active
        if active: return original(bound, *args, **kwargs)
        active = True
        try:
            expected, error = None, None
            try: expected = original(bound, *args, **kwargs)
            except (helper.Refused, OSError, ValueError, TypeError) as caught: error = caught
            assert bound.home.name.startswith('history-helper-fixture-') and bound.home.parent == Path(tempfile.gettempdir())
            fixture = dict(home=str(bound.home), closed=True, native=bound.native())
            args = dict(mode='collect', profileId=bound.profile, platform=bound.platform, policy=bound.policy, _historyFixture=fixture)
            # Injected IO/races are outside the real-file native subset. The complete verb
            # delegates; no IO mock is silently replaced with a stable filesystem answer.
            if helper.read_regular is not original_read or kwargs.get('owned_candidates'):
                args['unprovenIo'] = True
            actual = call('claude-history', args)
            compare('claude-history', actual, expected, error is not None)
            if error: raise error
            return expected
        finally: active = False

    original_read = helper.read_regular
    original_unchanged = helper.BoundProfile.unchanged
    def unchanged(bound, expected, candidates=()):
        value, error = None, None
        try: value = original_unchanged(bound, expected, candidates)
        except (helper.Refused, OSError, ValueError, TypeError) as caught: error = caught
        try: closed = bound.closed()
        except helper.Refused: closed = None
        args = dict(mode='protected-check', profileId=bound.profile, platform=bound.platform,
            policy=bound.policy, expectedTarget=expected, records=list(candidates),
            _historyFixture=dict(home=str(bound.home),closed=closed,native=bound.native()))
        if helper.read_regular is not original_read: args['unprovenIo'] = True
        actual = call('claude-history', args)
        compare('claude-history',actual,{'unchanged':value},error is not None)
        if error: raise error
        return value
    with mock.patch.object(helper.BoundProfile, 'collect', collect), mock.patch.object(helper.BoundProfile, 'unchanged', unchanged):
        result = unittest.TextTestRunner(verbosity=0).run(unittest.defaultTestLoader.loadTestsFromModule(module))
    assert result.wasSuccessful()
    # Real-file witnesses for Python splitlines and a dangling pending-directory link.
    for variant in ['vertical-tab', 'form-feed', 'bare-cr', 'dangling-waiting']:
        fixture_case = module.Fixtures()
        fixture_case.setUp()
        try:
            if variant == 'dangling-waiting':
                (fixture_case.registry / 'waiting-input').symlink_to(fixture_case.home / 'missing')
            else:
                separator = {'vertical-tab':'\v','form-feed':'\f','bare-cr':'\r'}[variant]
                p = fixture_case.home / '.ssh/config'
                p.write_text('# comment' + separator + 'ProxyJump fixture-invalid\n' + p.read_text())
            try:
                expected = original(fixture_case.bound)
                refused = False
            except (helper.Refused, OSError, ValueError):
                expected, refused = None, True
            args = dict(mode='collect', profileId='gmail', platform='mac', policy=fixture_case.policy,
                        _historyFixture=dict(home=str(fixture_case.home),closed=True,native=fixture_case.native))
            compare('claude-history',call('claude-history',args),expected,refused)
        finally:
            fixture_case.tearDown()
    # REVIEW-M2-part3-cheap M1: policy text Python's clean_text refuses never becomes a success.
    for root in ['/home/fixture/\tprojects', '/home/fixture/\nprojects', '/' + 'a' * 4096, '/home/fixture/\x7fprojects']:
        fixture_case = module.Fixtures()
        fixture_case.setUp()
        try:
            policy = json.loads(json.dumps(fixture_case.policy))
            policy['project']['transcriptRoot'] = root
            try:
                helper.BoundProfile('gmail', 'mac', policy, home=fixture_case.home, closed=lambda: True,
                                    native=lambda: fixture_case.native).collect()
                raise AssertionError('reference accepted invalid policy text')
            except helper.Refused:
                pass
            args = dict(mode='collect', profileId='gmail', platform='mac', policy=policy,
                        _historyFixture=dict(home=str(fixture_case.home),closed=True,native=fixture_case.native))
            compare('claude-history', call('claude-history', args), None, True)
        finally:
            fixture_case.tearDown()
    # Every other mode is an exact fallback before mutation. Original transaction/marker
    # suites separately verify the unchanged authoritative writer and its acknowledgements.
    for mode in ['append','protected-check','verify-transcripts','snapshot-cleanup']:
        actual = call('claude-history', dict(mode=mode))
        compare('claude-history', actual, None)
    actual = call('claude-history', dict(mode='collect', title='\u0085'))
    compare('claude-history',actual,None)


def pins_parity():
    source = (ROOT / 'src/web-server/status/status-collectors.ts').read_text()
    query = source.split('export const PINNED_THREADS_QUERY = `',1)[1].split('`;',1)[0]
    cases = [[], [('one', 'Fixture', {'pinnedAt':'2026-01-01','pinOrderKey':'a'})],
        [('child','Child',{'pinnedAt':1,'lineage':{'parentThreadId':'parent'}}),
         ('null','No key',{'pinnedAt':2,'pinOrderKey':None}),
         ('missing','Missing key',{'pinnedAt':3}),
         ('first','First',{'pinnedAt':1,'pinOrderKey':1})],
        [('unicode','\u00e9',{'pinnedAt':1})],
        [('shape','Bad lineage',{'pinnedAt':1,'lineage':[]})],
        [('large','x'*1_048_577,{'pinnedAt':1})],
        [(str(i),'Fixture',{'pinnedAt':i,'pinOrderKey':i}) for i in range(1001)]]
    for values in cases:
        with tempfile.TemporaryDirectory(prefix='aac-pins-') as tmp:
            path = Path(tmp)/'fixture.sqlite'
            conn = sqlite3.connect(path)
            conn.execute('CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT,title TEXT,archived_at TEXT,deleted_at TEXT,payload_json TEXT)')
            conn.executemany('INSERT INTO orchestration_v2_projection_threads VALUES (?,?,NULL,NULL,?)',[(i,t,json.dumps(p)) for i,t,p in values])
            conn.commit()
            conn.row_factory = sqlite3.Row
            rows = [dict(r) for r in conn.execute(query)]
            conn.close()
            before = path.read_bytes()
            expected = rows
            refused = any(r['lineageType'] not in (None,'null','object')
                          or r['parentType'] not in (None,'null','text') for r in rows)
            limited = any(len(t)>1_048_576 for _,t,_ in values) or len(values)>1000
            if limited: refused=True
            failure = ('output_too_large','limit') if limited else ('damaged','shape') if refused else None
            actual = call('t3-pins', {'dbPath':str(path)}, failure)
            compare('t3-pins',actual,expected,refused)
            assert path.read_bytes()==before and sorted(p.name for p in Path(tmp).iterdir())==['fixture.sqlite'], 'pins wrote database sidecars'

    with tempfile.TemporaryDirectory(prefix='aac-pins-') as tmp:
        path = Path(tmp)/'missing.sqlite'
        compare('t3-pins',call('t3-pins',{'dbPath':str(path)},('not_installed','missing')),None,True)
        assert list(Path(tmp).iterdir())==[]
        conn = sqlite3.connect(path)
        conn.execute('CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT,title TEXT,archived_at TEXT,deleted_at TEXT,payload_json TEXT)')
        conn.commit()
        conn.execute('BEGIN EXCLUSIVE')
        try:
            compare('t3-pins',call('t3-pins',{'dbPath':str(path)},('refused','io')),None,True)
        finally:
            conn.rollback()
            conn.close()


def main(binary):
    global BINARY
    BINARY = str(Path(binary).resolve())
    key_parity()
    production = '--production' in sys.argv
    if not production:
        history_parity()
    else:
        # Fixture-only seams must be rejected before accessing any private home.
        compare('claude-history', call('claude-history', {'mode':'collect','_historyFixture':{'home':'/nonexistent/aac-fixture'}}), None)
    pins_parity()
    for verb, counts in COUNTS.items():
        if not production or verb != 'claude-history':
            assert counts['equal']>0, (verb,'no native successes')
        print(verb, json.dumps(counts,sort_keys=True), '0 differences')

if __name__ == '__main__': main(sys.argv[1])
