"""Scoped, GET-only Muse portal quota reader for a normal browser session.

The native messaging bridge supplies only dev.meta.ai session cookies. Device
credentials stay on api.meta.ai. Same email, subscription tier and team
membership are checked before any quota becomes a dashboard sample.
"""
import datetime as dt
import json
import math
import os
from pathlib import Path
import re
import stat
import tempfile
import time
import urllib.error
import urllib.request

ORIGIN = "https://dev.meta.ai"
CAPSULE_NAME = "muse-console-session.json"
MAX_BYTES = 65536
NAMES = {"llama_dev_sess", "llm_sess"}
FIELDS = {"tier", "window_weighted_used", "window_weighted_limit", "window_duration_secs",
          "window_resets_at", "weekly_weighted_used", "weekly_weighted_limit", "weekly_resets_at"}
ERROR_MESSAGES = {
    "no_browser_cookie": "No existing Muse web sign-in was found. Open dev.meta.ai in this browser and sync Muse again.",
    "needs_sign_in": "The existing Muse web session expired. Open dev.meta.ai in this browser and sync Muse again.",
    "account_mismatch": "The Muse web account differs from the signed-in Muse CLI account.",
    "plan_mismatch": "The selected web team's subscription differs from the Muse CLI subscription.",
    "choose_team": "This Muse account has several teams. Select the team that owns your coding subscription.",
    "team_mismatch": "The selected Muse team is not available to this signed-in account.",
    "inactive_subscription": "This account has no active Muse Code subscription.",
    "network_error": "The Muse web usage request did not finish. Try again later.",
    "provider_error": "Muse could not return its web usage right now.",
    "invalid_response": "Muse returned no usable rolling or weekly quota.",
    "invalid_request": "The Muse browser usage request was invalid.",
    "local_storage_error": "The private Muse web session could not be read or saved.",
    "host_unavailable": "The Mac browser usage helper could not start.",
    "busy": "A Muse usage refresh is already running.",
    "error": "Muse web usage could not be read.",
}


class MuseError(Exception):
    def __init__(self, code, teams=None):
        self.code = code if code in ERROR_MESSAGES else "error"
        self.teams = teams or []
        super().__init__(self.code)


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def valid_email(value):
    return isinstance(value, str) and len(value) <= 254 and re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", value) is not None


def valid_plan(value):
    return isinstance(value, str) and 0 < len(value) <= 160 and not re.search(r"[\x00-\x1f\x7f]|dca:|bearer\s|sk-|eyJ", value, re.I)


def validate_cookies(values, now=None):
    if not isinstance(values, list) or not 0 < len(values) <= 8:
        raise MuseError("no_browser_cookie")
    now = time.time() if now is None else now
    result, seen = [], set()
    for row in values:
        if not isinstance(row, dict) or set(row) - {"name", "value", "domain", "path", "secure", "hostOnly", "expirationDate"}:
            raise MuseError("invalid_request")
        name, value = row.get("name"), row.get("value")
        if (name not in NAMES or row.get("domain") not in {"dev.meta.ai", ".dev.meta.ai"}
                or row.get("path") != "/" or row.get("secure") is not True
                or type(row.get("hostOnly")) is not bool or not isinstance(value, str)
                or not 0 < len(value) <= 8192 or re.search(r"[;\x00-\x20\x7f]", value)
                or value.startswith(("dca:", "sk-"))):
            raise MuseError("invalid_request")
        if row["hostOnly"] and row["domain"] != "dev.meta.ai":
            raise MuseError("invalid_request")
        expiry = row.get("expirationDate")
        if expiry is not None and not finite(expiry):
            raise MuseError("invalid_request")
        if expiry is not None and expiry <= now:
            continue
        if name in seen:
            raise MuseError("invalid_request")
        seen.add(name)
        result.append({key: row[key] for key in row if key in {"name", "value", "domain", "path", "secure", "hostOnly", "expirationDate"}})
    if not result:
        raise MuseError("no_browser_cookie")
    return result


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        # A session cookie must never follow an authentication or other redirect.
        raise MuseError("needs_sign_in")


class PortalClient:
    def __init__(self, cookies):
        self.header = "; ".join(row["name"] + "=" + row["value"] for row in validate_cookies(cookies))
        self.deadline = time.monotonic() + 16
        self.opener = urllib.request.build_opener(NoRedirect())

    def get(self, route):
        if route not in {"/api/auth/me", "/api/portal/teams"} and not re.fullmatch(r"/api/portal/teams/[0-9]{1,32}/subscription-quota", route):
            raise MuseError("invalid_request")
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise MuseError("network_error")
        request = urllib.request.Request(ORIGIN + route, headers={"Cookie": self.header,
            "Accept": "application/json", "User-Agent": "CCS-Muse-Usage", "Referer": ORIGIN + "/"})
        try:
            with self.opener.open(request, timeout=min(8, remaining)) as response:
                if response.status != 200:
                    raise MuseError("provider_error")
                raw = response.read(MAX_BYTES + 1)
                if len(raw) > MAX_BYTES or time.monotonic() > self.deadline:
                    raise MuseError("invalid_response")
                payload = json.loads(raw)
                if not isinstance(payload, dict):
                    raise MuseError("invalid_response")
                return payload
        except urllib.error.HTTPError as error:
            raise MuseError("needs_sign_in" if error.code in (401, 403) else "provider_error") from None
        except MuseError:
            raise
        except (OSError, ValueError, UnicodeError):
            raise MuseError("network_error") from None


