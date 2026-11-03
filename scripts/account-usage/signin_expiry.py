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
are unreadable (they read as `unavailable`); argparse exits 2 on bad arguments.
"""

import argparse
import ctypes
import datetime
import itertools
import json
import os
import pathlib
import re
import stat
import sys
import time

SCHEMA_VERSION = 1
WARN_SECONDS = 24 * 3600
CRITICAL_SECONDS = 4 * 3600
# Enumerating every config dir is bounded; a machine with more roots than this
# reports the first MAX_ROOTS and stays honest about the rest by not guessing.
MAX_ROOTS = 64


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
    """UTC ISO-8601 for epoch milliseconds, or None. Milliseconds are kept:
    a stamp truncated to its second can predate a collection that started
    later in that same second, hiding a deadline that crossed within it."""
    if ms is None:
        return None
    stamp = datetime.datetime.fromtimestamp(
        ms / 1000, datetime.timezone.utc
    ).isoformat(timespec="milliseconds")
    return stamp.replace("+00:00", "Z")


def logged_out_at(refresh_ms, access_ms):
    """The login's end time, reported only when the data supports it.

    The exact end is the first access expiry that falls after the refresh
    expiry; the current access expiry only bounds it. Without a future
    access expiry to name, the end is unknown rather than guessed."""
    if refresh_ms is None or access_ms is None:
        return None
    if access_ms < refresh_ms:
        return None
    return access_ms


def capability_state(data, now_ms):
    """(end_ms, state) for one credentials payload, from what the tokens
    can actually still do.

    A login with no refresh token cannot renew: it ends at the access
    expiry (critical while it lasts, logged_out after). Access missing with
    a live refresh token means the next run renews: warn, not healthy.
    Tokens present with no readable times at all are unavailable, not ok.
    With empty tokens the retained expiry fields are remnants, never a
    named logout event in the future."""
    access_ms, refresh_ms = data["access"], data["refresh"]
    has_access, has_refresh = data["has_access"], data["has_refresh"]
    if not (has_access or has_refresh):
        return None, "logged_out"
    if (has_access and access_ms is None) or (has_refresh and refresh_ms is None):
        # A token whose own expiry cannot be read leaves the login's real
        # end unknown in both directions: unavailable, never a guess.
        return None, "unavailable"
    if not has_refresh:
        if access_ms is None:
            return None, "unavailable"
        if access_ms <= now_ms:
            return access_ms, "logged_out"
        return access_ms, "critical"
    if not has_access:
        if refresh_ms is not None and refresh_ms <= now_ms:
            return None, "logged_out"
        return None, "warn"
    end = logged_out_at(refresh_ms, access_ms)
    if end is not None:
        if end <= now_ms:
            return end, "logged_out"
        remaining = (end - now_ms) / 1000
        if remaining < CRITICAL_SECONDS:
            return end, "critical"
        if remaining < WARN_SECONDS:
            return end, "warn"
        return end, "ok"
    if refresh_ms is not None:
        if refresh_ms <= now_ms:
            return None, "logged_out"
        if (refresh_ms - now_ms) / 1000 < WARN_SECONDS:
            return None, "warn"
    return None, "ok"


def read_credentials(path):
    """The two times and token presence from one credentials file, or None.

    The file is read but only its times and token presence are kept; token
    values never leave this function."""
    try:
        # O_NOFOLLOW refuses a linked credentials file: a link outside the
        # selected home can never be read and attributed to this profile.
        descriptor = os.fdopen(os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)), "rb")
    except OSError:
        return None
    try:
        with descriptor:
            raw = json.loads(descriptor.read(65536).decode("utf-8"))
    except (OSError, ValueError, RecursionError):
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


def _home_relative(config_dir, home):
    """(posix, windows) forms of config_dir relative to home, or None."""
    try:
        rel = pathlib.Path(config_dir).relative_to(home)
    except ValueError:
        return None
    if not str(rel):
        return None
    return "~/" + rel.as_posix(), "~\\" + str(rel).replace("/", "\\")


def fix_command(config_dir, home):
    """An executable re-login command for this machine's shell. A dir under
    the home publishes as a $HOME-relative form the target expands at run
    time; a dir outside it (a relocated CCS scope) is the one deliberate
    exception and publishes as a quoted literal, as the README states."""
    relative = _home_relative(config_dir, home)
    if sys.platform == "win32":
        if relative is None:
            literal = str(config_dir).replace("'", "''")
            return f"$env:CLAUDE_CONFIG_DIR = '{literal}'; claude auth login"
        escaped = relative[1][1:].replace("`", "``").replace('"', '`"').replace("$", "`$")
        return f'$env:CLAUDE_CONFIG_DIR = "$HOME{escaped}"; claude auth login'
    if relative is None:
        literal = str(config_dir).replace("'", "'\\''")
        return f"CLAUDE_CONFIG_DIR='{literal}' claude auth login"
    escaped = (
        relative[0][1:]
        .replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("$", "\\$")
        .replace("`", "\\`")
    )
    return f'CLAUDE_CONFIG_DIR="$HOME{escaped}" claude auth login'


def _within_allowed(path, home, ccs_dir=None):
    """True when the real path resolves inside the home or inside the
    explicitly configured CCS directory: a linked profile can never
    smuggle in a store from anywhere else (Windows reparse points
    included, through realpath), while a relocated CCS scope keeps its
    saved profiles."""
    try:
        real = os.path.realpath(path)
        roots = [os.path.realpath(home)]
        if ccs_dir is not None:
            roots.append(os.path.realpath(ccs_dir))
    except OSError:
        return False
    return any(
        real == root or real.startswith(root if root.endswith(os.sep) else root + os.sep)
        for root in roots
    )


def _dir_state(path):
    """(is_directory, read_failed) for one path. Path.is_dir() cannot be
    used for this: it swallows permission and I/O errors into False, which
    would make an unreadable profile indistinguishable from an absent
    one. A missing path is not a failure; an unreadable one — including a
    dangling symlink whose target is gone — is."""
    try:
        mode = path.stat().st_mode
    except (FileNotFoundError, NotADirectoryError):
        try:
            path.lstat()
        except OSError:
            return False, False
        return False, True
    except OSError:
        return False, True
    return stat.S_ISDIR(mode), False


def claude_profile(name, config_dir, now_ms, home, ccs_dir=None):
    """One config dir's sign-in state; missing/unreadable files read as unavailable."""
    relative = _home_relative(config_dir, home)
    # The default dir is what a bare re-login writes, so it names no
    # CLAUDE_CONFIG_DIR: on macOS the login and the Keychain item stay the
    # same one this collector reads.
    fix = "claude auth login" if pathlib.Path(config_dir) == home / ".claude" else fix_command(config_dir, home)
    entry = {
        "name": name,
        "configDir": relative[0] if relative else str(config_dir),
        "provider": "claude",
        "accessExpiresAt": None,
        "refreshExpiresAt": None,
        "loggedOutAt": None,
        "state": "unavailable",
        "fixCommand": fix,
    }
    if not _within_allowed(config_dir, home, ccs_dir):
        entry["error"] = "profile directory resolves outside the allowed roots"
        return entry
    credentials_path = pathlib.Path(config_dir) / ".credentials.json"
    # The file itself is checked too: O_NOFOLLOW does not exist on Windows,
    # where a linked credentials file must not be followed out either.
    if not _within_allowed(credentials_path, home, ccs_dir):
        entry["error"] = "credentials file missing or unreadable"
        return entry
    data = read_credentials(credentials_path)
    if data is None:
        entry["error"] = "credentials file missing or unreadable"
        return entry
    end, state = capability_state(data, now_ms)
    if state == "unavailable":
        entry["error"] = "expiry times unreadable"
    # A time is published only for a token that exists: a blanked token's
    # retained expiry field is a remnant, and the renderers read these
    # stamps as evidence of token presence.
    entry.update(
        accessExpiresAt=iso(data["access"]) if data["has_access"] else None,
        refreshExpiresAt=iso(data["refresh"]) if data["has_refresh"] else None,
        loggedOutAt=iso(end),
        state=state,
    )
    return entry


