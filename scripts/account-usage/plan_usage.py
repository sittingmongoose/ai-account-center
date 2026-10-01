#!/usr/bin/env python3
"""Usage-only Kimi Code, Qwen Token Plan, Z.ai and OpenCode Go collector.

Run locally on the computer holding the existing credential. Exactly one public
DashboardAccount DTO is printed; credentials never leave the process except in
authenticated requests to each provider's fixed HTTPS usage endpoint.
"""

import argparse
import datetime as dt
import hashlib
import json
import math
from pathlib import Path
import re
import time
import urllib.parse
import uuid

from plan_common import (CollectionError, HttpClient, PROVIDERS, account, credentials,
                         number, percent, plan_label, platform_name, reset_at, usage_window, utc_now)


def _valid_windows(windows):
    result = [window for window in windows if any(window.get(key) is not None for key in
              ("usedPercent", "used", "limit", "remaining", "resetAt", "expiresAt", "unlimited", "enabled"))]
    if not result:
        raise CollectionError("error", "The usage service returned no valid plan quota windows.")
    return result


def parse_go(payload):
    usage = payload.get("usage")
    if not isinstance(usage, dict):
        raise CollectionError("error", "The usage service returned an invalid plan quota response.")
    windows = []
    for key, label, minutes in (("rolling", "5 hours", 300), ("weekly", "Weekly", 10080),
                                ("monthly", "Monthly", None)):
        item = usage.get(key)
        if not isinstance(item, dict) or item.get("status") not in ("ok", "rate-limited") or percent(item.get("percent")) is None:
            raise CollectionError("error", "The usage service returned an incomplete plan quota response.")
        windows.append(usage_window(key, label, item["percent"], item.get("resetsAt"), minutes, kind="rate_limit"))
    return "Go", windows


def _kimi_counts(detail):
    limit = number(detail.get("limit"))
    used = number(detail.get("used"))
    remaining = number(detail.get("remaining"))
    if limit is not None and limit > 0:
        if used is None and remaining is not None and remaining <= limit:
            used = limit - remaining
        if used is not None:
            return percent(100 * used / limit), used, limit
    return None, used, limit


def _kimi_reset(detail):
    return next((detail.get(key) for key in ("resetTime", "resetAt", "reset_time", "reset_at")
                 if reset_at(detail.get(key)) is not None), None)


def _kimi_booster(payload):
    wallet = payload.get("boosterWallet")
    if not isinstance(wallet, dict):
        return []
    monthly_limit = wallet.get("monthlyChargeLimit")
    monthly_used = wallet.get("monthlyUsed")
    monthly_limit = monthly_limit if isinstance(monthly_limit, dict) else {}
    monthly_used = monthly_used if isinstance(monthly_used, dict) else {}
    currency = monthly_limit.get("currency") or monthly_used.get("currency")
    currency = currency if isinstance(currency, str) and re.fullmatch(r"[A-Z]{3}", currency) else None
    scale = 100 if currency else 1
    unit = currency or "cents"
    result = []
    balance = wallet.get("balance")
    if isinstance(balance, dict) and balance.get("type") == "BOOSTER":
        total_raw, left_raw = number(balance.get("amount")), number(balance.get("amountLeft"))

        def cents(raw):
            if raw is None:
                return None
            value = math.trunc(raw) / 1000000
            return 1 if 0 < value < 1 else math.floor(value + .5)

        total, left = cents(total_raw), cents(left_raw)
        if total is not None and total > 0:
            used = total - left if left is not None and left <= total else None
            ratio = 100 * used / total if used is not None else None
            result.append(usage_window("booster-balance", "Booster balance", ratio,
                                        used=used / scale if used is not None else None,
                                        limit=total / scale, remaining=left / scale if left is not None else None,
                                        unit=unit, kind="balance"))
    limit, used = number(monthly_limit.get("priceInCents")), number(monthly_used.get("priceInCents"))
    enabled = wallet.get("monthlyChargeLimitEnabled")
    if limit is not None or used is not None or isinstance(enabled, bool):
        ratio = percent(100 * used / limit) if used is not None and limit is not None and limit > 0 else None
        remaining = limit - used if limit is not None and used is not None and used <= limit else None
        result.append(usage_window("booster-monthly-spend", "Monthly booster spend", ratio,
                                    used=used / scale if used is not None else None,
                                    limit=limit / scale if limit is not None else None,
                                    remaining=remaining / scale if remaining is not None else None,
                                    unit=unit, kind="spend", enabled=enabled))
    return result


