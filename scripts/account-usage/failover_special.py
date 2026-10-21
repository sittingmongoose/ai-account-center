"""Own-host Antigravity and OpenCode workspace sources for usage failover.

Inventory is read-only and makes no provider requests. Its identity hints select
a candidate; collection must prove that identity again with the provider. Fields
prefixed with ``_`` are private process data, never part of the wire inventory.
No credential leaves its host, and no live application login is replaced.

Packaged dependencies: desktop_usage.py, desktop_helpers.py, and
browser-bridge/opencode-muse/native-host/opencode_console.py (as a sibling).
"""
from __future__ import annotations

import datetime as dt
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
from email.utils import parsedate_to_datetime
import urllib.error
import urllib.parse
import urllib.request

import desktop_helpers as helpers
import desktop_usage as desktop

HASH = re.compile(r"[a-f0-9]{64}\Z")
PROFILE = re.compile(r"[a-z][a-z0-9_-]{0,47}\Z")
REGISTRY = re.compile(r"registry-([0-9]{12})\.json\Z")
PLATFORMS = {"ubuntu": "Ubuntu", "mac": "Mac", "windows": "Windows"}
MAX_BYTES = 1024 * 1024
IDENTITY_KIND = "antigravity-subject"
WALLET_KIND = "opencode-workspace"
USERINFO_URL = "https://www.googleapis.com/oauth2/v2/userinfo"
TOKEN_URL = "https://oauth2.googleapis.com/token"


class RateLimited(helpers.UsageError):
    def __init__(self, retry_after):
        super().__init__("rate_limited", "Antigravity requested a pause before another usage request.")
        self.retry_after_seconds = retry_after


def _retry_after(value, now=None):
    if not isinstance(value, str) or len(value) > 128:
        return 60
    value = value.strip()
    if re.fullmatch(r"[0-9]{1,12}", value):
        return max(1, min(86400, int(value)))
    try:
        instant = parsedate_to_datetime(value)
        if instant.tzinfo is None:
            return 60
        seconds = math.ceil((instant - (now or dt.datetime.now(dt.timezone.utc))).total_seconds())
        return max(1, min(86400, seconds))
    except (ValueError, TypeError, OverflowError):
        return 60


class HttpClient:
    """The legacy bounded Google transport with explicit account-wide 429 backoff.

    Kept local to failover so existing collectors retain their public behavior.
    A throttle on identity, refresh, or quota must stop attempts on other hosts.
    """
    @staticmethod
    def request_json(url, headers=None, body=None, form=False):
        if url not in {USERINFO_URL, TOKEN_URL, desktop.AGY_BASE + "loadCodeAssist",
                       desktop.AGY_BASE + "retrieveUserQuotaSummary"}:
            raise helpers.UsageError("error", "The account request is unavailable.")
        headers = dict(headers or {})
        headers["Accept"] = "application/json"
        headers.setdefault("User-Agent", "CCS-Account-Usage/1.0")
        data = None
        if body is not None:
            data = (urllib.parse.urlencode(body).encode() if form else _json(body).encode())
            headers["Content-Type"] = "application/x-www-form-urlencoded" if form else "application/json"
        request = urllib.request.Request(url, data=data, headers=headers)
        try:
            with urllib.request.build_opener(helpers.NoRedirect()).open(request, timeout=helpers.HTTP_TIMEOUT) as response:
                raw = response.read(helpers.MAX_RESPONSE_BYTES + 1)
            if len(raw) > helpers.MAX_RESPONSE_BYTES:
                raise helpers.UsageError("error", "The account response exceeded its size limit.")
            return _decode(raw)
        except urllib.error.HTTPError as error:
            try:
                if error.code == 429:
                    raise RateLimited(_retry_after(error.headers.get("Retry-After") if error.headers else None)) from None
                if error.code in (401, 403) or error.code == 400 and helpers.is_invalid_grant(error):
                    raise helpers.UsageError("needs_sign_in", "The saved Antigravity account needs a renewed sign-in.") from None
                raise helpers.UsageError("error", "The account usage service is temporarily unavailable.") from None
            finally:
                error.close()
        except (urllib.error.URLError, TimeoutError, OSError, ValueError, UnicodeError):
            raise helpers.UsageError("error", "The account usage request could not be completed.") from None


