"""Private, read-only primitives for coding-plan usage collectors.

Credential objects stay in this process. Public results are constructed from an
explicit scalar allowlist; upstream bodies and exception text are never emitted.
"""

import datetime as dt
from contextlib import closing
import base64
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


MAX_BYTES = 1024 * 1024
UTC = dt.timezone.utc
PROVIDERS = {
    "kimi-code": "Kimi Code",
    "qwen": "Qwen Token Plan",
    "zai": "Z.ai Coding Plan",
    "opencode-go": "OpenCode Go",
}


class CollectionError(Exception):
    def __init__(self, status, message):
        self.status = status
        self.message = message


def utc_now():
    return dt.datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def platform_name():
    return "mac" if sys.platform == "darwin" else "windows" if os.name == "nt" else "ubuntu"


def number(value):
    """Finite nonnegative numeric scalars only, including API numeric strings."""
    if isinstance(value, bool) or value is None:
        return None
    try:
        result = float(value)
    except (TypeError, ValueError, OverflowError):
        return None
    return result if math.isfinite(result) and result >= 0 else None


def percent(value):
    value = number(value)
    return value if value is not None and value <= 100 else None


def reset_at(value):
    """Normalize authoritative ISO or Unix seconds/milliseconds timestamps."""
    if value is None or isinstance(value, bool):
        return None
    try:
        timestamp = number(value)
        if timestamp is not None:
            if timestamp <= 0:
                return None
            if timestamp > 100000000000:
                timestamp /= 1000
            instant = dt.datetime.fromtimestamp(timestamp, UTC)
        elif isinstance(value, str):
            instant = dt.datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
            if instant.tzinfo is None:
                return None
            instant = instant.astimezone(UTC)
        else:
            return None
        if instant.year < 2000 or instant.year > 2200:
            return None
        return instant.isoformat(timespec="seconds").replace("+00:00", "Z")
    except (ValueError, TypeError, OverflowError, OSError):
        return None


def email_address(value):
    if not isinstance(value, str) or len(value) > 254:
        return None
    return value if re.fullmatch(r"[^\s@\x00-\x1f]+@[^\s@\x00-\x1f]+\.[^\s@\x00-\x1f]+", value) else None


def plan_label(value):
    if not isinstance(value, str):
        return None
    # Never forward an arbitrary upstream string which could contain a key.
    known = {"free", "lite", "essential", "basic", "standard", "pro", "professional", "premium",
             "max", "ultra", "team", "business", "enterprise", "go"}
    return value.lower() if value.lower() in known else None


def usage_window(key, label, used_percent=None, reset=None, minutes=None,
                 used=None, limit=None, unit=None, kind=None, remaining=None,
                 expires_at=None, unlimited=None, enabled=None):
    used_percent = percent(used_percent)
    result = {
        "key": key,
        "label": label,
        "usedPercent": used_percent,
        "remainingPercent": round(100 - used_percent, 8) if used_percent is not None else None,
        "resetAt": reset_at(reset),
        "windowMinutes": number(minutes),
        "used": number(used),
        "limit": number(limit),
        "unit": unit,
    }
    if kind in ("rate_limit", "balance", "spend", "extra_usage"):
        result["kind"] = kind
    if remaining is not None:
        result["remaining"] = number(remaining)
    if expires_at is not None:
        result["expiresAt"] = reset_at(expires_at)
    if isinstance(unlimited, bool):
        result["unlimited"] = unlimited
    if isinstance(enabled, bool):
        result["enabled"] = enabled
    return result


def account(provider, platform):
    return {
        "id": "plan-{}-{}".format(provider, platform),
        "provider": provider,
        "providerLabel": PROVIDERS[provider],
        "label": PROVIDERS[provider],
        "email": None,
        "plan": None,
        "platform": platform,
        "source": "Local credential store on " + {"mac": "Mac", "windows": "Windows", "ubuntu": "Ubuntu"}[platform],
        "status": "unavailable",
        "message": None,
        "fetchedAt": None,
        "sampledAt": None,
        "isActive": False,
        "windows": [],
        "capabilities": {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []},
    }


def load_json(path):
    try:
        with path.open("rb") as handle:
            contents = handle.read(MAX_BYTES + 1)
        return json.loads(contents) if len(contents) <= MAX_BYTES else None
    except (OSError, ValueError, UnicodeError):
        return None


def _string(value):
    return value if isinstance(value, str) and value and len(value) <= 32768 and "\x00" not in value else None