def parse_kimi(payload):
    windows = {}
    usage = payload.get("usage")
    if isinstance(usage, dict):
        ratio, used, limit = _kimi_counts(usage)
        windows["weekly"] = usage_window("weekly", "Weekly", ratio, _kimi_reset(usage), 10080, used, limit,
                                          kind="rate_limit", remaining=usage.get("remaining"))
    limits = payload.get("limits")
    if isinstance(limits, list):
        for index, item in enumerate(limits[:30]):
            if not isinstance(item, dict):
                continue
            detail, window = item.get("detail"), item.get("window")
            if not isinstance(detail, dict) or not isinstance(window, dict):
                continue
            duration = number(window.get("duration"))
            unit = window.get("timeUnit")
            multiplier = {"TIME_UNIT_MINUTE": 1, "TIME_UNIT_HOUR": 60, "TIME_UNIT_DAY": 1440, "TIME_UNIT_WEEK": 10080,
                          "TIME_UNIT_SECOND": 1 / 60}.get(unit)
            minutes = duration * multiplier if duration is not None and multiplier is not None else None
            if minutes == 300:
                key, label = "5h", "5 hours"
            elif minutes == 10080:
                key, label = "weekly", "Weekly"
            else:
                key, label = "limit-{}".format(index), "Plan limit {}".format(index + 1)
            ratio, used, limit = _kimi_counts(detail)
            if ratio is not None or _kimi_reset(detail) is not None or used is not None or limit is not None:
                windows[key] = usage_window(key, label, ratio, _kimi_reset(detail), minutes, used, limit,
                                            kind="rate_limit", remaining=detail.get("remaining"))
    usages = payload.get("usages")
    if isinstance(usages, dict):
        for upstream, key, label, minutes in (("limit_5h", "5h", "5 hours", 300),
                                              ("limit_7d", "weekly", "Weekly", 10080),
                                              ("limit_month_total", "monthly", "Monthly total", None),
                                              ("limit_month_code", "monthly-code", "Monthly coding", None)):
            item = usages.get(upstream)
            ratio = number(item.get("used_ratio")) if isinstance(item, dict) else None
            legacy = windows.get(key)
            modern_reset = reset_at(item.get("reset_time")) if isinstance(item, dict) else None
            legacy_reset = legacy.get("resetAt") if legacy else None
            if (ratio == 0 and not isinstance(usages.get("limit_month_total"), dict) and
                    legacy and legacy.get("used") is not None and legacy["used"] > 0 and
                    legacy.get("windowMinutes") == minutes and modern_reset and legacy_reset):
                first = dt.datetime.fromisoformat(modern_reset.replace("Z", "+00:00"))
                second = dt.datetime.fromisoformat(legacy_reset.replace("Z", "+00:00"))
                if abs((first - second).total_seconds()) <= 2:
                    # Some transitional responses carry an unused modern zero
                    # placeholder beside exact, same-period legacy counters.
                    continue
            if isinstance(item, dict) and ((ratio is not None and ratio <= 1) or reset_at(item.get("reset_time")) is not None):
                windows[key] = usage_window(key, label, ratio * 100 if ratio is not None and ratio <= 1 else None,
                                            item.get("reset_time"), minutes, kind="rate_limit")
    ordered = sorted(windows.values(), key=lambda item: (item["windowMinutes"] is None, item["windowMinutes"] or 0, item["key"]))
    user = payload.get("user")
    membership = user.get("membership") if isinstance(user, dict) else None
    level = membership.get("level") if isinstance(membership, dict) else None
    if payload.get("version") in (None, "GOODS_VERSION_V1"):
        plan = {"LEVEL_FREE": "Adagio", "LEVEL_TRIAL": "Andante", "LEVEL_BASIC": "Moderato",
                "LEVEL_INTERMEDIATE": "Allegretto", "LEVEL_ADVANCED": "Allegro"}.get(level)
    else:
        plan = {"LEVEL_FREE": "Free", "LEVEL_TRIAL": "Trial", "LEVEL_BASIC": "Basic",
                "LEVEL_INTERMEDIATE": "Intermediate", "LEVEL_ADVANCED": "Advanced"}.get(level)
    return plan, _valid_windows(ordered + _kimi_booster(payload))


