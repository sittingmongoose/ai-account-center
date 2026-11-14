"""Terminal metadata is part of parity, even when the result value matches."""
import importlib.util
import pathlib
import subprocess
import unittest
import json
spec=importlib.util.spec_from_file_location('parity_runner',pathlib.Path(__file__).with_name('run.py'))
runner=importlib.util.module_from_spec(spec);spec.loader.exec_module(runner)
class TerminalTests(unittest.TestCase):
 def test_rejects_equal_values_with_wrong_terminal_metadata(self):
  event={'type':'result','protocol':1,'id':'parity','value':{}}
  for change,code in [({'id':'wrong'},0),({'type':'failure'},0),({'protocol':2},0),({},1)]:
   with self.subTest(change=change,code=code),self.assertRaises(AssertionError):
    runner.terminal(subprocess.CompletedProcess([],code,json.dumps(event|change)))
  self.assertEqual(runner.terminal(subprocess.CompletedProcess([],0,json.dumps(event))),{})
if __name__=='__main__':unittest.main()
