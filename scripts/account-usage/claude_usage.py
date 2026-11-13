"""Identity-bound Claude Desktop quota and reset timestamps.

Windows decrypts its existing OAuth cache as the same signed-in OS user. Tokens
stay in memory. The public collector never writes. The private reset_host seam
can send an explicitly confirmed exact-grant claim. Neither path refreshes or
writes auth, follows redirects, or emits raw provider bodies or exceptions.
Optional web extras read the Desktop profile's own live web session cookie in
memory (same user, read-only, identity re-verified) and fall back to the legacy
same-user DPAPI session capsule, verified again against this native account
and organization before any balance request. Credential values are never
persisted or emitted.
"""

import argparse
import base64
import ctypes
import datetime as dt
from email.utils import parsedate_to_datetime
import hashlib
import json
import math
import ntpath
import os
from pathlib import Path
import re
import sqlite3
import stat
import sys
import time
import urllib.error
import urllib.request


# Same safe-ID rule as the server (CLAUDE_PROFILE_ID_PATTERN). Which IDs exist
# comes from the caller's manifest entry, passed per invocation below.
SAFE_PROFILE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")
EXPECTED_EMAIL = re.compile(r"^[^\s@]+@[^\s@]+\.[^\s@]+$")
EXPECTED_EMAIL_ENV = "CCS_CLAUDE_PROFILE_EMAIL"
WINDOWS = {
    "five_hour": ("Five-hour usage", 300),
    "seven_day": ("Weekly usage", 10080),
    "seven_day_opus": ("Weekly Opus usage", 10080),
    "seven_day_sonnet": ("Weekly Sonnet usage", 10080),
    "seven_day_oauth_apps": ("Weekly OAuth app usage", 10080),
    "seven_day_cowork": ("Weekly Cowork usage", 10080),
}
UUID = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
MAX_BYTES = 1024 * 1024
MAX_CAPSULE_BYTES = 65536
CHROMIUM_EPOCH = 11644473600000000
MAX_RESET_GRANTS = 20
MAX_SCOPED_LIMITS = 64
# Same gateway the desktop-live normalizeSample enforces: any window whose
# completed key or label exceeds these bounds invalidates the whole sample
# downstream. Never truncate (truncation can collide); omit only that window.
WINDOWS_KEY_PATTERN = re.compile(r"^[a-z0-9][a-z0-9_]{0,63}$")
MAX_WINDOW_KEY_LEN = 64
MAX_WINDOW_LABEL_LEN = 80
UTC = dt.timezone.utc


class UsageUnavailable(Exception):
    def __init__(self, status="unavailable", retry_after=None):
        self.status = status
        self.retry_after = (min(86400, retry_after) if isinstance(retry_after, (int, float)) and
                            not isinstance(retry_after, bool) and math.isfinite(retry_after) and retry_after >= 0 else None)


def retry_after_seconds(value):
    """Only a bounded provider delay leaves the HTTP boundary, never its body."""
    if not isinstance(value, str) or len(value) > 128:
        return None
    try:
        if re.fullmatch(r"\d+", value.strip()):
            seconds = int(value.strip())
        else:
            timestamp = parsedate_to_datetime(value)
            if timestamp.tzinfo is None:
                return None
            seconds = math.ceil(timestamp.timestamp() - time.time())
        return min(86400, max(0, seconds))
    except (ValueError, TypeError, OverflowError):
        return None


def now_iso():
    return dt.datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value if math.isfinite(value) and value >= 0 else None


def reset_at(value):
    if not isinstance(value, str) or len(value) > 64:
        return None
    try:
        instant = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if instant.tzinfo is None or not 2000 <= instant.year <= 2200:
            return None
        return instant.astimezone(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except (ValueError, TypeError, OverflowError):
        return None


# Present files that could not be read in this process; failover inventories report them.
READ_FAILURES = 0


def read_json(path):
    global READ_FAILURES
    try:
        with path.open("rb") as handle:
            contents = handle.read(MAX_BYTES + 1)
        if len(contents) <= MAX_BYTES:
            result = json.loads(contents.decode("utf-8-sig"))
            if isinstance(result, dict):
                return result
    except (FileNotFoundError, NotADirectoryError):
        raise UsageUnavailable()
    except (OSError, ValueError, UnicodeError):
        pass
    READ_FAILURES += 1
    raise UsageUnavailable()


def decode_base64(value):
    if not isinstance(value, str) or len(value) > MAX_BYTES:
        raise UsageUnavailable()
    try:
        return base64.b64decode(value, validate=True)
    except (ValueError, TypeError):
        raise UsageUnavailable()


def valid_profile_id(profile_id):
    return isinstance(profile_id, str) and SAFE_PROFILE_ID.fullmatch(profile_id) is not None


def valid_expected_email(value):
    return (isinstance(value, str) and len(value) <= 254
            and EXPECTED_EMAIL.fullmatch(value) is not None)


def resolve_expected_email(expected_email=None):
    """The caller binds one invocation to one manifest email.

    The server passes `--expected-email` from the manifest entry; the env var
    is the manual-use fallback. Nothing here maps IDs to emails anymore.
    """
    candidate = expected_email if expected_email is not None else os.environ.get(EXPECTED_EMAIL_ENV)
    return candidate if valid_expected_email(candidate) else None


def resolve_windows_profile_root(profile_path, home=None):
    """Follow the Store default's relocation, never another named profile.

    The new directory is authoritative as soon as it exists, including while
    signed out or partially written. Do not revive a superseded login from the
    old directory when the current one cannot supply usage.
    """
    home = Path.home() if home is None else Path(home)
    legacy = home / "AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude"
    current = home / "AppData/Local/Claude-Data/claude_pzs8sxrjxfjjc"
    if ntpath.normcase(ntpath.normpath(str(profile_path))) != ntpath.normcase(ntpath.normpath(str(legacy))):
        return profile_path
    try:
        current.lstat()
    except (FileNotFoundError, NotADirectoryError):
        return profile_path
    except OSError:
        # An inaccessible current profile is not proof it was never migrated.
        return current
    return current


def windows_profile_path(profile_id, home=None, is_default=False):
    """Caller-selected profile directory; the default Store profile is flagged,
    never guessed from the ID."""
    home = Path.home() if home is None else Path(home)
    if is_default:
        legacy = home / "AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude"
        return resolve_windows_profile_root(legacy, home)
    return home / "AppData" / "Roaming" / ("Claude-" + profile_id)


def _canonical_profile_directory(value):
    """Resolve native aliases only after rejecting shares and linked directories."""
    if str(value).startswith(("\\\\", "//")):
        raise UsageUnavailable()
    # Windows path-only callers on POSIX cannot inspect a Windows filesystem.
    if not value.is_absolute():
        return value
    root = Path(os.path.abspath(value))
    try:
        for directory in [*reversed(root.parents), root]:
            info = directory.lstat()
            if (not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or
                    getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024)):
                raise UsageUnavailable()
        return root.resolve(strict=True)
    except (FileNotFoundError, NotADirectoryError):
        return value
    except (OSError, ValueError):
        raise UsageUnavailable()


def resolve_profile_path(profile_id, profile_dir, home=None, is_default=False):
    """Prefer the caller-supplied manifest directory; otherwise derive the
    generic named-profile directory (or the Store default when flagged)."""
    if profile_dir is not None:
        if (not isinstance(profile_dir, str) or not profile_dir or len(profile_dir) > 4096
                or "\x00" in profile_dir or not ntpath.isabs(profile_dir)):
            raise UsageUnavailable()
        root = _canonical_profile_directory(Path(profile_dir))
        resolved = resolve_windows_profile_root(root, home)
        return _canonical_profile_directory(resolved) if resolved != root else root
    if not valid_profile_id(profile_id):
        raise UsageUnavailable()
    return _canonical_profile_directory(windows_profile_path(profile_id, home, is_default))


