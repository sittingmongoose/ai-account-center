"""Private Antigravity credential adapters; no command-line secret transport.

Only the Ubuntu fallback file is verified for production integration. macOS and
Windows stores are reference adapters until an interactive native read/write
round trip is explicitly reviewed. No function starts or stops an application.
"""
from __future__ import annotations
import base64
import ctypes
import datetime as dt
import hashlib
import json
import math
import os
import pathlib
import re
import secrets
import stat
from dataclasses import dataclass, field
from typing import Callable, Protocol

MAX_BYTES = 16384
SERVICE = 'gemini'
ACCOUNT = 'antigravity'
WINDOWS_TARGET = SERVICE + ':' + ACCOUNT
FALLBACK_RELATIVE = pathlib.Path('.gemini/antigravity-cli/antigravity-oauth-token')

class AuthError(Exception):
    """Fixed safe errors, without provider responses or credential values."""

class IdentityUnavailable(AuthError): pass
class IdentityMismatch(AuthError): pass
class NeedsSignIn(AuthError): pass

@dataclass(frozen=True)
class PrivateCredential:
    raw: bytes = field(repr=False)
    revision: str = field(repr=False)

@dataclass(frozen=True)
class VerifiedIdentity:
    email: str
    subject: str = field(repr=False)
    verified_at: str
    authority: str = 'Google OAuth2 userinfo'
    source: str = 'provider-userinfo'

    @property
    def identity_key(self) -> str:
        raw = json.dumps(['antigravity', self.email.lower(), self.subject], separators=(',', ':'), ensure_ascii=False).encode()
        return hashlib.sha256(raw).hexdigest()

@dataclass(frozen=True)
class FileSnapshot:
    credential: PrivateCredential = field(repr=False)
    device: int
    inode: int
    modified_ns: int

@dataclass(frozen=True)
class InstallReceipt:
    before: FileSnapshot = field(repr=False)
    installed: FileSnapshot = field(repr=False)


