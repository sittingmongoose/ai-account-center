"""Real Muse helper/cache output for the Node interop test; all I/O is synthetic."""
import json
from pathlib import Path
import sys
import tempfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
import desktop_helpers as helpers
import desktop_usage as usage
import muse_console as muse

EMAIL, PLAN = "fixture@example.com", "Muse Code High Usage"
COOKIE = {"name": "llama_dev_sess", "value": "synthetic-session", "domain": "dev.meta.ai",
          "path": "/", "secure": True, "hostOnly": True}
QUOTA = {"tier": PLAN, "window_weighted_used": "0", "window_weighted_limit": "1000",
         "window_duration_secs": 18000, "window_resets_at": None,
         "weekly_weighted_used": "63", "weekly_weighted_limit": "1000",
         "weekly_resets_at": "2026-10-05T00:00:00Z"}
CODES = ["needs_sign_in", "inactive_subscription", "account_mismatch", "plan_mismatch",
         "team_mismatch", "choose_team", "invalid_response", "invalid_request", "error",
         "rate_limited", "provider_error", "network_error", "unexpected_failure"]
TRANSIENT = {"rate_limited", "provider_error", "network_error"}


def scenario(code, retain_native=False):
    with tempfile.TemporaryDirectory(prefix="muse-contract-") as folder:
        home = Path(folder)
        cache_root = home / ".ccs/account-usage"
        cache_root.mkdir(parents=True, mode=0o700)
        clock = [1000]
        stamp = ["2026-10-01T12:00:00Z"]
        failure = [None]
        quota = [dict(QUOTA)]

        def portal(*_args, **_kwargs):
            if failure[0] == "unexpected_failure":
                raise RuntimeError("synthetic private upstream details")
            if failure[0] is not None:
                raise muse.MuseError(failure[0], retry_after=1800)
            return "42", quota[0]

        def offline(*_args, **_kwargs):
            raise AssertionError("Unexpected network request in Muse interop fixture")

        with patch("desktop_usage.muse_credentials", return_value=helpers.Credential(
                "dca:synthetic-fixture", email=EMAIL)), \
             patch("muse_console.read_capsule", return_value=([COOKIE], "42", EMAIL, PLAN)), \
             patch("muse_console.verify_device_identity", return_value=None), \
             patch("muse_console.fetch_quota", side_effect=portal), \
             patch("muse_console.time.time", side_effect=lambda: clock[0]), \
             patch("desktop_helpers.utc_now", side_effect=lambda: stamp[0]), \
             patch("desktop_usage.utc_now", side_effect=lambda: stamp[0]), \
             patch("desktop_usage.request_json", side_effect=offline) as key_request, \
             patch("urllib.request.OpenerDirector.open", side_effect=offline) as web_request:
            initial = usage.collect("muse", "mac", home)
            # Exercise Node-only retention when a new helper has no host sample.
            # Hard failures exercise the real native invalidation path instead.
            if code in TRANSIENT and not retain_native:
                (cache_root / muse.CACHE_NAME).unlink()
            failure[0], clock[0], stamp[0] = code, 1400, "2026-10-01T12:10:00Z"
            failed = usage.collect("muse", "mac", home)
            state = muse.read_usage_state(cache_root)
            native_cleared = state["quota"] is None and state["sampledAt"] is None
            cooldown = state["nextRequestAt"]
            # A later transient outage must not resurrect a rejected old sample.
            failure[0], clock[0], stamp[0] = "network_error", 3300, "2026-10-01T12:20:00Z"
            subsequent = usage.collect("muse", "mac", home)
            # A successful partial reading with real zero/null values is valid.
            failure[0], clock[0], stamp[0] = None, 4000, "2026-10-01T12:30:00Z"
            quota[0] = {"tier": PLAN, "weekly_weighted_used": "0", "weekly_weighted_limit": "1000",
                        "weekly_resets_at": None}
            recovered = usage.collect("muse", "mac", home)
            key_request.assert_not_called()
            web_request.assert_not_called()
        return {"code": code, "initial": initial, "failed": failed, "subsequent": subsequent,
                "recovered": recovered, "nativeCleared": native_cleared,
                "nextRequestAt": cooldown, "externalRequests": 0}


if __name__ == "__main__":
    result = {"cases": [scenario(code) for code in CODES],
              "nativeRateLimited": scenario("rate_limited", retain_native=True)}
    print(json.dumps(result, allow_nan=False, separators=(",", ":")))
