"""Scoped, GET-only Muse portal quota reader for a normal browser session.

The native messaging bridge supplies only dev.meta.ai session cookies. Device
credentials stay on api.meta.ai. Same email, subscription tier and team
membership are checked before any quota becomes a dashboard sample.
"""
import datetime as dt
from contextlib import contextmanager
import email.utils
import hashlib
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
CACHE_NAME = "muse-console-usage.json"
REFRESH_SECONDS = 300
RATE_LIMIT_SECONDS = 600
MAX_RETRY_SECONDS = 86400
MIN_PROVIDER_AS_OF = 946684800  # 2000-01-01 UTC; older values are not Muse observations.
MAX_PROVIDER_AS_OF = 4102444800  # 2100-01-01 UTC; also bounded by the current clock below.
AS_OF_FUTURE_SKEW_SECONDS = 300
MAX_BYTES = 65536
NAMES = {"llama_dev_sess", "llm_sess"}
FIELDS = {"tier", "window_weighted_used", "window_weighted_limit", "window_duration_secs",
          "window_resets_at", "weekly_weighted_used", "weekly_weighted_limit", "weekly_resets_at", "as_of"}
ERROR_MESSAGES = {
    "no_browser_cookie": "No existing Muse web sign-in was found. Open dev.meta.ai in this browser and sync Muse again.",
    "needs_sign_in": "The existing Muse web session expired. Open dev.meta.ai in this browser and sync Muse again.",
    "account_mismatch": "The Muse web account differs from the signed-in Muse CLI account.",
    "identity_unavailable": "Muse is not returning the signed-in web account email right now. Usage will refresh automatically when it is available.",
    "plan_mismatch": "The selected web team's subscription differs from the Muse CLI subscription.",
    "choose_team": "This Muse account has several teams. Select the team that owns your coding subscription.",
    "team_mismatch": "The selected Muse team is not available to this signed-in account.",
    "inactive_subscription": "This account has no active Muse Code subscription.",
    "network_error": "The Muse web usage request did not finish. Try again later.",
    "provider_error": "Muse could not return its web usage right now.",
    "rate_limited": "Muse is limiting usage requests. Usage will refresh automatically after a cooldown.",
    "invalid_response": "Muse returned no usable rolling or weekly quota.",
    "invalid_request": "The Muse browser usage request was invalid.",
    "local_storage_error": "The private Muse web session could not be read or saved.",
    "host_unavailable": "The Mac browser usage helper could not start.",
    "busy": "A Muse usage refresh is already running.",
    "error": "Muse web usage could not be read.",
}


class MuseError(Exception):
    def __init__(self, code, teams=None, retry_after=None):
        self.code = code if code in ERROR_MESSAGES else "error"
        self.teams = teams or []
        self.team_id = None
        self.retry_after = retry_after
        super().__init__(self.code)


def retry_seconds(value, now=None):
    """Accept Retry-After seconds or HTTP date, with a bounded provider cooldown."""
    now = time.time() if now is None else now
    seconds = None
    if isinstance(value, str) and len(value) <= 128:
        if re.fullmatch(r"[0-9]{1,12}", value.strip()):
            seconds = int(value.strip())
        else:
            try:
                date = email.utils.parsedate_to_datetime(value)
                if date.tzinfo is not None:
                    seconds = date.timestamp() - now
            except (ValueError, TypeError, OverflowError):
                pass
    return min(MAX_RETRY_SECONDS, max(REFRESH_SECONDS, seconds)) if finite(seconds) else RATE_LIMIT_SECONDS


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def provider_as_of(value, now=None):
    """Optional provider observation time in Unix seconds, never a quota prerequisite."""
    if isinstance(value, bool) or not isinstance(value, (int, float, str)):
        return None
    if isinstance(value, str) and (len(value) > 64 or not re.fullmatch(r"[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?", value)):
        return None
    try:
        seconds = float(value)
        current = time.time() if now is None else float(now)
    except (ValueError, TypeError, OverflowError):
        return None
    if (not math.isfinite(seconds) or not math.isfinite(current)
            or not MIN_PROVIDER_AS_OF <= seconds <= min(MAX_PROVIDER_AS_OF, current + AS_OF_FUTURE_SKEW_SECONDS)):
        return None
    return seconds


