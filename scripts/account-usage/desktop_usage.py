#!/usr/bin/env python3
"""Usage-only collectors for existing Antigravity, Muse Code and Cursor logins.

Run on the computer that owns the login. No credentials, API keys, raw upstream
responses or account mutations are emitted. Python 3 standard library only.
"""

import argparse
import datetime as dt
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
import time
import cursor_usage

from desktop_helpers import (
    Credential, PROVIDERS, UsageError, account, config_home, counter, email, jwt_claims,
    nonnegative, number, omp_credentials, percent, quota_window, read_json,
    request_json, reset_at, safe_text, secret, sqlite_rows, utc_now,
)

MUSE_URL = "https://api.meta.ai/muse-code/key"
AGY_BASE = "https://cloudcode-pa.googleapis.com/v1internal:"
# The quota summary gates its Gemini (and Argon) groups on the client version
# in the User-Agent: 1.0.0-era clients stopped receiving them on 2026-10-22,
# while current Antigravity client versions still do. One constant, so a future
# version bump is a single change; the per-model retrieveUserQuota fallback in
# normalize_antigravity_gemini covers any window the summary then omits.
AGY_USER_AGENT = "antigravity/hub/2.24.0 (aidev_client; os_type=linux; arch=amd64)"
# The transport's deadline spans the whole collector process. This budget
# starts when the module is imported, so credential reads and every quota
# call count against the optional per-model read; the interpreter start-up
# and imports before it load, and the SSH handshake outside it, fit in the
# deadline's remaining allowance (13 + 8s socket = 21 of 25s).
AGY_BUDGET_STARTED = time.monotonic()
AGY_OPTIONAL_BUDGET_SECONDS = 13
CURSOR_SUMMARY_URL = cursor_usage.SUMMARY_URL
CURSOR_PROFILE_URL = cursor_usage.PROFILE_URL


def credential_from_omp(provider, value):
    token = value.get("access")
    if provider == "muse":
        try:
            packed = json.loads(token) if isinstance(token, str) and token.startswith("{") else None
            if isinstance(packed, dict):
                token = packed.get("oauthAccessToken")
        except ValueError:
            return None
        if not isinstance(token, str) or not token.startswith("dca:"):
            return None
    token = secret(token)
    if token is None:
        return None
    return Credential(token, "OMP", secret(value.get("refresh")) or "",
                      number(value.get("expires")), email(value.get("email")),
                      safe_text(value.get("projectId")), google_client="antigravity" if provider == "antigravity" else None)


def muse_credentials(home):
    value = read_json(config_home(home) / "muse/auth.json") or {}
    providers = value.get("providers")
    meta = providers.get("meta") if isinstance(providers, dict) else None
    if isinstance(meta, dict):
        token = secret(meta.get("access_token"))
        if token and token.startswith("dca:"):
            return Credential(token, "Muse CLI", email=email(meta.get("user_email")))
    for value in omp_credentials("muse-code", home):
        credential = credential_from_omp("muse", value)
        if credential:
            return credential
    return None


def antigravity_credentials(home):
    value = read_json(pathlib.Path(home) / ".gemini/antigravity-cli/antigravity-oauth-token") or {}
    token = value.get("token")
    if value.get("auth_method") == "consumer" and isinstance(token, dict):
        access = secret(token.get("access_token"))
        if access:
            claims = jwt_claims(value.get("id_token") or "")
            expiry = reset_at(token.get("expiry"))
            expiry = dt.datetime.fromisoformat(expiry.replace("Z", "+00:00")).timestamp() * 1000 if expiry else None
            return Credential(access, "Antigravity CLI", secret(token.get("refresh_token")) or "",
                              expiry, email(claims.get("email")), google_client="consumer")
    for value in omp_credentials("google-antigravity", home):
        credential = credential_from_omp("antigravity", value)
        if credential:
            return credential
    return None


def cursor_root(home, platform):
    if platform == "mac":
        return pathlib.Path(home) / "Library/Application Support/Cursor"
    if platform == "windows":
        appdata = os.environ.get("APPDATA")
        return (pathlib.Path(appdata) if appdata else pathlib.Path(home) / "AppData/Roaming") / "Cursor"
    return config_home(home) / "Cursor"


