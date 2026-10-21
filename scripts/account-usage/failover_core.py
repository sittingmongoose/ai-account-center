"""Read-only, identity-bound Codex and Claude quota sources.

Inventory does not contact providers. Credentials stay in private ``_`` fields
until collection on their owning machine; callers publish only the whitelisted
inventory fields. No sign-in, refresh, activation, or credential write occurs.
"""

import base64
import ctypes
import datetime as dt
from email.utils import parsedate_to_datetime
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import sys
import time
import urllib.error
import urllib.request

import claude_usage
from desktop_helpers import attribute_reads, email, read_failed, safe_text, secret, quota_window

MAX_BYTES = 1024 * 1024
MAX_SOURCES = 64
MAX_FILES = 256
HTTP_SECONDS = 12
UTC = dt.timezone.utc
SAFE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")
UUID = re.compile(claude_usage.UUID)
CODEX_URL = "https://chatgpt.com/backend-api/wham/usage"
CLAUDE_PROFILE_URL = "https://api.anthropic.com/api/oauth/profile"
CLAUDE_USAGE_URL = "https://api.anthropic.com/api/oauth/usage"


class Failure(Exception):
    def __init__(self, code="unavailable", retry_after=None):
        super().__init__(code)
        self.code = code
        self.retry_after = retry_after


class SourceInventory(list):
    """Private side channel for known signed-out sources, without inventing identities."""
    def __init__(self, values=(), sign_in_needed_selectors=()):
        super().__init__(values)
        self.sign_in_needed_selectors = list(sign_in_needed_selectors)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     ensure_ascii=True, allow_nan=False).encode("ascii")).hexdigest()


def _object(value):
    return value if isinstance(value, dict) else {}


def _number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) else None


def _id(value):
    return value if isinstance(value, str) and value == value.strip() and 0 < len(value) <= 512 and not re.search(r"[\x00-\x20\x7f]", value) else None


def _uuid(value):
    return value.lower() if isinstance(value, str) and UUID.fullmatch(value) else None


def _within(root, path):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _local_path(home, value):
    """Follow only bounded links whose written targets stay inside this home.

    Do not stat a UNC/network link target: offline mounts can outlive a process
    deadline. T3 shadow auth links into another local account home are allowed.
    """
    root = Path(os.path.abspath(home))
    path = Path(os.path.abspath(value))
    for _ in range(12):
        if not _within(root, path):
            return None
        current = root
        parts = path.relative_to(root).parts
        redirected = False
        for index, part in enumerate(parts):
            current = current / part
            try:
                info = current.lstat()
            except (FileNotFoundError, NotADirectoryError):
                return None
            except OSError:
                read_failed()
                return None
            reparse = getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024)
            if stat.S_ISLNK(info.st_mode) or reparse:
                try:
                    target = os.readlink(current)
                except OSError:
                    read_failed()
                    return None
                if target.startswith("\\\\?\\UNC\\") or target.startswith("//"):
                    return None
                if target.startswith("\\\\?\\"):
                    target = target[4:]
                if target.startswith("\\\\"):
                    return None
                path = Path(os.path.abspath(current.parent / target / Path(*parts[index + 1:])))
                redirected = True
                break
        if not redirected:
            return current
    return None


def _read_bytes(home, value, max_bytes=MAX_BYTES):
    path = _local_path(home, value)
    if path is None:
        return None
    try:
        raw = _read_present(path, max_bytes)
    except (FileNotFoundError, NotADirectoryError):
        return None
    except (OSError, ValueError, UnicodeError):
        raw = None
    if raw is None:
        # Present but refused or unreadable: not proof that the sign-in was removed.
        read_failed()
    return raw


def _read_present(path, max_bytes):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > max_bytes:
        return None
    if hasattr(os, "getuid") and info.st_uid != os.getuid():
        return None
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    with os.fdopen(descriptor, "rb") as handle:
        actual = os.fstat(handle.fileno())
        if (actual.st_dev, actual.st_ino) != (info.st_dev, info.st_ino):
            return None
        raw = handle.read(max_bytes + 1)
    return raw if len(raw) <= max_bytes else None


