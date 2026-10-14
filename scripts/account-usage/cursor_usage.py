"""Read-only Cursor plan and quota collection shared by native and failover paths.

Schema evidence: Cursor 3.23.23's DashboardService protobuf definitions and
usageDataService, plus cursor.com's public getFullStripeStatus client. The
Connect JSON transport is deliberately not a guessed website REST route.
"""

import urllib.parse

from desktop_helpers import email, jwt_claims, nonnegative, percent, quota_window, reset_at, utc_now
from plan_common import CollectionError

SUMMARY_URL = "https://cursor.com/api/usage-summary"
PROFILE_URL = "https://cursor.com/api/auth/me"
MEMBERSHIP_URL = "https://cursor.com/api/auth/stripe"
RPC_BASE = "https://api2.cursor.sh/aiserver.v1.DashboardService/"
PLAN_INFO_URL = RPC_BASE + "GetPlanInfo"
CURRENT_USAGE_URL = RPC_BASE + "GetCurrentPeriodUsage"
NEW_PLANS = frozenset(("plus", "premium", "super", "unified_ultra"))
LEGACY_PLANS = frozenset(("free", "pro", "pro_plus", "ultra", "enterprise", "free_trial",
                          "pro_student", "express", "team", "business"))


def membership(value):
    """Preserve server tier identity without exposing arbitrary provider strings."""
    if not isinstance(value, str) or not value or len(value) > 64 or not value.isascii():
        return None
    if not all(character.isalnum() or character == "_" for character in value):
        return None
    value = value.lower()
    return value if value in NEW_PLANS or value in LEGACY_PLANS else "unknown"


def cookie(credential):
    subject = jwt_claims(credential.access).get("sub")
    if not isinstance(subject, str) or not subject.strip() or len(subject) > 256:
        raise CollectionError("needs_sign_in", "The saved Cursor account has no usable session identity.")
    user_id = subject.split("|")[-1].strip()
    if not user_id or any(ord(character) < 32 for character in user_id):
        raise CollectionError("needs_sign_in", "The saved Cursor account has no usable session identity.")
    return "WorkosCursorSessionToken=" + urllib.parse.quote(user_id + "::" + credential.access, safe="")


def normalize_legacy(payload):
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
                                        used=used / 100, limit=limit / 100 if limit is not None else None, unit="USD",
                                        remaining=nonnegative(bucket.get("remaining")) / 100 if nonnegative(bucket.get("remaining")) is not None else None,
                                        kind="spend" if key == "plan" else "extra_usage",
                                        enabled=bucket.get("enabled"), unlimited=bucket.get("unlimited")))
    if not windows and isinstance(usage.get("overall"), dict):
        value = dict(payload)
        value["individualUsage"] = {"plan": usage["overall"]}
        return normalize_legacy(value)
    return windows


def _period(plan_info):
    info = plan_info.get("planInfo")
    if not isinstance(info, dict):
        return None
    period = info.get("includedUsagePeriod")
    if isinstance(period, bool):
        return None
    if period in (2, "INCLUDED_USAGE_PERIOD_WEEKLY"):
        return "weekly"
    if period in (1, "INCLUDED_USAGE_PERIOD_MONTHLY"):
        return "monthly"
    return None


def _milliseconds_reset(value):
    # Protobuf int64 JSON is a decimal string. This field is milliseconds,
    # independently of the monthly billing cycle timestamps.
    if isinstance(value, str) and value.isascii() and value.isdigit() and len(value) <= 16:
        value = int(value)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return reset_at(value / 1000)


def normalize_shared(payload, plan_info, plan):
    if plan_info.get("isMerged") is False:
        return []
    bucket = payload.get("planUsage")
    value = percent(bucket.get("totalPercentUsed")) if isinstance(bucket, dict) else None
    if value is None:
        return []
    # A merged account's total is the native client's shared pool. Legacy auto
    # and API percentages and dollar limits do not describe this allowance.
    label = "Grok" if plan == "plus" else "Grok and Composer"
    info = plan_info.get("planInfo") if isinstance(plan_info.get("planInfo"), dict) else {}
    period = _period(plan_info)
    if period == "weekly":
        reset = _milliseconds_reset(info.get("includedUsageResetsAt"))
    elif period == "monthly":
        reset = _milliseconds_reset(payload.get("billingCycleEnd")) or _milliseconds_reset(info.get("billingCycleEnd"))
    else:
        reset = None
    return [quota_window("grok-shared-" + (period or "usage"), label, used_percent=value,
                         reset=reset, minutes=10080 if period == "weekly" else None)]


