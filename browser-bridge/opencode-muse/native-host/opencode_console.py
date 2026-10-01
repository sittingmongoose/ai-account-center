"""OpenCode console GET-only workspace wallet reader.

This account is deliberately distinct from a key-authenticated Go account:
the usage key does not expose a workspace identifier. Secrets stay within the
owning OS user and requests to the fixed HTTPS console origin.
"""
import datetime as dt
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import tempfile
import time

ORIGIN = "https://opencode.ai"
MAX_BYTES = 1024 * 1024
MAX_COOKIES = 20
CAPSULE_NAME = "opencode-console-session.json"
WORKSPACE = re.compile(r"(?:wrk_|org_)[A-Za-z0-9_-]{1,128}\Z")
COOKIE_NAMES = {"auth", "__Host-console_session"}


class Unavailable(Exception):
    def __init__(self, code):
        self.code = code


def now_iso():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def iso(value):
    if value is None:
        return None
    if not isinstance(value, str) or len(value) > 64:
        return None
    try:
        result = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if result.tzinfo is None or not 2000 <= result.year <= 2200:
            return None
        return result.astimezone(dt.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except (ValueError, OverflowError):
        return None


def validate_cookies(values, now=None):
    if not isinstance(values, list) or not 0 < len(values) <= MAX_COOKIES:
        raise Unavailable("no_browser_cookie")
    now = time.time() if now is None else now
    result = []
    seen = set()
    for row in values:
        if not isinstance(row, dict) or set(row) - {"name", "value", "domain", "path", "secure", "hostOnly", "expirationDate"}:
            raise Unavailable("invalid_request")
        name, value = row.get("name"), row.get("value")
        domain, path = row.get("domain"), row.get("path")
        if (name not in COOKIE_NAMES or domain not in {"opencode.ai", ".opencode.ai"} or path != "/"
                or row.get("secure") is not True or type(row.get("hostOnly")) is not bool
                or not isinstance(value, str) or not 0 < len(value) <= 8192
                or re.search(r"[;\x00-\x20\x7f]", value)):
            raise Unavailable("invalid_request")
        if name == "__Host-console_session" and (domain != "opencode.ai" or not row["hostOnly"]):
            raise Unavailable("invalid_request")
        expiration = row.get("expirationDate")
        if expiration is not None and (not finite(expiration) or expiration <= now):
            continue
        if name in seen:
            raise Unavailable("invalid_request")
        seen.add(name)
        safe = {key: row[key] for key in ("name", "value", "domain", "path", "secure", "hostOnly")}
        if expiration is not None:
            safe["expirationDate"] = expiration
        result.append(safe)
    if not result:
        raise Unavailable("no_browser_cookie")
    return result


def cookie_header(values):
    return "; ".join(row["name"] + "=" + row["value"] for row in values)


class ConsoleClient:
    def __init__(self, cookies):
        self.header = cookie_header(cookies)
        self.deadline = time.monotonic() + 20

    def get(self, route, workspace=None):
        if route not in {"/console/api/orgs", "/console/api/billing/status", "/console/api/go/status"}:
            raise Unavailable("invalid_request")
        if workspace is not None and (not isinstance(workspace, str) or not WORKSPACE.fullmatch(workspace)):
            raise Unavailable("invalid_request")
        if (route == "/console/api/orgs") != (workspace is None):
            raise Unavailable("invalid_request")
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise Unavailable("network_error")
        from curl_cffi import requests
        headers = {"Cookie": self.header, "Accept": "application/json", "Origin": ORIGIN,
                   "Referer": ORIGIN + "/console/"}
        if workspace:
            headers["x-org-id"] = workspace
        try:
            response = requests.get(ORIGIN + route, headers=headers, impersonate="chrome", verify=True,
                                    allow_redirects=False, timeout=min(8, remaining), stream=True)
            try:
                if response.status_code in (401, 403):
                    raise Unavailable("needs_sign_in")
                if response.status_code != 200:
                    raise Unavailable("provider_error")
                body = bytearray()
                for part in response.iter_content(chunk_size=65536):
                    body.extend(part)
                    if len(body) > MAX_BYTES or time.monotonic() >= self.deadline:
                        raise Unavailable("network_error")
                return json.loads(body)
            finally:
                response.close()
        except Unavailable:
            raise
        except Exception:
            raise Unavailable("network_error")


def select_workspace(payload, requested):
    if not isinstance(payload, list) or not 0 < len(payload) <= 100:
        raise Unavailable("invalid_response")
    ids = []
    for row in payload:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str) or not WORKSPACE.fullmatch(row["id"]):
            raise Unavailable("invalid_response")
        if row["id"] in ids:
            raise Unavailable("invalid_response")
        ids.append(row["id"])
    if requested is not None:
        if not isinstance(requested, str) or not WORKSPACE.fullmatch(requested) or requested not in ids:
            raise Unavailable("workspace_mismatch")
        return requested
    if len(ids) != 1:
        raise Unavailable("choose_workspace")
    return ids[0]


