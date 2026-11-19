#!/usr/bin/env python3
"""L5 fixture transport: real private socket + Rust Computer pool + recorder, never real SSH.

As a runner: transport.py <aac-agent> <aac-http>. It runs each unchanged agent harness both
in place and through a generated executable proxy, compares their printed counts, then checks
the long-lived service's refusal, reuse, breaker, streaming and control boundaries.
The real service lifecycle regressions (home, scan, ssh, deadline) are the sibling
`tests/spec/agent/lifecycle.mjs <aac-agent> <aac-http>`, not part of `tests/spec/run.mjs`:
that runner has no agent binary. Run this file and lifecycle.mjs together on Nas1.
All binaries stay at their build paths. No keys or trust files are created.
"""
import concurrent.futures
import contextlib
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import re
import socket
import shlex
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
SECRET = 'c' * 64
BOOT = 'abcdef' * 4
PRINCIPAL = {'kind': 'session', 'id': 'front-door', 'role': 'service'}

class UnixHttp(http.client.HTTPConnection):
    def __init__(self, path, timeout=30):
        super().__init__('localhost', timeout=timeout)
        self.path = str(path)
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)

class Service:
    def __init__(self, agent, service, *, mode='on', scenario='agent', sha=None, size=None, collection=False, remote_env=None, cursor_fixture=False):
        self.temp = tempfile.TemporaryDirectory(prefix='a5-')
        self.root = Path(self.temp.name)
        self.agent = str(Path(agent).resolve())
        self.trace = self.root / 'trace.jsonl'
        self.scenario = self.root / 'scenario'
        self.scenario.write_text(scenario)
        (self.root / 'remote-env.json').write_text(json.dumps(remote_env or {}))
        identity = json.loads(subprocess.check_output([self.agent, '--version'], env={'PATH': os.defpath}))
        self.identity = identity
        artifact = self.agent
        if size is not None:
            artifact = str(self.root / 'oversize')
            with open(artifact, 'wb') as f: f.truncate(size)
        catalog = {'artifacts': [{'file': artifact, 'target': identity['target'], 'version': identity['version'],
                                  'sha256': sha or hashlib.sha256(Path(artifact).read_bytes()).hexdigest()}]}
        (self.root / 'catalog.json').write_text(json.dumps(catalog))
        run = self.root / 'run'
        run.mkdir(mode=0o700)
        folder = run / ('aac-http-' + BOOT)
        folder.mkdir(mode=0o700)
        self.socket = folder / 'http.sock'
        config = {'version': 1, 'agentTransport': mode, 'computers': {'fixture': {
            'label': 'Fixture', 'os': 'linux', 'sshAlias': 'fixture',
            'transport': {'kind': 'ssh', 'host': 'fixture.invalid', 'key': str(self.root / 'no-key'), 'user': None, 'port': None}}},
            'agentTransportSsh': {'knownHostsFile': str(self.root / 'no-known-hosts')}}
        (self.root / 'config.yaml').write_text(json.dumps(config))
        recorder = self.root / 'ssh'
        recorder.write_text('#!/usr/bin/python3\nimport sys\nsys.path.insert(0,' + repr(str(Path(__file__).parent)) + ')\nfrom transport import record\nrecord(' + repr(str(self.root)) + ',' + repr(self.agent) + ')\n')
        recorder.chmod(0o700)
        scp = self.root / 'scp'
        scp.write_text('#!/bin/sh\nexit 99\n')
        scp.chmod(0o700)
        env = {**os.environ, 'PATH': str(self.root) + ':' + os.environ.get('PATH', os.defpath),
               'AAC_AGENT_PACKAGE_ROOT': str(ROOT), 'AAC_AGENT_CATALOG': str(self.root / 'catalog.json')}
        if cursor_fixture:
            package = self.root / 'cursor-package'
            install_cursor_fixture(package, self.root)
            env['AAC_AGENT_PACKAGE_ROOT'] = str(package)
        env.pop('AAC_AGENT_TRANSPORT', None)
        self.child = subprocess.Popen([str(Path(service).resolve())], stdin=subprocess.PIPE,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env)
        boot = {'ccsDir': str(self.root), 'socketPath': str(self.socket),
                'secret': SECRET, 'bootId': BOOT}
        if collection:
            rows = json.loads((ROOT / 'config/private-files.json').read_text())['rows']
            boot['files'] = {row['id']: ('rust-service' if row['movesWith'] == 'collection' else row['owner'])
                             for row in rows}
        self.child.stdin.write((json.dumps(boot) + '\n').encode())
        self.child.stdin.flush()
        until = time.monotonic() + 10
        while True:
            try:
                status, _ = self.ask('/api/health', principal={'kind': 'session', 'id': 'public-health', 'role': 'public'}, method='GET')
                if status == 200: break
            except (OSError, http.client.HTTPException): pass
            if self.child.poll() is not None: raise AssertionError(self.child.stderr.read().decode())
            if time.monotonic() > until: raise AssertionError('service did not bind')
            time.sleep(.01)
    def ask(self, path, data=None, *, principal=PRINCIPAL, method='POST', secret=SECRET):
        conn = UnixHttp(self.socket)
        try:
            body = data if isinstance(data, str) else json.dumps(data) if data is not None else None
            conn.request(method, path, body=body, headers={'x-aac-boot-secret': secret, 'x-aac-principal': json.dumps(principal)})
            response = conn.getresponse()
            return response.status, response.read().decode()
        finally: conn.close()
    def payload(self, args=None, id='parity', deadline=15000, cap=1024*1024):
        return {'computer': {'platform': 'linux', 'sshHost': 'fixture'}, 'args': args or {},
                'deadlineMs': deadline, 'timeoutMs': deadline + 5000, 'maxBytes': cap, 'callId': id}
    def call(self, verb='health', **kwargs):
        status, raw = self.ask('/internal/agent/' + verb, self.payload(**kwargs))
        assert status == 200, (status, raw)
        return [json.loads(line) for line in raw.splitlines()]
    def traces(self):
        return [json.loads(line) for line in self.trace.read_text().splitlines()] if self.trace.exists() else []
    def close(self):
        self.child.stdin.close()
        try: self.child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.child.kill(); self.child.wait()
        self.child.stderr.close()
        control_hash = hashlib.sha256(str(self.root / 'run/agent-ssh').encode()).hexdigest()[:8]
        control_dir = Path('/tmp') / ('aac-' + str(os.getuid()) + '-' + control_hash)
        if control_dir.exists(): control_dir.rmdir()
        self.temp.cleanup()
    def __enter__(self): return self
    def __exit__(self, *unused): self.close()

