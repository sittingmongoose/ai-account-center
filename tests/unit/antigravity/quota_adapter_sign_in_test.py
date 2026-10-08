"""A revoked saved Antigravity login reports sign-in, not temporary unavailability."""
import datetime as dt
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
sys.path.insert(0, str(ROOT / 'scripts/antigravity'))
import desktop_helpers as helpers
import desktop_usage as desktop
from auth_platforms import IdentityUnavailable, NeedsSignIn, PrivateCredential, refresh_in_memory, revision
from quota_adapter import SavedProfile, collect_snapshot, existing_collector_dependencies

NOW = dt.datetime(2026, 10, 7, tzinfo=dt.timezone.utc)
TOKEN_URL = 'https://oauth2.googleapis.com/token'
REVOKED = b'{"error":"invalid_grant","error_description":"Token has been expired or revoked."}'


def expired_login():
    raw = json.dumps({'auth_method': 'consumer', 'token': {
        'access_token': 'synthetic-access', 'refresh_token': 'synthetic-refresh',
        'token_type': 'Bearer', 'expiry': '2026-10-06T00:00:00Z'}}).encode()
    return PrivateCredential(raw, revision(raw))


class RefreshFailureMappingTests(unittest.TestCase):
    def test_needs_sign_in_refresh_failure_becomes_needs_sign_in(self):
        def refresh(_refresh_token):
            raise helpers.UsageError('needs_sign_in', "The saved account's sign-in has expired or was revoked.")
        with self.assertRaises(NeedsSignIn):
            refresh_in_memory(expired_login(), refresh, NOW)

    def test_other_refresh_failures_stay_unavailable(self):
        def refresh(_refresh_token):
            raise helpers.UsageError('error', 'The usage service is temporarily unavailable.')
        with self.assertRaises(IdentityUnavailable):
            refresh_in_memory(expired_login(), refresh, NOW)


class RevokedSavedLoginTests(unittest.TestCase):
    def collect(self, body):
        saved = expired_login()
        profile = SavedProfile('fixture', 'fixture@example.com', 'a' * 64, saved.revision, saved)
        with tempfile.TemporaryDirectory(prefix='aac-revoked-login-fixture-') as root, \
                patch.object(desktop, 'antigravity_oauth_client',
                             return_value=('fixture-client.apps.googleusercontent.com', 'synthetic-app-value')), \
                patch.object(helpers.urllib.request, 'build_opener') as opener:
            error = urllib.error.HTTPError(TOKEN_URL, 400, 'Bad Request', {}, io.BytesIO(body))
            self.addCleanup(error.close)
            opener.return_value.open.side_effect = error
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: NOW)
            result = collect_snapshot(profile, dependencies)
        return result, opener.return_value.open

    def test_revoked_saved_login_reports_needs_sign_in_after_one_token_request(self):
        result, sent = self.collect(REVOKED)
        self.assertEqual(result['status'], 'needs_sign_in')
        self.assertEqual(result['identityValidation'], 'needs_sign_in')
        self.assertFalse(result['identityVerified'])
        self.assertEqual(result['windows'], [])
        self.assertEqual(sent.call_count, 1)
        self.assertEqual(sent.call_args.args[0].full_url, TOKEN_URL)
        encoded = json.dumps(result)
        for private in ('synthetic-access', 'synthetic-refresh', 'synthetic-app-value', 'Token has been', 'invalid_grant'):
            self.assertNotIn(private, encoded)

    def test_other_token_refusals_stay_unavailable(self):
        for body in (b'{"error":"invalid_request"}', b'not json'):
            with self.subTest(body=body):
                result, _sent = self.collect(body)
                self.assertEqual(result['status'], 'unavailable')
                self.assertEqual(result['identityValidation'], 'unavailable')


if __name__ == '__main__':
    unittest.main()
