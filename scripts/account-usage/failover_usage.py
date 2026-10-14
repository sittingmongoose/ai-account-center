"""Bounded private protocol for own-host account usage failover.

The dashboard supplies only public source digests. This dispatcher reacquires
the matching local credential and verifies its identity/revision before and
after collection. Tokens and local paths never enter a protocol response.
Packaged modules may be loaded from the dashboard's SSH stdin bundle.
"""

import contextlib
import datetime as dt
import importlib
import json
import math
import os
from pathlib import Path
import re
import sys

MAX_REQUEST = 64 * 1024
MAX_RESPONSE = 512 * 1024
MAX_CANDIDATES = 128
MAX_WINDOWS = 256
DIGEST = re.compile(r"^[a-f0-9]{64}$")
SELECTOR = re.compile(r"^[A-Za-z0-9][A-Za-z0-9:_.@-]{0,159}$")
CANDIDATE_KEYS = {"locator", "selectors", "identity", "revision", "email", "plan"}
FAILURES = {
    "unavailable", "not_installed", "needs_sign_in", "identity_mismatch", "account_mismatch",
    "source_changed", "rate_limited", "invalid_response", "network_error", "provider_error",
    "error", "busy", "no_browser_cookie", "local_storage_error", "identity_unavailable",
    "plan_mismatch", "team_mismatch", "choose_team", "inactive_subscription",
}
KINDS = {
    "codex": {"codex-principal-v1"}, "claude": {"claude-account-org-v1"},
    "antigravity": {"antigravity-subject"}, "opencode-wallet": {"opencode-workspace"},
    "muse": {"muse-team", "muse-native-credential"}, "cursor": {"cursor-subject", "cursor-credential"},
    "kimi-code": {"kimi-code-credential"}, "qwen": {"qwen-session"},
    "zai": {"zai-credential"}, "opencode-go": {"opencode-go-credential"},
}
LABELS = {"codex": "Codex", "claude": "Claude", "antigravity": "Antigravity",
          "muse": "Muse Code", "cursor": "Cursor", "kimi-code": "Kimi Code",
          "qwen": "Qwen", "zai": "Z.ai", "opencode-go": "OpenCode Go", "opencode-wallet": "OpenCode"}
UTC = dt.timezone.utc


class _Discard:
    def write(self, value):
        return len(value)

    def flush(self):
        pass


def _text(value, maximum=160):
    if (not isinstance(value, str) or not value.strip() or len(value) > maximum or
            re.search(r"[\x00-\x1f\x7f]|(?:bearer\s|sk[-_]|dca:|eyJ[A-Za-z0-9_-]{8}|access_token|refresh_token|https?://)", value, re.I)):
        return None
    return value.strip()


def _email(value):
    value = _text(value, 254)
    return value if value and re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", value) else None


def _number(value, nonnegative=False):
    if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value):
        return None
    return value if not nonnegative or value >= 0 else None


