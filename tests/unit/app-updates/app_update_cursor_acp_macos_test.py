"""Offline fixtures for Cursor, T3's ACP pair, and Mac login-session execution."""

import contextlib
import hashlib
import json
import os
import pathlib
import plistlib
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest
import zipfile
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_update_common as common
import app_update_cursor as cursor
import app_update_antigravity as acp
import app_update_macos as mac
import app_updates as updater


OLD, NEW = "2026.11.05-old", "2026.11.11-new"


class Fixtures(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="aac-new-update-fixture-")
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(mock.patch.object(pathlib.Path, "home", return_value=self.root))
        self.stack.enter_context(mock.patch.dict(os.environ, {"LOCALAPPDATA": str(self.root / "Local")}))
        self.stack.enter_context(mock.patch.object(acp.platform, "machine", return_value="x86_64"))
        self.stack.enter_context(mock.patch.object(mac.os, "getuid", return_value=501, create=True))
        if os.name == "nt":
            # Simulated Unix runtimes need Unix executable bits, absent on NTFS.
            real_stat = pathlib.Path.stat
            def unix_mode(path, *args, **kwargs):
                value = real_stat(path, *args, **kwargs)
                if path.name in ("agy_acp_server.par", "localharness_external"):
                    fields = list(value)
                    fields[0] |= 0o111
                    return os.stat_result(fields)
                return value
            self.stack.enter_context(mock.patch.object(pathlib.Path, "stat", unix_mode))

    def cursor(self, host="ubuntu", version=OLD):
        path = cursor.cursor_launcher(host)
        path.parent.mkdir(parents=True, exist_ok=True)
        if host == "windows":
            path.touch()
        else:
            binary = self.root / ".local/share/cursor-agent/versions" / version / "cursor-agent"
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text("#!/bin/sh\necho " + version + "\n")
            binary.chmod(0o755)
            path.unlink(missing_ok=True)
            path.symlink_to(binary)
        return common.Install("cursor-cli", host, path, version)

    def old_acp(self, host):
        root = acp.managed_root(host)
        old = root / "versions" / ("a" * 64)
        old.mkdir(parents=True, exist_ok=True)
        record = {"releaseId": old.name, "version": "1.3.0"}
        for kind, name in (("executable", "agy_acp_server" + (".exe" if host == "windows" else ".par")),
                           ("harness", "localharness_external" + (".exe" if host == "windows" else ""))):
            (old / name).write_bytes(b"old")
            (old / name).chmod(0o755)
            record[kind] = {"name": name, "bytes": 3}
        common.write_private_json(old / acp.RECORD, record)
        common.write_private_json(root / "active.json", {"releaseId": old.name})
        return acp.detect_antigravity_acp(host), old

    def release(self, host, initialized=None, extra=None):
        initialized = initialized or {"protocolVersion": 1,
            "agentInfo": {"name": "antigravity-acp", "version": "1.4.5"},
            "agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}, "auth": {"logout": {}}},
            "authMethods": [{"id": "oauth-personal"}]}
        script = ("#!" + sys.executable + "\nimport sys,json,time\n"
                  "json.loads(sys.stdin.readline())\n"
                  "print(json.dumps({'jsonrpc':'2.0','id':1,'result':" + repr(initialized) + "}),flush=True)\n"
                  "time.sleep(60)\n").encode()
        exe = "agy_acp_server" + (".exe" if host == "windows" else ".par")
        harness = "localharness_external" + (".exe" if host == "windows" else "")
        archive = self.root / (host + "-fixture.zip")
        with zipfile.ZipFile(archive, "w") as output:
            output.writestr(exe, script)
            output.writestr(harness, b"harness")
            if extra:
                output.writestr(*extra)
        asset = {"version": "1.4.5", "url": "https://dl.google.com/fixture.zip",
                 "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(), "archiveBytes": archive.stat().st_size,
                 "executable": {"name": exe, "bytes": len(script)}, "harness": {"name": harness, "bytes": 7}}
        self.stack.enter_context(mock.patch.dict(acp.RELEASES, {acp.platform_key(host): asset}))
        self.stack.enter_context(mock.patch.object(acp, "download", side_effect=lambda url, target, **kw: shutil.copyfile(archive, target)))
        return asset


