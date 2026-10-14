"""Local-only credential inventory and selected usage reads for additional providers.

Public inventory fields are fingerprints and display metadata. Values prefixed
with an underscore belong only to this process and must never be serialized.
The dispatcher checks the public locator, identity and revision again before
calling collect; collection uses the exact in-memory credential it selected.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import time

import desktop_helpers as desktop
import desktop_usage as desktop_usage
import plan_common as plans
import plan_usage

PROVIDERS = {"muse", "cursor", "kimi-code", "qwen", "zai", "opencode-go"}
MAX_CANDIDATES = 128
HOST_LABELS = {"mac": "Mac", "windows": "Windows", "ubuntu": "Ubuntu"}


def _hash(kind, value):
    packed = json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(("aac-failover-v1:" + kind + ":" + packed).encode()).hexdigest()


def _json(path):
    return desktop.read_json(path) or {}


def _slots(directory, pattern):
    """Enumerate bounded, fixed store names; never follow a directory symlink."""
    directory = Path(directory)
    try:
        if directory.is_symlink() or not directory.is_dir():
            return []
        entries = sorted(directory.iterdir(), key=lambda value: value.name)
        return [match.group(1) for entry in entries[:512]
                if not entry.is_symlink() and (match := re.fullmatch(pattern, entry.name))][:MAX_CANDIDATES]
    except OSError:
        return []


def _omp(provider, home):
    path = Path(home) / ".omp/agent/agent.db"
    connection = None
    try:
        if not path.is_file() or path.is_symlink():
            return []
        connection = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=.3)
        connection.execute("PRAGMA query_only=ON")
        deadline = time.monotonic() + 2
        connection.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
        rows = connection.execute(
            "SELECT rowid,credential_type,data FROM auth_credentials WHERE provider=? "
            "AND disabled_cause IS NULL ORDER BY rowid DESC LIMIT ?", (provider, MAX_CANDIDATES)).fetchall()
        result = []
        for identifier, kind, raw in rows:
            if not isinstance(raw, str) or len(raw) > plans.MAX_BYTES:
                continue
            try:
                value = json.loads(raw)
            except (ValueError, TypeError):
                continue
            if isinstance(value, dict):
                result.append((str(identifier), kind, value))
        return result
    except (OSError, sqlite3.Error):
        return []
    finally:
        if connection is not None:
            connection.close()


def _cookie_header(value):
    if not isinstance(value, str) or not value or len(value) > 32768:
        raise ValueError()
    pairs, names = [], set()
    for fragment in value.split(";"):
        name, separator, content = fragment.strip().partition("=")
        if (not separator or not name or name in names or
                any(ord(character) < 32 or ord(character) == 127 for character in name + content)):
            raise ValueError()
        pairs.append((name, content))
        names.add(name)
    return sorted(pairs)


def _muse_context(ccs_dir, credential):
    """Only a verified, private local capsule names the quota's selected team."""
    import muse_console as muse
    address = desktop.email(credential.email)
    if not address:
        return None
    try:
        cookies, team, bound_email, plan = muse.read_capsule(ccs_dir / "account-usage", allow_expired=True)
        if bound_email.lower() != address.lower():
            return None
        return {"email": address.lower(), "team": team, "plan": plan,
                "session": muse.session_values(cookies)}
    except (muse.MuseError, OSError, ValueError, TypeError):
        return None


def _candidate(provider, locator, selectors, credential, muse_context=None):
    access = credential.access if isinstance(credential, desktop.Credential) else credential.get("secret")
    if not isinstance(access, str) or not access or len(access) > plans.MAX_BYTES:
        return None
    address = desktop.email(credential.email if isinstance(credential, desktop.Credential) else credential.get("email"))
    plan = desktop.safe_text(credential.plan, 48) if isinstance(credential, desktop.Credential) else None
    material = access
    if provider == "muse":
        if muse_context is not None:
            kind, identity = "muse-team", [muse_context["email"], muse_context["team"]]
            material = [access, muse_context]
        else:
            # /key authenticates this exact device credential but does not
            # expose a stable subscription/team id. Email cannot fill that gap.
            kind, identity = "muse-native-credential", access
    elif provider == "cursor":
        subject = desktop.jwt_claims(access).get("sub")
        if isinstance(subject, str) and 0 < len(subject) <= 256 and not re.search(r"[\x00-\x20\x7f]", subject):
            kind, identity = "cursor-subject", subject
        else:
            kind, identity = "cursor-credential", access
    elif provider == "qwen":
        try:
            cookie, region = plan_usage._qwen_credential(access)
            gateway = plan_usage._qwen_gateway_cookie(access, cookie)
            # An inference key embedded beside a cookie does not identify the
            # cookie's account. Only the actual quota sessions establish this.
            material = [region, _cookie_header(cookie), _cookie_header(gateway)]
        except (plans.CollectionError, ValueError, TypeError):
            return None
        kind, identity = "qwen-session", material
    else:
        kind, identity = provider + "-credential", access
    return {"locator": _hash(provider + ":locator", locator), "selectors": selectors,
            "identity": {"kind": kind, "key": _hash(kind, identity)},
            "revision": _hash(provider + ":revision", material), "email": address, "plan": plan,
            "_provider": provider, "_credential": credential,
            **({"_muse_context": muse_context} if provider == "muse" else {})}


