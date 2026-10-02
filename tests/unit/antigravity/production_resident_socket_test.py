"""Disposable actual Unix socket/PTY/standin fixtures; never starts native agy."""
import base64
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
PUBLIC_RUNTIME = Path(__file__).resolve().parents[3] / 'scripts/antigravity/runtime'
sys.path.insert(0, str(PUBLIC_RUNTIME))
from resident_broker import encode_frame, extract_frame, MAX_FRAME


class Peer:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(3); self.socket.connect(str(path)); self.buffer = bytearray()
        self.outputs = []; self.exit = None

    def receive(self):
        while True:
            message = extract_frame(self.buffer)
            if message is not None:return message
            raw = self.socket.recv(65536)
            if not raw:raise EOFError()
            self.buffer.extend(raw)

    def request(self, method, **parameters):
        if method == 'stop' and hasattr(self, 'fixture'):
            expected = json.loads(self.fixture.auth.read_text())
            self.request('approve-idle-plan', plan=parameters['plan'], expected=expected, credentialFingerprint=hashlib.sha256(self.fixture.auth.read_bytes()).hexdigest())
        request_id = str(uuid.uuid4())
        self.socket.sendall(encode_frame({'requestId': request_id, 'method': method, **parameters}))
        while True:
            message = self.receive()
            if 'terminalOutput' in message:self.outputs.append(base64.b64decode(message['terminalOutput'])); continue
            if 'nativeExit' in message:self.exit = message['nativeExit']; continue
            if 'switchState' in message:continue
            if message.get('requestId') == request_id:
                if 'error' in message:raise RuntimeError(message['error'])
                result = message['result']
                if method == 'prove' and getattr(self, 'auto_complete', True): self.request('complete-transaction', expected=parameters['expected'])
                return result

    def close(self):self.socket.close()