def parse_zai(payload):
    if payload.get("success") is False or payload.get("code") not in (None, 0, 200, "0", "200"):
        raise CollectionError("error", "The usage service could not return this coding plan.")
    data = payload.get("data")
    limits = data.get("limits") if isinstance(data, dict) else None
    if not isinstance(limits, list):
        raise CollectionError("error", "The usage service returned an invalid plan quota response.")
    windows = []
    seen = set()
    for index, item in enumerate(limits[:30]):
        if not isinstance(item, dict):
            continue
        meter_type = item.get("type")
        if meter_type not in ("TOKENS_LIMIT", "CREDIT_LIMIT", "TIME_LIMIT"):
            continue
        count = number(item.get("number"))
        time_unit = item.get("unit")
        monthly_requests = meter_type == "TIME_LIMIT" and time_unit == 5 and count == 1
        # Public implementations disagree on units 4/5 outside the observed
        # monthly request marker. Preserve their quota/reset without guessing
        # whether a numeric period denotes minutes, days, or calendar months.
        multiplier = {1: 1440, 3: 60, 6: 10080}.get(time_unit)
        minutes = count * multiplier if count is not None and multiplier is not None and not monthly_requests else None
        duration_label = {1: "days", 3: "hours", 6: "weeks"}.get(time_unit)
        duration = "{} {}".format("{:g}".format(count), duration_label) if count is not None and duration_label else "Plan limit"
        if minutes == 300:
            duration = "5 hours"
        elif minutes == 10080:
            duration = "Weekly"
        elif monthly_requests:
            duration = "Monthly"
        kind = "Requests" if meter_type == "TIME_LIMIT" else "Tokens" if meter_type == "TOKENS_LIMIT" else "Credits"
        key = "{}-{}-{}".format(meter_type.lower().replace("_limit", ""), time_unit, count)
        if key in seen:
            key += "-{}".format(index)
        seen.add(key)
        used, limit = number(item.get("currentValue")), number(item.get("usage"))
        ratio = percent(item.get("percentage"))
        if used is not None and limit is not None and limit > 0:
            ratio = percent(100 * used / limit)
        windows.append(usage_window(key, duration + " · " + kind, ratio, item.get("nextResetTime"), minutes,
                                    used, limit, "requests" if meter_type == "TIME_LIMIT" else kind.lower(),
                                    kind="rate_limit", remaining=item.get("remaining")))
        # The provider reports feature counters inside the shared request quota.
        # They have no independent limit/reset, so preserve only their reported
        # counters instead of copying the parent quota's reset into each row.
        details = item.get("usageDetails")
        if meter_type == "TIME_LIMIT" and isinstance(details, list):
            labels = {"search-prime": "Search", "web-reader": "Web reader", "zread": "Zread"}
            for detail in details[:30]:
                if not isinstance(detail, dict) or detail.get("modelCode") not in labels or number(detail.get("usage")) is None:
                    continue
                code = detail["modelCode"]
                windows.append(usage_window(key + "-" + code, labels[code], used=detail["usage"],
                                            unit="requests", kind="extra_usage"))
    return plan_label(data.get("level")), _valid_windows(windows)


def _zai_pack_expiry(value):
    if isinstance(value, str) and re.fullmatch(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}", value):
        try:
            # The reset inventory's documented console timestamps are UTC+8.
            value = dt.datetime.strptime(value, "%Y-%m-%d %H:%M:%S").replace(
                tzinfo=dt.timezone(dt.timedelta(hours=8))).isoformat()
        except ValueError:
            return None
    return reset_at(value)