def unprotect(blob, expected_length=32):
    """DPAPI with CRYPTPROTECT_UI_FORBIDDEN; no interactive auth prompts."""
    import ctypes.wintypes as wt

    class Blob(ctypes.Structure):
        _fields_ = [("cbData", wt.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]

    array = (ctypes.c_ubyte * len(blob)).from_buffer_copy(blob)
    source, destination = Blob(len(blob), array), Blob()
    crypt = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    crypt.CryptUnprotectData.argtypes = [
        ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.POINTER(Blob),
        ctypes.c_void_p, ctypes.c_void_p, wt.DWORD, ctypes.POINTER(Blob),
    ]
    crypt.CryptUnprotectData.restype = wt.BOOL
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.LocalFree.restype = ctypes.c_void_p
    if not crypt.CryptUnprotectData(
        ctypes.byref(source), None, None, None, None, 1, ctypes.byref(destination)
    ):
        raise UsageUnavailable()
    try:
        if (expected_length is not None and destination.cbData != expected_length) or not 0 < destination.cbData <= MAX_CAPSULE_BYTES:
            raise UsageUnavailable()
        return ctypes.string_at(destination.pbData, destination.cbData)
    finally:
        kernel.LocalFree(destination.pbData)


def profile_key(profile_root):
    """Unwrap the Local State os_crypt key as the same signed-in OS user."""
    state = read_json(profile_root / "Local State")
    crypto_state = state.get("os_crypt")
    if not isinstance(crypto_state, dict):
        raise UsageUnavailable()
    wrapped = decode_base64(crypto_state.get("encrypted_key"))
    if not wrapped.startswith(b"DPAPI") or len(wrapped) <= 5:
        raise UsageUnavailable()
    return unprotect(wrapped[5:])


def decrypt_cache(profile_path):
    # cryptography is already installed on the user's Windows machine. No
    # packages are downloaded by the collector.
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    config = read_json(profile_path / "config.json")
    account_uuid = config.get("lastKnownAccountUuid")
    if not isinstance(account_uuid, str) or not re.fullmatch(UUID, account_uuid):
        raise UsageUnavailable("needs_sign_in")
    key = profile_key(profile_path)
    blob = decode_base64(config.get("oauth:tokenCacheV2"))
    if not blob.startswith(b"v10") or len(blob) < 31:
        raise UsageUnavailable()
    try:
        decoded = AESGCM(key).decrypt(blob[3:15], blob[15:], None)
        if len(decoded) > MAX_BYTES:
            raise UsageUnavailable()
        cache = json.loads(decoded)
    except Exception:
        raise UsageUnavailable()
    if not isinstance(cache, dict) or len(cache) > 128:
        raise UsageUnavailable()
    return account_uuid, cache


def eligible_entries(account_uuid, cache, allow_expired=False):
    candidates = []
    now = time.time() * 1000
    for cache_key, entry in cache.items():
        if not isinstance(cache_key, str) or len(cache_key) > 4096 or not isinstance(entry, dict):
            continue
        # Native V2 grammar: acct:<account>|<client>:<organization>:<resource>:<scopes>.
        # A URL/scope substring elsewhere in the key is not enough to trust it.
        match = re.fullmatch(
            r"acct:(" + UUID + r")\|(" + UUID + r"):(" + UUID +
            r"):https://api\.anthropic\.com:([A-Za-z0-9_: -]+)", cache_key
        )
        if match is None:
            continue
        identities = list(match.group(1, 2, 3))
        scopes = match.group(4).split(" ")
        token, expires = entry.get("token"), number(entry.get("expiresAt"))
        if (
            identities[0] != account_uuid or "user:profile" not in scopes
            or not all(re.fullmatch(r"[A-Za-z0-9_:-]+", scope) for scope in scopes)
            or not isinstance(token, str)
            or not token or len(token) > 32768 or any(ord(char) < 32 for char in token)
            or expires is None or not allow_expired and expires <= now + 30000
        ):
            continue
        candidates.append((expires, identities, token))
    return sorted(candidates, key=lambda item: item[0], reverse=True)[:4]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self):
        self.deadline = time.monotonic() + 10
        self.opener = urllib.request.build_opener(NoRedirect())

    def get(self, route, token):
        if route not in ("/api/oauth/profile", "/api/oauth/usage", "/api/oauth/usage?cedar_ember=1"):
            raise UsageUnavailable()
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise UsageUnavailable("error")
        request = urllib.request.Request(
            "https://api.anthropic.com" + route,
            headers={
                "Authorization": "Bearer " + token,
                "anthropic-beta": "oauth-2025-04-20",
                "User-Agent": "CCS-ReadOnly-Usage/1.0",
                "Accept": "application/json",
            },
        )
        try:
            with self.opener.open(request, timeout=min(4, remaining)) as response:
                parts, size = [], 0
                while size <= MAX_BYTES:
                    if time.monotonic() >= self.deadline:
                        raise UsageUnavailable("error")
                    part = response.read1(min(65536, MAX_BYTES + 1 - size))
                    if not part:
                        break
                    parts.append(part)
                    size += len(part)
                if size > MAX_BYTES:
                    raise UsageUnavailable("error")
                value = json.loads(b"".join(parts))
                if not isinstance(value, dict):
                    raise UsageUnavailable("error")
                return value
        except urllib.error.HTTPError as error:
            if error.code == 429:
                raise UsageUnavailable("rate_limited", retry_after_seconds(error.headers.get("Retry-After")))
            raise UsageUnavailable("needs_sign_in" if error.code in (401, 403) else "error")
        except (OSError, ValueError, UnicodeError, urllib.error.URLError):
            raise UsageUnavailable("error")


def _slug(value):
    """Lowercase machine slug: non-alphanumeric runs become one underscore,
    edges are stripped, length is capped at 40, and an empty result is unsafe."""
    if not isinstance(value, str):
        return None
    slug = re.sub(r"[^a-z0-9]+", "_", value.lower()).strip("_")
    slug = slug[:40].rstrip("_")
    return slug or None


def _words(slug):
    return " ".join(part.capitalize() for part in slug.split("_") if part)


def _group_minutes(group):
    if group == "weekly":
        return 10080
    if group == "session":
        return 300
    return None


def _scoped_model(scope):
    """A limit's model is its display name only; an id codename never names a window."""
    model = scope.get("model") if isinstance(scope, dict) else None
    display = _plan_name(model.get("display_name")) if isinstance(model, dict) else None
    if display is None:
        return None, None
    slug = _slug(display)
    if slug is None:
        return None, None
    return slug, display


def _scoped_surface(scope):
    """A limit's surface names the key suffix and the parenthesized label words."""
    surface = scope.get("surface") if isinstance(scope, dict) else None
    candidates = []
    if isinstance(surface, dict):
        candidates.append(surface.get("id"))
        candidates.append(surface.get("display_name"))
    elif isinstance(surface, str):
        candidates.append(surface)
    for raw in candidates:
        name = _plan_name(raw)
        if name is None:
            continue
        slug = _slug(name)
        if slug is None:
            continue
        return slug, _words(slug)
    return None, None


def _window_bounds_ok(key, label):
    """Gateway normalizeSample enforces: safe key and short label."""
    return (isinstance(key, str) and len(key) <= MAX_WINDOW_KEY_LEN
            and WINDOWS_KEY_PATTERN.fullmatch(key) is not None
            and isinstance(label, str) and len(label) <= MAX_WINDOW_LABEL_LEN)


