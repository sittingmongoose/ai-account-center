"""Conditional native service fixtures; invented profiles and disposable PTYs only.

No installed native CLI, DBus credential method, real store or provider is used.
"""
from dataclasses import replace
from datetime import datetime, timezone, timedelta
import json
import os
from pathlib import Path
import pty
import selectors
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

RUNTIME = Path(__file__).resolve().parents[3] / 'scripts/antigravity/runtime'
sys.path.insert(0, str(RUNTIME))
from native_status_service import NativeStatusService
from runtime_continuity import ContinuityError
from native_status_attestor import ProcessIdentity, StatusError
from native_status_composition import create_native_status_factory
from native_status_omitted_counters import NATIVE_SHA256
from native_status_hook_cleanup import guarded_read,prepare_hook_reversal,apply_removal,CleanupError

CID = 'a74e320e-f6ae-4c7d-aa85-4fa77fd4eeef'
ROOT = ProcessIdentity(500,'fixture:500',os.getuid(),1,'/fixture/agy',1,50)
PARENT = ProcessIdentity(501,'fixture:501',os.getuid(),500,'/fixture/sh',1,51)
PEER = ProcessIdentity(502,'fixture:502',os.getuid(),501,'/fixture/python',1,52)


class Worker:
    def __init__(self,callback,*,close_fds=()):
        self.callback=callback;self.fds=close_fds;self.closed=False;self.result=None
    def poll(self):return self.result
    def close(self):self.closed=True;return True