def cursor_credentials(home, platform):
    root = cursor_root(home, platform)
    rows = sqlite_rows(root / "User/globalStorage/state.vscdb",
                       "SELECT key,value FROM ItemTable WHERE key IN "
                       "('cursorAuth/accessToken','cursorAuth/refreshToken','cursorAuth/cachedEmail','cursorAuth/stripeMembershipType')")
    fields = {row[0]: row[1] for row in rows}
    value = read_json(root / "auth.json") or {}
    token = secret(fields.get("cursorAuth/accessToken")) or secret(value.get("accessToken"))
    if token:
        claims = jwt_claims(token)
        expires = number(claims.get("exp"))
        return Credential(token, "Cursor desktop", secret(fields.get("cursorAuth/refreshToken")) or secret(value.get("refreshToken")) or "",
                          expires * 1000 if expires is not None else None,
                          email(claims.get("email")) or email(fields.get("cursorAuth/cachedEmail")),
                          plan=safe_text(fields.get("cursorAuth/stripeMembershipType")))
    for value in omp_credentials("cursor", home):
        credential = credential_from_omp("cursor", value)
        if credential:
            return credential
    return None


def muse_window_label(key, minutes):
    """Meta's own /usage card names, with our cadence: "Current usage (5-hour)", "Weekly limit"."""
    if key == "weekly":
        return "Weekly limit"
    if minutes is None or not 0 < minutes <= 525600:
        return "Current usage"
    return "Current usage (" + (f"{minutes / 60:g}-hour" if minutes % 60 == 0 else f"{minutes:g}-minute") + ")"


def normalize_muse(payload):
    usage = payload.get("subs_usage")
    windows = []
    portal = payload.get("subscription_quota")
    if isinstance(portal, dict):
        for key in ("window", "weekly"):
            used = counter(portal.get(key + "_weighted_used"))
            limit = counter(portal.get(key + "_weighted_limit"))
            duration = counter(portal.get("window_duration_secs")) if key == "window" else None
            minutes = duration / 60 if duration is not None else 10080 if key == "weekly" else None
            label = muse_window_label(key, minutes)
            reset_value = portal.get(key + "_resets_at")
            reset = reset_at(counter(reset_value) if isinstance(reset_value, str) and counter(reset_value) is not None else reset_value)
            if used is None and reset is None:
                continue
            value = (used / limit * 100) if used is not None and limit is not None and limit > 0 else None
            windows.append(quota_window(key, label, used_percent=value, reset=reset, minutes=minutes,
                                        used=used, limit=limit, unit="weighted tokens"))
        return windows
    if isinstance(usage, dict):
        for key, duration in (("window", None), ("weekly", 10080)):
            bucket = usage.get(key)
            if isinstance(bucket, dict):
                duration = number(bucket.get("window_duration_mins", duration))
                window = quota_window(key, muse_window_label(key, duration), used_percent=bucket.get("used_percent"),
                                      reset=bucket.get("resets_at"), minutes=duration,
                                      enabled=bucket.get("enabled"), unlimited=bucket.get("unlimited"))
                if window["usedPercent"] is not None or window["resetAt"] is not None:
                    windows.append(window)
    return windows