def _timestamp(value, observation=False):
    if not isinstance(value, str) or len(value) > 64 or not re.match(r"^\d{4}-\d{2}-\d{2}T", value):
        return None
    try:
        instant = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if instant.tzinfo is None or not 2000 <= instant.year <= 2200:
            return None
        if observation and instant > dt.datetime.now(UTC) + dt.timedelta(minutes=5):
            return None
        return instant.astimezone(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except (ValueError, OverflowError):
        return None


def public_candidate(value, provider, request=False):
    """Reconstruct six fields, excluding every adapter's credential material."""
    if not isinstance(value, dict) or provider not in KINDS:
        return None
    if request and set(value) != CANDIDATE_KEYS:
        return None
    if any(not isinstance(value.get(key), str) or not DIGEST.fullmatch(value[key]) for key in ("locator", "revision")):
        return None
    identity = value.get("identity")
    if (not isinstance(identity, dict) or set(identity) != {"kind", "key"} or
            identity.get("kind") not in KINDS[provider] or
            not isinstance(identity.get("key"), str) or not DIGEST.fullmatch(identity["key"])):
        return None
    selectors = value.get("selectors")
    if (not isinstance(selectors, list) or not 1 <= len(selectors) <= 32 or
            any(not isinstance(selector, str) or not SELECTOR.fullmatch(selector) for selector in selectors)):
        return None
    address, plan = value.get("email"), value.get("plan")
    if address is not None and _email(address) is None or plan is not None and _text(plan) is None:
        return None
    return {"locator": value["locator"], "selectors": list(dict.fromkeys(selectors)),
            "identity": {"kind": identity["kind"], "key": identity["key"]},
            "revision": value["revision"], "email": _email(address), "plan": _text(plan)}


def _window(value):
    if not isinstance(value, dict):
        return None
    key, label = _text(value.get("key"), 64), _text(value.get("label"), 80)
    if not key or not label:
        return None
    result = {"key": key, "label": label}
    for field in ("usedPercent", "remainingPercent", "windowMinutes", "used", "limit", "remaining"):
        raw = value.get(field)
        parsed = _number(raw, nonnegative=field != "remaining")
        if raw is not None and parsed is None:
            return None
        if field == "remainingPercent" and parsed is not None and parsed > 100:
            return None
        result[field] = parsed
    for field in ("resetAt", "expiresAt"):
        raw = value.get(field)
        parsed = _timestamp(raw)
        if raw is not None and parsed is None:
            return None
        result[field] = parsed
    unit = value.get("unit")
    if unit is not None and _text(unit, 40) is None:
        return None
    result["unit"] = _text(unit, 40)
    kind = value.get("kind")
    if kind is not None:
        if kind not in ("rate_limit", "balance", "spend", "extra_usage"):
            return None
        result["kind"] = kind
    for field in ("unlimited", "enabled"):
        if field in value:
            if not isinstance(value[field], bool):
                return None
            result[field] = value[field]
    if value.get("status") == "cached":
        sampled = _timestamp(value.get("sampledAt"), observation=True)
        if sampled is None:
            return None
        result.update(status="cached", sampledAt=sampled)
    pool = _text(value.get("poolId"), 160)
    if pool:
        result["poolId"] = pool
    if value.get("poolIdSource") in ("provider-id", "provider-bucket-membership"):
        result["poolIdSource"] = value["poolIdSource"]
    pool_label = _text(value.get("poolLabel"), 80)
    if pool_label:
        result["poolLabel"] = pool_label
    models = value.get("modelIds")
    if isinstance(models, list) and len(models) <= 64:
        result["modelIds"] = [item for item in models if _text(item, 160) and
                              re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,159}", item)]
    if not any(result.get(field) is not None for field in (
            "usedPercent", "remainingPercent", "used", "limit", "remaining", "unlimited", "enabled")):
        return None
    return result


def _reported_plan(value):
    if not isinstance(value, dict):
        return None
    result = {}
    plan_type = _text(value.get("planType"), 80)
    if plan_type:
        result["planType"] = plan_type
    for name in ("paidTier", "currentTier"):
        tier = value.get(name)
        if isinstance(tier, dict):
            normalized = {field: text for field in ("id", "name") if (text := _text(tier.get(field), 120))}
            if normalized:
                result[name] = normalized
    return result or None


def public_sample(value, provider, platform):
    """Rebuild a bounded DashboardAccount; unknown/raw fields never pass through."""
    if not isinstance(value, dict) or value.get("status") not in ("ok", "cached"):
        return None
    actual_provider = "opencode-go" if provider == "opencode-wallet" else provider
    if value.get("provider") != actual_provider or value.get("platform") != platform:
        return None
    sampled = _timestamp(value.get("sampledAt"), observation=True)
    fetched = _timestamp(value.get("fetchedAt"), observation=True)
    if not sampled or not fetched or sampled > fetched:
        return None
    address, plan = value.get("email"), value.get("plan")
    if address is not None and _email(address) is None or plan is not None and _text(plan) is None:
        return None
    raw_windows = value.get("windows")
    if not isinstance(raw_windows, list) or not 1 <= len(raw_windows) <= MAX_WINDOWS:
        return None
    windows, seen = [], set()
    for raw in raw_windows:
        window = _window(raw)
        if not window or window["key"] in seen:
            return None
        if window.get("status") == "cached" and window["sampledAt"] > fetched:
            return None
        seen.add(window["key"])
        windows.append(window)
    label = LABELS[provider]
    result = {"id": "native:" + actual_provider + ":" + platform,
              "provider": actual_provider, "providerLabel": label, "label": label,
              "email": _email(address), "plan": _text(plan), "platform": platform,
              "source": label + " usage on " + {"ubuntu": "Ubuntu", "mac": "Mac", "windows": "Windows"}[platform],
              "status": value["status"], "message": None, "sampledAt": sampled, "fetchedAt": fetched,
              "isActive": False, "windows": windows,
              "capabilities": {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []}}
    if provider == "antigravity":
        reported = _reported_plan(value.get("reportedPlan"))
        if reported:
            result["reportedPlan"] = reported
    if provider == "claude":
        availability = value.get("optionalExtras")
        if (isinstance(availability, dict) and
                all(availability.get(key) in ("ok", "unavailable") for key in ("resetCredits", "prepaidBalance"))):
            result["optionalExtras"] = {key: availability[key] for key in ("resetCredits", "prepaidBalance")}
            context = value.get("optionalExtrasContext")
            if isinstance(context, str) and re.fullmatch(r"[a-f0-9]{64}", context):
                result["optionalExtrasContext"] = context
    return result


def _adapter(provider):
    name = ("core" if provider in ("codex", "claude") else
            "special" if provider in ("antigravity", "opencode-wallet") else "additional")
    return importlib.import_module("failover_" + name)


def _scope(value, fallback):
    if value is None:
        return fallback
    if (not isinstance(value, str) or not value or len(value) > 4096 or
            re.search(r"[\x00-\x1f\x7f]", value) or not Path(value).is_absolute()):
        raise ValueError()
    return Path(os.path.abspath(value))


def _inventory(adapter, provider, home, platform, ccs):
    values = adapter.inventory(provider, home, platform, ccs_dir=ccs)
    if not isinstance(values, list) or len(values) > 512:
        return [], []
    # Conflicting identities at one locator cannot silently pick the first login.
    found, ambiguous = {}, set()
    for value in values[:MAX_CANDIDATES]:
        public = public_candidate(value, provider)
        if public is None:
            continue
        locator = public["locator"]
        previous = found.get(locator)
        if previous and previous[0] != public:
            ambiguous.add(locator)
        else:
            found[locator] = (public, value)
    statuses = getattr(values, "sign_in_needed_selectors", []) if provider == "claude" else []
    statuses = (list(dict.fromkeys(selector for selector in statuses
                                  if isinstance(selector, str) and
                                  re.fullmatch(r"claude-(?:desktop-)?profile:[A-Za-z0-9][A-Za-z0-9_-]{0,63}", selector)))
                if isinstance(statuses, list) and len(statuses) <= MAX_CANDIDATES * 2 else [])
    return [entry for locator, entry in found.items() if locator not in ambiguous], statuses


def _failure(code="unavailable", candidate=None, retry=None):
    result = {"version": 1, "action": "collect", "candidate": candidate,
              "sample": None, "failureCode": code if code in FAILURES else "unavailable"}
    retry = _number(retry, nonnegative=True)
    if retry is not None and retry <= 86400:
        result["retryAfterSeconds"] = retry
    return result


def _same(left, right):
    return all(left.get(key) == right.get(key) for key in ("locator", "identity", "revision"))


def _run(request):
    if not isinstance(request, dict):
        return _failure("invalid_response")
    action = request.get("action")
    inventory_error = {"version": 1, "action": "inventory", "candidates": []}
    failed = inventory_error if action == "inventory" else _failure("invalid_response")
    provider = request.get("provider")
    allowed = {"version", "action", "provider", "ccsDir", "homeDir"} | ({"candidate"} if action == "collect" else set())
    if (type(request.get("version")) is not int or request["version"] != 1 or
            action not in ("inventory", "collect") or not isinstance(provider, str) or provider not in KINDS or
            not set(request).issubset(allowed)):
        return failed
    wanted = public_candidate(request.get("candidate"), provider, request=True) if action == "collect" else None
    if action == "collect" and wanted is None:
        return failed
    home = _scope(request.get("homeDir"), Path.home())
    ccs = _scope(request.get("ccsDir"), home / ".ccs")
    platform = "windows" if sys.platform == "win32" else "mac" if sys.platform == "darwin" else "ubuntu"
    adapter = _adapter(provider)
    before, sign_in_needed = _inventory(adapter, provider, home, platform, ccs)
    if action == "inventory":
        return {"version": 1, "action": "inventory", "candidates": [public for public, _ in before],
                **({"signInNeededSelectors": sign_in_needed} if sign_in_needed else {})}
    selected = next(((public, private) for public, private in before if _same(public, wanted)), None)
    if selected is None:
        return _failure("source_changed")
    public, private = selected
    # Caller display metadata must not substitute for current source identity.
    # It remains only selection data; collection always receives the private row.
    result = adapter.collect(private, home, platform, ccs_dir=ccs)
    after, _ = _inventory(adapter, provider, home, platform, ccs)
    if not any(_same(current, public) for current, _ in after):
        return _failure("source_changed")
    if not isinstance(result, dict):
        return _failure("invalid_response", public)
    code = result.get("failureCode")
    if code is not None:
        return _failure(code, public, result.get("retryAfterSeconds"))
    sample = public_sample(result.get("sample"), provider, platform)
    if sample is None:
        return _failure("invalid_response", public)
    return {"version": 1, "action": "collect", "candidate": public, "sample": sample, "failureCode": None}


def run(request):
    """One request, one safe response. Adapter stdout/stderr are never forwarded."""
    try:
        with contextlib.redirect_stdout(_Discard()), contextlib.redirect_stderr(_Discard()):
            response = _run(request)
        encoded = json.dumps(response, ensure_ascii=True, allow_nan=False, separators=(",", ":"))
        if len(encoded.encode("ascii")) > MAX_RESPONSE:
            raise ValueError()
        return response
    except Exception:
        return ({"version": 1, "action": "inventory", "candidates": []}
                if isinstance(request, dict) and request.get("action") == "inventory" else _failure())


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError()
        result[key] = value
    return result


def _invalid(_):
    raise ValueError()


def main():
    try:
        raw = sys.stdin.buffer.read(MAX_REQUEST + 1)
        if len(raw) > MAX_REQUEST:
            raise ValueError()
        request = json.loads(raw, object_pairs_hook=_unique, parse_constant=_invalid)
        result = run(request)
    except Exception:
        result = _failure("invalid_response")
    print(json.dumps(result, ensure_ascii=True, allow_nan=False, separators=(",", ":")))


if __name__ == "__main__":
    main()