class StatusServiceFixtures(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='aac-status-service-')
        self.addCleanup(self.temp.cleanup)
        self.directory=Path(self.temp.name);self.directory.chmod(0o700)
        self.helper=self.directory/'helper.py';self.helper.write_text('# disposable public helper')
        self.master,self.slave=pty.openpty()
        self.addCleanup(os.close,self.master);self.addCleanup(os.close,self.slave)
        self.session=SimpleNamespace(process=SimpleNamespace(pid=500,poll=lambda:None),
            master=self.master,slave=self.slave,generation=1,stopped=False,conversation=CID,cwd='/fixture/project')
        self.runtime=SimpleNamespace(sessions={'fixture-token':self.session},allowed=['/fixture/agy'],
            inspect_unmanaged=lambda:{'complete':True,'processes':[]})
        self.broker=SimpleNamespace(runtime=self.runtime,input_epochs={'fixture-token':0},listener=None,
            clients=[],capability=lambda:True,current_transaction=None)
        self.clock=[100.0]
        self.identities={v.pid:v for v in (ROOT,PARENT,PEER)}
        self.snapshot=SimpleNamespace(device=1,inode=2,modified_ns=3,credential=SimpleNamespace(revision='a'*64))
        self.profile={'email':'fixture@example.com','subject':'123456789',
            'source':'Google OAuth2 userinfo','verifiedAt':datetime.now(timezone.utc).isoformat(),
            'credentialFingerprint':'a'*64,'credentialCurrent':True,'nativeSnapshotIdentity':(1,2,3,'a'*64)}
        self.workers=[]
        def worker(*args,**kwargs):
            current=Worker(*args,**kwargs);self.workers.append(current);return current
        self.reader=Mock(side_effect=lambda:dict(self.profile))
        self.publisher=Mock(return_value=True);self.publisher.file_current=Mock(return_value=True)
        self.service=NativeStatusService(self.broker,path=self.directory/'status.sock',python_path=sys.executable,
            helper_path=self.helper,original_command_file=self.directory/'original.json',
            read_profile=self.reader,read_snapshot=lambda:self.snapshot,publisher_validator=self.publisher,
            producer_contract_approved=True,inspector=self.identities.__getitem__,worker_factory=worker,
            clock=lambda:self.clock[0])
        self.service.peer_validator=lambda pid:pid==PEER.pid
        self.service._observe=lambda session,record:{'nativeOpenedConversation':CID,'cwd':session.cwd,
            'hasNativePty':True,'runtimeStarted':True,'sqliteIdle':True}
        self.addCleanup(self.service.close)

    def frame(self,**changes):
        return json.dumps({'email':'fixture@example.com','conversation_id':CID,'cwd':'/fixture/project',
            'version':'1.2.16','agent_state':'idle',**changes}).encode()

    def capture(self,raw=None):
        return self.service.accept(self.frame() if raw is None else raw,peer_pid=502,peer_uid=os.getuid())

    def earn(self):
        self.capture();self.service.poll();self.workers[-1].result=dict(self.profile);self.service.poll()
        # The new complete frame earns a current sample; the network result never
        # restamps the old complete frame.
        self.capture();return self.service.proof(self.session)

    def test_deferred_one_worker_coalesces_many_frames(self):
        self.capture();self.assertEqual(self.workers,[])
        for _ in range(8):self.capture();self.service.poll()
        self.assertEqual(len(self.workers),1);self.reader.assert_not_called()
        self.assertIn(self.master,self.workers[0].fds);self.assertIn(self.slave,self.workers[0].fds)

    def test_current_native_identity_and_omitted_idle_require_earned_profile(self):
        self.capture();self.assertFalse(self.service.ready(self.session))
        self.workers[0].result=dict(self.profile);self.service.poll()
        result=self.service.proof(self.session)
        self.assertEqual(result['credentialFingerprint'],'a'*64)
        self.assertEqual(result['identity']['subject'],'123456789')
        self.assertEqual(self.service.identity(self.session)['source'],'native-runtime')
        self.assertEqual(self.service.census(self.broker)['busy'],False)

    def test_google_completion_never_retimestamps_original_sample(self):
        self.capture();self.service.poll();self.clock[0]+=1.01
        self.workers[0].result=dict(self.profile);self.service.poll()
        self.assertFalse(self.service.ready(self.session))
        self.capture();self.assertTrue(self.service.ready(self.session))

    def test_invalid_bound_latest_frame_clears_idle_and_foreign_peer_cannot(self):
        self.earn()
        with self.assertRaises(StatusError):self.capture(b'{"private":"CANARY"')
        self.assertFalse(self.service.ready(self.session))
        self.capture();self.assertTrue(self.service.ready(self.session))
        self.service.refuse_peer(999,os.getuid());self.assertTrue(self.service.ready(self.session))
        self.service.refuse_peer(502,os.getuid());self.assertFalse(self.service.ready(self.session))

    def test_positive_counters_or_new_busy_frame_cannot_become_idle(self):
        self.earn()
        for changes in ({'task_count':1},{'pending_input_count':1},{'tool_confirmation_pending':True},
                        {'agent_state':'working'}):
            self.capture(self.frame(**changes));self.assertFalse(self.service.ready(self.session))

    def test_input_generation_native_birth_or_publisher_change_invalidates(self):
        self.earn();self.broker.input_epochs['fixture-token']+=1
        self.assertFalse(self.service.ready(self.session));self.assertNotIn(id(self.session),self.service.records)
        self.capture();self.assertTrue(self.service.ready(self.session))
        self.identities[500]=replace(ROOT,birth='reused')
        self.assertFalse(self.service.ready(self.session))
        self.identities[500]=ROOT;self.capture();self.publisher.return_value=False
        self.assertFalse(self.service.ready(self.session))

    def test_new_birth_requires_fresh_poststart_verification(self):
        self.earn();self.identities[500]=replace(ROOT,birth='new-native-birth')
        self.capture();self.assertIsNone(self.service.profile)
        self.service.poll();self.assertEqual(len(self.workers),2)
        self.assertFalse(self.service.ready(self.session))
        self.workers[-1].result=dict(self.profile);self.service.poll();self.capture()
        self.assertTrue(self.service.ready(self.session))

    def test_current_native_revision_replaces_cache_without_faking_current(self):
        self.earn();self.snapshot.credential.revision='b'*64
        self.assertFalse(self.service.ready(self.session));self.assertIsNone(self.service.profile)
        self.profile.update(credentialFingerprint='b'*64,nativeSnapshotIdentity=(1,2,3,'b'*64))
        self.capture();self.service.poll();self.assertEqual(len(self.workers),2)
        self.workers[-1].result=dict(self.profile);self.service.poll();self.capture()
        self.assertEqual(self.service.proof(self.session)['credentialFingerprint'],'b'*64)

    def test_changed_snapshot_while_reader_running_is_never_blessed(self):
        self.capture();self.service.poll();self.snapshot.modified_ns=4
        self.workers[0].result=dict(self.profile);self.service.poll()
        self.assertFalse(self.service.ready(self.session));self.assertIsNone(self.service.profile)
        self.assertEqual(len(self.workers),1)

    def test_failed_reader_is_closed_and_cooldown_prevents_spawn_loop(self):
        self.capture();self.service.poll();self.workers[0].poll=Mock(side_effect=StatusError('fixture-refusal'))
        self.service.poll();self.assertTrue(self.workers[0].closed)
        for _ in range(5):self.capture();self.service.poll()
        self.assertEqual(len(self.workers),1)
        self.clock[0]+=60;self.capture();self.service.poll();self.assertEqual(len(self.workers),2)

    def test_owned_reader_timeout_and_service_close_release_worker(self):
        self.capture();self.service.poll();self.clock[0]+=5.01;self.service.poll()
        self.assertTrue(self.workers[0].closed);self.assertIsNone(self.service.worker)
        self.clock[0]+=60;self.capture();self.service.poll();self.service.close()
        self.assertTrue(self.workers[-1].closed)

    def test_default_contract_false_never_forks_and_never_claims_idle(self):
        self.service.approved=False;self.capture();self.service.poll()
        self.assertEqual(self.workers,[]);self.assertFalse(self.service.ready(self.session))
        self.assertFalse(self.service.census(self.broker)['available'])

    def test_selector_registers_owned_status_receiver_and_cleanup_unlinks_only_own_socket(self):
        with selectors.DefaultSelector() as selector:
            self.service.listen(selector)
            self.assertEqual(selector.get_key(self.service.receiver.listener).data,('status',self.service))
            self.service.close();self.assertFalse((self.directory/'status.sock').exists())
            self.assertEqual(len(selector.get_map()),0)

    def test_quiesced_validation_is_earned_stop_not_synthetic_idle(self):
        self.earn()
        expected=self.ts_identity()
        state={'binding':{'contexts':{'fixture-token':{}},'fingerprint':'a'*64,'expected':dict(expected)}}
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.session.stopped=True;self.session.process.poll=lambda:0
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.snapshot.credential.revision='b'*64
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertTrue(self.service.validate_transaction('restart',state,expected,None))
        self.publisher.file_current.return_value=False
        self.assertFalse(self.service.validate_transaction('restart',state,expected,None))

    def ts_identity(self, **changes):
        return {'email':'fixture@example.com','subject':'123456789','plan':None,
            'verifiedAt':datetime.now(timezone.utc).isoformat(),'source':'provider-userinfo',**changes}

    def stopped_state(self):
        self.earn();expected=self.ts_identity()
        self.session.stopped=True;self.session.process.poll=lambda:0
        return {'binding':{'contexts':{'fixture-token':{}},'fingerprint':'a'*64,
                          'expected':dict(expected)}},expected

    def shutdown_refresh(self, callback=None):
        self.snapshot=SimpleNamespace(device=1,inode=4,modified_ns=5,credential=SimpleNamespace(revision='b'*64))
        self.profile.update(credentialFingerprint='b'*64,nativeSnapshotIdentity=(1,4,5,'b'*64),
                            verifiedAt=datetime.now(timezone.utc).isoformat())
        self.immediate_reader(callback)

    def immediate_reader(self, callback=None):
        reader=callback or self.reader
        class ImmediateWorker(Worker):
            def poll(current):
                if current.result is None:current.result=reader()
                return current.result
        def factory(*args,**kwargs):
            worker=ImmediateWorker(*args,**kwargs);self.workers.append(worker);return worker
        self.service.worker_factory=factory

    def test_fresh_full_ts_identity_time_and_source_change_preserves_same_identity(self):
        state,expected=self.stopped_state()
        state['binding']['expected'].update(verifiedAt=(datetime.now(timezone.utc)-timedelta(seconds=2)).isoformat(),
                                            source='native-runtime')
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'a'*64))

    def test_same_identity_shutdown_refresh_earns_one_current_reader_and_caches_exact_snapshot(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.assertEqual(self.reader.call_count,1);self.assertEqual(len(self.workers),2)
        self.assertTrue(self.workers[-1].closed);self.assertEqual(self.service.verified_roots,set())
        self.assertIn(self.master,self.workers[-1].fds);self.assertIn(self.slave,self.workers[-1].fds)
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.assertEqual(self.reader.call_count,1);self.assertEqual(len(self.workers),2)

    def test_shutdown_snapshot_change_during_reader_refuses_and_does_not_spawn_retry(self):
        state,expected=self.stopped_state()
        def reader():
            value=dict(self.profile);self.snapshot.modified_ns=6;return value
        self.shutdown_refresh(reader)
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.assertTrue(self.workers[-1].closed);self.assertIsNone(self.service.profile)
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.assertEqual(len(self.workers),2)

    def test_foreign_shutdown_google_subject_refuses_without_claiming_new_account(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        self.profile['subject']='987654321'
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))

    def test_foreign_current_fingerprint_refuses_before_worker(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'c'*64))
        self.assertEqual(len(self.workers),1);self.reader.assert_not_called()

    def test_stale_ts_identity_or_invalid_authority_refuses_before_current_reader(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        for changes in ({'verifiedAt':(datetime.now(timezone.utc)-timedelta(seconds=301)).isoformat()},
                        {'source':'unverified-jwt'}, {'verifiedAt':'invalid'}, {'verifiedAt':None}):
            with self.subTest(field=next(iter(changes))):
                self.assertFalse(self.service.validate_transaction('quiesced',state,{**expected,**changes},'b'*64))
        self.assertEqual(len(self.workers),1)

    def test_stale_or_wrong_authority_google_cache_is_not_a_current_proof(self):
        state,expected=self.stopped_state()
        cached=dict(self.service.profile)
        self.service.profile['verifiedAt']=(datetime.now(timezone.utc)-timedelta(seconds=301)).isoformat()
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.service.profile={**cached,'source':'unverified-jwt'}
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))

    def test_expired_same_snapshot_google_cache_earns_fresh_reader_for_full_ts_identity(self):
        state,expected=self.stopped_state();self.immediate_reader()
        self.service.profile['verifiedAt']=(datetime.now(timezone.utc)-timedelta(seconds=301)).isoformat()
        self.clock[0]+=60
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertEqual(self.reader.call_count,1);self.assertEqual(len(self.workers),2)
        self.assertEqual(self.service.verified_roots,set())
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertEqual(self.reader.call_count,1)

    def test_expired_cache_reader_failure_cools_down_then_earns_only_one_fresh_retry(self):
        state,expected=self.stopped_state();self.immediate_reader()
        self.service.profile['verifiedAt']=(datetime.now(timezone.utc)-timedelta(seconds=301)).isoformat()
        self.clock[0]+=60;self.reader.side_effect=StatusError('profile-worker-unavailable')
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertTrue(self.workers[-1].closed);self.assertEqual(self.reader.call_count,1)
        self.reader.side_effect=lambda:dict(self.profile)
        for _ in range(3):
            self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertEqual(self.reader.call_count,1)
        self.clock[0]+=60
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertEqual(self.reader.call_count,2);self.assertEqual(len(self.workers),3)

    def test_expired_foreign_false_current_or_invalid_source_cache_is_not_refreshed_into_approval(self):
        state,expected=self.stopped_state();self.immediate_reader();self.clock[0]+=60
        original=dict(self.service.profile)
        for changes in ({'subject':'987654321'},{'credentialCurrent':False},
                        {'source':'unverified-jwt'},{'credentialFingerprint':'c'*64}):
            with self.subTest(field=next(iter(changes))):
                self.service.profile={**original,**changes,
                    'verifiedAt':(datetime.now(timezone.utc)-timedelta(seconds=301)).isoformat()}
                self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertEqual(len(self.workers),1);self.reader.assert_not_called()

    def test_monotonic_expired_unsafe_cache_cannot_be_cleared_then_reblessed_by_reader(self):
        state,expected=self.stopped_state();self.immediate_reader();self.clock[0]+=301
        original=dict(self.service.profile)
        for changes in ({'subject':'987654321'},{'credentialCurrent':False},
                        {'source':'unverified-jwt'},{'verifiedAt':'malformed'}):
            with self.subTest(field=next(iter(changes))):
                self.service.profile={**original,**changes}
                self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))
        self.assertEqual(len(self.workers),1);self.reader.assert_not_called()

    def test_google_reader_foreign_fingerprint_or_false_current_refuses(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        self.profile['credentialFingerprint']='c'*64
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.service._clear_profile();self.service.retry_after=0
        self.profile.update(credentialFingerprint='b'*64,credentialCurrent=False)
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))

    def test_new_managed_writer_during_current_reader_invalidates_quiescence(self):
        state,expected=self.stopped_state()
        def reader():
            self.runtime.sessions['new-writer']=SimpleNamespace(stopped=False,process=SimpleNamespace(poll=lambda:None))
            return dict(self.profile)
        self.shutdown_refresh(reader)
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))

    def test_unmanaged_or_incomplete_census_after_current_reader_refuses_quiescence(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        self.runtime.inspect_unmanaged=lambda:{'complete':True,'processes':[{'fixture-writer':True}]}
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.runtime.inspect_unmanaged=lambda:{'complete':False,'processes':[]}
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))

    def test_restart_birth_needs_new_poststart_reader_after_quiesced_refresh(self):
        state,expected=self.stopped_state();self.shutdown_refresh()
        self.assertTrue(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.session.stopped=False;self.session.process.poll=lambda:None
        self.session.generation+=1
        self.identities[500]=replace(ROOT,birth='after-quiesced-refresh')
        self.capture()
        self.assertIsNone(self.service.profile);self.service.poll()
        self.assertEqual(len(self.workers),3)
        self.assertTrue(self.service.ready(self.session))
        self.assertEqual(self.reader.call_count,2)

    def test_bounded_quiesced_reader_timeout_closes_worker_and_cooldown_prevents_repeated_queries(self):
        state,expected=self.stopped_state()
        self.snapshot.credential.revision='b'*64
        with patch('native_status_service.time.monotonic',side_effect=(1000,1006)):
            self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.assertTrue(self.workers[-1].closed);self.assertEqual(len(self.workers),2)
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'b'*64))
        self.assertEqual(len(self.workers),2);self.reader.assert_not_called()

    def test_no_runtime_gate_or_changed_expected_identity_cannot_earn_quiesced_approval(self):
        state,expected=self.stopped_state()
        for changes in ({'email':'other@example.com'},{'subject':'987654321'}):
            with self.subTest(field=next(iter(changes))):
                self.assertFalse(self.service.validate_transaction('quiesced',state,{**expected,**changes},'a'*64))
        self.service.approved=False
        self.assertFalse(self.service.validate_transaction('quiesced',state,expected,'a'*64))

    def restart_broker(self):
        from resident_broker import ResidentBroker
        broker=object.__new__(ResidentBroker)
        broker.runtime=self.runtime;broker.status_service=self.service
        broker.capability=lambda:True;broker.current_transaction='fixture-opaque'
        broker.input_epochs=self.broker.input_epochs
        broker.restart_tokens={'fixture-token'}
        # SimpleNamespace is unhashable; the disposable peer object supplies no IO.
        class Peer:
            def fileno(current):return 99
        peer=Peer();broker.foregrounds={'fixture-token':peer}
        broker.clients={peer:{'pid':700,'uid':os.getuid()}}
        self.session.argv=('--conversation',CID);self.session.environment={'FIXTURE':'only'}
        self.session.generation=2
        context=broker._context('fixture-token')
        broker.receipts={'fixture-opaque':{'phase':'target-running',
            'binding':{'contexts':{'fixture-token':list(context)}},
            'runningContexts':{'fixture-token':list(context)},
            'runningCredentialSnapshot':self.service._snapshot_key(self.snapshot),
            'runningNativeKeys':{'fixture-token':self.service._key(self.session)}}}
        broker.observers={'fixture-token':SimpleNamespace(ready=lambda:self.service.ready(self.session))}
        broker.identity_binder=self.service.bind
        self.runtime.prove=lambda token,email:{'email':email,'pid':500,'birth':self.identities[500].birth}
        return broker

    def assert_pending_code(self, broker, code):
        with self.assertRaises(ContinuityError) as caught:broker._proof(self.ts_identity())
        self.assertEqual(caught.exception.args,(code,))

    def test_owned_restart_missing_frame_and_pending_current_reader_are_typed_then_earn_real_proof(self):
        broker=self.restart_broker()
        self.assert_pending_code(broker,'runtime-startup-not-ready')
        self.capture();self.service.poll()
        self.assert_pending_code(broker,'runtime-startup-not-ready')
        self.workers[-1].result=dict(self.profile);self.service.poll();self.capture()
        proof=broker._proof(self.ts_identity())
        self.assertEqual(proof['identity']['subject'],self.profile['subject'])
        self.assertTrue(proof['runtimeStarted']);self.assertTrue(proof['sessionRestored'])

    def test_initial_nonrestart_or_wrong_phase_never_earns_startup_retry_code(self):
        broker=self.restart_broker()
        for phase in ('stopped','committed','unknown'):
            broker.receipts['fixture-opaque']['phase']=phase
            self.assert_pending_code(broker,'runtime-proof-unavailable')
        broker.current_transaction=None
        self.assert_pending_code(broker,'runtime-proof-unavailable')

    def test_restart_busy_positive_counter_or_wrong_account_frame_is_hard_refusal(self):
        broker=self.restart_broker()
        for changes in ({'agent_state':'working'},{'task_count':1},
                        {'pending_input_count':1},{'tool_confirmation_pending':True}):
            self.capture(self.frame(**changes));self.service.poll()
            self.assert_pending_code(broker,'runtime-proof-unavailable')
        self.capture();self.service.poll()
        with self.assertRaises(ContinuityError) as caught:
            broker._proof(self.ts_identity(email='foreign@example.com'))
        self.assertEqual(caught.exception.args,('runtime-proof-unavailable',))

    def test_restart_malformed_bound_first_frame_and_stale_frame_are_hard_refusals(self):
        broker=self.restart_broker()
        with self.assertRaises(StatusError):self.capture(b'{"fixture":"truncated"')
        self.assert_pending_code(broker,'runtime-proof-unavailable')
        self.capture();self.service.poll();self.clock[0]+=1.01
        self.assert_pending_code(broker,'runtime-proof-unavailable')

    def test_restart_new_birth_input_context_publisher_or_unowned_writer_is_hard_refusal(self):
        broker=self.restart_broker();state=broker.receipts['fixture-opaque']
        original_key=state['runningNativeKeys']['fixture-token']
        state['runningNativeKeys']['fixture-token']=('foreign-key',)
        self.assert_pending_code(broker,'runtime-proof-unavailable')
        state['runningNativeKeys']['fixture-token']=original_key
        self.broker.input_epochs['fixture-token']=1;broker.input_epochs=self.broker.input_epochs
        # _context normally owns this map; bind it explicitly in the stand-in.
        state['runningContexts']['fixture-token'][6]=0
        self.assert_pending_code(broker,'runtime-proof-unavailable')
        self.broker.input_epochs['fixture-token']=0
        self.publisher.file_current.return_value=False
        self.assert_pending_code(broker,'runtime-proof-unavailable')
        self.publisher.file_current.return_value=True
        self.runtime.inspect_unmanaged=lambda:{'complete':True,'processes':[{'fixture-outside':True}]}
        self.assert_pending_code(broker,'runtime-proof-unavailable')

    def test_restart_known_foreign_subject_or_revision_is_never_a_pending_readiness_response(self):
        broker=self.restart_broker();self.earn()
        with self.assertRaises(ContinuityError) as caught:
            broker._proof(self.ts_identity(subject='987654321'))
        self.assertEqual(caught.exception.args,('runtime-proof-unavailable',))
        self.service.profile['credentialFingerprint']='c'*64
        self.assert_pending_code(broker,'runtime-proof-unavailable')

    def test_startup_wait_never_masks_current_native_snapshot_or_revision_change(self):
        broker=self.restart_broker()
        self.snapshot.modified_ns+=1
        self.assert_pending_code(broker,'runtime-proof-unavailable')
        self.snapshot.modified_ns-=1;self.snapshot.credential.revision='b'*64
        self.assert_pending_code(broker,'runtime-proof-unavailable')

    def test_ready_sibling_identity_is_checked_before_another_owned_generation_can_wait(self):
        broker=self.restart_broker();self.earn()
        other=SimpleNamespace(**vars(self.session));other.process=SimpleNamespace(pid=600,poll=lambda:None)
        self.identities[600]=replace(ROOT,pid=600,birth='fixture:600')
        self.runtime.sessions['second']=other;broker.input_epochs['second']=0
        broker.foregrounds['second']=broker.foregrounds['fixture-token']
        broker.observers['second']=SimpleNamespace(ready=lambda:False)
        broker.restart_tokens.add('second');state=broker.receipts['fixture-opaque']
        context=broker._context('second')
        state['binding']['contexts']['second']=list(context)
        state['runningContexts']['second']=list(context)
        state['runningNativeKeys']['second']=self.service._key(other)
        self.assert_pending_code(broker,'runtime-startup-not-ready')
        with self.assertRaises(ContinuityError) as caught:
            broker._proof(self.ts_identity(subject='987654321'))
        self.assertEqual(caught.exception.args,('runtime-proof-unavailable',))

    def test_restart_reader_failure_is_hard_refusal_and_foreign_peer_cannot_create_failure(self):
        broker=self.restart_broker();self.capture();self.service.poll()
        self.service.refuse_peer(999,os.getuid())
        self.assert_pending_code(broker,'runtime-startup-not-ready')
        self.workers[-1].poll=Mock(side_effect=StatusError('profile-worker-unavailable'))
        self.assert_pending_code(broker,'runtime-proof-unavailable')

    def test_retained_committed_receipt_does_not_report_manual_switch_in_progress(self):
        self.earn();self.broker.current_transaction='retained-id'
        self.broker.receipts={'retained-id':{'phase':'committed'}}
        self.assertFalse(self.service.census(self.broker)['manualActivationInProgress'])
        for phase in ('stopped','target-running','recovery-running','unknown'):
            self.broker.receipts['retained-id']['phase']=phase
            self.assertTrue(self.service.census(self.broker)['manualActivationInProgress'])

    def test_commit_revalidates_running_original_sample_current_subject_and_revision(self):
        self.earn();state={'binding':{'contexts':{'fixture-token':{}}}}
        expected={'email':'fixture@example.com','subject':'123456789'}
        self.assertTrue(self.service.validate_transaction('commit',state,expected,None))
        self.assertFalse(self.service.validate_transaction('commit',state,{**expected,'subject':'999'},None))
        self.clock[0]+=1.01
        self.assertFalse(self.service.validate_transaction('commit',state,expected,None))

    def test_same_email_wrong_google_subject_does_not_bind_transaction(self):
        self.earn()
        with self.assertRaises(ContinuityError):self.service.bind({'email':'fixture@example.com','subject':'different'},
            [{'pid':500,'birth':ROOT.birth}])

    def test_all_sessions_share_exactly_one_private_reader(self):
        other=SimpleNamespace(**vars(self.session));other.process=SimpleNamespace(pid=600,poll=lambda:None)
        self.identities[600]=replace(ROOT,pid=600,birth='fixture:600')
        self.identities[602]=replace(PEER,pid=602,parent=600)
        self.runtime.sessions['second']=other;self.broker.input_epochs['second']=0
        self.service.peer_validator=lambda pid:pid in (502,602)
        self.capture();self.service.accept(self.frame(),peer_pid=602,peer_uid=os.getuid())
        self.service.poll();self.assertEqual(len(self.workers),1)
        self.workers[0].result=dict(self.profile);self.service.poll();self.capture()
        self.service.accept(self.frame(),peer_pid=602,peer_uid=os.getuid())
        self.assertTrue(self.service.ready(self.session));self.assertTrue(self.service.ready(other))