def normalize_additional(payload):
    usage = payload.get("individualUsage")
    if not isinstance(usage, dict) or not isinstance(usage.get("onDemand"), dict):
        return []
    # Only independently reported spend is retained, with its own billing
    # reset. An included allowance never becomes additional spend by renaming.
    windows = normalize_legacy({"individualUsage": {"onDemand": usage["onDemand"]},
                                "billingCycleEnd": payload.get("billingCycleEnd")})
    result = [window for window in windows if window["kind"] == "extra_usage"]
    for window in result:
        window["label"] = "Additional usage spend"
    return result


def observation(result, failure_code=None):
    # An explicit account conflict invalidates all newly read metadata.
    if failure_code == "account_mismatch":
        return {}
    plan, at = result.get("observedPlan"), result.get("observedPlanAt")
    if plan in NEW_PLANS | LEGACY_PLANS | {"unknown"} and isinstance(at, str) and reset_at(at):
        return {"observedPlan": plan, "observedPlanAt": at}
    return {}


def _cooldown(error):
    if error.failure_code == "rate_limited":
        return {"failureCode": "rate_limited", "retryAfterSeconds": error.retry_after or 600}
    return {}


def collect(credential, result, client):
    """Mutate only the sanitized sample; retain verified plan on quota failure."""
    result["plan"] = None
    headers = {"Cookie": cookie(credential)}
    stripe = client.get(MEMBERSHIP_URL, headers)
    plan = membership(stripe.get("membershipType"))
    if plan is None:
        raise CollectionError("unavailable", "Cursor did not report a recognized subscription identity.",
                              failure_code="invalid_response")
    result.update(plan=plan, observedPlan=plan, observedPlanAt=utc_now())
    rpc_headers = {"Authorization": "Bearer " + credential.access, "Content-Type": "application/json",
                   "Connect-Protocol-Version": "1"}
    info = {}
    if plan == "unknown":
        info = client.get(PLAN_INFO_URL, rpc_headers, body=b"{}")
        if info.get("isMerged") is not True:
            raise CollectionError("unavailable", "Cursor returned an unsupported subscription type.",
                                  failure_code="invalid_response")
    if plan in NEW_PLANS or plan == "unknown":
        payload = client.get(CURRENT_USAGE_URL, rpc_headers, body=b"{}")
        result["windows"] = normalize_shared(payload, info, plan)
        if not result["windows"]:
            raise CollectionError("unavailable", "Cursor did not report its shared usage percentage.",
                                  failure_code="invalid_response")
        if plan != "unknown":
            try:
                info = client.get(PLAN_INFO_URL, rpc_headers, body=b"{}")
            except CollectionError as error:
                result["message"] = "Usage is current; the usage reset is unavailable."
                if _cooldown(error):
                    return {**observation(result), **_cooldown(error)}
            else:
                if info.get("isMerged") is False:
                    result["windows"] = []
                    raise CollectionError("unavailable", "Cursor returned inconsistent subscription metadata.",
                                          failure_code="invalid_response")
                result["windows"] = normalize_shared(payload, info, plan)
        try:
            summary = client.get(SUMMARY_URL, headers)
            result["windows"].extend(normalize_additional(summary))
        except CollectionError as error:
            result["message"] = "Included usage is current; additional usage spend is unavailable."
            if _cooldown(error):
                return {**observation(result), **_cooldown(error)}
    else:
        payload = client.get(SUMMARY_URL, headers)
        result["windows"] = normalize_legacy(payload)
    try:
        profile = client.get(PROFILE_URL, headers)
    except CollectionError as error:
        return {**observation(result), **_cooldown(error)}
    user = profile.get("user") if isinstance(profile.get("user"), dict) else profile
    address = email(user.get("email"))
    if address and credential.email and address.lower() != credential.email.lower():
        result.pop("observedPlan", None)
        result.pop("observedPlanAt", None)
        result["plan"] = None
        raise CollectionError("unavailable", "The saved account identity could not be verified.",
                              failure_code="account_mismatch")
    if address:
        result["email"] = address
    return observation(result)