def _desktop_candidates(provider, home, platform, ccs_dir):
    found = []
    if provider == "muse":
        paths = [(desktop.config_home(home) / "muse/auth.json", "native:muse", ["discover"])]
        paths += [(ccs_dir / "muse-homes" / slot / "muse/auth.json", "config-home:" + slot,
                   ["config-home:" + slot]) for slot in _slots(ccs_dir / "muse-homes", r"([a-f0-9]{8})")]
        for path, locator, selectors in paths:
            providers = _json(path).get("providers")
            meta = providers.get("meta") if isinstance(providers, dict) else None
            token = desktop.secret(meta.get("access_token")) if isinstance(meta, dict) else None
            if token and token.startswith("dca:"):
                found.append((locator, selectors, desktop.Credential(token, "Muse CLI", email=desktop.email(meta.get("user_email")))))
    else:
        root = desktop_usage.cursor_root(home, platform)
        rows = desktop.sqlite_rows(root / "User/globalStorage/state.vscdb",
            "SELECT key,value FROM ItemTable WHERE key IN "
            "('cursorAuth/accessToken','cursorAuth/cachedEmail','cursorAuth/stripeMembershipType')")
        fields = dict(rows)
        for locator, token, address, plan in (
            ("cursor:sqlite", fields.get("cursorAuth/accessToken"), fields.get("cursorAuth/cachedEmail"), fields.get("cursorAuth/stripeMembershipType")),
            ("cursor:auth-json", _json(root / "auth.json").get("accessToken"), None, None)):
            token = desktop.secret(token)
            if token:
                claims = desktop.jwt_claims(token)
                found.append((locator, ["discover"], desktop.Credential(token, "Cursor desktop",
                    email=desktop.email(claims.get("email")) or desktop.email(address), plan=desktop.safe_text(plan))))
    for rowid, kind, data in _omp("muse-code" if provider == "muse" else provider, home):
        if kind != "oauth":
            continue
        credential = desktop_usage.credential_from_omp(provider, data)
        if credential:
            found.append(("omp:" + rowid, ["discover"], credential))
    return found


