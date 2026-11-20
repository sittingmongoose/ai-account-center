"""Offline fixtures for the account-isolation check Update all runs after an Antigravity ACP install.

The kit's real qualification command never runs here: a fake launcher in the test
home answers with the exit code and the two lines (QUALIFY:, WRAPPER-RUNS:) the
kit documents, and records its argv so a test can tell it ran (and for which
release) or that it did not run at all.
"""

import json
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401  (discovery stays inside each test's home)
import app_update_cursor_acp_macos_test as _acp_fixture  # the offline fake T3
import app_update_antigravity as acp

RELEASE = "b" * 64
NEW, OLD = "1.4.6", "1.4.5"


def pass_line(version=NEW):
    return ("QUALIFY: PASS %s.. %s: each profile used only its own sign-in file (4.0 s); recorded\n"
            "WRAPPER-RUNS: %s.. %s\n" % (RELEASE[:8], version, RELEASE[:8], version))


def fail_line(version=NEW, wrapper=OLD):
    return ("problem: the second profile's session named the first profile's sign-in file\n"
            "QUALIFY: FAIL %s.. %s: the release did not keep each profile on its own sign-in file; "
            "nothing recorded (4.0 s)\n"
            "WRAPPER-RUNS: %s.. %s\n" % (RELEASE[:8], version, RELEASE[:8], wrapper))


def cannot_line(version=NEW, wrapper=OLD, reason="bwrap is missing"):
    return ("QUALIFY: CANNOT TEST %s.. %s: %s\n"
            "WRAPPER-RUNS: %s.. %s\n" % (RELEASE[:8], version, reason, RELEASE[:8], wrapper))


CANNOT_NAMELESS = ("QUALIFY: CANNOT TEST: no readable Antigravity release (T3's active.json)\n"
                   "WRAPPER-RUNS: none -\n")


class Fixture(unittest.TestCase):
    """The existing offline T3 fixture, without its own test methods."""

    TOKEN = _acp_fixture.AcpTests.TOKEN
    setUp = _acp_fixture.AcpTests.setUp
    install_record = _acp_fixture.AcpTests.install_record
    runtime = _acp_fixture.AcpTests.runtime
    fake_t3 = _acp_fixture.AcpTests.fake_t3
    run_update = _acp_fixture.AcpTests.run_update
    isolated = _acp_fixture.AcpTests.isolated

    def kit(self, exit_code, output, sleep=0):
        """A fake kit command: it records its argv, then answers as the real one would."""
        kit = self.root / ".local/share/t3-antigravity-isolated"
        kit.mkdir(parents=True, exist_ok=True)
        launcher = self.root / ".local/bin/t3-antigravity-check"
        launcher.parent.mkdir(parents=True, exist_ok=True)
        launcher.write_text(
            "#!" + sys.executable + "\n"
            "import json, pathlib, sys, time\n"
            "home = pathlib.Path(__file__).resolve().parents[2]\n"
            "spec = json.loads((home / 'qualify-spec.json').read_text())\n"
            "(home / 'qualify-argv.json').write_text(json.dumps(sys.argv[1:]))\n"
            "time.sleep(spec.get('sleep', 0))\n"
            "sys.stdout.write(spec.get('output', ''))\n"
            "sys.stdout.flush()\n"
            "raise SystemExit(spec.get('exit', 0))\n"
        )
        launcher.chmod(0o755)
        (self.root / "qualify-spec.json").write_text(
            json.dumps({"exit": exit_code, "output": output, "sleep": sleep}))
        return launcher

    def argv(self):
        path = self.root / "qualify-argv.json"
        return json.loads(path.read_text()) if path.exists() else None


class QualifyHookTests(Fixture):
    def test_a_new_release_runs_the_kit_and_verifies_isolation_on_exit_0(self):
        install, _ = self.runtime("ubuntu", OLD)
        self.fake_t3(install, offer=NEW, newest=NEW)
        self.kit(0, pass_line())
        row = self.run_update(install)
        self.assertEqual(
            (row["status"], row["messageCode"], row["previousVersion"], row["version"]),
            ("updated", "antigravity_acp_qualified", OLD, NEW))
        # The release T3 just activated is the one the check tests.
        self.assertEqual(self.argv(), ["--qualify", RELEASE])
        self.assertNotIn("wrapperVersion", row)
        self.assertNotIn("qualifyReason", row)

    def test_exit_1_keeps_the_row_updated_and_names_the_release_the_wrapper_runs(self):
        install, _ = self.runtime("ubuntu", OLD)
        self.fake_t3(install, offer=NEW, newest=NEW)
        self.kit(1, fail_line())
        row = self.run_update(install)
        self.assertEqual(
            (row["status"], row["messageCode"], row["previousVersion"], row["version"]),
            ("updated", "antigravity_acp_isolation_unverified", OLD, NEW))
        self.assertEqual(row["wrapperVersion"], OLD)
        self.assertNotIn("qualifyReason", row)

    def test_exit_2_adds_the_checks_reason_and_keeps_the_row_updated(self):
        install, _ = self.runtime("ubuntu", OLD)
        self.fake_t3(install, offer=NEW, newest=NEW)
        self.kit(2, cannot_line())
        row = self.run_update(install)
        self.assertEqual(
            (row["status"], row["messageCode"], row["version"]),
            ("updated", "antigravity_acp_isolation_unverified", NEW))
        self.assertEqual(row["wrapperVersion"], OLD)
        self.assertEqual(row["qualifyReason"], "bwrap is missing")

    def test_a_nameless_cannot_test_says_the_wrapper_runs_none(self):
        install, _ = self.runtime("ubuntu", OLD)
        self.fake_t3(install, offer=NEW, newest=NEW)
        self.kit(2, CANNOT_NAMELESS)
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"]),
                         ("updated", "antigravity_acp_isolation_unverified"))
        self.assertIn("wrapperVersion", row)
        self.assertIsNone(row["wrapperVersion"])
        self.assertEqual(row["qualifyReason"], "no readable Antigravity release (T3's active.json)")

    def test_a_timeout_counts_as_exit_2(self):
        install, _ = self.runtime("ubuntu", OLD)
        self.fake_t3(install, offer=NEW, newest=NEW)
        self.kit(0, pass_line(), sleep=5)
        with mock.patch.object(acp, "QUALIFY_SECONDS", 1):
            row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"]),
                         ("updated", "antigravity_acp_isolation_unverified"))
        # Killed before its WRAPPER-RUNS line: the row names no release rather than inventing one.
        self.assertNotIn("wrapperVersion", row)
        self.assertEqual(row["qualifyReason"], "the check did not finish in time")

    def test_without_the_kit_a_new_release_keeps_todays_row_and_runs_nothing(self):
        install, _ = self.runtime("ubuntu", OLD)
        self.fake_t3(install, offer=NEW, newest=NEW)
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"]), ("updated", "antigravity_acp_updated"))
        self.assertNotIn("wrapperVersion", row)
        self.assertNotIn("qualifyReason", row)
        self.assertIsNone(self.argv(), "no kit, no qualification run")
        # Positive control: the same install with the kit present does run it.
        with self.isolated():
            found = acp.detect_antigravity_acp("ubuntu")
            self.fake_t3(found, offer="1.4.7", installed="1.4.7", newest="1.4.7")
            self.kit(0, pass_line("1.4.7"))
            row = self.run_update(found)
        self.assertEqual(row["messageCode"], "antigravity_acp_qualified")
        self.assertEqual(self.argv(), ["--qualify", RELEASE])

    def test_an_unchanged_release_runs_nothing(self):
        install, _ = self.runtime("ubuntu", NEW)
        self.fake_t3(install, offer=NEW, newest=NEW)
        self.kit(0, pass_line())
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"]), ("current", "current"))
        self.assertIsNone(self.argv(), "nothing was installed, so nothing is qualified")
        # Positive control: once there is a new release, the check runs.
        with self.isolated():
            found = acp.detect_antigravity_acp("ubuntu")
            self.fake_t3(found, offer="1.4.7", installed="1.4.7", newest="1.4.7")
            self.kit(0, pass_line("1.4.7"))
            row = self.run_update(found)
        self.assertEqual((row["status"], row["messageCode"]), ("updated", "antigravity_acp_qualified"))
        self.assertEqual(self.argv(), ["--qualify", RELEASE])


if __name__ == "__main__":
    unittest.main()