class LexicalReversalFixtures(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='aac-hook-reversal-');self.addCleanup(self.temp.cleanup)
        self.path=Path(self.temp.name)/'settings.json'
        self.original=b'{"theme":1,"statusLine":{"command":"prior user command","enabled":true},"tail":[]}'
        self.proposed=b'{"theme":1,"statusLine":{"command":"exact product hook","enabled":true},"tail":[]}'
        self.current=b'{\n "theme" : 2, "statusLine" : {"command":"exact product hook","enabled":true},\n "nativeAdded" : [5, 6], "tail" : []\n}\n'
        self.path.write_bytes(self.current);self.path.chmod(0o600)

    def proposal(self):
        pin,raw=guarded_read(self.path);return prepare_hook_reversal(self.original,self.proposed,raw,pin)

    def test_prior_user_hook_restored_without_losing_native_lexemes_or_values(self):
        plan=self.proposal();audit={};result=apply_removal(self.path,plan,before_commit=lambda:None,audit=audit)
        expected=self.current.replace(b'{"command":"exact product hook","enabled":true}',
            b'{"command":"prior user command","enabled":true}')
        self.assertTrue(audit['committed']);self.assertEqual(result['mode'],0o600);self.assertEqual(self.path.read_bytes(),expected)

    def test_foreign_hook_or_changed_leaf_is_never_overwritten(self):
        plan=self.proposal();self.path.write_bytes(self.current.replace(b'exact product hook',b'foreign hook'))
        with self.assertRaises(CleanupError):apply_removal(self.path,plan,before_commit=lambda:None)
        self.assertIn(b'foreign hook',self.path.read_bytes())
        pin,raw=guarded_read(self.path)
        with self.assertRaises(CleanupError):prepare_hook_reversal(self.original,self.proposed,raw,pin)