def _windows_unprotect(ciphertext):
    """Same-user DPAPI only; never prompt, persist plaintext, or bypass protection."""
    if os.name != "nt":
        return None
    try:
        import ctypes
        from ctypes import wintypes

        class Blob(ctypes.Structure):
            _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]

        crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        crypt32.CryptUnprotectData.argtypes = [ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.POINTER(Blob),
                                               ctypes.c_void_p, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(Blob)]
        crypt32.CryptUnprotectData.restype = wintypes.BOOL
        kernel32.LocalFree.argtypes = [ctypes.c_void_p]
        kernel32.LocalFree.restype = ctypes.c_void_p
        buffer = ctypes.create_string_buffer(ciphertext)
        input_blob = Blob(len(ciphertext), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_ubyte)))
        output_blob = Blob()
        if not crypt32.CryptUnprotectData(ctypes.byref(input_blob), None, None, None, None, 1, ctypes.byref(output_blob)):
            return None
        try:
            if output_blob.cbData > 131072:
                return None
            return ctypes.string_at(output_blob.pbData, output_blob.cbData).decode("utf-8")
        finally:
            kernel32.LocalFree(ctypes.cast(output_blob.pbData, ctypes.c_void_p))
    except (OSError, ValueError, UnicodeError, AttributeError):
        return None


def _qwen_console_capsule(home):
    capsule = load_json(home / ".ccs" / "account-usage" / "qwen-console-session.json")
    if not isinstance(capsule, dict) or capsule.get("region") not in ("intl", "cn"):
        return None
    version2 = capsule.get("version") == 2
    encoded = capsule.get("cookiesDPAPI" if version2 else "cookieDPAPI")
    if not isinstance(encoded, str) or not encoded or len(encoded) > (196608 if version2 else 65536):
        return None
    try:
        decrypted = _windows_unprotect(base64.b64decode(encoded, validate=True))
    except (ValueError, TypeError):
        return None
    if version2:
        try:
            packed = json.loads(decrypted) if isinstance(decrypted, str) else None
        except ValueError:
            return None
        if not isinstance(packed, dict) or not isinstance(packed.get("gatewayCookie"), str):
            return None
        cookie, gateway_cookie = packed.get("consoleCookie"), packed["gatewayCookie"]
    else:
        cookie, gateway_cookie = decrypted, None
    if not isinstance(cookie, str) or not cookie or len(cookie) > 32768 or any(char in cookie for char in ("\r", "\n", "\x00")):
        return None
    if gateway_cookie is not None and (len(gateway_cookie) > 32768 or any(char in gateway_cookie for char in ("\r", "\n", "\x00"))):
        return None
    return cookie, capsule["region"], gateway_cookie


def _safari_qwen_cookies(contents, allowed_domains, now=None):
    """Read scoped cookies from bounded Safari binarycookies bytes in memory.

    Format: interstateone/BinaryCookies. Record offsets are relative to their
    own page/record; expiry is a little-endian double since 2001-01-01. Zero
    expiry denotes a session cookie and does not invent a lifetime for it.
    """
    if len(contents) > 16 * MAX_BYTES or len(contents) < 8 or contents[:4] != b"cook":
        return []
    now = time.time() if now is None else now
    page_count = struct.unpack_from(">I", contents, 4)[0]
    if page_count > 4096 or 8 + page_count * 4 > len(contents):
        return []
    position = 8 + page_count * 4
    result = []
    total_records = 0
    try:
        for page_index in range(page_count):
            page_size = struct.unpack_from(">I", contents, 8 + page_index * 4)[0]
            if page_size < 12 or position + page_size > len(contents):
                return []
            page = contents[position:position + page_size]
            position += page_size
            if page[:4] != b"\x00\x00\x01\x00":
                return []
            count = struct.unpack_from("<I", page, 4)[0]
            total_records += count
            header_end = 12 + count * 4
            if count > 10000 or total_records > 20000 or header_end > len(page) or page[header_end - 4:header_end] != b"\x00" * 4:
                return []
            seen_offsets = set()
            for index in range(count):
                offset = struct.unpack_from("<I", page, 8 + index * 4)[0]
                if offset in seen_offsets or offset < header_end or offset + 56 > len(page):
                    return []
                seen_offsets.add(offset)
                size = struct.unpack_from("<I", page, offset)[0]
                if size < 56 or offset + size > len(page):
                    return []
                record = page[offset:offset + size]

                def read_string(field_offset):
                    string_offset = struct.unpack_from("<I", record, field_offset)[0]
                    if string_offset < 56 or string_offset >= len(record):
                        raise ValueError()
                    end = record.find(b"\x00", string_offset)
                    if end < 0 or end - string_offset > 8192:
                        raise ValueError()
                    return record[string_offset:end].decode("utf-8")

                raw_domain = read_string(16).lower()
                domain = raw_domain.lstrip(".")
                if domain not in allowed_domains:
                    continue
                expiry = struct.unpack_from("<d", record, 40)[0]
                if not math.isfinite(expiry) or expiry < 0 or (expiry != 0 and expiry + 978307200 <= now):
                    continue
                name, path, value = read_string(20), read_string(24), read_string(28)
                if not re.fullmatch(r"[!#$%&'*+\-.^_`|~0-9A-Za-z]{1,256}", name) or not value:
                    continue
                if any(char in value for char in (";", "\r", "\n", "\x00")):
                    continue
                if not path.startswith("/") or not any(request_path.startswith(path.rstrip("/") + "/") or request_path == path
                                                       for request_path in ("/tool/user/info.json", "/data/api.json", "/cn-beijing")):
                    continue
                result.append({"name": name, "value": value, "domain": domain, "path": path, "expiry": expiry,
                               "hostOnly": not raw_domain.startswith(".")})
    except (ValueError, UnicodeError, struct.error):
        return []
    return result