def _mac_keychain_text():
    """The Keychain item's JSON through Security's noninteractive C API.

    The `security` CLI can prompt for keychain access; over ssh that prompt
    hangs until the timeout and the primary store never reports. User
    interaction is explicitly disabled for this lookup, so it either reads
    or fails immediately."""
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
        status_code = find(None, len(service), service, 0, None,
                           ctypes.byref(length), ctypes.byref(pointer), None)
        if status_code != 0:
            return None
        try:
            if not 0 < length.value <= 65536:
                return None
            return ctypes.string_at(pointer, length.value).decode("utf-8")
        finally:
            security.SecKeychainItemFreeContent(None, pointer)
    except (OSError, ValueError, UnicodeError, AttributeError):
        return None


def mac_keychain_profile(now_ms):
    """macOS: Claude Desktop stores the same JSON in the login Keychain."""
    entry = {
        "name": "keychain",
        "configDir": "Keychain: 'Claude Code-credentials'",
        "provider": "claude",
        "accessExpiresAt": None,
        "refreshExpiresAt": None,
        "loggedOutAt": None,
        "state": "unavailable",
        "fixCommand": "claude auth login",
        "error": "keychain item unreadable",
    }
    if sys.platform != "darwin":
        entry.pop("error")
        return entry
    raw = _mac_keychain_text()
    if raw is None:
        return entry
    data = read_credentials_from_text(raw)
    if data is None:
        entry["error"] = "keychain item holds no readable credentials"
        return entry
    entry.pop("error")
    end, state = capability_state(data, now_ms)
    if state == "unavailable":
        entry["error"] = "expiry times unreadable"
    # The same token-presence rule as file profiles: a blanked token's
    # retained expiry field is a remnant and is never published.
    entry.update(
        accessExpiresAt=iso(data["access"]) if data["has_access"] else None,
        refreshExpiresAt=iso(data["refresh"]) if data["has_refresh"] else None,
        loggedOutAt=iso(end),
        state=state,
    )
    return entry


