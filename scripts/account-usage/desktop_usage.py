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
import urllib.parse

from desktop_helpers import (
    Credential, PROVIDERS, UsageError, account, config_home, counter, email, jwt_claims,
    nonnegative, number, omp_credentials, percent, quota_window, read_json,
    request_json, reset_at, safe_text, secret, sqlite_rows, utc_now,
)

MUSE_URL = "https://api.meta.ai/muse-code/key"
AGY_BASE = "https://cloudcode-pa.googleapis.com/v1internal:"
CURSOR_SUMMARY_URL = "https://cursor.com/api/usage-summary"
CURSOR_PROFILE_URL = "https://cursor.com/api/auth/me"


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


def normalize_muse(payload):
    usage = payload.get("subs_usage")
    windows = []
    portal = payload.get("subscription_quota")
    if isinstance(portal, dict):
        for key, prefix, label in (("window", "window", "Rolling usage"), ("weekly", "weekly", "Weekly usage")):
            used = counter(portal.get(prefix + "_weighted_used"))
            limit = counter(portal.get(prefix + "_weighted_limit"))
            duration = counter(portal.get("window_duration_secs")) if key == "window" else None
            minutes = duration / 60 if duration is not None else 10080 if key == "weekly" else None
            if key == "window" and minutes is not None and 0 < minutes <= 525600:
                label = str(int(minutes / 60)) + "-hour usage" if minutes % 60 == 0 else "Rolling usage"
            reset_value = portal.get(prefix + "_resets_at")
            reset = reset_at(counter(reset_value) if isinstance(reset_value, str) and counter(reset_value) is not None else reset_value)
            if used is None and reset is None:
                continue
            value = (used / limit * 100) if used is not None and limit is not None and limit > 0 else None
            windows.append(quota_window(key, label, used_percent=value, reset=reset, minutes=minutes,
                                        used=used, limit=limit, unit="weighted tokens"))
        return windows
    if isinstance(usage, dict):
        for key, label, duration in (("window", "Rolling usage", None), ("weekly", "Weekly usage", 10080)):
            bucket = usage.get(key)
            if isinstance(bucket, dict):
                duration = number(bucket.get("window_duration_mins", duration))
                if key == "window" and duration is not None and 0 < duration <= 525600:
                    label = (str(int(duration / 60)) + "-hour usage") if duration % 60 == 0 else str(duration).rstrip("0").rstrip(".") + "-minute usage"
                window = quota_window(key, label, used_percent=bucket.get("used_percent"),
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
                                  identity_loader=lambda: verify_device_identity(credential, bound_email, bound_plan),
                                  bound_team=team)
        if cached:
            result.update(email=bound_email, plan=bound_plan, status="cached" if cached["cached"] else "ok", message=cached["message"],
                          sampledAt=cached["sampledAt"], windows=normalize_muse({"subscription_quota": cached["quota"]}))
            result["source"] += " + authenticated Meta web quota"
            return
    except ImportError:
        pass
    except MuseError as error:
        if error.code not in {"no_browser_cookie", "local_storage_error"}:
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
        if not result["email"] or not result["plan"] or bound_email.lower() != result["email"].lower() or bound_plan != result["plan"]:
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
    return [quota_window("google-ai-credits", "Google AI credits", kind="balance", remaining=remaining, unit="credits")]


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


def fetch_antigravity(credential, result, home=None):
    now = dt.datetime.now(dt.timezone.utc).timestamp() * 1000
    refreshed = False
    if credential.expires is not None and credential.expires <= now + 30000:
        credential = refresh_antigravity(credential, home)
        refreshed = True
    try:
        fetch_antigravity_quota(credential, result)
    except UsageError as error:
        if error.status != "needs_sign_in" or refreshed or not credential.refresh:
            raise
        fetch_antigravity_quota(refresh_antigravity(credential, home), result)


