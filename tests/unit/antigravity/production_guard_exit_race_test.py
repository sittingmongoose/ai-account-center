"""Disposable Python parent/child only. No installed native/provider/user state."""
import importlib.util
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

GUARD=Path(__file__).resolve().parents[3]/'scripts/antigravity/runtime/linux_process_guard.py'


def load(path,name):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module


class ExitRaceFixtures(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='aigr-');self.addCleanup(self.temp.cleanup)
        self.script=Path(self.temp.name)/'owned_parent.py'
        self.script.write_text('''import os,signal,subprocess,sys,time
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
def stop(*_):
 child.terminate()
 child.wait(timeout=2)
 raise SystemExit(0)
signal.signal(signal.SIGTERM,stop)
print(child.pid,flush=True)
while True:time.sleep(.1)
''')
        self.parent=None;self.child=None;self.identities=None;self.child_fd=None

    def tearDown(self):
        try:
            if self.parent:
                if self.parent.poll() is None:self.parent.terminate()
                self.parent.wait(timeout=3)
        finally:
            if self.parent:self.parent.stdout.close()
            if self.child_fd is not None:
                try:
                    if not select.select([self.child_fd],[],[],0)[0]:
                        signal.pidfd_send_signal(self.child_fd,signal.SIGTERM)
                        if not select.select([self.child_fd],[],[],2)[0]:
                            self.fail('owned stand-in child cleanup incomplete')
                finally:os.close(self.child_fd)

    def run_race(self,guard):
        self.parent=subprocess.Popen([sys.executable,str(self.script)],stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
        if not select.select([self.parent.stdout],[],[],2)[0]:self.fail('owned stand-in not ready')
        self.child=int(self.parent.stdout.readline())
        # Retain an independent exact-child pidfd for fixture teardown. Check
        # its live parent relation before it becomes a cleanup authority.
        child_fd=os.pidfd_open(self.child,0)
        try:
            fields=Path('/proc',str(self.child),'stat').read_text().rsplit(')',1)[1].split()
            if int(fields[1])!=self.parent.pid:self.fail('stand-in child ancestry changed')
        except BaseException:
            os.close(child_fd);raise
        self.child_fd=child_fd
        self.identities=[guard.inspect(pid,[sys.executable]) for pid in (self.parent.pid,self.child)]
        real_open=os.pidfd_open;real_select=select.select;real_signal=signal.pidfd_send_signal
        fds={};sent_parent=False;forced_gap=False
        def opened(pid,*args):
            fd=real_open(pid,*args);fds[pid]=fd;return fd
        def signaled(fd,*args,**kwargs):
            nonlocal sent_parent
            value=real_signal(fd,*args,**kwargs)
            if fd==fds.get(self.parent.pid):sent_parent=True
            return value
        def scheduled_gap(readers,writers,errors,timeout=0):
            nonlocal forced_gap
            child_fd=fds.get(self.child)
            if sent_parent and not forced_gap and readers==[child_fd] and timeout==0:
                # Exact real scheduling window: the preceding readiness check
                # saw an alive child, then parent SIGTERM causes child exit
                # before the next /proc read. No fake successful signal/proof.
                if not real_select([child_fd],[],[],2)[0]:self.fail('owned child did not exit')
                forced_gap=True;return ([],[],[])
            return real_select(readers,writers,errors,timeout)
        with patch.object(guard.os,'pidfd_open',opened),patch.object(guard.signal,'pidfd_send_signal',signaled),patch.object(guard.select,'select',scheduled_gap):
            try:return guard.stop_reviewed(self.identities,[sys.executable],timeout_seconds=1)
            finally:self.assertTrue(sent_parent);self.assertTrue(forced_gap)

    def test_reviewed_exited_child_requires_exact_pidfd_confirmed_exit(self):
        guard=load(GUARD,'candidate_guard_race')
        result=self.run_race(guard)
        self.assertTrue(result['complete']);self.assertEqual(len(result['stopped']),2)
        self.assertEqual({x['pid'] for x in result['stopped']},{self.parent.pid,self.child})
        self.parent.wait(timeout=2)
        self.assertFalse(Path('/proc',str(self.child)).exists())

    def test_missing_metadata_with_live_pidfd_still_refuses(self):
        guard=load(GUARD,'candidate_guard_live')
        pid=12345;identity={'pid':pid,'startTime':'invented:1','ownerId':'1000','fingerprint':'a'*64}
        inspections=iter([identity,identity,FileNotFoundError()])
        def inspected(*_):
            value=next(inspections)
            if isinstance(value,Exception):raise value
            return value
        with patch.object(guard,'_allowed_executables',return_value=set()),patch.object(guard,'_inspect',side_effect=inspected),\
                patch.object(guard.os,'pidfd_open',return_value=99),patch.object(guard.os,'close'),\
                patch.object(guard.select,'select',return_value=([],[],[])),patch.object(guard.signal,'pidfd_send_signal') as sent:
            with self.assertRaises(FileNotFoundError):guard.stop_reviewed([identity],['/invented'],timeout_seconds=1)
            sent.assert_not_called()

    def test_changed_birth_still_refuses_entire_plan_before_signal(self):
        guard=load(GUARD,'candidate_guard_birth')
        identity={'pid':12345,'startTime':'invented:1','ownerId':'1000','fingerprint':'a'*64}
        changed=dict(identity,startTime='invented:2')
        with patch.object(guard,'_allowed_executables',return_value=set()),patch.object(guard,'_inspect',return_value=changed),\
                patch.object(guard.os,'pidfd_open',return_value=99),patch.object(guard.os,'close'),\
                patch.object(guard.signal,'pidfd_send_signal') as sent:
            with self.assertRaisesRegex(guard.GuardError,'stale-process'):guard.stop_reviewed([identity],['/invented'],timeout_seconds=1)
            sent.assert_not_called()


if __name__=='__main__':unittest.main()