def read_credentials_from_text(raw):
    """Same shape as read_credentials, from an in-memory JSON document."""
    try:
        parsed = json.loads(raw)
    except (ValueError, RecursionError):
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


def codex_profile(now_ms, home=None):
    """Codex: its auth.json carries tokens and a last_refresh time but no
    published end-of-login time, so no `logged out at` can be computed; the
    entry reports what exists instead of guessing. Only the field NAMES are
    published, never values."""
    home = home or pathlib.Path.home()
    for candidate in (home / ".codex" / "auth.json",):
        present = os.path.lexists(candidate)
        # The same containment as Claude stores: a linked auth.json outside
        # the home is never followed, even though only key names publish.
        # A file that exists but cannot be read is unseen, not absent: it
        # reports unavailable instead of vanishing.
        unreadable = {
            "name": "codex",
            "provider": "codex",
            "configDir": "~/.codex/auth.json",
            "state": "unavailable",
            "error": "auth file missing or unreadable",
            "fixCommand": "",
        }
        if present and not _within_allowed(candidate, home):
            return unreadable
        try:
            descriptor = os.fdopen(
                os.open(candidate, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)), "rb"
            )
            with descriptor as handle:
                raw = json.loads(handle.read(65536).decode("utf-8"))
        except (OSError, ValueError, RecursionError):
            return unreadable if present else None
        keys = sorted(raw.keys()) if isinstance(raw, dict) else []
        relative = _home_relative(candidate, home)
        return {
            "name": "codex",
            "provider": "codex",
            "configDir": relative[0] if relative else "~/.codex/auth.json",
            "state": "unsupported",
            "fieldsAvailable": keys,
            "fixCommand": "",
        }
    return None


def _saved_roots(home, ccs_dir):
    """Saved instance homes and settings-defined homes through the sibling
    discovery; None when that module is unavailable (a plain standalone run
    still covers the default and T3 dirs below)."""
    try:
        import failover_core

        return failover_core._roots(home, "claude", ccs_dir)
    except Exception:  # discovery is best effort by contract; never fatal
        return None


