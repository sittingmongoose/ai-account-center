#!/usr/bin/env python3
"""Offline installer checks: temporary fake bundles and mocked macOS commands."""
from __future__ import annotations

from contextlib import redirect_stdout
import io
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import install_bundle as migration


def make_bundle(path: Path, marker: str, *, identity: str = migration.BUNDLE_ID) -> Path:
    executable = path / "Contents/MacOS" / migration.EXECUTABLE
    executable.parent.mkdir(parents=True)
    executable.write_text(marker)
    executable.chmod(0o755)
    (path / "Contents/Info.plist").write_bytes(plistlib.dumps({
        "CFBundleIdentifier": identity,
        "CFBundleExecutable": migration.EXECUTABLE,
    }))
    return path


def marker(path: Path) -> str:
    return (path / "Contents/MacOS" / migration.EXECUTABLE).read_text()


class MockCommands:
    def __init__(self, *, fail_sign: bool = False, fail_bootstrap: bool = False,
                 loaded_program: str | None = None, disabled: bool | None = None,
                 loaded_output: str | None = None):
        self.calls: list[list[str]] = []
        self.fail_sign = fail_sign
        self.fail_bootstrap = fail_bootstrap
        self.loaded_program = loaded_program
        self.disabled = disabled
        self.loaded_output = loaded_output
        self.stage_path: Path | None = None

    def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
        self.calls.append(list(arguments))
        program = Path(arguments[0]).name
        if program == "ditto":
            shutil.copytree(arguments[1], arguments[2])
            self.stage_path = Path(arguments[2])
        elif program == "codesign" and self.fail_sign:
            raise migration.InstallError("Mock signature failure")
        elif program == "launchctl" and arguments[1] == "print-disabled":
            entry = ('"' + migration.BUNDLE_ID + '" => ' + str(self.disabled).lower()) if self.disabled is not None else ""
            return subprocess.CompletedProcess(arguments, 0, stdout="disabled services = {\n" + entry + "\n}\n", stderr="")
        elif program == "launchctl" and arguments[1] == "print":
            if self.loaded_output is not None:
                output = self.loaded_output
            elif self.loaded_program is not None:
                output = "program = " + self.loaded_program + "\narguments = {\n" + self.loaded_program + "\n}\n"
            else:
                raise migration.InstallError("Mock service is not loaded")
            return subprocess.CompletedProcess(arguments, 0, stdout=output, stderr="")
        elif program == "launchctl" and arguments[1] == "bootout":
            self.loaded_program = None
            self.loaded_output = None
        elif program == "launchctl" and arguments[1] == "bootstrap" and self.fail_bootstrap:
            raise migration.InstallError("Mock launch failure")
        return subprocess.CompletedProcess(arguments, 0, stdout="", stderr="")


class LiteralDisabledCommands(MockCommands):
    def __init__(self, entries: str):
        super().__init__()
        self.entries = entries

    def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
        if arguments[:2] == ["/bin/launchctl", "print-disabled"]:
            self.calls.append(list(arguments))
            return subprocess.CompletedProcess(arguments, 0,
                stdout="disabled services = {\n" + self.entries + "\n}\n", stderr="")
        return super().__call__(arguments)