def _read(home, value):
    raw = _read_bytes(home, value)
    if raw is None:
        return None
    try:
        value = json.loads(raw.decode("utf-8-sig"))
        if isinstance(value, dict):
            return value
    except (ValueError, UnicodeError):
        pass
    read_failed()
    return None


def _children(home, value):
    root = _local_path(home, value)
    if root is None:
        return []
    try:
        # Bound directory enumeration as well as the number of returned homes.
        with os.scandir(root) as iterator:
            entries = []
            for entry in iterator:
                if len(entries) >= MAX_FILES:
                    break
                entries.append(entry.name)
        return [root / name for name in sorted(entries) if SAFE_NAME.fullmatch(name)][:MAX_SOURCES]
    except (FileNotFoundError, NotADirectoryError):
        return []
    except OSError:
        read_failed()
        return []


def _settings_homes(home, provider):
    state = home / ".t3/userdata"
    settings = _read(home, state / "settings.json") or {}
    instances = _object(settings.get("providerInstances"))
    result = []
    for instance in list(instances.values())[:MAX_SOURCES]:
        item = _object(instance)
        if item.get("driver") != ("codex" if provider == "codex" else "claudeAgent"):
            continue
        config = _object(item.get("config"))
        fields = ("homePath", "shadowHomePath") if provider == "codex" else ("homePath",)
        for field in fields:
            value = config.get(field)
            if not isinstance(value, str) or len(value) > 1024 or "\x00" in value:
                continue
            if value == "~":
                value = str(home)
            elif value.startswith(("~/", "~\\")):
                value = str(home / value[2:])
            if Path(value).is_absolute() and _within(home, Path(os.path.abspath(value))):
                result.append(Path(value))
    return result


def _roots(home, provider, ccs_dir):
    default = home / (".codex" if provider == "codex" else ".claude")
    result = [(default, ["discover", provider + "-profile:default"])]
    saved = ccs_dir / ("codex-instances" if provider == "codex" else "instances")
    profiles = attribute_reads([provider + "-profile:*"], lambda: _children(ccs_dir, saved))
    result.extend((root, [provider + "-profile:" + root.name]) for root in profiles)
    def t3_roots():
        found = _children(home, home / (".codex-t3" if provider == "codex" else ".claude-t3"))
        if provider == "codex":
            found.extend(root / "shadow" for root in _children(home, home / ".t3/userdata/providers/codex"))
        return found + _settings_homes(home, provider)
    result.extend((root, ["t3"]) for root in attribute_reads(["t3"], t3_roots))
    return result[:MAX_FILES]


def _jwt(value):
    if not isinstance(value, str) or len(value) > 32768 or value.count(".") != 2:
        return {}
    try:
        parts = value.split(".")
        if any(not re.fullmatch(r"[A-Za-z0-9_-]+", part) or len(part) % 4 == 1 for part in parts):
            return {}
        if not isinstance(json.loads(base64.urlsafe_b64decode(parts[0] + "=" * (-len(parts[0]) % 4))), dict):
            return {}
        part = parts[1]
        return _object(json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))))
    except (ValueError, UnicodeError):
        return {}


def _codex_identity(tokens):
    payload = _jwt(tokens.get("id_token"))
    auth = _object(payload.get("https://api.openai.com/auth"))
    account = _id(auth.get("chatgpt_account_id"))
    address = email(payload.get("email") or _object(payload.get("https://api.openai.com/profile")).get("email"))
    if not account or not address or tokens.get("account_id") not in (None, account):
        return None
    fields = [auth.get("chatgpt_user_id"), auth.get("user_id"), payload.get("sub"), payload.get("iss")]
    if any(value is not None and _id(value) is None for value in fields):
        return None
    if fields[0] and fields[1] and fields[0] != fields[1]:
        return None
    user, subject, issuer = fields[0] or fields[1], fields[2], fields[3]
    # As in matchesCodexActivationIdentity, a workspace is not a person. Retain
    # every known principal field; a sparse login cannot downgrade that binding.
    binding = [account, user, subject, issuer, None if user or (subject and issuer) else address]
    access = _jwt(tokens.get("access_token"))
    access_auth = _object(access.get("https://api.openai.com/auth"))
    for key, expected in (("chatgpt_account_id", account), ("chatgpt_user_id", user), ("user_id", user)):
        if key in access_auth and expected and access_auth[key] != expected:
            return None
    for key, expected in (("sub", subject), ("iss", issuer)):
        if key in access and expected and access[key] != expected:
            return None
    return {"binding": binding, "account": account, "email": address,
            "plan": safe_text(auth.get("chatgpt_plan_type"), 64)}