def _is_weekly_scoped_allowed(kind, group):
    """A model/surface-scoped entry may only take a seven_day_* weekly
    identity when its group is weekly and it is weekly_scoped or an unknown
    kind with group weekly. Anything else is skipped, never given a weekly
    key it did not name."""
    if group != "weekly":
        return False
    if kind == "weekly_scoped":
        return True
    if kind in ("session", "weekly_all"):
        return False
    return isinstance(kind, str) and re.fullmatch(r"[a-z0-9_]+", kind) is not None


def _limits_window(entry, used):
    """One self-describing limits[] entry as a window, or None when unsafe.

    The provider names every quota itself: known kinds map directly, scoped
    entries take a screened model (and optional surface) name, and an unknown
    kind with a screened model scope is treated as scoped. An unknown kind
    without a scope keeps its own lowercase kind as the key; the label is the
    kind's words minus a trailing all/scoped, title-cased, plus " usage".
    Minutes for unknown kinds come from the group: weekly 10080, session 300,
    anything else None.
    """
    kind, group = entry.get("kind"), entry.get("group")

    def window(key, label, minutes):
        return {
            "key": key, "label": label, "kind": "rate_limit",
            "usedPercent": used, "remainingPercent": max(0, 100 - used),
            "resetAt": reset_at(entry.get("resets_at")), "expiresAt": None,
            "windowMinutes": minutes, "used": None, "limit": None, "unit": None,
        }

    if kind == "session":
        return window("five_hour", "Five-hour usage", 300)
    if kind == "weekly_all":
        return window("seven_day", "Weekly usage", 10080)
    scope = entry.get("scope") if isinstance(entry.get("scope"), dict) else None
    model_slug, display = _scoped_model(scope)
    surface_slug, surface_words = _scoped_surface(scope)
    # is_active selects the provider's current limiting bucket. False is still
    # a genuine model quota, including an unused zero-percent window.
    minutes = _group_minutes(group) if kind != "weekly_scoped" else 10080 if group == "weekly" else None
    if model_slug is not None:
        if not _is_weekly_scoped_allowed(kind, group):
            return None
        key = "seven_day_" + model_slug
        label = "Weekly " + display + " usage"
        if surface_slug is not None:
            key += "_" + surface_slug
            label += " (" + surface_words + ")"
        if not _window_bounds_ok(key, label):
            return None
        return window(key, label, minutes)
    if surface_slug is not None:
        if not _is_weekly_scoped_allowed(kind, group):
            return None
        key = "seven_day_" + surface_slug
        label = "Weekly " + surface_words + " usage"
        if not _window_bounds_ok(key, label):
            return None
        return window(key, label, minutes)
    if kind == "weekly_scoped":
        return None
    if not isinstance(kind, str) or re.fullmatch(r"[a-z0-9_]+", kind) is None:
        return None
    parts = kind.split("_")
    while parts and parts[-1] in ("all", "scoped"):
        parts.pop()
    if not parts:
        return None
    label = " ".join(part.capitalize() for part in parts) + " usage"
    if not _window_bounds_ok(kind, label):
        return None
    return window(kind, label, minutes)


def normalize_limits(usage):
    """Every limits[] quota the provider names, deduped by key.

    Identical duplicates collapse; conflicting duplicates for one key drop
    that key entirely rather than silently picking one account quota.
    """
    limits = usage.get("limits")
    if not isinstance(limits, list) or len(limits) > MAX_SCOPED_LIMITS:
        return []
    windows = {}
    for entry in limits:
        if not isinstance(entry, dict):
            continue
        used = number(entry.get("percent"))
        if used is None:
            continue
        candidate = _limits_window(entry, used)
        if candidate is None:
            continue
        key = candidate["key"]
        if key in windows:
            if windows[key] != candidate:
                windows[key] = None
            continue
        windows[key] = candidate
    return [window for window in windows.values() if window is not None]


def _legacy_window(key, value):
    label, minutes = WINDOWS[key]
    used = number(value.get("utilization"))
    if used is None:
        return None
    return {
        "key": key, "label": label, "kind": "rate_limit",
        "usedPercent": used, "remainingPercent": max(0, 100 - used),
        "resetAt": reset_at(value.get("resets_at")),
        "expiresAt": reset_at(value.get("expires_at")),
        "windowMinutes": minutes, "used": None, "limit": None, "unit": None,
    }


def normalize_windows(usage):
    windows, seen = [], set()
    # Legacy top-level windows win over limits[] for the same key: a valid
    # top-level value is authoritative even when limits names the quota too.
    for key in WINDOWS:
        value = usage.get(key)
        if not isinstance(value, dict):
            continue
        window = _legacy_window(key, value)
        if window is None:
            continue
        windows.append(window)
        seen.add(key)
    # Unknown seven_day_<slug> top-level windows fill in for future models;
    # bare codename keys (nimbus_quill and the like) are never windows.
    for key in sorted(k for k in usage if isinstance(k, str)):
        if key in seen or key in WINDOWS or re.fullmatch(r"seven_day_[a-z0-9_]+", key) is None:
            continue
        value = usage.get(key)
        if not isinstance(value, dict):
            continue
        used = number(value.get("utilization"))
        if used is None:
            continue
        label = "Weekly " + _words(key[len("seven_day_"):]) + " usage"
        if not _window_bounds_ok(key, label):
            continue
        windows.append({
            "key": key, "label": label,
            "kind": "rate_limit", "usedPercent": used,
            "remainingPercent": max(0, 100 - used),
            "resetAt": reset_at(value.get("resets_at")),
            "expiresAt": reset_at(value.get("expires_at")),
            "windowMinutes": 10080, "used": None, "limit": None, "unit": None,
        })
        seen.add(key)
    for window in normalize_limits(usage):
        if window["key"] not in seen:
            windows.append(window)
            seen.add(window["key"])
    extra = usage.get("extra_usage")
    if isinstance(extra, dict) and isinstance(extra.get("is_enabled"), bool):
        used, limit = number(extra.get("used_credits")), number(extra.get("monthly_limit"))
        utilization = number(extra.get("utilization"))
        currency = extra.get("currency")
        currency = currency if isinstance(currency, str) and re.fullmatch(r"[A-Z]{3}", currency) else None
        # OAuth credit amounts are minor currency units (CodexBar's primary
        # mapper documents this). Only label money when the API names its
        # currency; never silently assume USD when currency is absent.
        if currency:
            used = used / 100 if used is not None else None
            limit = limit / 100 if limit is not None else None
        window = {
            "key": "extra_usage", "label": "Extra usage", "kind": "extra_usage",
            "usedPercent": utilization,
            "remainingPercent": max(0, 100 - utilization) if utilization is not None else None,
            "resetAt": reset_at(extra.get("resets_at")),
            "expiresAt": reset_at(extra.get("expires_at")),
            "windowMinutes": None, "used": used, "limit": limit,
            "remaining": max(0, limit - used) if used is not None and limit is not None else None,
            "unit": currency or ("credits" if used is not None or limit is not None else None),
            "enabled": extra["is_enabled"],
        }
        if isinstance(extra.get("is_unlimited"), bool):
            window["unlimited"] = extra["is_unlimited"]
        windows.append(window)
    # Final gateway: normalize_windows must never emit a window that the
    # desktop-live normalizeSample would reject wholesale for key/label reasons.
    return [window for window in windows
            if _window_bounds_ok(window.get("key"), window.get("label"))]


def balance_window(key, label, remaining, unit, limit=None, used=None, expires=None, enabled=None):
    window = {"key": key, "label": label, "kind": "balance", "usedPercent": None,
              "remainingPercent": None, "resetAt": None, "expiresAt": expires,
              "windowMinutes": None, "used": used, "limit": limit,
              "remaining": remaining, "unit": unit}
    if isinstance(enabled, bool):
        window["enabled"] = enabled
    return window


