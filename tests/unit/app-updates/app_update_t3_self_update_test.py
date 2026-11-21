"""A running T3 desktop updates through T3's own updater. Offline fixtures; no T3, network or process touched."""

import contextlib
import io
import json
import os
import pathlib
import plistlib
import struct
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401  (discovery stays inside each test's home)
import app_update_common as common
import app_update_t3 as t3
import app_update_t3_rpc as t3_rpc
import app_updates as updater

OLD = "0.1.12-nightly.20261120.4994"
NEW = "0.1.12-nightly.20261120.5009"


def bundle(path, version):
    (path / "Contents").mkdir(parents=True, exist_ok=True)
    with (path / "Contents/Info.plist").open("wb") as stream:
        plistlib.dump({"CFBundleIdentifier": t3.MAC_ID, "CFBundleShortVersionString": version}, stream)


def asar(executable, version):
    executable.parent.mkdir(parents=True, exist_ok=True)
    executable.touch()
    package = json.dumps({"name": "t3code", "version": version}).encode()
    header = json.dumps({"files": {"package.json": {"size": len(package), "offset": "0"}}}).encode()
    padded = header + b"\0" * ((4 - len(header) % 4) % 4)
    resources = executable.parent / "resources"
    resources.mkdir(exist_ok=True)
    (resources / "app.asar").write_bytes(struct.pack("<4I", 4, 8 + len(padded), 4 + len(padded), len(header)) + padded + package)


def descriptor(version, managed="desktop-managed", desktop=True):
    capabilities = {"serverSelfUpdate": managed, "serverSelfUpdateProgress": True}
    if desktop:
        capabilities["desktopAppUpdate"] = True
    return {"serverVersion": version, "capabilities": capabilities}


class FakeT3:
    """T3's server as its own client sees it: prepare, then a commit that stops the server."""

    def __init__(self, install, reply=None, commit=None):
        self.install, self.calls = install, []
        self.reply = reply if reply is not None else {"targetVersion": NEW, "method": "desktop-app", "desktopUpdateToken": "request-1"}
        self.commit = commit or "handoff"

    def rpc(self, origin, token, deadline):
        fake = self

        class Client:
            def call(self, method, payload):
                fake.calls.append((method, payload))
                if method == "server.updateServer":
                    return fake.reply
                if fake.commit == "refused":
                    raise common.UpdateFailure("t3_install_failed:rpc")
                if fake.commit == "answered":
                    return {"targetVersion": NEW, "method": "desktop-app"}
                # T3 installed itself and relaunched: the socket drops.
                if self_install := fake.install:
                    (bundle if self_install.platform == "mac" else lambda path, version: asar(path, version))(self_install.path, NEW)
                raise common.UpdateFailure("t3_unreachable")

            def close(self):
                pass

        return Client()


class RunningT3SelfUpdate(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(pathlib.Path, "home", return_value=self.root).start()
        mock.patch.dict(os.environ, {"CCS_HOME": str(self.root / "state"), "LOCALAPPDATA": str(self.root / "local")}).start()
        mock.patch.object(t3, "latest_release", return_value=NEW).start()
        self.scopes = []

        @contextlib.contextmanager
        def bearer(cli, base, deadline, scopes=("providers:manage", "orchestration:read"), prefix="aac-acp-"):
            self.scopes.append(tuple(scopes))
            yield "token"

        mock.patch.object(t3_rpc, "bearer", side_effect=bearer).start()
        mock.patch.object(t3_rpc, "discover", return_value=(["t3"], self.root / ".t3", mock.Mock(hostname="127.0.0.1", port=3773))).start()
        # The swap path is what refused the running Mac T3 on 2026-11-20 (restart_context).
        self.swap_mac = mock.patch.object(t3, "update_mac", side_effect=common.UpdateFailure("restart_context")).start()
        self.swap_windows = mock.patch.object(t3, "update_windows", side_effect=common.UpdateFailure("restart_failed")).start()
        self.stop = mock.patch.object(t3, "terminate_desktops").start()
        self.relaunch = mock.patch.object(t3, "restart_desktops").start()

    def mac(self):
        path = self.root / "Applications" / t3.MAC_NAME
        bundle(path, OLD)
        return t3.T3Install("t3-code", "mac", path, OLD, "official-download", t3.MAC_ID, t3.MAC_TEAM, path, desktop=True)

    def windows(self):
        path = self.root / "local/Programs/t3code" / t3.WINDOWS_NAME
        asar(path, OLD)
        return t3.T3Install("t3-code", "windows", path, OLD, "official-download", publisher=t3.WINDOWS_PUBLISHER,
                            package_root=path.parent, desktop=True)

    def run_update(self, install, fake, offered=None, ready=None, native_worker=False):
        mock.patch.object(t3_rpc, "environment_descriptor", return_value=offered or descriptor(OLD)).start()
        mock.patch.object(t3_rpc, "Rpc", side_effect=fake.rpc).start()
        wait = mock.patch.object(t3_rpc, "wait_ready", side_effect=ready).start()
        if native_worker:
            with mock.patch.object(updater, "detect_one", return_value=install):
                payload = updater.apply_single_app("t3-code", install.platform, lambda _event: None, 600)
            return payload["results"][0], wait
        return t3.update_t3(install, time.monotonic() + 600), wait

    def assert_self_updated(self, install, row, fake, wait):
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("updated", "t3_updated", NEW))
        self.assertEqual(row["restartTargets"], [{"kind": "desktop"}])
        self.assertEqual([call[0] for call in fake.calls], ["server.updateServer", "server.commitDesktopUpdate"])
        self.assertEqual(fake.calls[0][1], {"targetVersion": NEW})
        self.assertEqual(fake.calls[1][1], {"requestId": "request-1"})
        self.assertEqual(self.scopes, [("environment:maintain",)])
        # The row is written only after the relaunched server answered on the new build.
        self.assertEqual(wait.call_args.args[2], NEW)
        # T3 quits, installs and relaunches itself: this helper never stops, swaps or reopens it.
        self.swap_mac.assert_not_called(); self.swap_windows.assert_not_called()
        self.stop.assert_not_called(); self.relaunch.assert_not_called()

    def test_running_mac_t3_takes_t3s_own_updater_instead_of_restart_context(self):
        install = self.mac()
        fake = FakeT3(install)
        row, wait = self.run_update(install, fake, native_worker=True)
        self.assert_self_updated(install, row, fake, wait)
        self.assertEqual(t3.mac_bundle_version(install.path), NEW)

    def test_running_windows_t3_takes_t3s_own_updater(self):
        install = self.windows()
        fake = FakeT3(install)
        row, wait = self.run_update(install, fake, native_worker=True)
        self.assert_self_updated(install, row, fake, wait)
        self.assertEqual(t3.windows_bundle_version(install.path), NEW)

    def test_t3_refusing_the_install_changes_nothing_and_never_swaps(self):
        install = self.mac()
        row, wait = self.run_update(install, FakeT3(install, commit="refused"))
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("failed", "update_failed", OLD))
        wait.assert_not_called(); self.swap_mac.assert_not_called(); self.stop.assert_not_called()

    def test_a_commit_that_answers_instead_of_stopping_is_not_success(self):
        install = self.windows()
        row, _wait = self.run_update(install, FakeT3(install, commit="answered"))
        self.assertEqual((row["status"], row["version"]), ("failed", OLD))
        self.swap_windows.assert_not_called()

    def test_a_reply_for_another_method_or_an_older_build_is_never_committed(self):
        for reply in ({"targetVersion": NEW, "method": "boot-service", "updateId": "x"},
                      {"targetVersion": OLD, "method": "desktop-app", "desktopUpdateToken": "request-1"},
                      {"targetVersion": "latest", "method": "desktop-app", "desktopUpdateToken": "request-1"}):
            with self.subTest(reply=reply):
                install = self.mac()
                fake = FakeT3(install, reply=reply)
                row, _wait = self.run_update(install, fake)
                self.assertEqual((row["status"], row["version"]), ("failed", OLD))
                self.assertEqual([call[0] for call in fake.calls], ["server.updateServer"])
                mock.patch.stopall()
                self.setUp()

    def test_t3_that_does_not_come_back_on_the_new_build_is_restart_failed(self):
        install = self.mac()
        row, _wait = self.run_update(install, FakeT3(install),
                                     ready=common.UpdateFailure("t3_not_ready", cause="t3_not_ready", reason="previous_server"))
        self.assertEqual(row["status"], "restart_failed")
        self.assertEqual(row["messageCode"], "restart_failed")
        self.assertEqual(row["version"], NEW)  # What is installed now, not the old snapshot.

    def test_t3_that_does_not_come_back_is_restart_failed_even_when_the_revoke_fails(self):
        # Review of e7ba47269 (Medium 1): wait_ready fails inside bearer, bearer's finally cannot revoke
        # through the same broken new build, and t3_session_revoke_failed replaced restart_failed.
        @contextlib.contextmanager
        def revoke_fails(cli, base, deadline, scopes=("providers:manage", "orchestration:read"), prefix="aac-acp-"):
            try:
                yield "token"
            finally:
                raise common.UpdateFailure("t3_session_revoke_failed")

        mock.patch.object(t3_rpc, "bearer", side_effect=revoke_fails).start()
        install = self.mac()
        row, _wait = self.run_update(install, FakeT3(install),
                                     ready=common.UpdateFailure("t3_not_ready", cause="t3_not_ready", reason="previous_server"))
        self.assertEqual(row["status"], "restart_failed")
        self.assertEqual(row["messageCode"], "restart_failed")

    def test_t3_installing_a_newer_build_than_the_lookup_reports_that_build(self):
        newer = "0.1.12-nightly.20261120.5010"
        install = self.windows()
        fake = FakeT3(install, reply={"targetVersion": newer, "method": "desktop-app", "desktopUpdateToken": "request-1"})
        original = fake.rpc

        def rpc(*args):
            client = original(*args)
            call = client.call

            def wrapped(method, payload):
                if method == "server.commitDesktopUpdate":
                    asar(install.path, newer)
                    raise common.UpdateFailure("t3_unreachable")
                return call(method, payload)
            client.call = wrapped
            return client
        fake.rpc = rpc
        row, wait = self.run_update(install, fake)
        self.assertEqual((row["status"], row["version"]), ("updated", newer))
        self.assertEqual(wait.call_args.args[2], newer)

    def test_a_t3_that_is_not_running_or_not_desktop_managed_keeps_the_verified_swap(self):
        for offered in (None, descriptor(OLD, managed="boot-service", desktop=False), descriptor(NEW),
                        descriptor(OLD, desktop=False)):
            with self.subTest(offered=offered):
                install = self.mac()
                mock.patch.object(t3_rpc, "environment_descriptor", return_value=offered).start()
                rpc = mock.patch.object(t3_rpc, "Rpc").start()
                with mock.patch.object(t3, "update_mac", return_value=(0, 0)) as swap:
                    row = t3.update_t3(install, time.monotonic() + 600)
                swap.assert_called_once()
                rpc.assert_not_called()
                self.assertEqual(row["status"], "updated")
                mock.patch.stopall()
                self.setUp()