def _candidate(provider, home, storage, selectors, binding, revision, address, plan, **private):
    return {
        "locator": digest([provider, str(storage).replace("\\", "/")]),
        "selectors": selectors,
        "identity": {"kind": "codex-principal-v1" if provider == "codex" else "claude-account-org-v1",
                     "key": digest([provider, binding])},
        "revision": digest(revision), "email": address, "plan": plan,
        "_provider": provider, **{"_" + key: value for key, value in private.items()},
    }


def _codex_candidates(home, ccs_dir):
    result = []
    for root, selectors in _roots(home, "codex", ccs_dir):
        storage = root / "auth.json"
        raw = attribute_reads(selectors, lambda: _read(home if _within(home, storage) else ccs_dir, storage)) or {}
        tokens = _object(raw.get("tokens"))
        identity = _codex_identity(tokens)
        token = secret(tokens.get("access_token"))
        if not identity or not token:
            continue
        result.append(_candidate("codex", home, storage, selectors, identity["binding"],
                                 [tokens, identity["binding"]], identity["email"], identity["plan"],
                                 token=token, account=identity["account"], mode="cli"))
    return result


def _mac_default_keychain():
    """Existing default Claude CLI keychain item, with UI explicitly disabled.

    Custom service names and Desktop Safe Storage are deliberately not guessed.
    ``security -w`` can prompt, so use Security's noninteractive API instead.
    """
    if sys.platform != "darwin":
        return None
    try:
        security = ctypes.CDLL("/System/Library/Frameworks/Security.framework/Security")
        security.SecKeychainSetUserInteractionAllowed.argtypes = [ctypes.c_bool]
        security.SecKeychainSetUserInteractionAllowed.restype = ctypes.c_int32
        if security.SecKeychainSetUserInteractionAllowed(False) != 0:
            return None
        find = security.SecKeychainFindGenericPassword
        find.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_char_p, ctypes.c_uint32,
                         ctypes.c_char_p, ctypes.POINTER(ctypes.c_uint32),
                         ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p]
        find.restype = ctypes.c_int32
        security.SecKeychainItemFreeContent.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        service = b"Claude Code-credentials"
        length, pointer = ctypes.c_uint32(), ctypes.c_void_p()
        status_code = find(None, len(service), service, 0, None, ctypes.byref(length), ctypes.byref(pointer), None)
        if status_code != 0:
            return None
        try:
            if not 0 < length.value <= MAX_BYTES:
                return None
            return _object(json.loads(ctypes.string_at(pointer, length.value)))
        finally:
            security.SecKeychainItemFreeContent(None, pointer)
    except (OSError, ValueError, UnicodeError, AttributeError):
        return None


def _claude_cli_candidates(home, platform, ccs_dir):
    result = []
    for root, selectors in _roots(home, "claude", ccs_dir):
        default = root == home / ".claude"
        anchor = home if _within(home, root) else ccs_dir
        metadata = attribute_reads(selectors, lambda: _read(
            anchor, home / ".claude.json" if default else root / ".claude.json")) or {}
        account = _object(metadata.get("oauthAccount"))
        account_id, org_id = _uuid(account.get("accountUuid")), _uuid(account.get("organizationUuid"))
        address = email(account.get("emailAddress"))
        if not account_id or not org_id or not address:
            continue
        sources = [(root / ".credentials.json",
                    attribute_reads(selectors, lambda: _read(anchor, root / ".credentials.json")))]
        if default and platform == "mac":
            sources.append((root / "keychain-default", attribute_reads(selectors, _mac_default_keychain)))
        for storage, credentials in sources:
            oauth = _object(_object(credentials).get("claudeAiOauth"))
            token = secret(oauth.get("accessToken"))
            if not token:
                continue
            result.append(_candidate("claude", home, storage, selectors, [account_id, org_id],
                                     [oauth, account_id, org_id, address], address,
                                     safe_text(oauth.get("subscriptionType"), 64), token=token,
                                     account=account_id, organization=org_id, mode="cli",
                                     expires=_number(oauth.get("expiresAt"))))
    return result


