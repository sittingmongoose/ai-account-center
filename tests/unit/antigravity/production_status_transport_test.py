"""Injected HTTPS opener and parser-version/path boundaries; no requests/actions."""
import importlib.metadata
import io
import json
from pathlib import Path
import sys
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


class ParserEnvironmentFixtures(unittest.TestCase):
    def test_exact_pinned_existing_packages_and_current_minor_sitepath_pass(self):
        import pyte
        with patch.object(resident_main,'PARSER',Path(pyte.__file__).parent.parent):
            resident_main.verify_parser_environment()

    def test_wrong_package_version_or_outside_bundle_path_fails(self):
        import pyte
        with patch.object(resident_main,'PARSER',Path(pyte.__file__).parent.parent),\
             patch.object(resident_main.importlib.metadata,'version',return_value='wrong'):
            with self.assertRaises(ContinuityError):resident_main.verify_parser_environment()
        with patch.object(resident_main,'PARSER',RUNTIME):
            with self.assertRaises(ContinuityError):resident_main.verify_parser_environment()

if __name__=='__main__':unittest.main()
