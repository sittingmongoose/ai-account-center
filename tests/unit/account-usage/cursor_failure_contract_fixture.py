"""Real Cursor collector/dispatcher output; credential files and HTTP are synthetic."""

import base64
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import urllib.error
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
import cursor_usage as cursor
import desktop_helpers as desktop
import failover_usage as dispatcher

EMAIL = "cursor-fixture@example.test"
INITIAL_AT = "2026-10-14T12:00:00Z"
OBSERVED_AT = "2026-10-14T12:01:00Z"
PRIVATE = "synthetic-private-provider-detail"


def token(subject="auth0|cursor-fixture", signature="initial"):
    claims = base64.urlsafe_b64encode(json.dumps({"sub": subject, "email": EMAIL}).encode()).decode().rstrip("=")
    return "fixture." + claims + "." + signature


def scenario(status=415, change=None):
    """Exercise real inventory, HTTP error conversion, adapter, and final boundary."""
    with tempfile.TemporaryDirectory(prefix="cursor-dispatcher-contract-") as folder:
        home = Path(folder)
        auth_path = home / ".config/Cursor/auth.json"
        auth_path.parent.mkdir(parents=True)
        auth_path.write_text(json.dumps({"accessToken": token()}), encoding="utf-8")
        phase = ["initial"]
        requests = []

        def response(value):
            return io.BytesIO(json.dumps(value).encode("utf-8"))

        def provider(request, **_kwargs):
            url = request.full_url
            requests.append(url)
            if url == cursor.MEMBERSHIP_URL:
                return response({"membershipType": "ultra" if phase[0] == "initial" else "premium", "private": PRIVATE})
            if url == cursor.CURRENT_USAGE_URL:
                if change == "revision":
                    auth_path.write_text(json.dumps({"accessToken": token(signature="rotated")}), encoding="utf-8")
                elif change == "identity":
                    auth_path.write_text(json.dumps({"accessToken": token(subject="auth0|different-fixture")}), encoding="utf-8")
                if change != "account_mismatch":
                    headers = {"Retry-After": "1800"} if status == 429 else {}
                    raise urllib.error.HTTPError(url, status, PRIVATE, headers, io.BytesIO(PRIVATE.encode()))
                return response({"planUsage": {"totalPercentUsed": 17}})
            if url == cursor.PLAN_INFO_URL:
                return response({"isMerged": True, "planInfo": {"includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY"}})
            if url == cursor.SUMMARY_URL:
                return response({"individualUsage": {"plan": {"totalPercentUsed": 50, "apiPercentUsed": 25}}})
            if url == cursor.PROFILE_URL:
                return response({"email": "different@example.test" if phase[0] != "initial" and change == "account_mismatch" else EMAIL})
            raise AssertionError("Unexpected provider URL in offline Cursor fixture")

        with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(home / ".config")}), \
                patch.object(dispatcher.sys, "platform", "linux"), \
                patch.object(cursor, "utc_now", side_effect=lambda: INITIAL_AT if phase[0] == "initial" else OBSERVED_AT), \
                patch.object(desktop, "utc_now", side_effect=lambda: INITIAL_AT if phase[0] == "initial" else OBSERVED_AT), \
                patch("urllib.request.OpenerDirector.open", side_effect=provider):
            base = {"version": 1, "provider": "cursor", "homeDir": str(home), "ccsDir": str(home / "ccs")}
            inventory = dispatcher.run({**base, "action": "inventory"})
            if len(inventory["candidates"]) != 1:
                raise AssertionError("Synthetic Cursor credential was not inventoried")
            candidate = inventory["candidates"][0]
            request = {**base, "action": "collect", "candidate": candidate}
            initial = dispatcher.run(request)
            phase[0] = "failed"
            requests.clear()
            failed = dispatcher.run(request)
        return {"status": status, "change": change, "candidate": candidate, "initial": initial,
                "failed": failed, "providerRequests": len(requests), "externalRequests": 0}


if __name__ == "__main__":
    print(json.dumps({"cases": [scenario(415), scenario(429)],
                      "revisionChanged": scenario(429, "revision"),
                      "identityChanged": scenario(429, "identity"),
                      "accountMismatch": scenario(415, "account_mismatch")}, allow_nan=False, separators=(",", ":")))