def _digest(value):
    return hashlib.sha256(value if isinstance(value, bytes) else value.encode("utf-8")).hexdigest()


def _json(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False, allow_nan=False)


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError()
        result[key] = value
    return result


def _decode(raw):
    def invalid(_):
        raise ValueError()
    value = json.loads(raw, object_pairs_hook=_unique, parse_constant=invalid)
    if not isinstance(value, dict):
        raise ValueError()
    return value


def _linked(info):
    return stat.S_ISLNK(info.st_mode) or bool(getattr(info, "st_file_attributes", 0) & 0x400)


def _path(root, relative):
    """Fixed own-host paths only; never follow links, junctions, or a foreign owner."""
    root = Path(root).absolute()
    relative = Path(relative)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError()
    # Check ancestors too: a symlinked scope must not redirect private reads.
    for current in [*reversed(root.parents), root]:
        info = current.lstat()
        if _linked(info) or not stat.S_ISDIR(info.st_mode):
            raise ValueError()
    current = root
    for index, part in enumerate(relative.parts):
        current = current / part
        info = current.lstat()
        if _linked(info) or (index < len(relative.parts) - 1 and not stat.S_ISDIR(info.st_mode)):
            raise ValueError()
        if os.name == "posix" and info.st_uid != os.getuid():
            raise ValueError()
    return current