def revision(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def parse_credential(raw: bytes) -> dict:
    if not isinstance(raw, bytes) or not 0 < len(raw) <= MAX_BYTES:
        raise AuthError('Invalid Antigravity credential size.')
    try:
        value = json.loads(raw.decode('utf-8'))
    except (ValueError, UnicodeError):
        raise AuthError('Invalid Antigravity credential format.') from None
    if not isinstance(value, dict) or value.get('auth_method') != 'consumer':
        raise AuthError('The saved login is not an Antigravity consumer credential.')
    if set(value) - {'auth_method', 'token', 'id_token'}:
        raise AuthError('Unsupported Antigravity credential fields.')
    token = value.get('token')
    if not isinstance(token, dict) or set(token) - {'access_token', 'refresh_token', 'token_type', 'expiry'}:
        raise AuthError('Unsupported Antigravity token fields.')
    for key in ('access_token', 'refresh_token'):
        text = token.get(key)
        if not isinstance(text, str) or not 0 < len(text) <= MAX_BYTES or re.search(r'[\x00-\x20\x7f]', text):
            raise AuthError('The saved Antigravity login is incomplete.')
    if token.get('token_type') != 'Bearer':
        raise AuthError('Unsupported Antigravity token type.')
    try:
        expiry = dt.datetime.fromisoformat(token['expiry'].replace('Z', '+00:00'))
        if expiry.tzinfo is None or not 2000 <= expiry.year <= 2100:
            raise ValueError()
    except (KeyError, TypeError, ValueError, AttributeError):
        raise AuthError('Invalid Antigravity token expiry.') from None
    if 'id_token' in value and (not isinstance(value['id_token'], str) or len(value['id_token']) > MAX_BYTES):
        raise AuthError('Invalid Antigravity identity token.')
    return value


def credential(raw: bytes) -> PrivateCredential:
    parse_credential(raw)
    return PrivateCredential(raw, revision(raw))


def verify_identity(value: PrivateCredential, userinfo: Callable[[str], dict], now: dt.datetime | None = None) -> VerifiedIdentity:
    token = parse_credential(value.raw)['token']['access_token']
    try:
        data = userinfo(token)
    except Exception as error:
        if getattr(error, 'status', None) == 'needs_sign_in' or getattr(error, 'code', None) in (401, 403):
            raise NeedsSignIn('The saved Antigravity account needs a renewed sign-in.') from None
        raise IdentityUnavailable('The saved Antigravity identity could not be verified.') from None
    email, subject = data.get('email'), data.get('id') or data.get('sub')
    if (data.get('verified_email', data.get('email_verified')) is not True
            or not isinstance(email, str) or len(email) > 254
            or not re.fullmatch(r'[^\s@]+@[^\s@]+\.[^\s@]+', email)
            or not isinstance(subject, str) or not re.fullmatch(r'[0-9]{1,128}', subject)):
        raise IdentityMismatch('Google did not return a verified Antigravity account identity.')
    stamp = (now or dt.datetime.now(dt.timezone.utc)).astimezone(dt.timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z')
    return VerifiedIdentity(email.lower(), subject, stamp)


def refresh_in_memory(value: PrivateCredential, refresh: Callable[[str], dict], now: dt.datetime) -> PrivateCredential:
    data = parse_credential(value.raw)
    try:
        response = refresh(data['token']['refresh_token'])
    except Exception as error:
        if getattr(error, 'status', None) == 'needs_sign_in' or getattr(error, 'code', None) in (401, 403):
            raise NeedsSignIn('The saved Antigravity account needs a renewed sign-in.') from None
        raise IdentityUnavailable('The saved Antigravity session could not be renewed temporarily.') from None
    try:
        access = response['access_token']
        seconds = response['expires_in']
        if not isinstance(access, str) or type(seconds) not in (int, float) or not math.isfinite(seconds) or not 0 < seconds <= 86400:
            raise ValueError()
        if now.tzinfo is None:
            raise ValueError()
        data['token']['access_token'] = access
        data['token']['expiry'] = (now + dt.timedelta(seconds=seconds)).astimezone(dt.timezone.utc).isoformat(timespec='microseconds').replace('+00:00', 'Z')
        if 'refresh_token' in response:
            data['token']['refresh_token'] = response['refresh_token']
        if 'id_token' in response:
            data['id_token'] = response['id_token']
        return credential(json.dumps(data, separators=(',', ':')).encode())
    except Exception:
        raise NeedsSignIn('The saved Antigravity session returned an unusable renewal.') from None



class SecureFileStore:
    """Ubuntu's verified native fallback. Exact file only; history is untouched.

    A Linux rename-exchange preserves the displaced inode. A foreign writer
    detected across publication is restored without unlinking its contents.
    Unexpected races preserve both files and report a conflict for review.
    """
    def __init__(self, home: pathlib.Path):
        self.home = pathlib.Path(home).absolute()
        self.path = self.home / FALLBACK_RELATIVE

    def _parent(self) -> int:
        if os.name != 'posix' or not hasattr(os, 'getuid'):
            raise AuthError('This verified file adapter requires POSIX ownership checks.')
        current = self.home
        for part in ('', '.gemini', 'antigravity-cli'):
            if part:
                current = current / part
            info = current.lstat()
            if (not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode)
                    or info.st_uid != os.getuid() or info.st_mode & stat.S_IWOTH):
                raise AuthError('The Antigravity credential directory is not owned safely.')
        fd = os.open(self.path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        info = os.fstat(fd)
        if info.st_uid != os.getuid() or not stat.S_ISDIR(info.st_mode):
            os.close(fd)
            raise AuthError('The Antigravity credential directory changed.')
        return fd

    @staticmethod
    def _read_at(parent: int, name: str, validate: bool = True) -> FileSnapshot:
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
            with os.fdopen(fd, 'rb') as stream:
                info = os.fstat(stream.fileno())
                if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                        or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1
                        or not 0 < info.st_size <= MAX_BYTES):
                    raise AuthError('The Antigravity credential file is not protected safely.')
                raw = stream.read(MAX_BYTES + 1)
                after = os.fstat(stream.fileno())
                if (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_size) != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size):
                    raise AuthError('The Antigravity credential changed while being read.')
                return FileSnapshot(credential(raw) if validate else PrivateCredential(raw, revision(raw)), info.st_dev, info.st_ino, info.st_mtime_ns)
        except AuthError:
            raise
        except OSError:
            raise AuthError('The protected Antigravity credential is not readable.') from None

    def read_current(self) -> FileSnapshot:
        try:
            parent = self._parent()
            try:
                return self._read_at(parent, self.path.name)
            finally:
                os.close(parent)
        except OSError:
            raise AuthError('The Antigravity credential directory is not readable.') from None

    @staticmethod
    def _same(left: FileSnapshot, right: FileSnapshot) -> bool:
        return (left.device, left.inode, left.modified_ns, left.credential.revision) == (right.device, right.inode, right.modified_ns, right.credential.revision)

    @staticmethod
    def _exchange(parent: int, left: str, right: str) -> None:
        libc = ctypes.CDLL(None, use_errno=True)
        call = getattr(libc, 'renameat2', None)
        if call is None:
            raise AuthError('Atomic credential exchange is unsupported on this host.')
        call.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        call.restype = ctypes.c_int
        if call(parent, os.fsencode(left), parent, os.fsencode(right), 2) != 0:
            raise AuthError('Atomic credential exchange failed.')

    def _install(self, next_credential: PrivateCredential, expected: FileSnapshot) -> InstallReceipt:
        parse_credential(next_credential.raw)
        parent = self._parent()
        staging = '.aac-credential-' + secrets.token_hex(16)
        staged_snapshot = None
        retain = False
        try:
            current = self._read_at(parent, self.path.name)
            if not self._same(current, expected):
                raise AuthError('Another process changed the Antigravity login. Review again.')
            fd = os.open(staging, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
            with os.fdopen(fd, 'wb') as stream:
                stream.write(next_credential.raw)
                stream.flush()
                os.fsync(stream.fileno())
            staged_snapshot = self._read_at(parent, staging)
            if not self._same(self._read_at(parent, self.path.name), expected):
                raise AuthError('Another process changed the Antigravity login. Review again.')
            self._exchange(parent, staging, self.path.name)
            displaced = self._read_at(parent, staging, validate=False)
            live = self._read_at(parent, self.path.name, validate=False)
            if not self._same(displaced, expected) or not self._same(live, staged_snapshot):
                retain = True
                # Restore whatever was displaced only while our exact inode is
                # still live. Never delete an unrecognized replacement.
                if self._same(live, staged_snapshot):
                    self._exchange(parent, staging, self.path.name)
                    restored = self._read_at(parent, self.path.name, validate=False)
                    if self._same(restored, displaced) and self._same(self._read_at(parent, staging, validate=False), staged_snapshot):
                        retain = False
                raise AuthError('A concurrent login change was preserved. Review again.')
            os.fsync(parent)
            return InstallReceipt(displaced, live)
        except OSError:
            raise AuthError('The Antigravity login could not be installed safely.') from None
        finally:
            if not retain and staged_snapshot is not None:
                try:
                    remaining = self._read_at(parent, staging)
                    # Success leaves the expected original inode staged; a
                    # pre-exchange failure leaves the owned new inode staged.
                    if self._same(remaining, expected) or self._same(remaining, staged_snapshot):
                        os.unlink(staging, dir_fd=parent)
                except (OSError, AuthError):
                    pass
            os.close(parent)

    def install(self, next_credential: PrivateCredential, expected: FileSnapshot) -> InstallReceipt:
        """Requires parent transaction's held activation lock and idle census."""
        return self._install(next_credential, expected)

    def rollback(self, receipt: InstallReceipt) -> InstallReceipt:
        """Refuses rollback when a foreign process replaced the written login."""
        return self._install(receipt.before.credential, receipt.installed)


class NativeAPI(Protocol):
    def read(self) -> bytes: ...
    def replace_existing(self, raw: bytes) -> None: ...

@dataclass(frozen=True)
class NativeReceipt:
    before: PrivateCredential = field(repr=False)
    installed: PrivateCredential = field(repr=False)

class ExistingNativeStore:
    """Reference keyring adapter. An existing target/ACL must be preserved.

    OS keyrings lack compare-and-swap; operations require the same external
    transaction/process lock. This does not claim atomicity against independent
    native login writers. Ubuntu production does not use this adapter.
    """
    def __init__(self, api: NativeAPI): self.api = api
    def read_current(self) -> PrivateCredential: return credential(self.api.read())
    def install(self, value: PrivateCredential, expected: PrivateCredential) -> NativeReceipt:
        parse_credential(value.raw)
        before = self.read_current()
        if before.revision != expected.revision:
            raise AuthError('Another process changed the Antigravity login. Review again.')
        try:
            self.api.replace_existing(value.raw)
            installed = self.read_current()
        except Exception:
            raise AuthError('The native Antigravity login update could not be verified.') from None
        if installed.revision != value.revision:
            raise AuthError('Another process changed the native Antigravity login.')
        return NativeReceipt(before, installed)
    def rollback(self, receipt: NativeReceipt) -> NativeReceipt:
        return self.install(receipt.before, receipt.installed)


class WindowsCredentialAPI:
    """Win32 Credential Manager; existing exact target, original metadata.

    Never create a credential or grant access. Read/modify must happen in the
    user's existing interactive logon session, not an SSH service session.
    """
    def __init__(self):
        if os.name != 'nt': raise AuthError('Windows Credential Manager is unavailable.')
        from ctypes import wintypes
        class FILETIME(ctypes.Structure): _fields_ = [('low', wintypes.DWORD), ('high', wintypes.DWORD)]
        class CREDENTIAL(ctypes.Structure):
            _fields_ = [('Flags', wintypes.DWORD), ('Type', wintypes.DWORD), ('TargetName', wintypes.LPWSTR), ('Comment', wintypes.LPWSTR), ('LastWritten', FILETIME), ('CredentialBlobSize', wintypes.DWORD), ('CredentialBlob', ctypes.POINTER(ctypes.c_ubyte)), ('Persist', wintypes.DWORD), ('AttributeCount', wintypes.DWORD), ('Attributes', ctypes.c_void_p), ('TargetAlias', wintypes.LPWSTR), ('UserName', wintypes.LPWSTR)]
        self.type = CREDENTIAL
        self.api = ctypes.WinDLL('Advapi32.dll', use_last_error=True)
        self.api.CredReadW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(ctypes.POINTER(CREDENTIAL))]
        self.api.CredReadW.restype = wintypes.BOOL
        self.api.CredWriteW.argtypes = [ctypes.POINTER(CREDENTIAL), wintypes.DWORD]
        self.api.CredWriteW.restype = wintypes.BOOL
        self.api.CredFree.argtypes = [ctypes.c_void_p]
    def _read(self):
        ptr = ctypes.POINTER(self.type)()
        if not self.api.CredReadW(WINDOWS_TARGET, 1, 0, ctypes.byref(ptr)):
            raise AuthError('The existing Antigravity credential is unavailable in this logon session.')
        return ptr
    def read(self):
        ptr = self._read()
        try:
            size = ptr.contents.CredentialBlobSize
            if not 0 < size <= MAX_BYTES: raise AuthError('Invalid native Antigravity credential size.')
            return ctypes.string_at(ptr.contents.CredentialBlob, size)
        finally: self.api.CredFree(ptr)
    def replace_existing(self, raw):
        parse_credential(raw)
        ptr = self._read()
        try:
            # Keep all original native metadata and access semantics.
            buffer = (ctypes.c_ubyte * len(raw)).from_buffer_copy(raw)
            replacement = self.type.from_buffer_copy(ptr.contents)
            replacement.CredentialBlob = ctypes.cast(buffer, ctypes.POINTER(ctypes.c_ubyte))
            replacement.CredentialBlobSize = len(raw)
            if not self.api.CredWriteW(ctypes.byref(replacement), 0):
                raise AuthError('The existing native Antigravity credential could not be replaced.')
        finally: self.api.CredFree(ptr)


