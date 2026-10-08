"""Private, read-only helpers shared by the desktop usage collectors.

Only normalized account DTOs leave the process. Credential files are read in
place; SQLite connections are explicitly read-only and never copy the database.
"""

import base64
import datetime as dt
import json
import math
import os
import pathlib
import re
import sqlite3
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field

MAX_RESPONSE_BYTES = 65536
HTTP_TIMEOUT = 8
PROVIDERS = {"antigravity": "Antigravity CLI", "muse": "Muse Code", "cursor": "Cursor"}


class UsageError(Exception):
    """An error with a fixed, safe description, never an upstream response."""

    def __init__(self, status, message):
        super().__init__(message)
        self.status = status
        self.message = message


@dataclass
class Credential:
    access: str = field(repr=False)
    source: str = "Native account"
    refresh: str = field(default="", repr=False)
    expires: float = None
    email: str = None
    project: str = None
    plan: str = None
    google_client: str = None


def utc_now():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if not math.isfinite(value):
        return None
    return float(value)


def nonnegative(value):
    value = number(value)
    return value if value is not None and value >= 0 else None


def counter(value):
    """Known protobuf counters may encode a numeric amount as a string."""
    if isinstance(value, str):
        if len(value) > 64 or not re.fullmatch(r"\d+(?:\.\d+)?(?:[eE][+-]?\d+)?", value):
            return None
        try:
            value = float(value)
        except ValueError:
            return None
    return nonnegative(value)


def percent(value):
    value = number(value)
    # A provider may report usage over its allowance. Preserve that reading;
    # only the derived remaining amount is clamped at zero.
    return round(value, 4) if value is not None and value >= 0 else None


def safe_text(value, maximum=160):
    if not isinstance(value, str) or not value.strip() or len(value) > maximum:
        return None
    value = value.strip()
    if re.search(r"[\x00-\x1f\x7f]", value):
        return None
    # Display metadata must never accidentally become a credential channel.
    if re.search(r"(?i)(bearer\s|dca:|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8})", value):
        return None
    return value


def email(value):
    value = safe_text(value, 254)
    return value if value and re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", value) else None


def secret(value):
    if not isinstance(value, str) or not value.strip() or len(value) > 16384:
        return None
    return value.strip() if not re.search(r"[\x00-\x20\x7f]", value.strip()) else None


def reset_at(value):
    """Normalize supplied timestamps only; never derive a billing reset."""
    try:
        if number(value) is not None:
            seconds = float(value)
            if seconds > 100000000000:
                seconds /= 1000
            stamp = dt.datetime.fromtimestamp(seconds, dt.timezone.utc)
        elif isinstance(value, str):
            stamp = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
            if stamp.tzinfo is None:
                return None
            stamp = stamp.astimezone(dt.timezone.utc)
        else:
            return None
        if not 2000 <= stamp.year <= 2100:
            return None
        return stamp.isoformat(timespec="seconds").replace("+00:00", "Z")
    except (ValueError, OverflowError, OSError):
        return None


def quota_window(key, label, used_percent=None, remaining_percent=None,
                 reset=None, minutes=None, used=None, limit=None, unit=None,
                 kind="rate_limit", remaining=None, expires=None, unlimited=None, enabled=None):
    used_percent, remaining_percent = percent(used_percent), percent(remaining_percent)
    if used_percent is None and remaining_percent is not None:
        used_percent = max(0, round(100 - remaining_percent, 4))
    if remaining_percent is None and used_percent is not None:
        remaining_percent = max(0, round(100 - used_percent, 4))
    minutes = number(minutes)
    window = {
        "key": key, "label": label,
        "usedPercent": used_percent, "remainingPercent": remaining_percent,
        "resetAt": reset_at(reset),
        "windowMinutes": minutes if minutes is not None and 0 < minutes <= 525600 else None,
        "used": nonnegative(used), "limit": nonnegative(limit), "unit": unit,
        "kind": kind,
    }
    if remaining is not None:
        window["remaining"] = nonnegative(remaining)
    if expires is not None:
        window["expiresAt"] = reset_at(expires)
    if isinstance(unlimited, bool):
        window["unlimited"] = unlimited
    if isinstance(enabled, bool):
        window["enabled"] = enabled
    return window