def claude_roots(home, ccs_dir=None):
    """Every Claude config dir this machine knows: the default, the saved
    ~/.ccs/instances homes, the T3 account dirs, and settings-defined homes.

    The saved-instance and T3 dirs are enumerated here even when the
    sibling discovery is available, because its name filter refuses valid
    directory names (dotted and email-shaped names, apostrophes); display
    names are sanitized later, never at discovery. Returns (roots, complete): complete is False when
    the sibling discovery was unavailable or enumeration hit the bound, so
    the report can say the list may be short instead of implying all."""
    default = home / ".claude"
    roots = [("default", default)]
    seen = {os.path.realpath(default)}
    complete = True
    # The sibling discovery records every swallowed read failure and
    # truncation through desktop_helpers; a count that moves while it runs
    # means this list may be short even when it looks comfortably small.
    try:
        import desktop_helpers

        failures_before = desktop_helpers.total_read_failures()
    except Exception:
        desktop_helpers = None
        failures_before = None
    try:
        import failover_core

        settings_before = failover_core.SETTINGS_HOME_FAILURES[0]
    except Exception:
        failover_core = None
        settings_before = None
    pairs = _saved_roots(home, ccs_dir or home / ".ccs")
    if pairs is not None and failures_before is not None:
        try:
            if desktop_helpers.total_read_failures() > failures_before:
                complete = False
        except Exception:
            complete = False
    if pairs is not None and settings_before is not None:
        try:
            if failover_core.SETTINGS_HOME_FAILURES[0] > settings_before:
                complete = False
        except Exception:
            complete = False
    if pairs is not None:
        # The sibling discovery truncates at its own bound; entries beyond it
        # vanish silently unless the length gives it away.
        try:
            import failover_core

            limit = getattr(failover_core, "MAX_SOURCES", None)
        except Exception:
            limit = None
        if limit is not None and len(pairs) >= limit:
            complete = False
        for root, selectors in pairs:
            is_dir, stat_failed = _dir_state(root)
            if stat_failed:
                complete = False
                continue
            if not is_dir:
                continue
            text_path = os.path.realpath(root)
            if text_path in seen:
                continue
            seen.add(text_path)
            label = root.name or text_path
            if selectors and isinstance(selectors[0], str) and selectors[0].startswith(
                "claude-profile:"
            ):
                label = selectors[0].split(":", 1)[1]
            roots.append((label, root))
    else:
        complete = False

    def scan(base):
        # The collector's own enumeration, with no name filter beyond
        # dot-prefixed entries: the sibling's filter refuses directory
        # names the app accepts (dotted and email-shaped names,
        # apostrophes), for saved instances exactly as for T3 dirs.
        nonlocal complete
        is_dir, stat_failed = _dir_state(base)
        if stat_failed:
            complete = False
            return
        if not is_dir:
            return
        try:
            children = list(itertools.islice(base.iterdir(), MAX_ROOTS + 1))
        except OSError:
            complete = False
            return
        if len(children) > MAX_ROOTS:
            complete = False
            children = children[:MAX_ROOTS]
        for child in sorted(children, key=lambda item: item.name):
            child_is_dir, child_failed = _dir_state(child)
            if child_failed:
                complete = False
                continue
            if not child_is_dir or child.name.startswith("."):
                continue
            text_path = os.path.realpath(child)
            if text_path in seen:
                continue
            seen.add(text_path)
            roots.append((child.name, child))

    scan((ccs_dir or home / ".ccs") / "instances")
    scan(home / ".claude-t3")
    if len(roots) > MAX_ROOTS:
        complete = False
        roots = roots[:MAX_ROOTS]
    return roots, complete


def collect(home, now_ms, include_keychain=True, ccs_dir=None):
    """One machine's report. include_keychain=False keeps a --home fixture
    run from touching the real login Keychain at all; ccs_dir selects the
    saved-instance scope on installs that relocate it."""
    roots, complete = claude_roots(home, ccs_dir)
    allowed_ccs = ccs_dir or home / ".ccs"
    profiles = [
        claude_profile(name, config_dir, now_ms, home, allowed_ccs)
        for name, config_dir in roots
    ]
    if not complete:
        profiles.append(
            {
                "name": "saved-profiles",
                "provider": "claude",
                "configDir": "discovery",
                "accessExpiresAt": None,
                "refreshExpiresAt": None,
                "loggedOutAt": None,
                "state": "unavailable",
                "fixCommand": "",
                "error": "profile discovery was incomplete; this machine's list may be missing homes",
            }
        )
    if include_keychain and sys.platform == "darwin":
        keychain = mac_keychain_profile(now_ms)
        profiles.append(keychain)
    codex = codex_profile(now_ms, home)
    if codex:
        profiles.append(codex)
    return {
        "schemaVersion": SCHEMA_VERSION,
        "generatedAt": iso(now_ms),
        "profiles": profiles,
    }


def run():
    """Entry point for the in-memory helper transport: the real machine's
    default home, including the Keychain probe. A relocated CCS scope on
    this host is honoured with the app's own precedence (config-manager:
    CCS_DIR names the directory itself; CCS_HOME carries a .ccs inside)."""
    home = pathlib.Path.home()
    raw_dir = os.environ.get("CCS_DIR")
    raw_home = os.environ.get("CCS_HOME")
    if raw_dir:
        ccs = pathlib.Path(os.path.abspath(raw_dir))
    elif raw_home:
        ccs = pathlib.Path(os.path.abspath(raw_home)) / ".ccs"
    else:
        ccs = None
    return collect(home, int(time.time() * 1000), ccs_dir=ccs)


def main():
    parser = argparse.ArgumentParser(description="Read-only sign-in expiry report")
    parser.add_argument("--home", type=pathlib.Path, default=None,
                        help="Home directory override (tests); skips the Keychain")
    parser.add_argument("--ccs-dir", type=pathlib.Path, default=None,
                        help="Saved-instance scope override (tests)")
    args = parser.parse_args()
    now_ms = int(time.time() * 1000)
    home = args.home or pathlib.Path.home()
    print(json.dumps(
        collect(home, now_ms, include_keychain=args.home is None, ccs_dir=args.ccs_dir),
        ensure_ascii=True, allow_nan=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
