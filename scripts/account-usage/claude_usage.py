"""Read-only, identity-bound Claude Desktop quota and reset timestamps.

Windows decrypts its existing OAuth cache as the same signed-in OS user. Tokens
stay in memory; this helper never refreshes, writes auth, reads Cookies, follows
HTTP redirects, or emits upstream response bodies or exception text.
Optional web extras use the existing same-user DPAPI session capsule, verified
again against this native account and organization before any balance request.
"""

import argparse
import base64
import ctypes
import datetime as dt
import hashlib
import json
import math
import ntpath
import os
from pathlib import Path
import re
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
UTC = dt.timezone.utc


class UsageUnavailable(Exception):
    def __init__(self, status="unavailable"):
        self.status = status


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


def read_json(path):
    try:
        with path.open("rb") as handle:
            contents = handle.read(MAX_BYTES + 1)
        if len(contents) > MAX_BYTES:
            raise UsageUnavailable()
        result = json.loads(contents.decode("utf-8-sig"))
        if not isinstance(result, dict):
            raise UsageUnavailable()
        return result
    except (OSError, ValueError, UnicodeError):
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


def windows_profile_path(profile_id, home=None, is_default=False):
    """Caller-selected profile directory; the default Store profile is flagged,
    never guessed from the ID."""
    home = Path.home() if home is None else Path(home)
    if is_default:
        return home / "AppData" / "Local" / "Packages" / "Claude_pzs8sxrjxfjjc" / "LocalCache" / "Roaming" / "Claude"
    return home / "AppData" / "Roaming" / ("Claude-" + profile_id)


def resolve_profile_path(profile_id, profile_dir, home=None, is_default=False):
    """Prefer the caller-supplied manifest directory; otherwise derive the
    generic named-profile directory (or the Store default when flagged)."""
    if profile_dir is not None:
        if (not isinstance(profile_dir, str) or not profile_dir or len(profile_dir) > 4096
                or "\x00" in profile_dir or not ntpath.isabs(profile_dir)):
            raise UsageUnavailable()
        return Path(profile_dir)
    if not valid_profile_id(profile_id):
        raise UsageUnavailable()
    return windows_profile_path(profile_id, home, is_default)


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


def decrypt_cache(profile_path):
    # cryptography is already installed on the user's Windows machine. No
    # packages are downloaded by the collector.
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    config = read_json(profile_path / "config.json")
    state = read_json(profile_path / "Local State")
    account_uuid = config.get("lastKnownAccountUuid")
    if not isinstance(account_uuid, str) or not re.fullmatch(UUID, account_uuid):
        raise UsageUnavailable("needs_sign_in")
    crypto_state = state.get("os_crypt")
    if not isinstance(crypto_state, dict):
        raise UsageUnavailable()
    wrapped = decode_base64(crypto_state.get("encrypted_key"))
    if not wrapped.startswith(b"DPAPI") or len(wrapped) <= 5:
        raise UsageUnavailable()
    key = unprotect(wrapped[5:])
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


def eligible_entries(account_uuid, cache):
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
            or expires is None or expires <= now + 30000
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
        if route not in ("/api/oauth/profile", "/api/oauth/usage"):
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
            raise UsageUnavailable("needs_sign_in" if error.code in (401, 403) else "error")
        except (OSError, ValueError, UnicodeError, urllib.error.URLError):
            raise UsageUnavailable("error")


def normalize_fable_limit(usage):
    """Read the reported global Fable weekly cap, never infer it from other models."""
    limits = usage.get("limits")
    if not isinstance(limits, list) or len(limits) > MAX_SCOPED_LIMITS:
        return []
    window = None
    for entry in limits:
        if not isinstance(entry, dict) or entry.get("kind") != "weekly_scoped" or entry.get("group") != "weekly":
            continue
        scope = entry.get("scope")
        model = scope.get("model") if isinstance(scope, dict) else None
        if (not isinstance(model, dict) or model.get("display_name") != "Fable"
            or scope.get("surface") is not None):
            continue
        used = number(entry.get("percent"))
        if used is None:
            return []
        candidate = {
            "key": "seven_day_fable", "label": "Weekly Fable usage", "kind": "rate_limit",
            "usedPercent": used, "remainingPercent": max(0, 100 - used),
            "resetAt": reset_at(entry.get("resets_at")), "expiresAt": None,
            "windowMinutes": 10080, "used": None, "limit": None, "unit": None,
        }
        if window is not None and candidate != window:
            # Conflicting scopes must not silently pick one account quota.
            return []
        window = candidate
    # is_active selects the provider's current limiting bucket. False is still
    # a genuine model quota, including an unused zero-percent Fable window.
    return [window] if window is not None else []


def normalize_windows(usage):
    windows = []
    for key, (label, minutes) in WINDOWS.items():
        value = usage.get(key)
        if not isinstance(value, dict):
            continue
        used = number(value.get("utilization"))
        if used is None:
            continue
        windows.append({
            "key": key,
            "label": label,
            "kind": "rate_limit",
            "usedPercent": used,
            "remainingPercent": max(0, 100 - used),
            "resetAt": reset_at(value.get("resets_at")),
            "expiresAt": reset_at(value.get("expires_at")),
            "windowMinutes": minutes,
            "used": None,
            "limit": None,
            "unit": None,
        })
    windows.extend(normalize_fable_limit(usage))
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
    return windows