class ReleaseBoundaryFixtures(unittest.TestCase):
    def test_status_listener_startup_failure_closes_own_control_socket_without_native_launch(self):
        from resident_broker import ResidentBroker
        with tempfile.TemporaryDirectory(prefix='aac-status-listen-') as directory:
            path=Path(directory)/'control.sock'
            closed=[]
            service=SimpleNamespace(listen=Mock(side_effect=ContinuityError('fixture-status-unavailable')),
                close=lambda:closed.append(True))
            runtime=SimpleNamespace(sessions={})
            broker=ResidentBroker(runtime,path,status_service=service)
            with self.assertRaises(ContinuityError):broker.serve()
            self.assertEqual(closed,[True]);self.assertFalse(path.exists())
            self.assertEqual(runtime.sessions,{})

    def test_tui_email_without_native_status_never_proves_runtime_identity(self):
        from ubuntu_runtime_factory import create_ubuntu_resident_broker
        with patch('ubuntu_runtime_factory.NativeHeader',return_value=SimpleNamespace(account_email='fixture@example.com')):
            broker=create_ubuntu_resident_broker(binary='/fixture/agy',database='/fixture/db',socket_path='/fixture/socket')
            try:
                session=SimpleNamespace()
                broker.observer_factory(session)
                with self.assertRaises(ContinuityError):broker.runtime.native_identity(session)
            finally:broker.selector.close()

    def test_false_release_returns_before_binary_or_private_callback_access(self):
        for release in ({'nativeActivationReleased':False},{'nativeActivationReleased':1},None):
            with patch('native_status_composition.pin_native_file',side_effect=AssertionError('must not read')):
                self.assertIsNone(create_native_status_factory(release=release,binary='/DO-NOT-READ',home='/DO-NOT-READ',
                    status_path='/fixture/status',python_path='/fixture/python',helper_path='/fixture/helper',
                    original_command_file='/fixture/original'))

    def test_released_factory_uses_injected_callbacks_without_running_them(self):
        profile=Mock(side_effect=AssertionError('constructor must not contact provider'))
        snapshot=Mock(side_effect=AssertionError('constructor must not read private store'))
        factory=create_native_status_factory(release={'nativeActivationReleased':True,'nativeVersion':'1.2.16',
            'nativeSha256':NATIVE_SHA256,'nativeProofReceiptSha256':'b'*64},binary='/fixture/agy',home='/fixture/home',
            status_path='/fixture/status',python_path='/fixture/python',helper_path='/fixture/helper',
            original_command_file='/fixture/original',read_profile=profile,read_snapshot=snapshot,
            publisher_validator=lambda root:True)
        self.assertTrue(callable(factory));profile.assert_not_called();snapshot.assert_not_called()


if __name__=='__main__':unittest.main()