def fetch_muse(credential, result, home=None):
    home = pathlib.Path(home) if home is not None else pathlib.Path.home()
    root = home / ".ccs/account-usage"

    def failed(code, message):
        # Internal collector classification, never a public dashboard field.
        # An unclassified failure must not authorize reuse of previous quota.
        result.update(status="needs_sign_in" if code == "needs_sign_in" else "unavailable",
                      failureCode=code, message=message, windows=[], sampledAt=None)

    # Use only a previously verified, credential-bound observation during its
    # shared cooldown. This also avoids minting an unused inference key on each
    # independent bar/dashboard refresh.
    cached = None
    try:
        from muse_console import MuseError, ERROR_MESSAGES, quota_sample, read_capsule, verify_device_identity
        cookies, team, bound_email, bound_plan = read_capsule(root)
        if credential.email and credential.email.lower() == bound_email.lower():
            result.update(email=bound_email, plan=bound_plan)
            cached = quota_sample(root, cookies, bound_email, bound_plan, team, credential.access,
                                  identity_loader=lambda: verify_device_identity(credential, bound_email),
                                  bound_team=team)
        if cached:
            result.update(email=bound_email, plan=cached["quota"]["tier"], status="cached" if cached["cached"] else "ok", message=cached["message"],
                          sampledAt=cached["sampledAt"], windows=normalize_muse({"subscription_quota": cached["quota"]}))
            result["source"] += " + authenticated Meta web quota"
            return
    except ImportError:
        pass
    except MuseError as error:
        if error.code not in {"no_browser_cookie", "local_storage_error"}:
            if error.plan is not None:
                result["plan"] = error.plan
            failed(error.code, ERROR_MESSAGES[error.code])
            return
    try:
        payload = request_json(MUSE_URL, {"Authorization": "Bearer " + credential.access, "x-api-version": "1.0.0"}, {})
    except UsageError as error:
        failed("needs_sign_in" if error.status == "needs_sign_in" else "error", error.message)
        return
    result["email"] = email(payload.get("user_email")) or credential.email
    result["plan"] = safe_text(payload.get("subs_tier_name"))
    result["windows"] = normalize_muse(payload)
    if credential.email and result["email"] and credential.email.lower() != result["email"].lower():
        failed("account_mismatch", "The Muse usage account differs from the signed-in Muse CLI account.")
        return
    if payload.get("is_subs_active") is False:
        failed("inactive_subscription", "This account has no active Muse Code subscription.")
        return
    if result["windows"]:
        return
    # Muse omits subs_usage while a rolling window is idle, even when weekly
    # usage exists. Its web portal has a separate, browser-authenticated quota
    # endpoint. Never send the CLI's device credential to that web origin.
    result["status"], result["message"] = "unavailable", (
        "Muse confirmed this subscription but omitted its usage. Sync Muse in the Mac browser usage bridge to read the rolling and weekly limits.")
    result["failureCode"] = "invalid_response"
    try:
        from muse_console import MuseError, ERROR_MESSAGES, quota_sample, read_capsule
    except ImportError:
        return
    try:
        cookies, team, bound_email, bound_plan = read_capsule(root)
        if not result["email"] or not result["plan"] or bound_email.lower() != result["email"].lower():
            failed("account_mismatch", ERROR_MESSAGES["account_mismatch"])
            return
        sample = quota_sample(root, cookies, result["email"], result["plan"], team, credential.access, bound_team=team)
        result["windows"] = normalize_muse({"subscription_quota": sample["quota"]})
        if result["windows"]:
            result["status"] = "cached" if sample["cached"] else "ok"
            result["message"], result["sampledAt"] = sample["message"], sample["sampledAt"]
            result.pop("failureCode", None)
            result["source"] += " + authenticated Meta web quota"
    except MuseError as error:
        if error.plan is not None:
            result["plan"] = error.plan
        failed(error.code, result["message"] if error.code == "no_browser_cookie"
               else ERROR_MESSAGES.get(error.code, ERROR_MESSAGES["error"]))


def agy_groups(payload):
    for _ in range(4):
        groups = payload.get("groups")
        if isinstance(groups, list):
            return groups[:32]
        buckets = payload.get("buckets")
        if isinstance(buckets, list):
            return [{"displayName": "Antigravity", "buckets": buckets}]
        nested = next((payload.get(key) for key in ("quotaSummary", "summary", "response") if isinstance(payload.get(key), dict)), None)
        if nested is None:
            break
        payload = nested
    return []


def normalize_antigravity(payload):
    windows, keys = [], set()
    for group_index, group in enumerate(agy_groups(payload)):
        if not isinstance(group, dict):
            continue
        group_label = safe_text(group.get("displayName")) or "Antigravity"
        buckets = group.get("buckets")
        if not isinstance(buckets, list):
            continue
        for bucket_index, bucket in enumerate(buckets[:32]):
            if not isinstance(bucket, dict):
                continue
            fraction = number(bucket.get("remainingFraction"))
            remaining = bucket.get("remaining")
            remaining_amount = counter(bucket.get("remainingAmount"))
            if fraction is None and isinstance(remaining, dict):
                fraction = number(remaining.get("remainingFraction"))
                if fraction is None and remaining.get("case") == "remainingFraction":
                    fraction = number(remaining.get("value"))
                if remaining_amount is None:
                    remaining_amount = counter(remaining.get("remainingAmount"))
                    if remaining_amount is None and remaining.get("case") == "remainingAmount":
                        remaining_amount = counter(remaining.get("value"))
            fraction = fraction if fraction is not None and 0 <= fraction <= 1 else None
            period = bucket.get("window")
            minutes = 10080 if period == "weekly" else 300 if period == "5h" else None
            period_label = "Weekly" if period == "weekly" else "5-hour" if period == "5h" else None
            bucket_label = safe_text(bucket.get("displayName"))
            label = group_label + " · " + period_label if period_label else (
                group_label + " · " + bucket_label if bucket_label and bucket_label != group_label else group_label)
            key = safe_text(bucket.get("bucketId")) or "group-" + str(group_index) + "-" + str(bucket_index)
            if key in keys:
                key += "-" + str(group_index) + "-" + str(bucket_index)
            window = quota_window(key, label, remaining_percent=fraction * 100 if fraction is not None else None,
                                  reset=bucket.get("resetTime"), minutes=minutes,
                                  remaining=remaining_amount, unit=safe_text(bucket.get("unit"), 32),
                                  enabled=not bucket["disabled"] if isinstance(bucket.get("disabled"), bool) else None,
                                  unlimited=bucket.get("unlimited"))
            if window["remainingPercent"] is not None or window["resetAt"] is not None or window.get("remaining") is not None:
                windows.append(window)
                keys.add(key)
            if len(windows) >= 64:
                return windows
    return windows


