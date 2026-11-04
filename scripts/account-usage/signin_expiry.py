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
login identity on this machine — the default `~/.claude`, the per-account
`~/.claude-t3/<name>` dirs, a configured home naming the default dir
(launched with an explicit selector, so a different macOS login), any
storage identity this probe process itself inherited, and the custom/local
OAuth variant identities when their selectors are present. Each identity
is one row bound to exactly the store its own fix command repairs: on
macOS that is the identity's single Keychain service first (the canonical
store there), the credentials file only when that item cannot answer at
all — a readable item's verdict stands even when it is `unavailable`, and
no other spelling's or identity's item may substitute. Fix commands pin
their credential selectors (session-scoped on every shell), so a pasted
command repairs the login its row names whatever the shell exports.
Token values are never read beyond their presence and never printed.

Other providers are probed for readable login end times too; a provider whose
store carries no such time is reported as `unsupported` rather than guessed.

Output: one JSON object on stdout, nothing else. Exit 0 even when some roots
are unreadable (they read as `unavailable`); argparse exits 2 on bad arguments.
"""

import argparse
import ctypes
import datetime
import hashlib
import json
import os
import pathlib
import re
import stat
import sys
import time
import unicodedata

SCHEMA_VERSION = 1
WARN_SECONDS = 24 * 3600
CRITICAL_SECONDS = 4 * 3600
# Enumerating every config dir is bounded; a machine with more roots than this
# reports the first MAX_ROOTS and stays honest about the rest by not guessing.
MAX_ROOTS = 64
# Raw entries one base scan will look at while collecting candidate homes;
# dot-prefixed entries are looked at but never counted as candidates.
MAX_SCAN_ENTRIES = 1024
# The macOS Keychain service of the selector-free default login.
KEYCHAIN_PLAIN_SERVICE = "Claude Code-credentials"
# Credential selectors a pasted re-login must pin, in one fixed order:
# the two storage selectors, then the OAuth-variant selectors that move
# a login to the custom/local storage identities upstream.
SELECTOR_ORDER = [
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "USE_LOCAL_OAUTH",
    "USE_STAGING_OAUTH",
    "CLAUDE_LOCAL_OAUTH_API_BASE",
    "CLAUDE_CODE_CUSTOM_OAUTH_URL",
]
# OAuth variant storage: in the shipped build the service suffix is
# build-selected, and the one suffix an environment can select is the
# custom-OAuth one, by a present (non-empty) custom URL. The local and
# staging suffixes belong to other build flavors whose items cannot be
# identified from this probe's environment, so they are not probed.
OAUTH_VARIANTS = (("custom-oauth", "CLAUDE_CODE_CUSTOM_OAUTH_URL"),)
# _mac_keychain_text returns this when an item exists and was read but
# its payload is over the byte cap: the store answered, unreadably —
# never silence the fallback file may speak over.
KEYCHAIN_ANSWERED_UNREADABLE = object()
# ...and this when the store answers but will not give the item (a
# locked Keychain, interaction not allowed, an auth failure): only
# "no such item" may fall back to the credentials file.
KEYCHAIN_LOCKED = object()


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
        if access_ms <= now_ms:
            return access_ms, "logged_out"
        return access_ms, "critical"
    if not has_access:
        if refresh_ms <= now_ms:
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


def _read_regular_bytes(path, limit=65536):
    """Bounded bytes from a regular file, or None. The lstat + S_ISREG check
    comes first (as in the sibling collector): a plain open of a FIFO blocks
    until a writer appears, which would stall the whole report until the
    transport timeout; O_NONBLOCK plus an fstat recheck covers a file swapped
    between the stat and the open."""
    try:
        info = os.lstat(path)
    except OSError:
        return None
    if not stat.S_ISREG(info.st_mode):
        return None
    try:
        raw_fd = os.open(
            path,
            os.O_RDONLY
            | getattr(os, "O_NOFOLLOW", 0)
            | getattr(os, "O_NONBLOCK", 0)
            | getattr(os, "O_BINARY", 0),
        )
    except OSError:
        return None
    try:
        # fdopen itself refuses a directory swapped in at the open; that
        # refusal (OSError, or ValueError from a mode the descriptor
        # cannot take) must not strand the raw descriptor, and closing
        # an already-closed descriptor must not escape either.
        descriptor = os.fdopen(raw_fd, "rb")
    except (OSError, ValueError):
        try:
            os.close(raw_fd)
        except OSError:
            pass
        return None
    try:
        with descriptor:
            actual = os.fstat(descriptor.fileno())
            if not stat.S_ISREG(actual.st_mode) or (actual.st_dev, actual.st_ino) != (
                info.st_dev,
                info.st_ino,
            ):
                return None
            return descriptor.read(limit)
    except OSError:
        return None


def read_credentials(path):
    """The two times and token presence from one credentials file, or None.

    The file is read but only its times and token presence are kept; token
    values never leave this function."""
    # The helper's lstat + O_NOFOLLOW refuse a linked credentials file
    # wherever it points, in-home links included: a link is never followed
    # and attributed to this profile.
    content = _read_regular_bytes(path)
    if content is None:
        return None
    try:
        raw = json.loads(content.decode("utf-8"))
    except (ValueError, RecursionError):
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
    """(posix, windows) forms of config_dir relative to home, or None.
    The home itself is "~", never "~/." — the two strings hash to
    different Keychain services upstream."""
    try:
        rel = pathlib.Path(config_dir).relative_to(home)
    except ValueError:
        return None
    if str(rel) == ".":
        return "~", "~"
    if not str(rel):
        return None
    return "~/" + rel.as_posix(), "~\\" + str(rel).replace("/", "\\")


def _posix_login_command(cleared, assignment):
    """A subshell-scoped re-login: the named selectors are unset for the
    invocation only, so pasting changes nothing in the user's own
    shell, and `claude` still resolves through the shell (functions
    and aliases included) rather than through `env`. A failed unset
    (a readonly selector) stops the command instead of proceeding
    unscoped against the wrong login."""
    return f"(unset {' '.join(cleared)} && {assignment}claude auth login)"


def _win32_login_command(overrides):
    """The PowerShell form of the same scoping: every selector is saved,
    set or cleared for the invocation, then restored in a finally
    block — which PowerShell runs even when the login is abandoned
    with Ctrl+C — so the pasted line leaves the session's own
    selectors exactly as they were. Restoring an unset selector from
    its saved $null removes it again, so presence is preserved as
    faithfully as values."""
    save = "; ".join(f"$aac{index} = $env:{name}" for index, name in enumerate(SELECTOR_ORDER))
    apply = "; ".join(
        f"$env:{name} = {overrides.get(name) or '$null'}" for name in SELECTOR_ORDER
    )
    restore = "; ".join(
        f"$env:{name} = $aac{index}" for index, name in enumerate(SELECTOR_ORDER)
    )
    return f"{save}; {apply}; try {{ claude auth login }} finally {{ {restore} }}"


def _default_fix_command():
    """The re-login command for the default ~/.claude login. It must not
    inherit credential selectors exported for another account or
    storage identity in the shell it is pasted into, so every selector
    is unset for the invocation — unset, never emptied: a set-but-empty
    CLAUDE_SECURESTORAGE_CONFIG_DIR still selects storage upstream
    (the plain service), and an empty CLAUDE_CONFIG_DIR is only a
    fallback by accident of one helper."""
    if sys.platform == "win32":
        return _win32_login_command({})
    return _posix_login_command(SELECTOR_ORDER, "")


def _raw_home_suffix(spelling, home):
    """The part of a raw selector spelling after the home prefix —
    "" for the home itself, the verbatim remainder (leading separator
    included) for a dir under it, None outside it. The remainder is
    kept byte-exact: a trailing slash or a doubled separator in an
    inherited selector is part of the identity upstream hashes."""
    home_text = str(home)
    if spelling == home_text:
        return ""
    for separator in ("/", "\\"):
        if spelling.startswith(home_text + separator):
            return spelling[len(home_text):]
    return None


def fix_command(config_dir, home, storage_var="CLAUDE_CONFIG_DIR", spelling=None):
    """An executable re-login command for this machine's shell. A dir under
    the home publishes as a $HOME-relative form the target expands at run
    time; a dir outside it (a relocated CCS scope) is the one deliberate
    exception and publishes as a quoted literal, as the README states.
    `storage_var` is the selector the command sets — the config dir, or
    the securestorage dir for an inherited storage identity — and every
    other selector is cleared for the invocation: inherited from the
    pasting shell, any of them would redirect the login to another
    account's or variant's store. `spelling`, when given, is the raw
    selector string this identity was inherited with: upstream hashes
    it verbatim, so the command must assign it verbatim too — the
    $HOME form carries its raw remainder, and a spelling that form
    cannot reproduce publishes as a quoted literal."""
    relative = _home_relative(config_dir, home)
    if sys.platform == "win32":
        # PowerShell reads curly quotes (U+2018-U+201F) as quotes too, and
        # no escaping of the ASCII set can keep one inside either quoted
        # form: a directory named with one gets no command rather than a
        # pasted line that runs something else. The home expands the way
        # Path.home() resolved it on this machine (see below).
        curly = "".join(chr(cp) for cp in range(0x2018, 0x2020))
        if spelling is not None:
            if any(character in spelling for character in curly):
                return ""
            raw_suffix = _raw_home_suffix(spelling, home)
            if raw_suffix is None:
                literal = spelling.replace("'", "''")
                return _win32_login_command({storage_var: f"'{literal}'"})
            escaped = raw_suffix.replace("`", "``").replace('"', '`"').replace("$", "`$")
            home_expr = "$(if (Test-Path env:USERPROFILE) { $env:USERPROFILE } else { $env:HOMEDRIVE + $env:HOMEPATH })"
            return _win32_login_command({storage_var: f'"{home_expr}{escaped}"'})
        if relative is None:
            literal = str(config_dir)
            if any(character in literal for character in curly):
                return ""
            literal = literal.replace("'", "''")
            return _win32_login_command({storage_var: f"'{literal}'"})
        if any(character in relative[1] for character in curly):
            return ""
        escaped = relative[1][1:].replace("`", "``").replace('"', '`"').replace("$", "`$")
        # Python's Windows home is USERPROFILE when the variable exists
        # (even empty), else HOMEDRIVE+HOMEPATH; the command resolves
        # the same presence test at paste time.
        home_expr = "$(if (Test-Path env:USERPROFILE) { $env:USERPROFILE } else { $env:HOMEDRIVE + $env:HOMEPATH })"
        return _win32_login_command({storage_var: f'"{home_expr}{escaped}"'})
    cleared = [name for name in SELECTOR_ORDER if name != storage_var]
    if spelling is not None:
        suffix = _raw_home_suffix(spelling, home)
        if suffix is None:
            literal = spelling.replace("'", "'\\''")
            return _posix_login_command(cleared, f"{storage_var}='{literal}' ")
    elif relative is None:
        literal = str(config_dir).replace("'", "'\\''")
        return _posix_login_command(cleared, f"{storage_var}='{literal}' ")
    else:
        suffix = relative[0][1:]
    if "!" in suffix:
        # Interactive bash history-expands `!` even inside double quotes, and
        # `\!` keeps its backslash there, so a `!`-carrying suffix is pasted
        # as a single-quoted segment instead, where `!` is inert.
        quoted = suffix.replace("'", "'\\''")
        return _posix_login_command(cleared, f"{storage_var}=\"$HOME\"'{quoted}' ")
    escaped = (
        suffix.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("$", "\\$")
        .replace("`", "\\`")
    )
    return _posix_login_command(cleared, f'{storage_var}="$HOME{escaped}" ')


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


def _launcher_spelling(value, home, ccs_dir=None):
    """The spelled config dir the launcher would set for a configured
    home: T3 trims the setting and resolves it (normalizing `.`/`..`
    segments) before assigning CLAUDE_CONFIG_DIR, and upstream hashes
    exactly that string for the macOS Keychain service. The collector
    must hash the same string, so a configured root is normalized the
    same way (without following symlinks — the spelled string is the
    identity). A value that normalizes outside the allowed roots is
    refused outright: reading it under its unnormalized spelling would
    let a `..` segment smuggle in a store the containment rules bar."""
    text = os.path.normpath(os.path.expanduser(str(value).strip()))
    for root in (home, ccs_dir):
        if root is None:
            continue
        spelled_root = os.path.normpath(str(root))
        if text == spelled_root or text.startswith(
            spelled_root if spelled_root.endswith(os.sep) else spelled_root + os.sep
        ):
            return pathlib.Path(text)
    raise ValueError("configured home resolves outside the allowed roots")


def _has_broken_ancestor(path):
    """True when the nearest existing ancestor of a missing path is itself a
    broken lookup: a dangling symlink (lstat sees it, stat cannot follow it)
    or a component whose lstat fails outright. The final component is not
    the whole story — `~/dead-link/profile` and a dangling `~/.ccs` both
    fail stat and lstat at the leaf, yet a configured home is hiding behind
    the broken link. A plain file as the nearest ancestor is a confirmed
    absence: no profiles can hide under a file."""
    for ancestor in (path, *path.parents):
        try:
            info = ancestor.lstat()
        except (FileNotFoundError, NotADirectoryError):
            continue
        except OSError:
            return True
        if stat.S_ISLNK(info.st_mode):
            try:
                ancestor.stat()
            except OSError:
                return True
        return False
    return False


def _dir_state(path):
    """(is_directory, read_failed) for one path. Path.is_dir() cannot be
    used for this: it swallows permission and I/O errors into False, which
    would make an unreadable profile indistinguishable from an absent
    one. A missing path is not a failure; an unreadable one — including a
    dangling symlink whose target is gone, at the path or above it — is."""
    try:
        mode = path.stat().st_mode
    except (FileNotFoundError, NotADirectoryError):
        return False, _has_broken_ancestor(path)
    except OSError:
        return False, True
    return stat.S_ISDIR(mode), False


def claude_profile(name, config_dir, now_ms, home, ccs_dir=None, kind="dir", spelling=None):
    """One login identity's sign-in state from its credentials file;
    missing/unreadable files read as unavailable. `kind` is the identity
    the row (and its fix command) is bound to: "default" (the
    selector-free login — naming ~/.claude explicitly would move the
    macOS login to a different, hashed, Keychain item), "dir" (a
    CLAUDE_CONFIG_DIR identity), or "storage-dir" (an inherited
    CLAUDE_SECURESTORAGE_CONFIG_DIR identity, whose file also lives in
    that dir)."""
    relative = _home_relative(config_dir, home)
    if kind == "default":
        fix = _default_fix_command()
    elif kind == "storage-dir":
        fix = fix_command(config_dir, home, storage_var="CLAUDE_SECURESTORAGE_CONFIG_DIR",
                          spelling=spelling)
    else:
        fix = fix_command(config_dir, home, spelling=spelling)
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


def _keychain_service_for(config_dir_text, oauth_suffix=""):
    """The per-config-dir Keychain service name, matching the naming
    helper in Claude Code's bundled source: "Claude Code", the OAuth
    variant suffix ("" for the standard login), "-credentials-", and
    the first eight hex digits of the SHA-256 of the config dir string.
    Both dir resolutions upstream (the config dir and the securestorage
    dir) NFC-normalize before this hash, so every leg normalizes here."""
    digest = hashlib.sha256(
        unicodedata.normalize("NFC", config_dir_text).encode("utf-8")
    ).hexdigest()[:8]
    return f"Claude Code{oauth_suffix}-credentials-{digest}"


def _keychain_service_for_root(root, kind, oauth_suffix="", spelling=None):
    """The one Keychain service an identity's launch reads and its fix
    command rewrites — exactly one per identity, never a candidate
    list: the default identity's command unsets every selector, so it
    is always the plain service (with the variant suffix for a variant
    identity); every other identity hashes its own spelled dir, the
    string its command sets. No other spelling's or identity's item
    may substitute for it. A dir string UTF-8 cannot encode (a
    non-UTF-8 filename) yields no service rather than aborting."""
    if kind == "default":
        return f"Claude Code{oauth_suffix}-credentials"
    try:
        return _keychain_service_for(spelling if spelling is not None else str(root),
                                     oauth_suffix)
    except (UnicodeError, ValueError):
        return None


def _ambient_storage_roots(home):
    """Storage identities this probe process itself inherited, as
    (dir, kind, raw spelling) triples: a non-empty CLAUDE_SECURESTORAGE_CONFIG_DIR is
    a "storage-dir" identity; otherwise a non-empty CLAUDE_CONFIG_DIR
    is a "dir" identity. Upstream's own precedence gives the
    securestorage value the storage choice outright. These publish as
    their own rows — an inherited selector must never rebind the
    default row, whose repair command recreates the plain login."""
    if "CLAUDE_SECURESTORAGE_CONFIG_DIR" in os.environ:
        raw = os.environ["CLAUDE_SECURESTORAGE_CONFIG_DIR"]
        # Set-but-empty selects the plain store outright upstream,
        # whatever the config selector says: no ambient identity.
        return [(pathlib.Path(raw), "storage-dir", raw)] if raw else []
    configured = os.environ.get("CLAUDE_CONFIG_DIR")
    if configured:
        return [(pathlib.Path(configured), "dir", configured)]
    return []


def _mac_keychain_account():
    """The Keychain account upstream keys its items to: $USER, else the
    account-database username, validated against [A-Za-z0-9._-]+ with
    a fixed fallback name. The lookup must be scoped to it — an
    account-less search returns the first item with the service name,
    which can be another account's login entirely."""
    value = os.environ.get("USER")
    if not value:
        try:
            import pwd

            value = pwd.getpwuid(os.getuid()).pw_name
        except Exception:
            value = None
    if value and re.fullmatch(r"[A-Za-z0-9._-]+", value):
        return value
    return "claude-code-user"