def balance_window(payload):
    if not isinstance(payload, dict):
        raise Unavailable("invalid_response")
    mode, billing = payload.get("mode"), payload.get("billingMode")
    if mode not in {"pay-as-you-go", "invoiceable"} or billing not in {"prepaid", "legacy", "seat", "credit"}:
        raise Unavailable("invalid_response")
    if mode != "pay-as-you-go" or billing != "prepaid":
        raise Unavailable("wallet_not_prepaid")
    raw = payload.get("balanceMicroCents")
    if not isinstance(raw, str) or not re.fullmatch(r"-?[0-9]{1,20}", raw):
        raise Unavailable("invalid_response")
    balance = int(raw) / 100000000
    # The published provider schema uses signed micro-cents. Retain real debt.
    expiration = next((iso(payload.get(key)) for key in ("expiresAt", "expires_at", "expiration")
                       if iso(payload.get(key)) is not None), None)
    return {"key": "zen-balance", "label": "Zen balance", "kind": "balance", "usedPercent": None,
            "remainingPercent": None, "used": None, "limit": None, "remaining": balance,
            "unit": "USD", "resetAt": None, "expiresAt": expiration, "windowMinutes": None}


def go_windows(payload):
    if not isinstance(payload, dict) or not isinstance(payload.get("access"), dict):
        return []
    access = payload["access"]
    meters = access.get("meters")
    if not isinstance(meters, dict):
        return []
    windows = []
    for key, label, minutes in (("fiveHour", "5 hours", 300), ("week", "Weekly", 10080), ("month", "Monthly", None)):
        meter = meters.get(key)
        if not isinstance(meter, dict):
            continue
        used, limit = meter.get("usedMicroCents"), meter.get("limitMicroCents")
        if (not isinstance(used, str) or not isinstance(limit, str) or not re.fullmatch(r"[0-9]{1,20}", used)
                or not re.fullmatch(r"[0-9]{1,20}", limit) or int(limit) <= 0):
            continue
        percent = int(used) * 100 / int(limit)
        if not finite(percent) or percent < 0:
            continue
        reset = iso(meter.get("resetsAt"))
        if key == "month" and reset is None:
            reset = iso(access.get("endsAt"))
        windows.append({"key": "console-" + key, "label": label, "kind": "rate_limit",
                        "usedPercent": percent, "remainingPercent": max(0, 100 - percent), "used": None,
                        "limit": None, "remaining": None, "unit": None, "resetAt": reset,
                        "expiresAt": None, "windowMinutes": minutes})
    return windows


def collect(cookies, requested=None, client=None):
    cookies = validate_cookies(cookies)
    client = client or ConsoleClient(cookies)
    workspace = select_workspace(client.get("/console/api/orgs"), requested)
    wallet = balance_window(client.get("/console/api/billing/status", workspace))
    windows = []
    try:
        windows = go_windows(client.get("/console/api/go/status", workspace))
    except Unavailable:
        pass
    windows.append(wallet)
    checked = now_iso()
    suffix = hashlib.sha256(workspace.encode()).hexdigest()[:12]
    sample = {"id": "plan-opencode-go-console-mac-" + suffix, "provider": "opencode-go",
              "providerLabel": "OpenCode Go", "label": "OpenCode console wallet", "email": None,
              "plan": "Go" if len(windows) > 1 else None, "platform": "mac",
              "source": "Authenticated OpenCode console workspace on Mac", "status": "ok",
              "message": "Console workspace; API-key account identity has not been linked.",
              "fetchedAt": checked, "sampledAt": checked, "isActive": False, "windows": windows,
              "capabilities": {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []}}
    return workspace, sample


def private_directory(root):
    root = Path(root)
    if root.exists() and (root.is_symlink() or not root.is_dir()):
        raise Unavailable("local_storage_error")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    metadata = root.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise Unavailable("local_storage_error")
    return root


def write_capsule(root, cookies, workspace):
    root = private_directory(root)
    path = root / CAPSULE_NAME
    if path.exists() and (path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
                          or stat.S_IMODE(path.stat().st_mode) & 0o077):
        raise Unavailable("local_storage_error")
    # Two-hour bridge refresh keeps expiring browser cookies current. The
    # capsule never outlives its actual cookie expiration; no auth refresh here.
    capsule = {"schemaVersion": 1, "origin": ORIGIN, "source": "browser-native-cookie-api",
               "workspaceId": workspace, "capturedAt": now_iso(), "cookies": cookies}
    handle, temporary = tempfile.mkstemp(prefix=".opencode-session-", dir=root)
    try:
        os.fchmod(handle, 0o600)
        with os.fdopen(handle, "w") as file:
            json.dump(capsule, file, separators=(",", ":"), allow_nan=False)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def read_capsule(root):
    root = private_directory(root)
    path = root / CAPSULE_NAME
    metadata = path.lstat()
    if (stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) & 0o077 or metadata.st_size > MAX_BYTES):
        raise Unavailable("local_storage_error")
    with path.open("rb") as file:
        payload = json.loads(file.read(MAX_BYTES + 1))
    if (not isinstance(payload, dict) or payload.get("schemaVersion") != 1 or payload.get("origin") != ORIGIN
            or payload.get("source") != "browser-native-cookie-api"):
        raise Unavailable("local_storage_error")
    captured = iso(payload.get("capturedAt"))
    if captured is None:
        raise Unavailable("local_storage_error")
    age = time.time() - dt.datetime.fromisoformat(captured.replace("Z", "+00:00")).timestamp()
    if age < -60 or age > 24 * 3600:
        raise Unavailable("needs_sign_in")
    cookies = validate_cookies(payload.get("cookies"))
    requested = payload.get("workspaceId")
    if not isinstance(requested, str) or not WORKSPACE.fullmatch(requested):
        raise Unavailable("local_storage_error")
    return cookies, requested