def _plan_candidates(provider, home, ccs_dir):
    found = []
    aliases = {"kimi-code": ("kimi-for-coding", "kimi-code"), "qwen": ("alibaba-token-plan",),
               "zai": ("zai", "zai-coding-plan"), "opencode-go": ("opencode-go",)}
    opencode = _json(home / ".local/share/opencode/auth.json")
    for alias in aliases[provider]:
        item = opencode.get(alias)
        if isinstance(item, dict) and item.get("type") == "api" and plans._string(item.get("key")):
            found.append(("opencode:" + alias, ["discover"], {"secret": item["key"], "source": "OpenCode", "email": None, "expires": None}))
    for rowid, kind, data in _omp("alibaba-token-plan" if provider == "qwen" else provider, home):
        secret = plans._string(data.get("key")) or plans._string(data.get("access"))
        if secret and kind in ("api_key", "oauth"):
            found.append(("omp:" + rowid, ["discover"], {"secret": secret, "source": "OMP",
                "email": plans.email_address(data.get("email")),
                "expires": plans.number(data.get("expires")) if provider == "kimi-code" and kind == "oauth" else None}))
    env_names = {"kimi-code": ("KIMI_CODE_API_KEY", "KIMI_API_KEY"), "qwen": ("BAILIAN_TOKEN_PLAN_API_KEY",),
                 "zai": ("ZAI_API_KEY", "ZHIPU_API_KEY"), "opencode-go": ("OPENCODE_GO_API_KEY", "OPENCODE_API_KEY")}
    for name in env_names[provider]:
        secret = plans._string(os.environ.get(name))
        if secret:
            found.append(("environment:" + name, ["discover"], {"secret": secret, "source": "User environment", "email": None, "expires": None}))
    if provider in plans.KEY_PROVIDERS:
        pattern = re.escape(provider) + r"-([a-f0-9]{8})\.(?:json|dpapi)"
        for slot in sorted(set(_slots(ccs_dir / "account-usage/keys", pattern))):
            item = plans.aac_key_credential(provider, slot, home, ccs_dir=ccs_dir)
            if item:
                found.append(("aac-key:" + slot, ["aac-key:" + slot], item))
    if provider == "qwen":
        browser = []
        for item in plans._safari_qwen_candidates(home):
            try:
                _, region = plan_usage._qwen_credential(item["secret"])
                browser.append(("safari:" + region, ["discover"], item))
            except plans.CollectionError:
                pass
        slots = ["default"] + _slots(ccs_dir / "account-usage", r"qwen-console-session-([a-f0-9]{8})\.json")
        for slot in slots:
            items = plans._selected_credentials(provider, home, {"kind": "browser-capsule", "capsuleId": slot}, ccs_dir=ccs_dir)
            for item in items:
                selectors = ["browser-capsule:" + slot] + (["discover"] if slot == "default" else [])
                browser.insert(0, ("browser-capsule:" + slot, selectors, item))
        found = browser + found
    if provider == "kimi-code":
        # Preserve today's preference for a nonexpired login without dropping
        # older slots: a selected account may explicitly refer to one of them.
        def expired(row):
            value = row[2].get("expires")
            return value is not None and (value / 1000 if value > 100000000000 else value) <= time.time()
        found.sort(key=expired)
    return found


def inventory(provider, home, platform, ccs_dir=None):
    if provider not in PROVIDERS or platform not in HOST_LABELS:
        return []
    home = Path(home)
    ccs_dir = Path(ccs_dir) if ccs_dir is not None else home / ".ccs"
    found = _desktop_candidates(provider, home, platform, ccs_dir) if provider in {"muse", "cursor"} else _plan_candidates(provider, home, ccs_dir)
    result = []
    for locator, selectors, credential in found[:MAX_CANDIDATES]:
        context = _muse_context(ccs_dir, credential) if provider == "muse" else None
        item = _candidate(provider, locator, selectors, credential, context)
        if item is not None:
            result.append(item)
    return result


def _failure(code, retry=None):
    result = {"sample": None, "failureCode": code}
    if isinstance(retry, (int, float)) and not isinstance(retry, bool) and 0 < retry <= 86400:
        result["retryAfterSeconds"] = retry
    return result


def _raise_identity():
    raise plans.CollectionError("unavailable", "The saved account identity could not be verified.", failure_code="account_mismatch")


def _muse(credential, result, ccs_dir, client, context):
    current = _muse_context(ccs_dir, credential)
    if current != context:
        return _failure("source_changed")
    payload = client.get(desktop_usage.MUSE_URL, {"Authorization": "Bearer " + credential.access,
        "x-api-version": "1.0.0", "Content-Type": "application/json"}, body=b"{}")
    address = desktop.email(payload.get("user_email"))
    if not address or credential.email and address.lower() != credential.email.lower():
        _raise_identity()
    result.update(email=address, plan=desktop.safe_text(payload.get("subs_tier_name")))
    if payload.get("is_subs_active") is False:
        return _failure("inactive_subscription")
    if context is None:
        # A native response has no proven portal-team scope. Keep it isolated
        # from portal readings, even when the native email matches.
        result["windows"] = desktop_usage.normalize_muse(payload)
        return None if result["windows"] else _failure("no_browser_cookie")
    # A portal identity must read its exact team, even if /key also reports
    # quota whose subscription scope cannot be independently established.
    import muse_console as muse
    root = ccs_dir / "account-usage"
    cookies, team, address_bound, _ = muse.read_capsule(root)
    if address_bound.lower() != address.lower():
        _raise_identity()
    if (team != context["team"] or address_bound.lower() != context["email"] or
            muse.session_values(cookies) != context["session"]):
        return _failure("source_changed")
    sample = muse.quota_sample(root, cookies, address, result["plan"], team, credential.access, bound_team=team)
    result.update(windows=desktop_usage.normalize_muse({"subscription_quota": sample["quota"]}),
                  status="cached" if sample["cached"] else "ok", sampledAt=sample["sampledAt"], message=sample["message"])
    return None


