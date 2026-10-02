#!/usr/bin/env python3
"""AAC-owned API key store helper (CONTRACT-registry-lifecycle section 3.2).

    key_store.py put    --provider <id> --key-id <8 hex>   (the secret on stdin)
    key_store.py delete --provider <id> --key-id <8 hex>

The secret is read from stdin only, never from argv or the environment. Every
path is derived from fixed folders: `~/.ccs/account-usage/keys/` (0700) holds
`<provider>-<keyId>.json` (0600) on Ubuntu and Mac, and `<provider>-<keyId>.dpapi`
on Windows (CryptProtectData, CurrentUser, entropy `AAC/account-key/v1`, over the
UTF-8 bytes of the same JSON record). This is the one format
`plan_common.aac_key_credential` reads.

Output is a single JSON line: `{"ok":true,"fingerprint":"sha256:...","last4":"..."}`
for put and `{"ok":true}` for delete. Failures print nothing and exit 1; usage
errors exit 2. The secret, paths and error text are never printed.
"""

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import stat
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from plan_common import (  # noqa: E402
    KEY_ENTROPY,
    KEY_ID,
    KEY_PROVIDERS,
    KEY_SECRET,
    MAX_KEY_FILE_BYTES,
    key_fingerprint,
)

MAX_STDIN_BYTES = 4096


class StoreError(Exception):
    pass


def key_directory(home=None):
    home = Path.home() if home is None else Path(home)
    return home / ".ccs" / "account-usage" / "keys"


def _windows_protect(plaintext, entropy):
    """DPAPI CurrentUser over `plaintext`; raw bytes, never base64."""
    import ctypes
    from ctypes import wintypes

    class Blob(ctypes.Structure):
        _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]

    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    crypt32.CryptProtectData.argtypes = [ctypes.POINTER(Blob), ctypes.c_wchar_p, ctypes.POINTER(Blob),
                                         ctypes.c_void_p, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(Blob)]
    crypt32.CryptProtectData.restype = wintypes.BOOL
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    data = ctypes.create_string_buffer(plaintext)
    data_blob = Blob(len(plaintext), ctypes.cast(data, ctypes.POINTER(ctypes.c_ubyte)))
    entropy_buffer = ctypes.create_string_buffer(entropy)
    entropy_blob = Blob(len(entropy), ctypes.cast(entropy_buffer, ctypes.POINTER(ctypes.c_ubyte)))
    output = Blob()
    # CRYPTPROTECT_UI_FORBIDDEN (1): never prompt.
    if not crypt32.CryptProtectData(ctypes.byref(data_blob), None, ctypes.byref(entropy_blob),
                                    None, None, 1, ctypes.byref(output)):
        raise StoreError()
    try:
        return ctypes.string_at(output.pbData, output.cbData)
    finally:
        kernel32.LocalFree(ctypes.cast(output.pbData, ctypes.c_void_p))


def _ensure_directory(directory, windows):
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = os.lstat(str(directory))
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        raise StoreError()
    if not windows:
        if hasattr(os, "getuid") and info.st_uid != os.getuid():
            raise StoreError()
        if info.st_mode & 0o077:
            os.chmod(str(directory), 0o700)


def _write_atomic(directory, name, payload):
    temporary = directory / ".{}.{}.tmp".format(name, os.urandom(4).hex())
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    descriptor = os.open(str(temporary), flags, 0o600)
    try:
        try:
            os.write(descriptor, payload)
            if hasattr(os, "fchmod"):
                os.fchmod(descriptor, 0o600)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        os.replace(str(temporary), str(directory / name))
    finally:
        try:
            os.unlink(str(temporary))
        except OSError:
            pass


def read_secret(stream):
    raw = stream.read(MAX_STDIN_BYTES + 1)
    if len(raw) > MAX_STDIN_BYTES:
        raise StoreError()
    try:
        text = raw.decode("ascii")
    except UnicodeError:
        raise StoreError()
    if text.endswith("\r\n"):
        text = text[:-2]
    elif text.endswith("\n"):
        text = text[:-1]
    if not KEY_SECRET.match(text):
        raise StoreError()
    return text


def put(provider, key_id, secret, home=None, windows=None, now=None):
    windows = os.name == "nt" if windows is None else windows
    directory = key_directory(home)
    _ensure_directory(directory, windows)
    fingerprint = key_fingerprint(secret)
    created = (now or dt.datetime.now(dt.timezone.utc)).strftime("%Y-%m-%dT%H:%M:%SZ")
    record = {"version": 1, "provider": provider, "keyId": key_id, "secret": secret,
              "fingerprint": fingerprint, "last4": secret[-4:], "createdAt": created}
    text = json.dumps(record, separators=(",", ":"))
    if len(text.encode("utf-8")) > MAX_KEY_FILE_BYTES:
        raise StoreError()
    if windows:
        _write_atomic(directory, "{}-{}.dpapi".format(provider, key_id),
                      _windows_protect(text.encode("utf-8"), KEY_ENTROPY))
    else:
        _write_atomic(directory, "{}-{}.json".format(provider, key_id), (text + "\n").encode("utf-8"))
    return {"ok": True, "fingerprint": fingerprint, "last4": secret[-4:]}


def delete(provider, key_id, home=None, windows=None):
    windows = os.name == "nt" if windows is None else windows
    path = key_directory(home) / "{}-{}.{}".format(provider, key_id, "dpapi" if windows else "json")
    try:
        info = os.lstat(str(path))
    except FileNotFoundError:
        return {"ok": True}
    if stat.S_ISDIR(info.st_mode):
        raise StoreError()
    os.unlink(str(path))
    return {"ok": True}


def main(argv=None, stdin=None, stdout=None):
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("action", choices=("put", "delete"))
    parser.add_argument("--provider", required=True, choices=KEY_PROVIDERS)
    parser.add_argument("--key-id", required=True)
    args = parser.parse_args(argv)
    if not KEY_ID.match(args.key_id):
        parser.exit(2)
    stdout = stdout or sys.stdout
    try:
        if args.action == "put":
            secret = read_secret(stdin if stdin is not None else sys.stdin.buffer)
            result = put(args.provider, args.key_id, secret)
        else:
            result = delete(args.provider, args.key_id)
    except (StoreError, OSError, ValueError):
        return 1
    stdout.write(json.dumps(result, separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