def install_cursor_fixture(package, root):
    target = 'x86_64-unknown-linux-gnu'
    folder = package / 'dist/native/aac-cursor-account'
    binary = folder / target / 'aac-cursor-account'
    binary.parent.mkdir(parents=True)
    source = """#!/usr/bin/python3
import json, sys, time
from pathlib import Path
assert sys.argv == [sys.argv[0], 'read', '--ccs-dir', ROOT]
root = Path(ROOT)
bounds = json.load(sys.stdin)
with (root / 'cursor-trace.jsonl').open('a') as trace:
    trace.write(json.dumps(bounds) + '\\n')
if (root / 'cursor-failure').exists():
    print(json.dumps({'state':'unavailable','cause':'fixture_failure'}))
    sys.exit(1)
if (root / 'cursor-slow').exists():
    until = time.monotonic() + 10
    while not (root / 'cursor-barrier').exists():
        assert time.monotonic() < until
        time.sleep(.01)
rows = [{'h':'2026-01-01 00:00','m':'fixture-model','i':3,'o':0,'cr':0,'cw':0,
         'n':1,'uncosted':0,'cents':2}, {'h':'bad'}]
print(json.dumps({'state':'read','readAt':bounds['untilMs'],'account':'fixture',
                  'key':{'last4':'long-ab12','fingerprint':'fixture'},'rows':rows,
                  'events':1,'unmeasured':2}))
"""
    binary.write_text(source.replace("ROOT", repr(str(root))))
    binary.chmod(0o700)
    body = binary.read_bytes()
    manifest = {'name':'aac-cursor-account','targets':{target:{
        'file':f'{target}/aac-cursor-account','size':len(body),
        'sha256':hashlib.sha256(body).hexdigest(),
    }}}
    (folder / 'manifest.json').write_text(json.dumps(manifest))