def normalize_reset_credits(value, now=None):
    # Grants listed while eligible is not true are UNKNOWN inventory, never a
    # confirmed zero: the provider withholds grant detail for ineligible
    # accounts, so an empty list there carries no count at all.
    if (not isinstance(value, dict) or value.get("eligible") is not True or
        not isinstance(value.get("grants"), list) or len(value["grants"]) > MAX_RESET_GRANTS):
        return []
    now = time.time() if now is None else now
    grants, available = [], 0
    for index, grant in enumerate(value["grants"], 1):
        if not isinstance(grant, dict):
            return []
        left, total = grant.get("resets_left"), grant.get("resets_total")
        if (isinstance(left, bool) or not isinstance(left, int) or not 0 <= left <= 50 or
            (total is not None and (isinstance(total, bool) or not isinstance(total, int) or not left <= total <= 50)) or
            not isinstance(grant.get("paused"), bool)):
            return []
        start, end = reset_at(grant.get("starts_at")), reset_at(grant.get("ends_at"))
        if (grant.get("starts_at") is not None and start is None) or (grant.get("ends_at") is not None and end is None):
            return []
        # clears names the limit windows one grant refills. Anything outside
        # the bounded machine-key shape fails the whole inventory closed.
        clears = None
        if "clears" in grant:
            raw_clears = grant["clears"]
            if (not isinstance(raw_clears, list) or len(raw_clears) > 8 or
                any(not isinstance(item, str) or re.fullmatch(r"[a-z0-9_]{1,40}", item) is None
                    for item in raw_clears)):
                return []
            clears, seen_clears = [], set()
            for item in raw_clears:
                if item not in seen_clears:
                    seen_clears.add(item)
                    clears.append(item)
        started = start is None or dt.datetime.fromisoformat(start.replace("Z", "+00:00")).timestamp() <= now
        expired = end is not None and dt.datetime.fromisoformat(end.replace("Z", "+00:00")).timestamp() <= now
        usable = value["eligible"] and not grant["paused"] and started and not expired and left > 0
        available += left if usable else 0
        if available > 50:
            return []
        category = "used" if left == 0 else "expired" if expired else "available" if usable else "saved"
        # A used grant's historical end belongs to its own row, never to the
        # summary of currently available resets. usable_now gates redemption,
        # not whether an otherwise valid reset remains in saved inventory.
        window = balance_window("reset_credit_" + category + "_grant_" + str(index),
            category.capitalize() + " rate-limit reset grant " + str(index), left, "resets",
            limit=total, used=total - left if total is not None else None, expires=end)
        if clears:
            window["clears"] = clears
        grants.append(window)
    return [balance_window("reset_credits_available", "Rate-limit resets available", available, "resets")] + grants


def normalize_session_reset_allowance(value):
    """One weekly session-reset allowance: availability gates the remaining count."""
    if not isinstance(value, dict) or value.get("eligible") is not True:
        return []
    if not isinstance(value.get("available"), bool):
        return []
    per_week = value.get("resets_per_week")
    if isinstance(per_week, bool) or not isinstance(per_week, int) or not 1 <= per_week <= 50:
        return []
    remaining = per_week if value["available"] else 0
    window = balance_window("session_reset_allowance", "Weekly session reset", remaining, "resets",
        limit=per_week, used=per_week - remaining)
    window["resetAt"] = reset_at(value.get("next_available_at"))
    return [window]


# The upstream program name this normalizes; kept as an alias for readers.
normalize_juniper_tide = normalize_session_reset_allowance


def normalize_prepaid(value):
    if not isinstance(value, dict):
        return []
    amount = number(value.get("amount"))
    if amount is None:
        return []
    currency = value.get("currency")
    if currency is not None and (not isinstance(currency, str) or not re.fullmatch(r"[A-Z]{3}", currency)):
        return []
    currency = currency if isinstance(currency, str) and re.fullmatch(r"[A-Z]{3}", currency) else None
    # The primary ClaudeWebAPIFetcher maps explicitly named-currency amount
    # from cents. A null currency remains raw provider credits, never USD.
    return [balance_window("prepaid_balance", "Prepaid balance" if currency else "Prepaid credit balance",
        amount / 100 if currency else amount, currency or "credits", expires=reset_at(value.get("next_expires_at")))]


def _validate_cookie_rows(rows, organization_uuid):
    """Shared session-cookie validation for capsule and live Chromium rows.

    Operates on decrypted row dicts carrying name, host_key, path,
    top_frame_site_key, value, is_secure, is_httponly, expires_utc and an
    optional has_expires flag. Raises UsageUnavailable on any mismatch;
    cookie values never leave memory.
    """
    cookies, seen = [], set()
    for cookie in rows:
        if (not isinstance(cookie, dict) or cookie.get("name") not in ("sessionKey", "lastActiveOrg") or
            cookie.get("host_key") not in ("claude.ai", ".claude.ai") or
            cookie.get("path") != "/" or cookie.get("top_frame_site_key", "") != ""):
            raise UsageUnavailable()
        name, token = cookie["name"], cookie.get("value")
        if not isinstance(token, str) or not token or len(token) > 32768 or ";" in token or any(ord(char) < 32 for char in token):
            raise UsageUnavailable()
        if cookie.get("has_expires"):
            # Stored expiry is migration snapshot metadata, not authoritative:
            # Desktop keeps the live session while the capsule copy's timestamp
            # goes stale. Identity is re-verified against /api/account before
            # balances; a truly dead session fails there and stays unavailable.
            # Only malformed expiry metadata is rejected here.
            if number(cookie.get("expires_utc")) is None:
                raise UsageUnavailable()
        elif "expires_utc" in cookie:
            # Live Chromium rows carry expires_utc even when the has_expires
            # column is absent: a present but non-numeric expiry is malformed.
            # Past timestamps are not rejected; the server decides liveness.
            if number(cookie.get("expires_utc")) is None:
                raise UsageUnavailable()
        if name == "sessionKey" and not (cookie.get("is_secure") == 1 and cookie.get("is_httponly") == 1):
            raise UsageUnavailable()
        if name == "lastActiveOrg" and token != organization_uuid:
            raise UsageUnavailable()
        if name in seen:
            # Native bootstrap can retain a Safari-imported org cookie and
            # create its own variant. Only duplicate public org selectors
            # already bound to the same OAuth organization may be coalesced.
            if name == "lastActiveOrg":
                continue
            raise UsageUnavailable()
        cookies.append({"name": name, "value": token})
        seen.add(name)
    if "sessionKey" not in seen or len(cookies) > 2:
        raise UsageUnavailable()
    return cookies


def capsule_cookies(profile_id, account_uuid, organization_uuid, home=None, expected_email=None):
    expected = resolve_expected_email(expected_email)
    if not valid_profile_id(profile_id) or expected is None:
        raise UsageUnavailable()
    home = Path.home() if home is None else Path(home)
    path = home / ".ccs/claude-session-migration" / (profile_id + "-source.dpapi")
    if path.is_symlink():
        raise UsageUnavailable()
    with path.open("rb") as handle:
        raw = handle.read(MAX_CAPSULE_BYTES + 1)
    if len(raw) > MAX_CAPSULE_BYTES:
        raise UsageUnavailable()
    capsule = json.loads(unprotect(raw, expected_length=None))
    if (not isinstance(capsule, dict) or capsule.get("schemaVersion") != 1 or
        capsule.get("profileId") != profile_id or capsule.get("email") != expected or
        capsule.get("accountUuid") != account_uuid or not isinstance(capsule.get("cookies"), list) or
        len(capsule["cookies"]) > 20):
        raise UsageUnavailable()
    return _validate_cookie_rows(capsule["cookies"], organization_uuid)