def parse_zai_reset_packs(payload):
    if payload.get("success") is not True or payload.get("code") not in (None, 0, 200, "0", "200"):
        raise CollectionError("error", "Reset pack inventory could not be read.")
    data = payload.get("data")
    if not isinstance(data, dict):
        raise CollectionError("error", "Reset pack inventory could not be read.")
    windows = []
    recognized = False
    for field, prefix, label in (("fiveHourResets", "5h", "5-hour"), ("weekResets", "weekly", "Weekly")):
        rows = data.get(field)
        if not isinstance(rows, list):
            continue
        recognized = True
        # Preserve known-zero availability only for an explicitly complete list.
        availability_known = len(rows) <= 100 and all(isinstance(row, dict) and isinstance(row.get("available"), bool) for row in rows)
        if availability_known:
            count = sum(1 for row in rows if row["available"])
            windows.append(usage_window("reset-packs-" + prefix, "Available " + label.lower() + " reset packs",
                                        remaining=count, unit="packs", kind="balance"))
        for index, row in enumerate(rows[:100]):
            if not isinstance(row, dict):
                continue
            expiry = _zai_pack_expiry(row.get("expireTime"))
            available = row.get("available")
            if expiry is None and not isinstance(available, bool):
                continue
            identifier = row.get("recordId")
            stable = str(identifier) if isinstance(identifier, (int, str)) and not isinstance(identifier, bool) else str(index)
            key = "reset-pack-{}-{}".format(prefix, hashlib.sha256(stable.encode()).hexdigest()[:12])
            windows.append(usage_window(key, label + " reset pack", expires_at=expiry,
                                        remaining=(1 if available else 0) if isinstance(available, bool) else None,
                                        unit="packs", kind="balance"))
    if not recognized:
        raise CollectionError("error", "Reset pack inventory could not be read.")
    return windows


def _unwrap_qwen(payload, markers=None):
    markers = markers or ("per5HourPercentage", "per1WeekPercentage", "per1MonthPercentage",
                          "per5HourResetTime", "per1WeekResetTime", "per1MonthResetTime")
    result = payload
    for _ in range(7):
        if not isinstance(result, dict):
            break
        if result.get("successResponse") is False or result.get("success") is False:
            raise CollectionError("needs_sign_in", "The Qwen console session could not access token-plan usage.")
        if any(key in result for key in markers):
            return result
        next_value = result.get("DataV2", result.get("Data", result.get("data")))
        if isinstance(next_value, str):
            try:
                next_value = json.loads(next_value)
            except ValueError:
                break
        result = next_value
    raise CollectionError("error", "The Qwen console returned no valid token-plan quota response.")


