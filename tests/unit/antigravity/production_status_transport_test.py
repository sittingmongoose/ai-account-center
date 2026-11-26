"""Injected HTTPS opener boundaries for native_status_userinfo; no network requests."""
import io
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock

SOURCE = Path(__file__).resolve().parents[3] / 'scripts/antigravity'
sys.path.insert(0, str(SOURCE))
from native_status_userinfo import request_userinfo, ENDPOINT, MAX_RESPONSE, NoRedirect


class UserinfoTransportFixtures(unittest.TestCase):
    def call(self, raw):
        opener = Mock()
        opener.open.return_value = io.BytesIO(raw)
        result = request_userinfo('invented-private-access', opener=opener)
        args, kwargs = opener.open.call_args
        self.assertEqual(args[0].full_url, ENDPOINT)
        self.assertEqual(args[0].method, 'GET')
        self.assertEqual(kwargs, {'timeout': 5})
        self.assertIsNone(args[0].data)
        self.assertEqual(opener.open.call_count, 1)
        return result

    def test_fixed_existing_saved_access_readonly_get_bound_response(self):
        value = {'email': 'fixture@example.com', 'id': '123', 'verified_email': True}
        self.assertEqual(self.call(json.dumps(value).encode()), value)

    def test_response_size_nonobject_duplicates_and_nonfinite_are_refused(self):
        for raw in (
            b'x' * (MAX_RESPONSE + 1),
            b'[]',
            b'{"email":"a","email":"b"}',
            b'{"unknown":Infinity}',
            b'{"unknown":1e999}',
            b'{',
        ):
            with self.subTest(length=len(raw)), self.assertRaises((ValueError, UnicodeError)):
                request_userinfo(
                    'invented-access', opener=Mock(open=Mock(return_value=io.BytesIO(raw)))
                )

    def test_redirect_handler_refuses_alternate_endpoint(self):
        self.assertIsNone(
            NoRedirect().redirect_request(None, None, 302, 'fixture', {}, 'https://example.invalid')
        )

    def test_invalid_access_never_opens_transport(self):
        opener = Mock()
        for token in ('', None, False, 'x' * 16385):
            with self.assertRaises(ValueError):
                request_userinfo(token, opener=opener)
        opener.open.assert_not_called()


if __name__ == '__main__':
    unittest.main()