def _cookie_db_path(profile_root):
    """First existing Chromium Cookies file, Network/Cookies preferred."""
    for candidate in (Path(profile_root) / "Network" / "Cookies", Path(profile_root) / "Cookies"):
        try:
            if candidate.is_file():
                return candidate
        except (OSError, ValueError):
            continue
    return None


def _cookie_db_uri(path):
    """Read-only immutable sqlite URI; Path.as_uri keeps Windows drive paths valid."""
    path = Path(path)
    if not path.is_absolute():
        raise UsageUnavailable()
    return path.as_uri() + "?mode=ro&immutable=1"


def _open_cookie_db(profile_root):
    """Open the Chromium Cookies DB read-only (immutable), never write."""
    path = _cookie_db_path(profile_root)
    if path is None:
        raise UsageUnavailable()
    uri = _cookie_db_uri(path)
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=1.0)
        try:
            row = connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='cookies'"
            ).fetchone()
            if row is None:
                connection.close()
                raise UsageUnavailable()
            return connection
        except sqlite3.Error:
            try:
                connection.close()
            except Exception:
                pass
            raise UsageUnavailable()
    except (sqlite3.Error, OSError, ValueError):
        raise UsageUnavailable()


def _cookie_columns(connection):
    try:
        rows = connection.execute("PRAGMA table_info(cookies)").fetchall()
    except sqlite3.Error:
        raise UsageUnavailable()
    return [row[1] for row in rows]


def live_session_cookies(profile_root, organization_uuid):
    """Read the Desktop profile's live web session cookies in memory.

    Only the bounded sessionKey/lastActiveOrg rows for claude.ai are
    selected, decrypted with the same os_crypt key as decrypt_cache, and
    validated with the capsule's shared row validator. Cookie values stay
    in memory: never logged, persisted, or included in exceptions.
    """
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except Exception:
        raise UsageUnavailable()

    key = profile_key(profile_root)
    connection = _open_cookie_db(profile_root)
    try:
        columns = _cookie_columns(connection)
        required = ["name", "host_key", "path", "value", "encrypted_value",
                    "expires_utc", "is_secure", "is_httponly"]
        missing = [col for col in required if col not in columns]
        if missing:
            raise UsageUnavailable()
        optional = ["top_frame_site_key", "creation_utc", "has_expires"]
        selected = required + [col for col in optional if col in columns]
        sql = f"SELECT {','.join(selected)} FROM cookies WHERE name IN (?,?) AND host_key IN (?,?) LIMIT 20"
        try:
            raw_rows = connection.execute(
                sql, ("sessionKey", "lastActiveOrg", "claude.ai", ".claude.ai")
            ).fetchall()
        except sqlite3.Error:
            raise UsageUnavailable()
    finally:
        try:
            connection.close()
        except Exception:
            pass

    has_top_frame = "top_frame_site_key" in columns
    has_has_expires = "has_expires" in columns

    rows = []
    for raw in raw_rows:
        record = dict(zip(selected, raw))
        name = record.get("name")
        host_key = record.get("host_key")
        path_val = record.get("path")
        encrypted = record.get("encrypted_value")
        plaintext_col = record.get("value")
        expires_utc = record.get("expires_utc")
        is_secure = record.get("is_secure")
        is_httponly = record.get("is_httponly")
        top_frame = record.get("top_frame_site_key", "") if has_top_frame else ""
        if top_frame is None:
            top_frame = ""
        # Decrypt v10 blobs; plain value is used only when encrypted is empty.
        if encrypted is not None and not (isinstance(encrypted, (bytes, bytearray)) and len(encrypted) == 0):
            # Normalize memoryview/other buffer types to bytes.
            if isinstance(encrypted, memoryview):
                encrypted = bytes(encrypted)
            if not isinstance(encrypted, (bytes, bytearray)):
                raise UsageUnavailable()
            blob = bytes(encrypted)
            if not blob.startswith(b"v10") or len(blob) < 31:
                raise UsageUnavailable()
            try:
                plaintext = AESGCM(key).decrypt(blob[3:15], blob[15:], None)
            except Exception:
                raise UsageUnavailable()
            # Newer Chromium prefixes plaintext with the 32-byte SHA-256 of
            # the cookie host. Strip only on an exact hash match; otherwise
            # accept the plaintext only as direct UTF-8.
            if not isinstance(host_key, str):
                raise UsageUnavailable()
            try:
                host_hash = hashlib.sha256(host_key.encode("utf-8")).digest()
                stripped_host = host_key[1:] if host_key.startswith(".") else host_key
                stripped_hash = hashlib.sha256(stripped_host.encode("utf-8")).digest()
                candidates = [host_hash, stripped_hash] if stripped_host != host_key else [host_hash]
            except Exception:
                raise UsageUnavailable()
            value_bytes = plaintext
            if len(plaintext) >= 32:
                prefix = plaintext[:32]
                for candidate in candidates:
                    if prefix == candidate:
                        value_bytes = plaintext[32:]
                        break
                # Also handle the degenerate case where host_key itself is
                # queried both ways: already covered by candidates.
            try:
                token = value_bytes.decode("utf-8")
            except (UnicodeError, ValueError):
                raise UsageUnavailable()
        elif encrypted is not None and isinstance(encrypted, (bytes, bytearray)) and len(encrypted) == 0:
            # Encrypted column present but empty: fall back to the plain column.
            if isinstance(plaintext_col, bytes):
                try:
                    token = plaintext_col.decode("utf-8")
                except (UnicodeError, ValueError):
                    raise UsageUnavailable()
            elif isinstance(plaintext_col, str):
                token = plaintext_col
            elif plaintext_col is None:
                token = ""
            else:
                raise UsageUnavailable()
        else:
            # encrypted_value is NULL/None: plain column only.
            if isinstance(plaintext_col, bytes):
                try:
                    token = plaintext_col.decode("utf-8")
                except (UnicodeError, ValueError):
                    raise UsageUnavailable()
            elif isinstance(plaintext_col, str):
                token = plaintext_col
            elif plaintext_col is None:
                token = ""
            else:
                raise UsageUnavailable()

        entry = {
            "name": name,
            "host_key": host_key,
            "path": path_val,
            "top_frame_site_key": top_frame,
            "value": token,
            "is_secure": is_secure,
            "is_httponly": is_httponly,
            "expires_utc": expires_utc,
        }
        if has_has_expires:
            entry["has_expires"] = record.get("has_expires")
        rows.append(entry)

    return _validate_cookie_rows(rows, organization_uuid)