def cursor_gates(agent, service):
    route = '/internal/collection-cursor'
    since, until = 1_767_225_600_000, 1_767_229_200_000
    payload = {'sinceMs':since,'untilMs':until,'force':False}
    with Service(agent, service, cursor_fixture=True) as fixture:
        assert fixture.ask(route, payload, method='PUT')[0] == 503
        assert not (fixture.root / 'cursor-trace.jsonl').exists()
    with Service(agent, service, collection=True, cursor_fixture=True) as fixture:
        trace = fixture.root / 'cursor-trace.jsonl'
        public = {'kind':'session','id':'fixture-admin','role':'admin'}
        assert fixture.ask(route, payload, method='PUT', principal=public)[0] == 401
        assert fixture.ask(route, dict(payload, sinceMs=until+1), method='PUT')[0] == 503
        assert not trace.exists()
        (fixture.root / 'cursor-slow').touch()
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            a = pool.submit(fixture.ask, route, payload, method='PUT')
            until_spawn = time.monotonic() + 5
            while not trace.exists():
                assert time.monotonic() < until_spawn
                time.sleep(.01)
            b = pool.submit(fixture.ask, route, dict(payload, force=True), method='PUT')
            time.sleep(.1)
            (fixture.root / 'cursor-barrier').touch()
            status, raw = a.result()
            assert status == 200 and b.result() == (status, raw)
        first = json.loads(raw)
        assert first['state'] == 'read' and first['readAt'] == '2026-01-01T01:00:00.000Z'
        assert first['key']['last4'] == 'ab12' and len(first['rows']) == 1
        assert first['events'] == 1 and first['unmeasured'] == 2
        assert fixture.ask(route, payload, method='PUT') == (200, raw)
        assert trace.read_text().splitlines() == [json.dumps({'sinceMs':since,'untilMs':until})]
        (fixture.root / 'cursor-failure').touch()
        status, raw = fixture.ask(route, dict(payload, force=True), method='PUT')
        failed = json.loads(raw)
        assert status == 200 and failed['state'] == 'unavailable'
        assert failed['cause'] == 'fixture_failure'
        for name in ('rows', 'readAt', 'account', 'events', 'unmeasured', 'key'):
            assert failed[name] == first[name]
        saved = fixture.root / 'cache/cursor-account-v1.json'
        assert json.loads(saved.read_text()) == failed
        assert saved.stat().st_mode & 0o777 == 0o600
        binary = (fixture.root / 'cursor-package/dist/native/aac-cursor-account'
                  / 'x86_64-unknown-linux-gnu/aac-cursor-account')
        binary.write_bytes(binary.read_bytes() + b'\n# changed after packaging\n')
        status, raw = fixture.ask(route, dict(payload, force=True), method='PUT')
        missing = json.loads(raw)
        assert status == 200 and missing['state'] == 'no_reader' and missing['rows'] == []
        assert len(trace.read_text().splitlines()) == 2
    print('PASS H2 Cursor: digest-bound local reader, shared pending read, TTL, atomic save and last-good rows')


