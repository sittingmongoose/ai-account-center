#!/usr/bin/env python3
"""Freeze real calls made by the existing unittest suite; no real-home access."""
import base64
import importlib.util
import inspect
import json
import os
import pathlib
import stat
import sys
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
import signin_expiry as helper
import failover_core
import desktop_helpers

NOW = 1_800_000_000_123
spec = importlib.util.spec_from_file_location('signin_tests', ROOT / 'tests/unit/account-usage/signin_expiry_test.py')
suite_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite_module)
suite_module.NOW = NOW
rows = []
active = False
current = ''
original_collect = helper.collect
original_fix = helper.fix_command
original_run = unittest.TestCase.run


def snapshot(base):
    out = []
    if not base.exists():
        return out
    for directory, dirs, files in os.walk(base, followlinks=False):
        for name in dirs + files:
            path = pathlib.Path(directory) / name
            info = path.lstat()
            item = {'path': str(path), 'mode': stat.S_IMODE(info.st_mode)}
            if stat.S_ISLNK(info.st_mode):
                item['link'] = os.readlink(path)
            elif stat.S_ISDIR(info.st_mode):
                item['directory'] = True
            elif stat.S_ISFIFO(info.st_mode):
                item['fifo'] = True
            elif stat.S_ISREG(info.st_mode):
                try:
                    item['text'] = path.read_text()
                except UnicodeError:
                    item['text'] = None
                if item['text'] is None:
                    del item['text']
                    item['bytes'] = base64.b64encode(path.read_bytes()).decode()
            else:
                continue
            out.append(item)
    return out


def collect(home, now_ms, include_keychain=True, ccs_dir=None):
    global active
    if active:
        return original_collect(home, now_ms, include_keychain, ccs_dir)
    active = True
    env = {key: os.environ[key] for key in helper.SELECTOR_ORDER if key in os.environ}
    files = snapshot(home)
    ccs = ccs_dir or home / '.ccs'
    if not str(ccs).startswith(str(home) + '/'):
        files += [{'path': str(ccs), 'directory': True, 'mode': 0o700}] + snapshot(ccs)
    keychain = {}
    states = {}
    reads = {}
    saved = []
    discovery_incomplete = False
    old_keychain = helper._mac_keychain_text
    old_state = helper._dir_state
    old_read = helper._read_regular_bytes
    old_saved = helper._saved_roots

    def key(service=helper.KEYCHAIN_PLAIN_SERVICE):
        answer = old_keychain(service)
        if answer is helper.KEYCHAIN_LOCKED:
            item = {'status': 'locked'}
        elif answer is helper.KEYCHAIN_INDETERMINATE:
            item = {'status': 'indeterminate'}
        elif answer is helper.KEYCHAIN_ANSWERED_UNREADABLE:
            item = {'status': 'oversized'}
        elif answer is None:
            item = None
        else:
            item = {'status': 'payload', 'text': answer}
        keychain[service] = item
        return answer

    def state(path):
        answer = old_state(path)
        states[str(path)] = list(answer)
        return answer

    def read(path, limit=65536):
        answer = old_read(path, limit)
        reads[str(path)] = None if answer is None else base64.b64encode(answer).decode()
        return answer

    def saved_roots(home, ccs):
        nonlocal discovery_incomplete
        before = (desktop_helpers.total_read_failures(), failover_core.SETTINGS_HOME_FAILURES[0])
        answer = old_saved(home, ccs)
        discovery_incomplete = before != (desktop_helpers.total_read_failures(), failover_core.SETTINGS_HOME_FAILURES[0])
        saved.append(answer)
        return answer

    try:
        with mock.patch.object(helper, '_mac_keychain_text', side_effect=key), mock.patch.object(helper, '_dir_state', side_effect=state), mock.patch.object(helper, '_read_regular_bytes', side_effect=read), mock.patch.object(helper, '_saved_roots', side_effect=saved_roots):
            result = original_collect(home, now_ms, include_keychain, ccs_dir)
        services = {}
        if sys.platform == 'darwin':
            for _label, root, kind, spelling in helper.claude_roots(home, ccs, include_ambient=include_keychain)[0]:
                if kind != 'default' and spelling:
                    for suffix in ['', '-custom-oauth']:
                        service = helper._keychain_service_for_root(root, kind, suffix, spelling=spelling)
                        if service is not None:
                            services[service] = [spelling, suffix]
        # Save only injected discovery boundaries. Ordinary cases exercise Rust's own discovery.
        injected = isinstance(old_saved, mock.Mock) or isinstance(failover_core._roots, mock.Mock) or sys.modules.get('failover_core') is None
        args = {'home': str(home), 'ccsDir': str(ccs), 'nowMs': now_ms, 'platform': sys.platform, 'environment': env, 'includeKeychain': include_keychain, 'keychain': keychain, 'services': services, 'cwd': os.getcwd() if str(os.getcwd()).startswith(str(home)) else str(home), 'io': {'states': states, 'reads': reads, 'discoveryIncomplete': discovery_incomplete}}
        if injected:
            args['io']['savedRoots'] = None if not saved or saved[0] is None else [[str(p), selectors] for p, selectors in saved[0]]
        rows.append({'source': current, 'verb': 'signin-expiry', 'args': args, 'files': files, 'expected': result})
        return result
    finally:
        active = False


def fix(*args, **kwargs):
    result = original_fix(*args, **kwargs)
    if not active:
        bound = inspect.signature(original_fix).bind(*args, **kwargs)
        bound.apply_defaults()
        values = bound.arguments
        rows.append({'source': current, 'verb': 'signin-expiry', 'args': {'op': 'fix', 'home': str(values['home']), 'path': str(values['config_dir']), 'storageVar': values['storage_var'], 'spelling': values['spelling'], 'platform': sys.platform, 'environment': {k: os.environ[k] for k in helper.SELECTOR_ORDER if k in os.environ}}, 'files': [], 'expected': result})
    return result


def run(self, result=None):
    global current
    current = self.id().split('.')[-1]
    return original_run(self, result)

helper.collect = collect
helper.fix_command = fix
unittest.TestCase.run = run
result = unittest.TextTestRunner(verbosity=0).run(unittest.defaultTestLoader.loadTestsFromModule(suite_module))
if not result.wasSuccessful():
    raise SystemExit('Existing Python tests failed during capture')
# Every original test is named, including direct helper/safety checks kept unchanged in Python.
source_tests = sorted(name for name in vars(suite_module.SignInExpiryTests) if name.startswith('test_'))
text = json.dumps({'schemaVersion': 1, 'nowMs': NOW, 'sourceTests': source_tests, 'cases': rows}, ensure_ascii=True, indent=2, allow_nan=False)
# Each call has a fresh temporary home. Replace all captured fixture roots, including relocated
# scopes, in JSON before replay; never rewrite arbitrary expected timestamps or token fields.
import re
paths = sorted(set(re.findall(r'/tmp/tmp[a-zA-Z0-9_-]+', text)), key=len, reverse=True)
for index, path in enumerate(paths):
    text = text.replace(path, '${FIXTURE_' + str(index) + '}')
(ROOT / 'tests/spec/agent/signin-expiry.json').write_text(text + '\n')
print(f'{len(rows)} sign-in cases from {len(source_tests)} unchanged Python tests')