def parse_qwen(payload, subscription=None, quota_config=None, addon=None):
    data = _unwrap_qwen(payload)
    subscription_data = None
    if subscription is not None:
        try:
            subscription_data = _unwrap_qwen(subscription, ("specCode", "spec_code", "planName", "plan_name", "endTime"))
        except CollectionError:
            pass
    tier = None
    quotas = None
    if isinstance(subscription_data, dict):
        tier = subscription_data.get("specCode", subscription_data.get("spec_code"))
    if isinstance(tier, str) and quota_config is not None:
        try:
            configs = _unwrap_qwen(quota_config, (tier,))
            quotas = configs.get(tier)
        except CollectionError:
            pass
    quotas = quotas if isinstance(quotas, dict) else {}
    windows = []
    for prefix, key, label, minutes in (("per5Hour", "5h", "5 hours", 300),
                                       ("per1Week", "weekly", "Weekly", 10080),
                                       ("per1Month", "monthly", "Monthly", None)):
        ratio = number(data.get(prefix + "Percentage"))
        reset = data.get(prefix + "ResetTime")
        if ratio is None and reset_at(reset) is None:
            continue
        used_percent = ratio * 100 if ratio is not None and ratio <= 1 else ratio
        limit_keys = {"5h": ("five_hour", "fiveHour"), "weekly": ("weekly",), "monthly": ("monthly",)}[key]
        limit = next((number(quotas.get(name)) for name in limit_keys if number(quotas.get(name)) is not None), None)
        valid_percent = percent(used_percent)
        used = limit * valid_percent / 100 if limit is not None and valid_percent is not None else None
        remaining = limit - used if limit is not None and used is not None else None
        windows.append(usage_window(key, label, used_percent, reset, minutes, used=used, limit=limit,
                                    remaining=remaining, unit="credits" if limit is not None else None, kind="rate_limit"))
    plan = None
    if isinstance(subscription_data, dict):
        plan = plan_label(subscription_data.get("planName", subscription_data.get("plan_name"))) or plan_label(tier)
        expiry = subscription_data.get("endTime")
        if reset_at(expiry) is not None:
            windows.append(usage_window("subscription", "Plan subscription", expires_at=expiry, kind="rate_limit"))
    if addon is not None:
        try:
            extras = _unwrap_qwen(addon, ("remainingCredits", "totalCredits", "activeCount", "items"))
        except CollectionError:
            extras = None
        if isinstance(extras, dict):
            limit, remaining = number(extras.get("totalCredits")), number(extras.get("remainingCredits"))
            used = limit - remaining if limit is not None and remaining is not None and remaining <= limit else None
            ratio = 100 * used / limit if used is not None and limit > 0 else None
            if limit is not None or remaining is not None:
                windows.append(usage_window("addon-credits", "Additional credits", ratio, used=used,
                                            limit=limit, remaining=remaining, unit="credits", kind="balance"))
            active = number(extras.get("activeCount"))
            if active is not None:
                windows.append(usage_window("addon-packs", "Active credit packs", remaining=active,
                                            unit="packs", kind="balance"))
            items = extras.get("items")
            if isinstance(items, list):
                for index, item in enumerate(items[:100]):
                    if not isinstance(item, dict):
                        continue
                    total, remaining = number(item.get("totalCredits")), number(item.get("remainingCredits"))
                    expiry = reset_at(item.get("endTime"))
                    if total is None and remaining is None and expiry is None:
                        continue
                    spent = total - remaining if total is not None and remaining is not None and remaining <= total else None
                    ratio = spent / total * 100 if spent is not None and total > 0 else None
                    identifier = item.get("orderId")
                    identifier = str(identifier) if isinstance(identifier, (int, str)) and not isinstance(identifier, bool) else str(index)
                    key = "addon-pack-" + hashlib.sha256(identifier.encode()).hexdigest()[:12]
                    windows.append(usage_window(key, "Additional credit pack {}".format(index + 1), ratio,
                                                used=spent, limit=total, remaining=remaining, expires_at=expiry,
                                                unit="credits", kind="balance"))
                if all(isinstance(item, dict) and (number(item.get("totalCredits")) is not None or
                       number(item.get("remainingCredits")) is not None or reset_at(item.get("endTime")) is not None) for item in items):
                    windows.append(usage_window("addon-listed-packs", "Listed active credit packs", remaining=len(items),
                                                unit="packs", kind="balance"))
    return plan, _valid_windows(windows)


def _qwen_credential(secret):
    try:
        packed = json.loads(secret)
    except ValueError:
        packed = None
    if not isinstance(packed, dict) or not isinstance(packed.get("cookie"), str) or not packed["cookie"].strip():
        raise CollectionError("unavailable", "The saved Qwen API key has no console session; live token-plan usage requires its existing console cookie.")
    cookie = packed["cookie"]
    if len(cookie) > 32768 or "\r" in cookie or "\n" in cookie or "\x00" in cookie:
        raise CollectionError("unavailable", "The saved Qwen console session is invalid.")
    base = packed.get("baseUrl", "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1")
    if not isinstance(base, str):
        raise CollectionError("unavailable", "The saved Qwen token-plan region is unsupported.")
    base = base.rstrip("/")
    region = {"https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1": "intl",
              "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1": "cn"}.get(base)
    if region is None:
        raise CollectionError("unavailable", "The saved Qwen token-plan region is unsupported.")
    return cookie, region