def _safari_qwen_candidates(home):
    # No OS permission prompts or cookie copying. Read only the standard file.
    path = home / "Library" / "Containers" / "com.apple.Safari" / "Data" / "Library" / "Cookies" / "Cookies.binarycookies"
    try:
        with path.open("rb") as handle:
            contents = handle.read(16 * MAX_BYTES + 1)
    except OSError:
        return []
    domains = {"intl": {"qwencloud.com", "home.qwencloud.com", "cs-data.qwencloud.com"},
               "cn": {"aliyun.com", "console.aliyun.com", "bailian.console.aliyun.com", "bailian-cs.console.aliyun.com"}}
    result = []
    for region, allowed in domains.items():
        cookies = _safari_qwen_cookies(contents, allowed)
        cookies.sort(key=lambda item: (-len(item["domain"]), -len(item["path"]), -item["expiry"]))
        def header_for(host, request_path):
            selected = {}
            for cookie in cookies:
                domain_matches = host == cookie["domain"] or (not cookie["hostOnly"] and host.endswith("." + cookie["domain"]))
                path = cookie["path"]
                path_matches = request_path == path or request_path.startswith(path.rstrip("/") + "/")
                if domain_matches and path_matches:
                    selected.setdefault(cookie["name"], cookie["value"])
            return "; ".join(name + "=" + value for name, value in selected.items())

        console_host = "home.qwencloud.com" if region == "intl" else "bailian.console.aliyun.com"
        gateway_host = "cs-data.qwencloud.com" if region == "intl" else "bailian-cs.console.aliyun.com"
        header = header_for(console_host, "/tool/user/info.json" if region == "intl" else "/cn-beijing")
        gateway_header = header_for(gateway_host, "/data/api.json")
        if not header or len(header) > 32768:
            continue
        if len(gateway_header) > 32768:
            continue
        packed = {"cookie": header, "gatewayCookie": gateway_header, "baseUrl": "https://token-plan.{}.maas.aliyuncs.com/compatible-mode/v1".format(
            "ap-southeast-1" if region == "intl" else "cn-beijing")}
        result.append({"secret": json.dumps(packed), "source": "Safari console", "email": None, "expires": None})
    return result