def fetch_antigravity_quota(credential, result):
    # The control plane selects Antigravity quota/project behavior by native
    # client User-Agent. A generic UA returns Gemini eligibility tiers instead.
    headers = {"Authorization": "Bearer " + credential.access, "User-Agent": "antigravity/1.0.0"}
    load = request_json(AGY_BASE + "loadCodeAssist", headers,
                        {"metadata": {"ideType": "ANTIGRAVITY", "platform": "PLATFORM_UNSPECIFIED", "pluginType": "GEMINI"}})
    project = load.get("cloudaicompanionProject")
    if isinstance(project, dict):
        project = project.get("id") or project.get("projectId")
    project = safe_text(project) or credential.project
    if project is None:
        raise UsageError("unavailable", "The signed-in account did not return an Antigravity quota project.")
    tier = load.get("paidTier") or load.get("currentTier")
    info = load.get("planInfo")
    result["plan"] = (safe_text(info.get("planType")) if isinstance(info, dict) else None) or (
        safe_text(tier.get("name")) or safe_text(tier.get("id")) if isinstance(tier, dict) else None)
    payload = request_json(AGY_BASE + "retrieveUserQuotaSummary", headers, {"project": project})
    result["windows"] = normalize_antigravity(payload) + normalize_antigravity_credits(load)


def cursor_cookie(credential):
    claims = jwt_claims(credential.access)
    subject = claims.get("sub")
    if not isinstance(subject, str) or not subject.strip() or len(subject) > 256:
        raise UsageError("needs_sign_in", "The saved Cursor account has no usable session identity.")
    user_id = subject.split("|")[-1].strip()
    if not user_id or any(ord(char) < 32 for char in user_id):
        raise UsageError("needs_sign_in", "The saved Cursor account has no usable session identity.")
    return "WorkosCursorSessionToken=" + urllib.parse.quote(user_id + "::" + credential.access, safe="")


def normalize_cursor(payload):
    usage = payload.get("individualUsage")
    if not isinstance(usage, dict):
        return []
    reset = payload.get("billingCycleEnd") or payload.get("endOfMonth") or payload.get("resetsAt") or payload.get("nextReset")
    windows = []
    for key, label in (("plan", "Plan spend"), ("onDemand", "On-demand spend")):
        bucket = usage.get(key)
        if not isinstance(bucket, dict):
            continue
        used, limit = nonnegative(bucket.get("used")), nonnegative(bucket.get("limit"))
        reported_percentage = percent(bucket.get("totalPercentUsed"))
        spend_percentage = min(100, used / limit * 100) if used is not None and limit is not None and limit > 0 else None
        # Cursor can report model allowance percentages that differ from its
        # dollars-spent ratio. Keep those two server measures in separate rows.
        if reported_percentage is not None:
            windows.append(quota_window(key + "-reported", "Included usage" if key == "plan" else "On-demand usage",
                                        used_percent=reported_percentage, reset=reset,
                                        enabled=bucket.get("enabled"), unlimited=payload.get("isUnlimited") if key == "plan" else bucket.get("unlimited")))
        if key == "plan":
            for field, detail_label in (("autoPercentUsed", "Cursor models"), ("apiPercentUsed", "Other models")):
                if percent(bucket.get(field)) is not None:
                    windows.append(quota_window(field, detail_label, used_percent=bucket[field], reset=reset,
                                                enabled=bucket.get("enabled"), unlimited=payload.get("isUnlimited")))
        if used is not None:
            windows.append(quota_window(key, label, used_percent=spend_percentage, reset=reset,
                                        used=used / 100 if used is not None else None,
                                        limit=limit / 100 if limit is not None else None, unit="USD",
                                        remaining=nonnegative(bucket.get("remaining")) / 100 if nonnegative(bucket.get("remaining")) is not None else None,
                                        kind="spend" if key == "plan" else "extra_usage",
                                        enabled=bucket.get("enabled"), unlimited=bucket.get("unlimited")))
    if not windows and isinstance(usage.get("overall"), dict):
        value = dict(payload)
        value["individualUsage"] = {"plan": usage["overall"]}
        return normalize_cursor(value)
    return windows


def fetch_cursor(credential, result):
    headers = {"Cookie": cursor_cookie(credential)}
    payload = request_json(CURSOR_SUMMARY_URL, headers)
    result["plan"] = safe_text(payload.get("membershipType")) or credential.plan
    result["windows"] = normalize_cursor(payload)
    try:
        profile = request_json(CURSOR_PROFILE_URL, headers)
        user = profile.get("user") if isinstance(profile.get("user"), dict) else profile
        result["email"] = email(user.get("email")) or credential.email
    except UsageError:
        # A separate identity endpoint failing must not erase a successful quota.
        pass


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