def _qwen_gateway_cookie(secret, console_cookie):
    packed = json.loads(secret)
    cookie = packed.get("gatewayCookie", console_cookie)
    if not isinstance(cookie, str) or len(cookie) > 32768 or any(char in cookie for char in ("\r", "\n", "\x00")):
        raise CollectionError("unavailable", "The saved Qwen gateway session is invalid.")
    return cookie


def fetch_qwen(client, secret):
    cookie, region = _qwen_credential(secret)
    gateway_cookie = _qwen_gateway_cookie(secret, cookie)
    api = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage"
    if region == "intl":
        origin = "https://home.qwencloud.com"
        referer = origin + "/analytics/token-plan/individual"
        info = client.get(origin + "/tool/user/info.json", {"Cookie": cookie})
        sec_token = info.get("data", {}).get("secToken") if isinstance(info.get("data"), dict) else None
        cornerstone = {"domain": "home.qwencloud.com", "consoleSite": "QWENCLOUD", "console": "ONE_CONSOLE",
                       "xsp_lang": "en-US", "protocol": "V2", "productCode": "p_efm"}
        gateway, action, cloud_region = "https://cs-data.qwencloud.com/data/api.json", "IntlBroadScopeAspnGateway", "ap-southeast-1"
    else:
        origin = "https://bailian.console.aliyun.com"
        referer = origin + "/cn-beijing?tab=plan"
        page = client.get(referer, {"Cookie": cookie, "Accept": "text/html"}, text=True)
        match = re.search(r'SEC_TOKEN:\s*"([^"\r\n]+)"', page)
        sec_token = match.group(1) if match else None
        cornerstone = {"feTraceId": str(uuid.uuid4()), "feURL": referer + "#/efm/subscription/token-plan/personal",
                       "protocol": "V2", "console": "ONE_CONSOLE", "productCode": "p_efm", "switchUserType": 3,
                       "domain": "bailian.console.aliyun.com", "consoleSite": "BAILIAN_ALIYUN", "userNickName": "",
                       "userPrincipalName": "", "xsp_lang": "zh-CN"}
        gateway, action, cloud_region = "https://bailian-cs.console.aliyun.com/data/api.json", "BroadScopeAspnGateway", "cn-beijing"
    if not isinstance(sec_token, str) or not sec_token or len(sec_token) > 16384:
        raise CollectionError("needs_sign_in", "The saved Qwen console session has expired or is incomplete.")
    headers = {"Content-Type": "application/x-www-form-urlencoded", "Origin": origin,
               "Referer": referer, "X-Requested-With": "XMLHttpRequest"}
    if gateway_cookie:
        headers["Cookie"] = gateway_cookie
    for part in gateway_cookie.split(";"):
        name, separator, value = part.strip().partition("=")
        if separator and name in ("login_aliyunid_csrf", "csrf") and "\r" not in value and "\n" not in value:
            decoded = urllib.parse.unquote(value)
            if "\r" in decoded or "\n" in decoded or "\x00" in decoded:
                raise CollectionError("unavailable", "The saved Qwen console session is invalid.")
            headers["x-xsrf-token"] = decoded
            headers["x-csrf-token"] = decoded
            break
    def request(suffix, extra=None):
        endpoint = api.rsplit("/", 1)[0] + "/" + suffix
        data = {"cornerstoneParam": cornerstone}
        data.update(extra or {})
        params = {"Api": endpoint, "Data": data, "V": "1.0"}
        body = urllib.parse.urlencode({"product": "sfm_bailian", "action": action, "region": cloud_region,
                                       "sec_token": sec_token, "params": json.dumps(params, separators=(",", ":"))}).encode()
        query = urllib.parse.urlencode({"product": "sfm_bailian", "action": action, "api": endpoint})
        return client.get(gateway + "?" + query, headers, body=body)

    usage = request("usage")
    optional = {}
    for suffix in ("addon/list", "subscription", "quota-config"):
        try:
            extra = {"commodityCode": "sfm_tokenplansolo_public_intl"} if suffix == "subscription" and region == "intl" else None
            if suffix == "addon/list":
                extra = {"commodityCode": "sfm_tokenplansoloaddon_public_" + ("intl" if region == "intl" else "cn"),
                         "status": ["ACTIVE"], "pageNum": 1, "pageSize": 10}
            optional[suffix] = request(suffix, extra)
        except Exception:
            # An optional endpoint failure cannot erase a valid quota sample.
            optional[suffix] = None
    return usage, optional["subscription"], optional["quota-config"], optional["addon/list"]