def record(root, agent):
    if '-G' in sys.argv[1:]:
        sys.exit(0)  # No-connect capability probes are not agent launcher calls.
    root = Path(root)
    argv = shlex.split(sys.argv[-1])
    if len(argv) == 5 and argv[:2] == ['/bin/sh', '-c'] and argv[3] == 'aac-failover':
        with (root / 'trace.jsonl').open('a') as f:
            f.write(json.dumps({'stage': 'helper', 'pid': os.getpid()}) + '\n')
        home = root / 'fixture-home'
        home.mkdir(exist_ok=True)
        os.environ['HOME'] = str(home)
        os.execv(argv[0], argv)
    command = argv[-1]
    stage = re.search(r"case '(probe|prepare|commit|run)'", command)
    assert stage, command
    stage = stage[1]
    with (root / 'trace.jsonl').open('a') as f:
        f.write(json.dumps({'stage': stage, 'pid': os.getpid(), 'argv': sys.argv[1:]}) + '\n')
    scenario = (root / 'scenario').read_text()
    if scenario == 'unreachable': sys.exit(255)
    if scenario == 'missing': sys.exit(87)
    assert stage in ('probe', 'run'), 'service attempted shipping'
    if stage == 'probe':
        if scenario == 'old':
            identity = json.loads(subprocess.check_output([agent, '--version']))
            identity['version'] = '0.0.0'
            print(json.dumps(identity)); sys.exit(0)
        os.execv(agent, [agent, '--version'])
    if scenario == 'signin-rows':
        request = json.loads(sys.stdin.buffer.readline())
        assert request['verb'] == 'signin-expiry'
        value = json.loads((root / 'signin-fixture.json').read_text())
        print(json.dumps({'type':'result','protocol':1,'id':request['id'],'value':value}))
        sys.exit(0)
    if scenario == 'collect-slow':
        request = json.loads(sys.stdin.buffer.readline())
        while not (root / 'barrier').exists():
            time.sleep(.01)
        print(json.dumps({'type': 'result', 'protocol': 1, 'id': request['id'],
                          'value': {'schemaVersion': 1, 'profiles': []}}), flush=True)
        sys.exit(0)
    if scenario == 'burst':
        request = json.loads(sys.stdin.buffer.readline())
        for n in range(16):
            print(json.dumps({'type':'progress','protocol':1,'id':request['id'],'data':{'n':n}}))
        print(json.dumps({'type':'result','protocol':1,'id':request['id'],'value':{'fixture':True}}),flush=True)
        sys.exit(0)
    if scenario in ('flood', 'pool'):
        request = json.loads(sys.stdin.buffer.readline())
        if scenario == 'flood':
            print('x' * 100000, flush=True)
            time.sleep(3)
        else:
            (root / ('pid.' + str(os.getpid()))).touch()
            if len(list(root.glob('pid.*'))) >= 4: (root / 'barrier').touch()
            until = time.monotonic() + 5
            while not (root / 'barrier').exists():
                assert time.monotonic() < until, 'pool never admitted four channels'
                time.sleep(.01)
            print(json.dumps({'type':'result','protocol':1,'id':request['id'],'value':{'fixture':True}}),flush=True)
        sys.exit(0)
    if scenario in ('stream', 'ignore-cancel'):

        request = json.loads(sys.stdin.buffer.readline())
        emit = lambda kind, **fields: print(json.dumps({'type': kind, 'protocol': 1, 'id': request['id'], **fields}), flush=True)
        emit('progress', data={'pid': os.getpid(), 'ready': True})
        for line in sys.stdin:
            control = json.loads(line)
            if 'input' in control: emit('progress', data={'input': control['input']})
            if control.get('cancel') and scenario != 'ignore-cancel':
                emit('failure', failure={'cause': 'cancelled', 'detail': 'fixture cancelled', 'retry': {'kind':'never'}})
                # The HTTP acknowledgement must wait through this gap until this process exits.
                time.sleep(.1)
                sys.exit(1)
        sys.exit(1)
    os.environ.update(json.loads((root / 'remote-env.json').read_text()))
    os.execv(agent, [agent])

def fixture_remote_env():
    # A real SSH account has its own environment. The fake remote reproduces only
    # corpus fixtures, never an ambient provider token or home.
    result = {}
    for key in ('ZAI_API_KEY', 'KIMI_API_KEY', 'OPENCODE_GO_API_KEY'):
        value = os.environ.get(key)
        if value in ('fixture-environment-key', 'non-ascii-\u00e9', 'surrogate-\udcff'):
            result[key] = value
    for key in ('HOME', 'XDG_CONFIG_HOME'):
        value = os.environ.get(key, '')
        if re.fullmatch(r'/+tmp/aac-(?:m2p4|signin-review)-[a-z0-9_]+(?:/[^/]+)*', value):
            result[key] = value
    return result

