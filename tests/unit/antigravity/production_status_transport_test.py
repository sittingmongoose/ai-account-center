"""Injected HTTPS opener and parser-version/path boundaries; no requests/actions."""
import importlib
import importlib.metadata
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock,patch
RUNTIME=Path(__file__).resolve().parents[3]/'scripts/antigravity/runtime'
sys.path.insert(0,str(RUNTIME))
from native_status_userinfo import request_userinfo,ENDPOINT,MAX_RESPONSE,NoRedirect
import resident_main
from runtime_continuity import ContinuityError


class UserinfoTransportFixtures(unittest.TestCase):
    def call(self,raw):
        opener=Mock();opener.open.return_value=io.BytesIO(raw)
        result=request_userinfo('invented-private-access',opener=opener)
        args,kwargs=opener.open.call_args
        self.assertEqual(args[0].full_url,ENDPOINT);self.assertEqual(args[0].method,'GET')
        self.assertEqual(kwargs,{'timeout':5});self.assertIsNone(args[0].data)
        self.assertEqual(opener.open.call_count,1)
        return result

    def test_fixed_existing_saved_access_readonly_get_bound_response(self):
        value={'email':'fixture@example.com','id':'123','verified_email':True}
        self.assertEqual(self.call(json.dumps(value).encode()),value)

    def test_response_size_nonobject_duplicates_and_nonfinite_are_refused(self):
        for raw in (b'x'*(MAX_RESPONSE+1),b'[]',b'{"email":"a","email":"b"}',
                    b'{"unknown":Infinity}',b'{"unknown":1e999}',b'{'):
            with self.subTest(length=len(raw)),self.assertRaises((ValueError,UnicodeError)):
                request_userinfo('invented-access',opener=Mock(open=Mock(return_value=io.BytesIO(raw))))

    def test_redirect_handler_refuses_alternate_endpoint(self):
        self.assertIsNone(NoRedirect().redirect_request(None,None,302,'fixture',{},'https://example.invalid'))

    def test_invalid_access_never_opens_transport(self):
        opener=Mock()
        for token in ('',None,False,'x'*16385):
            with self.assertRaises(ValueError):request_userinfo(token,opener=opener)
        opener.open.assert_not_called()


def fake_parser(directory,versions=(('pyte','0.8.2'),('wcwidth','0.9.1'))):
    """Invented importable parser packages with dist-info metadata only."""
    for name,version in versions:
        (directory/name).mkdir(parents=True,exist_ok=True)
        (directory/name/'__init__.py').write_text('# invented fixture package\n')
        info=directory/('%s-%s.dist-info'%(name,version));info.mkdir(parents=True,exist_ok=True)
        (info/'METADATA').write_text('Metadata-Version: 2.1\nName: %s\nVersion: %s\n'%(name,version))


class ParserEnvironmentFixtures(unittest.TestCase):
    """Hermetic: invented packages in a version-neutral parser directory."""
    def setUp(self):
        temp=tempfile.TemporaryDirectory(prefix='aic-parser-');self.addCleanup(temp.cleanup)
        self.parser=Path(temp.name)/'parser';fake_parser(self.parser)
        self.forget();self.addCleanup(self.forget)
        sys.path.insert(0,str(self.parser));self.addCleanup(self.unpath)
        importlib.invalidate_caches()

    def forget(self):
        for name in ('pyte','wcwidth'):sys.modules.pop(name,None)

    def unpath(self):
        while str(self.parser) in sys.path:sys.path.remove(str(self.parser))

    def test_exact_pinned_packages_in_the_version_neutral_parser_directory_pass(self):
        self.assertEqual(resident_main.PARSER.name,'parser')
        with patch.object(resident_main,'PARSER',self.parser):
            resident_main.verify_parser_environment()

    def test_wrong_package_version_or_outside_bundle_path_fails(self):
        with patch.object(resident_main,'PARSER',self.parser),\
             patch.object(resident_main.importlib.metadata,'version',return_value='wrong'):
            with self.assertRaises(ContinuityError):resident_main.verify_parser_environment()
        with patch.object(resident_main,'PARSER',RUNTIME):
            with self.assertRaises(ContinuityError):resident_main.verify_parser_environment()

    def test_missing_parser_module_is_named_not_a_traceback(self):
        self.unpath();self.forget();importlib.invalidate_caches()
        if importlib.util.find_spec('pyte') is not None:
            self.skipTest('this interpreter has a system pyte')
        with patch.object(resident_main,'PARSER',self.parser.parent/'absent'):
            with self.assertRaisesRegex(resident_main.ParserUnavailable,'^missing Python module pyte$'):
                resident_main.verify_parser_environment()

if __name__=='__main__':unittest.main()
