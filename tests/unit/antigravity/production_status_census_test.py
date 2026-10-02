"""Exact census root selection and owned-family ancestry fixtures, offline only."""
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock,patch
RUNTIME=Path(__file__).resolve().parents[3]/'scripts/antigravity/runtime'
sys.path.insert(0,str(RUNTIME))
import runtime_continuity as runtime


class RootSelectionFixtures(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='aac-census-fixture-');self.addCleanup(self.temp.cleanup)
        self.proc=Path(self.temp.name)/'proc';self.proc.mkdir()
        self.parents={}
        def path(value):return self.proc if value=='/proc' else Path(value)
        self.mapping=patch.object(runtime,'Path',side_effect=path);self.mapping.start();self.addCleanup(self.mapping.stop)
        self.inspector=Mock(side_effect=lambda pid,allowed:SimpleNamespace(public=lambda:{'identity':{'pid':pid},'role':'cli'}))
        self.inspection=patch.object(runtime,'inspect_private',self.inspector);self.inspection.start();self.addCleanup(self.inspection.stop)

    def add(self,pid,comm,parent=1):
        directory=self.proc/str(pid);directory.mkdir();(directory/'comm').write_text(comm+'\n')
        fields=['S',str(parent),*['0']*20]
        (directory/'stat').write_text(str(pid)+' ('+comm+') '+' '.join(fields))

    def census(self):
        return runtime.census_cli('/fixture/agy',allowed_children=('/fixture/python3.13','/fixture/sh',
            '/fixture/bash','/fixture/agentapi','/fixture/webm_encoder'))

    def test_unrelated_interpreters_are_not_native_writer_roots(self):
        for pid,name in enumerate(('python3.13','python','sh','bash'),100):self.add(pid,name)
        self.assertEqual(self.census(),[]);self.inspector.assert_not_called()

    def test_actual_native_and_known_service_candidates_receive_full_inspection(self):
        for pid,name in enumerate(('agy','agentapi','webm_encoder','language_server'),200):self.add(pid,name)
        self.assertEqual(sorted(v.public()['identity']['pid'] for v in self.census()),list(range(200,204)))
        self.assertEqual(self.inspector.call_count,4)
        self.assertEqual(self.inspector.call_args_list[0].args[1],
            ['/fixture/agy','/fixture/python3.13','/fixture/sh','/fixture/bash','/fixture/agentapi','/fixture/webm_encoder'])

    def test_ambiguous_or_unreadable_selected_native_candidate_blocks_complete_census(self):
        self.add(300,'agy')
        for error in (runtime.ContinuityError('unreviewed-process'),PermissionError()):
            self.inspector.side_effect=error
            with self.assertRaises(runtime.ContinuityError):self.census()

    def test_renamed_interpreter_matching_native_root_is_never_skipped(self):
        self.add(400,'agy');self.inspector.side_effect=runtime.ContinuityError('unreviewed-executable')
        with self.assertRaises(runtime.ContinuityError):self.census()
        self.inspector.assert_called_once()

    def test_owned_native_family_keeps_exact_interpreter_descendants(self):
        self.add(500,'agy');self.add(501,'bash',500);self.add(502,'python3.13',501);self.add(600,'python3.13',1)
        identities={pid:{'pid':pid,'startTime':'fixture','ownerId':str(os.getuid()),'fingerprint':'a'*64} for pid in (500,501,502)}
        self.inspector.side_effect=lambda pid,allowed:SimpleNamespace(identity=identities[pid])
        managed=runtime.ManagedPtyRuntime('/fixture/agy',lambda *_:{},lambda *_:{},lambda *_:False,
            allowed_children=('/fixture/bash','/fixture/python3.13'))
        session=SimpleNamespace(process=SimpleNamespace(pid=500),owned_identities={})
        family=managed._family(session)
        self.assertEqual([v.identity['pid'] for v in family],[500,501,502])
        self.assertEqual(set(session.owned_identities),{500,501,502})
        self.assertNotIn(600,[v.identity['pid'] for v in family])

    def test_unknown_executable_with_earned_ancestry_blocks_family(self):
        self.add(700,'agy');self.add(701,'python3.13',700)
        self.inspector.side_effect=lambda pid,allowed: (_ for _ in ()).throw(runtime.ContinuityError('unreviewed-executable')) if pid==701 else SimpleNamespace(identity={'pid':700})
        managed=runtime.ManagedPtyRuntime('/fixture/agy',lambda *_:{},lambda *_:{},lambda *_:False,
            allowed_children=('/fixture/python3.13',))
        with self.assertRaises(runtime.ContinuityError):managed._family(SimpleNamespace(process=SimpleNamespace(pid=700),owned_identities={}))

if __name__=='__main__':unittest.main()