class CursorTests(Fixtures):
    def test_detects_mac_from_official_release_link_without_accessing_keychain(self):
        install = self.cursor("mac")
        with mock.patch.object(cursor, "cli_probe") as probe:
            found = cursor.detect_cursor("mac")
        self.assertEqual((found.path, found.version, found.probe), (install.path, OLD, None))
        probe.assert_not_called()

    def test_windows_uses_powershell_launcher_for_probe_and_supported_update(self):
        install = self.cursor("windows")
        with mock.patch.object(cursor, "powershell", side_effect=[OLD, "", NEW]) as ps:
            found = cursor.detect_cursor("windows")
            row = cursor.update_cursor(found)
        self.assertEqual((row["previousVersion"], row["version"], row["status"]), (OLD, NEW, "updated"))
        self.assertIn(str(install.path), ps.call_args_list[1].args[0])
        self.assertIn(" update;", ps.call_args_list[1].args[0])
        self.assertNotIn("Remove-Item", str(ps.call_args_list))

    def test_windows_in_use_staging_is_action_required_on_success_and_failure(self):
        install = self.cursor("windows")
        staged = install.path.parent / "versions" / (NEW + ".new")
        staged.mkdir(parents=True)
        for error in (None, common.UpdateFailure()):
            with self.subTest(error=error), mock.patch.object(cursor, "windows_command", side_effect=error), \
                    mock.patch.object(cursor, "detect_cursor", return_value=install):
                row = cursor.update_cursor(install)
            self.assertEqual((row["status"], row["messageCode"], row["version"]),
                             ("action_required", "cursor_update_staged", OLD))
            self.assertTrue(staged.is_dir())

    def test_windows_prefers_ps1_and_accepts_the_older_cmd_only_layout(self):
        install = self.cursor("windows")
        self.assertEqual(install.path.suffix, ".cmd")
        ps1 = install.path.with_suffix(".ps1"); ps1.touch()
        self.assertEqual(cursor.cursor_launcher("windows"), ps1)

    def test_mac_update_runs_in_gui_and_reports_both_versions(self):
        install = self.cursor("mac")
        with mock.patch.object(cursor, "gui_command", side_effect=lambda *args: self.cursor("mac", NEW)) as gui:
            row = cursor.update_cursor(install, time.monotonic() + 60)
        self.assertEqual((row["status"], row["previousVersion"], row["version"]), ("updated", OLD, NEW))
        self.assertEqual(gui.call_args.args[0], "cursor-update")

    @unittest.skipIf(os.name == "nt", "executes Unix shebang fixtures")
    def test_linux_native_update_keeps_running_process_and_login_files(self):
        install = self.cursor()
        login = self.root / ".cursor/fixture-login"
        login.parent.mkdir(); login.write_text("unchanged")
        old = install.path.resolve()
        new = old.parent.parent / NEW / "cursor-agent"
        old.write_text("#!" + sys.executable + "\nimport sys,pathlib,os,time\n"
                       "p=pathlib.Path(" + repr(str(install.path)) + ")\n"
                       "if sys.argv[1]=='--version': print(" + repr(OLD) + ")\n"
                       "elif sys.argv[1]=='update':\n"
                       " n=pathlib.Path(" + repr(str(new)) + "); n.parent.mkdir(); n.write_text('#!/bin/sh\\necho " + NEW + "\\n'); n.chmod(0o755); p.unlink(); p.symlink_to(n)\n"
                       "else: time.sleep(60)\n")
        process = subprocess.Popen([str(old), "hold"])
        try:
            row = cursor.update_cursor(install)
            self.assertEqual((row["status"], row["previousVersion"], row["version"]), ("updated", OLD, NEW))
            self.assertIsNone(process.poll())
            self.assertTrue(old.is_file())
            self.assertEqual(login.read_text(), "unchanged")
        finally:
            process.terminate(); process.wait(timeout=5)

    def test_absent_unmanaged_and_failed_probes_do_not_claim_installed(self):
        self.assertIsNone(cursor.detect_cursor("ubuntu"))
        path = cursor.cursor_launcher("ubuntu"); path.parent.mkdir(parents=True); path.write_text("outside")
        self.assertEqual(cursor.detect_cursor("ubuntu").manager, "unsupported")
        self.cursor()
        with mock.patch.object(cursor, "cli_probe", return_value=(None, "timeout")):
            self.assertEqual(cursor.detect_cursor("ubuntu").probe, "timeout")

    def test_deadline_never_starts_updater(self):
        install = self.cursor()
        with mock.patch.object(cursor, "command") as run:
            row = cursor.update_cursor(install, time.monotonic() - 1)
        run.assert_not_called()
        self.assertFalse(row["updateAttempted"])
        self.assertEqual(row["messageCode"], "timeout")