def live_cookie_fingerprint(profile_root):
    """SHA-256 hex over the live cookie rows, or None when no DB/rows.

    Covers each selected row's (name, host_key, creation_utc,
    encrypted_value-or-value bytes) in stable order. Never logs cookie
    material; used by failover for before/after change detection.
    """
    connection = None
    try:
        path = _cookie_db_path(profile_root)
        if path is None:
            return None
        try:
            connection = sqlite3.connect(_cookie_db_uri(path), uri=True, timeout=1.0)
            row = connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='cookies'"
            ).fetchone()
            if row is None:
                return None
            columns = [r[1] for r in connection.execute("PRAGMA table_info(cookies)").fetchall()]
        except (sqlite3.Error, OSError, ValueError, UsageUnavailable):
            return None
        if "name" not in columns or "host_key" not in columns:
            return None
        selected = ["name", "host_key"]
        if "creation_utc" in columns:
            selected.append("creation_utc")
        if "encrypted_value" in columns:
            selected.append("encrypted_value")
        if "value" in columns:
            selected.append("value")
        sql = f"SELECT {','.join(selected)} FROM cookies WHERE name IN (?,?) AND host_key IN (?,?) LIMIT 20"
        try:
            raw_rows = connection.execute(
                sql, ("sessionKey", "lastActiveOrg", "claude.ai", ".claude.ai")
            ).fetchall()
        except sqlite3.Error:
            return None
        if not raw_rows:
            return None
        entries = []
        for raw in raw_rows:
            record = dict(zip(selected, raw))
            name = record.get("name")
            host_key = record.get("host_key")
            creation = record.get("creation_utc", 0)
            if creation is None:
                creation = 0
            encrypted = record.get("encrypted_value")
            value_col = record.get("value")
            if encrypted is not None and not (isinstance(encrypted, (bytes, bytearray)) and len(encrypted) == 0):
                if isinstance(encrypted, memoryview):
                    raw_bytes = bytes(encrypted)
                elif isinstance(encrypted, (bytes, bytearray)):
                    raw_bytes = bytes(encrypted)
                elif isinstance(encrypted, str):
                    raw_bytes = encrypted.encode("utf-8")
                else:
                    raw_bytes = b""
            elif value_col is not None:
                if isinstance(value_col, bytes):
                    raw_bytes = value_col
                elif isinstance(value_col, str):
                    raw_bytes = value_col.encode("utf-8")
                else:
                    raw_bytes = b""
            else:
                raw_bytes = b""
            entries.append((str(name), str(host_key), creation, raw_bytes))
        entries.sort(key=lambda item: (item[0], item[1], item[2] if isinstance(item[2], int) else str(item[2])))
        hasher = hashlib.sha256()
        for name, host_key, creation, raw_bytes in entries:
            hasher.update(name.encode("utf-8") + b"\x00")
            hasher.update(host_key.encode("utf-8") + b"\x00")
            hasher.update(str(creation).encode("utf-8") + b"\x00")
            hasher.update(raw_bytes + b"\x00")
        return hasher.hexdigest()
    finally:
        if connection is not None:
            try:
                connection.close()
            except Exception:
                pass


class WebClient:
    def __init__(self, cookies, organization_uuid):
        from curl_cffi import requests
        if not re.fullmatch(UUID, organization_uuid):
            raise UsageUnavailable()
        self.requests = requests
        self.deadline = time.monotonic() + 7
        self.organization_uuid = organization_uuid
        self.cookie = "; ".join(item["name"] + "=" + item["value"] for item in cookies)
        self.routes = {"/api/account", "/api/organizations/" + organization_uuid + "/usage?cedar_ember=1",
                       "/api/organizations/" + organization_uuid + "/prepaid/credits"}

    def get(self, route):
        remaining = self.deadline - time.monotonic()
        if route not in self.routes or remaining <= 0:
            raise UsageUnavailable()
        response = self.requests.get("https://claude.ai" + route,
            headers={"Cookie": self.cookie, "Accept": "application/json", "Origin": "https://claude.ai", "Referer": "https://claude.ai/"},
            impersonate="chrome", verify=True, allow_redirects=False, timeout=min(3, remaining), stream=True)
        try:
            if response.status_code == 429:
                raise UsageUnavailable("rate_limited", retry_after_seconds(response.headers.get("Retry-After")))
            if response.status_code != 200:
                raise UsageUnavailable()
            body = bytearray()
            for chunk in response.iter_content(chunk_size=65536):
                body.extend(chunk)
                if time.monotonic() >= self.deadline or len(body) > MAX_BYTES:
                    raise UsageUnavailable()
            value = json.loads(body)
            if not isinstance(value, dict):
                raise UsageUnavailable()
            return value
        finally:
            try:
                response.close()
            except Exception:
                # Closing a failed transfer must not replace its provider hold.
                pass


def verified_web_client(profile_id, account_uuid, organization_uuid, home=None, expected_email=None, profile_root=None):
    """The profile's existing web session, bound to the same OAuth account and org.

    Cookies stay on Windows. Private reset reads need the full provider inventory,
    while the public collector publishes only its normalized, ID-free windows.
    """
    expected = resolve_expected_email(expected_email)
    if expected is None:
        raise UsageUnavailable()
    if profile_root is not None:
        try:
            cookies = live_session_cookies(profile_root, organization_uuid)
        except UsageUnavailable:
            cookies = capsule_cookies(profile_id, account_uuid, organization_uuid, home, expected)
    else:
        cookies = capsule_cookies(profile_id, account_uuid, organization_uuid, home, expected)
    client = WebClient(cookies, organization_uuid)
    account = client.get("/api/account")
    if account.get("uuid") != account_uuid or account.get("email_address") != expected:
        raise UsageUnavailable()
    memberships = account.get("memberships")
    if not isinstance(memberships, list) or len(memberships) > 128:
        raise UsageUnavailable()
    organizations = {membership["organization"].get("uuid") for membership in memberships
        if isinstance(membership, dict) and isinstance(membership.get("organization"), dict)}
    if organization_uuid not in organizations:
        raise UsageUnavailable()
    return client


def optional_web_windows(profile_id, account_uuid, organization_uuid, home=None, availability=None, expected_email=None, cooldown=None, profile_root=None):
    # Optional balances never turn valid native OAuth quota into a sign-in
    # request or an unreadable balance into zero. Existing callers keep a list;
    # the optional output parameter carries only a bounded provider cooldown.
    if availability is not None:
        availability.update(resetCredits="unavailable", prepaidBalance="unavailable")

    def rate_limited(error):
        if not isinstance(error, UsageUnavailable) or error.status != "rate_limited":
            return False
        if cooldown is not None:
            cooldown["failureCode"] = "rate_limited"
            if error.retry_after is not None:
                cooldown["retryAfterSeconds"] = error.retry_after
        return True

    try:
        client = verified_web_client(profile_id, account_uuid, organization_uuid, home, expected_email, profile_root)
    except Exception as error:
        rate_limited(error)
        return []
    windows = []
    try:
        usage = client.get("/api/organizations/" + organization_uuid + "/usage?cedar_ember=1")
        cedar = usage.get("cedar_ember")
        if availability is not None and isinstance(cedar, dict) and cedar.get("eligible") is True:
            availability["resetCredits"] = "ok"
        windows.extend(normalize_reset_credits(cedar))
        windows.extend(normalize_session_reset_allowance(usage.get("juniper_tide")))
    except Exception as error:
        if rate_limited(error):
            return windows
    try:
        prepaid = client.get("/api/organizations/" + organization_uuid + "/prepaid/credits")
        if availability is not None:
            availability["prepaidBalance"] = "ok"
        windows.extend(normalize_prepaid(prepaid))
    except Exception as error:
        rate_limited(error)
    return windows


def _plan_name(value):
    """A bounded plain-text plan name; control characters and credential-shaped
    strings never pass, exactly like every other upstream string this
    collector forwards. C1 controls count as control characters: the
    displays' whitespace classes do not treat them as word boundaries, so a
    name carrying one could hide a Max account's Fable column."""
    if not isinstance(value, str):
        return None
    name = value.strip()
    if not 0 < len(name) <= 80 or re.search(
        r"[\x00-\x1f\x7f-\x9f\u061c\u200e\u200f\u2028-\u202e\u2066-\u2069\ufeff]", name
    ):
        return None
    if re.search(r"(?i)(bearer\s|dca:|sk[-_]|eyJ[A-Za-z0-9_-]{8})", name):
        return None
    return name