GEMINI_PERIOD_LABELS = {"5h": "5-hour", "weekly": "Weekly"}
# Same shared window can straddle a second boundary between back-to-back calls.
GEMINI_RESET_CLUSTER_SECONDS = 10


def antigravity_summary_has_gemini(summary_windows):
    """Whether the quota summary already reports a Gemini (or Argon) group.

    Both collectors check this before the optional per-model read: when the
    summary carries Gemini keys or labels, the derivation stands down and the
    extra request is never made, so a throttle on it cannot hold the account.
    """
    return any(isinstance(window, dict) and (
        str(window.get("key", "")).startswith("gemini")
        or str(window.get("label", "")).lower().startswith("gemini"))
        for window in summary_windows)


def _antigravity_gemini_period(reset, anchors):
    """Classify a per-model Gemini reset against the summary's own windows.

    The per-model quota carries no window label; a reset matching a summary
    window's reset (both derive from the same plan anchors, seconds apart)
    adopts that window's period. When distinct periods both match -- or none
    does -- the period stays unknown rather than being guessed.
    """
    try:
        stamp = dt.datetime.fromisoformat(reset.replace("Z", "+00:00"))
    except ValueError:
        return None, None
    matched = set()
    for anchor_reset, minutes in anchors:
        if anchor_reset is None or minutes not in (300, 10080):
            continue
        try:
            anchor = dt.datetime.fromisoformat(anchor_reset.replace("Z", "+00:00"))
        except ValueError:
            continue
        if abs((stamp - anchor).total_seconds()) <= 600:
            matched.add(("5h", 300) if minutes == 300 else ("weekly", 10080))
    return next(iter(matched)) if len(matched) == 1 else (None, None)


def _antigravity_gemini_groups(buckets):
    """Cluster gemini buckets into shared windows by their resets.

    Buckets missing a reset describe no window and are skipped. A bucket with
    a reset but neither a remainingFraction nor a remainingAmount is an
    exhausted window: proto3 JSON omits zero-valued floats, so the missing
    fraction decodes as 0, never as unknown. A bucket carrying only an amount
    has no denominator and stays unknown. An id that fails display validation
    still counts toward the minimum; it is only kept out of the published
    model list. Each cluster publishes the earliest reset it contains.
    """
    collected = []
    for bucket in (buckets if isinstance(buckets, list) else [])[:256]:
        if not isinstance(bucket, dict):
            continue
        model_id = bucket.get("modelId")
        reset = reset_at(bucket.get("resetTime"))
        if reset is None:
            continue
        if not isinstance(model_id, str) or not model_id.startswith("gemini-") or not 0 < len(model_id) <= 160:
            continue
        fraction = number(bucket.get("remainingFraction"))
        if fraction is None:
            if bucket.get("remainingAmount") is not None:
                # An amount without a fraction has no denominator; the bucket
                # stays unknown instead of being read as exhausted.
                continue
            # proto3 JSON omits zero-valued floats: no fraction and no amount
            # is an exhausted window, which floors the shared minimum at 0.
            fraction = 0.0
        if not 0 <= fraction <= 1:
            continue
        collected.append((dt.datetime.fromisoformat(reset.replace("Z", "+00:00")), reset,
                          safe_text(model_id, 160), fraction))
    collected.sort(key=lambda entry: entry[0])
    groups = []
    for stamp, reset, model_id, fraction in collected:
        if groups and (stamp - groups[-1][0]).total_seconds() <= GEMINI_RESET_CLUSTER_SECONDS:
            groups[-1][2].append((model_id, fraction))
        else:
            groups.append((stamp, reset, [(model_id, fraction)]))
    return [(reset, members) for _stamp, reset, members in groups]