class AcpTests(Fixtures):
    def test_pinned_platforms_are_exact_t3_google_sources(self):
        self.assertEqual(len(acp.RELEASES), 6)
        for asset in acp.RELEASES.values():
            self.assertEqual(asset["version"], "1.4.5")
            self.assertTrue(asset["url"].startswith("https://dl.google.com/agy-extensions/releases/"))
            self.assertRegex(asset["sha256"], r"^[a-f0-9]{64}$")

    def test_every_os_activates_pin_and_preserves_in_use_old_release(self):
        for host in ("ubuntu", "mac", "windows"):
            with self.subTest(host=host):
                install, old = self.old_acp(host)
                asset = self.release(host)
                process = subprocess.Popen([sys.executable, "-c", "import time;time.sleep(60)"], cwd=old)
                try:
                    with mock.patch.object(acp, "validate_runtime") as validate, mock.patch.object(acp, "command") as run:
                        row = acp.update_antigravity_acp(install)
                    self.assertEqual((row["previousVersion"], row["version"], row["status"]), ("1.3.0", "1.4.5", "updated"))
                    self.assertTrue(old.is_dir()); self.assertIsNone(process.poll())
                    self.assertEqual(acp.read_json(install.path / "active.json"), {"releaseId": asset["sha256"]})
                    self.assertTrue((install.path / "versions" / asset["sha256"] / acp.RECORD).is_file())
                    self.assertEqual(row["messageCode"], "antigravity_acp_updated")
                    self.assertEqual(acp.detect_antigravity_acp(host).version, "1.4.5")
                    self.assertTrue(validate.called)
                    if host == "mac":
                        self.assertEqual(run.call_args.args[0][:4], ["/usr/bin/codesign", "--force", "--sign", "-"])
                    self.assertFalse(list((install.path / "versions").glob(".install-*")))
                finally:
                    process.terminate(); process.wait(timeout=5)

    @unittest.skipIf(os.name == "nt", "executes Unix shebang fixtures")
    def test_real_fixture_acp_initialize_and_tree_cleanup(self):
        install, old = self.old_acp("ubuntu")
        self.release("ubuntu")
        row = acp.update_antigravity_acp(install)
        self.assertEqual((row["status"], row["version"]), ("updated", "1.4.5"))
        self.assertTrue(old.is_dir())

    @unittest.skipIf(os.name == "nt", "executes Unix shebang fixtures")
    def test_real_fixture_with_wrong_protocol_identity_never_activates(self):
        install, old = self.old_acp("ubuntu")
        self.release("ubuntu", initialized={"protocolVersion": 1, "agentInfo": {"name": "wrong", "version": "1.4.5"},
                     "agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}, "auth": {"logout": {}}},
                     "authMethods": [{"id": "oauth-personal"}]})
        row = acp.update_antigravity_acp(install)
        self.assertEqual(row["messageCode"], "signature_failed")
        self.assertEqual(acp.read_json(install.path / "active.json")["releaseId"], old.name)

    def test_failed_checksum_or_protocol_never_changes_active(self):
        for failure in ("checksum", "protocol"):
            with self.subTest(failure=failure):
                install, old = self.old_acp("ubuntu")
                asset = self.release("ubuntu")
                if failure == "checksum":
                    asset["sha256"] = "b" * 64
                with mock.patch.object(acp, "validate_runtime", side_effect=common.UpdateFailure("signature_failed")):
                    row = acp.update_antigravity_acp(install)
                self.assertEqual((row["status"], row["version"], row["messageCode"]), ("failed", "1.3.0", "signature_failed"))
                self.assertEqual(acp.read_json(install.path / "active.json")["releaseId"], old.name)
                self.assertFalse((install.path / "versions" / asset["sha256"]).exists())

    def test_extra_archive_member_is_rejected_even_with_valid_checksum(self):
        install, old = self.old_acp("ubuntu")
        self.release("ubuntu", extra=("../outside", b"unsafe"))
        with mock.patch.object(acp, "validate_runtime") as validate:
            row = acp.update_antigravity_acp(install)
        self.assertEqual(row["messageCode"], "signature_failed")
        validate.assert_not_called()
        self.assertEqual(acp.read_json(install.path / "active.json")["releaseId"], old.name)

    def test_incomplete_existing_destination_is_never_overwritten(self):
        install, old = self.old_acp("ubuntu")
        asset = self.release("ubuntu")
        destination = install.path / "versions" / asset["sha256"]
        destination.mkdir(); (destination / "keep").write_text("held")
        row = acp.update_antigravity_acp(install)
        self.assertEqual(row["status"], "failed")
        self.assertEqual((destination / "keep").read_text(), "held")
        self.assertEqual(acp.read_json(install.path / "active.json")["releaseId"], old.name)

    def test_unknown_pointer_is_unknown_and_absence_does_not_install(self):
        self.assertIsNone(acp.detect_antigravity_acp("ubuntu"))
        install, _ = self.old_acp("ubuntu")
        common.write_private_json(install.path / "active.json", {"releaseId": "../outside"})
        self.assertEqual(acp.detect_antigravity_acp("ubuntu").probe, "failed")

    def test_t3_activation_after_final_read_is_never_overwritten(self):
        for version in ("1.5.0", "1.4.5"):
            with self.subTest(version=version):
                install, old = self.old_acp("windows")
                self.release("windows")
                newer = old.with_name("c" * 64)
                newer.mkdir(exist_ok=True)
                record = acp.read_json(old / acp.RECORD)
                record.update(releaseId=newer.name, version=version)
                for kind in ("executable", "harness"):
                    shutil.copyfile(old / record[kind]["name"], newer / record[kind]["name"])
                common.write_private_json(newer / acp.RECORD, record)
                detect = acp.detect_antigravity_acp
                def concurrent_activation(host):
                    observed = detect(host)
                    # Reviewer's exact interleaving: T3 wins after AAC's read.
                    common.write_private_json(install.path / "active.json", {"releaseId": newer.name})
                    return observed
                with mock.patch.object(acp, "validate_runtime"), \
                        mock.patch.object(acp, "detect_antigravity_acp", side_effect=concurrent_activation):
                    row = acp.update_antigravity_acp(install)
                self.assertEqual(acp.read_json(install.path / "active.json"), {"releaseId": newer.name})
                self.assertEqual(detect("windows").version, version)
                self.assertEqual((row["status"], row["messageCode"]),
                                 ("current", "antigravity_acp_newer_kept" if version == "1.5.0" else "current"))

    def publish_t3(self, old, version="1.5.0", release_id="d" * 64):
        directory = old.with_name(release_id)
        directory.mkdir(exist_ok=True)
        record = acp.read_json(old / acp.RECORD)
        record.update(releaseId=release_id, version=version)
        for kind in ("executable", "harness"):
            shutil.copyfile(old / record[kind]["name"], directory / record[kind]["name"])
        common.write_private_json(directory / acp.RECORD, record)
        return directory

    def test_newer_activation_between_check_and_rename_converges(self):
        install, old = self.old_acp("windows")
        asset = self.release("windows")
        replace = os.replace
        writes = []
        def race(source, destination):
            if pathlib.Path(destination) == install.path / "active.json":
                writes.append(json.loads(pathlib.Path(source).read_text())["releaseId"])
                if len(writes) == 1:
                    newer = self.publish_t3(old)
                    # T3 publishes the complete directory then activates, immediately
                    # before AAC's actual rename overwrites that activation.
                    pointer = install.path / "t3-pointer.tmp"
                    pointer.write_text(json.dumps({"releaseId": newer.name}))
                    replace(pointer, destination)
            return replace(source, destination)
        with mock.patch.object(acp, "validate_runtime"), mock.patch.object(os, "replace", side_effect=race):
            row = acp.update_antigravity_acp(install)
        self.assertEqual(writes, [asset["sha256"], "d" * 64])
        self.assertEqual(acp.read_json(install.path / "active.json"), {"releaseId": "d" * 64})
        self.assertEqual((row["status"], row["version"], row["messageCode"]),
                         ("updated", "1.5.0", "antigravity_acp_updated"))

    def test_t3_pointer_change_after_aac_write_survives(self):
        install, old = self.old_acp("windows")
        self.release("windows")
        replace = os.replace
        changed = False
        def race(source, destination):
            nonlocal changed
            value = replace(source, destination)
            if pathlib.Path(destination) == install.path / "active.json" and not changed:
                changed = True
                newer = self.publish_t3(old)
                pointer = install.path / "t3-pointer.tmp"
                pointer.write_text(json.dumps({"releaseId": newer.name}))
                replace(pointer, destination)
            return value
        with mock.patch.object(acp, "validate_runtime"), mock.patch.object(os, "replace", side_effect=race):
            row = acp.update_antigravity_acp(install)
        self.assertEqual((row["status"], row["version"]), ("updated", "1.5.0"))
        self.assertEqual(acp.read_json(install.path / "active.json"), {"releaseId": "d" * 64})

    def test_continuing_publications_exhaust_bounded_activation_honestly(self):
        install, old = self.old_acp("windows")
        self.release("windows")
        replace = os.replace
        writes = []
        def race(source, destination):
            if pathlib.Path(destination) == install.path / "active.json":
                writes.append(destination)
                self.publish_t3(old, "2.0." + str(len(writes)), format(len(writes), "064x"))
            return replace(source, destination)
        with mock.patch.object(acp, "validate_runtime"), mock.patch.object(os, "replace", side_effect=race):
            row = acp.update_antigravity_acp(install)
        self.assertEqual(len(writes), 5)
        self.assertEqual((row["status"], row["messageCode"]),
                         ("failed", "antigravity_acp_activation_unsettled"))
        self.assertEqual(row["version"], acp.detect_antigravity_acp("windows").version)

    def test_newest_complete_on_disk_wins_without_t3_activation(self):
        install, old = self.old_acp("windows")
        self.release("windows")
        newer = self.publish_t3(old)
        incomplete = self.publish_t3(old, "9.0.0", "e" * 64)
        (incomplete / "localharness_external.exe").unlink()
        with mock.patch.object(acp, "validate_runtime"):
            row = acp.update_antigravity_acp(install)
        self.assertEqual((row["status"], row["version"]), ("updated", "1.5.0"))
        self.assertEqual(acp.read_json(install.path / "active.json"), {"releaseId": newer.name})

    def test_no_downgrade_or_network_for_current_runtime(self):
        install, old = self.old_acp("windows")
        newer = self.publish_t3(old)
        common.write_private_json(install.path / "active.json", {"releaseId": newer.name})
        install = acp.detect_antigravity_acp("windows")
        pointer = (install.path / "active.json").read_bytes()
        with mock.patch.object(acp, "download") as fetch, mock.patch.object(acp, "write_private_json") as write:
            row = acp.update_antigravity_acp(install)
        self.assertEqual((row["status"], row["version"], row["messageCode"]),
                         ("current", "1.5.0", "antigravity_acp_newer_kept"))
        self.assertEqual((install.path / "active.json").read_bytes(), pointer)
        fetch.assert_not_called()
        write.assert_not_called()