def claude_plan(account, organization=None, fallback=None):
    """The plan name Claude itself displays, whatever Claude names it now.
    The organization's display fields are the official name ("Ultra" for a
    renamed Max plan) and win over everything; then a single has_claude_*
    entitlement flag — a new tier arrives under its own flag and surfaces
    with its own name, underscores read as spaces; then the entitlement
    booleans; then the stored class. Fable eligibility does NOT ride on this
    name (a "Ultra" name carries no max word): claude_plan_class carries the
    entitlement class to the displays instead."""
    for source in (organization, account):
        if not isinstance(source, dict):
            continue
        for key in ("plan_display_name", "plan_display_label", "plan_type", "planType",
                    "tier", "tier_name", "tierName", "plan_name", "planName"):
            name = _plan_name(source.get(key))
            if name:
                return name.replace("_", " ") if name == name.lower() and "_" in name else name
    flags = sorted({
        key[len("has_claude_"):]
        for key, value in (account or {}).items()
        if isinstance(key, str) and isinstance(value, bool) and value
        and SAFE_FLAG.fullmatch(key)
    })
    names = [name for name in (flag.replace("_", " ") for flag in flags) if _plan_name(name)]
    if len(names) == 1:
        return names[0]
    if (account or {}).get("has_claude_max") is True:
        return "max"
    if (account or {}).get("has_claude_pro") is True:
        return "pro"
    # The fallback publishes under the same screen as every other source:
    # a stored string carrying a separator or control never bypasses it.
    return _plan_name(fallback)


def claude_plan_class(account, organization=None, name=None):
    """The entitlement class the Fable column gates on, independent of the
    display name. The rate limit tier names it in bounded tokens
    ("default_claude_max_20x"), then the entitlement flags, then the selected
    display name's own max word."""
    tier_tokens = set()
    if isinstance(organization, dict) and isinstance(organization.get("rate_limit_tier"), str):
        tier_tokens = set(re.split(r"[^a-z0-9]+", organization["rate_limit_tier"].lower()))
    if "max" in tier_tokens:
        return "max"
    if "pro" in tier_tokens:
        return "pro"
    account = account or {}
    if account.get("has_claude_max") is True:
        return "max"
    if account.get("has_claude_pro") is True:
        return "pro"
    if name and re.search(r"(?i)(?:^|[\s/_-])max(?:\s*(?:5|20)x)?(?:$|[\s/_-])", name):
        return "max"
    if name and re.search(r"(?i)(?:^|[\s/_-])pro(?:$|[\s/_-])", name):
        return "pro"
    return None


SAFE_FLAG = re.compile(r"has_claude_[a-z0-9_]{1,40}")