def proxy(agent, service, collection=False):
    wire = sys.stdin.read()
    request = json.loads(wire)
    # Keep raw args: serde must not erase duplicate keys before the verb's original proof gate.
    match = re.search(r'"args"\s*:\s*', wire)
    decoder = json.JSONDecoder()
    _, end = decoder.raw_decode(wire[match.end():])
    args = wire[match.end():match.end()+end]
    # Original trailing padding remains padding in the transported argument object, preserving
    # the original whole-request cap witness without changing any semantic argument.
    trailing = len(wire.rstrip('\n')) - len(wire.rstrip())
    if trailing and args.endswith('}'): args = args[:-1] + ' ' * trailing + '}'
    with Service(agent, service, collection=collection, remote_env=fixture_remote_env()) as fixture:
        family = 'collect' if collection and request['verb'] in (
            'quota-read', 'signin-expiry', 'failover-inventory') else 'agent'
        payload = fixture.payload(id=request['id'], deadline=request['deadlineMs'], cap=16*1024*1024 if request['verb']=='scan-usage' else 1024*1024)
        payload.pop('args')
        body = json.dumps(payload)[:-1] + ',"args":' + args + '}'
        status, response = fixture.ask('/internal/' + family + '/' + request['verb'], body)
        assert status == 200, (status, response)
        events = [json.loads(line) for line in response.splitlines()]
        assert len(events) == 1, events
        event = events[0]
        if event['type'] == 'fallback':
            event = {'type': 'result', 'protocol': 1, 'id': request['id'], 'value': {'fallback': 'python'}}
        print(json.dumps(event))
        return int(event['type'] == 'failure')

def gates(agent, service):
    with Service(agent, service) as s:
        for id in ['first', 'second']:
            assert s.call(id=id)[-1]['type'] == 'result'
        assert [t['stage'] for t in s.traces()] == ['probe', 'run', 'run'], s.traces()
        status, _ = s.ask('/internal/agent/health', principal={'kind':'session','id':'fixture-admin','role':'admin'})
        assert status == 401
        status, _ = s.ask('/internal/agent/parity/control', secret='d'*64)
        assert status == 401
        before = len(s.traces())
        assert s.call(args={'x': 'y'})[-1]['type'] == 'failure'
        assert s.call(id='after-failure')[-1]['type'] == 'result'
        assert len(s.traces()) == before + 2
    for scenario in ['missing', 'old', 'unreachable']:
        with Service(agent, service, scenario=scenario) as s:
            for n in range(5): assert s.call(id=str(n))[-1]['type'] == 'fallback'
            assert len(s.traces()) == (3 if scenario == 'unreachable' else 5)
            assert all(t['stage'] == 'probe' for t in s.traces())
    for kwargs in [{'mode':'off'}, {'sha':'a'*64}, {'size':64*1024*1024+1}]:
        with Service(agent, service, **kwargs) as s:
            assert s.call()[-1]['type'] == 'fallback'
            assert not s.traces()
    with Service(agent, service) as s:
        assert s.call(id='warm')[-1]['type']=='result'
        s.scenario.write_text('pool')
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            results=list(pool.map(lambda n:s.call(id='pool'+str(n),deadline=10000),range(8)))
        assert all(rows[-1]['type']=='result' for rows in results),results
        assert len(list(s.root.glob('pid.*')))==8
    with Service(agent, service, scenario='flood') as s:
        assert s.call(cap=4096)[-1]['type']=='fallback'
    with Service(agent, service, scenario='burst') as s:
        for n in range(10):
            events=s.call(id='burst'+str(n))
            assert [e.get('data',{}).get('n') for e in events[:-1]]==list(range(16)), events
            assert events[-1]['type']=='result'
    # Max L1: six HTTP deadlines, three transport timeouts, then an open circuit.
    with Service(agent, service) as s:
        assert s.call(id='timeout-warm')[-1]['type'] == 'result'
        s.scenario.write_text('ignore-cancel')
        before = len(s.traces())
        for n in range(6):
            payload = s.payload(id='timeout-' + str(n), deadline=1000)
            payload['timeoutMs'] = 1000
            status, raw = s.ask('/internal/agent/health', payload)
            assert status == 200 and json.loads(raw.splitlines()[-1])['type'] == 'fallback', raw
            expected = before + min(n + 1, 3)
            assert len(s.traces()) == expected, ('timeout breaker', n, s.traces())
        for trace in s.traces():
            assert not Path('/proc', str(trace['pid'])).exists(), 'owned child survived deadline'
    for scenario in ['stream', 'ignore-cancel']:

        with Service(agent, service, scenario=scenario) as s:
            conn = UnixHttp(s.socket)
            conn.request('POST', '/internal/agent/health', json.dumps(s.payload()), headers={'x-aac-boot-secret':SECRET,'x-aac-principal':json.dumps(PRINCIPAL)})
            response = conn.getresponse()
            first = json.loads(response.readline())
            assert first['type'] == 'progress'
            pid = first['data']['pid']
            assert s.ask('/internal/agent/parity/control', {'input':'fixture-input'}) == (200,'{"accepted":true}')
            assert json.loads(response.readline())['data'] == {'input':'fixture-input'}
            began = time.monotonic()
            status, raw = s.ask('/internal/agent/parity/control', {'cancel':True})
            assert status == 200 and json.loads(raw) == {'cancelled':scenario=='stream'}, raw
            assert time.monotonic()-began >= (.09 if scenario=='stream' else 1.9)
            until=time.monotonic()+2
            while Path('/proc',str(pid)).exists() and time.monotonic()<until: time.sleep(.01)
            assert not Path('/proc',str(pid)).exists(), 'far fixture process survived'
            conn.close()
    print('PASS L5 service: reuse, principal, breaker, hash, artifact cap, off, existing-only, progress/input, exit-confirmed cancel and no-ack termination')