class MigrationChecks(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="ai-account-center-migration-check-")
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.paths = migration.InstallPaths.for_home(self.home)
        self.paths.applications.mkdir()
        self.source = make_bundle(self.root / "source.app", "new-build")
        self.private = self.home / ".ccs/bar/accounts-connection.json"
        self.private.parent.mkdir(parents=True, mode=0o700)
        self.private.write_bytes(b"offline fixture; not real credentials\n")
        self.private.chmod(0o600)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def stage(self, value: str = "new-build") -> Path:
        return make_bundle(self.paths.applications / (".test-stage-" + str(len(list(self.paths.applications.iterdir()))) + ".app"), value)

    def install(self, runner: MockCommands, *, launch: bool = False) -> None:
        with redirect_stdout(io.StringIO()):
            migration.install(self.source, home=self.home, runner=runner, launch=launch)

    def test_real_legacy_migration_and_repeat_upgrade(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        first = migration.BundleTransaction(self.paths)
        first.install(self.stage())
        self.assertEqual(marker(self.paths.target), "new-build")
        self.assertTrue(self.paths.legacy.is_symlink())
        self.assertEqual(os.readlink(self.paths.legacy), migration.APP_NAME)
        self.assertEqual(self.paths.legacy.resolve(), self.paths.target.resolve())
        self.assertEqual(marker(first.backup / migration.LEGACY_NAME), "old-build")
        second = migration.BundleTransaction(self.paths)
        second.install(self.stage("second-build"))
        self.assertEqual(marker(self.paths.target), "second-build")
        self.assertEqual(marker(second.backup / migration.APP_NAME), "new-build")
        self.assertTrue((second.backup / migration.LEGACY_NAME).is_symlink())
        self.assertNotEqual(first.backup, second.backup)

    def test_expected_and_broken_alias_are_repaired(self) -> None:
        self.paths.legacy.symlink_to(migration.LEGACY_LINK)
        self.assertFalse(self.paths.legacy.exists())
        self.assertEqual(migration.inspect_install(self.paths), ("absent", "alias"))
        transaction = migration.BundleTransaction(self.paths)
        transaction.install(self.stage())
        self.assertEqual(marker(self.paths.legacy), "new-build")
        self.assertEqual(os.readlink(self.paths.legacy), migration.LEGACY_LINK)

    def test_foreign_symlinks_files_and_bundle_ids_are_rejected(self) -> None:
        foreign = make_bundle(self.root / "foreign.app", "foreign", identity="example.unrelated")
        for destination in (self.paths.target, self.paths.legacy):
            for link in (str(foreign), "../elsewhere.app", "missing.app"):
                destination.symlink_to(link)
                with self.assertRaises(migration.InstallError):
                    migration.inspect_install(self.paths)
                self.assertEqual(os.readlink(destination), link)
                destination.unlink()
            destination.write_text("unrelated file")
            with self.assertRaises(migration.InstallError):
                migration.inspect_install(self.paths)
            self.assertEqual(destination.read_text(), "unrelated file")
            destination.unlink()
            make_bundle(destination, "foreign", identity="example.unrelated")
            with self.assertRaises(migration.InstallError):
                migration.inspect_install(self.paths)
            self.assertEqual(marker(destination), "foreign")
            shutil.rmtree(destination)
        self.paths.target.symlink_to(migration.LEGACY_NAME)
        with self.assertRaises(migration.InstallError):
            migration.inspect_install(self.paths)
        self.assertEqual(marker(foreign), "foreign")

    def test_rollback_restores_real_legacy_and_existing_canonical(self) -> None:
        make_bundle(self.paths.target, "old-canonical")
        make_bundle(self.paths.legacy, "old-legacy")
        transaction = migration.BundleTransaction(self.paths)
        def fail_after_replace() -> None:
            raise migration.InstallError("Injected failure after canonical replacement")
        with self.assertRaises(migration.InstallError):
            transaction.install(self.stage(), after_replace=fail_after_replace)
        self.assertEqual(marker(self.paths.target), "old-canonical")
        self.assertEqual(marker(self.paths.legacy), "old-legacy")
        self.assertFalse(self.paths.legacy.is_symlink())
        self.assertEqual(transaction.moved, [])

    def test_rollback_preserves_broken_alias(self) -> None:
        self.paths.legacy.symlink_to(migration.LEGACY_LINK)
        transaction = migration.BundleTransaction(self.paths)
        with self.assertRaises(RuntimeError):
            transaction.install(self.stage(), after_replace=lambda: (_ for _ in ()).throw(RuntimeError("injected")))
        self.assertFalse(migration.path_exists(self.paths.target))
        self.assertTrue(self.paths.legacy.is_symlink())
        self.assertFalse(self.paths.legacy.exists())
        self.assertEqual(os.readlink(self.paths.legacy), migration.LEGACY_LINK)

    def test_rollback_preserves_canonical_replacement_even_with_same_bundle_id(self) -> None:
        make_bundle(self.paths.target, "old-canonical")
        make_bundle(self.paths.legacy, "old-legacy")
        private_before = self.private.read_bytes(), self.private.stat().st_mode
        transaction = migration.BundleTransaction(self.paths)
        def replace_before_failure() -> None:
            self.paths.target.rename(self.root / "displaced-new.app")
            make_bundle(self.paths.target, "concurrent-canonical")
            raise migration.InstallError("Injected replacement race")
        with self.assertRaisesRegex(migration.InstallError, "Original apps remain in") as error:
            transaction.install(self.stage(), after_replace=replace_before_failure)
        self.assertIn(str(transaction.backup), str(error.exception))
        self.assertEqual(marker(self.paths.target), "concurrent-canonical")
        self.assertEqual(marker(transaction.backup / migration.APP_NAME), "old-canonical")
        self.assertEqual(marker(self.paths.legacy), "old-legacy")
        with self.assertRaises(migration.InstallError):
            transaction.rollback()
        self.assertEqual(marker(self.paths.target), "concurrent-canonical")
        self.assertEqual((self.private.read_bytes(), self.private.stat().st_mode), private_before)

    def test_rollback_preserves_legacy_replacement_and_original_backup(self) -> None:
        make_bundle(self.paths.target, "old-canonical")
        make_bundle(self.paths.legacy, "old-legacy")
        transaction = migration.BundleTransaction(self.paths)
        transaction.install(self.stage())
        self.paths.legacy.unlink()
        make_bundle(self.paths.legacy, "concurrent-legacy", identity="example.unrelated")
        with self.assertRaisesRegex(migration.InstallError, "resolve the occupied paths"):
            transaction.rollback()
        self.assertEqual(marker(self.paths.legacy), "concurrent-legacy")
        self.assertEqual(marker(transaction.backup / migration.LEGACY_NAME), "old-legacy")
        self.assertEqual(marker(self.paths.target), "old-canonical")
        with self.assertRaises(migration.InstallError):
            transaction.rollback()
        self.assertEqual(marker(self.paths.legacy), "concurrent-legacy")
        self.assertEqual(marker(self.paths.target), "old-canonical")

    def test_rollback_preserves_in_place_changed_app_identity(self) -> None:
        make_bundle(self.paths.target, "old-canonical")
        transaction = migration.BundleTransaction(self.paths)
        transaction.install(self.stage())
        info = self.paths.target / "Contents/Info.plist"
        info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "example.replacement", "CFBundleExecutable": migration.EXECUTABLE}))
        with self.assertRaises(migration.InstallError):
            transaction.rollback()
        self.assertEqual(plistlib.loads(info.read_bytes())["CFBundleIdentifier"], "example.replacement")
        self.assertEqual(marker(transaction.backup / migration.APP_NAME), "old-canonical")

    def test_atomic_restore_refuses_last_moment_occupied_destination(self) -> None:
        make_bundle(self.paths.target, "old-canonical")
        transaction = migration.BundleTransaction(self.paths)
        transaction.install(self.stage())
        rename = migration.rename_into_empty
        def concurrent_destination(source: Path, destination: Path) -> None:
            if source == transaction.backup / migration.APP_NAME:
                make_bundle(destination, "concurrent-at-restore")
            rename(source, destination)
        with patch.object(migration, "rename_into_empty", concurrent_destination):
            with self.assertRaises(migration.InstallError):
                transaction.rollback()
        self.assertEqual(marker(self.paths.target), "concurrent-at-restore")
        self.assertEqual(marker(transaction.backup / migration.APP_NAME), "old-canonical")

    def test_canonical_replacement_during_cleanup_is_returned_without_deletion(self) -> None:
        make_bundle(self.paths.target, "old-canonical")
        transaction = migration.BundleTransaction(self.paths)
        transaction.install(self.stage())
        rename = migration.rename_into_empty
        def replace_after_identity_check(source: Path, destination: Path) -> None:
            if source == self.paths.target and destination.name.startswith(".rollback-"):
                source.rename(self.root / "displaced-checked-new.app")
                make_bundle(source, "late-canonical-replacement")
            rename(source, destination)
        with patch.object(migration, "rename_into_empty", replace_after_identity_check):
            with self.assertRaises(migration.InstallError):
                transaction.rollback()
        self.assertEqual(marker(self.paths.target), "late-canonical-replacement")
        self.assertEqual(marker(transaction.backup / migration.APP_NAME), "old-canonical")

    def test_legacy_alias_replacement_during_cleanup_is_returned_without_deletion(self) -> None:
        make_bundle(self.paths.legacy, "old-legacy")
        foreign = make_bundle(self.root / "foreign.app", "foreign", identity="example.other")
        transaction = migration.BundleTransaction(self.paths)
        transaction.install(self.stage())
        rename = migration.rename_into_empty
        def replace_after_identity_check(source: Path, destination: Path) -> None:
            if source == self.paths.legacy and destination.name.startswith(".rollback-"):
                source.unlink()
                source.symlink_to(foreign)
            rename(source, destination)
        with patch.object(migration, "rename_into_empty", replace_after_identity_check):
            with self.assertRaises(migration.InstallError):
                transaction.rollback()
        self.assertEqual(os.readlink(self.paths.legacy), str(foreign))
        self.assertEqual(marker(foreign), "foreign")
        self.assertEqual(marker(transaction.backup / migration.LEGACY_NAME), "old-legacy")

    def test_launch_preference_clean_enabled_existing_absent_disabled(self) -> None:
        self.assertEqual(migration.read_agent(self.paths), (None, None, True))
        make_bundle(self.paths.legacy, "old-build")
        self.assertEqual(migration.read_agent(self.paths), (None, None, False))
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, True), 0o600)
        before = self.paths.agent.read_bytes()
        self.assertEqual(migration.read_agent(self.paths), (before, 0o600, True))
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, False), 0o600)
        self.assertFalse(migration.read_agent(self.paths)[2])

    def test_existing_disabled_upgrade_keeps_config_and_opens_only_new_app(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        private_before = self.private.read_bytes()
        private_mode = self.private.stat().st_mode
        runner = MockCommands()
        self.install(runner, launch=True)
        self.assertFalse(migration.path_exists(self.paths.agent))
        self.assertEqual(self.private.read_bytes(), private_before)
        self.assertEqual(self.private.stat().st_mode, private_mode)
        self.assertIn(["/usr/bin/open", str(self.paths.target)], runner.calls)
        self.assertFalse(any("bootstrap" in call or "enable" in call for call in runner.calls))

    def test_clean_install_and_enabled_upgrade_keep_stable_canonical_agent(self) -> None:
        runner = MockCommands()
        self.install(runner, launch=True)
        document = plistlib.loads(self.paths.agent.read_bytes())
        self.assertEqual(document["Label"], migration.BUNDLE_ID)
        self.assertEqual(document["ProgramArguments"], [str(self.paths.target / "Contents/MacOS/CCSBar")])
        self.assertTrue(document["RunAtLoad"])
        self.assertTrue(any("bootstrap" in call for call in runner.calls))
        second = MockCommands()
        self.install(second, launch=True)
        self.assertEqual(plistlib.loads(self.paths.agent.read_bytes()), document)
        self.assertEqual(os.readlink(self.paths.legacy), migration.LEGACY_LINK)

    def test_false_run_at_load_is_preserved_with_canonical_program_path(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = plistlib.dumps({
            "Label": migration.BUNDLE_ID,
            "ProgramArguments": [str(self.paths.legacy / "Contents/MacOS/CCSBar")],
            "RunAtLoad": False,
        })
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        runner = MockCommands()
        self.install(runner, launch=True)
        document = plistlib.loads(self.paths.agent.read_bytes())
        self.assertFalse(document["RunAtLoad"])
        self.assertEqual(document["Label"], migration.BUNDLE_ID)
        self.assertEqual(document["ProgramArguments"], [str(self.paths.target / "Contents/MacOS/CCSBar")])
        self.assertEqual(self.paths.agent.stat().st_mode & 0o777, 0o600)
        self.assertIn(["/usr/bin/open", str(self.paths.target)], runner.calls)
        self.assertFalse(any("bootstrap" in call or "enable" in call for call in runner.calls))

    def test_stage_signature_failure_touches_no_install_or_process(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        runner = MockCommands(fail_sign=True)
        with self.assertRaises(migration.InstallError):
            self.install(runner, launch=True)
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(migration.path_exists(self.paths.target))
        self.assertFalse(any(Path(call[0]).name in ("launchctl", "ps", "open") for call in runner.calls))
        self.assertFalse(list(self.paths.applications.glob(".ai-account-center-stage-*")))

    def test_launch_failure_rolls_back_app_alias_and_agent(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = plistlib.dumps({
            "Label": migration.BUNDLE_ID,
            "ProgramArguments": [str(self.paths.legacy / "Contents/MacOS/CCSBar")],
            "RunAtLoad": True,
        })
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        with self.assertRaises(migration.InstallError):
            self.install(MockCommands(fail_bootstrap=True), launch=True)
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(self.paths.legacy.is_symlink())
        self.assertFalse(migration.path_exists(self.paths.target))
        self.assertEqual(self.paths.agent.read_bytes(), old_agent)
        self.assertEqual(self.paths.agent.stat().st_mode & 0o777, 0o600)

    def test_clean_install_rollback_preserves_foreign_agent_replacement(self) -> None:
        foreign = b"foreign replacement; offline fixture\n"
        agent = self.paths.agent
        class ReplacedAgent(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "bootstrap"]:
                    self.calls.append(list(arguments))
                    agent.unlink()
                    agent.write_bytes(foreign)
                    raise migration.InstallError("Injected launch failure")
                return super().__call__(arguments)
        runner = ReplacedAgent()
        private_before = self.private.read_bytes(), self.private.stat().st_mode
        with self.assertRaisesRegex(migration.InstallError, "launch preference recovery backup") as error:
            self.install(runner, launch=True)
        self.assertIn(str(agent), str(error.exception))
        self.assertEqual(agent.read_bytes(), foreign)
        self.assertFalse(migration.path_exists(self.paths.target))
        self.assertFalse(migration.path_exists(self.paths.legacy))
        self.assertEqual((self.private.read_bytes(), self.private.stat().st_mode), private_before)

    def test_upgrade_rollback_preserves_foreign_agent_and_original_bytes(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = plistlib.dumps({"Label": migration.BUNDLE_ID,
                                   "ProgramArguments": [str(self.paths.legacy / "Contents/MacOS/CCSBar")],
                                   "RunAtLoad": True})
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        foreign = plistlib.dumps({"Label": "example.concurrent", "ProgramArguments": ["/foreign/program"]})
        agent = self.paths.agent
        class ReplacedAgent(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "bootstrap"]:
                    self.calls.append(list(arguments))
                    agent.unlink()
                    agent.write_bytes(foreign)
                    agent.chmod(0o640)
                    raise migration.InstallError("Injected launch failure")
                return super().__call__(arguments)
        with self.assertRaisesRegex(migration.InstallError, "launch preference recovery backup"):
            self.install(ReplacedAgent(), launch=True)
        self.assertEqual(agent.read_bytes(), foreign)
        self.assertEqual(agent.stat().st_mode & 0o777, 0o640)
        backups = list(self.paths.backups.glob("*/" + migration.BUNDLE_ID + ".plist"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), old_agent)
        self.assertEqual(backups[0].stat().st_mode & 0o777, 0o600)
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(migration.path_exists(self.paths.target))

    def test_in_place_agent_content_change_is_preserved_even_with_matching_metadata(self) -> None:
        agent = self.paths.agent
        foreign_content: list[bytes] = []
        class ChangedAgent(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "bootstrap"]:
                    self.calls.append(list(arguments))
                    before = agent.stat()
                    data = agent.read_bytes().replace(migration.BUNDLE_ID.encode(), b"x" * len(migration.BUNDLE_ID))
                    agent.write_bytes(data)
                    os.utime(agent, ns=(before.st_atime_ns, before.st_mtime_ns))
                    self_inode = agent.stat().st_ino
                    if self_inode != before.st_ino:
                        raise AssertionError("Fixture must keep the same agent inode")
                    foreign_content.append(data)
                    raise migration.InstallError("Injected launch failure")
                return super().__call__(arguments)
        with self.assertRaisesRegex(migration.InstallError, "changed path"):
            self.install(ChangedAgent(), launch=True)
        self.assertEqual(agent.read_bytes(), foreign_content[0])

    def test_agent_deleted_before_rollback_restores_original_bytes(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = migration.agent_document(self.paths, True)
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        agent = self.paths.agent
        class RemovedAgent(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "bootstrap"]:
                    self.calls.append(list(arguments))
                    agent.unlink()
                    raise migration.InstallError("Injected launch failure")
                return super().__call__(arguments)
        with self.assertRaises(migration.InstallError):
            self.install(RemovedAgent(), launch=True)
        self.assertEqual(agent.read_bytes(), old_agent)
        self.assertEqual(agent.stat().st_mode & 0o777, 0o600)

    def test_agent_restore_refuses_a_last_moment_foreign_destination(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = migration.agent_document(self.paths, True)
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        rename = migration.rename_into_empty
        foreign = b"last moment foreign agent\n"
        def concurrent_agent(source: Path, destination: Path) -> None:
            if source.name == migration.BUNDLE_ID + ".plist" and destination == self.paths.agent:
                destination.write_bytes(foreign)
            rename(source, destination)
        with patch.object(migration, "rename_into_empty", concurrent_agent):
            with self.assertRaisesRegex(migration.InstallError, "launch preference recovery backup"):
                self.install(MockCommands(fail_bootstrap=True), launch=True)
        self.assertEqual(self.paths.agent.read_bytes(), foreign)
        backups = list(self.paths.backups.glob("*/" + migration.BUNDLE_ID + ".plist"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), old_agent)
        self.assertEqual(marker(self.paths.legacy), "old-build")

    def test_agent_publication_refuses_a_last_moment_foreign_destination(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = migration.agent_document(self.paths, True)
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        rename = migration.rename_into_empty
        foreign = b"foreign agent appeared during publication\n"
        def concurrent_agent(source: Path, destination: Path) -> None:
            if source.name.endswith(".tmp") and destination == self.paths.agent:
                destination.write_bytes(foreign)
            rename(source, destination)
        with patch.object(migration, "rename_into_empty", concurrent_agent):
            with self.assertRaisesRegex(migration.InstallError, "launch preference recovery backup"):
                self.install(MockCommands(), launch=True)
        self.assertEqual(self.paths.agent.read_bytes(), foreign)
        backups = list(self.paths.backups.glob("*/" + migration.BUNDLE_ID + ".plist"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), old_agent)
        self.assertEqual(marker(self.paths.legacy), "old-build")

    def test_agent_replacement_during_rollback_cleanup_is_preserved(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        old_agent = migration.agent_document(self.paths, True)
        migration.atomic_write(self.paths.agent, old_agent, 0o600)
        rename = migration.rename_into_empty
        foreign = b"foreign replacement during agent cleanup\n"
        agent_moves = 0
        def concurrent_agent(source: Path, destination: Path) -> None:
            nonlocal agent_moves
            if source == self.paths.agent and destination.name.startswith(".rollback-"):
                agent_moves += 1
                if agent_moves == 2:
                    source.rename(self.root / "displaced-written-agent.plist")
                    source.write_bytes(foreign)
            rename(source, destination)
        with patch.object(migration, "rename_into_empty", concurrent_agent):
            with self.assertRaisesRegex(migration.InstallError, "launch preference recovery backup"):
                self.install(MockCommands(fail_bootstrap=True), launch=True)
        self.assertEqual(self.paths.agent.read_bytes(), foreign)
        backups = list(self.paths.backups.glob("*/" + migration.BUNDLE_ID + ".plist"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), old_agent)
        self.assertEqual(marker(self.paths.legacy), "old-build")

    def test_recreated_stage_after_successful_install_is_preserved(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        class RecreatedStage(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                result = super().__call__(arguments)
                if arguments[0] == "/usr/bin/open":
                    make_bundle(self.stage_path, "foreign-recreated-stage", identity="example.concurrent")
                return result
        runner = RecreatedStage()
        with self.assertRaisesRegex(migration.InstallError, "changed path"):
            self.install(runner, launch=True)
        self.assertEqual(marker(runner.stage_path), "foreign-recreated-stage")
        self.assertEqual(marker(self.paths.target), "new-build")
        self.assertTrue(self.paths.legacy.is_symlink())
        self.assertFalse(migration.path_exists(self.paths.agent))

    def test_replaced_prepared_stage_before_install_is_not_claimed_or_deleted(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        displaced = self.root / "displaced-verified.app"
        class ReplacedStage(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[0] == "/bin/ps":
                    self.stage_path.rename(displaced)
                    make_bundle(self.stage_path, "concurrent-same-id-stage")
                return super().__call__(arguments)
        runner = ReplacedStage()
        with self.assertRaisesRegex(migration.InstallError, "prepared staging path was replaced"):
            self.install(runner)
        self.assertEqual(marker(runner.stage_path), "concurrent-same-id-stage")
        self.assertEqual(marker(displaced), "new-build")
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(migration.path_exists(self.paths.target))

    def test_stage_replacement_during_failed_verification_is_preserved(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        displaced = self.root / "displaced-stage.app"
        class ReplacedStage(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[0] == "/usr/bin/codesign":
                    self.calls.append(list(arguments))
                    self.stage_path.rename(displaced)
                    make_bundle(self.stage_path, "foreign-during-verification", identity="example.concurrent")
                    raise migration.InstallError("Injected verification failure")
                return super().__call__(arguments)
        runner = ReplacedStage()
        with self.assertRaisesRegex(migration.InstallError, "changed path"):
            self.install(runner)
        self.assertEqual(marker(runner.stage_path), "foreign-during-verification")
        self.assertEqual(marker(displaced), "new-build")
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(any(Path(call[0]).name in ("launchctl", "ps", "open") for call in runner.calls))

    def test_foreign_launch_agent_is_rejected_before_commands(self) -> None:
        migration.atomic_write(self.paths.agent, plistlib.dumps({"Label": "example.other"}))
        runner = MockCommands()
        with self.assertRaises(migration.InstallError):
            self.install(runner)
        self.assertEqual(runner.calls, [])

    def test_loaded_foreign_service_is_never_booted_out(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        runner = MockCommands(loaded_program="/Applications/Unrelated.app/Contents/MacOS/other")
        with self.assertRaisesRegex(migration.InstallError, "unexpected executable"):
            self.install(runner, launch=True)
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(migration.path_exists(self.paths.target))
        self.assertFalse(any("bootout" in call or "bootstrap" in call or Path(call[0]).name == "ps" for call in runner.calls))

    def test_loaded_service_extra_arguments_are_rejected(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        program = str(self.paths.legacy / "Contents/MacOS/CCSBar")
        runner = MockCommands(loaded_output="program = " + program + "\narguments = {\n" + program + "\n--foreign-command\n}\n")
        with self.assertRaisesRegex(migration.InstallError, "unexpected arguments"):
            self.install(runner)
        self.assertFalse(any("bootout" in call for call in runner.calls))
        self.assertEqual(marker(self.paths.legacy), "old-build")

    def test_loaded_owned_service_is_revalidated_before_bootout(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        program = str(self.paths.legacy / "Contents/MacOS/CCSBar")
        runner = MockCommands(loaded_program=program)
        self.install(runner)
        prints = [index for index, call in enumerate(runner.calls) if call[:2] == ["/bin/launchctl", "print"]]
        bootout = runner.calls.index(["/bin/launchctl", "bootout", "gui/" + str(os.getuid()) + "/" + migration.BUNDLE_ID])
        self.assertEqual(len(prints), 2)
        self.assertLess(prints[-1], bootout)
        self.assertEqual(marker(self.paths.target), "new-build")

    def test_loaded_program_bundle_identity_is_checked_before_bootout(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        program = str(self.paths.legacy / "Contents/MacOS/CCSBar")
        class ChangedBundle(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "print"]:
                    info = self_path / "Contents/Info.plist"
                    info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "example.concurrent", "CFBundleExecutable": migration.EXECUTABLE}))
                return super().__call__(arguments)
        self_path = self.paths.legacy
        runner = ChangedBundle(loaded_program=program)
        with self.assertRaisesRegex(migration.InstallError, "different bundle identifier"):
            self.install(runner)
        self.assertFalse(any("bootout" in call for call in runner.calls))
        self.assertEqual(plistlib.loads((self.paths.legacy / "Contents/Info.plist").read_bytes())["CFBundleIdentifier"], "example.concurrent")

    def test_loaded_service_replacement_before_bootout_is_preserved(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        program = str(self.paths.legacy / "Contents/MacOS/CCSBar")
        class ChangedService(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "print"] and any("print" in call for call in self.calls):
                    self.loaded_program = "/foreign/replacement"
                return super().__call__(arguments)
        runner = ChangedService(loaded_program=program)
        with self.assertRaises(migration.InstallError):
            self.install(runner, launch=True)
        self.assertFalse(any("bootout" in call or "bootstrap" in call for call in runner.calls))
        self.assertEqual(marker(self.paths.legacy), "old-build")

    def test_rollback_does_not_boot_out_a_foreign_service_after_launch_failure(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, True))
        class ReplacedService(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "bootstrap"]:
                    self.calls.append(list(arguments))
                    self.loaded_program = "/foreign/replacement"
                    raise migration.InstallError("Injected bootstrap failure")
                return super().__call__(arguments)
        runner = ReplacedService()
        with self.assertRaisesRegex(migration.InstallError, "unexpected executable"):
            self.install(runner, launch=True)
        self.assertFalse(any("bootout" in call for call in runner.calls))
        self.assertEqual(marker(self.paths.legacy), "old-build")

    def test_system_disabled_override_is_preserved_with_true_run_at_load(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, True), 0o600)
        runner = MockCommands(disabled=True)
        self.install(runner, launch=True)
        self.assertTrue(plistlib.loads(self.paths.agent.read_bytes())["RunAtLoad"])
        self.assertTrue(runner.disabled)
        self.assertIn(["/usr/bin/open", str(self.paths.target)], runner.calls)
        self.assertFalse(any("bootstrap" in call or "enable" in call or "disable" in call for call in runner.calls))
        second = MockCommands(disabled=True)
        self.install(second, launch=True)
        self.assertTrue(plistlib.loads(self.paths.agent.read_bytes())["RunAtLoad"])
        self.assertFalse(any("bootstrap" in call or "enable" in call for call in second.calls))

    def test_explicit_enabled_override_is_not_rewritten(self) -> None:
        runner = MockCommands(disabled=False)
        self.install(runner, launch=True)
        self.assertFalse(runner.disabled)
        self.assertTrue(any("bootstrap" in call for call in runner.calls))
        self.assertFalse(any("enable" in call or "disable" in call for call in runner.calls))

    def test_enabled_word_override_bootstraps_without_rewriting_preference(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, True), 0o600)
        entry = '"' + migration.BUNDLE_ID + '" => enabled'
        runner = LiteralDisabledCommands(entry)
        self.install(runner, launch=True)
        self.assertTrue(plistlib.loads(self.paths.agent.read_bytes())["RunAtLoad"])
        self.assertEqual(self.paths.agent.stat().st_mode & 0o777, 0o600)
        self.assertTrue(any("bootstrap" in call for call in runner.calls))
        self.assertFalse(any("enable" in call or "disable" in call for call in runner.calls))
        self.assertEqual(runner.entries, entry)

    def test_disabled_word_override_opens_directly_without_rewriting_preference(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, True), 0o600)
        entry = '"' + migration.BUNDLE_ID + '" => disabled'
        runner = LiteralDisabledCommands(entry)
        self.install(runner, launch=True)
        self.assertTrue(plistlib.loads(self.paths.agent.read_bytes())["RunAtLoad"])
        self.assertIn(["/usr/bin/open", str(self.paths.target)], runner.calls)
        self.assertFalse(any("bootstrap" in call or "enable" in call or "disable" in call for call in runner.calls))
        self.assertEqual(runner.entries, entry)

    def test_mixed_duplicate_override_entries_refuse_before_actions(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        migration.atomic_write(self.paths.agent, migration.agent_document(self.paths, True), 0o600)
        before = self.paths.agent.read_bytes()
        label = '"' + migration.BUNDLE_ID + '" => '
        for first, second in (("enabled", "false"), ("disabled", "true"), ("enabled", "disabled"),
                              ("false", "true"), ("true", "malformed"), ("enabled", "garbage")):
            with self.subTest(first=first, second=second):
                runner = LiteralDisabledCommands(label + first + "\n" + label + second)
                with self.assertRaisesRegex(migration.InstallError, "preference is invalid"):
                    self.install(runner, launch=True)
                self.assertEqual(marker(self.paths.legacy), "old-build")
                self.assertEqual(self.paths.agent.read_bytes(), before)
                self.assertFalse(migration.path_exists(self.paths.target))
                self.assertFalse(any("bootout" in call or "bootstrap" in call or "enable" in call
                                     or Path(call[0]).name in ("ps", "open") for call in runner.calls))

    def test_malformed_override_value_refuses_before_actions(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        label = '"' + migration.BUNDLE_ID + '" => '
        for value in ("", "enable", "disabled-extra", "trueish", "false extra", "enabled # comment", '"disabled"', "ENABLED"):
            with self.subTest(value=value):
                runner = LiteralDisabledCommands(label + value)
                with self.assertRaisesRegex(migration.InstallError, "preference is invalid"):
                    self.install(runner, launch=True)
                self.assertEqual(marker(self.paths.legacy), "old-build")
                self.assertFalse(migration.path_exists(self.paths.target))
                self.assertFalse(any("bootout" in call or "bootstrap" in call or "enable" in call
                                     or Path(call[0]).name in ("ps", "open") for call in runner.calls))

    def test_unreadable_system_disabled_override_refuses_changes(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        class UnreadableOverride(MockCommands):
            def __call__(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
                if arguments[:2] == ["/bin/launchctl", "print-disabled"]:
                    self.calls.append(list(arguments))
                    return subprocess.CompletedProcess(arguments, 0, stdout="unrecognized preference", stderr="")
                return super().__call__(arguments)
        runner = UnreadableOverride()
        with self.assertRaisesRegex(migration.InstallError, "could not be read"):
            self.install(runner, launch=True)
        self.assertEqual(marker(self.paths.legacy), "old-build")
        self.assertFalse(any("bootout" in call or "bootstrap" in call for call in runner.calls))

    def test_stop_matches_only_two_exact_paths(self) -> None:
        make_bundle(self.paths.legacy, "old-build")
        make_bundle(self.paths.target, "new-build")
        old = str(self.paths.legacy / "Contents/MacOS/CCSBar")
        new = str(self.paths.target / "Contents/MacOS/CCSBar")
        birth = "Thu Oct  1 04:05:06 2026 "
        active = {101: old, 202: new, 303: "/Applications/CCS Bar.app/Contents/MacOS/CCSBar", 404: new + "-other"}
        killed: list[int] = []
        def fake_kill(pid: int, _signal: int) -> None:
            self.assertEqual(_signal, migration.signal.SIGTERM)
            killed.append(pid)
            del active[pid]
        def fake_ps(arguments: list[str]) -> subprocess.CompletedProcess[str]:
            self.assertEqual(arguments, ["/bin/ps", "-axo", "pid=,lstart=,comm="])
            rows = "".join(str(pid) + " " + birth + path + "\n" for pid, path in active.items())
            return subprocess.CompletedProcess(arguments, 0, stdout=rows, stderr="")
        with patch.object(migration.os, "kill", fake_kill):
            self.assertEqual(migration.stop_exact_processes(self.paths, fake_ps), 2)
        self.assertEqual(killed, [101, 202])

    def test_pid_reuse_before_signal_is_never_signalled(self) -> None:
        make_bundle(self.paths.target, "installed")
        program = str(self.paths.target / "Contents/MacOS/CCSBar")
        snapshots = iter(["101 Thu Oct 1 04:05:06 2026 " + program,
                          "101 Thu Oct 1 04:05:07 2026 " + program])
        def fake_ps(arguments: list[str]) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(arguments, 0, stdout=next(snapshots), stderr="")
        with patch.object(migration.os, "kill") as kill:
            self.assertEqual(migration.stop_exact_processes(self.paths, fake_ps), 0)
        kill.assert_not_called()

    def test_executable_change_before_signal_is_never_signalled(self) -> None:
        make_bundle(self.paths.target, "installed")
        program = str(self.paths.target / "Contents/MacOS/CCSBar")
        snapshots = iter(["101 Thu Oct 1 04:05:06 2026 " + program,
                          "101 Thu Oct 1 04:05:06 2026 /foreign/process"])
        def fake_ps(arguments: list[str]) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(arguments, 0, stdout=next(snapshots), stderr="")
        with patch.object(migration.os, "kill") as kill:
            self.assertEqual(migration.stop_exact_processes(self.paths, fake_ps), 0)
        kill.assert_not_called()

    def test_pid_reuse_after_signal_is_not_waited_on(self) -> None:
        make_bundle(self.paths.target, "installed")
        program = str(self.paths.target / "Contents/MacOS/CCSBar")
        original = "101 Thu Oct 1 04:05:06 2026 " + program
        snapshots = iter([original, original, "101 Thu Oct 1 04:05:07 2026 " + program])
        def fake_ps(arguments: list[str]) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(arguments, 0, stdout=next(snapshots), stderr="")
        with patch.object(migration.os, "kill") as kill, patch.object(migration.time, "sleep") as sleep:
            self.assertEqual(migration.stop_exact_processes(self.paths, fake_ps), 1)
        kill.assert_called_once_with(101, migration.signal.SIGTERM)
        sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
