"""Actual disposable broker/status sockets, birth-bound reader and native stand-in.

Every account and native observation is invented. No installed AGY, Google,
credential store, user conversation or system service is contacted.
"""
import hashlib
import io
import json
from datetime import datetime, timezone
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest
import production_resident_socket_test as fixture_source


class IntegratedStatusResidentFixtures(unittest.TestCase):
    def setUp(self):
        self.fixture=fixture_source.BrokerFixtures('runTest')
        self.fixture.setUp();self.addCleanup(self.fixture.temp.cleanup);self.addCleanup(self.fixture.tearDown)
        f=self.fixture
        f.server.terminate();f.server.wait(timeout=5);f.server.stderr.close()
        f.auth.write_text(json.dumps({'email':'first@example.com','subject':'123456789'}))
        f.original_status=f.root/'original-status.json';f.original_status.write_text('{"command":null}');f.original_status.chmod(0o600)
        # A native stand-in emits invented native status through the actual pinned
        # product hook process. It receives no prompt/model/tool input.
        source=f.standin.read_text().replace('import json,os,sys,time','import json,os,sys,time,subprocess,threading')
        source=source.replace('for line in sys.stdin:', '''def emit_status():
  account=json.loads((root/'fictional-account.json').read_text())
  status={'email':account['email'],'conversation_id':cid,'cwd':os.getcwd(),'version':'1.2.16','agent_state':'idle'}
  subprocess.run([os.environ['FIXTURE_HELPER_PYTHON'],'-I',os.environ['FIXTURE_STATUS_HELPER'],
   '--socket',str(root/'ipc/status.sock'),'--original-command-file',str(root/'original-status.json')],
   input=json.dumps(status).encode(),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=False)
def native_status():
 while True:
  if (root/'fixture-pause-publisher').exists():
   if not (root/'fixture-publisher-paused').exists():
    emit_status()
    (root/'fixture-publisher-paused').write_text('invented fixture publisher is quiet')
   time.sleep(0.005)
   continue
  emit_status()
  until=time.monotonic()+2.0
  while time.monotonic()<until and not (root/'fixture-pause-publisher').exists():time.sleep(0.005)
threading.Thread(target=native_status,daemon=True).start()
for line in sys.stdin:''')
        f.standin.write_text(source)
        script=f.server_script.read_text()
        script=script.replace('from runtime_continuity import ManagedPtyRuntime,ContinuityError,digest',
            'from runtime_continuity import ManagedPtyRuntime,ContinuityError,digest\nfrom native_status_service import NativeStatusService\nfrom types import SimpleNamespace\nfrom datetime import datetime,timezone')
        script=script.replace('def ready(self):return self.idle','def ready(self):return status_service.ready(self.session)')
        script=script.replace('def identity(session):return observers[id(session)].identity()',
            'def identity(session):return status_service.identity(session)')
        script=script.replace("runtime=ManagedPtyRuntime(str(root/'aic-fixture-cli'),metadata,identity,idle)",
            "runtime=ManagedPtyRuntime(str(root/'aic-fixture-cli'),metadata,identity,idle,allowed_children=(os.path.realpath(sys.executable),))")
        script=script.replace("signal.signal(signal.SIGTERM",'''def read_snapshot():
 info=(root/'fictional-account.json').stat()
 return SimpleNamespace(device=info.st_dev,inode=info.st_ino,modified_ns=info.st_mtime_ns,
  credential=SimpleNamespace(revision=hashlib.sha256((root/'fictional-account.json').read_bytes()).hexdigest()))
def read_profile():
 before=read_snapshot();account=json.loads((root/'fictional-account.json').read_text());after=read_snapshot()
 if vars(before)!=vars(after):raise ValueError('fixture changed')
 return {'email':account['email'],'subject':account['subject'],'source':'Google OAuth2 userinfo',
  'verifiedAt':datetime.now(timezone.utc).isoformat(),'credentialFingerprint':after.credential.revision,
  'credentialCurrent':True,'nativeSnapshotIdentity':(after.device,after.inode,after.modified_ns,after.credential.revision)}
class Publisher:
 def __call__(self,root_identity):return True
 def file_current(self):return True
status_service=NativeStatusService(broker,path=root/'ipc/status.sock',python_path=os.path.realpath(sys.executable),
 helper_path=Path('''+repr(str(fixture_source.PUBLIC_RUNTIME/'native_status_hook.py'))+'''),
 original_command_file=root/'original-status.json',read_profile=read_profile,read_snapshot=read_snapshot,
 publisher_validator=Publisher(),producer_contract_approved=True)
original_stop=runtime.stop_reviewed
def stop_reviewed(plan):
 try:return original_stop(plan)
 except Exception as error:
  with (root/'fixture-codes').open('a') as out:out.write('STOP:'+type(error).__name__+':'+str(error)+'\\n')
  raise
runtime.stop_reviewed=stop_reviewed
broker.status_service=status_service;broker.identity_binder=status_service.bind
broker.census_provider=status_service.census;broker.transaction_validator=status_service.validate_transaction
signal.signal(signal.SIGTERM''')
        f.server_script.write_text(script)
        f.server=subprocess.Popen([sys.executable,str(f.server_script),str(f.root)],
            env=dict(os.environ,FIXTURE_CAPABILITY='1'),stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        # The broker binds its socket before attaching the native status
        # listener. Wait for both fixture endpoints within the same deadline.
        status_socket=f.root/'ipc/status.sock'
        deadline=time.monotonic()+3
        while not (f.socket.exists() and status_socket.exists()) and time.monotonic()<deadline:
            if f.server.poll() is not None:self.fail('private stand-in broker failed')
            time.sleep(0.01)
        self.assertTrue(f.socket.exists())
        self.assertTrue(status_socket.exists())
        f.standin_env={'FIXTURE_HELPER_PYTHON':os.path.realpath(sys.executable),
                      'FIXTURE_STATUS_HELPER':str(fixture_source.PUBLIC_RUNTIME/'native_status_hook.py')}
        # Existing launch helper merges these fixture-only values into its private
        # foreground launch environment; never modifies process-wide env/HOME.
        def wait_proof(control,expected):
            deadline=time.monotonic()+3;last='not-observed'
            while time.monotonic()<deadline:
                try:return control.request('prove',expected=expected)
                except RuntimeError as error:last=str(error);time.sleep(0.025)
            self.fail('fixture status proof refusal: '+last+'; '+((f.root/'fixture-codes').read_text()[-1000:] if (f.root/'fixture-codes').exists() else 'no fixture events'))
        f.wait_proof=wait_proof
        def stop(control):
            # Coordinate the invented publisher explicitly: emit one fresh
            # frame, then prove its helper exited before reviewing the family.
            # This changes no product checkpoint/idle guard or native behavior.
            (f.root/'fixture-pause-publisher').write_text('disposable fixture pause')
            quiet=time.monotonic()+3
            while not (f.root/'fixture-publisher-paused').exists() and time.monotonic()<quiet:time.sleep(0.005)
            self.assertTrue((f.root/'fixture-publisher-paused').exists())
            deadline=time.monotonic()+3;last='not-reviewed'
            while time.monotonic()<deadline:
                plan=control.request('inspect')
                if len(plan['processes'])!=1:time.sleep(0.01);continue
                try:
                    receipt=control.request('stop',plan=plan)
                    self.assertTrue(receipt['complete'],'fixture partial stop: '+((f.root/'fixture-codes').read_text()[-1000:] if (f.root/'fixture-codes').exists() else 'no codes'))
                    (f.root/'fixture-pause-publisher').unlink()
                    (f.root/'fixture-publisher-paused').unlink()
                    return receipt
                except RuntimeError as error:
                    last=str(error)
                    if last not in ('stale-checkpoint','native-plan-unbound','runtime-proof-unavailable','busy-or-unproven-idle'):raise
                    time.sleep(0.025)
            self.fail('fixture exact quiet family never earned stop: '+last)
        f.stop=stop
        original_peer=f.peer
        def peer():
            current=original_peer();original_request=current.request
            def request(method,**parameters):
                if method=='launch':parameters['environment'].update(f.standin_env)
                return original_request(method,**parameters)
            current.request=request
            return current
        f.peer=peer

    def tearDown(self):
        self.fixture.server.terminate();self.fixture.server.wait(timeout=5)
        if self.fixture.server.returncode!=0:
            raw=self.fixture.server.stderr.read()
            self.fixture.server.stderr.close();self.fixture.server.stderr=io.BytesIO(raw)
            sys.stderr.write('INVENTED FIXTURE SERVER ERROR: '+raw.decode(errors='replace')+'\n')

    def initial(self):
        f=self.fixture;token,plan=f.launch();control=f.peer();control.auto_complete=False
        # Match the actual TS coordinator's fresh identity DTO without changing
        # the invented native credential bytes or their snapshot fingerprint.
        expected={**json.loads(f.auth.read_text()),'plan':None,
            'verifiedAt':datetime.now(timezone.utc).isoformat(),'source':'provider-userinfo'}
        proof=f.wait_proof(control,expected);time.sleep(0.05)
        native_pid=control.request('inspect')['processes'][0]['identity']['pid']
        f.native_before=(native_pid,os.readlink('/proc/'+str(native_pid)+'/fd/0'),
            os.readlink('/proc/'+str(native_pid)+'/cwd'))
        return token,plan,control,expected,proof

    def test_exact_owned_session_status_selector_stop_restart_and_durable_input_release(self):
        f=self.fixture;token,plan,control,expected,before=self.initial()
        receipt=f.stop(control)
        self.assertTrue(receipt['complete']);self.assertTrue(control.request('approve-quiesced-stop',
            receipt=receipt,expected=expected,credentialFingerprint=hashlib.sha256(f.auth.read_bytes()).hexdigest())['available'])
        control.request('restart',receipt=receipt)
        after=f.wait_proof(control,expected)
        self.assertTrue(after['runtimeStarted']);self.assertTrue(after['sessionRestored'])
        self.assertEqual(after['identity']['subject'],expected['subject'])
        native_pid=control.request('inspect')['processes'][0]['identity']['pid']
        self.assertNotEqual(native_pid,f.native_before[0])
        self.assertEqual(os.readlink('/proc/'+str(native_pid)+'/fd/0'),f.native_before[1])
        self.assertEqual(os.readlink('/proc/'+str(native_pid)+'/cwd'),f.native_before[2])
        control.request('complete-transaction',expected=expected)
        census=control.request('census')
        self.assertFalse(census['busy']);self.assertFalse(census['manualActivationInProgress'])
        # Transcript/history fixture never changed; old foreground session token,
        # PTY/cwd/env are asserted by the existing actual phase receipt fixtures.
        self.assertIsNone(f.foreground.exit)
        self.assertEqual(hashlib.sha256((f.brain/'work').read_bytes()).hexdigest(),f.history_hash)

    def test_target_subject_failure_recovers_original_account_on_same_retained_terminal(self):
        f=self.fixture;token,plan,control,original,before=self.initial();original_bytes=f.auth.read_bytes()
        receipt=f.stop(control)
        f.auth.write_text(json.dumps({'email':'target@example.com','subject':'987654321'}))
        control.request('restart',receipt=receipt)
        target={'email':'target@example.com','subject':'987654321'}
        f.wait_proof(control,target)
        with self.assertRaises(RuntimeError):control.request('prove',expected={'email':target['email'],'subject':'111'})
        control.request('stop-owned-restarts')
        f.auth.write_bytes(original_bytes);control.request('restart',receipt=receipt)
        proof=f.wait_proof(control,original);self.assertEqual(proof['identity']['subject'],original['subject'])
        native_pid=control.request('inspect')['processes'][0]['identity']['pid']
        self.assertNotEqual(native_pid,f.native_before[0])
        self.assertEqual(os.readlink('/proc/'+str(native_pid)+'/fd/0'),f.native_before[1])
        self.assertEqual(os.readlink('/proc/'+str(native_pid)+'/cwd'),f.native_before[2])
        control.request('complete-transaction',expected=original)
        self.assertFalse(control.request('census')['manualActivationInProgress'])
        self.assertIsNone(f.foreground.exit)
        self.assertEqual(hashlib.sha256((f.brain/'work').read_bytes()).hexdigest(),f.history_hash)

if __name__=='__main__':unittest.main()