def collection_gates(agent, service):
    with Service(agent, service) as s:
        status, _ = s.ask('/internal/collect/signin-expiry', s.payload())
        assert status == 503 and not s.traces()

    with Service(agent, service, collection=True, scenario='collect-slow') as s:
        route = '/internal/collect/signin-expiry'
        public = {'kind': 'session', 'id': 'fixture-admin', 'role': 'admin'}
        assert s.ask(route, s.payload(), principal=public)[0] == 401
        assert json.loads(s.ask('/internal/collect/failover-inventory',
                                s.payload({'action': 'collect'}))[1])['reason'] == 'shape'
        assert not s.traces()
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            a = pool.submit(s.ask, route, s.payload(id='collection-a'))
            b = pool.submit(s.ask, route, s.payload(id='collection-b'))
            until = time.monotonic() + 5
            while not any(t['stage'] == 'run' for t in s.traces()):
                assert time.monotonic() < until, s.traces()
                time.sleep(.01)
            time.sleep(.1)
            (s.root / 'barrier').touch()
            for expected, future in [('collection-a', a), ('collection-b', b)]:
                code, raw = future.result()
                assert code == 200
                assert json.loads(raw) == {'type':'result','protocol':1,'id':expected,
                                           'value':{'schemaVersion':1,'profiles':[]}}
        assert [t['stage'] for t in s.traces()] == ['probe', 'run']

        helper_route = '/internal/collect/failover-collect'
        # This candidate deliberately fails validation before credential lookup or HTTP.
        helper_args = {'version': 1, 'provider': 'claude', 'action': 'collect', 'candidate': {}}
        assert s.ask(helper_route, s.payload(helper_args), principal=public)[0] == 401
        status, raw = s.ask(helper_route, s.payload(helper_args))
        event = json.loads(raw)
        assert status == 200 and event['type'] == 'result', event
        assert event['value']['failureCode'] == 'invalid_response', event
        assert [t['stage'] for t in s.traces()] == ['probe', 'run', 'helper']

        source_route = '/internal/collection-sources'
        assert s.ask(source_route, method='GET') == (200, '{"state":"absent"}')
        registry = s.root / 'account-usage-accounts.json'
        binding = {'id':'kimi-code:usage', 'provider':'kimi-code', 'platform':'ubuntu',
                   'credential':{'kind':'discover'}}
        registry.write_text(json.dumps({'version':2,'accounts':[binding]}))
        registry.chmod(0o600)
        status, raw = s.ask(source_route, method='GET')
        assert status == 200 and json.loads(raw) == {'state':'ok','registry':{
            'version':2,'accounts':[dict(binding, sshHost=None, label=None, createdAt=None, createdBy=None)]}}
        registry.write_text(json.dumps({'version':2,'accounts':[binding,binding]}))
        assert s.ask(source_route, method='GET')[0] == 503

        key = 'a' * 64
        route = '/internal/collection-cache/account-usage-observations/' + key
        sample = {'version':1,'key':key,'identity':None,'primary':None,'saved':None,'nextAttempt':1234}
        assert s.ask(route, sample, method='PUT', principal=public)[0] == 401
        assert s.ask(route, {**sample, 'nextAttempt': 'bad'}, method='PUT') == (200, '{"saved":false}')
        assert s.ask(route, sample, method='PUT') == (200, '{"saved":true}')
        disk = s.root / 'account-usage-failover' / (key + '.json')
        assert disk.read_text() == json.dumps(sample)
        assert disk.stat().st_mode & 0o777 == 0o600
        link = disk.with_name('b' * 64 + '.json')
        link.hardlink_to(disk)
        assert s.ask(route, {**sample, 'nextAttempt': 5678}, method='PUT') == (200, '{"saved":false}')
        assert json.loads(disk.read_text()) == sample
        # Invalid files and peer/cooldown state are outside observation retention.
        for n in range(257):
            old = format(n, '064x')
            entry = disk.with_name(old + '.json')
            entry.write_text(json.dumps({**sample, 'key': old}))
            entry.chmod(0o600)
        name = 'c' * 64
        cooldown = disk.with_name('cooldown-' + name + '.json')
        cooldown.write_text('{}')
        write_route = '/internal/collection-cache/account-usage-observations/' + name
        assert s.ask(write_route, {**sample, 'key': name}, method='PUT') == (200, '{"saved":true}')
        assert disk.exists() and link.exists() and cooldown.exists()
        valid = [p for p in disk.parent.iterdir()
                 if re.fullmatch('[a-f0-9]{64}.json', p.name) and p.stat().st_nlink == 1]
        assert len(valid) == 256 and disk.with_name(name + '.json') in valid
        profile = 'fixture-profile'
        remote_route = '/internal/collection-cache/claude-live-snapshots/' + profile
        remote = {'schemaVersion':1, 'manifestHash':'b'*64,
                  'sample':{'profileId':profile, 'platform':'windows', 'windows':[]}}
        assert s.ask(remote_route, remote, method='PUT') == (200, '{"saved":true}')
        assert (s.root / 'claude-desktop-live-cache' / (profile + '.json')).read_text() == json.dumps(remote)
        cache_route = '/internal/collection-provider-cache'
        request = {'profileId': profile, 'manifestHash': remote['manifestHash']}
        status, raw = s.ask(cache_route, request, method='PUT')
        assert status == 200 and json.loads(raw) == {'value': remote['sample']}
        assert s.ask(cache_route, dict(request, manifestHash='a'*64), method='PUT') == (200, '{"value":null}')
        binding = 'd' * 64
        wallet = s.root / 'opencode-console-wallet-account.json'
        account = 'plan-opencode-go-console-mac-123456abcdef'
        wallet.write_text(json.dumps({'version':1,'binding':binding,'accountId':account}))
        wallet.chmod(0o600)
        status, raw = s.ask(cache_route, {'bindingHash':binding}, method='PUT')
        assert status == 200 and json.loads(raw) == {'value':account}

    print('PASS H2 collection: switch, principal, read-only verbs, coalescing, row bytes and private cache writes')