def account(provider, platform):
    label = PROVIDERS[provider]
    return {
        "id": "native:" + provider + ":" + platform,
        "provider": provider, "providerLabel": label, "label": label,
        "email": None, "plan": None, "platform": platform,
        "source": "Native account on " + {"mac": "Mac", "windows": "Windows", "ubuntu": "Ubuntu"}[platform],
        "status": "unavailable", "message": "No readable signed-in account was found on this computer.",
        "fetchedAt": None, "sampledAt": None, "isActive": False, "windows": [],
        "capabilities": {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []},
    }


def read_json(path):
    try:
        with pathlib.Path(path).open("rb") as handle:
            raw = handle.read(MAX_RESPONSE_BYTES + 1)
        if len(raw) > MAX_RESPONSE_BYTES:
            return None
        payload = json.loads(raw)
        return payload if isinstance(payload, dict) else None
    except (OSError, ValueError, UnicodeError):
        return None


def sqlite_rows(path, query, params=()):
    connection = None
    try:
        path = pathlib.Path(path)
        if not path.is_file():
            return []
        connection = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=2)
        connection.execute("PRAGMA query_only=ON")
        deadline = time.monotonic() + 3
        connection.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
        return connection.execute(query, params).fetchmany(16)
    except (OSError, sqlite3.Error):
        return []
    finally:
        if connection is not None:
            connection.close()


def omp_credentials(provider, home):
    rows = sqlite_rows(pathlib.Path(home) / ".omp/agent/agent.db",
                       "SELECT data FROM auth_credentials WHERE provider=? AND credential_type='oauth' "
                       "AND disabled_cause IS NULL ORDER BY rowid DESC LIMIT 8", (provider,))
    values = []
    for row in rows:
        try:
            if isinstance(row[0], str) and len(row[0]) <= MAX_RESPONSE_BYTES:
                value = json.loads(row[0])
                if isinstance(value, dict):
                    values.append(value)
        except (ValueError, TypeError):
            pass
    return values


def jwt_claims(token):
    try:
        parts = token.split(".")
        if len(parts) != 3 or len(parts[1]) > MAX_RESPONSE_BYTES:
            return {}
        raw = base64.urlsafe_b64decode(parts[1] + "=" * (-len(parts[1]) % 4))
        claims = json.loads(raw)
        return claims if isinstance(claims, dict) else {}
    except (ValueError, UnicodeError, TypeError):
        return {}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise UsageError("error", "The usage service returned an unexpected redirect.")


def is_invalid_grant(error):
    """Recognize an OAuth invalid_grant body; a read or parse failure is not one."""
    try:
        value = json.loads(error.read(4096))
    except Exception:
        return False
    return isinstance(value, dict) and value.get("error") == "invalid_grant"


def request_json(url, headers=None, body=None, form=False):
    headers = dict(headers or {})
    headers["Accept"] = "application/json"
    headers.setdefault("User-Agent", "CCS-Account-Usage/1.0")
    data = None
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode("utf-8")
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body, allow_nan=False).encode("utf-8")
            headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers)
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=HTTP_TIMEOUT) as response:
            raw = response.read(MAX_RESPONSE_BYTES + 1)
        if len(raw) > MAX_RESPONSE_BYTES:
            raise UsageError("error", "The usage service response exceeded its size limit.")
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise UsageError("error", "The usage service returned an unsupported response.")
        return value
    except urllib.error.HTTPError as error:
        if error.code in (401, 403):
            raise UsageError("needs_sign_in", "The saved account could not authenticate with the usage service.") from None
        if error.code == 400 and is_invalid_grant(error):
            raise UsageError("needs_sign_in", "The saved account's sign-in has expired or was revoked.") from None
        raise UsageError("error", "The usage service is temporarily unavailable.") from None
    except (urllib.error.URLError, TimeoutError, OSError):
        raise UsageError("error", "The usage service could not be reached within its time limit.") from None
    except (ValueError, UnicodeError):
        raise UsageError("error", "The usage service returned an unsupported response.") from None


def config_home(home):
    value = os.environ.get("XDG_CONFIG_HOME")
    return pathlib.Path(value) if value and pathlib.Path(value).is_absolute() else pathlib.Path(home) / ".config"