class BrokerFixtures(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='aic-resident-fixture-')
        self.root = Path(self.temp.name); self.socket = self.root / 'ipc/broker.sock'
        self.cid = str(uuid.uuid4()); self.other = str(uuid.uuid4())
        self.binary = self.root / 'aic-fixture-cli'
        shutil.copyfile(sys.executable, self.binary); self.binary.chmod(0o700)
        self.auth = self.root / 'fictional-account.json'
        self.auth.write_text(json.dumps({'email': 'first@example.com', 'subject': 'fictional-first'}))
        self.meta = self.root / 'fixture-meta.json'
        self.meta.write_text(json.dumps({'idle': True, 'cid': self.cid}))
        self.brain = self.root / 'brain'; self.brain.mkdir()
        (self.brain / 'work').write_bytes(b'fixture-history-do-not-touch')
        self.history_hash = hashlib.sha256((self.brain / 'work').read_bytes()).hexdigest()
        self.input_log = self.root / 'inputs'
        self.standin = self.root / 'standin.py'
        self.standin.write_text('''import json,os,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT'])
meta=json.loads((root/'fixture-meta.json').read_text())
cid=sys.argv[sys.argv.index('--conversation')+1] if '--conversation' in sys.argv else meta['cid']
opened=(root/(cid+'.db')).open('a+')
if os.environ.get('FIXTURE_AMBIGUOUS')=='1':other=(root/(os.environ['FIXTURE_OTHER']+'.db')).open('a+')
account=json.loads((root/'fictional-account.json').read_text())
print(json.dumps({'event':'ready','email':account['email'],'subject':account['subject'],
'conversationId':cid,'cwd':os.getcwd(),'pty':os.ttyname(0),'envSentinel':os.environ['FIXTURE_PRIVATE']}),flush=True)
for line in sys.stdin:
 with (root/'inputs').open('a') as f:f.write(line)
 print(json.dumps({'event':'ready'}),flush=True)
''')
        self.server_script = self.root / 'server.py'
        staged = str(PUBLIC_RUNTIME)
        self.server_script.write_text(f'''import json,os,signal,sys,hashlib
from pathlib import Path
sys.path.insert(0,{staged!r})
from resident_broker import ResidentBroker,actual_open_conversation
from runtime_continuity import ManagedPtyRuntime,ContinuityError,digest
root=Path(sys.argv[1]); observers={{}}
class FixtureObserver:
 def __init__(self,session):self.session=session;self.buffer=b'';self.value={{}};self.idle=False
 def feed(self,raw):
  self.buffer+=raw
  if len(self.buffer)>65536:raise ContinuityError('fixture-budget')
  while b'\\n' in self.buffer:
   line,self.buffer=self.buffer.split(b'\\n',1)
   try:value=json.loads(line.strip())
   except ValueError:continue
   if value.get('event')=='ready':self.value.update(value);self.idle=True
 def ready(self):return self.idle
 def input_pending(self):self.idle=False
 def exited(self):self.idle=False
 def identity(self):return {{**self.value,'source':'native-runtime','runtimeStarted':True,'sessionRestored':True}}
def metadata(cid,cwd):
 value=json.loads((root/'fixture-meta.json').read_text())
 if cid!=value['cid'] or cwd!=str(root):raise ContinuityError('workspace-mismatch')
 return {{'idle':value['idle'],'fingerprint':digest([cid,cwd,value['idle']]),'cwd':cwd}}
def factory(session):
 observed=FixtureObserver(session);observers[id(session)]=observed;return observed
def identity(session):return observers[id(session)].identity()
def idle(session):return observers[id(session)].ready()
def binding(expected,proofs):
 account=json.loads((root/'fictional-account.json').read_text())
 if expected['email']!=account['email'] or expected['subject']!=account['subject']:raise ContinuityError('runtime-proof-unavailable')
 return {{'identity':{{**expected,'source':'native-runtime'}},'credentialFingerprint':hashlib.sha256((root/'fictional-account.json').read_bytes()).hexdigest(),'runtimeStarted':True,'sessionRestored':True}}
runtime=ManagedPtyRuntime(str(root/'aic-fixture-cli'),metadata,identity,idle)
broker=ResidentBroker(runtime,str(root/'ipc/broker.sock'),factory,lambda session,rt:actual_open_conversation(session,rt,metadata),lambda:os.environ.get('FIXTURE_CAPABILITY')=='1',binding,lambda args:not args or (args and args[0]==str(root/'standin.py')),transaction_validator=lambda phase,state,expected,fingerprint: (fingerprint is None or hashlib.sha256((root/'fictional-account.json').read_bytes()).hexdigest()==fingerprint))
signal.signal(signal.SIGTERM,lambda *_:setattr(broker,'running',False))
broker.serve()
''')
        self.peers = []; self.foreground = None
        self.server = subprocess.Popen([sys.executable, str(self.server_script), str(self.root)],
                                       env=dict(os.environ, FIXTURE_CAPABILITY='1'),
                                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        deadline = time.monotonic() + 3
        while not self.socket.exists() and time.monotonic() < deadline:
            if self.server.poll() is not None:self.fail('fixture broker failed before listening')
            time.sleep(0.01)
        self.assertTrue(self.socket.exists())

    def tearDown(self):
        for peer in self.peers:peer.close()
        self.server.terminate()
        try:self.server.wait(timeout=5)
        except subprocess.TimeoutExpired:self.fail('fixture broker cleanup incomplete')
        errors = self.server.stderr.read(); self.server.stderr.close()
        self.assertEqual(self.server.returncode, 0, 'fixture broker error (private stderr suppressed)')
        self.assertEqual(hashlib.sha256((self.brain / 'work').read_bytes()).hexdigest(), self.history_hash)
        self.temp.cleanup()

    def peer(self):
        peer = Peer(self.socket); peer.fixture = self; self.peers.append(peer); return peer

    def wait_proof(self, control, expected):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:return control.request('prove', expected=expected)
            except RuntimeError:time.sleep(0.01)
        self.fail('fixture header did not produce a fresh bound proof')

    def launch(self, explicit=True, ambiguous=False):
        foreground = self.peer(); self.foreground = foreground
        args = [str(self.standin)] + (['--conversation', self.cid] if explicit else [])
        env = dict(os.environ, FIXTURE_ROOT=str(self.root), FIXTURE_PRIVATE='private-env-not-for-http',
                   FIXTURE_AMBIGUOUS='1' if ambiguous else '0', FIXTURE_OTHER=self.other)
        result = foreground.request('launch', args=args, cwd=str(self.root), environment=env)
        self.assertTrue(result['ok'])
        control = self.peer(); plan = None
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            plan = control.request('inspect')
            if plan['processes'] and plan['continuity']['restorable']:
                time.sleep(0.04); return result['sessionToken'], control.request('inspect')
            if ambiguous and plan['processes']:return result['sessionToken'], plan
            time.sleep(0.01)
        self.fail('fixture native session did not become restorable')

    def test_new_unmanaged_process_after_review_is_not_stopped_and_invalidates_plan(self):
        token, plan = self.launch()
        env = dict(os.environ, FIXTURE_ROOT=str(self.root), FIXTURE_PRIVATE='private-fixture')
        import pty
        master, slave = pty.openpty()
        foreign = subprocess.Popen([str(self.binary), str(self.standin), '--conversation', self.cid],
                                   cwd=self.root, env=env, stdin=slave, stdout=slave, stderr=slave)
        try:
            time.sleep(0.05)
            with self.assertRaisesRegex(RuntimeError, 'stale-checkpoint|native-plan-unbound'):
                self.peer().request('stop', plan=plan)
            self.assertIsNone(foreign.poll())
            self.assertTrue((Path('/proc') / str(plan['processes'][0]['identity']['pid'])).exists())
        finally:
            foreign.terminate(); foreign.wait(timeout=3)
            os.close(master); os.close(slave)

    def test_complete_receipt_cannot_be_forged_or_replayed(self):
        token, plan = self.launch(); control = self.peer()
        receipt = control.request('stop', plan=plan)
        forged = {**receipt, 'stopped': []}
        with self.assertRaisesRegex(RuntimeError, 'incomplete-stop'):
            control.request('restart', receipt=forged)
        control.request('restart', receipt=receipt)
        with self.assertRaisesRegex(RuntimeError, 'incomplete-stop'):
            control.request('restart', receipt=receipt)

    def test_actual_foreground_launcher_keeps_normal_terminal_input_and_restores_tty(self):
        import pty, termios, fcntl
        self.standin.write_text(self.standin.read_text().replace(
            "for line in sys.stdin:", "for line in sys.stdin:\n if line=='fixture-exit\\n':sys.exit(0)"))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        original = termios.tcgetattr(slave)
        launcher = PUBLIC_RUNTIME / 'foreground_launcher.py'
        child = subprocess.Popen([sys.executable, str(launcher), '--socket', str(self.socket), str(self.standin)],
                                 cwd=self.root, env=dict(os.environ, FIXTURE_ROOT=str(self.root),
                                                        FIXTURE_PRIVATE='private-fixture'),
                                 stdin=slave, stdout=slave, stderr=slave)
        try:
            control = self.peer(); deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                plan = control.request('inspect')
                if plan['processes'] and plan['continuity']['restorable']:break
                time.sleep(0.02)
            self.assertTrue(plan['continuity']['restorable'])
            os.write(master, b'fixture-input-once\n')
            deadline = time.monotonic() + 2
            while not self.input_log.exists() and time.monotonic() < deadline:time.sleep(0.01)
            self.assertEqual(self.input_log.read_text(), 'fixture-input-once\n')
            os.write(master, b'fixture-exit\n')
            self.assertEqual(child.wait(timeout=3), 0)
            self.assertEqual(termios.tcgetattr(slave), original)
        finally:
            if child.poll() is None:child.terminate(); child.wait(timeout=3)
            os.close(master); os.close(slave)

    def test_input_during_stopped_interval_is_blocked_without_replay_or_foreground_exit(self):
        token, plan = self.launch(); control = self.peer()
        receipt = control.request('stop', plan=plan)
        result = self.foreground.request('input', bytes=base64.b64encode(b'never-deliver-or-replay\n').decode())
        self.assertEqual(result, {'ok':False, 'inputBlocked':True})
        with self.assertRaisesRegex(RuntimeError, 'restart-context-changed'):
            control.request('restart', receipt=receipt)
        self.assertFalse(self.input_log.exists())

    def test_detached_foreground_cannot_claim_restored_runtime_identity(self):
        token, plan = self.launch(); control = self.peer()
        receipt = control.request('stop', plan=plan); control.request('restart', receipt=receipt); time.sleep(0.05)
        self.foreground.close(); self.peers.remove(self.foreground); time.sleep(0.05)
        with self.assertRaisesRegex(RuntimeError, 'runtime-proof-unavailable'):
            control.request('prove', expected={'email':'first@example.com','subject':'fictional-first'})

    def test_exact_no_argument_foreground_workflow_native_standin_resumes_same_pty_and_conversation(self):
        import pty, termios, fcntl
        source = Path(__file__).resolve().parent / 'fixture_native_standin.c'
        built = self.root / 'compiled-standin'
        compiler = subprocess.run(['/usr/bin/gcc','-std=c11','-D_POSIX_C_SOURCE=200809L',
                                   '-Wall','-Wextra','-Werror',str(source),'-o',str(built)],
                                  capture_output=True, timeout=10)
        self.assertEqual(compiler.returncode, 0, 'controlled fixture compilation failed')
        built.chmod(0o700); os.replace(built, self.binary)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        original = termios.tcgetattr(slave)
        launcher = PUBLIC_RUNTIME / 'foreground_launcher.py'
        child = subprocess.Popen([sys.executable, str(launcher), '--socket', str(self.socket)],
                                 cwd=self.root, env=dict(os.environ, FIXTURE_ROOT=str(self.root),
                                  FIXTURE_PRIVATE='private-fixture', FIXTURE_CID=self.cid),
                                 stdin=slave, stdout=slave, stderr=slave)
        terminal = bytearray()
        try:
            control = self.peer(); deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                plan = control.request('inspect')
                if plan['processes'] and plan['continuity']['restorable']:break
                time.sleep(0.02)
            self.assertTrue(plan['continuity']['restorable'])
            before_pid = plan['processes'][0]['identity']['pid']
            receipt = control.request('stop', plan=plan)
            self.assertTrue(receipt['complete'])
            self.auth.write_text(json.dumps({'email':'second@example.com','subject':'fictional-second'}))
            control.request('restart', receipt=receipt)
            deadline = time.monotonic() + 3
            proof = None
            while time.monotonic() < deadline:
                try:
                    proof = control.request('prove', expected={'email':'second@example.com',
                        'subject':'fictional-second','plan':None,'source':'provider-userinfo',
                        'verifiedAt':'2026-10-01T00:00:00Z'})
                    break
                except RuntimeError:time.sleep(0.01)
            self.assertTrue(proof['sessionRestored'])
            after = control.request('inspect')
            self.assertNotEqual(before_pid, after['processes'][0]['identity']['pid'])
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                if select.select([master],[],[],0.05)[0]:terminal.extend(os.read(master,65536))
                if terminal.count(b'"event":"ready"') == 2:break
            headers = [json.loads(x) for x in terminal.splitlines() if x.startswith(b'{')]
            self.assertEqual(len(headers), 2)
            self.assertEqual([x['email'] for x in headers], ['first@example.com','second@example.com'])
            for field in ('pty','cwd','conversationId','envSentinel'):
                self.assertEqual(headers[0][field], headers[1][field])
            os.write(master,b'fixture-exit\n')
            self.assertEqual(child.wait(timeout=3), 0)
            self.assertEqual(termios.tcgetattr(slave), original)
            self.assertFalse(self.input_log.exists())
        finally:
            if child.poll() is None:child.terminate(); child.wait(timeout=3)
            os.close(master); os.close(slave)

    def test_real_socket_permissions_and_peer_uid(self):
        self.assertEqual(stat.S_IMODE(self.socket.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.socket.parent.stat().st_mode), 0o700)
        peer = self.peer()
        _, uid, _ = struct.unpack('3i', peer.socket.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        self.assertEqual(uid, os.getuid())
        self.assertTrue(peer.request('capability')['canProveRuntimeIdentity'])

    def test_real_stop_restart_preserves_foreground_pty_cwd_env_cid_without_replay(self):
        token, plan = self.launch()
        control = self.peer(); before = plan['processes'][0]['identity']
        receipt = control.request('stop', plan=plan)
        self.assertTrue(receipt['complete'])
        self.auth.write_text(json.dumps({'email':'second@example.com','subject':'fictional-second'}))
        control.request('restart', receipt=receipt)
        expected = {'email':'second@example.com','subject':'fictional-second','plan':None,
                    'source':'provider-userinfo','verifiedAt':'2026-10-01T00:00:00Z'}
        deadline = time.monotonic() + 3; proof = None
        while time.monotonic() < deadline:
            try:proof = control.request('prove', expected=expected); break
            except RuntimeError:time.sleep(0.02)
        self.assertIsNotNone(proof); self.assertTrue(proof['sessionRestored'])
        after = control.request('inspect')['processes'][0]['identity']
        self.assertNotEqual(before['pid'], after['pid']); self.assertNotEqual(before['startTime'], after['startTime'])
        # Force foreground drain with a harmless private RPC; collect real output.
        self.foreground.request('capability')
        lines = b''.join(self.foreground.outputs).splitlines()
        headers = [json.loads(x) for x in lines if x.startswith(b'{')]
        self.assertEqual(len(headers), 2)
        for field in ('pty','cwd','envSentinel','conversationId'):
            self.assertEqual(headers[0][field], headers[1][field])
        self.assertFalse(self.input_log.exists())
        safe = json.dumps(plan)
        for forbidden in ('private-env-not-for-http', str(self.root), str(self.standin), self.cid):
            self.assertNotIn(forbidden, safe)

    def test_normal_no_conversation_invocation_adopts_actual_open_db_not_newest_summary(self):
        token, plan = self.launch(explicit=False)
        self.assertTrue(plan['continuity']['restorable'])
        self.assertTrue((self.root / (self.cid + '.db')).exists())
        control = self.peer(); receipt = control.request('stop', plan=plan)
        self.assertTrue(receipt['complete']); control.request('restart', receipt=receipt)
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                control.request('prove', expected={'email':'first@example.com','subject':'fictional-first',
                    'plan':None,'source':'provider-userinfo','verifiedAt':'2026-10-01T00:00:00Z'})
                break
            except RuntimeError:time.sleep(0.01)
        self.foreground.request('capability')
        records = [json.loads(x) for x in b''.join(self.foreground.outputs).splitlines() if x.startswith(b'{')]
        self.assertEqual([x['conversationId'] for x in records], [self.cid, self.cid])

    def test_ambiguous_actual_open_conversations_remain_nonrestorable(self):
        token, plan = self.launch(explicit=False, ambiguous=True)
        self.assertFalse(plan['continuity']['restorable'])
        with self.assertRaisesRegex(RuntimeError, 'busy-or-unproven-idle'):
            self.peer().request('stop', plan=plan)

    def test_metadata_change_invalidates_checkpoint_before_signal(self):
        token, plan = self.launch()
        self.meta.write_text(json.dumps({'idle':False,'cid':self.cid}))
        with self.assertRaisesRegex(RuntimeError, 'stale-checkpoint|native-plan-unbound'):
            self.peer().request('stop', plan=plan)
        self.assertTrue((Path('/proc') / str(plan['processes'][0]['identity']['pid'])).exists())

    def test_foreground_disconnect_defers_automatic_or_manual_stop(self):
        token, plan = self.launch(); self.foreground.close(); self.peers.remove(self.foreground)
        time.sleep(0.05); current = self.peer().request('inspect')
        with self.assertRaisesRegex(RuntimeError, 'busy-or-unproven-idle'):
            self.peer().request('stop', plan=current)
        self.assertTrue((Path('/proc') / str(plan['processes'][0]['identity']['pid'])).exists())

    def test_native_user_input_is_delivered_once_and_not_replayed_after_restart(self):
        token, _ = self.launch()
        self.foreground.request('input', bytes=base64.b64encode(b'fixture-only-original-input\n').decode())
        deadline = time.monotonic() + 2
        while not self.input_log.exists() and time.monotonic() < deadline:time.sleep(0.01)
        time.sleep(0.05)
        control = self.peer(); plan = control.request('inspect')
        receipt = control.request('stop', plan=plan); self.assertTrue(receipt['complete'])
        control.request('restart', receipt=receipt); time.sleep(0.05)
        self.assertEqual(self.input_log.read_text(), 'fixture-only-original-input\n')

    def test_wrong_selected_subject_or_email_never_becomes_a_runtime_proof(self):
        token, plan = self.launch(); control = self.peer()
        receipt = control.request('stop', plan=plan); control.request('restart', receipt=receipt); time.sleep(0.05)
        for expected in ({'email':'wrong@example.com','subject':'fictional-first'},
                         {'email':'first@example.com','subject':'foreign-subject'}):
            with self.assertRaisesRegex(RuntimeError, 'runtime-proof-unavailable'):
                control.request('prove', expected=expected)

    def test_target_proof_failure_recovers_exact_same_pty_cwd_cid_once(self):
        _, plan = self.launch(); control = self.peer(); control.auto_complete = False
        receipt = control.request('stop', plan=plan)
        self.auth.write_text(json.dumps({'email':'second@example.com','subject':'fictional-second'}))
        control.request('restart', receipt=receipt); time.sleep(0.06)
        self.wait_proof(control, {'email':'second@example.com','subject':'fictional-second'})
        with self.assertRaisesRegex(RuntimeError, 'runtime-proof-unavailable'):
            control.request('prove', expected={'email':'wrong@example.com','subject':'fictional-second'})
        self.assertTrue(control.request('stop-owned-restarts')['ok'])
        self.auth.write_text(json.dumps({'email':'first@example.com','subject':'fictional-first'}))
        control.request('restart', receipt=receipt); time.sleep(0.06)
        expected = {'email':'first@example.com','subject':'fictional-first'}
        self.assertTrue(self.wait_proof(control, expected)['sessionRestored'])
        self.assertTrue(control.request('complete-transaction', expected=expected)['ok'])
        with self.assertRaisesRegex(RuntimeError, 'incomplete-stop'):
            control.request('restart', receipt=receipt)
        with self.assertRaisesRegex(RuntimeError, 'recovery-unavailable'):
            control.request('stop-owned-restarts')
        self.foreground.request('capability')
        headers = [json.loads(x) for x in b''.join(self.foreground.outputs).splitlines() if x.startswith(b'{')]
        self.assertEqual([x['email'] for x in headers], ['first@example.com','second@example.com','first@example.com'])
        for field in ('pty','cwd','conversationId','envSentinel'):
            self.assertEqual(len({header[field] for header in headers}), 1)
        self.assertFalse(self.input_log.exists())

    def test_quiesced_stop_proof_binds_exact_receipt_and_current_native_revision(self):
        _, plan = self.launch(); control = self.peer()
        receipt = control.request('stop', plan=plan)
        expected = {'email':'first@example.com','subject':'fictional-first'}
        fingerprint = hashlib.sha256(self.auth.read_bytes()).hexdigest()
        proof = control.request('approve-quiesced-stop', receipt=receipt, expected=expected, credentialFingerprint=fingerprint)
        self.assertTrue(proof['available']); self.assertTrue(proof['complete']); self.assertFalse(proof['busy'])
        self.assertIsInstance(proof['sampledAt'], str)
        changed = control.request('approve-quiesced-stop', receipt=receipt, expected=expected, credentialFingerprint='0'*64)
        self.assertFalse(changed['available']); self.assertIsNone(changed['sampledAt'])
        forged = control.request('approve-quiesced-stop', receipt={**receipt,'stopped':[]}, expected=expected, credentialFingerprint=fingerprint)
        self.assertFalse(forged['available'])

    def test_restart_keeps_input_blocked_and_generation_change_refuses_commit_and_recovery(self):
        _, plan = self.launch(); control = self.peer(); control.auto_complete = False
        receipt = control.request('stop', plan=plan)
        control.request('restart', receipt=receipt); time.sleep(0.06)
        self.wait_proof(control, {'email':'first@example.com','subject':'fictional-first'})
        self.assertEqual(self.foreground.request('input', bytes=base64.b64encode(b'never-replay\n').decode()),
                         {'ok':False,'inputBlocked':True})
        with self.assertRaisesRegex(RuntimeError, 'transaction-context-changed'):
            control.request('complete-transaction', expected={'email':'first@example.com','subject':'fictional-first'})
        with self.assertRaisesRegex(RuntimeError, 'recovery-context-changed'):
            control.request('stop-owned-restarts')
        self.assertFalse(self.input_log.exists())

    def test_foreign_writer_during_target_phase_is_not_stopped_or_omitted(self):
        _, plan = self.launch(); control = self.peer(); control.auto_complete = False
        receipt = control.request('stop', plan=plan); control.request('restart', receipt=receipt); time.sleep(0.06)
        self.wait_proof(control, {'email':'first@example.com','subject':'fictional-first'})
        import pty
        master, slave = pty.openpty()
        foreign = subprocess.Popen([str(self.binary),str(self.standin),'--conversation',self.cid],
            cwd=self.root, env=dict(os.environ,FIXTURE_ROOT=str(self.root),FIXTURE_PRIVATE='foreign'),
            stdin=slave,stdout=slave,stderr=slave)
        try:
            time.sleep(0.03)
            with self.assertRaisesRegex(RuntimeError, 'unmanaged-writer'):
                control.request('stop-owned-restarts')
            self.assertIsNone(foreign.poll())
            with self.assertRaisesRegex(RuntimeError, 'transaction-context-changed'):
                control.request('complete-transaction',expected={'email':'first@example.com','subject':'fictional-first'})
        finally:
            foreign.terminate(); foreign.wait(timeout=3); os.close(master); os.close(slave)

    def test_second_completed_transaction_does_not_prevent_new_exact_recovery(self):
        _, plan = self.launch(); control = self.peer(); control.auto_complete = False
        first = control.request('stop', plan=plan); control.request('restart', receipt=first); time.sleep(0.06)
        expected = {'email':'first@example.com','subject':'fictional-first'}
        self.wait_proof(control, expected)
        control.request('complete-transaction',expected=expected)
        second = control.request('stop',plan=control.request('inspect'))
        control.request('restart',receipt=second); time.sleep(0.06)
        self.wait_proof(control, expected)
        self.assertTrue(control.request('stop-owned-restarts')['ok'])
        control.request('restart',receipt=second); time.sleep(0.06)
        self.wait_proof(control, expected)
        self.assertTrue(control.request('complete-transaction',expected=expected)['ok'])
        with self.assertRaisesRegex(RuntimeError,'incomplete-stop'):
            control.request('restart',receipt=first)

    def test_unknown_method_unsafe_prompt_or_incomplete_receipt_no_launch_or_restart(self):
        control = self.peer()
        with self.assertRaisesRegex(RuntimeError, 'ipc-method-unsupported'):control.request('invented-action')
        for args in ([str(self.standin), '--print', 'secret-prompt'], [str(self.standin), '--continue']):
            with self.assertRaisesRegex(RuntimeError, 'unsafe-replay'):
                control.request('launch', args=args, cwd=str(self.root), environment=dict(os.environ))
        self.assertEqual(control.request('inspect')['processes'], [])
        with self.assertRaisesRegex(RuntimeError, 'incomplete-stop'):
            control.request('restart', receipt={'complete':False,'restartState':'foreign'})

    def test_malformed_or_oversized_frame_disconnects_without_action(self):
        for frame in (struct.pack('!I', MAX_FRAME + 1), struct.pack('!I', 1) + b'!'):
            peer = self.peer(); peer.socket.sendall(frame)
            self.assertEqual(peer.socket.recv(1), b'')
        self.assertEqual(self.peer().request('inspect')['processes'], [])

    def test_stop_owned_restarts_only_our_new_generation(self):
        token, plan = self.launch(); control = self.peer()
        receipt = control.request('stop', plan=plan); control.request('restart', receipt=receipt); time.sleep(0.05)
        pid = control.request('inspect')['processes'][0]['identity']['pid']
        control.request('stop-owned-restarts')
        self.assertFalse((Path('/proc') / str(pid)).exists())
        self.assertEqual(control.request('inspect')['processes'], [])


if __name__ == '__main__':unittest.main()