def _mac_keychain_text(service=KEYCHAIN_PLAIN_SERVICE):
    """The Keychain item's JSON through Security's noninteractive C API,
    scoped to the account upstream writes under.

    The `security` CLI can prompt for keychain access; over ssh that prompt
    hangs until the timeout and the primary store never reports. User
    interaction is explicitly disabled for this lookup, so it either reads
    or fails immediately. Returns the payload text ("" for a present but
    empty item), KEYCHAIN_ANSWERED_UNREADABLE for a present item over the
    byte cap, KEYCHAIN_LOCKED when the store answers but will not give
    the item (locked, interaction not allowed, auth failure), or None
    only for "no such item" — the one silence that leaves the file
    fallback free to speak."""
    if sys.platform != "darwin":
        return None
    try:
        security = ctypes.CDLL("/System/Library/Frameworks/Security.framework/Security")
        security.SecKeychainSetUserInteractionAllowed.argtypes = [ctypes.c_bool]
        security.SecKeychainSetUserInteractionAllowed.restype = ctypes.c_int32
        if security.SecKeychainSetUserInteractionAllowed(False) != 0:
            return KEYCHAIN_LOCKED
        find = security.SecKeychainFindGenericPassword
        find.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_char_p, ctypes.c_uint32,
                         ctypes.c_char_p, ctypes.POINTER(ctypes.c_uint32),
                         ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p]
        find.restype = ctypes.c_int32
        security.SecKeychainItemFreeContent.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        service_bytes = service.encode("utf-8")
        account_bytes = _mac_keychain_account().encode("utf-8")
        length, pointer = ctypes.c_uint32(), ctypes.c_void_p()
        status_code = find(None, len(service_bytes), service_bytes,
                           len(account_bytes), account_bytes,
                           ctypes.byref(length), ctypes.byref(pointer), None)
        if status_code == -25300:
            # errSecItemNotFound: the one silence the file may speak over.
            return None
        if status_code != 0:
            # Locked, interaction not allowed, auth failed: the store
            # answered but will not give the item.
            return KEYCHAIN_LOCKED
        try:
            if length.value > 65536:
                return KEYCHAIN_ANSWERED_UNREADABLE
            if length.value == 0 or not pointer:
                return ""
            # surrogateescape: a payload that is not UTF-8 text still
            # counts as the item answering (its JSON parse then fails),
            # never as silence the fallback file may speak over.
            return ctypes.string_at(pointer, length.value).decode(
                "utf-8", "surrogateescape")
        finally:
            security.SecKeychainItemFreeContent(None, pointer)
    except (OSError, ValueError, UnicodeError, AttributeError):
        return None


