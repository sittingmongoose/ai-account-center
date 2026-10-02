"""Actual broker phase methods with invented private runtime/identity providers.

No native process, credential, Google endpoint or file is used. These prove the
phase coordinator's guards; they do not establish a released native binder.
"""
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
import copy
import hashlib
import os
import sys
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'scripts/antigravity/runtime'))
from resident_broker import ResidentBroker, ContinuityError, digest


class Peer:
    def fileno(self): return 201


class Runtime:
    def __init__(self):
        self.binary = '/fictional/native'; self.allowed = [self.binary]
        self.token = str(uuid.uuid4()); self.cid = str(uuid.uuid4())
        self.exit = None; self.pid = 45001; self.stops = []; self.starts = []; self.closed = []
        self.partial = False; self.idle = True; self.unmanaged = []
        self.sessions = {self.token: SimpleNamespace(conversation=self.cid, cwd='/fictional/project',
            master=101, slave=102, argv=('/fictional/native','--conversation',self.cid),
            environment={'FIXTURE':'private'}, generation=1, stopped=False,
            process=SimpleNamespace(poll=lambda: self.exit))}

    def metadata(self, cid, cwd): return {'idle':self.idle,'fingerprint':digest([cid,cwd]),'cwd':cwd}
    def inspect_unmanaged(self): return {'complete':True,'processes':self.unmanaged}
    def review(self, token):
        s = self.sessions[token]
        return {'complete':True,'processes':[{'role':'cli','identity':{'pid':self.pid,
            'startTime':str(s.generation),'ownerId':str(os.getuid()),'fingerprint':'a'*64}}],
            'continuity':{'restorable':True,'fingerprint':digest([s.conversation,s.cwd,s.master,
                s.slave,s.generation,self.pid])},'idle':self.idle}
    def stop_reviewed(self, plan):
        self.stops.append(copy.deepcopy(plan)); self.exit=0; self.sessions[self.token].stopped=True
        return {'complete':not self.partial,'stopped':[row['identity'] for row in plan['processes']],
                'restartState':self.token}
    def restart(self, receipt):
        s=self.sessions[receipt['restartState']]
        if not s.stopped:raise ContinuityError('not-stopped')
        self.starts.append((s.master,s.slave,s.conversation,s.cwd,s.argv,dict(s.environment)))
        s.generation+=1; self.pid+=1; self.exit=None; s.stopped=False
    def prove(self, token, email):
        s=self.sessions[token]
        return {'email':email,'source':'native-runtime','runtimeStarted':True,
                'sessionRestored':True,'conversationId':s.conversation}
    def close(self, token): self.closed.append(token); raise AssertionError('transaction closed PTY')