def _configured_windows_profile_root(value):
    """An explicit private manifest may select another local drive, never a link or share."""
    try:
        if not value.is_absolute() or str(value).startswith(("\\\\", "//")):
            return None
        root = Path(os.path.abspath(value))
        for current in [*reversed(root.parents), root]:
            info = current.lstat()
            if (not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or
                    getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024)):
                return None
        for name in ("config.json", "Local State"):
            info = (root / name).lstat()
            if (not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode) or
                    getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 1024)):
                return None
        return root.resolve(strict=True)
    except (OSError, ValueError):
        return None


def _claude_desktop_candidates(home, platform, ccs_dir, claude_profile=None):
    if platform != "windows" or sys.platform != "win32":
        return []
    families = ["claude-profile:*", "claude-desktop-profile:*"]
    manifest = attribute_reads(families, lambda: _read(ccs_dir, ccs_dir / "claude-desktop-profiles.json")) or {}
    profiles = manifest.get("profiles") if manifest.get("version") == 1 else None
    profiles = profiles if isinstance(profiles, list) else []
    if claude_profile is not None:
        selected = {"id": claude_profile.get("id"), "email": claude_profile.get("email"),
                    "windows": {"isDefault": claude_profile.get("isDefault", False),
                                **({"profilePath": claude_profile["profilePath"]} if "profilePath" in claude_profile else {})}}
        # Only this selected server-owned entry overrides a remote copy. The
        # request is not written to the host's manifest or login storage.
        profiles = [selected] + [item for item in profiles if _object(item).get("id") != selected["id"]]
    roots, configured_roots = {}, set()
    for root in (claude_usage.windows_profile_path("default", home, True),
                 home / "AppData/Roaming/Claude"):
        roots[str(root)] = (root, ["discover", "claude-profile:default", "claude-desktop-profile:default"], None, "default")
    for root in attribute_reads(families, lambda: _children(home, home / "AppData/Roaming")):
        if root.name.startswith("Claude-") and SAFE_NAME.fullmatch(root.name[7:]):
            identifier = root.name[7:]
            roots[str(root)] = (root, ["claude-profile:" + identifier, "claude-desktop-profile:" + identifier], None, identifier)
    result = SourceInventory()
    for profile in profiles[:MAX_SOURCES]:
        profile = _object(profile)
        identifier, address = profile.get("id"), email(profile.get("email"))
        launcher = _object(profile.get("windows"))
        if not isinstance(identifier, str) or not SAFE_NAME.fullmatch(identifier) or not address or not launcher:
            continue
        supplied = launcher.get("profilePath")
        if supplied is None:
            root = claude_usage.windows_profile_path(identifier, home, launcher.get("isDefault") is True)
        elif isinstance(supplied, str) and len(supplied) <= 4096 and Path(supplied).is_absolute():
            root = Path(supplied)
            configured_roots.add(str(root))
        else:
            continue
        native = roots.get(str(root))
        selectors = sorted(set((native[1] if native else []) +
                               ["claude-profile:" + identifier, "claude-desktop-profile:" + identifier]))
        if launcher.get("isDefault") is True or root.name.lower() == "claude":
            selectors = sorted(set(selectors + ["claude-profile:default", "claude-desktop-profile:default"]))
        roots[str(root)] = (root, selectors, address, identifier)
    for root, selectors, address, identifier in list(roots.values())[:MAX_SOURCES]:
        attribute_reads(selectors, lambda: _desktop_root(
            home, root, selectors, address, identifier, configured_roots, result))
    return result