class DescriptorAndScopes(unittest.TestCase):
    def test_offer_needs_desktop_management_the_update_capability_and_the_installed_version(self):
        self.assertTrue(t3_rpc.desktop_updater_offered(OLD, descriptor(OLD)))
        self.assertFalse(t3_rpc.desktop_updater_offered(OLD, descriptor(OLD, managed="boot-service")))
        self.assertFalse(t3_rpc.desktop_updater_offered(OLD, descriptor(OLD, desktop=False)))
        self.assertFalse(t3_rpc.desktop_updater_offered(OLD, descriptor(NEW)))
        self.assertFalse(t3_rpc.desktop_updater_offered(None, descriptor(OLD)))
        self.assertFalse(t3_rpc.desktop_updater_offered(OLD, {"serverVersion": OLD, "capabilities": "x"}))

    def test_descriptor_is_read_only_from_the_published_loopback_origin(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)):
            self.assertIsNone(t3_rpc.environment_descriptor())
            runtime = pathlib.Path(folder) / ".t3/userdata/server-runtime.json"
            runtime.parent.mkdir(parents=True)
            runtime.write_text(json.dumps({"version": 1, "origin": "http://example.com:3773", "port": 3773}))
            with mock.patch.object(t3_rpc.urllib.request, "urlopen") as get:
                self.assertIsNone(t3_rpc.environment_descriptor())
            get.assert_not_called()
            runtime.write_text(json.dumps({"version": 1, "origin": "http://127.0.0.1:3773", "port": 3773}))
            response = mock.MagicMock()
            response.__enter__.return_value.read.return_value = json.dumps(descriptor(OLD)).encode()
            with mock.patch.object(t3_rpc.urllib.request, "urlopen", return_value=response) as get:
                self.assertEqual(t3_rpc.environment_descriptor(), descriptor(OLD))
            self.assertEqual(get.call_args.args[0], "http://127.0.0.1:3773/.well-known/t3/environment")

    def test_issued_session_carries_exactly_the_requested_scopes(self):
        with mock.patch.object(t3_rpc, "auth_command", return_value=json.dumps({"sessionId": "s", "token": "t"})) as issue, \
                mock.patch.object(t3_rpc, "retire_session"):
            with t3_rpc.bearer(["t3"], pathlib.Path("/base"), time.monotonic() + 60, scopes=("environment:maintain",), prefix="aac-t3-update-") as token:
                self.assertEqual(token, "t")
        argv = issue.call_args.args[2]
        self.assertEqual([argv[i + 1] for i, word in enumerate(argv) if word == "--scope"], ["environment:maintain"])
        self.assertTrue(argv[argv.index("--label") + 1].startswith("aac-t3-update-"))
        with mock.patch.object(t3_rpc, "auth_command", return_value=json.dumps({"sessionId": "s", "token": "t"})) as issue, \
                mock.patch.object(t3_rpc, "retire_session"):
            with t3_rpc.bearer(["t3"], pathlib.Path("/base"), time.monotonic() + 60):
                pass
        argv = issue.call_args.args[2]
        self.assertEqual([argv[i + 1] for i, word in enumerate(argv) if word == "--scope"], ["providers:manage", "orchestration:read"])


if __name__ == "__main__":
    unittest.main()