def collection_projection(agent, service):
    import threading
    from unittest import mock
    from quota import (Handler, LocalOpener, KIMI, NOW, STAMP, common, response, usage)
    import failover_usage

    with Service(agent, service, collection=True) as s:
        home = s.root / 'fixture-home'
        auth = home / '.local/share/opencode/auth.json'
        auth.parent.mkdir(parents=True)
        auth.write_text(json.dumps({'kimi-for-coding': {'type': 'api', 'key': 'fixture-dummy-token'}}))
        with http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
            threading.Thread(target=server.serve_forever, daemon=True).start()
            endpoint = 'http://127.0.0.1:' + str(server.server_port)
            Handler.queue = [response(KIMI), response(KIMI)]
            Handler.calls = []
            client = common.HttpClient()
            client.opener = LocalOpener(client.opener, endpoint)
            try:
                with mock.patch.dict(os.environ, {}, clear=True), \
                        mock.patch.object(common, 'utc_now', return_value=STAMP), \
                        mock.patch.object(usage, 'utc_now', return_value=STAMP):
                    reference = usage.collect('kimi-code', 'ubuntu', home=home, client=client)
                    inventory = failover_usage.run({
                        'version': 1, 'action': 'inventory', 'provider': 'kimi-code',
                        'homeDir': str(home), 'ccsDir': str(s.root),
                    })
                payload = {'root': str(s.root), 'socketPath': str(s.socket), 'secret': SECRET, 'bootId': BOOT,
                           'args': {'provider': 'kimi-code', 'platform': 'ubuntu', 'home': str(home),
                                    'endpoint': endpoint, 'now': NOW}, 'reference': reference, 'inventory': inventory}
                command = [os.environ.get('BUN', 'bun'), str(Path(__file__).with_name('collection-dashboard.ts'))]
                env = {**os.environ, 'HOME': str(home), 'XDG_CONFIG_HOME': str(home / '.config')}
                subprocess.run(command, input=json.dumps(payload), env=env, text=True, check=True)
                assert len(Handler.calls) == 2 and not Handler.queue
                assert [t['stage'] for t in s.traces()] == ['probe', 'run', 'run', 'run']
            finally:
                server.shutdown()


