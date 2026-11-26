#!/usr/bin/env python3
"""Offline OMP SQLite/Safari binarycookies parity against plan_common/plan_usage.

Run with a parity-fixtures agent. All HTTP is redirected to loopback; fixture
homes are retained under the printed directory for the lane lead to clean up.
"""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import uuid
from unittest import mock

import quota as q


def cookies(records):
    encoded = []
    for domain, name, path, value, expiry in records:
        record = bytearray(56)
        for field, text in zip((16, 20, 24, 28), (domain, name, path, value)):
            struct.pack_into('<I', record, field, len(record))
            record.extend(text.encode() + b'\0')
        struct.pack_into('<I', record, 0, len(record))
        struct.pack_into('<d', record, 40, expiry)
        encoded.append(record)
    page = bytearray(b'\0\0\1\0' + struct.pack('<I', len(encoded)))
    offset = 12 + 4 * len(encoded)
    for record in encoded:
        page.extend(struct.pack('<I', offset))
        offset += len(record)
    page.extend(b'\0' * 4)
    for record in encoded:
        page.extend(record)
    return b'cook' + struct.pack('>II', 1, len(page)) + page


def main():
    binary = str(Path(sys.argv[1]).resolve())
    root = Path(tempfile.mkdtemp(prefix='aac-u6b-readers-', dir=Path.home() / 'aac-lanes'))
    print('Retained fixtures:', root, flush=True)
    server = q.http.server.ThreadingHTTPServer(('127.0.0.1', 0), q.Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    endpoint = 'http://127.0.0.1:' + str(server.server_port)
    count = 0
    failures = []

    def check(name, provider, setup, replies, expected_status='ok'):
        nonlocal count
        home = root / name
        home.mkdir()
        tracked = setup(home)
        before = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in tracked}
        client = q.common.HttpClient()
        client.opener = q.LocalOpener(client.opener, endpoint)
        q.Handler.queue = replies.copy(); q.Handler.calls = []
        with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(q.usage, 'utc_now', return_value=q.STAMP):
            reference = q.usage.collect(provider, 'mac', home=home, client=client)
        expected_calls = q.Handler.calls.copy()
        q.Handler.queue = replies.copy(); q.Handler.calls = []
        request = {'protocol': 1, 'verb': 'quota-read', 'id': name, 'deadlineMs': 15000,
                   'args': {'provider': provider, 'platform': 'mac', 'home': str(home), 'endpoint': endpoint, 'now': q.NOW}}
        process = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True,
                                 timeout=20, check=True, env={'PATH': os.defpath, 'HOME': str(home), 'CCS_HOME': str(home / '.ccs')})
        event = json.loads(process.stdout)
        assert event['type'] == 'result' and event['id'] == name and event['protocol'] == 1, name
        value = event['value']
        # Report field names, never credential values, even on a failed assertion.
        assert value == reference, (name, 'reply differs', [k for k in set(value) | set(reference) if value.get(k) != reference.get(k)])
        assert value['status'] == expected_status, (name, 'status')
        actual_calls = q.Handler.calls.copy()
        assert len(actual_calls) == len(expected_calls), (name, 'request count')
        for (method, headers, body), (ref_method, ref_headers, ref_body) in zip(actual_calls, expected_calls):
            actual = {k.lower(): v for k, v in headers.items()}
            expected = {k.lower(): v for k, v in ref_headers.items()}
            assert method == ref_method, (name, 'method')
            for key in ('authorization', 'cookie', 'origin', 'referer', 'x-requested-with', 'accept', 'x-aac-fixture-url'):
                assert actual.get(key) == expected.get(key), (name, key)
            if body or ref_body:
                actual_form = q.urllib.parse.parse_qs(body.decode())
                expected_form = q.urllib.parse.parse_qs(ref_body.decode())
                for form in (actual_form, expected_form):
                    if 'params' in form:
                        form['params'] = json.loads(form['params'][0])
                for form in (actual_form, expected_form):
                    cornerstone = form.get('params', {}).get('Data', {}).get('cornerstoneParam', {})
                    if 'feTraceId' in cornerstone:
                        # Both readers create their own per-request trace ID.
                        uuid.UUID(cornerstone.pop('feTraceId'))
                assert actual_form == expected_form, (name, 'body')
        assert before == {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in tracked}, (name, 'store modified')
        assert 'fixture-' not in process.stdout + process.stderr, (name, 'secret exposed')
        count += 1
        print('PASS', name, flush=True)

    def omp(rows):
        def setup(home):
            path = home / '.omp/agent/agent.db'
            path.parent.mkdir(parents=True)
            with sqlite3.connect(path) as db:
                db.execute('CREATE TABLE auth_credentials (id INTEGER PRIMARY KEY, provider TEXT, credential_type TEXT, data TEXT, disabled_cause TEXT)')
                db.executemany('INSERT INTO auth_credentials VALUES (?,?,?,?,?)',
                               [(i, p, kind, json.dumps(data), disabled) for i, p, kind, data, disabled in rows])
            return [path]
        return setup

    def safari(data):
        def setup(home):
            path = home / 'Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies'
            path.parent.mkdir(parents=True)
            path.write_bytes(data)
            return [path]
        return setup

    qwen = [q.response({'data': {'secToken': 'fixture-sec'}}), q.response(q.QWEN)] + [q.response({'data': {}})] * 3
    cases = []
    for provider, payload in [('opencode-go', q.GO), ('kimi-code', q.KIMI), ('zai', q.ZAI), ('qwen', q.QWEN)]:
        alias = 'alibaba-token-plan' if provider == 'qwen' else provider
        secret = json.dumps({'cookie': 'fixture-session=yes'}) if provider == 'qwen' else 'fixture-new-key'
        rows = [(1, alias, 'api_key', {'key': 'fixture-old-key'}, None),
                (2, alias, 'oauth', {'access': secret, 'email': 'reader@example.test'}, None),
                (3, alias, 'api_key', {'key': 'fixture-disabled-key'}, 'disabled'),
                (4, 'unrelated', 'api_key', {'key': 'fixture-other-key'}, None)]
        replies = qwen if provider == 'qwen' else [q.response(payload)] + ([q.response(q.PACKS)] if provider == 'zai' else [])
        cases.append(('omp-'+provider, provider, omp(rows), replies))
    cases += [('omp-expired-skip', 'kimi-code', omp([
        (1, 'kimi-code', 'oauth', {'access': 'fixture-good', 'expires': '4102444800000'}, None),
        (2, 'kimi-code', 'oauth', {'access': 'fixture-expired', 'expires': '1'}, None)]), [q.response(q.KIMI)]),
        ('omp-expired-only', 'kimi-code', omp([(1, 'kimi-code', 'oauth', {'access': 'fixture-expired', 'expires': 1}, None)]), [], 'needs_sign_in'),
        ('omp-limit', 'opencode-go', omp([(1, 'opencode-go', 'api_key', {'key': 'fixture-old'}, None)] +
            [(i, 'opencode-go', 'api_key', {}, None) for i in range(2, 22)]), [], 'unavailable')]
    future = 4102444800 - 978307200
    base = [('.qwencloud.com', 'a_session', '/', 'fixture-domain', future),
            ('home.qwencloud.com', 'z_host', '/tool', 'fixture-host', 0),
            ('cs-data.qwencloud.com', 'gateway', '/data', 'fixture-gateway', future)]
    cases += [('safari-intl-order', 'qwen', safari(cookies(base)), qwen),
              ('safari-cn', 'qwen', safari(cookies([('.aliyun.com', 'session', '/', 'fixture-cn', future)])),
               [(200, {}, b'<script>SEC_TOKEN: "fixture-cn-sec"</script>')] + qwen[1:]),
              ('safari-scoping', 'qwen', safari(cookies(base + [
                  ('home.qwencloud.com', 'a_session', '/', 'fixture-specific', future),
                  ('qwencloud.com', 'host_only', '/', 'fixture-excluded', future),
                  ('.qwencloud.com', 'expired', '/', 'fixture-expired', 1),
                  ('.qwencloud.com', 'wrong_path', '/tool/other', 'fixture-wrong-path', future),
                  ('.evil.test', 'unrelated', '/', 'fixture-other-domain', future),
                  ('.qwencloud.com', 'bad_value', '/', 'fixture-bad;value', future)])), qwen),
              ('safari-truncated', 'qwen', safari(cookies(base)[:-5]), [], 'unavailable'),
              ('safari-bad-magic', 'qwen', safari(b'bad!'+cookies(base)[4:]), [], 'unavailable')]
    # A malformed record invalidates the complete Python page, including valid earlier records.
    bad = bytearray(cookies(base))
    last_offset = struct.unpack_from('<I', bad, 12 + 8 + 2*4)[0]
    struct.pack_into('<I', bad, 12 + last_offset + 20, 1)
    cases.append(('safari-invalid-string', 'qwen', safari(bad), [], 'unavailable'))
    try:
        for case in cases:
            try:
                check(*case)
            except AssertionError as error:
                failures.append(str(error))
                print('FAIL', error, flush=True)
    finally:
        server.shutdown()
        server.server_close()
    print(f'reader parity: {count} equal, {len(failures)} failures, 0 accepted fallbacks')
    assert not failures, failures


if __name__ == '__main__':
    main()