class BrokerPhases(unittest.TestCase):
    def setUp(self):
        self.runtime=Runtime(); self.account={'email':'first@example.com','subject':'fixture-first'}
        self.fingerprint='b'*64
        def binder(expected, proofs):
            if expected != self.account:raise ContinuityError('runtime-proof-unavailable')
            return {'identity':{**expected,'plan':None,'source':'native-runtime','verifiedAt':'2026-10-01T00:00:00Z'},
                    'credentialFingerprint':self.fingerprint,'runtimeStarted':True,'sessionRestored':True}
        def phase_validator(phase,state,expected,fingerprint):
            return phase != 'quiesced' or (expected==self.account and fingerprint==self.fingerprint)
        self.broker=ResidentBroker(self.runtime,'/fictional/socket',capability=lambda:True,
            identity_binder=binder,transaction_validator=phase_validator)
        self.peer=Peer(); token=self.runtime.token
        self.broker.clients[self.peer]={'pid':901,'uid':os.getuid(),'foreground':token}
        self.broker.foregrounds[token]=self.peer
        self.broker.observers[token]=SimpleNamespace(ready=lambda:True,input_pending=lambda:None)
        self.broker.input_epochs[token]=0; self.messages=[]
        self.broker._send=lambda peer,value:self.messages.append(value)
        self.census=patch('resident_broker.census_cli',return_value=[]);self.census.start()
        self.addCleanup(self.census.stop);self.addCleanup(self.broker.selector.close)

    def approve(self):
        plan=self.broker._inspect()
        self.assertTrue(self.broker._approve_idle_plan(plan,self.account,self.fingerprint)['approved'])
        return plan
    def stopped(self):return self.broker._stop(self.approve())

    def test_fresh_quiesced_proof_has_new_timestamp_only_after_bound_stop(self):
        receipt=self.stopped()
        proof=self.broker._quiesced(receipt,self.account,self.fingerprint)
        self.assertTrue(proof['available']);self.assertFalse(proof['busy'])
        self.assertIsInstance(proof['sampledAt'],str)

    def test_each_private_context_change_refuses_stop_before_any_signal(self):
        for field,changed in [('conversation',str(uuid.uuid4())),('cwd','/foreign/project'),
            ('master',777),('slave',778),('argv',('/foreign/argv',)),
            ('environment',{'FOREIGN':'value'}),('generation',9)]:
            with self.subTest(field=field):
                plan=self.approve();s=self.runtime.sessions[self.runtime.token];old=getattr(s,field)
                setattr(s,field,changed)
                with self.assertRaises(ContinuityError):self.broker._stop(plan)
                self.assertEqual(self.runtime.stops,[]);setattr(s,field,old)

    def test_changed_email_subject_or_revision_refuses_stop_before_any_signal(self):
        for field,changed in [('email','other@example.com'),('subject','other-subject'),('revision','c'*64)]:
            with self.subTest(field=field):
                plan=self.approve();oldaccount=dict(self.account);oldrevision=self.fingerprint
                if field=='revision':self.fingerprint=changed
                else:self.account={**self.account,field:changed}
                with self.assertRaises(ContinuityError):self.broker._stop(plan)
                self.assertEqual(self.runtime.stops,[]);self.account=oldaccount;self.fingerprint=oldrevision

    def test_input_generation_and_foreground_replacement_refuse_stop(self):
        plan=self.approve();self.broker.input_epochs[self.runtime.token]+=1
        with self.assertRaises(ContinuityError):self.broker._stop(plan)
        self.assertEqual(self.runtime.stops,[])
        self.broker.input_epochs[self.runtime.token]=0;plan=self.approve()
        self.broker.clients[self.peer]['pid']+=1
        with self.assertRaises(ContinuityError):self.broker._stop(plan)
        self.assertEqual(self.runtime.stops,[])

    def test_quiesced_each_changed_context_or_queued_input_is_unavailable(self):
        receipt=self.stopped();s=self.runtime.sessions[self.runtime.token]
        for field,changed in [('conversation',str(uuid.uuid4())),('cwd','/other'),('master',9),
                              ('slave',10),('generation',3),('environment',{'new':'env'})]:
            with self.subTest(field=field):
                original=getattr(s,field);setattr(s,field,changed)
                self.assertFalse(self.broker._quiesced(receipt,self.account,self.fingerprint)['available'])
                setattr(s,field,original)
        self.broker.input_epochs[self.runtime.token]+=1
        self.assertFalse(self.broker._quiesced(receipt,self.account,self.fingerprint)['available'])

    def test_partial_stop_cannot_restart_or_earn_quiesced_proof(self):
        self.runtime.partial=True;receipt=self.stopped()
        self.assertFalse(receipt['complete'])
        self.assertFalse(self.broker._quiesced(receipt,self.account,self.fingerprint)['available'])
        with self.assertRaises(ContinuityError):self.broker._restart(receipt)
        self.assertEqual(self.runtime.starts,[])

    def test_unmanaged_writer_prevents_recovery_and_is_never_stopped(self):
        receipt=self.stopped();self.broker._restart(receipt)
        self.runtime.unmanaged=[{'role':'cli','identity':{'pid':999}}]
        with self.assertRaises(ContinuityError):self.broker._stop_owned_restarts()
        self.assertEqual(len(self.runtime.stops),1);self.assertEqual(self.runtime.closed,[])

    def test_target_failure_recovers_same_pty_template_once_without_replay(self):
        receipt=self.stopped();self.broker._restart(receipt)
        self.assertNotIn({'switchState':'resumed'},self.messages)
        self.assertTrue(self.broker._stop_owned_restarts()['ok'])
        self.broker._restart(receipt);self.assertEqual(self.runtime.starts[0],self.runtime.starts[1])
        self.assertEqual(self.runtime.closed,[])
        self.assertTrue(self.broker._complete(self.account)['ok'])
        self.assertEqual(self.messages.count({'switchState':'resumed'}),1)
        with self.assertRaises(ContinuityError):self.broker._restart(receipt)
        with self.assertRaises(ContinuityError):self.broker._stop_owned_restarts()

    def test_blocked_input_invalidates_commit_and_recovery_without_delivery(self):
        receipt=self.stopped();self.broker._restart(receipt)
        result=self.broker.dispatch(self.peer,{'requestId':'fixture','method':'input','bytes':'eAo='})
        self.assertTrue(result['inputBlocked'])
        with self.assertRaises(ContinuityError):self.broker._complete(self.account)
        with self.assertRaises(ContinuityError):self.broker._stop_owned_restarts()
        self.assertNotIn({'switchState':'resumed'},self.messages)

    def test_missing_native_phase_validator_cannot_promote_stopped_state(self):
        receipt=self.stopped();self.broker.transaction_validator=None
        self.assertFalse(self.broker._quiesced(receipt,self.account,self.fingerprint)['available'])
        with self.assertRaises(ContinuityError):self.broker._restart(receipt)
        self.assertEqual(self.runtime.starts,[])


if __name__=='__main__':unittest.main()