def mac_keychain_profile_for_service(now_ms, service):
    """One Keychain service's verdict as a profile. `answered` separates
    storage-read success from the health verdict: True once the item was
    actually read — including an empty, oversized or unparseable payload
    whose verdict is `unavailable`. Only an unanswered probe (no item, a
    locked Keychain) may fall back to the credentials file. The marker
    is internal: publication copies named fields only."""
    entry = {
        "accessExpiresAt": None,
        "refreshExpiresAt": None,
        "loggedOutAt": None,
        "state": "unavailable",
        "error": "keychain item unreadable",
    }
    if sys.platform != "darwin":
        entry.pop("error")
        return entry
    raw = _mac_keychain_text(service)
    if raw is None:
        return entry
    entry["answered"] = True
    if raw is KEYCHAIN_LOCKED:
        entry["error"] = "keychain locked or access not allowed"
        return entry
    if raw is KEYCHAIN_ANSWERED_UNREADABLE:
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


def mac_keychain_profile(now_ms, home):
    """The default login's Keychain verdict as a standalone profile:
    the plain service's item, dressed with the row fields. Collection
    binds services per identity through mac_keychain_profile_for_service;
    this remains for direct probes of the default store."""
    service = _keychain_service_for_root(home / ".claude", "default")
    entry = {
        "name": "keychain",
        "configDir": "Keychain: '%s'" % service,
        "provider": "claude",
        "fixCommand": _default_fix_command(),
        "answered": False,
    }
    entry.update(mac_keychain_profile_for_service(now_ms, service))
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
        # Presence is a stat verdict, never lexists: lexists also returns
        # False when the parent directory denies access, which would read
        # an existing-but-unseen auth file as simply absent. Only a clean
        # FileNotFoundError under a healthy ancestor chain is absence;
        # anything else is unseen and reports unavailable, not silence.
        try:
            info = candidate.lstat()
            present = True
        except (FileNotFoundError, NotADirectoryError):
            info = None
            present = _has_broken_ancestor(candidate)
        except OSError:
            info = None
            present = True
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
        # A non-file at the auth path (a directory, a FIFO) is present but
        # unreadable; opening a FIFO would block, so it never gets opened.
        # A link is left to the containment check above and the no-follow
        # open inside the helper.
        if info is not None and not (
            stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode)
        ):
            return unreadable
        content = _read_regular_bytes(candidate)
        if content is None:
            return unreadable if present else None
        try:
            raw = json.loads(content.decode("utf-8"))
        except (ValueError, RecursionError):
            return unreadable
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