def credentials(provider, home=None):
    """Resolve only fixed, existing user-owned stores. Never modify auth state."""
    home = Path.home() if home is None else Path(home)
    candidates = []
    opencode = load_json(home / ".local" / "share" / "opencode" / "auth.json")
    aliases = {"kimi-code": ("kimi-for-coding", "kimi-code"), "qwen": ("alibaba-token-plan",),
               "zai": ("zai", "zai-coding-plan"), "opencode-go": ("opencode-go",)}
    if isinstance(opencode, dict):
        for name in aliases[provider]:
            item = opencode.get(name)
            if isinstance(item, dict) and item.get("type") == "api" and _string(item.get("key")):
                candidates.append({"secret": item["key"], "source": "OpenCode", "email": None, "expires": None})

    db_path = home / ".omp" / "agent" / "agent.db"
    omp_provider = "alibaba-token-plan" if provider == "qwen" else provider
    if db_path.is_file():
        try:
            with closing(sqlite3.connect(db_path.resolve().as_uri() + "?mode=ro", uri=True, timeout=0.3)) as db:
                db.execute("PRAGMA query_only=ON")
                rows = db.execute(
                    "SELECT credential_type,data FROM auth_credentials "
                    "WHERE provider=? AND disabled_cause IS NULL ORDER BY id DESC LIMIT 20",
                    (omp_provider,),
                ).fetchall()
                for credential_type, contents in rows:
                    if not isinstance(contents, str) or len(contents) > MAX_BYTES:
                        continue
                    try:
                        data = json.loads(contents)
                    except ValueError:
                        continue
                    if not isinstance(data, dict):
                        continue
                    secret = _string(data.get("key")) or _string(data.get("access"))
                    if secret:
                        candidates.append({
                            "secret": secret, "source": "OMP", "email": email_address(data.get("email")),
                            "expires": number(data.get("expires")) if provider == "kimi-code" and credential_type == "oauth" else None,
                        })
        except (OSError, sqlite3.Error):
            pass

    if provider == "qwen":
        qwen = load_json(home / ".qwen" / "settings.json")
        env = qwen.get("env") if isinstance(qwen, dict) else None
        secret = _string(env.get("BAILIAN_TOKEN_PLAN_API_KEY")) if isinstance(env, dict) else None
        if secret:
            candidates.append({"secret": secret, "source": "Qwen Code", "email": None, "expires": None})

    env_names = {"kimi-code": ("KIMI_CODE_API_KEY", "KIMI_API_KEY"), "qwen": ("BAILIAN_TOKEN_PLAN_API_KEY",),
                 "zai": ("ZAI_API_KEY", "ZHIPU_API_KEY"), "opencode-go": ("OPENCODE_GO_API_KEY", "OPENCODE_API_KEY")}
    for name in env_names[provider]:
        secret = _string(os.environ.get(name))
        if secret:
            candidates.append({"secret": secret, "source": "User environment", "email": None, "expires": None})
    if provider == "qwen":
        candidates[0:0] = _safari_qwen_candidates(home)
        capsule = _qwen_console_capsule(home)
        if capsule:
            cookie, region, gateway_cookie = capsule
            # Quota authentication uses the console session, not inference key.
            # Keep any existing key local when constructing OMP's packed format.
            token = candidates[0]["secret"] if candidates else None
            if token:
                try:
                    packed = json.loads(token)
                    token = packed.get("token") if isinstance(packed, dict) else None
                except ValueError:
                    pass
            packed = {"cookie": cookie, "baseUrl": "https://token-plan.{}.maas.aliyuncs.com/compatible-mode/v1".format(
                "ap-southeast-1" if region == "intl" else "cn-beijing")}
            if _string(token):
                packed["token"] = token
            if gateway_cookie is not None:
                packed["gatewayCookie"] = gateway_cookie
            candidates.insert(0, {"secret": json.dumps(packed), "source": "Brave console", "email": None, "expires": None})
    return candidates


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class HttpClient:
    def __init__(self, deadline_seconds=7):
        self.deadline = time.monotonic() + deadline_seconds
        self.opener = urllib.request.build_opener(_NoRedirect())

    def get(self, url, headers=None, body=None, text=False):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise CollectionError("error", "The usage service timed out.")
        request_headers = {"Accept": "application/json", "User-Agent": "CCS-Accounts/1.0"}
        request_headers.update(headers or {})
        request = urllib.request.Request(url, data=body, headers=request_headers)
        try:
            with self.opener.open(request, timeout=min(4, remaining)) as response:
                chunks = []
                size = 0
                while size <= MAX_BYTES:
                    if time.monotonic() >= self.deadline:
                        raise CollectionError("error", "The usage service timed out.")
                    # read1 performs at most one underlying read, so a slowly
                    # streaming body cannot postpone the overall deadline.
                    chunk = response.read1(min(65536, MAX_BYTES + 1 - size))
                    if not chunk:
                        break
                    chunks.append(chunk)
                    size += len(chunk)
                contents = b"".join(chunks)
            if len(contents) > MAX_BYTES:
                raise CollectionError("error", "The usage service returned an oversized response.")
            if text:
                return contents.decode("utf-8")
            result = json.loads(contents)
            if not isinstance(result, dict):
                raise ValueError()
            return result
        except urllib.error.HTTPError as error:
            if error.code == 401:
                raise CollectionError("needs_sign_in", "The saved credential was rejected by the usage service.")
            if error.code == 403:
                raise CollectionError("needs_sign_in", "The saved account cannot access this plan's usage.")
            if error.code == 429:
                raise CollectionError("error", "The usage service asked to slow down; try again later.")
            raise CollectionError("error", "The usage service could not complete the request.")
        except CollectionError:
            raise
        except (OSError, ValueError, UnicodeError, urllib.error.URLError):
            raise CollectionError("error", "The usage service is unavailable or returned an invalid response.")