def balance_window(key, label, remaining, unit, limit=None, used=None, expires=None, enabled=None):
    window = {"key": key, "label": label, "kind": "balance", "usedPercent": None,
              "remainingPercent": None, "resetAt": None, "expiresAt": expires,
              "windowMinutes": None, "used": used, "limit": limit,
              "remaining": remaining, "unit": unit}
    if isinstance(enabled, bool):
        window["enabled"] = enabled
    return window


def normalize_reset_credits(value, now=None):
    if (not isinstance(value, dict) or not isinstance(value.get("eligible"), bool) or
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
        grants.append(balance_window("reset_credit_" + category + "_grant_" + str(index),
            category.capitalize() + " rate-limit reset grant " + str(index), left, "resets",
            limit=total, used=total - left if total is not None else None, expires=end))
    return [balance_window("reset_credits_available", "Rate-limit resets available", available, "resets")] + grants


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
    cookies, seen = [], set()
    now = time.time() * 1000000 + CHROMIUM_EPOCH
    for cookie in capsule["cookies"]:
        if (not isinstance(cookie, dict) or cookie.get("name") not in ("sessionKey", "lastActiveOrg") or
            cookie.get("host_key") not in ("claude.ai", ".claude.ai") or
            cookie.get("path") != "/" or cookie.get("top_frame_site_key", "") != ""):
            raise UsageUnavailable()
        name, token = cookie["name"], cookie.get("value")
        if not isinstance(token, str) or not token or len(token) > 32768 or ";" in token or any(ord(char) < 32 for char in token):
            raise UsageUnavailable()
        if cookie.get("has_expires"):
            expires = number(cookie.get("expires_utc"))
            if expires is None or expires <= now:
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
            response.close()


def optional_web_windows(profile_id, account_uuid, organization_uuid, home=None, availability=None, expected_email=None):
    # This optional path must never turn a valid native OAuth quota failure
    # into a sign-in request, nor an unreadable web balance into a false zero.
    if availability is not None:
        availability.update(resetCredits="unavailable", prepaidBalance="unavailable")
    expected = resolve_expected_email(expected_email)
    if expected is None:
        return []
    try:
        cookies = capsule_cookies(profile_id, account_uuid, organization_uuid, home, expected)
        client = WebClient(cookies, organization_uuid)
        account = client.get("/api/account")
        if account.get("uuid") != account_uuid or account.get("email_address") != expected:
            return []
        memberships = account.get("memberships")
        if not isinstance(memberships, list) or len(memberships) > 128:
            return []
        organizations = {membership["organization"].get("uuid") for membership in memberships
            if isinstance(membership, dict) and isinstance(membership.get("organization"), dict)}
        if organization_uuid not in organizations:
            return []
    except Exception:
        return []
    windows = []
    try:
        usage = client.get("/api/organizations/" + organization_uuid + "/usage?cedar_ember=1")
        if availability is not None:
            availability["resetCredits"] = "ok"
        windows.extend(normalize_reset_credits(usage.get("cedar_ember")))
    except Exception:
        pass
    try:
        prepaid = client.get("/api/organizations/" + organization_uuid + "/prepaid/credits")
        if availability is not None:
            availability["prepaidBalance"] = "ok"
        windows.extend(normalize_prepaid(prepaid))
    except Exception:
        pass
    return windows


def collect(profile_id, platform, home=None, expected_email=None, profile_dir=None, is_default=False):
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
    try:
        account_uuid, cache = decrypt_cache(resolve_profile_path(profile_id, profile_dir, home, is_default))
        candidates = eligible_entries(account_uuid, cache)
        if not candidates:
            raise UsageUnavailable("needs_sign_in")
        client = Client()
        for _, identities, token in candidates:
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
            usage = client.get("/api/oauth/usage", token)
            windows = normalize_windows(usage)
            if not windows:
                raise UsageUnavailable()
            optional_extras = {"resetCredits": "unavailable", "prepaidBalance": "unavailable"}
            windows.extend(optional_web_windows(profile_id, account_uuid, organization["uuid"], home, optional_extras, expected))
            result.update(
                status="ok", email=expected,
                plan="max" if account.get("has_claude_max") is True else "pro" if account.get("has_claude_pro") is True else None,
                fetchedAt=now_iso(), accountVerified=True, organizationVerified=True, windows=windows,
                optionalExtras=optional_extras,
                sourceContextFingerprint=hashlib.sha256(json.dumps([
                    "claude-desktop-windows-v1", profile_id, account_uuid.lower(),
                    organization["uuid"].lower(), identities[1].lower(), "https://api.anthropic.com",
                ], separators=(",", ":")).encode("ascii")).hexdigest(),
            )
            return result
        raise UsageUnavailable("needs_sign_in")
    except UsageUnavailable as error:
        result["status"] = error.status
    except Exception:
        # Includes missing optional cryptography module and native decode errors.
        # No native exception text, paths or decrypted data are printed.
        result["status"] = "unavailable"
    return result


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