def reset_login_binding(account, cache):
    return hashlib.sha256(json.dumps([account, cache], sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def capture_reset_login(request):
    """Read saved credentials only; no OAuth/provider call belongs in the Store capture section."""
    profile_id = request["profile"]
    if (sys.platform != "win32" or not valid_profile_id(profile_id)
            or resolve_expected_email(request["expectedEmail"]) is None):
        raise UsageUnavailable()
    root = resolve_profile_path(profile_id, request.get("profileDir"), None, request.get("isDefault") is True)
    account, cache = decrypt_cache(root)
    candidates = eligible_entries(account, cache)
    if not candidates:
        raise UsageUnavailable()
    if resolve_profile_path(profile_id, request.get("profileDir"), None, request.get("isDefault") is True) != root:
        raise UsageUnavailable()
    return {"account": account, "organization": candidates[0][1][2], "savedLoginBinding": reset_login_binding(account, cache),
            "observedAtMs": int(time.time() * 1000)}


def collect(profile_id, platform, home=None, expected_email=None, profile_dir=None, is_default=False, private_reset_sink=None, expected_saved_binding=None, expected_saved_organization=None):
    result = {
        "schemaVersion": 1, "provider": "claude", "profileId": profile_id,
        "platform": platform, "status": "unavailable", "email": None,
        "source": "Claude Desktop live quota on Windows", "plan": None,
        "fetchedAt": None, "accountVerified": False, "organizationVerified": False,
        "windows": [],
    }
    expected = resolve_expected_email(expected_email)
    if not valid_profile_id(profile_id) or expected is None or platform != "windows" or sys.platform != "win32":
        return result
    cooldown = {}
    try:
        profile_root = resolve_profile_path(profile_id, profile_dir, home, is_default)

        def verify_root():
            if resolve_profile_path(profile_id, profile_dir, home, is_default) != profile_root:
                raise UsageUnavailable()

        account_uuid, cache = decrypt_cache(profile_root)
        candidates = eligible_entries(account_uuid, cache)
        saved_binding = reset_login_binding(account_uuid, cache) if private_reset_sink is not None else None

        if expected_saved_binding is not None and (private_reset_sink is None or saved_binding != expected_saved_binding):
            raise UsageUnavailable()

        def verify_login():
            verify_root()
            current_account, current_cache = decrypt_cache(profile_root)
            current_binding = reset_login_binding(current_account, current_cache)
            if current_binding != saved_binding:
                raise UsageUnavailable()

        if not candidates:
            raise UsageUnavailable("needs_sign_in")
        if expected_saved_organization is not None:
            candidates = [entry for entry in candidates if entry[1][2] == expected_saved_organization]
            if not candidates:
                raise UsageUnavailable()
        client = Client()
        for _, identities, token in candidates:
            verify_root()
            try:
                profile = client.get("/api/oauth/profile", token)
            except UsageUnavailable as error:
                if error.status == "needs_sign_in":
                    continue
                raise
            account, organization = profile.get("account"), profile.get("organization")
            if not isinstance(account, dict) or not isinstance(organization, dict):
                continue
            if (
                account.get("email") != expected
                or account.get("uuid") != account_uuid
                or not isinstance(organization.get("uuid"), str)
                or organization["uuid"] != identities[2]
            ):
                continue
            verify_root()
            identity_verified_ms = int(time.time() * 1000)
            usage = None
            if private_reset_sink is not None:
                # The OAuth usage endpoint can throttle while the existing web
                # session still serves grants. Prefer that identity-verified
                # inventory without exporting its cookies or changing the
                # OAuth token used for an explicitly confirmed claim.
                verify_login()
                try:
                    web = verified_web_client(profile_id, account_uuid, organization["uuid"], home, expected, profile_root)
                    usage = web.get("/api/organizations/" + organization["uuid"] + "/usage?cedar_ember=1")
                except Exception:
                    # OAuth-only sign-ins retain their original read path.
                    pass
                verify_login()
            if usage is None:
                usage = client.get("/api/oauth/usage?cedar_ember=1", token)
            verify_root()
            if private_reset_sink is not None:
                verify_login()
            windows = normalize_windows(usage)
            # The OAuth usage payload with cedar_ember=1 carries the grant
            # inventory and weekly allowance directly; parse them here so
            # CLI and desktop both see them without a web capsule.
            windows.extend(normalize_reset_credits(usage.get("cedar_ember")))
            windows.extend(normalize_session_reset_allowance(usage.get("juniper_tide")))
            if not windows:
                raise UsageUnavailable()
            optional_extras = {"resetCredits": "unavailable", "prepaidBalance": "unavailable"}
            oauth_has_reset = any(w["key"] == "reset_credits_available" or w["key"].startswith("reset_credit_") for w in windows)
            web_windows = [] if private_reset_sink is not None else optional_web_windows(profile_id, account_uuid, organization["uuid"], home, optional_extras, expected, cooldown, profile_root=profile_root)
            # The web helper resets availability to unavailable on entry; if
            # the OAuth payload already supplied the grants, the account is
            # not in an unavailable web state — preserve the OAuth verdict.
            if oauth_has_reset:
                optional_extras["resetCredits"] = "ok"
            # Deduplicate web grants that the OAuth payload already supplied.
            seen_keys = {w["key"] for w in windows}
            for w in web_windows:
                if w["key"] not in seen_keys:
                    windows.append(w)
                    seen_keys.add(w["key"])
            verify_root()
            plan_name = claude_plan(
                account, organization,
                "max" if account.get("has_claude_max") is True else "pro" if account.get("has_claude_pro") is True else None,
            )
            result.update(
                status="ok", email=expected,
                plan=plan_name,
                planClass=claude_plan_class(account, organization, plan_name),
                fetchedAt=now_iso(), accountVerified=True, organizationVerified=True, windows=windows,
                optionalExtras=optional_extras,
                sourceContextFingerprint=hashlib.sha256(json.dumps([
                    "claude-desktop-windows-v1", profile_id, account_uuid.lower(),
                    organization["uuid"].lower(), identities[1].lower(), "https://api.anthropic.com",
                ], separators=(",", ":")).encode("ascii")).hexdigest(),
            )
            result.update(cooldown)
            if private_reset_sink is not None:
                # Internal host seam only. Never put this on result/public usage or a cache.
                private_reset_sink({"account": account_uuid, "organization": organization["uuid"],
                                    "usage": usage, "token": token, "verifyRoot": verify_root,
                                    "verifyLogin": verify_login, "observedAtMs": identity_verified_ms, "savedLoginBinding": saved_binding})
            return result
        raise UsageUnavailable("needs_sign_in")
    except UsageUnavailable as error:
        result["status"] = "error" if error.status == "rate_limited" else error.status
        if error.status == "rate_limited":
            cooldown["failureCode"] = "rate_limited"
            if error.retry_after is not None:
                cooldown["retryAfterSeconds"] = error.retry_after
    except Exception:
        # Includes missing optional cryptography module and native decode errors.
        # No native exception text, paths or decrypted data are printed.
        result["status"] = "unavailable"
    result.update(cooldown)
    return result


def reset_host(request):
    """Private stdin host RPC. Credentials stay on this host, including during writes.

    The server must already have durably consumed the caller's exact confirmation.
    No public collector CLI, refresh or switch calls this function.
    """
    try:
        profile_id = request["profile"]
        if request.get("action") == "capture":
            return capture_reset_login(request)
        captured = []
        result = collect(profile_id, "windows", expected_email=request["expectedEmail"],
                         profile_dir=request.get("profileDir"), is_default=request.get("isDefault") is True,
                         private_reset_sink=captured.append, expected_saved_binding=request.get("expectedSavedBinding"),
                         expected_saved_organization=request.get("expectedSavedOrganization"))
        if result.get("status") != "ok" or len(captured) != 1:
            if result.get("failureCode") == "rate_limited":
                return {"unavailable": True, "reason": "rate-limited"}
            return {"unavailable": True}
        private = captured[0]
        cedar = private["usage"].get("cedar_ember")
        if not isinstance(cedar, dict) or cedar.get("eligible") is not True or not isinstance(cedar.get("grants"), list):
            return {"unavailable": True}
        identity = {"provider": "claude", "account": private["account"],
                    "organization": private["organization"], "profile": profile_id, "host": request["host"]}
        binding = hashlib.sha256(json.dumps([identity, cedar], sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        windows = {window["key"]: window["usedPercent"] for window in normalize_windows(private["usage"])
                   if window["kind"] == "rate_limit"}
        # Grants retain the provider's legacy Fable bucket ID, while current
        # usage names that quota in limits[]. Use only the exact, unscoped
        # Fable window; an absent or conflicting reading stays unconfirmed.
        if "seven_day_overage_included" not in windows and "seven_day_fable" in windows:
            windows["seven_day_overage_included"] = windows["seven_day_fable"]
        expiries = []
        for grant in cedar["grants"]:
            raw = grant.get("ends_at") if isinstance(grant, dict) else None
            end = reset_at(raw)
            if raw is not None and end is None:
                return {"unavailable": True}
            expiries.append(int(dt.datetime.fromisoformat(end.replace("Z", "+00:00")).timestamp() * 1000) if end else None)
        if request.get("action") == "read":
            private["verifyLogin"]()
            return {"identity": identity, "observedAtMs": private["observedAtMs"], "savedLoginBinding": private.get("savedLoginBinding"), "raw": cedar,
                    "expiries": expiries, "windows": windows, "sourceBinding": binding, "accountLabel": request["expectedEmail"]}
        write = request.get("write", {})
        body = write.get("body", {})
        if (request.get("action") != "claim" or write.get("identity") != identity or
            write.get("sourceBinding") != binding or write.get("method") != "claude.reset_rate_limits" or
            set(body) != {"program", "grant_id", "request_id"} or body.get("program") != "cedar_ember" or
            not isinstance(body.get("grant_id"), str) or re.fullmatch(r"[a-z0-9_-]{1,40}", body["grant_id"]) is None or
            not isinstance(body.get("request_id"), str) or re.fullmatch(r"[A-Za-z0-9_-]{1,64}", body["request_id"]) is None):
            return {"outcome": "identity-changed"}
        matches = [g for g in cedar["grants"] if isinstance(g, dict) and g.get("id") == body["grant_id"]]
        if len(matches) != 1 or matches[0].get("usable_now") is not True or matches[0].get("paused") is not False:
            return {"outcome": "unavailable"}
        if re.fullmatch(UUID, private["organization"]) is None:
            return {"outcome": "identity-changed"}
        private["verifyRoot"]()
        private["verifyLogin"]()
        client = Client()
        req = urllib.request.Request(
            "https://api.anthropic.com/api/organizations/" + private["organization"] + "/reset_rate_limits",
            data=json.dumps(body, separators=(",", ":")).encode("ascii"), method="POST",
            headers={"Authorization": "Bearer " + private["token"], "anthropic-beta": "oauth-2025-04-20",
                     "Content-Type": "application/json", "Accept": "application/json", "User-Agent": "CCS-ReadOnly-Usage/1.0"})
        try:
            with client.opener.open(req, timeout=8) as response:
                raw = response.read(MAX_BYTES + 1)
                if len(raw) > MAX_BYTES:
                    return {"outcome": "ambiguous"}
                answer = json.loads(raw)
                outcome = {"reset": "reset", "not_limited": "nothingToReset", "already_used": "already-used",
                           "ineligible": "noCredit", "cooldown": "cooldown"}.get(answer.get("result"), "ambiguous")
                return {"outcome": outcome}
        except urllib.error.HTTPError as error:
            if error.code == 429:
                return {"outcome": "rate-limited", "retryAfterMs": (retry_after_seconds(error.headers.get("Retry-After")) or 60) * 1000}
            return {"outcome": "signed-out" if error.code in (401, 403) else "ambiguous"}
    except Exception:
        # Neither exception text nor provider response bodies cross the helper boundary.
        pass
    return {"unavailable": True, "outcome": "ambiguous"}


def profile_arg(value):
    if not valid_profile_id(value):
        raise argparse.ArgumentTypeError("profile id is not valid")
    return value


def main():
    parser = argparse.ArgumentParser(description="Read existing Claude Desktop account usage")
    parser.add_argument("--provider", choices=("claude",), required=True)
    parser.add_argument("--profile", type=profile_arg, required=True)
    parser.add_argument("--platform", choices=("ubuntu", "mac", "windows"), required=True)
    parser.add_argument("--expected-email", default=None,
                        help="manifest email this invocation is bound to (or %s)" % EXPECTED_EMAIL_ENV)
    parser.add_argument("--profile-dir", default=None,
                        help="manifest profile directory; without it the generic named-profile directory is used")
    parser.add_argument("--default", action="store_true",
                        help="use the default Store profile directory when --profile-dir is absent")
    args = parser.parse_args()
    print(json.dumps(collect(args.profile, args.platform, expected_email=args.expected_email,
                             profile_dir=args.profile_dir, is_default=args.default),
                     ensure_ascii=True, allow_nan=False, separators=(",", ":")))


if __name__ == "__main__":
    main()