def provider_sampled_at(quota, now=None):
    seconds = provider_as_of(quota.get("as_of"), now)
    if seconds is None:
        return None
    return dt.datetime.fromtimestamp(seconds, dt.timezone.utc).isoformat().replace("+00:00", "Z")


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
            if error.code == 429:
                raise MuseError("rate_limited", retry_after=retry_seconds((error.headers or {}).get("Retry-After"))) from None
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


def fetch_quota(cookies, expected_email, expected_plan, requested=None, client=None, now=None):
    if not valid_email(expected_email) or not valid_plan(expected_plan):
        raise MuseError("account_mismatch")
    cookies = validate_cookies(cookies)
    client = client or PortalClient(cookies)
    me = client.get("/api/auth/me")
    actual = me.get("email") if isinstance(me, dict) else None
    if not valid_email(actual):
        raise MuseError("identity_unavailable")
    if actual.strip().lower() != expected_email.strip().lower():
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
    try:
        response = client.get("/api/portal/teams/" + selected + "/subscription-quota")
    except MuseError as error:
        error.team_id = selected
        raise
    quota = response.get("subscription_quota") if isinstance(response, dict) else None
    if not isinstance(quota, dict):
        raise MuseError("invalid_response")
    if quota.get("tier") != expected_plan:
        raise MuseError("plan_mismatch")
    # Unknown response fields, cookies and billing metadata never escape.
    safe = {key: quota[key] for key in FIELDS if key != "as_of" and key in quota}
    if any(isinstance(value, (dict, list)) or isinstance(value, str) and len(value) > 160 for value in safe.values()):
        raise MuseError("invalid_response")
    # The portal labels as_of as its update time. Malformed or absent metadata
    # must not discard usable counters, and cached observations are never retimed.
    observed = provider_as_of(quota.get("as_of"), now)
    if observed is not None:
        safe["as_of"] = observed
    return selected, safe


def private_root(root):
    root = Path(root)
    if os.name != "posix" or root.is_symlink() or not root.is_dir():
        raise MuseError("local_storage_error")
    info = root.stat()
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise MuseError("local_storage_error")
    return root


def private_json(root, name):
    root = private_root(root)
    path = root / name
    if path.is_symlink():
        raise MuseError("local_storage_error")
    if not path.exists():
        return None
    info = path.stat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > MAX_BYTES):
        raise MuseError("local_storage_error")
    try:
        return json.loads(path.read_bytes())
    except (OSError, ValueError, UnicodeError):
        raise MuseError("local_storage_error") from None


def atomic_private_json(root, name, value):
    root = private_root(root)
    destination = root / name
    if destination.is_symlink():
        raise MuseError("local_storage_error")
    fd, filename = tempfile.mkstemp(prefix=".muse-usage-", dir=root)
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


