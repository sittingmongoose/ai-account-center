"""Lane D follow-ups for the update-all robustness rounds (RECHECK-1 L2/L6, RECHECK-2 R2-1/2/3/6).

Each test fails at origin/main on the defect it is named after and passes at this branch's
head. Fakes only: nothing is installed, updated or restarted, and no network is reached.
"""
import json
import os
import pathlib
import sys
import time
import unittest
from unittest import mock

sys.path[:0] = [str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"),
                str(pathlib.Path(__file__).resolve().parent)]
import hermetic  # noqa: F401
import app_update_common as common
import app_update_cursor as cursor
import app_update_macos as mac
import app_update_zcode as zcode
from app_update_robustness_test import Home, attach, scope, write_package


class LaneD(Home):
    def consumers(self, *paths):
        settings = self.root / ".t3/userdata/settings.json"
        settings.parent.mkdir(parents=True, exist_ok=True)
        settings.write_text(json.dumps({"providerInstances": {
            "zcode-%d" % index: {"enabled": True, "config": {"commandPath": "node", "commandArgs": [str(path), "server"]}}
            for index, path in enumerate(paths)}}))

    def npm_prefix(self, prefix, version):
        root = self.root / prefix / "lib/node_modules"
        write_package(root, "zcode-acp-server", version)
        (root / "zcode-acp-server/dist").mkdir(parents=True, exist_ok=True)
        (root / "zcode-acp-server/dist/cli.js").write_text("cli")
        return root

    def npm_writes(self, runs=None):
        """A fake npm that installs where it is told (--prefix), at the version it is told."""
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            argv = [str(a) for a in argv]
            if runs is not None:
                runs.append(argv)
            prefix = pathlib.Path(argv[argv.index("--prefix") + 1])
            package, requested = argv[-1].rsplit("@", 1)
            write_package(prefix / "lib/node_modules", package, "0.84.3" if requested == "latest" else requested)
            return 0, b""
        return run

    # ---------------------------------------------------------------------------------------------
    # RECHECK-1 L2: URL credentials on non-http schemes, and a bare pwd= key, survive both scrubbers.
    # ---------------------------------------------------------------------------------------------
    def test_r1_l2_scheme_url_credentials_and_pwd_keys_never_survive(self):
        cases = {
            "psql: postgres://dbuser:dbpass99@dbhost/db": "dbpass99",
            "clone: ssh://gituser:gitpass99@githost/org/repo.git": "gitpass99",
            "redis: redis://svc:running-pw-77@cache.internal:6379/2": "running-pw-77",
            "connect pwd=hunter2pwd ok": "hunter2pwd",
        }
        for text, secret in cases.items():
            with self.subTest(text=text):
                self.assertNotIn(secret, common.scrub(text))
        # The http form keeps its own words, and ordinary URLs and paths stay readable.
        self.assertEqual(common.scrub("db is at postgres://dbhost/db after the move"), "db is at postgres://dbhost/db after the move")
        self.assertEqual(common.scrub("fetch https://registry.npmjs.org/zcode-acp-server"), "fetch https://registry.npmjs.org/zcode-acp-server")

    # ---------------------------------------------------------------------------------------------
    # RECHECK-1 L6: the first owner's failure skipped the adapter's other copies.
    # ---------------------------------------------------------------------------------------------
    def test_r1_l6_one_unwritable_copy_does_not_skip_its_sibling(self):
        first = self.npm_prefix("opt/homebrew", "0.84.2")
        second = self.npm_prefix("second-node", "0.84.2")
        self.consumers(first / "zcode-acp-server/dist/cli.js", second / "zcode-acp-server/dist/cli.js")
        node, cli = self.root / "node", self.root / "npm-cli.js"
        runs = []
        with mock.patch.object(zcode, "registry_version", side_effect={"zcode-acp-server": "0.84.3"}.get), \
                mock.patch.object(zcode, "resolve_npm", return_value=(node, cli)), \
                mock.patch.object(zcode, "writable", side_effect=lambda owner, host: _real_prefix(owner) != first.parents[1].resolve()), \
                mock.patch.object(common, "_run_bounded", side_effect=self.npm_writes(runs=runs)), \
                mock.patch.object(zcode, "scan", return_value=[]), scope() as record:
            install = zcode.detect_adapters("mac")
            row = attach(zcode.update_adapters(install, time.monotonic() + 600), record)
        self.assertEqual(zcode.package_version(second, "zcode-acp-server"), "0.84.3",
                         "the writable copy was never attempted")
        self.assertEqual(len(runs), 1)
        self.assertIn("second-node", runs[0][runs[0].index("--prefix") + 1])
        # The row keeps the honest worst: the unwritable copy still needs its user.
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "prefix_not_writable"))
        self.assertEqual(row["parts"][1]["version"], "0.84.2", "the lowest across copies stays")

    def test_r1_l6_a_real_failure_outranks_a_not_writable_sibling(self):
        first = self.npm_prefix("opt/homebrew", "0.84.2")
        second = self.npm_prefix("second-node", "0.84.2")
        self.consumers(first / "zcode-acp-server/dist/cli.js", second / "zcode-acp-server/dist/cli.js")
        node, cli = self.root / "node", self.root / "npm-cli.js"
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            return 1, b""
        with mock.patch.object(zcode, "registry_version", side_effect={"zcode-acp-server": "0.84.3"}.get), \
                mock.patch.object(zcode, "resolve_npm", return_value=(node, cli)), \
                mock.patch.object(zcode, "writable", side_effect=lambda owner, host: _real_prefix(owner) != second.parents[1].resolve()), \
                mock.patch.object(common, "_run_bounded", side_effect=run), \
                mock.patch.object(zcode, "scan", return_value=[]), scope() as record:
            install = zcode.detect_adapters("mac")
            row = attach(zcode.update_adapters(install, time.monotonic() + 600), record)
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))

    # ---------------------------------------------------------------------------------------------
    # RECHECK-2 R2-1: a TLS warning anywhere hid the 401 named on the "Update failed:" line.
    # ---------------------------------------------------------------------------------------------
    def test_r2_1_a_tls_warning_never_hides_the_401_on_the_failure_line(self):
        output = ("Warning: Setting the NODE_TLS_REJECT_UNAUTHORIZED environment variable to '0' makes TLS "
                  "connections and HTTPS requests insecure. Please think twice.\n"
                  "Error: Update failed: [unauthenticated] Error: Request failed with status code 401")
        with scope():
            failure = cursor.classify(common.UpdateFailure(output=output), 0)
        self.assertEqual((failure.code, failure.cause, failure.reason), ("cursor_signin_needed", "auth_required", "http_401"))

    def test_r2_1_a_real_tls_failure_on_the_failure_line_stays_undetermined(self):
        output = ("Warning: NODE_TLS_REJECT_UNAUTHORIZED is set to '0'\n"
                  "Error: Update failed: unable to verify the first certificate")
        with scope() as record:
            failure = cursor.classify(common.UpdateFailure(output=output), 0)
        self.assertEqual(failure.code, "update_failed")
        self.assertIn("signin_undetermined", record.notes)

    # ---------------------------------------------------------------------------------------------
    # RECHECK-2 R2-2: the success check compared Cursor versions as text.
    # ---------------------------------------------------------------------------------------------
    def test_r2_2_cursor_compares_versions_numerically(self):
        install = common.Install("cursor-cli", "ubuntu", self.root / "cursor-agent", "2026.9.30-abcdef1")
        refreshed = common.Install("cursor-cli", "ubuntu", install.path, "2026.10.01-abcdef2")
        with mock.patch.object(cursor, "run_update"), mock.patch.object(cursor, "detect_cursor", return_value=refreshed), scope():
            row = cursor.update_cursor(install, time.monotonic() + 10)
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"]),
                         ("updated", "cursor_updated", "2026.9.30-abcdef1", "2026.10.01-abcdef2"))

    # ---------------------------------------------------------------------------------------------
    # RECHECK-2 R2-3: a down registry read as "the shared updater could lower another package".
    # ---------------------------------------------------------------------------------------------
    def test_r2_3_a_down_registry_is_named_not_blamed_on_the_shared_updater(self):
        modules = self.root / ".local/lib/node_modules"
        write_package(modules, "zcode-acp-server", "0.84.2")
        install = common.Install("zcode", "ubuntu", self.root / "app", "3.14.4")
        with mock.patch.object(zcode, "latest_zcode", return_value="3.14.5"), \
                mock.patch.object(zcode, "acp_unit_present", return_value=True), \
                mock.patch.object(zcode, "scan", return_value=[]), \
                mock.patch.object(zcode, "registry_version", return_value=None), \
                mock.patch.object(zcode, "run_unit") as unit, scope() as record:
            row = attach(zcode.update_zcode(install, time.monotonic() + 10), record)
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("failed", "registry_unreadable", False))
        self.assertEqual(row["diagnostics"]["reason"], "registry_unreadable")
        unit.assert_not_called()

    # ---------------------------------------------------------------------------------------------
    # RECHECK-2 R2-6: output past the 64 KiB diagnostic limit was dropped whole.
    # ---------------------------------------------------------------------------------------------
    def test_r2_6_stderr_past_the_limit_keeps_its_tail_for_classification(self):
        filler = ("x" * 60 + "\n") * 1200  # about 73 KiB
        sample = filler + "npm error EPERM: operation not permitted\n"
        argv = [sys.executable, "-c", "import sys; sys.stderr.write(sys.argv[1]); sys.exit(1)", sample]
        with scope():
            with self.assertRaises(common.UpdateFailure) as caught:
                common.command(argv, timeout=60)
        self.assertNotIn("<output omitted", caught.exception.output)
        self.assertIn("EPERM", caught.exception.output)
        self.assertLess(len(caught.exception.output), 70000)

    def test_r2_6_the_mac_tail_keeps_the_last_bytes_from_a_whole_line(self):
        path = self.root / "agent-output.log"
        filler = ("y" * 60 + "\n") * 1200
        path.write_text(filler + "Update failed: [unauthenticated] Error\n")
        text = mac._tail(path)
        self.assertTrue(text.endswith("Update failed: [unauthenticated] Error\n"), text[-80:])
        self.assertLess(len(text), 70000)


def _real_prefix(owner):
    return pathlib.Path(owner.prefix).resolve()


if __name__ == "__main__":
    unittest.main()