class MacKeychainAPI:
    """Security.framework access to the existing item, without changing ACLs."""
    def __init__(self):
        import sys
        if sys.platform != 'darwin': raise AuthError('macOS Keychain is unavailable.')
        self.api = ctypes.CDLL('/System/Library/Frameworks/Security.framework/Security')
        self.api.SecKeychainFindGenericPassword.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_char_p, ctypes.c_uint32, ctypes.c_char_p, ctypes.POINTER(ctypes.c_uint32), ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_void_p)]
        self.api.SecKeychainFindGenericPassword.restype = ctypes.c_int32
        self.api.SecKeychainItemFreeContent.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        self.api.SecKeychainItemModifyAttributesAndData.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
        self.api.SecKeychainItemModifyAttributesAndData.restype = ctypes.c_int32
        self.api.SecKeychainSetUserInteractionAllowed.argtypes = [ctypes.c_bool]
        self.api.SecKeychainSetUserInteractionAllowed.restype = ctypes.c_int32
        if self.api.SecKeychainSetUserInteractionAllowed(False) != 0:
            raise AuthError('macOS Keychain access without interaction is unavailable.')
        self.core = ctypes.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')
        self.core.CFRelease.argtypes = [ctypes.c_void_p]
    def _read(self):
        size, data, item = ctypes.c_uint32(), ctypes.c_void_p(), ctypes.c_void_p()
        status = self.api.SecKeychainFindGenericPassword(None, len(SERVICE), SERVICE.encode(), len(ACCOUNT), ACCOUNT.encode(), ctypes.byref(size), ctypes.byref(data), ctypes.byref(item))
        if status != 0: raise AuthError('The existing Antigravity Keychain item is unavailable without interaction.')
        try:
            if not 0 < size.value <= MAX_BYTES * 2: raise AuthError('Invalid Antigravity Keychain item size.')
            raw = ctypes.string_at(data, size.value)
        except Exception:
            if item: self.core.CFRelease(item)
            raise
        finally: self.api.SecKeychainItemFreeContent(None, data)
        return raw, item
    @staticmethod
    def decode(raw):
        try:
            if raw.startswith(b'go-keyring-base64:'): return base64.b64decode(raw[len(b'go-keyring-base64:'):], validate=True)
            if raw.startswith(b'go-keyring-encoded:'): return bytes.fromhex(raw[len(b'go-keyring-encoded:'):].decode('ascii'))
            return raw
        except ValueError: raise AuthError('Invalid Antigravity Keychain encoding.') from None
    def read(self):
        raw, item = self._read()
        try: return self.decode(raw)
        finally: self.core.CFRelease(item)
    def replace_existing(self, raw):
        parse_credential(raw)
        before, item = self._read()
        try:
            # Preserve the installed item's encoding and its existing ACL.
            if before.startswith(b'go-keyring-base64:'): encoded = b'go-keyring-base64:' + base64.b64encode(raw)
            elif before.startswith(b'go-keyring-encoded:'): encoded = b'go-keyring-encoded:' + raw.hex().encode('ascii')
            else: encoded = raw
            buffer = ctypes.create_string_buffer(encoded)
            if self.api.SecKeychainItemModifyAttributesAndData(item, None, len(encoded), buffer) != 0:
                raise AuthError('The existing Antigravity Keychain item could not be replaced.')
        finally: self.core.CFRelease(item)