def _dedupe_key(path, kind="dir"):
    """Identity of a login for de-duplication: the filesystem object
    itself when stat can see it, so a symlinked instance — and, on a
    case-insensitive volume, a case-variant spelling of the same dir —
    publishes once; the realpath when it cannot. A zero inode (some
    Windows volumes report one) is no identity and falls back too.

    macOS is the exception: there the login's identity is its Keychain
    service, so the key is the NFC-normalized spelled string the
    service hashes, grouped by service shape — the selector-free
    default (plain service) apart from every hashed identity. Two
    spellings of one directory can hold different logins and keep
    separate rows; spellings that hash alike (NFC/NFD forms, or a
    storage-dir and a dir identity with the same string) are one
    login and publish once."""
    if sys.platform == "darwin":
        spelled = str(path)
        try:
            spelled = unicodedata.normalize("NFC", spelled)
        except (ValueError, UnicodeError):
            pass
        return ("spelled", "default" if kind == "default" else "hashed", spelled)
    try:
        info = path.stat()
    except OSError:
        return ("path", os.path.realpath(path))
    if not info.st_ino:
        return ("path", os.path.realpath(path))
    return ("inode", info.st_dev, info.st_ino)


def claude_roots(home, ccs_dir=None, include_ambient=False):
    """Every Claude login identity this machine knows, as
    (label, dir, kind) triples: the default, the saved
    ~/.ccs/instances homes, the T3 account dirs, settings-defined
    homes, a configured home naming the default dir as its own explicit
    identity, and — on real-machine runs — any storage identity this
    probe process inherited.

    The saved-instance and T3 dirs are enumerated here even when the
    sibling discovery is available, because its name filter refuses valid
    directory names (dotted and email-shaped names, apostrophes); display
    names are sanitized later, never at discovery. Returns (roots, complete): complete is False when
    the sibling discovery was unavailable or enumeration hit the bound, so
    the report can say the list may be short instead of implying all."""
    default = home / ".claude"
    roots = [("default", default, "default", str(default))]
    seen = {_dedupe_key(default, "default"): 0}
    complete = True

    def add_root(label, root, kind="dir", spelling=None):
        # One identity under two spellings publishes once, under its real
        # name: when the first-seen spelling is a link and a later one is
        # the directory itself, the later spelling takes the slot. The
        # default root (slot 0) is never replaced. is_symlink here only
        # chooses the label; every presence verdict still comes from
        # _dir_state before this point. On macOS the dedupe key carries
        # the spelling, so distinct spellings never reach this merge.
        # `spelling` is the raw selector string for an inherited
        # identity (hashed and repaired verbatim); every other identity
        # spells itself by its normalized dir.
        key = _dedupe_key(root, kind)
        existing = seen.get(key)
        if existing is None:
            seen[key] = len(roots)
            roots.append((label, root, kind, spelling if spelling is not None else str(root)))
        elif existing > 0 and roots[existing][1].is_symlink() and not root.is_symlink():
            roots[existing] = (label, root, kind, spelling if spelling is not None else str(root))
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
        # Every sibling truncation (the roots list, settings instances,
        # each base's children) records itself through the read-failure
        # counters watched above, so a long list alone is not evidence of
        # one: exactly MAX_SOURCES readable homes stay complete.
        allowed_ccs = ccs_dir or home / ".ccs"
        for root, selectors in pairs:
            if selectors and "discover" in selectors:
                # The sibling's own default root: the same identity as
                # slot 0, never a second (explicit) default row. Its
                # presence is still accounted like any other root's.
                _is_dir, stat_failed = _dir_state(root)
                if stat_failed:
                    complete = False
                continue
            try:
                # Normalization comes FIRST: configured homes arrive
                # spelled as written, and the launcher trims and
                # resolves before assigning the selector upstream
                # hashes. Statting the raw spelling drops a home whose
                # `..` crosses a folder that does not exist (and a
                # padded one) before it can ever be normalized; a
                # spelling that escapes the allowed roots once
                # normalized is refused, never read as written.
                root = _launcher_spelling(root, home, allowed_ccs)
            except ValueError:
                complete = False
                continue
            is_dir, stat_failed = _dir_state(root)
            if stat_failed:
                complete = False
                continue
            if not is_dir:
                continue
            label = root.name or os.path.realpath(root)
            for selector in selectors or []:
                if isinstance(selector, str) and selector.startswith("claude-profile:"):
                    label = selector.split(":", 1)[1]
                    break
            if str(root) == str(default):
                # A configured home naming the default dir is launched
                # with the selector set explicitly — on macOS that is a
                # different (hashed) login from the plain default, and
                # its repair must set the selector too. Its own row.
                label = "default-configured"
            add_root(label, root)
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
            # Only candidate homes count toward the bound: dot-prefixed
            # entries and plain files are never homes, and seventy of
            # either beside one real account must not read as truncated
            # discovery. The raw scan stays bounded too; hitting either
            # bound is reported, never silent.
            children = []
            scanned = 0
            for child in base.iterdir():
                scanned += 1
                if scanned > MAX_SCAN_ENTRIES:
                    complete = False
                    break
                if child.name.startswith("."):
                    continue
                child_is_dir, child_failed = _dir_state(child)
                if child_failed:
                    complete = False
                    continue
                if not child_is_dir:
                    continue
                children.append(child)
                if len(children) > MAX_ROOTS:
                    complete = False
                    children = children[:MAX_ROOTS]
                    break
        except OSError:
            complete = False
            return
        for child in sorted(children, key=lambda item: item.name):
            add_root(child.name, child)

    scan((ccs_dir or home / ".ccs") / "instances")
    scan(home / ".claude-t3")
    if include_ambient:
        # A storage identity inherited by this probe process: its own
        # row, gated to the allowed roots like every other identity.
        # The spelled value is kept exactly as given — upstream hashes
        # the env string itself for this identity, unnormalized.
        for ambient, kind, raw in _ambient_storage_roots(home):
            is_dir, stat_failed = _dir_state(ambient)
            if stat_failed:
                complete = False
                continue
            if not is_dir and sys.platform != "darwin":
                # No dir, no file, no other store: nothing to report.
                # (On macOS a Keychain item can exist for it regardless.)
                continue
            if not _within_allowed(ambient, home, ccs_dir or home / ".ccs"):
                complete = False
                continue
            add_root(ambient.name or "ambient", ambient, kind, spelling=raw)
    if len(roots) > MAX_ROOTS:
        complete = False
        roots = roots[:MAX_ROOTS]
    return roots, complete