def team_rows(payload):
    rows = payload.get("teams") if isinstance(payload, dict) else None
    if not isinstance(rows, list) or not 0 < len(rows) <= 100:
        raise MuseError("invalid_response")
    teams, seen = [], set()
    for row in rows:
        raw = row.get("team_id") if isinstance(row, dict) else None
        value = str(raw) if isinstance(raw, int) and not isinstance(raw, bool) else raw
        if not isinstance(value, str) or not re.fullmatch(r"[0-9]{1,32}", value) or value in seen:
            raise MuseError("invalid_response")
        seen.add(value)
        name = row.get("team_name")
        name = name.strip() if isinstance(name, str) else ""
        if not name or len(name) > 160 or re.search(r"[\x00-\x1f\x7f]|dca:|bearer\s|sk-|eyJ", name, re.I):
            name = "Team " + value
        teams.append({"id": value, "name": name})
    return teams


def fetch_quota(cookies, expected_email, expected_plan, requested=None, client=None):
    if not valid_email(expected_email) or not valid_plan(expected_plan):
        raise MuseError("account_mismatch")
    cookies = validate_cookies(cookies)
    client = client or PortalClient(cookies)
    me = client.get("/api/auth/me")
    actual = me.get("email") if isinstance(me, dict) else None
    if not valid_email(actual) or actual.strip().lower() != expected_email.strip().lower():
        raise MuseError("account_mismatch")
    teams = team_rows(client.get("/api/portal/teams"))
    if requested is None:
        if len(teams) != 1:
            raise MuseError("choose_team", teams)
        selected = teams[0]["id"]
    else:
        if not isinstance(requested, str) or requested not in {row["id"] for row in teams}:
            raise MuseError("team_mismatch")
        selected = requested
    response = client.get("/api/portal/teams/" + selected + "/subscription-quota")
    quota = response.get("subscription_quota") if isinstance(response, dict) else None
    if not isinstance(quota, dict):
        raise MuseError("invalid_response")
    if quota.get("tier") != expected_plan:
        raise MuseError("plan_mismatch")
    # Unknown response fields, cookies and billing metadata never escape.
    safe = {key: quota[key] for key in FIELDS if key in quota}
    if any(isinstance(value, (dict, list)) or isinstance(value, str) and len(value) > 160 for value in safe.values()):
        raise MuseError("invalid_response")
    return selected, safe


def private_root(root):
    root = Path(root)
    if os.name != "posix" or root.is_symlink() or not root.is_dir():
        raise MuseError("local_storage_error")
    info = root.stat()
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise MuseError("local_storage_error")
    return root


def write_capsule(root, cookies, team, email, plan):
    root = private_root(root)
    cookies = validate_cookies(cookies)
    if not isinstance(team, str) or not re.fullmatch(r"[0-9]{1,32}", team) or not valid_email(email) or not valid_plan(plan):
        raise MuseError("invalid_request")
    destination = root / CAPSULE_NAME
    if destination.is_symlink():
        raise MuseError("local_storage_error")
    value = {"schemaVersion": 1, "provider": "muse", "origin": ORIGIN, "email": email,
             "plan": plan, "teamId": team, "cookies": cookies}
    fd, filename = tempfile.mkstemp(prefix=".muse-session-", dir=root)
    try:
        with os.fdopen(fd, "w") as output:
            os.fchmod(output.fileno(), 0o600)
            json.dump(value, output, allow_nan=False, separators=(",", ":"))
            output.flush()
            os.fsync(output.fileno())
        os.replace(filename, destination)
    finally:
        if os.path.exists(filename):
            os.unlink(filename)


def read_capsule(root):
    if not (Path(root) / CAPSULE_NAME).exists():
        raise MuseError("no_browser_cookie")
    root = private_root(root)
    path = root / CAPSULE_NAME
    if not path.exists():
        raise MuseError("no_browser_cookie")
    if path.is_symlink():
        raise MuseError("local_storage_error")
    info = path.stat()
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > MAX_BYTES:
        raise MuseError("local_storage_error")
    try:
        value = json.loads(path.read_bytes())
    except (OSError, ValueError, UnicodeError):
        raise MuseError("local_storage_error") from None
    if (not isinstance(value, dict) or set(value) != {"schemaVersion", "provider", "origin", "email", "plan", "teamId", "cookies"}
            or type(value["schemaVersion"]) is not int or value["schemaVersion"] != 1 or value["provider"] != "muse" or value["origin"] != ORIGIN
            or not valid_email(value["email"]) or not valid_plan(value["plan"])
            or not isinstance(value["teamId"], str) or not re.fullmatch(r"[0-9]{1,32}", value["teamId"])):
        raise MuseError("local_storage_error")
    return validate_cookies(value["cookies"]), value["teamId"], value["email"], value["plan"]


def collect_browser(cookies, requested=None):
    # The installed collector owns account identification. Browser-provided
    # email/plan claims are neither accepted nor trusted.
    import desktop_usage as usage
    from desktop_helpers import account, email, safe_text, utc_now
    credential = usage.muse_credentials(Path.home())
    if credential is None:
        raise MuseError("account_mismatch")
    payload = usage.request_json(usage.MUSE_URL, {"Authorization": "Bearer " + credential.access,
                                "x-api-version": "1.0.0"}, {})
    expected_email = email(payload.get("user_email")) or credential.email
    plan = safe_text(payload.get("subs_tier_name"))
    if payload.get("is_subs_active") is False:
        raise MuseError("inactive_subscription")
    team, quota = fetch_quota(cookies, expected_email, plan, requested)
    result = account("muse", "mac")
    result.update(email=expected_email, plan=plan, source="Authenticated Meta web quota on Mac",
                  status="ok", message=None, fetchedAt=utc_now(), sampledAt=utc_now(),
                  windows=usage.normalize_muse({"subscription_quota": quota}))
    if not result["windows"]:
        raise MuseError("invalid_response")
    return team, result