def _desktop_root(home, root, selectors, address, identifier, configured_roots, result):
    # Validate both files before the existing DPAPI reader opens them.
    if str(root) in configured_roots:
        root = _configured_windows_profile_root(root)
        if root is None:
            return
    elif _local_path(home, root / "config.json") is None or _local_path(home, root / "Local State") is None:
        return
    try:
        account_id, cache = claude_usage.decrypt_cache(root)
        entries = claude_usage.eligible_entries(account_id, cache, allow_expired=True)[:4]
        if not entries:
            result.sign_in_needed_selectors.extend(selector for selector in selectors if selector != "discover")
        for expires, identities, token in entries:
            account_id, org_id = _uuid(account_id), _uuid(identities[2])
            if not account_id or not org_id:
                continue
            result.append(_candidate("claude", home, root / ("oauth-" + digest(identities)),
                                     selectors, [account_id, org_id],
                                     [token, identities, address, expires], address, None, token=token,
                                     account=account_id, organization=org_id, profile=identifier,
                                     mode="desktop", expires=expires))
    except claude_usage.UsageUnavailable as error:
        if error.status == "needs_sign_in":
            result.sign_in_needed_selectors.extend(selector for selector in selectors if selector != "discover")
    except Exception:
        # Present but unreadable (DPAPI, cache format): not a removed sign-in.
        read_failed()


def inventory(provider, home, platform, ccs_dir=None, claude_profile=None):
    """Bounded local inventory; account names only select, never prove identity."""
    if provider not in ("codex", "claude") or platform not in ("ubuntu", "mac", "windows"):
        return []
    home = Path(os.path.abspath(home))
    ccs_dir = Path(os.path.abspath(ccs_dir)) if ccs_dir is not None else home / ".ccs"
    desktop = _claude_desktop_candidates(home, platform, ccs_dir, claude_profile) if provider == "claude" else []
    candidates = (_codex_candidates(home, ccs_dir) if provider == "codex" else
                  _claude_cli_candidates(home, platform, ccs_dir) + desktop)
    unique = {}
    for candidate in candidates:
        previous = unique.get(candidate["locator"])
        if previous:
            previous["selectors"] = sorted(set(previous["selectors"] + candidate["selectors"]))
        elif len(unique) < MAX_SOURCES:
            unique[candidate["locator"]] = candidate
    return SourceInventory(unique.values(), sorted(set(getattr(desktop, "sign_in_needed_selectors", []))))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _retry_after(value):
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


class Client:
    def __init__(self):
        self.deadline = time.monotonic() + HTTP_SECONDS
        self.opener = urllib.request.build_opener(_NoRedirect())

    def get(self, url, token, account=None):
        if url not in (CODEX_URL, CLAUDE_PROFILE_URL, CLAUDE_USAGE_URL):
            raise Failure()
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise Failure()
        headers = {"Authorization": "Bearer " + token, "Accept": "application/json"}
        if url == CODEX_URL:
            headers.update({"ChatGPT-Account-Id": account,
                            "User-Agent": "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal"})
        else:
            headers.update({"anthropic-beta": "oauth-2025-04-20", "User-Agent": "CCS-ReadOnly-Usage/1.0"})
        try:
            request = urllib.request.Request(url, headers=headers)
            with self.opener.open(request, timeout=min(5, remaining)) as response:
                chunks, size = [], 0
                while size <= MAX_BYTES:
                    if time.monotonic() >= self.deadline:
                        raise Failure()
                    chunk = response.read1(min(65536, MAX_BYTES + 1 - size))
                    if not chunk:
                        break
                    size += len(chunk)
                    chunks.append(chunk)
                if size > MAX_BYTES:
                    raise Failure("invalid_response")
                value = json.loads(b"".join(chunks))
                if not isinstance(value, dict):
                    raise Failure("invalid_response")
                return value
        except urllib.error.HTTPError as error:
            if error.code == 429:
                raise Failure("rate_limited", _retry_after((error.headers or {}).get("Retry-After"))) from None
            raise Failure("needs_sign_in" if error.code in (401, 403) else "unavailable") from None
        except (OSError, ValueError, UnicodeError, urllib.error.URLError):
            raise Failure() from None


