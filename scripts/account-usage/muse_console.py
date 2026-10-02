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
# Sidecar pin of the web user bound to the capsule (a hash of /api/auth/me
# userId, never the id itself). Separate file: capsule and cache schemas stay.
BINDING_NAME = "muse-console-binding.json"
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


def valid_team(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9]{1,32}", value) is not None


def valid_hash(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def user_hash(me):
    """SHA-256 of the portal's stable account id (auth/me userId), or None when unusable."""
    value = me.get("userId") if isinstance(me, dict) else None
    if isinstance(value, int) and not isinstance(value, bool):
        value = str(value)
    if not isinstance(value, str) or not 0 < len(value) <= 128 or re.search(r"[\x00-\x20\x7f]", value):
        return None
    return hashlib.sha256(("muse-user:" + value).encode()).hexdigest()


def fetch_quota(cookies, expected_email, expected_plan, requested=None, client=None, now=None, bound_team=None,
                bound_user=None, capsule_session=False, observed=None):
    """Read one team's subscription quota after binding the web session to the CLI account.

    The portal email must equal the CLI email ("email" binding). A valid
    different email, masked ones included, is always account_mismatch.

    When /api/auth/me answers without a usable email (blank, missing or
    malformed), the session may read only ``bound_team``, the private capsule's
    team, which an earlier email-verified reading bound to this same email and
    plan, and only when auth/me still names a web user (userId):
    - "user": ``bound_user`` is the pinned hash of that user and must match;
      another user is account_mismatch.
    - "session": nothing is pinned yet (``bound_user`` None). Only the capsule's
      own stored session qualifies (``capsule_session``), and it must list
      exactly one team. Its user becomes the pin (see quota_sample).
    Anything else, including an unreadable pin (any other ``bound_user``), is
    identity_unavailable. The web session must still list that exact team and
    its tier must equal the live CLI plan. A binding never reads another team.

    ``observed``, when a dict, receives the binding mode and the user hash.
    """
    if not valid_email(expected_email) or not valid_plan(expected_plan):
        raise MuseError("account_mismatch")
    if bound_team is not None and requested not in (None, bound_team):
        # A capsule binding reads only its own team. A team switch is the
        # email-verified path's job, so callers pass no binding for it.
        raise MuseError("identity_unavailable")
    cookies = validate_cookies(cookies)
    client = client or PortalClient(cookies)
    me = client.get("/api/auth/me")
    actual = me.get("email") if isinstance(me, dict) else None
    user = user_hash(me)
    if valid_email(actual):
        if actual.strip().lower() != expected_email.strip().lower():
            raise MuseError("account_mismatch")
        mode = "email"
    elif not valid_team(bound_team) or user is None:
        raise MuseError("identity_unavailable")
    elif bound_user is None and capsule_session is True:
        requested, mode = bound_team, "session"
    elif valid_hash(bound_user):
        if user != bound_user:
            raise MuseError("account_mismatch")
        requested, mode = bound_team, "user"
    else:
        raise MuseError("identity_unavailable")
    teams = team_rows(client.get("/api/portal/teams"))
    if mode == "session" and len(teams) != 1:
        raise MuseError("identity_unavailable")
    if requested is None:
        if len(teams) != 1:
            raise MuseError("choose_team", teams)
        selected = teams[0]["id"]
    else:
        if not isinstance(requested, str) or requested not in {row["id"] for row in teams}:
            # Without an email, losing the verified team leaves the session unbound.
            raise MuseError("team_mismatch" if mode == "email" else "identity_unavailable")
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
    as_of = provider_as_of(quota.get("as_of"), now)
    if as_of is not None:
        safe["as_of"] = as_of
    if isinstance(observed, dict):
        observed.update(mode=mode, userHash=user)
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


def read_binding(root):
    value = private_json(root, BINDING_NAME)
    if value is None:
        return None
    if (not isinstance(value, dict) or set(value) != {"schemaVersion", "email", "plan", "teamId", "userHash"}
            or type(value["schemaVersion"]) is not int or value["schemaVersion"] != 1
            or not valid_email(value["email"]) or not valid_plan(value["plan"])
            or not valid_team(value["teamId"]) or not valid_hash(value["userHash"])):
        raise MuseError("local_storage_error")
    return value


def pinned_user(root, email, plan, team):
    """The pinned web user hash for this capsule binding.

    None means nothing is pinned yet. False means a pin exists but is
    unreadable or belongs to another email, plan or team, so the email-less
    path stays closed until an email-verified reading pins this binding.
    """
    try:
        value = read_binding(root)
    except MuseError:
        return False
    if value is None:
        return None
    if value["email"].lower() != email.lower() or value["plan"] != plan or value["teamId"] != team:
        return False
    return value["userHash"]


def session_values(cookies):
    return sorted((row["name"], row["value"]) for row in cookies)


def capsule_session(root, cookies, email, plan, team):
    """True when ``cookies`` are exactly the session stored in the capsule for this binding."""
    try:
        stored, stored_team, stored_email, stored_plan = read_capsule(root)
        presented = validate_cookies(cookies)
    except MuseError:
        return False
    return (stored_team == team and stored_email.lower() == email.lower() and stored_plan == plan
            and session_values(stored) == session_values(presented))


def pin_user(root, email, plan, team, observed):
    """Pin the web user after an email-verified reading or the capsule session's first one."""
    if observed.get("mode") not in {"email", "session"} or not valid_hash(observed.get("userHash")):
        return
    try:
        atomic_private_json(root, BINDING_NAME, {"schemaVersion": 1, "email": email, "plan": plan, "teamId": team,
                                                 "userHash": observed["userHash"]})
    except (MuseError, OSError):
        # Best effort: the reading stands. The earlier pin, or none, stays, so
        # the email-less path still admits only that user or the capsule session.
        pass


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


def quota_sample(root, cookies, expected_email, expected_plan, requested, access, client=None, now=None, identity_loader=None,
                 bound_team=None):
    """Persist successful observations and bounded retry state, never inferred quotas.

    ``bound_team`` is passed only by callers holding the private capsule binding
    for this same email and plan; see fetch_quota. The pinned web user and
    whether ``cookies`` are the capsule's own session are read here, from
    ``root``, never taken from the caller. A fresh sample carries ``binding``:
    the mode fetch_quota verified ("email", "user" or "session").
    """
    from desktop_usage import normalize_muse
    from desktop_helpers import utc_now
    now = time.time() if now is None else now
    if not valid_email(expected_email) or not valid_plan(expected_plan):
        raise MuseError("account_mismatch")
    if bound_team is not None and requested not in (None, bound_team):
        raise MuseError("identity_unavailable")
    validate_cookies(cookies)
    with usage_lock(root):
        state = read_usage_state(root)
        if (requested is not None and state is not None and state["teamId"] != requested
                and matches(state, expected_email, expected_plan, None, access)
                and state["lastError"] == "rate_limited" and state["nextRequestAt"] > now):
            # A provider 429 limits this web session, not one team. Choosing
            # another team waits too and keeps the first team's reading.
            raise MuseError("rate_limited", retry_after=state["nextRequestAt"] - now)
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
            binding = {"bound_team": bound_team}
            if bound_team is not None:
                binding.update(bound_user=pinned_user(root, expected_email, expected_plan, bound_team),
                               capsule_session=capsule_session(root, cookies, expected_email, expected_plan, bound_team))
            observed = {}
            team, quota = fetch_quota(cookies, expected_email, expected_plan, requested, client, now=now,
                                      observed=observed, **binding)
            if not normalize_muse({"subscription_quota": quota}):
                raise MuseError("invalid_response")
            state = {"schemaVersion": 1, "email": expected_email, "plan": expected_plan, "teamId": team,
                     "credentialFingerprint": fingerprint(access), "quota": quota,
                     "sampledAt": provider_sampled_at(quota, now) or utc_now(),
                     "nextRequestAt": now + REFRESH_SECONDS, "lastError": None}
            atomic_private_json(root, CACHE_NAME, state)
            pin_user(root, expected_email, expected_plan, team, observed)
            return dict(state_sample(state, False), binding=observed.get("mode"))
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
    # Invariant: the team, email and plan come from a CLI-bound sample, and the
    # cookies are a session that a fresh reading verified by email or by the
    # pinned web user (or the capsule's own session), or a pin guards them; see
    # browser_reading. fetch_quota trusts this binding and this session when the
    # portal omits the email, so never write a capsule from unverified claims.
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


def browser_reading(cookies, requested=None, root=None):
    """Collect one browser-synced reading: (team, dashboard sample, capsule_ok).

    capsule_ok says whether these cookies may replace the capsule's session: a
    fresh reading verified them (email, pinned user, or the capsule session
    itself), or the reading is cached and a pinned user guards the next fresh
    one. A first team-bound reading through any other session never does.
    """
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
            # The capsule binding reads only its own team. Choosing another
            # team in the popup needs an email-verified reading.
            sample = quota_sample(root, cookies, bound_email, bound_plan, requested or bound_team, credential.access,
                                  identity_loader=lambda: verify_device_identity(credential, bound_email, bound_plan),
                                  bound_team=bound_team if requested in (None, bound_team) else None)
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
    if sample["cached"]:
        capsule_ok = valid_hash(pinned_user(root, expected_email, plan, team))
    else:
        capsule_ok = sample.get("binding") in {"email", "user", "session"}
    return team, result, capsule_ok


def collect_browser(cookies, requested=None, root=None):
    team, result, _ = browser_reading(cookies, requested, root)
    return team, result


def sync_browser(cookies, requested=None, root=None):
    """Native-host museSync: collect, then keep the capsule only on a verified session."""
    root = root if root is not None else Path.home() / ".ccs/account-usage"
    team, result, capsule_ok = browser_reading(cookies, requested, root)
    if capsule_ok:
        write_capsule(root, cookies, team, result["email"], result["plan"])
    return team, result