def normalize_antigravity_gemini(payload, summary_windows):
    """Derive the Gemini windows from the per-model quota buckets.

    Google stopped listing the Gemini group in retrieveUserQuotaSummary
    (observed 2026-10-22); its shared windows survive only as per-model
    buckets in retrieveUserQuota, one reset per window. One derived window per
    clustered shared reset: the remaining fraction is the minimum across the
    models sharing it, and no window is derived without a provider reset or
    when the summary reports Gemini keys or labels again. A base key is only
    suffixed when the same period repeats, so a lone weekly group stays
    `gemini-weekly` even when a 5-hour group comes and goes.
    """
    if not isinstance(payload, dict):
        return []
    if antigravity_summary_has_gemini(summary_windows):
        return []
    anchors = [(window.get("resetAt"), window.get("windowMinutes"))
               for window in summary_windows if isinstance(window, dict)]
    seen_base_keys = {}
    windows = []
    for reset, members in _antigravity_gemini_groups(payload.get("buckets"))[:4]:
        period, minutes = _antigravity_gemini_period(reset, anchors)
        base = "gemini-" + (period or "window")
        seen_base_keys[base] = seen_base_keys.get(base, 0) + 1
        key = base if seen_base_keys[base] == 1 else base + "-" + str(seen_base_keys[base])
        label = "Gemini Models" + (" · " + GEMINI_PERIOD_LABELS[period] if period else "")
        window = quota_window(key, label, remaining_percent=min(f for _, f in members) * 100,
                              reset=reset, minutes=minutes)
        model_ids = sorted({model for model, _ in members if model is not None})
        if model_ids:
            window["modelIds"] = model_ids[:64]
        windows.append(window)
    return windows


def antigravity_reported_plan(load):
    """Retain only plan/tier display fields, for the single TypeScript plan table."""
    reported = {}
    info = load.get("planInfo")
    plan_type = safe_text(info.get("planType"), 80) if isinstance(info, dict) else None
    if plan_type:
        reported["planType"] = plan_type
    for key in ("paidTier", "currentTier"):
        tier = load.get(key)
        if isinstance(tier, dict):
            display = {field: value for field in ("id", "name")
                       if (value := safe_text(tier.get(field), 80)) is not None}
            if display:
                reported[key] = display
    tier = reported.get("paidTier") or reported.get("currentTier") or {}
    return plan_type or tier.get("name") or tier.get("id"), reported


def normalize_antigravity_credits(load):
    """Google's loadCodeAssist reports an optional shared AI credit balance.

    Known protobuf amounts may be numeric strings. Missing credit metadata is
    unknown, including an empty collection; only an explicitly reported zero
    produces a zero balance. Never infer allowance, expiry or currency.
    """
    tier = load.get("paidTier")
    credits = tier.get("availableCredits") if isinstance(tier, dict) else None
    if not isinstance(credits, list) or not 1 <= len(credits) <= 64:
        return []
    amounts = [counter(item.get("creditAmount")) for item in credits if isinstance(item, dict)]
    amounts = [value for value in amounts if value is not None]
    if not amounts:
        return []
    remaining = nonnegative(sum(amounts))
    if remaining is None:
        return []
    return [quota_window("google-ai-credits", "AI credits (overage)", kind="balance", remaining=remaining, unit="credits")]


def windows_private_metadata(path):
    """Windows chmod does not establish privacy; require a protected user ACL."""
    literal = str(path).replace("'", "''")
    script = (
        "$ErrorActionPreference='Stop';"
        "$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;"
        "$acl=Get-Acl -LiteralPath '" + literal + "';"
        "if(-not $acl.AreAccessRulesProtected){exit 1};"
        "$owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value;"
        "if($owner -ne $sid -and $owner -ne 'S-1-5-32-544'){exit 1};"
        "$rules=$acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]);"
        "if($rules.Count -gt 16){exit 1};"
        "foreach($rule in $rules){"
        "if($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -ne $sid "
        "-and $rule.IdentityReference.Value -ne 'S-1-5-18'){exit 1}};"
        "[Console]::Write('private')"
    )
    try:
        result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script],
                                capture_output=True, text=True, timeout=8)
        return result.returncode == 0 and result.stdout == "private"
    except (OSError, subprocess.TimeoutExpired):
        return False