def _codex_windows(payload, now):
    windows = []
    used_keys = set()

    def group(value, prefix="", title="Usage"):
        for position, camel, expected_seconds in (("primary_window", "primaryWindow", 18000),
                                                   ("secondary_window", "secondaryWindow", 604800)):
            window = _object(_object(value).get(position, _object(value).get(camel)))
            used = _number(window.get("used_percent", window.get("usedPercent")))
            if used is None or used < 0:
                continue
            seconds = _number(window.get("limit_window_seconds", window.get("limitWindowSeconds")))
            seconds = seconds if seconds is not None and seconds > 0 else expected_seconds
            cadence = "five_hour" if seconds == 18000 else "seven_day" if seconds == 604800 else position
            key = prefix + cadence
            if key in used_keys:
                continue
            used_keys.add(key)
            label = "5-hour usage" if seconds == 18000 else "Weekly usage" if seconds == 604800 else "Usage"
            if prefix:
                label = title + " - " + label
            raw_reset = _number(window.get("reset_at", window.get("resetAt")))
            reset = None
            if raw_reset is not None and raw_reset > 0:
                reset = raw_reset
            elif raw_reset is None:
                delay = _number(window.get("reset_after_seconds", window.get("resetAfterSeconds")))
                if delay is not None and delay >= 0:
                    reset = now + delay
            windows.append(quota_window(key, label, used_percent=used, reset=reset, minutes=seconds / 60))

    group(payload.get("rate_limit", payload.get("rateLimit")))
    group(payload.get("code_review_rate_limit", payload.get("codeReviewRateLimit")), "code_review_", "Code review")
    additional = payload.get("additional_rate_limits", payload.get("additionalRateLimits"))
    if isinstance(additional, list):
        for index, value in enumerate(additional[:16]):
            value = _object(value)
            title = safe_text(value.get("limit_name", value.get("limitName")), 64) or "Additional usage"
            group(value.get("rate_limit", value.get("rateLimit")), "additional_" + str(index) + "_", title)
    credits = _object(payload.get("credits"))
    balance = _number(credits.get("balance"))
    if isinstance(credits.get("balance"), str) and re.fullmatch(r"\d+(?:\.\d+)?", credits["balance"]):
        balance = float(credits["balance"])
    if balance is not None and balance >= 0 or isinstance(credits.get("unlimited"), bool):
        windows.append(quota_window("credits_balance", "Extra usage credits", kind="balance",
                                    remaining=balance, unit="credits", unlimited=credits.get("unlimited"),
                                    enabled=credits.get("has_credits")))
    resets = _object(payload.get("rate_limit_reset_credits"))
    count = _number(resets.get("available_count"))
    if count is not None and count >= 0 and count == int(count):
        windows.append(quota_window("banked_resets", "Banked resets", kind="balance", remaining=count, unit="resets"))
    monthly = _object(_object(payload.get("spend_control")).get("individual_limit"))
    used, limit = _number(monthly.get("used")), _number(monthly.get("limit"))
    remaining_percent = _number(monthly.get("remaining_percent"))
    if remaining_percent is not None and not 0 <= remaining_percent <= 100:
        remaining_percent = None
    if used is not None or limit is not None or remaining_percent is not None:
        windows.append(quota_window("monthly_credit_limit", "Monthly credit limit", kind="spend",
                                    used=used, limit=limit, remaining_percent=remaining_percent,
                                    reset=monthly.get("reset_at"), unit="credits",
                                    remaining=max(0, limit - used) if used is not None and limit is not None else None))
    return windows


def _sample(candidate, platform, windows, plan):
    provider = candidate["_provider"]
    label = "Codex" if provider == "codex" else "Claude"
    stamp = dt.datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    return {"id": "native:" + provider + ":" + platform, "provider": provider,
            "providerLabel": label, "label": label, "email": candidate["email"], "plan": plan,
            "platform": platform, "source": label + " verified quota on " +
            {"ubuntu": "Ubuntu", "mac": "Mac", "windows": "Windows"}[platform],
            "status": "ok", "message": None, "fetchedAt": stamp, "sampledAt": stamp,
            "isActive": False, "windows": windows,
            "capabilities": {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []}}


