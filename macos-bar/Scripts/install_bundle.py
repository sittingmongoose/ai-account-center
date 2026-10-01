#!/usr/bin/env python3
"""Install the renamed macOS bundle without touching private CCS connection data.

Filesystem helpers are importable so migration/rollback can be checked without
codesigning, launchctl, process signals, or an actual user installation.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
import ctypes
import errno
import fcntl
import hashlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
from typing import Callable, Iterator, Sequence
from uuid import uuid4


APP_NAME = "AI Account Center.app"
LEGACY_NAME = "CCS Bar.app"
EXECUTABLE = "CCSBar"
BUNDLE_ID = "party.sittingmongoose.ccs.accounts-bar"
LEGACY_LINK = APP_NAME


class InstallError(RuntimeError):
    pass


@dataclass(frozen=True)
class InstallPaths:
    applications: Path
    target: Path
    legacy: Path
    backups: Path
    agent: Path

    @classmethod
    def for_home(cls, home: Path) -> "InstallPaths":
        applications = home / "Applications"
        return cls(
            applications,
            applications / APP_NAME,
            applications / LEGACY_NAME,
            home / "Library/Application Support/CCS Bar/Backups",
            home / "Library/LaunchAgents" / (BUNDLE_ID + ".plist"),
        )


def path_exists(path: Path) -> bool:
    """Unlike Path.exists(), a dangling alias is still an occupied path."""
    return os.path.lexists(path)


@dataclass(frozen=True)
class PathIdentity:
    """Remember the actual filesystem object, not just its install pathname."""
    root: tuple[int, int, int]
    link: str | None = None
    info: tuple[int, int, int, int, int] | None = None
    executable: tuple[int, int, int, int, int] | None = None
    info_digest: bytes | None = None

    @staticmethod
    def file_identity(path: Path) -> tuple[int, int, int, int, int]:
        value = path.lstat()
        return value.st_dev, value.st_ino, value.st_mode, value.st_size, value.st_mtime_ns

    @classmethod
    def capture(cls, path: Path) -> "PathIdentity":
        value = path.lstat()
        root = value.st_dev, value.st_ino, value.st_mode
        if stat.S_ISLNK(value.st_mode):
            if os.readlink(path) != LEGACY_LINK:
                raise InstallError("Refusing to claim an unrelated alias: " + str(path))
            return cls(root, link=LEGACY_LINK)
        validate_bundle(path)
        info = path / "Contents/Info.plist"
        executable = path / "Contents/MacOS" / EXECUTABLE
        return cls(root, info=cls.file_identity(info), executable=cls.file_identity(executable),
                   info_digest=hashlib.sha256(info.read_bytes()).digest())

    def matches(self, path: Path) -> bool:
        try:
            return self == self.capture(path)
        except (OSError, InstallError):
            return False


@dataclass(frozen=True)
class FileIdentity:
    """Identify a regular preference file by inode, metadata and exact content."""
    metadata: tuple[int, int, int, int, int, int]
    digest: bytes

    @staticmethod
    def stat_identity(value: os.stat_result) -> tuple[int, int, int, int, int, int]:
        return value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_size, value.st_mtime_ns

    @classmethod
    def capture(cls, path: Path) -> "FileIdentity":
        if not stat.S_ISREG(path.lstat().st_mode):
            raise InstallError("Preference identity requires a regular file: " + str(path))
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(descriptor, "rb") as handle:
            before = os.fstat(handle.fileno())
            if not stat.S_ISREG(before.st_mode):
                raise InstallError("Preference identity requires a regular file: " + str(path))
            digest = hashlib.sha256(handle.read()).digest()
            after = os.fstat(handle.fileno())
        if cls.stat_identity(before) != cls.stat_identity(after):
            raise InstallError("The preference changed while its identity was read: " + str(path))
        return cls(cls.stat_identity(after), digest)

    def matches(self, path: Path) -> bool:
        try:
            return self == self.capture(path)
        except (OSError, InstallError):
            return False


def rename_into_empty(source: Path, destination: Path) -> None:
    """Atomic no-overwrite rename, including a last-moment occupied destination."""
    library = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        rename = library.renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        result = rename(os.fsencode(source), os.fsencode(destination), 0x00000004)  # RENAME_EXCL
    elif sys.platform.startswith("linux") and hasattr(library, "renameat2"):
        rename = library.renameat2
        rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        result = rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1)  # RENAME_NOREPLACE
    else:
        raise OSError(errno.ENOTSUP, "An atomic no-overwrite rename is unavailable.", str(destination))
    if result != 0:
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value), str(destination))


def ensure_plain_directory(path: Path) -> None:
    """Create a directory without following a substituted parent symlink."""
    if path_exists(path):
        if path.is_symlink() or not path.is_dir():
            raise InstallError("Installation directory is a symlink or non-directory: " + str(path))
        return
    ensure_plain_directory(path.parent)
    path.mkdir()


def new_backup_directory(paths: InstallPaths) -> Path:
    ensure_plain_directory(paths.backups)
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    backup = paths.backups / (timestamp + "-" + uuid4().hex)
    backup.mkdir(mode=0o700)
    return backup


def cleanup_owned_path(path: Path, identity: PathIdentity | FileIdentity, backup: Path) -> None:
    """Delete only the same object, with a second check inside a private backup."""
    if not path_exists(path):
        return
    if not identity.matches(path):
        raise InstallError("Cleanup preserved a changed path: " + str(path) +
                           "; recovery backups are in " + str(backup) + ".")
    holding = backup / (".rollback-" + uuid4().hex + "-" + path.name)
    try:
        rename_into_empty(path, holding)
        if not identity.matches(holding):
            if not path_exists(path):
                rename_into_empty(holding, path)
            raise InstallError("Cleanup preserved a replaced path: " + str(path) +
                               "; any displaced object remains in " + str(backup) + ".")
        if isinstance(identity, PathIdentity) and identity.link is None:
            shutil.rmtree(holding)
        else:
            holding.unlink()
    except OSError as error:
        raise InstallError("Cleanup could not finish for " + str(path) +
                           "; preserved objects remain in " + str(backup) + ".") from error


def validate_bundle(path: Path) -> None:
    if path.is_symlink() or not path.is_dir():
        raise InstallError("App bundle must be a real directory: " + str(path))
    info = path / "Contents/Info.plist"
    executable = path / "Contents/MacOS" / EXECUTABLE
    if info.is_symlink() or executable.is_symlink():
        raise InstallError("App identity and executable must not be external aliases: " + str(path))
    try:
        with info.open("rb") as handle:
            identity = plistlib.load(handle)
    except (OSError, ValueError, plistlib.InvalidFileException) as error:
        raise InstallError("App bundle identity is unreadable: " + str(path)) from error
    if not isinstance(identity, dict) or identity.get("CFBundleIdentifier") != BUNDLE_ID:
        raise InstallError("Refusing to replace an app with a different bundle identifier: " + str(path))
    if identity.get("CFBundleExecutable") != EXECUTABLE:
        raise InstallError("App bundle has an unexpected executable: " + str(path))
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise InstallError("App executable is missing or not executable: " + str(path))


def inspect_install_path(path: Path, *, legacy: bool = False) -> str:
    """Return absent/app/alias, rejecting unrelated files or link destinations."""
    if not path_exists(path):
        return "absent"
    if path.is_symlink():
        if legacy and os.readlink(path) == LEGACY_LINK:
            return "alias"
        raise InstallError("Refusing an unexpected app symlink: " + str(path))
    validate_bundle(path)
    return "app"


def inspect_install(paths: InstallPaths) -> tuple[str, str]:
    return inspect_install_path(paths.target), inspect_install_path(paths.legacy, legacy=True)


def read_agent(paths: InstallPaths) -> tuple[bytes | None, int | None, bool]:
    """Keep a disabled existing install disabled; a clean install defaults on."""
    target_state, legacy_state = inspect_install(paths)
    existing_install = target_state != "absent" or legacy_state != "absent"
    if not path_exists(paths.agent):
        return None, None, not existing_install
    if paths.agent.is_symlink() or not paths.agent.is_file():
        raise InstallError("Refusing a symlink or non-file launch preference.")
    data = paths.agent.read_bytes()
    try:
        document = plistlib.loads(data)
    except (ValueError, plistlib.InvalidFileException) as error:
        raise InstallError("The existing launch preference is invalid.") from error
    allowed = [str(path / "Contents/MacOS" / EXECUTABLE) for path in (paths.target, paths.legacy)]
    if not isinstance(document, dict) or document.get("Label") != BUNDLE_ID:
        raise InstallError("The existing launch agent belongs to another application.")
    if document.get("ProgramArguments") not in [[path] for path in allowed]:
        raise InstallError("The existing launch agent has an unexpected executable.")
    enabled = document.get("RunAtLoad", True)
    if not isinstance(enabled, bool):
        raise InstallError("The existing launch preference has an invalid enabled setting.")
    return data, stat.S_IMODE(paths.agent.stat().st_mode), enabled


def atomic_write(path: Path, data: bytes, mode: int = 0o644, *, overwrite: bool = True) -> FileIdentity:
    ensure_plain_directory(path.parent)
    temporary = path.with_name("." + path.name + "." + uuid4().hex + ".tmp")
    identity: FileIdentity | None = None
    try:
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
            os.fchmod(handle.fileno(), mode)
            identity = FileIdentity(FileIdentity.stat_identity(os.fstat(handle.fileno())), hashlib.sha256(data).digest())
        if overwrite:
            os.replace(temporary, path)
        else:
            rename_into_empty(temporary, path)
        return identity
    finally:
        if path_exists(temporary):
            if identity is not None and identity.matches(temporary):
                temporary.unlink()
            else:
                raise InstallError("Atomic write preserved a changed temporary file: " + str(temporary))


def agent_document(paths: InstallPaths, enabled: bool) -> bytes:
    return plistlib.dumps({
        "Label": BUNDLE_ID,
        "ProgramArguments": [str(paths.target / "Contents/MacOS" / EXECUTABLE)],
        "RunAtLoad": enabled,
        "ProcessType": "Interactive",
        "LimitLoadToSessionType": "Aqua",
    })


@contextmanager
def installation_lock(paths: InstallPaths) -> Iterator[None]:
    ensure_plain_directory(paths.applications)
    lock = paths.applications / ".ai-account-center-install.lock"
    flags = os.O_WRONLY | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(lock, flags, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise InstallError("Another AI Account Center installation is running.") from error
        yield
    finally:
        os.close(descriptor)


class BundleTransaction:
    """Move old bundles aside and install by rename, with explicit rollback."""
    def __init__(self, paths: InstallPaths):
        self.paths = paths
        self.backup: Path | None = None
        self.moved: list[tuple[Path, Path]] = []
        self.backup_identities: dict[Path, PathIdentity] = {}
        self.new_target: PathIdentity | None = None
        self.new_alias: PathIdentity | None = None
        self.alias_stage: Path | None = None
        self.alias_identity: PathIdentity | None = None

    def install(self, stage: Path, *, after_replace: Callable[[], None] | None = None,
                expected_identity: PathIdentity | None = None) -> None:
        validate_bundle(stage)
        if stage.parent != self.paths.applications or stage in (self.paths.target, self.paths.legacy):
            raise InstallError("The staged bundle must be a separate sibling of the installed app.")
        target_state, legacy_state = inspect_install(self.paths)
        identities = {path: PathIdentity.capture(path) for path, state in
                      ((self.paths.target, target_state), (self.paths.legacy, legacy_state))
                      if state != "absent"}
        staged_identity = expected_identity or PathIdentity.capture(stage)
        if not staged_identity.matches(stage):
            raise InstallError("The prepared staging path was replaced; it was preserved: " + str(stage))
        self.backup = new_backup_directory(self.paths)
        try:
            for path, state in ((self.paths.target, target_state), (self.paths.legacy, legacy_state)):
                if state != "absent":
                    if not identities[path].matches(path):
                        raise InstallError("The existing app changed during installation: " + str(path))
                    destination = self.backup / path.name
                    rename_into_empty(path, destination)
                    self.moved.append((path, destination))
                    self.backup_identities[destination] = identities[path]
            if path_exists(self.paths.target) or not staged_identity.matches(stage):
                raise InstallError("The staged or destination app changed during installation.")
            rename_into_empty(stage, self.paths.target)
            self.new_target = staged_identity
            if after_replace is not None:
                after_replace()
            self.alias_stage = self.paths.applications / (".ai-account-center-alias-" + uuid4().hex)
            self.alias_stage.symlink_to(LEGACY_LINK)
            self.alias_identity = PathIdentity.capture(self.alias_stage)
            if path_exists(self.paths.legacy):
                raise InstallError("The compatibility path was occupied during installation: " + str(self.paths.legacy))
            rename_into_empty(self.alias_stage, self.paths.legacy)
            self.new_alias = self.alias_identity
            self.alias_stage = None
            self.alias_identity = None
        except BaseException:
            self.rollback()
            raise

    def rollback(self) -> None:
        collisions: list[str] = []

        def remove_owned(path: Path, identity: PathIdentity) -> bool:
            if not path_exists(path):
                return True
            if not identity.matches(path):
                collisions.append(str(path))
                return False
            # Recheck the object after moving it into the private backup directory.
            # This avoids deleting a replacement made between the check and rename.
            holding = self.backup / (".rollback-" + uuid4().hex + "-" + path.name)
            try:
                rename_into_empty(path, holding)
                if not identity.matches(holding):
                    if not path_exists(path):
                        rename_into_empty(holding, path)
                    collisions.append(str(path))
                    return False
                if identity.link is not None:
                    holding.unlink()
                else:
                    shutil.rmtree(holding)
            except OSError:
                # Leave any incomplete cleanup in the private backup, never clobber
                # a path that appeared while rollback was running.
                collisions.append(str(path))
                return False
            return True

        if self.alias_stage is not None and self.alias_identity is not None:
            if remove_owned(self.alias_stage, self.alias_identity):
                self.alias_stage = None
                self.alias_identity = None
        if self.new_alias is not None and remove_owned(self.paths.legacy, self.new_alias):
            self.new_alias = None
        if self.new_target is not None and remove_owned(self.paths.target, self.new_target):
            self.new_target = None
        unresolved: list[tuple[Path, Path]] = []
        for original, backup in reversed(self.moved):
            identity = self.backup_identities[backup]
            # Never overwrite a replacement, even one with the same bundle ID.
            if path_exists(original) or not identity.matches(backup):
                collisions.append(str(original))
                unresolved.append((original, backup))
                continue
            try:
                rename_into_empty(backup, original)
            except OSError:
                collisions.append(str(original))
                unresolved.append((original, backup))
                continue
            del self.backup_identities[backup]
        self.moved = list(reversed(unresolved))
        if collisions:
            raise InstallError("Rollback preserved changed paths: " + ", ".join(dict.fromkeys(collisions)) +
                               ". Original apps remain in " + str(self.backup) +
                               "; resolve the occupied paths before restoring those backups.")


def run_command(arguments: Sequence[str]) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(list(arguments), check=True, capture_output=True, text=True)
    except subprocess.CalledProcessError as error:
        # Never echo inherited command output or authentication material.
        raise InstallError("Installation command failed: " + Path(arguments[0]).name) from error


@dataclass(frozen=True)
class PreparedStage:
    path: Path
    identity: PathIdentity


def prepare_stage(source: Path, paths: InstallPaths, runner: Callable = run_command) -> PreparedStage:
    validate_bundle(source)
    inspect_install(paths)
    stage = paths.applications / (".ai-account-center-stage-" + uuid4().hex + ".app")
    identity: PathIdentity | None = None
    try:
        runner(["/usr/bin/ditto", str(source), str(stage)])
        validate_bundle(stage)
        identity = PathIdentity.capture(stage)
        runner(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(stage)])
        if not identity.matches(stage):
            raise InstallError("The staged bundle changed during verification: " + str(stage))
        return PreparedStage(stage, identity)
    except BaseException as error:
        if path_exists(stage):
            if identity is None:
                raise InstallError("Stage cleanup preserved an unverified path: " + str(stage)) from error
            cleanup_owned_path(stage, identity, new_backup_directory(paths))
        raise


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    birth: str
    executable: str


def process_identities(runner: Callable) -> dict[int, ProcessIdentity]:
    # lstart is stable for a macOS process lifetime; comm keeps spaces in app paths.
    rows = runner(["/bin/ps", "-axo", "pid=,lstart=,comm="]).stdout.splitlines()
    identities: dict[int, ProcessIdentity] = {}
    for row in rows:
        parts = row.strip().split(None, 6)
        if len(parts) == 7 and parts[0].isdigit():
            pid = int(parts[0])
            identities[pid] = ProcessIdentity(pid, " ".join(parts[1:6]), parts[6])
    return identities


def validate_installed_program(paths: InstallPaths, program: str) -> None:
    allowed = {str(path / "Contents/MacOS" / EXECUTABLE): path for path in (paths.target, paths.legacy)}
    app = allowed.get(program)
    if app is None:
        raise InstallError("The loaded service has an unexpected executable; it was left unchanged.")
    state = inspect_install_path(app, legacy=app == paths.legacy)
    if state == "alias":
        validate_bundle(paths.target)
    elif state != "app":
        raise InstallError("The running app bundle is missing; no process or service was stopped.")


def loaded_service_program(paths: InstallPaths, service: str, runner: Callable) -> str | None:
    try:
        output = runner(["/bin/launchctl", "print", service]).stdout
    except InstallError:
        return None
    programs = re.findall(r"(?m)^\s*program = ([^\r\n]+)$", output)
    if len(programs) != 1:
        raise InstallError("The loaded launch service identity is unreadable; it was left unchanged.")
    program = programs[0].strip()
    arguments = re.search(r"(?ms)^\s*arguments = \{\s*\n(.*?)^\s*\}", output)
    if "arguments =" in output and arguments is None:
        raise InstallError("The loaded launch service arguments are unreadable; it was left unchanged.")
    if arguments is not None:
        values = [value.strip() for value in arguments[1].splitlines() if value.strip()]
        if values != [program]:
            raise InstallError("The loaded launch service has unexpected arguments; it was left unchanged.")
    validate_installed_program(paths, program)
    return program


def disabled_override(domain: str, runner: Callable) -> bool | None:
    output = runner(["/bin/launchctl", "print-disabled", domain]).stdout
    if "{" not in output or "}" not in output:
        raise InstallError("The system sign-in startup preference could not be read.")
    label = r'"' + re.escape(BUNDLE_ID) + r'"'
    occurrences = len(re.findall(label, output))
    entries = re.findall(r'(?m)^\s*' + label + r'\s*=>\s*([^\r\n]*)$', output)
    if occurrences == 0:
        return None
    values = {"true": True, "false": False, "disabled": True, "enabled": False}
    if occurrences != 1 or len(entries) != 1 or entries[0].strip() not in values:
        raise InstallError("The system sign-in startup preference is invalid.")
    return values[entries[0].strip()]


def stop_exact_processes(paths: InstallPaths, runner: Callable = run_command) -> int:
    executables = {str(path / "Contents/MacOS" / EXECUTABLE) for path in (paths.target, paths.legacy)}
    matched = [identity for identity in process_identities(runner).values() if identity.executable in executables]
    stopped = 0
    for identity in matched:
        # Revalidate the bundle and then PID birth/executable immediately before the signal.
        validate_installed_program(paths, identity.executable)
        if process_identities(runner).get(identity.pid) != identity:
            continue
        try:
            os.kill(identity.pid, signal.SIGTERM)
        except ProcessLookupError:
            continue
        stopped += 1
        for _ in range(50):
            # A reused PID is a different process, not an app still waiting to exit.
            if process_identities(runner).get(identity.pid) != identity:
                break
            time.sleep(0.1)
        else:
            raise InstallError("The installed app did not stop; the bundle was left unchanged.")
    return stopped


def install(source: Path, *, launch: bool = False, home: Path | None = None, runner: Callable = run_command) -> None:
    paths = InstallPaths.for_home(home or Path.home())
    with installation_lock(paths):
        previous_agent, previous_mode, enabled = read_agent(paths)
        previous_identity = FileIdentity.capture(paths.agent) if previous_agent is not None else None
        if previous_identity is not None and previous_identity.digest != hashlib.sha256(previous_agent).digest():
            raise InstallError("The launch preference changed during preflight; it was left unchanged.")
        # Copy and sign-check before stopping anything or moving an installed app.
        prepared = prepare_stage(source, paths, runner)
        stage = prepared.path
        domain = "gui/" + str(os.getuid())
        service = domain + "/" + BUNDLE_ID
        was_loaded = False
        unloaded_previous = False
        stopped = 0
        transaction = BundleTransaction(paths)
        agent_update_started = False
        written_agent_identity: FileIdentity | None = None
        original_agent_backup: Path | None = None
        original_agent_identity: FileIdentity | None = None
        bootstrap_attempted = False
        effective_enabled = enabled
        try:
            system_disabled = disabled_override(domain, runner)
            effective_enabled = enabled and system_disabled is not True
            loaded_program = loaded_service_program(paths, service, runner)
            was_loaded = loaded_program is not None
            if was_loaded:
                # Refresh ownership just before bootout, rather than trusting the label.
                current_program = loaded_service_program(paths, service, runner)
                if current_program is not None:
                    if current_program != loaded_program:
                        raise InstallError("The loaded launch service changed; it was left unchanged.")
                    runner(["/bin/launchctl", "bootout", service])
                    unloaded_previous = True
            stopped = stop_exact_processes(paths, runner)
            transaction.install(stage, expected_identity=prepared.identity)
            if enabled or previous_agent is not None:
                # Save the original bytes before touching the preference pathname.
                if previous_agent is not None:
                    original_agent_backup = transaction.backup / (BUNDLE_ID + ".plist")
                    original_agent_identity = atomic_write(original_agent_backup, previous_agent,
                                                          previous_mode or 0o644, overwrite=False)
                agent_update_started = True
                if path_exists(paths.agent):
                    if previous_identity is None:
                        raise InstallError("A launch preference appeared during installation; it was preserved: " + str(paths.agent))
                    cleanup_owned_path(paths.agent, previous_identity, transaction.backup)
                written_agent_identity = atomic_write(paths.agent, agent_document(paths, enabled),
                                                     previous_mode or 0o644, overwrite=False)
            if launch:
                if effective_enabled:
                    # Bootstrap respects launchctl's existing override; never enable it here.
                    bootstrap_attempted = True
                    runner(["/bin/launchctl", "bootstrap", domain, str(paths.agent)])
                else:
                    runner(["/usr/bin/open", str(paths.target)])
        except BaseException as error:
            # Stop only this newly installed launch-agent instance before undoing.
            cleanup_errors: list[InstallError] = []
            if bootstrap_attempted:
                try:
                    current_program = loaded_service_program(paths, service, runner)
                    if current_program is not None:
                        if (current_program != str(paths.target / "Contents/MacOS" / EXECUTABLE)
                                or transaction.new_target is None
                                or not transaction.new_target.matches(paths.target)):
                            raise InstallError("The launch service changed during rollback; it was left unchanged.")
                        runner(["/bin/launchctl", "bootout", service])
                except InstallError as cleanup:
                    cleanup_errors.append(cleanup)
            if agent_update_started:
                try:
                    if written_agent_identity is not None:
                        cleanup_owned_path(paths.agent, written_agent_identity, transaction.backup)
                    elif path_exists(paths.agent):
                        raise InstallError("Rollback preserved an occupied launch preference: " + str(paths.agent) +
                                           "; original preference bytes remain in " + str(transaction.backup) + ".")
                    if original_agent_backup is not None:
                        if (path_exists(paths.agent) or original_agent_identity is None
                                or not original_agent_identity.matches(original_agent_backup)):
                            raise InstallError("Rollback preserved the changed launch preference: " + str(paths.agent) +
                                               "; original preference bytes remain in " + str(transaction.backup) + ".")
                        rename_into_empty(original_agent_backup, paths.agent)
                except (InstallError, OSError) as cleanup:
                    cleanup_errors.append(InstallError(str(cleanup) + "; launch preference recovery backup: " +
                                                      str(transaction.backup)))
            try:
                transaction.rollback()
            except InstallError as cleanup:
                cleanup_errors.append(cleanup)
            # Restore the previously running installation when restoration is possible.
            if not cleanup_errors and unloaded_previous and previous_agent is not None:
                try:
                    runner(["/bin/launchctl", "bootstrap", domain, str(paths.agent)])
                except InstallError:
                    pass
            elif not cleanup_errors and stopped:
                previous_app = paths.target if paths.target.is_dir() else paths.legacy
                if previous_app.is_dir():
                    try:
                        runner(["/usr/bin/open", str(previous_app)])
                    except InstallError:
                        pass
            if cleanup_errors:
                raise InstallError(str(error) + "; " + "; ".join(str(value) for value in cleanup_errors)) from error
            raise
        finally:
            if path_exists(stage):
                pending_error = sys.exc_info()[1]
                try:
                    cleanup_owned_path(stage, prepared.identity, transaction.backup or new_backup_directory(paths))
                except InstallError as cleanup:
                    if pending_error is not None:
                        raise InstallError(str(pending_error) + "; " + str(cleanup)) from pending_error
                    raise
        print("Installed: " + str(paths.target))
        print("Compatibility alias: " + str(paths.legacy))
        print("Launch at sign-in: " + ("enabled" if effective_enabled else "disabled (preserved)"))


def main() -> int:
    parser = argparse.ArgumentParser(description="Install AI Account Center safely for the current macOS user.")
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--launch", action="store_true")
    arguments = parser.parse_args()
    if sys.platform != "darwin":
        print("This installer requires macOS.", file=sys.stderr)
        return 1
    try:
        install(arguments.source.absolute(), launch=arguments.launch)
    except (InstallError, OSError) as error:
        print("Installation failed: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