def antigravity_oauth_client(home=None):
    """Read installed-app OAuth metadata privately; never embed or emit it.

    These application values accompany the native Antigravity login, rather
    than representing a user's token. Provisioning them is separate from this
    strictly read-only collector and never copies a user's account login.
    """
    home = pathlib.Path(home) if home is not None else pathlib.Path.home()
    path = home / ".ccs/account-usage/antigravity-oauth-client.json"
    failure = "Antigravity's private app OAuth metadata is unavailable on this computer."
    try:
        if path.is_symlink() or path.parent.is_symlink():
            raise OSError()
        if os.name == "nt" and not windows_private_metadata(path):
            raise OSError()
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(descriptor, "rb") as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > 4096:
                raise OSError()
            if os.name == "posix" and (info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600):
                raise OSError()
            raw = handle.read(4097)
            if len(raw) > 4096:
                raise OSError()
        def unique_pairs(items):
            value = {}
            for key, item in items:
                if key in value:
                    raise ValueError()
                value[key] = item
            return value

        def invalid_constant(_):
            raise ValueError()

        value = json.loads(raw, object_pairs_hook=unique_pairs, parse_constant=invalid_constant)
        if not isinstance(value, dict):
            raise ValueError()
        if set(value) == {"schemaVersion", "clientId", "clientSecret"}:
            schema_version = value["schemaVersion"]
        elif set(value) == {"schema", "clientId", "clientSecret"}:
            schema_version = value["schema"]
        else:
            raise ValueError()
        if type(schema_version) is not int or schema_version != 1:
            raise ValueError()
        client_id, app_value = value["clientId"], value["clientSecret"]
        if (not isinstance(client_id, str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,256}\.apps\.googleusercontent\.com", client_id)
                or not isinstance(app_value, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,256}", app_value)):
            raise ValueError()
        return client_id, app_value
    except (OSError, ValueError, UnicodeError, RecursionError):
        raise UsageError("unavailable", failure) from None


def refresh_antigravity(credential, home=None):
    if not credential.refresh:
        raise UsageError("needs_sign_in", "The saved Antigravity account needs a renewed sign-in.")
    client_id, app_value = antigravity_oauth_client(home)
    payload = request_json("https://oauth2.googleapis.com/token", body={
        "grant_type": "refresh_token", "refresh_token": credential.refresh,
        "client_id": client_id, "client_secret": app_value,
    }, form=True)
    token = secret(payload.get("access_token"))
    if token is None:
        raise UsageError("needs_sign_in", "The saved Antigravity account could not renew its session.")
    # Google refresh grants renew the access token without rotating the stored
    # refresh token. Keep the new access token in this short-lived process only.
    return Credential(token, credential.source, credential.refresh, email=credential.email,
                      project=credential.project, plan=credential.plan, google_client=credential.google_client)


def fetch_antigravity(credential, result, home=None, budget_started=None):
    # One monotonic budget spans process start-up, the token refresh, the
    # quota calls and the authentication retry, so the optional per-model
    # read can never push the collection past its transport deadline.
    budget = AGY_BUDGET_STARTED if budget_started is None else budget_started
    now = dt.datetime.now(dt.timezone.utc).timestamp() * 1000
    refreshed = False
    if credential.expires is not None and credential.expires <= now + 30000:
        credential = refresh_antigravity(credential, home)
        refreshed = True
    try:
        fetch_antigravity_quota(credential, result, budget)
    except UsageError as error:
        if error.status != "needs_sign_in" or refreshed or not credential.refresh:
            raise
        fetch_antigravity_quota(refresh_antigravity(credential, home), result, budget)


def fetch_antigravity_quota(credential, result, budget_started=None):
    # The control plane selects Antigravity quota/project behavior by native
    # client User-Agent. A generic UA returns Gemini eligibility tiers instead.
    started = budget_started if budget_started is not None else time.monotonic()
    headers = {"Authorization": "Bearer " + credential.access, "User-Agent": AGY_USER_AGENT}
    load = request_json(AGY_BASE + "loadCodeAssist", headers,
                        {"metadata": {"ideType": "ANTIGRAVITY", "platform": "PLATFORM_UNSPECIFIED", "pluginType": "GEMINI"}})
    project = load.get("cloudaicompanionProject")
    if isinstance(project, dict):
        project = project.get("id") or project.get("projectId")
    project = safe_text(project) or credential.project
    if project is None:
        raise UsageError("unavailable", "The signed-in account did not return an Antigravity quota project.")
    result["plan"], reported = antigravity_reported_plan(load)
    if reported:
        result["reportedPlan"] = reported
    payload = request_json(AGY_BASE + "retrieveUserQuotaSummary", headers, {"project": project})
    windows = normalize_antigravity(payload)
    per_model = None
    # The optional per-model read runs only when the summary carries no Gemini
    # group, and only while the transport's remaining deadline still fits it:
    # each call may hold its socket for 8 of the transport's 25 seconds.
    if not antigravity_summary_has_gemini(windows) and time.monotonic() - started <= AGY_OPTIONAL_BUDGET_SECONDS:
        try:
            per_model = request_json(AGY_BASE + "retrieveUserQuota", headers, {"project": project})
        except UsageError as error:
            # The optional read stays optional, but a provider throttle must not be:
            # surface its cooldown so refreshes stop retrying the endpoint early.
            retry_after = getattr(error, "retry_after", None)
            if retry_after is not None and retry_after > 0 and "retryAfterSeconds" not in result:
                result["retryAfterSeconds"] = retry_after
    windows += normalize_antigravity_gemini(per_model, windows)
    result["windows"] = windows + normalize_antigravity_credits(load)


def cursor_cookie(credential):
    try:
        return cursor_usage.cookie(credential)
    except cursor_usage.CollectionError as error:
        raise UsageError(error.status, error.message) from None


def normalize_cursor(payload):
    return cursor_usage.normalize_legacy(payload)


def fetch_cursor(credential, result):
    from plan_common import HttpClient, CollectionError
    try:
        result.update(cursor_usage.collect(credential, result, HttpClient(deadline_seconds=16)))
    except CollectionError as error:
        result.update(status="needs_sign_in" if error.status == "needs_sign_in" else "unavailable",
                      failureCode=error.failure_code or ("needs_sign_in" if error.status == "needs_sign_in" else "provider_error"),
                      message=error.message, windows=[])
        if error.retry_after is not None:
            result["retryAfterSeconds"] = error.retry_after


def collect(provider, platform, home=None):
    result = account(provider, platform)
    home = pathlib.Path(home) if home is not None else pathlib.Path.home()
    try:
        if provider == "muse":
            credential = muse_credentials(home)
        elif provider == "antigravity":
            credential = antigravity_credentials(home)
        else:
            credential = cursor_credentials(home, platform)
        if credential is None:
            return result
        result.update(source=credential.source + " on " + {"mac": "Mac", "windows": "Windows", "ubuntu": "Ubuntu"}[platform],
                      email=credential.email, status="ok", message=None)
        if provider == "antigravity":
            fetch_antigravity(credential, result, home)
        elif provider == "muse":
            fetch_muse(credential, result, home)
        else:
            fetch_cursor(credential, result)
        result["fetchedAt"] = utc_now()
        if result["windows"] and result["sampledAt"] is None:
            result["sampledAt"] = result["fetchedAt"]
        if not result["windows"] and result["status"] == "ok":
            result["status"], result["message"] = "unavailable", "The account is signed in, but the service returned no recognized usage limits."
    except UsageError as error:
        result["status"], result["message"] = error.status, error.message
        result["windows"] = []
    except Exception:
        result["status"], result["message"] = "error", "The saved account usage could not be collected."
        result["windows"] = []
    return result


def main():
    parser = argparse.ArgumentParser(description="Read usage for an existing desktop account")
    parser.add_argument("--provider", choices=tuple(PROVIDERS), required=True)
    parser.add_argument("--platform", choices=("ubuntu", "mac", "windows"),
                        default="mac" if sys.platform == "darwin" else "windows" if sys.platform == "win32" else "ubuntu")
    args = parser.parse_args()
    print(json.dumps(collect(args.provider, args.platform), ensure_ascii=True, allow_nan=False, separators=(",", ":")))


if __name__ == "__main__":
    main()