def _claude_optional_windows(candidate, home, cooldown=None):
    availability = {"resetCredits": "unavailable", "prepaidBalance": "unavailable"}
    extras = {"optionalExtras": availability}
    identifier = candidate.get("_profile")
    if not isinstance(identifier, str) or not SAFE_NAME.fullmatch(identifier):
        return [], extras
    capsule = Path(home) / ".ccs/claude-session-migration" / (identifier + "-source.dpapi")
    before = _read_bytes(home, capsule, claude_usage.MAX_CAPSULE_BYTES)
    if before is None:
        return [], extras
    context = digest(["claude-optional-v1", candidate["identity"], candidate["revision"],
                      identifier, hashlib.sha256(before).hexdigest()])
    try:
        windows = claude_usage.optional_web_windows(identifier, candidate["_account"],
                                                    candidate["_organization"], home,
                                                    availability, candidate["email"], cooldown)
    except claude_usage.UsageUnavailable as error:
        windows = []
        availability.update(resetCredits="unavailable", prepaidBalance="unavailable")
        if error.status == "rate_limited" and cooldown is not None:
            cooldown["failureCode"] = "rate_limited"
            if error.retry_after is not None:
                cooldown["retryAfterSeconds"] = error.retry_after
    except Exception:
        windows = []
        availability.update(resetCredits="unavailable", prepaidBalance="unavailable")
    if before != _read_bytes(home, capsule, claude_usage.MAX_CAPSULE_BYTES):
        return [], {"optionalExtras": {"resetCredits": "unavailable", "prepaidBalance": "unavailable"}}
    extras["optionalExtrasContext"] = context
    return windows, extras


def collect(candidate, home, platform, ccs_dir=None, claude_profile=None):
    """Collect only a revalidated candidate; provider rejection never becomes zero."""
    try:
        if platform not in ("ubuntu", "mac", "windows"):
            raise Failure()
        provider = candidate.get("_provider")
        client = Client()
        extras = {}
        cooldown = {}
        if provider == "codex":
            payload = client.get(CODEX_URL, candidate["_token"], candidate["_account"])
            windows = _codex_windows(payload, time.time())
            plan = safe_text(payload.get("plan_type", payload.get("planType")), 64) or candidate["plan"]
        elif provider == "claude":
            expires = _number(candidate.get("_expires"))
            if expires is not None and expires <= time.time() * 1000 + 30000:
                raise Failure("needs_sign_in")
            payload = client.get(CLAUDE_PROFILE_URL, candidate["_token"])
            account, org = _object(payload.get("account")), _object(payload.get("organization"))
            live_email = email(account.get("email"))
            if (not live_email or (candidate["email"] is not None and live_email != candidate["email"]) or
                    _uuid(account.get("uuid")) != candidate["_account"] or
                    _uuid(org.get("uuid")) != candidate["_organization"]):
                raise Failure("identity_mismatch")
            candidate = {**candidate, "email": live_email}
            payload = client.get(CLAUDE_USAGE_URL, candidate["_token"])
            windows = claude_usage.normalize_windows(payload)
            if not windows:
                raise Failure("invalid_response")
            plan = "max" if account.get("has_claude_max") is True else "pro" if account.get("has_claude_pro") is True else candidate["plan"]
            if platform == "windows" and candidate.get("_mode") == "desktop":
                optional_windows, extras = _claude_optional_windows(candidate, home, cooldown)
                windows.extend(optional_windows)
        else:
            raise Failure()
        if not windows:
            raise Failure("invalid_response")
        return {"sample": {**_sample(candidate, platform, windows, plan), **extras}, "failureCode": None, **cooldown}
    except Failure as error:
        return {"sample": None, "failureCode": error.code,
                **({"retryAfterSeconds": error.retry_after} if error.retry_after is not None else {})}
    except Exception:
        return {"sample": None, "failureCode": "unavailable"}