@dataclass(frozen=True)
class UbuntuBackendProbe:
    store: str
    service_running: bool
    unlocked_matches: int
    locked_matches: int


def probe_ubuntu_backend() -> UbuntuBackendProbe:
    """Read target metadata only; never activate/unlock/create a keyring item."""
    try:
        import dbus
        bus = dbus.SessionBus()
        daemon = dbus.Interface(bus.get_object('org.freedesktop.DBus', '/org/freedesktop/DBus'), 'org.freedesktop.DBus')
        if not daemon.NameHasOwner('org.freedesktop.secrets', timeout=5):
            return UbuntuBackendProbe('fallback-file', False, 0, 0)
        owner = daemon.GetNameOwner('org.freedesktop.secrets', timeout=5)
        if int(daemon.GetConnectionUnixUser(owner, timeout=5)) != os.getuid():
            return UbuntuBackendProbe('unavailable', True, 0, 0)
        service = dbus.Interface(bus.get_object(owner, '/org/freedesktop/secrets'), 'org.freedesktop.Secret.Service')
        unlocked, locked = service.SearchItems(dbus.Dictionary({'service': SERVICE, 'username': ACCOUNT}, signature='ss'), timeout=5)
        total = len(unlocked) + len(locked)
        store = 'fallback-file' if total == 0 else 'locked' if locked else 'secret-service' if total == 1 else 'ambiguous'
        return UbuntuBackendProbe(store, True, len(unlocked), len(locked))
    except Exception:
        return UbuntuBackendProbe('unavailable', False, 0, 0)


