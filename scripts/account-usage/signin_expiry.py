#!/usr/bin/env python3
"""Read-only sign-in expiry collector for Claude OAuth logins.

Every Claude Code login carries two fixed times in its config dir's
`.credentials.json`: `expiresAt` (the access token, about 8 hours) and
`refreshTokenExpiresAt` (the hard end of the whole login; renewing the access
token never pushes it back). Once the refresh time has passed, the next access
expiry ends the login: Claude Code blanks the tokens and the next run needs a
fresh `claude auth login`.

"Logged out at" is therefore the first `expiresAt` that falls after
`refreshTokenExpiresAt`. The collector reports that time, a state
(ok / warn / critical / logged_out) and the exact fix command for every
config dir on this machine — the default `~/.claude`, the per-account
`~/.claude-t3/<name>` dirs, and on macOS the Keychain item Claude Desktop
uses. Token values are never read beyond their presence and never printed.

Other providers are probed for readable login end times too; a provider whose
store carries no such time is reported as `unsupported` rather than guessed.

Output: one JSON object on stdout, nothing else. Exit 0 even when some roots
are unreadable (they read as `unavailable`); exit 1 only on bad arguments.
"""

import argparse
import datetime
import json
import os
import pathlib
import re
import sys

SCHEMA_VERSION = 1
WARN_SECONDS = 24 * 3600
CRITICAL_SECONDS = 4 * 3600
ACCESS_TOKEN_LIFETIME_MS = 8 * 3600 * 1000


# A representable OAuth epoch: 2001-09-09 (1e12 ms) through Nov 2286 (1e13 ms).
EPOCH_MS_MIN, EPOCH_MS_MAX = 10 ** 12, 10 ** 13


def epoch_ms(value):
    """Milliseconds since the epoch for a finite, representable number or
    numeric string; anything out of range or non-finite reads as absent."""
    if isinstance(value, bool):
        return None
    try:
        if isinstance(value, (int, float)):
            ms = int(value)
        elif isinstance(value, str) and re.fullmatch(r"\d{1,16}", value.strip()):
            ms = int(value.strip())
        else:
            return None
    except (ValueError, OverflowError):
        return None
    if not EPOCH_MS_MIN <= ms <= EPOCH_MS_MAX:
        return None
    return ms


def iso(ms):
    """UTC ISO-8601 for epoch milliseconds, or None."""
    if ms is None:
        return None
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def logged_out_at(refresh_ms, access_ms, now_ms):
    """The login's end time, reported only when the data supports it.

    The exact end is the first access expiry that falls after the refresh
    expiry; the current access expiry only bounds it. Without a future
    access expiry to name, the end is unknown rather than guessed."""
    if refresh_ms is None or access_ms is None:
        return None
    if access_ms < refresh_ms:
        return None
    return access_ms


def state_for(logged_out_ms, now_ms, has_tokens, refresh_ms=None):
    if not has_tokens:
        return "logged_out"
    if logged_out_ms is not None:
        if logged_out_ms <= now_ms:
            return "logged_out"
        remaining = (logged_out_ms - now_ms) / 1000
        if remaining < CRITICAL_SECONDS:
            return "critical"
        if remaining < WARN_SECONDS:
            return "warn"
        return "ok"
    # No end is derivable from the pair: the refresh expiry still bounds the
    # login, so it degrades the state instead of claiming healthy.
    if refresh_ms is not None:
        if refresh_ms <= now_ms:
            return "logged_out"
        if (refresh_ms - now_ms) / 1000 < WARN_SECONDS:
            return "warn"
    return "ok"


def read_credentials(path):
    """The two times and token presence from one credentials file, or None.

    The file is read but only its times and token presence are kept; token
    values never leave this function."""
    try:
        with open(path, "rb") as handle:
            raw = json.loads(handle.read(65536).decode("utf-8"))
    except (OSError, ValueError):
        return None
    credentials = raw.get("claudeAiOauth", raw) if isinstance(raw, dict) else None
    if not isinstance(credentials, dict):
        return None
    return {
        "access": epoch_ms(credentials.get("expiresAt")),
        "refresh": epoch_ms(credentials.get("refreshTokenExpiresAt")),
        "has_access": isinstance(credentials.get("accessToken"), str) and bool(credentials["accessToken"]),
        "has_refresh": isinstance(credentials.get("refreshToken"), str) and bool(credentials["refreshToken"]),
    }


def claude_profile(name, config_dir, now_ms, fix_template):
    """One config dir's sign-in state; missing/unreadable files read as unavailable."""
    entry = {
        "name": name,
        "configDir": str(config_dir),
        "provider": "claude",
        "accessExpiresAt": None,
        "refreshExpiresAt": None,
        "loggedOutAt": None,
        "state": "unavailable",
        "fixCommand": fix_template.format(name=name, config_dir=str(config_dir)),
    }
    data = read_credentials(pathlib.Path(config_dir) / ".credentials.json")
    if data is None:
        return entry
    has_tokens = data["has_access"] or data["has_refresh"]
    end = logged_out_at(data["refresh"], data["access"], now_ms)
    entry.update(
        accessExpiresAt=iso(data["access"]),
        refreshExpiresAt=iso(data["refresh"]),
        loggedOutAt=iso(end),
        state=state_for(end, now_ms, has_tokens, data["refresh"]),
    )
    return entry