@contextmanager
def usage_lock(root):
    """One quota request across independently invoked dashboard, bars and bridge."""
    import fcntl
    root = private_root(root)
    try:
        fd = os.open(root / ".muse-usage.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        info = os.fstat(fd)
        if info.st_uid != os.getuid() or not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o600:
            os.close(fd)
            raise MuseError("local_storage_error")
    except OSError:
        raise MuseError("local_storage_error") from None
    deadline = time.monotonic() + 2
    try:
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise MuseError("busy") from None
                time.sleep(0.025)
        yield
    finally:
        os.close(fd)


def fingerprint(access):
    if not isinstance(access, str) or not access.startswith("dca:") or len(access) > 32768:
        raise MuseError("account_mismatch")
    return hashlib.sha256(access.encode()).hexdigest()


def read_usage_state(root):
    value = private_json(root, CACHE_NAME)
    if value is None:
        return None
    if (not isinstance(value, dict) or set(value) != {"schemaVersion", "email", "plan", "teamId", "credentialFingerprint",
            "quota", "sampledAt", "nextRequestAt", "lastError"} or type(value["schemaVersion"]) is not int
            or value["schemaVersion"] != 1 or not valid_email(value["email"]) or not valid_plan(value["plan"])
            or not isinstance(value["teamId"], str) or not re.fullmatch(r"[0-9]{1,32}", value["teamId"])
            or not isinstance(value["credentialFingerprint"], str) or not re.fullmatch(r"[0-9a-f]{64}", value["credentialFingerprint"])
            or not finite(value["nextRequestAt"]) or value["nextRequestAt"] < 0
            or value["lastError"] is not None and value["lastError"] not in ERROR_MESSAGES):
        raise MuseError("local_storage_error")
    quota = value["quota"]
    if quota is None:
        if value["sampledAt"] is not None:
            raise MuseError("local_storage_error")
    else:
        from desktop_usage import normalize_muse
        from desktop_helpers import reset_at
        if (not isinstance(quota, dict) or set(quota) - FIELDS or quota.get("tier") != value["plan"]
                or any(isinstance(row, (dict, list)) or isinstance(row, str) and len(row) > 160 for row in quota.values())
                or not isinstance(value["sampledAt"], str) or reset_at(value["sampledAt"]) is None
                or not normalize_muse({"subscription_quota": quota})):
            raise MuseError("local_storage_error")
    return value


def matches(state, email, plan, team, access):
    return (state is not None and state["email"].lower() == email.lower() and state["plan"] == plan
            and (team is None or state["teamId"] == team) and state["credentialFingerprint"] == fingerprint(access))


def state_sample(state, cached):
    message = None
    if cached:
        message = ("Muse is limiting requests; showing the last successful usage reading. Refresh resumes automatically."
                   if state["lastError"] == "rate_limited" else
                   "Showing the last successful Muse usage reading. Usage refreshes automatically.")
    return {"teamId": state["teamId"], "quota": state["quota"], "sampledAt": state["sampledAt"],
            "cached": cached, "message": message}


def cached_quota(root, email, plan, team, access, now=None):
    """Skip key minting and all portal requests while an identity-bound cache is due."""
    now = time.time() if now is None else now
    with usage_lock(root):
        state = read_usage_state(root)
        if matches(state, email, plan, team, access) and state["nextRequestAt"] > now:
            if state["quota"] is not None:
                return state_sample(state, True)
            raise MuseError(state["lastError"] or "provider_error")
    return None


def quota_sample(root, cookies, expected_email, expected_plan, requested, access, client=None, now=None, identity_loader=None):
    """Persist successful observations and bounded retry state, never inferred quotas."""
    from desktop_usage import normalize_muse
    from desktop_helpers import utc_now
    now = time.time() if now is None else now
    if not valid_email(expected_email) or not valid_plan(expected_plan):
        raise MuseError("account_mismatch")
    validate_cookies(cookies)
    with usage_lock(root):
        state = read_usage_state(root)
        if not matches(state, expected_email, expected_plan, requested, access):
            state = None
        if state and state["nextRequestAt"] > now:
            if state["quota"] is not None:
                return state_sample(state, True)
            raise MuseError(state["lastError"] or "provider_error")
        # Reserve the next request before network I/O so a process crash cannot
        # cause a stream of retry requests from independent collector callers.
        if state is None and requested is not None:
            if not isinstance(requested, str) or not re.fullmatch(r"[0-9]{1,32}", requested):
                raise MuseError("team_mismatch")
            state = {"schemaVersion": 1, "email": expected_email, "plan": expected_plan, "teamId": requested,
                     "credentialFingerprint": fingerprint(access), "quota": None, "sampledAt": None,
                     "nextRequestAt": now, "lastError": "network_error"}
        if state:
            state["nextRequestAt"] = now + REFRESH_SECONDS
            atomic_private_json(root, CACHE_NAME, state)
        try:
            if identity_loader is not None:
                identity_loader()
            team, quota = fetch_quota(cookies, expected_email, expected_plan, requested, client, now=now)
            if not normalize_muse({"subscription_quota": quota}):
                raise MuseError("invalid_response")
            state = {"schemaVersion": 1, "email": expected_email, "plan": expected_plan, "teamId": team,
                     "credentialFingerprint": fingerprint(access), "quota": quota,
                     "sampledAt": provider_sampled_at(quota, now) or utc_now(),
                     "nextRequestAt": now + REFRESH_SECONDS, "lastError": None}
            atomic_private_json(root, CACHE_NAME, state)
            return state_sample(state, False)
        except MuseError as error:
            # Choosing a team has no quota request and no resolved cache key.
            resolved_team = requested or error.team_id
            if resolved_team is None and state is None:
                raise
            if state is None:
                state = {"schemaVersion": 1, "email": expected_email, "plan": expected_plan, "teamId": resolved_team,
                         "credentialFingerprint": fingerprint(access), "quota": None, "sampledAt": None,
                         "nextRequestAt": now, "lastError": None}
            # invalid_response may come from identity/team parsing, so it does
            # not establish a verified quota-only failure safe for retention.
            transient = error.code in {"rate_limited", "provider_error", "network_error"}
            if not transient:
                state["quota"], state["sampledAt"] = None, None
            state["lastError"] = error.code
            state["nextRequestAt"] = now + (min(MAX_RETRY_SECONDS, max(REFRESH_SECONDS, error.retry_after or RATE_LIMIT_SECONDS))
                                               if error.code == "rate_limited" else REFRESH_SECONDS)
            atomic_private_json(root, CACHE_NAME, state)
            if transient and state["quota"] is not None:
                return state_sample(state, True)
            raise
        except Exception:
            # An unexpected failure is not evidence of a temporary quota-only
            # outage. Clear any old observation before the next cold caller.
            if state is not None:
                state["quota"], state["sampledAt"] = None, None
                state["lastError"] = "error"
                state["nextRequestAt"] = now + REFRESH_SECONDS
                atomic_private_json(root, CACHE_NAME, state)
            raise MuseError("error") from None


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


def verify_device_identity(credential, expected_email, expected_plan):
    import desktop_usage as usage
    from desktop_helpers import UsageError, email, safe_text
    try:
        payload = usage.request_json(usage.MUSE_URL, {"Authorization": "Bearer " + credential.access,
                                     "x-api-version": "1.0.0"}, {})
    except UsageError as error:
        raise MuseError("needs_sign_in" if error.status == "needs_sign_in" else "network_error") from None
    if (email(payload.get("user_email")) or credential.email or "").lower() != expected_email.lower():
        raise MuseError("account_mismatch")
    if payload.get("is_subs_active") is False:
        raise MuseError("inactive_subscription")
    if safe_text(payload.get("subs_tier_name")) != expected_plan:
        raise MuseError("plan_mismatch")


def restore_browser_sample(root, previous, now=None):
    """Recover the extension's real prior observation; never a fresh sample.

    Only ordinary native messaging can supply this legacy browser cache. A
    currently verified host identity and existing capsule bind it to the same
    native account, plan and team. The provider cooldown is left intact.
    """
    import desktop_usage as usage
    from desktop_helpers import reset_at
    now = time.time() if now is None else now
    if not isinstance(previous, dict) or set(previous) != {"teamId", "sample"}:
        raise MuseError("invalid_request")
    cookies, team, bound_email, bound_plan = read_capsule(root)
    credential = usage.muse_credentials(Path.home())
    if credential is None or not credential.email or credential.email.lower() != bound_email.lower() or previous["teamId"] != team:
        raise MuseError("account_mismatch")
    value = previous["sample"]
    allowed = {"id", "provider", "providerLabel", "label", "email", "plan", "platform", "source", "status", "message",
               "fetchedAt", "sampledAt", "isActive", "windows", "capabilities"}
    if (not isinstance(value, dict) or set(value) - allowed or value.get("id") != "native:muse:mac"
            or value.get("provider") != "muse" or value.get("platform") != "mac" or value.get("status") not in {"ok", "cached"}
            or value.get("source") != "Authenticated Meta web quota on Mac" or not valid_email(value.get("email"))
            or value["email"].lower() != bound_email.lower() or value.get("plan") != bound_plan):
        raise MuseError("account_mismatch")
    sampled, fetched = value.get("sampledAt"), value.get("fetchedAt")
    if not isinstance(sampled, str) or not isinstance(fetched, str) or reset_at(sampled) is None or reset_at(fetched) is None:
        raise MuseError("invalid_response")
    observed = dt.datetime.fromisoformat(sampled.replace("Z", "+00:00")).timestamp()
    invoked = dt.datetime.fromisoformat(fetched.replace("Z", "+00:00")).timestamp()
    if observed > now + 5 or invoked > now + 5 or observed > invoked or now - observed > 604800:
        raise MuseError("invalid_response")
    windows = value.get("windows")
    if not isinstance(windows, list) or not 0 < len(windows) <= 2:
        raise MuseError("invalid_response")
    quota, keys = {"tier": bound_plan}, set()
    for row in windows:
        fields = {"key", "label", "usedPercent", "remainingPercent", "resetAt", "windowMinutes", "used", "limit", "unit", "kind"}
        if (not isinstance(row, dict) or set(row) - fields or row.get("key") not in {"window", "weekly"}
                or row["key"] in keys or row.get("unit") != "weighted tokens" or row.get("kind") != "rate_limit"
                or not finite(row.get("used")) or row["used"] < 0 or not finite(row.get("limit")) or row["limit"] <= 0
                or not finite(row.get("usedPercent")) or row["usedPercent"] < 0
                or not math.isclose(round(row["used"] / row["limit"] * 100, 4), row["usedPercent"], rel_tol=0, abs_tol=1e-10)
                or row.get("resetAt") is not None and reset_at(row["resetAt"]) is None
                or not finite(row.get("windowMinutes")) or not 0 < row["windowMinutes"] <= 525600
                or row["key"] == "weekly" and row["windowMinutes"] != 10080):
            raise MuseError("invalid_response")
        keys.add(row["key"])
        prefix = "weekly" if row["key"] == "weekly" else "window"
        quota[prefix + "_weighted_used"], quota[prefix + "_weighted_limit"] = row["used"], row["limit"]
        quota[prefix + "_resets_at"] = row.get("resetAt")
        if prefix == "window":
            quota["window_duration_secs"] = row["windowMinutes"] * 60
    if not usage.normalize_muse({"subscription_quota": quota}):
        raise MuseError("invalid_response")
    with usage_lock(root):
        state = read_usage_state(root)
        if not matches(state, bound_email, bound_plan, team, credential.access):
            return False
        # A first rate-limit observation follows live native identity validation.
        # A failed key lookup or expired web sign-in cannot bootstrap a reading.
        if state["quota"] is None and state["lastError"] != "rate_limited":
            return False
        if state["quota"] is not None:
            existing = dt.datetime.fromisoformat(state["sampledAt"].replace("Z", "+00:00")).timestamp()
            if existing >= observed:
                return False
        state["quota"], state["sampledAt"] = quota, sampled
        atomic_private_json(root, CACHE_NAME, state)
        return True


def collect_browser(cookies, requested=None, root=None):
    # The installed collector owns account identification. Browser-provided
    # email/plan claims are neither accepted nor trusted.
    import desktop_usage as usage
    from desktop_helpers import account, email, safe_text, utc_now
    credential = usage.muse_credentials(Path.home())
    if credential is None:
        raise MuseError("account_mismatch")
    root = Path(root) if root is not None else Path.home() / ".ccs/account-usage"
    sample = None
    try:
        _, bound_team, bound_email, bound_plan = read_capsule(root)
        if credential.email and credential.email.lower() == bound_email.lower():
            sample = quota_sample(root, cookies, bound_email, bound_plan, requested or bound_team, credential.access,
                                  identity_loader=lambda: verify_device_identity(credential, bound_email, bound_plan))
            if sample:
                expected_email, plan = bound_email, bound_plan
    except MuseError as error:
        if error.code not in {"no_browser_cookie", "account_mismatch", "plan_mismatch"}:
            raise
    if sample is None:
        payload = usage.request_json(usage.MUSE_URL, {"Authorization": "Bearer " + credential.access,
                                "x-api-version": "1.0.0"}, {})
        expected_email = email(payload.get("user_email")) or credential.email
        plan = safe_text(payload.get("subs_tier_name"))
        if payload.get("is_subs_active") is False:
            raise MuseError("inactive_subscription")
        sample = quota_sample(root, cookies, expected_email, plan, requested, credential.access)
    team = sample["teamId"]
    result = account("muse", "mac")
    result.update(email=expected_email, plan=plan, source="Authenticated Meta web quota on Mac",
                  status="cached" if sample["cached"] else "ok", message=sample["message"], fetchedAt=utc_now(),
                  sampledAt=sample["sampledAt"], windows=usage.normalize_muse({"subscription_quota": sample["quota"]}))
    if not result["windows"]:
        raise MuseError("invalid_response")
    return team, result
