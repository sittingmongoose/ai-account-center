"""OpenCode quota failures use private fixtures and never contact a provider."""

import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

HELPERS = Path(__file__).resolve().parents[3] / "scripts/account-usage"
sys.path.insert(0, str(HELPERS))
import failover_additional as failover
import plan_common as common
import plan_usage as usage

GO_URL = "https://opencode.ai/zen/go/v1/usage"
PRIVATE = "fixture-private-key-and-response"


class OpenCodeGoFailureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.ccs = self.home / ".ccs"
        auth = self.home / ".local/share/opencode/auth.json"
        auth.parent.mkdir(parents=True, mode=0o700)
        auth.write_text(json.dumps({"opencode-go": {"type": "api", "key": PRIVATE}}))
        auth.chmod(0o600)
        self.environment = patch.dict(os.environ, {}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        network = patch("urllib.request.OpenerDirector.open",
                        side_effect=AssertionError("Unexpected provider request"))
        network.start()
        self.addCleanup(network.stop)

    def rejected(self, status, url=GO_URL):
        error = urllib.error.HTTPError(url, status, PRIVATE, {"Retry-After": "600"},
                                       io.BytesIO(PRIVATE.encode()))
        self.addCleanup(error.close)
        return patch("urllib.request.OpenerDirector.open", side_effect=error)

    def test_legacy_go_distinguishes_forbidden_usage_from_rejected_credentials(self):
        for http, status in [(403, "error"), (401, "needs_sign_in"), (429, "error")]:
            with self.subTest(http=http), self.rejected(http):
                result = usage.collect("opencode-go", "ubuntu", self.home)
                self.assertEqual(result["status"], status)
                self.assertEqual(result["windows"], [])
                self.assertIsNone(result["sampledAt"])
                self.assertNotIn(PRIVATE, json.dumps(result))

    def test_failover_go_only_requests_sign_in_for_rejected_credentials(self):
        candidate = failover.inventory("opencode-go", self.home, "ubuntu", self.ccs)[0]
        for http, code in [(403, "provider_error"), (401, "needs_sign_in"), (429, "rate_limited")]:
            with self.subTest(http=http), self.rejected(http):
                result = failover.collect(candidate, self.home, "ubuntu", self.ccs)
                self.assertEqual(result["failureCode"], code)
                self.assertIsNone(result["sample"])
                self.assertNotIn(PRIVATE, json.dumps(result))

    def test_other_providers_keep_their_forbidden_account_classification(self):
        url = "https://api.kimi.com/coding/v1/usages"
        with self.rejected(403, url), self.assertRaises(common.CollectionError) as caught:
            common.HttpClient().get(url, {"Authorization": "Bearer " + PRIVATE})
        self.assertEqual(caught.exception.status, "needs_sign_in")
        self.assertNotIn(PRIVATE, caught.exception.message)


if __name__ == "__main__":
    unittest.main()