def collect(home, now_ms, include_keychain=True, ccs_dir=None):
    """One machine's report. include_keychain=False keeps a --home fixture
    run from touching the real login Keychain at all (and from treating
    this process's inherited selectors as machine identities); ccs_dir
    selects the saved-instance scope on installs that relocate it."""
    roots, complete = claude_roots(home, ccs_dir, include_ambient=include_keychain)
    allowed_ccs = ccs_dir or home / ".ccs"
    profiles = [
        claude_profile(name, config_dir, now_ms, home, allowed_ccs, kind=kind,
                       spelling=spelling)
        for name, config_dir, kind, spelling in roots
    ]
    variant_dropped = False
    if include_keychain and sys.platform == "darwin":

        def merge_keychain(row, candidate):
            # The row keeps its own name, configDir and fix command:
            # the Keychain service's hash fingerprints the absolute
            # path and names no dir the reader can act on.
            row.update(
                accessExpiresAt=candidate["accessExpiresAt"],
                refreshExpiresAt=candidate["refreshExpiresAt"],
                loggedOutAt=candidate["loggedOutAt"],
                state=candidate["state"],
            )
            if candidate.get("error"):
                row["error"] = candidate["error"]
            else:
                row.pop("error", None)

        # The Keychain and .credentials.json are two stores for the same
        # login (Claude Code's backend is keychain-with-plaintext-fallback):
        # the Keychain is the canonical store, read first, and the file is
        # only its fallback for a store that cannot answer at all. Each
        # identity probes exactly its own service — the item its launch
        # reads and its fix command rewrites — and an ANSWERED item binds
        # the row, including an answered `unavailable`: a stale fallback
        # file (or another spelling's item) must never speak over the
        # canonical store's own verdict. The stores never publish as two
        # logins, one failed beside one healthy.
        # profiles[i] matches roots[i] here; later rows are appended below.
        for index, (_label, root, kind, spelling) in enumerate(roots):
            row = profiles[index]
            if row.get("error") == "profile directory resolves outside the allowed roots":
                # A refused dir's login is never probed either: the
                # containment refusal covers both of its stores.
                continue
            service = _keychain_service_for_root(root, kind, spelling=spelling)
            if service is None:
                continue
            candidate = mac_keychain_profile_for_service(now_ms, service)
            if candidate.get("answered"):
                merge_keychain(row, candidate)
            elif kind == "default" and row["state"] == "unavailable" and candidate.get("error"):
                # Neither default store could answer; the canonical
                # store's cause names the failure. Per-account rows keep
                # the file cause: a hashed item may simply not exist for
                # a file-kept account, which says nothing unreadable.
                row["error"] = candidate["error"]
        # The custom-OAuth identity stores under a suffixed service for
        # the same dirs. When this probe's environment carries a
        # present custom URL, an answered variant item publishes as its
        # own row; its repair is re-running the user's existing
        # overlay session, which no generated command can recreate, so
        # the row carries none.
        for variant, env_name in OAUTH_VARIANTS:
            if not os.environ.get(env_name):
                continue
            for index, (label, root, kind, spelling) in enumerate(roots):
                if profiles[index].get("error") == \
                        "profile directory resolves outside the allowed roots":
                    # The containment refusal covers every store of a
                    # refused dir, variant services included.
                    continue
                service = _keychain_service_for_root(root, kind, "-" + variant,
                                                     spelling=spelling)
                if service is None:
                    continue
                candidate = mac_keychain_profile_for_service(now_ms, service)
                if not candidate.get("answered"):
                    continue
                if len(profiles) >= 2 * MAX_ROOTS:
                    variant_dropped = True
                    break
                row = dict(profiles[index])
                row["name"] = f"{label}-{variant}"
                row["fixCommand"] = ""
                merge_keychain(row, candidate)
                profiles.append(row)
        # An inherited identity whose dir does not exist and whose item
        # did not answer has no store evidence at all: like an absent
        # configured root, it publishes nothing instead of a permanent
        # ghost unavailable row.
        ambient_spellings = {raw for _path, _kind, raw in _ambient_storage_roots(home)}
        if ambient_spellings:
            kept = []
            for index, row in enumerate(profiles):
                if index < len(roots):
                    _label, root, _kind, spelling = roots[index]
                    is_dir, _failed = _dir_state(root)
                    if (spelling in ambient_spellings and not is_dir
                            and row["state"] == "unavailable"
                            and row.get("error") == "credentials file missing or unreadable"):
                        continue
                kept.append(row)
            profiles = kept
    if not complete or variant_dropped:
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
    codex = codex_profile(now_ms, home)
    if codex:
        profiles.append(codex)
    # Distinct homes can sanitize to the same published name; a later
    # row must not erase an earlier login under that name at any
    # downstream keying, so collisions publish with a numeric suffix.
    # Every raw name is reserved up front, so a generated suffix never
    # lands on another row's own name (same, same, same-2 must publish
    # three distinct names, not same/same-2/same-2).
    reserved = {row["name"] for row in profiles}
    used = set()
    for row in profiles:
        base = row["name"]
        if base not in used:
            used.add(base)
            continue
        suffix = 2
        while f"{base}-{suffix}" in used or f"{base}-{suffix}" in reserved:
            suffix += 1
        row["name"] = f"{base}-{suffix}"
        used.add(row["name"])
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