class MacGuiTests(Fixtures):
    def gui(self, failure=None, running=False):
        seen = []
        def run(argv, **kwargs):
            seen.append(argv)
            if argv[1] == "bootstrap":
                plist = plistlib.loads(pathlib.Path(argv[3]).read_bytes())
                self.plist = plist
                self.assertEqual(plist["EnvironmentVariables"]["MUSE_NO_AUTO_UPDATE"], "1")
                self.assertNotIn("HOME", plist["EnvironmentVariables"])
                self.assertNotIn("Keychain", str(plist))
                if failure:
                    pathlib.Path(plist["StandardErrorPath"]).write_text(failure)
            if argv[1] == "print" and "/com.ccs." in argv[2]:
                return "state = running\n" if running else "state = not running\nlast exit code = " + ("1" if failure else "0")
            return ""
        self.stack.enter_context(mock.patch.object(mac, "command", side_effect=run))
        return seen

    def test_muse_warm_is_fixed_gui_launchagent_and_is_removed(self):
        seen = self.gui()
        mac.gui_command("muse-warm", 5)
        self.assertEqual(self.plist["ProgramArguments"], [str(self.root / ".local/bin/muse"), "exec", "--contributor-mode", "off", mac.MUSE_WARM_PROMPT])
        self.assertEqual(seen[1][1:3], ["bootstrap", "gui/501"])
        self.assertEqual(seen[-1][1], "bootout")
        self.assertFalse(pathlib.Path(self.plist["WorkingDirectory"]).exists())

    def test_locked_keychain_is_distinguished_from_catalog_failure_and_cleanup_runs(self):
        seen = self.gui("Muse Code couldn't access your Meta credentials because macOS Keychain is locked or requires approval")
        with self.assertRaises(common.UpdateFailure) as caught:
            mac.gui_command("muse-warm", 5)
        self.assertEqual(caught.exception.code, "mac_keychain_locked")
        self.assertEqual(seen[-1][1], "bootout")

    def test_no_gui_session_is_actionable_and_does_not_bootstrap(self):
        with mock.patch.object(mac, "command", side_effect=common.UpdateFailure()) as run:
            with self.assertRaises(common.UpdateFailure) as caught:
                mac.gui_command("muse-warm", 5)
        self.assertEqual(caught.exception.code, "mac_gui_unavailable")
        self.assertEqual(run.call_count, 1)

    def test_session_loss_after_bootstrap_is_distinguished_from_service_failure(self):
        for lost in ("missing", "service", "permission"):
            seen = []
            def run(argv, **kwargs):
                seen.append(argv)
                if argv[1] == "print" and "/com.ccs." in argv[2]:
                    raise common.UpdateFailure()
                if argv[1] == "print" and len(seen) > 2 and lost != "service":
                    raise common.UpdateFailure(output="Could not find domain for" if lost == "missing" else "Permission denied")
                return ""
            with mock.patch.object(mac, "command", side_effect=run):
                with self.assertRaises(common.UpdateFailure) as caught:
                    mac.gui_command("muse-warm", 5)
            self.assertEqual(caught.exception.code, "mac_gui_unavailable" if lost == "missing" else "update_failed")
            self.assertEqual(seen[1][1], "bootstrap")
            self.assertTrue(any(argv[1] == "bootout" for argv in seen))

    def test_no_arbitrary_operation_can_be_launched(self):
        with mock.patch.object(mac, "command") as run:
            with self.assertRaises(common.UpdateFailure):
                mac.gui_command("caller-command", 5)
        run.assert_not_called()

    def test_timeout_boots_out_worker_without_changing_keychain(self):
        seen = self.gui(running=True)
        with mock.patch.object(mac.time, "monotonic", side_effect=[0, 0, 0, 0, 2, 2]):
            with self.assertRaises(common.UpdateFailure):
                mac.gui_command("muse-warm", 1)
        self.assertEqual(seen[-1][1], "bootout")

    def test_updater_warm_uses_gui_on_mac_and_preserves_specific_keychain_error(self):
        install = common.Install("muse-code", "mac", self.root / ".local/bin/muse", "1.4.15")
        install.path.parent.mkdir(parents=True); install.path.touch()
        with mock.patch.object(updater, "gui_command") as gui, \
                mock.patch.object(updater, "command") as direct, mock.patch.object(updater, "muse_catalog_fresh", return_value=True):
            self.assertTrue(updater.muse_warm_catalog(install, time.monotonic() + 60, time.time()))
        gui.assert_called_once(); direct.assert_not_called()
        with mock.patch.object(updater, "gui_command", side_effect=common.UpdateFailure("mac_keychain_locked")):
            with self.assertRaises(common.UpdateFailure) as caught:
                updater.muse_warm_catalog(install, time.monotonic() + 60, time.time())
        self.assertEqual(caught.exception.code, "mac_keychain_locked")

    def test_muse_result_preserves_keychain_error_after_changed_and_unchanged_installs(self):
        install = common.Install("muse-code", "mac", self.root / ".local/bin/muse", "1.4.14")
        install.path.parent.mkdir(parents=True); install.path.touch()
        for code, version, status in ((code, version, status)
                for code in ("mac_keychain_locked", "mac_gui_unavailable")
                for version, status in (("1.4.15", "restart_failed"), ("1.4.14", "failed"))):
            refreshed = common.Install("muse-code", "mac", install.path, version)
            with self.subTest(version=version), mock.patch.object(updater, "scan", return_value=[]), \
                    mock.patch.object(updater, "cli_contexts", return_value=([], [])), mock.patch.object(updater, "perform_cli_update"), \
                    mock.patch.object(updater, "detect_cli", return_value=refreshed), mock.patch.object(updater, "muse_update_busy", return_value=False), \
                    mock.patch.object(updater, "muse_catalog_fresh", return_value=False), \
                    mock.patch.object(updater, "gui_command", side_effect=common.UpdateFailure(code)), \
                    mock.patch.object(updater, "restart_cli", return_value=[]):
                row = updater.update_cli(install, time.monotonic() + 60)
            self.assertEqual((row["status"], row["messageCode"], row["version"]), (status, code, version))


if __name__ == "__main__":
    unittest.main()
