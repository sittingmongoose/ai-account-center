#!/usr/bin/env python3
"""Offline plan parity: Python and Rust hit the same loopback response queue.

No ambient home, environment credentials, redirects, or real hosts are used.
Unsupported grammar is accepted only as the exact whole-verb fallback.
"""
import contextlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import urllib.parse
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
import plan_usage as usage
import plan_common as common

NOW = 1767225600
STAMP = '2026-01-01T00:00:00Z'
GO = {'usage': {k: {'status': 'ok', 'percent': 25, 'resetsAt': STAMP} for k in ('rolling', 'weekly', 'monthly')}}
KIMI = {'usage': {'limit': 100, 'used': 25, 'resetTime': STAMP}, 'user': {'membership': {'level': 'LEVEL_BASIC'}}}
ZAI = {'success': True, 'code': 200, 'data': {'level': 'pro', 'limits': [{'type': 'TOKENS_LIMIT', 'number': 5, 'unit': 3, 'usage': 100, 'currentValue': 25, 'nextResetTime': STAMP}]}}
PACKS = {'success': True, 'data': {'fiveHourResets': [], 'weekResets': []}}
QWEN = {'per5HourPercentage': 1, 'per5HourResetTime': STAMP}

class Handler(http.server.BaseHTTPRequestHandler):
    queue = []
    calls = []
    def log_message(self, *_):
        pass
    def do_POST(self):
        self.request_body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.do_GET()
    def do_GET(self):
        self.calls.append((self.command, dict(self.headers), getattr(self, "request_body", b"")))
        item = self.queue.pop(0) if self.queue else (500, {}, b'')
        code, headers, body = item
        if code == 'timeout':
            time.sleep(4.2)
            return
        stream = code == 'stream'
        if code == 'disconnect':
            self.close_connection = True
            return
        self.send_response(200 if stream else code)
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        with contextlib.suppress(BrokenPipeError, ConnectionResetError):
            if stream:
                for i in range(6):
                    if i: time.sleep(1)
                    self.wfile.write(body[len(body)*i//6:len(body)*(i+1)//6])
                    self.wfile.flush()
            else:
                self.wfile.write(body)

class LocalOpener:
    def __init__(self, opener, endpoint):
        self.opener, self.endpoint = opener, endpoint
    def open(self, request, timeout):
        replacement = urllib.request.Request(self.endpoint, data=request.data, headers=dict(request.header_items()))
        replacement.add_header("x-aac-fixture-url", request.full_url)
        return self.opener.open(replacement, timeout=timeout)

def response(payload):
    return (200, {}, json.dumps(payload).encode())

def main():
    binary = str(Path(sys.argv[1]).resolve())
    if len(sys.argv) > 2 and sys.argv[2] == '--production':
        with tempfile.TemporaryDirectory(prefix='aac-quota-prod-') as directory:
            home = Path(directory)
            count = 0
            for provider in common.PROVIDERS:
                args = {'provider': provider, 'platform': 'ubuntu', 'home': str(home)}
                base = args.copy()
                for injection in ({}, {'endpoint': 'http://127.0.0.1:1'}, {'now': NOW},
                                  {'endpoint': 'http://127.0.0.1:1', 'now': NOW}):
                    args = {**base, **injection}
                    request = {'protocol': 1, 'verb': 'quota-read', 'id': 'production', 'deadlineMs': 1000, 'args': args}
                    p = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, check=True, env={'PATH': os.defpath})
                    event = json.loads(p.stdout)
                    assert event['type'] == 'result' and event['id'] == 'production' and event['protocol'] == 1
                    with mock.patch.dict(os.environ, {}, clear=True):
                        reference = usage.collect(provider, 'ubuntu', home=home)
                    assert event['value'] == ({'fallback': 'python'} if injection else reference), (
                        provider, sorted(injection),
                        'production mode requires a binary built WITHOUT --features parity-fixtures')
                    count += 1
        print(f'{count} production empty-home/injection-refusal cases passed')
        return
    equal = fallback = 0
    fallback_cases = []
    live_providers = set()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    endpoint = 'http://127.0.0.1:' + str(server.server_port)
    try:
        with tempfile.TemporaryDirectory(prefix='aac-quota-') as directory:
            home = Path(directory)
            auth = home / '.local/share/opencode/auth.json'
            auth.parent.mkdir(parents=True)
            for provider, payload in [('opencode-go', GO), ('kimi-code', KIMI), ('zai', ZAI), ('qwen', QWEN)]:
                alias = {'kimi-code': 'kimi-for-coding', 'qwen': 'alibaba-token-plan'}.get(provider, provider)
                auth.write_text(json.dumps({alias: {'type': 'api', 'key': 'fixture-dummy-token'}}))
                variants = [('success', response(payload)), ('401', (401, {}, b'private')), ('403', (403, {}, b'private')), ('500', (500, {}, b'private')), ('timeout', ('timeout', {}, b'')), ('malformed', (200, {}, b'{')), ('oversized', (200, {}, b'x' * (common.MAX_BYTES + 1))), ('array', response([]))]
                variants += [('retry-' + str(i), (429, {'Retry-After': value}, b'private')) for i, value in enumerate(['0', '30', '999999999999', 'Thu, 01 Jan 2026 01:00:00 GMT', 'nonsense', '1.5', '-2'])]
                variants += [('unicode', response({'usage': {'weekly': {'used': '\u0085'}}})), ('fraction', response({'usage': {'used': 0.5, 'limit': 1}}))]
                # A plan the table does not list yet: Python and Rust keep the same provider name.
                if provider == 'kimi-code':
                    variants += [('level-' + level, response({**KIMI, 'version': 'GOODS_VERSION_V2', 'user': {'membership': {'level': level}}}))
                                 for level in ('LEVEL_ADVANCED', 'LEVEL_ULTRA_MAX', 'LEVEL_lower', 'LEVEL_A_B_C_D')]
                    variants += [('booster', response({**KIMI, 'boosterWallet': {
                        'balance': {'type': 'BOOSTER', 'amount': '100000000', 'amountLeft': '25000000'},
                        'monthlyChargeLimit': {'priceInCents': '5000', 'currency': 'USD'},
                        'monthlyUsed': {'priceInCents': '1250', 'currency': 'USD'},
                        'monthlyChargeLimitEnabled': True}}))]
                    variants += [('transitional-overlap', response({**KIMI,
                        'usage': {'limit': 100, 'used': 25, 'resetTime': STAMP},
                        'usages': {'limit_7d': {'used_ratio': 0, 'reset_time': STAMP}}}))]
                if provider == 'zai':
                    variants += [('level-' + str(i), response({**ZAI, 'data': {**ZAI['data'], 'level': level}}))
                                 for i, level in enumerate(('max', 'Coder', 'max_plus', 'Pro 2', ' pro', 'Plan 20261119', 'x' * 17, 'sk-' + 'a' * 40))]
                    large = {'type': 'TIME_LIMIT', 'number': '18014398509481985', 'unit': 5, 'usage': '100.5', 'currentValue': '25.25', 'nextResetTime': 1767225600000}
                    variants += [('large-count-key', response({**ZAI, 'data': {**ZAI['data'], 'limits': [large]}}))]
                for name, first in variants:
                    replies = [first] + ([response(PACKS)] if provider == 'zai' else [])
                    Handler.queue = replies.copy()
                    Handler.calls = []
                    client = common.HttpClient()
                    client.opener = LocalOpener(client.opener, endpoint)
                    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(common, 'utc_now', return_value=STAMP), mock.patch.object(usage, 'utc_now', return_value=STAMP):
                        reference = usage.collect(provider, 'ubuntu', home=home, client=client)
                    py_calls = Handler.calls.copy()
                    Handler.queue = replies.copy()
                    Handler.calls = []
                    request = {'protocol': 1, 'verb': 'quota-read', 'id': 'quota-parity', 'deadlineMs': 15000, 'args': {'provider': provider, 'platform': 'ubuntu', 'home': str(home), 'endpoint': endpoint, 'now': NOW}}
                    process = subprocess.run([binary], input=json.dumps(request) + '\n', text=True, capture_output=True, timeout=20, env={'PATH': os.defpath})
                    assert process.returncode == 0, (provider, name, 'nonzero exit')
                    event = json.loads(process.stdout)
                    assert event['protocol'] == 1 and event['id'] == 'quota-parity' and event['type'] == 'result'
                    value = event['value']
                    assert 'fixture-dummy-token' not in process.stdout + process.stderr
                    if value == {'fallback': 'python'}:
                        fallback += 1; fallback_cases.append((provider,name))
                    else:
                        assert value == reference, (provider, name, value, reference)
                        # Request auth and fixed headers must match as well as parsed replies.
                        assert len(Handler.calls) == len(py_calls), (provider, name, 'request count')
                        for (_, actual, _), (_, expected, _) in zip(Handler.calls, py_calls):
                            actual = {k.lower(): v for k, v in actual.items()}
                            expected = {k.lower(): v for k, v in expected.items()}
                            for key in ('authorization', 'x-opencode-session', 'accept', 'user-agent', 'content-type', 'x-aac-fixture-url'):
                                assert actual.get(key) == expected.get(key), (provider, name, key)
                        if value["status"] == "ok": live_providers.add(provider)
                        equal += 1
            # International Qwen uses its existing packed console session and POSTs.
            auth.write_text(json.dumps({'alibaba-token-plan': {'type': 'api', 'key': json.dumps({'cookie': 'fixture-cookie=made-up'})}}))
            for optional in (False, True, "fraction"):
                extra = {"data": {"version": 1.25}} if optional == "fraction" else {"data": {}}
                replies = [response({'data': {'secToken': 'fixture-sec'}}), response(QWEN)]
                replies += [response(extra) if optional else (500, {}, b'private')] * 3
                Handler.queue = replies.copy()
                Handler.calls = []
                client = common.HttpClient()
                client.opener = LocalOpener(client.opener, endpoint)
                with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(usage, 'utc_now', return_value=STAMP):
                    reference = usage.collect('qwen', 'ubuntu', home=home, client=client)
                py_calls = Handler.calls.copy()
                Handler.queue = replies.copy()
                Handler.calls = []
                request['args'] = {'provider': 'qwen', 'platform': 'ubuntu', 'home': str(home), 'endpoint': endpoint, 'now': NOW}
                process = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, check=True, env={'PATH': os.defpath})
                event = json.loads(process.stdout)
                assert event['type'] == 'result' and event['id'] == request['id'] and event['protocol'] == 1
                value = event['value']
                if value == {'fallback': 'python'}: fallback += 1; fallback_cases.append(('qwen-packed',str(optional)))
                else:
                    live_providers.add('qwen')
                    assert value == reference, ('qwen-packed', value, reference)
                    assert [c[0] for c in Handler.calls] == [c[0] for c in py_calls] == ['GET', 'POST', 'POST', 'POST', 'POST']
                    for (_, actual, body), (_, expected, refbody) in zip(Handler.calls, py_calls):
                        actual = {k.lower(): v for k,v in actual.items()}
                        expected = {k.lower(): v for k,v in expected.items()}
                        for key in ('cookie', 'origin', 'referer', 'x-requested-with', 'x-aac-fixture-url'):
                            assert actual.get(key) == expected.get(key), ('qwen-header', key)
                        if body:
                            first = urllib.parse.parse_qs(body.decode())
                            second = urllib.parse.parse_qs(refbody.decode())
                            first['params'] = json.loads(first['params'][0])
                            second['params'] = json.loads(second['params'][0])
                            assert first == second, 'qwen form body'

                    equal += 1
            # China-region Qwen reads SEC_TOKEN from the console HTML, then uses the CN gateway.
            auth.write_text(json.dumps({'alibaba-token-plan': {'type': 'api', 'key': json.dumps({'cookie': 'fixture-cn-cookie=made-up', 'baseUrl': 'https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1'})}}))
            replies = [(200, {}, b'<script>SEC_TOKEN: "fixture-cn-sec"</script>'), response(QWEN), response({'data': {}}), response({'data': {}}), response({'data': {}})]
            Handler.queue = replies.copy(); Handler.calls = []
            client = common.HttpClient(); client.opener = LocalOpener(client.opener, endpoint)
            with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(usage, 'utc_now', return_value=STAMP):
                reference = usage.collect('qwen', 'ubuntu', home=home, client=client)
            py_calls = Handler.calls.copy()
            Handler.queue = replies.copy(); Handler.calls = []
            request['args'] = {'provider': 'qwen', 'platform': 'ubuntu', 'home': str(home), 'endpoint': endpoint, 'now': NOW}
            process = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, check=True, env={'PATH': os.defpath})
            value = json.loads(process.stdout)['value']
            assert value == reference, ('qwen-cn', value, reference)
            assert [c[0] for c in Handler.calls] == [c[0] for c in py_calls] == ['GET', 'POST', 'POST', 'POST', 'POST']
            actual = {k.lower(): v for k, v in Handler.calls[0][1].items()}
            expected = {k.lower(): v for k, v in py_calls[0][1].items()}
            assert actual.get('accept') == expected.get('accept') == 'text/html'
            equal += 1; live_providers.add('qwen')
            # Target-bound Unix key records, including absent/private/invalid records.
            keydir = home / '.ccs/account-usage/keys'
            keydir.mkdir(parents=True, mode=0o700)
            for provider, payload in [('kimi-code', KIMI), ('zai', ZAI), ('opencode-go', GO)]:
                key = keydir / (provider + '-12345678.json')
                for state in ('valid', 'invalid', 'public', 'missing'):
                    if state != 'missing':
                        key.write_text(json.dumps({'version': 1, 'provider': provider, 'keyId': '12345678', 'secret': 'fixture-dummy-token' if state != 'invalid' else 'short'}))
                        key.chmod(0o644 if state == 'public' else 0o600)
                    else: key.unlink()
                    selector = {'kind': 'aac-key', 'keyId': '12345678'}
                    account = provider + ':acct:12345678'
                    replies = [response(payload)] + ([response(PACKS)] if provider == 'zai' else [])
                    Handler.queue = replies.copy()
                    client = common.HttpClient()
                    client.opener = LocalOpener(client.opener, endpoint)
                    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(usage, 'utc_now', return_value=STAMP):
                        reference = usage.collect(provider, 'ubuntu', home=home, client=client, credential=selector, account_id=account)
                    Handler.queue = replies.copy()
                    request['args'] = {'provider': provider, 'platform': 'ubuntu', 'home': str(home), 'endpoint': endpoint, 'now': NOW, 'credential': selector, 'account': account}
                    process = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, check=True, env={'PATH': os.defpath})
                    event = json.loads(process.stdout)
                    assert event['type'] == 'result' and event['id'] == request['id'] and event['protocol'] == 1
                    value = event['value']
                    if value == {'fallback': 'python'}: fallback += 1; fallback_cases.append(('qwen-packed',str(optional))); print("STDERR",process.stderr[:2500])
                    else:
                        assert value == reference, (provider, state, value, reference)
                        equal += 1
            # Empty fixture homes cannot accidentally inherit production environment secrets.
            auth.unlink()
            for provider in common.PROVIDERS:
                request['args'] = {'provider': provider, 'home': str(home), 'platform': 'ubuntu', 'now': NOW}
                process = subprocess.run([binary], input=json.dumps(request)+'\n', text=True, capture_output=True, check=True, env={'PATH': os.defpath, 'ZAI_API_KEY': 'must-not-be-read'})
                value = json.loads(process.stdout)['value']
                with mock.patch.dict(os.environ, {}, clear=True):
                    reference = usage.collect(provider, 'ubuntu', home=home)
                assert value == reference or value == {'fallback': 'python'}
                if value == reference: equal += 1
                else: fallback += 1; fallback_cases.append((provider,'empty'))
    finally:
        server.shutdown()
        server.server_close()
    print(f'quota parity: {equal} equal, {fallback} explicit fallback, 0 wrong ordinary replies')
    assert equal >= 30, 'must exercise ordinary Rust results'
    assert live_providers == set(common.PROVIDERS), 'must exercise native success for each provider'

if __name__ == '__main__':
    main()