def _read(root, relative, maximum=MAX_BYTES):
    file = _path(root, relative)
    before = file.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
        raise ValueError()
    if os.name == "posix" and before.st_mode & 0o077:
        raise ValueError()
    descriptor = os.open(file, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    with os.fdopen(descriptor, "rb") as handle:
        opened = os.fstat(handle.fileno())
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise ValueError()
        raw = handle.read(maximum + 1)
    after = file.lstat()
    if len(raw) > maximum or _linked(after) or (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size) != (
            opened.st_dev, opened.st_ino, opened.st_mtime_ns, opened.st_size):
        raise ValueError()
    return raw


def _identity(email, subject):
    email = helpers.email(email)
    if not email or not isinstance(subject, str) or not re.fullmatch(r"[0-9]{1,128}", subject):
        return None
    email = email.lower()
    return email, _digest(_json(["antigravity", email, subject]))


def _hint(value):
    claims = helpers.jwt_claims(value.get("id_token") or value.get("idToken") or "")
    return _identity(claims.get("email") or value.get("email"),
                     claims.get("sub") or value.get("subject") or value.get("sub"))


def _native(value):
    token = value.get("token")
    if value.get("auth_method") != "consumer" or not isinstance(token, dict):
        return None
    access = helpers.secret(token.get("access_token"))
    if not access:
        return None
    expires = helpers.reset_at(token.get("expiry"))
    expires = dt.datetime.fromisoformat(expires.replace("Z", "+00:00")).timestamp() * 1000 if expires else None
    hint = _hint(value)
    return helpers.Credential(access, "Antigravity CLI", helpers.secret(token.get("refresh_token")) or "",
                              expires, hint[0] if hint else None, google_client="consumer")


def _candidate(location, selectors, identity, revision, credential, plan=None):
    email, key = identity
    return {"locator": _digest("antigravity:" + location), "selectors": selectors,
            "identity": {"kind": IDENTITY_KIND, "key": key}, "revision": revision,
            "email": email, "plan": helpers.safe_text(plan, 80),
            "_provider": "antigravity", "_credential": credential}


def _saved(ccs_dir):
    try:
        directory = _path(ccs_dir, "antigravity-profiles")
        names = sorted(entry.name for entry in directory.iterdir() if REGISTRY.fullmatch(entry.name))
        if not names:
            return []
        value = _decode(_read(ccs_dir, Path("antigravity-profiles") / names[-1]))
        profiles = value.get("profiles")
        if (type(value.get("version")) is not int or value["version"] != 1
                or type(value.get("revision")) is not int or value["revision"] != int(REGISTRY.fullmatch(names[-1])[1])
                or not isinstance(profiles, list) or len(profiles) > 16):
            return []
        result, seen = [], set()
        for profile in profiles:
            if not isinstance(profile, dict):
                return []
            name, saved = profile.get("id"), profile.get("credential")
            if not isinstance(name, str) or not PROFILE.fullmatch(name) or name in seen or not isinstance(saved, dict):
                return []
            seen.add(name)
            verified = saved.get("identity")
            if not isinstance(verified, dict) or verified.get("source") not in ("provider-userinfo", "native-runtime"):
                continue
            identity = _identity(verified.get("email"), verified.get("subject"))
            revision = saved.get("fingerprint")
            if (not identity or identity[1] != profile.get("identityKey")
                    or helpers.email(profile.get("email", "")) is None
                    or profile["email"].lower() != identity[0]
                    or not isinstance(verified.get("verifiedAt"), str) or not helpers.reset_at(verified["verifiedAt"])
                    or not isinstance(revision, str) or not HASH.fullmatch(revision)):
                continue
            try:
                raw = _read(ccs_dir, Path("antigravity-instances") / name / "ubuntu" / ("credential-" + revision + ".bin"), 16384)
                credential = _native(_decode(raw))
                if credential and _digest(raw) == revision:
                    credential.email = identity[0]
                    result.append(_candidate("saved:" + name, ["antigravity-profile:" + name],
                                             identity, revision, credential, profile.get("plan")))
            except (FileNotFoundError, NotADirectoryError):
                continue
            except (OSError, ValueError, TypeError):
                helpers.read_failed()
                continue
        return result
    except (FileNotFoundError, NotADirectoryError):
        return []
    except (OSError, ValueError, TypeError):
        helpers.read_failed()
        return []


def _antigravity(home, ccs_dir):
    result = _saved(ccs_dir)
    try:
        raw = _read(home, ".gemini/antigravity-cli/antigravity-oauth-token", 16384)
        value = _decode(raw)
        credential, identity = _native(value), _hint(value)
        if credential and identity:
            result.append(_candidate("native", ["discover"], identity, _digest(raw), credential))
    except (FileNotFoundError, NotADirectoryError):
        pass
    except (OSError, ValueError, TypeError):
        helpers.read_failed()
    try:
        database = _path(home, ".omp/agent/agent.db")
        if not stat.S_ISREG(database.lstat().st_mode):
            return result
        rows = helpers.sqlite_rows(database,
            "SELECT rowid,data FROM auth_credentials WHERE provider=? AND credential_type='oauth' "
            "AND disabled_cause IS NULL ORDER BY rowid DESC LIMIT 16", ("google-antigravity",))
        for row_id, raw in rows:
            if not isinstance(row_id, int) or not isinstance(raw, str) or len(raw.encode()) > 65536:
                continue
            try:
                value = _decode(raw)
                identity = _hint(value)
                credential = desktop.credential_from_omp("antigravity", value)
                if identity and credential:
                    result.append(_candidate("omp:" + str(row_id), ["discover"], identity, _digest(raw), credential))
            except (ValueError, TypeError):
                continue
    except (FileNotFoundError, NotADirectoryError):
        pass
    except (OSError, ValueError, TypeError):
        helpers.read_failed()
    return result


def _wallet_supported(platform):
    return os.name == "posix" and platform != "windows"


def _wallet(ccs_dir, platform):
    # The existing capsule reader uses POSIX ownership checks. A Windows browser
    # login without that supported private capsule is not an invented source.
    if not _wallet_supported(platform):
        return []
    try:
        import opencode_console as console
        relative = Path("account-usage") / console.CAPSULE_NAME
        raw = _read(ccs_dir, relative)
        # Confirm that the directory exists first; read_capsule must never create
        # an absent directory while performing inventory.
        root = _path(ccs_dir, "account-usage")
        cookies, workspace = console.read_capsule(root)
        if _read(ccs_dir, relative) != raw:
            return []
        return [{"locator": _digest("opencode-wallet:browser-capsule"), "selectors": ["wallet"],
                 "identity": {"kind": WALLET_KIND, "key": _digest(workspace)}, "revision": _digest(raw),
                 "email": None, "plan": None, "_provider": "opencode-wallet",
                 "_cookies": cookies, "_workspace": workspace}]
    except Exception:
        # Missing bridge modules/capsules or unsupported storage are unavailable.
        return []


def inventory(provider, home: Path, platform: str, ccs_dir=None):
    """Bounded local hints; callers publish only the six public candidate fields."""
    if platform not in PLATFORMS:
        return []
    home = Path(home)
    ccs_dir = Path(ccs_dir) if ccs_dir is not None else home / ".ccs"
    if provider == "antigravity":
        return _antigravity(home, ccs_dir)
    if provider == "opencode-wallet":
        return _wallet(ccs_dir, platform)
    return []


def _verified(credential):
    user = HttpClient.request_json(USERINFO_URL, {"Authorization": "Bearer " + credential.access})
    if user.get("verified_email", user.get("email_verified")) is not True:
        return None
    return _identity(user.get("email"), user.get("id") or user.get("sub"))


def _refresh(credential, home):
    if not credential.refresh:
        raise helpers.UsageError("needs_sign_in", "The saved Antigravity account needs a renewed sign-in.")
    client_id, client_secret = desktop.antigravity_oauth_client(home)
    response = HttpClient.request_json(TOKEN_URL, body={
        "grant_type": "refresh_token", "refresh_token": credential.refresh,
        "client_id": client_id, "client_secret": client_secret,
    }, form=True)
    access = helpers.secret(response.get("access_token"))
    if not access:
        raise helpers.UsageError("needs_sign_in", "The saved Antigravity session could not be renewed.")
    return helpers.Credential(access, credential.source, credential.refresh, email=credential.email,
                              project=credential.project, plan=credential.plan, google_client=credential.google_client)


def _quota(credential, sample):
    headers = {"Authorization": "Bearer " + credential.access, "User-Agent": "antigravity/1.0.0"}
    load = HttpClient.request_json(desktop.AGY_BASE + "loadCodeAssist", headers, {
        "metadata": {"ideType": "ANTIGRAVITY", "platform": "PLATFORM_UNSPECIFIED", "pluginType": "GEMINI"}})
    project = load.get("cloudaicompanionProject")
    if isinstance(project, dict):
        project = project.get("id") or project.get("projectId")
    project = helpers.safe_text(project) or credential.project
    if project is None:
        raise helpers.UsageError("unavailable", "The signed-in account did not return an Antigravity quota project.")
    sample["plan"], reported = desktop.antigravity_reported_plan(load)
    if reported:
        sample["reportedPlan"] = reported
    summary = HttpClient.request_json(desktop.AGY_BASE + "retrieveUserQuotaSummary", headers, {"project": project})
    sample["windows"] = desktop.normalize_antigravity(summary) + desktop.normalize_antigravity_credits(load)


def _collect_antigravity(candidate, home, platform):
    credential = candidate["_credential"]
    now = dt.datetime.now(dt.timezone.utc).timestamp() * 1000
    refreshed = False
    if credential.expires is not None and credential.expires <= now + 30000:
        credential = _refresh(credential, home)
        refreshed = True
    try:
        identity = _verified(credential)
    except helpers.UsageError as error:
        if error.status != "needs_sign_in" or refreshed or not credential.refresh:
            raise
        credential = _refresh(credential, home)
        identity = _verified(credential)
    if (not identity or identity[0] != candidate["email"]
            or candidate["identity"] != {"kind": IDENTITY_KIND, "key": identity[1]}):
        return {"sample": None, "failureCode": "identity_mismatch"}
    sample = helpers.account("antigravity", platform)
    sample.update(email=identity[0], label=identity[0], plan=candidate["plan"], status="ok", message=None,
                  source="Verified Antigravity account on " + PLATFORMS[platform])
    # Do not call fetch_antigravity: its retry can change tokens after identity
    # verification. This exact verified access token must perform the quota read.
    _quota(credential, sample)
    if not sample["windows"]:
        return {"sample": None, "failureCode": "unavailable"}
    sample["fetchedAt"] = sample["sampledAt"] = helpers.utc_now()
    return {"sample": sample, "failureCode": None}


def _collect_wallet(candidate, platform):
    import opencode_console as console
    workspace, sample = console.collect(candidate["_cookies"], candidate["_workspace"])
    if (workspace != candidate["_workspace"]
            or candidate["identity"] != {"kind": WALLET_KIND, "key": _digest(workspace)}
            or sample.get("id") != "plan-opencode-go-console-mac-" + _digest(workspace)[:12]):
        return {"sample": None, "failureCode": "identity_mismatch"}
    failure = sample.get("_usageFailure")
    sample = console.public_sample(sample)
    sample["platform"] = platform
    sample["source"] = "Authenticated OpenCode console workspace on " + PLATFORMS[platform]
    result = {"sample": sample, "failureCode": None}
    if isinstance(failure, dict) and failure.get("code") == "rate_limited":
        result["failureCode"] = "rate_limited"
        retry = failure.get("retryAfterSeconds")
        if isinstance(retry, (int, float)) and not isinstance(retry, bool) and math.isfinite(retry):
            result["retryAfterSeconds"] = min(86400, max(0, retry))
    return result


def collect(candidate, home: Path, platform: str, ccs_dir=None):
    """Collect one freshly inventoried candidate, then reject local sign-in races."""
    try:
        provider = candidate["_provider"]
        if platform not in PLATFORMS or provider not in ("antigravity", "opencode-wallet"):
            return {"sample": None, "failureCode": "unavailable"}
        result = (_collect_antigravity(candidate, Path(home), platform) if provider == "antigravity"
                  else _collect_wallet(candidate, platform))
        if result["sample"] is not None:
            try:
                unchanged = any(
                    all(current.get(key) == candidate.get(key) for key in ("locator", "identity", "revision"))
                    for current in inventory(provider, home, platform, ccs_dir))
            except Exception:
                unchanged = False
            if not unchanged:
                failed = {"sample": None, "failureCode": "source_changed"}
                # A provider throttled the original request even when a later
                # credential read invalidates its quota publication.
                if result.get("failureCode") == "rate_limited":
                    failed["rateLimited"] = True
                    retry = result.get("retryAfterSeconds")
                    if isinstance(retry, (int, float)) and not isinstance(retry, bool) and math.isfinite(retry):
                        failed["retryAfterSeconds"] = min(86400, max(0, retry))
                return failed
        return result
    except RateLimited as error:
        return {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": error.retry_after_seconds}
    except helpers.UsageError as error:
        return {"sample": None, "failureCode": "needs_sign_in" if error.status == "needs_sign_in" else "provider_error"}
    except Exception as error:
        code = getattr(error, "code", None)
        if code == "rate_limited":
            retry = getattr(error, "retry_after_seconds", None)
            result = {"sample": None, "failureCode": "rate_limited"}
            if isinstance(retry, (int, float)) and not isinstance(retry, bool) and math.isfinite(retry):
                result["retryAfterSeconds"] = min(86400, max(0, retry))
            return result
        failure = ("needs_sign_in" if code in ("needs_sign_in", "no_browser_cookie") else
                   "identity_mismatch" if code == "workspace_mismatch" else "provider_error")
        return {"sample": None, "failureCode": failure}