def collect(provider, platform, home=None, client=None):
    result = account(provider, platform)
    found = credentials(provider, home)
    if not found:
        result["message"] = "No saved credential was found in this computer's existing account stores."
        return result
    selected = found[0]
    if provider == "qwen":
        # Prefer a session-bearing saved credential over plain inference keys.
        for item in found:
            try:
                _qwen_credential(item["secret"])
                selected = item
                break
            except CollectionError:
                pass
    elif provider == "kimi-code":
        now_ms = time.time() * 1000
        selected = next((item for item in found if item["expires"] is None or
                         (item["expires"] if item["expires"] > 100000000000 else item["expires"] * 1000) > now_ms), found[0])
    result["source"] = selected["source"] + " on " + {"mac": "Mac", "windows": "Windows", "ubuntu": "Ubuntu"}[platform]
    result["email"] = selected["email"]
    result["label"] = selected["email"] or result["providerLabel"]
    success_message = None
    try:
        expires = selected["expires"]
        if expires is not None and (expires / 1000 if expires > 100000000000 else expires) <= time.time():
            raise CollectionError("needs_sign_in", "The saved Kimi Code sign-in has expired; its owning application must renew it.")
        client = client or HttpClient()
        secret = selected["secret"]
        if provider == "kimi-code":
            payload = client.get("https://api.kimi.com/coding/v1/usages", {"Authorization": "Bearer " + secret})
            plan, windows = parse_kimi(payload)
        elif provider == "zai":
            payload = client.get("https://api.z.ai/api/monitor/usage/quota/limit", {"Authorization": secret,
                                 "Content-Type": "application/json"})
            plan, windows = parse_zai(payload)
            try:
                packs = client.get("https://api.z.ai/api/biz/customer-package-reset/list?targetType=PERSONAL",
                                   {"Authorization": "Bearer " + secret})
                windows.extend(parse_zai_reset_packs(packs))
            except Exception:
                # API-only accounts may not be entitled to reset packs. Never
                # turn denied/malformed inventory into a zero-card balance.
                success_message = "Account usage is current; reset pack inventory is unavailable."
        elif provider == "opencode-go":
            # Stable installation ID, derived locally without any credential input.
            installation = hashlib.sha256(str(Path.home() if home is None else home).encode()).hexdigest()[:32]
            payload = client.get("https://opencode.ai/zen/go/v1/usage", {"Authorization": "Bearer " + secret,
                                 "x-opencode-session": "ccs-usage-" + installation})
            plan, windows = parse_go(payload)
        else:
            plan, windows = parse_qwen(*fetch_qwen(client, secret))
        result.update(status="ok", message=success_message, plan=plan, windows=windows, fetchedAt=utc_now(), sampledAt=utc_now())
    except CollectionError as error:
        result.update(status=error.status, message=error.message, fetchedAt=utc_now())
    except Exception:
        # Never leak exception text: it can contain request headers or payloads.
        result.update(status="error", message="The usage collector could not read this plan's quota.", fetchedAt=utc_now())
    return result


def main():
    parser = argparse.ArgumentParser(description="Read an existing coding-plan account's usage.")
    parser.add_argument("--provider", required=True, choices=tuple(PROVIDERS))
    parser.add_argument("--platform", choices=("ubuntu", "mac", "windows"), default=platform_name())
    args = parser.parse_args()
    result = collect(args.provider, args.platform)
    print(json.dumps(result, ensure_ascii=True, separators=(",", ":"), allow_nan=False))


if __name__ == "__main__":
    main()