class UbuntuFileStore:
    """Production fallback composition with a fresh native-target guard.

    The currently installed Ubuntu login has no native Secret Service entry.
    If an independent login creates one, stop and review the new backend rather
    than writing a file that the native CLI might ignore. No native item is
    changed, created or unlocked by this composition.
    """
    def __init__(self, home: pathlib.Path, probe: Callable[[], UbuntuBackendProbe] = probe_ubuntu_backend):
        self.file = SecureFileStore(home)
        self.probe = probe
    def _guard(self):
        if self.probe().store != 'fallback-file':
            raise AuthError('The native Antigravity credential backend changed. Review again.')
    def read_current(self) -> FileSnapshot:
        self._guard()
        result = self.file.read_current()
        self._guard()
        return result
    def install(self, value: PrivateCredential, expected: FileSnapshot) -> InstallReceipt:
        self._guard()
        receipt = self.file.install(value, expected)
        try:
            self._guard()
        except AuthError:
            # Restore only our exact written file. Never replace or delete the
            # newly appeared keyring item or a foreign file writer's changes.
            self.file.rollback(receipt)
            raise AuthError('A native Antigravity login changed during activation; the file update was rolled back.') from None
        return receipt
    def rollback(self, receipt: InstallReceipt) -> InstallReceipt:
        return self.file.rollback(receipt)