def mac_keychain_profile(now_ms, fix_template):
    """macOS: Claude Desktop stores the same JSON in the login Keychain."""
    entry = {
        "name": "keychain",
        "configDir": "Keychain: 'Claude Code-credentials'",
        "provider": "claude",
        "accessExpiresAt": None,
        "refreshExpiresAt": None,
        "loggedOutAt": None,
        "state": "unavailable",
        "fixCommand": fix_template.format(name="keychain", config_dir="keychain"),
    }
    if sys.platform != "darwin":
        return entry
    import subprocess

    try:
        raw = subprocess.run(
            ["security", "find-generic-password", "-s", "Claude Code-credentials", "-w"],
            capture_output=True, timeout=10, check=True,
        ).stdout.decode("utf-8")
        data = read_credentials_from_text(raw)
    except (OSError, ValueError, subprocess.SubprocessError):
        return entry
    if data is None:
        return entry
    has_tokens = data["has_access"] or data["has_refresh"]
    end = logged_out_at(data["refresh"], data["access"], now_ms)
    entry.update(
        accessExpiresAt=iso(data["access"]),
        refreshExpiresAt=iso(data["refresh"]),
        loggedOutAt=iso(end),
        state=state_for(end, now_ms, has_tokens, data["refresh"]),
    )
    return entry


def read_credentials_from_text(raw):
    """Same shape as read_credentials, from an in-memory JSON document."""
    try:
        parsed = json.loads(raw)
    except ValueError:
        return None
    credentials = parsed.get("claudeAiOauth", parsed) if isinstance(parsed, dict) else None
    if not isinstance(credentials, dict):
        return None
    return {
        "access": epoch_ms(credentials.get("expiresAt")),
        "refresh": epoch_ms(credentials.get("refreshTokenExpiresAt")),
        "has_access": isinstance(credentials.get("accessToken"), str) and bool(credentials["accessToken"]),
        "has_refresh": isinstance(credentials.get("refreshToken"), str) and bool(credentials["refreshToken"]),
    }


def codex_profile(now_ms, fix_template, home=None):
    """Codex: its auth.json carries tokens and a last_refresh time but no
    published end-of-login time, so no `logged out at` can be computed; the
    entry reports what exists instead of guessing. Only the field NAMES are
    published, never values."""
    home = home or pathlib.Path.home()
    for candidate in (home / ".codex" / "auth.json",):
        try:
            with open(candidate, "rb") as handle:
                raw = json.loads(handle.read(65536).decode("utf-8"))
        except (OSError, ValueError):
            continue
        keys = sorted(raw.keys()) if isinstance(raw, dict) else []
        return {
            "name": "codex",
            "provider": "codex",
            "configDir": str(candidate),
            "state": "unsupported",
            "fieldsAvailable": keys,
            "fixCommand": "",
        }
    return None


def claude_roots(home):
    """The default config dir plus every ~/.claude-t3/<name> account dir."""
    roots = [("default", home / ".claude")]
    t3 = home / ".claude-t3"
    if t3.is_dir():
        for child in sorted(t3.iterdir()):
            if child.is_dir() and not child.name.startswith("."):
                roots.append((child.name, child))
    return roots


def collect(home, now_ms):
    # Quoted so a path with spaces stays one variable; the codex probe is not
    # a Claude login, so its row carries no fix command at all.
    fix_template = "CLAUDE_CONFIG_DIR='{config_dir}' claude auth login"
    profiles = [
        claude_profile(name, config_dir, now_ms, fix_template)
        for name, config_dir in claude_roots(home)
    ]
    keychain = mac_keychain_profile(now_ms, "claude auth login")
    if keychain["state"] != "unavailable" or sys.platform == "darwin":
        profiles.append(keychain)
    codex = codex_profile(now_ms, fix_template, home)
    if codex:
        profiles.append(codex)
    return {
        "schemaVersion": SCHEMA_VERSION,
        "generatedAt": iso(now_ms),
        "profiles": profiles,
    }


def main():
    parser = argparse.ArgumentParser(description="Read-only sign-in expiry report")
    parser.add_argument("--home", type=pathlib.Path, default=pathlib.Path.home(),
                        help="Home directory override (tests)")
    args = parser.parse_args()
    now_ms = int(time.time() * 1000)
    print(json.dumps(collect(args.home, now_ms), ensure_ascii=True, allow_nan=False))
    return 0


if __name__ == "__main__":
    import time

    sys.exit(main())