def main():
    agent, service = map(lambda p: str(Path(p).resolve()), sys.argv[1:3])
    collection = '--collection' in sys.argv
    if collection:
        collection_gates(agent, service)
        collection_projection(agent, service)
        cursor_gates(agent, service)
    else:
        gates(agent, service)
    if '--gates-only' in sys.argv: return
    with tempfile.TemporaryDirectory(prefix='aac-l5-proxy-') as tmp:
        wrapper = Path(tmp)/'agent-proxy'
        wrapper.write_text('#!/usr/bin/python3\nimport sys\nsys.path.insert(0,'+repr(str(Path(__file__).parent))+')\nfrom transport import proxy\nsys.exit(proxy('+repr(agent)+','+repr(service)+','+repr(collection)+'))\n')
        wrapper.chmod(0o700)
        for suite in [('run.py','--part3','--part4'), ('signin_review.py',), ('quota.py',), ('quota-review.py',), ('deadline_review.py',)]:
            outputs=[]
            for binary in [agent,str(wrapper)]:
                run=subprocess.run([sys.executable,str(Path(__file__).parent/suite[0]),binary,*suite[1:]], text=True, stdout=subprocess.PIPE,stderr=subprocess.STDOUT, cwd=ROOT)
                print(run.stdout, end='', flush=True)
                assert run.returncode==0, (suite,binary,run.returncode)
                outputs.append(run.stdout)
            # Deadline timings and temporary paths vary; all harness counts and result labels stay.
            mask=lambda t: re.sub(r'in \d+\.\d+s', 'in <time>s', re.sub(r'after \d+\.\d+ s','after <time>',t))
            assert mask(outputs[0])==mask(outputs[1]), ('harness counts changed',suite,outputs)
            print('PASS identical harness counts in-place / ' + ('collection' if collection else 'service') + ':', suite[0], flush=True)
if __name__=='__main__': main()
