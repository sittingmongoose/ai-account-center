"""Regenerate synthetic Claude normalization cases from the Python oracle (claude_usage.py).

python3 crates/aac-agent/tests/fixtures/claude-usage.py > crates/aac-agent/tests/fixtures/claude-usage.json
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "scripts/account-usage"))
import claude_usage as m  # noqa: E402

NOW = 1795716000.0
T = "2026-11-26T20:00:00Z"
cases = []


def add(kind, value, expected, **extra):
    cases.append({"kind": kind, "input": value, "expected": expected, **extra})


stamps = [None, 0, "", T, "2026-11-26T20:00:00.123456+00:00", "2026-11-26T20:00:00.1234567891Z",
          "2026-11-26T20:00Z", "2026-11-26T20Z", "2026-11-26 20:00:00+05", "2026-11-26T20:00:00+0530",
          "20261126T200000Z", "2026-11-26T20:00:00,5Z", "2026-11-26Z", "2026-11-26T24:00:00Z",
          "2026-11-26t20:00:00Z", "2026-11-26T20:00:00", "1999-12-31T23:00:00-02:00",
          "2200-12-31T23:00:00Z", "2201-01-01T00:00:00Z", "2026-02-30T00:00:00Z", "x" * 70,
          "2026-11-26T20:00:00+05:30:15", "2026-11-26T20:60:00Z", "2026-11-26T20:00:00.Z"]
for s in stamps:
    add("reset_at", s, m.reset_at(s))

usages = [
    {},
    {"five_hour": {"utilization": 12, "resets_at": T}, "seven_day": {"utilization": 40.5, "resets_at": None}},
    {"five_hour": {"utilization": True}, "seven_day": {"utilization": -1}, "seven_day_opus": {"utilization": 150}},
    {"seven_day_fable": {"utilization": 3, "expires_at": T}, "seven_day_x": "bad", "nimbus_quill": {"utilization": 9},
     "seven_day_" + "a" * 60: {"utilization": 1}},
    {"limits": [{"kind": "session", "percent": 10, "resets_at": T},
                {"kind": "weekly_all", "group": "weekly", "percent": 20.25},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 5,
                 "scope": {"model": {"id": "x", "display_name": "Fable"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 6,
                 "scope": {"model": {"display_name": "Opus 4"}, "surface": {"id": "claude_code"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 7, "scope": {"surface": "Cowork App"}},
                {"kind": "weekly_scoped", "group": "session", "percent": 7, "scope": {"surface": "Cowork App"}},
                {"kind": "daily_all", "group": "daily", "percent": 1},
                {"kind": "mystery_scoped", "group": "weekly", "percent": 2},
                {"kind": "Bad-Kind", "percent": 3},
                {"kind": "session", "percent": 4, "scope": "global"},
                {"kind": "session", "percent": 4, "scope": {"model": None}},
                {"kind": "session", "percent": 4, "scope": {"model": {"display_name": "X"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 1,
                 "scope": {"model": {"display_name": "Fable", "extra": 1}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 1,
                 "scope": {"model": {"display_name": "sk-secret"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": None,
                 "scope": {"model": {"display_name": "Fable"}}},
                "notadict"]},
    {"seven_day": {"utilization": 1},
     "limits": [{"kind": "weekly_all", "group": "weekly", "percent": 99},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 5,
                 "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 5.0,
                 "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 8,
                 "scope": {"model": {"display_name": "Sonnet"}}},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 9,
                 "scope": {"model": {"display_name": "Sonnet"}}}]},
    {"limits": [{"kind": "session", "percent": 1}] * 65},
    {"extra_usage": {"is_enabled": True, "used_credits": 1234, "monthly_limit": 5000, "utilization": 24.68,
                     "currency": "USD", "resets_at": T, "is_unlimited": False}},
    {"extra_usage": {"is_enabled": False, "used_credits": 3, "monthly_limit": 2}},
    {"extra_usage": {"is_enabled": True}},
    {"extra_usage": {"is_enabled": "yes", "used_credits": 3}},
    {"extra_usage": {"is_enabled": True, "used_credits": 10, "monthly_limit": 4, "currency": "usd", "utilization": 250}},
]
for u in usages:
    add("windows", u, m.normalize_windows(u))
    add("reset_windows", u, m.reset_windows(u))

credits = [
    None, {}, {"eligible": False, "grants": []}, {"eligible": True, "grants": []},
    {"eligible": True, "grants": [{"resets_left": 2, "resets_total": 3, "paused": False, "ends_at": "2027-01-01T00:00:00Z",
                                   "clears": ["five_hour", "seven_day", "five_hour"]},
                                  {"resets_left": 0, "resets_total": 1, "paused": False},
                                  {"resets_left": 1, "paused": True},
                                  {"resets_left": 1, "paused": False, "starts_at": "2027-06-01T00:00:00Z"},
                                  {"resets_left": 1, "paused": False, "ends_at": "2026-01-01T00:00:00Z"},
                                  {"resets_left": 1, "resets_total": None, "paused": False, "clears": []}]},
    {"eligible": True, "grants": [{"resets_left": 1.0, "paused": False}]},
    {"eligible": True, "grants": [{"resets_left": 3, "resets_total": 2, "paused": False}]},
    {"eligible": True, "grants": [{"resets_left": 1, "paused": False, "ends_at": "garbage"}]},
    {"eligible": True, "grants": [{"resets_left": 1, "paused": False, "clears": ["Bad"]}]},
    {"eligible": True, "grants": [{"resets_left": 30, "paused": False}, {"resets_left": 30, "paused": False}]},
    {"eligible": True, "grants": [{"resets_left": 1, "paused": False}] * 21},
    {"eligible": True, "grants": ["x"]},
]
for c in credits:
    add("reset_credits", c, m.normalize_reset_credits(c, NOW), now=NOW)

for a in [None, {}, {"eligible": True, "available": True, "resets_per_week": 2, "next_available_at": T},
          {"eligible": True, "available": False, "resets_per_week": 1},
          {"eligible": True, "available": "no", "resets_per_week": 1},
          {"eligible": True, "available": True, "resets_per_week": 0},
          {"eligible": True, "available": True, "resets_per_week": 2.0}]:
    add("session_allowance", a, m.normalize_session_reset_allowance(a))

for p in [None, {}, {"amount": 1250, "currency": "USD", "next_expires_at": T}, {"amount": 7},
          {"amount": 7, "currency": "usd"}, {"amount": -1}, {"amount": 3.5, "currency": None}]:
    add("prepaid", p, m.normalize_prepaid(p))

plans = [
    ({}, {}, None), ({}, {"plan_display_name": "Ultra"}, None), ({"plan_type": "claude_max"}, None, None),
    ({"has_claude_max": True}, {}, None), ({"has_claude_pro": True, "has_claude_max": False}, {}, None),
    ({"has_claude_team_plus": True}, {}, None), ({"has_claude_a": True, "has_claude_b": True, "has_claude_max": True}, {}, None),
    ({}, {"tier": " Max 20x "}, None), ({}, {"tier": "bad‎plan"}, "pro"), ({}, {"tier": "Bearer abc"}, "fallback"),
    ({}, {"rate_limit_tier": "default_claude_max_20x"}, None), ({}, {"rate_limit_tier": "pro_tier"}, "Max"),
    ({}, {}, "Team Pro"), ({}, {}, "maximum"), ({}, {}, "Max 5x"), ({}, {}, "x" * 81),
]
for account, org, fallback in plans:
    name = m.claude_plan(account, org, fallback)
    add("plan", {"account": account, "organization": org, "fallback": fallback},
        {"plan": name, "planClass": m.claude_plan_class(account, org, name)})

for answer in [None, {}, {"result": "reset"}, {"result": "not_limited"}, {"result": "ineligible", "reason": "expired"},
               {"result": "ineligible", "reason": "surface"}, {"result": "ineligible", "reason": "new"},
               {"result": "ineligible"}, {"result": "cooldown"}, {"result": "other"}]:
    add("claim_outcome", answer, m.claim_outcome(answer))

print(json.dumps(cases, indent=1, sort_keys=True))
