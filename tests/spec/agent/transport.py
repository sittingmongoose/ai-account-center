#!/usr/bin/env python3
"""L5 fixture transport: real private socket + Rust Computer pool + recorder, never real SSH.

As a runner: transport.py <aac-agent> <aac-http>. It runs each unchanged agent harness both
in place and through a generated executable proxy, compares their printed counts, then checks
the long-lived service's refusal, reuse, breaker, streaming and control boundaries.
All binaries stay at their build paths. No keys or trust files are created.
"""
import concurrent.futures
import contextlib
import hashlib
import http.client
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
    def __init__(self, agent, service, *, mode='on', scenario='agent', sha=None, size=None):
        self.temp = tempfile.TemporaryDirectory(prefix='a5-')
        self.root = Path(self.temp.name)
        self.agent = str(Path(agent).resolve())
        self.trace = self.root / 'trace.jsonl'
        self.scenario = self.root / 'scenario'
        self.scenario.write_text(scenario)
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
        config = {'agentTransport': mode, 'computers': {'fixture': {
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
        env.pop('AAC_AGENT_TRANSPORT', None)
        self.child = subprocess.Popen([str(Path(service).resolve())], stdin=subprocess.PIPE,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env)
        self.child.stdin.write((json.dumps({'ccsDir': str(self.root), 'socketPath': str(self.socket),
                                           'secret': SECRET, 'bootId': BOOT}) + '\n').encode())
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
        self.temp.cleanup()
    def __enter__(self): return self
    def __exit__(self, *unused): self.close()

def record(root, agent):
    root = Path(root)
    command = shlex.split(sys.argv[-1])[-1]
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
    os.execv(agent, [agent])

def proxy(agent, service):
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
    with Service(agent, service) as fixture:
        payload = fixture.payload(id=request['id'], deadline=request['deadlineMs'], cap=16*1024*1024 if request['verb']=='scan-usage' else 1024*1024)
        payload.pop('args')
        body = json.dumps(payload)[:-1] + ',"args":' + args + '}'
        status, response = fixture.ask('/internal/agent/' + request['verb'], body)
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

def main():
    agent, service = map(lambda p: str(Path(p).resolve()), sys.argv[1:3])
    gates(agent,service)
    if '--gates-only' in sys.argv: return
    with tempfile.TemporaryDirectory(prefix='aac-l5-proxy-') as tmp:
        wrapper = Path(tmp)/'agent-proxy'
        wrapper.write_text('#!/usr/bin/python3\nimport sys\nsys.path.insert(0,'+repr(str(Path(__file__).parent))+')\nfrom transport import proxy\nsys.exit(proxy('+repr(agent)+','+repr(service)+'))\n')
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
            print('PASS identical harness counts in-place / service:',suite[0],flush=True)
if __name__=='__main__': main()