def _cursor(credential, result, client):
    headers = {"Cookie": desktop_usage.cursor_cookie(credential)}
    payload = client.get(desktop_usage.CURSOR_SUMMARY_URL, headers)
    result.update(plan=desktop.safe_text(payload.get("membershipType")) or credential.plan,
                  windows=desktop_usage.normalize_cursor(payload))
    try:
        profile = client.get(desktop_usage.CURSOR_PROFILE_URL, headers)
    except plans.CollectionError:
        # A valid quota response authenticates the exact subject-bearing token.
        # Absence of optional profile metadata does not change that subject.
        return
    user = profile.get("user") if isinstance(profile.get("user"), dict) else profile
    address = desktop.email(user.get("email"))
    if address and credential.email and address.lower() != credential.email.lower():
        _raise_identity()
    if address:
        result["email"] = address


def _plan(provider, credential, result, home, client):
    expires = credential.get("expires")
    if expires is not None and (expires / 1000 if expires > 100000000000 else expires) <= time.time():
        return _failure("needs_sign_in")
    secret = credential["secret"]
    if provider == "kimi-code":
        payload = client.get("https://api.kimi.com/coding/v1/usages", {"Authorization": "Bearer " + secret})
        plan, windows = plan_usage.parse_kimi(payload)
    elif provider == "zai":
        payload = client.get("https://api.z.ai/api/monitor/usage/quota/limit", {"Authorization": secret, "Content-Type": "application/json"})
        plan, windows = plan_usage.parse_zai(payload)
        try:
            packs = client.get("https://api.z.ai/api/biz/customer-package-reset/list?targetType=PERSONAL", {"Authorization": "Bearer " + secret})
            windows.extend(plan_usage.parse_zai_reset_packs(packs))
        except plans.CollectionError:
            result["message"] = "Account usage is current; reset pack inventory is unavailable."
    elif provider == "opencode-go":
        installation = hashlib.sha256(str(home).encode()).hexdigest()[:32]
        payload = client.get("https://opencode.ai/zen/go/v1/usage", {"Authorization": "Bearer " + secret, "x-opencode-session": "ccs-usage-" + installation})
        plan, windows = plan_usage.parse_go(payload)
    else:
        plan, windows = plan_usage.parse_qwen(*plan_usage.fetch_qwen(client, secret))
    result.update(plan=plan, windows=windows)
    return None


def collect(candidate, home, platform, ccs_dir=None):
    """Read the already revalidated candidate, never a different default login."""
    provider = candidate.get("_provider")
    credential = candidate.get("_credential")
    if provider not in PROVIDERS or platform not in HOST_LABELS or credential is None:
        return _failure("invalid_request")
    home = Path(home)
    ccs_dir = Path(ccs_dir) if ccs_dir is not None else home / ".ccs"
    result = desktop.account(provider, platform) if provider in {"muse", "cursor"} else plans.account(provider, platform)
    result.update(status="ok", message=None, email=candidate["email"], plan=candidate["plan"])
    source = credential.source if isinstance(credential, desktop.Credential) else credential["source"]
    result["source"] = source + " on " + HOST_LABELS[platform]
    try:
        client = plans.HttpClient(deadline_seconds=16 if provider == "muse" else 7)
        if provider == "muse":
            failure = _muse(credential, result, ccs_dir, client, candidate.get("_muse_context"))
        elif provider == "cursor":
            failure = _cursor(credential, result, client)
        else:
            failure = _plan(provider, credential, result, home, client)
        if failure:
            return failure
        if not result["windows"]:
            return _failure("invalid_response")
        result["fetchedAt"] = desktop.utc_now()
        result["sampledAt"] = result["sampledAt"] or result["fetchedAt"]
        return {"sample": result, "failureCode": None}
    except plans.CollectionError as error:
        return _failure(error.failure_code or ("needs_sign_in" if error.status == "needs_sign_in" else "provider_error"), error.retry_after)
    except desktop.UsageError as error:
        return _failure("needs_sign_in" if error.status == "needs_sign_in" else "provider_error")
    except Exception as error:
        # Muse's fixed enum is safe; arbitrary exception bodies are not.
        import muse_console as muse
        if isinstance(error, muse.MuseError):
            return _failure(error.code, error.retry_after)
        return _failure("provider_error")
