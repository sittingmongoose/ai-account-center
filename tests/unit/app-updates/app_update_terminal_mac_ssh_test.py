"""The Mac Terminal relaunch never hands the SSH session to the reopened CLI (2026-11-19).

Runs the real broker and the real Terminal child on this computer: LaunchServices is replaced by
running the brokered `.command` script directly, and the CLI by a fixture that records which
SSH variables it received. No real CLI, Terminal or network is used.
"""
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401  (discovery stays inside each test's home)
import app_update_common as common
import app_update_processes as processes
import app_update_terminal as terminal

SSH = {"SSH_CONNECTION": "192.0.2.1 50000 192.0.2.2 22", "SSH_CLIENT": "192.0.2.1 50000 22", "SSH_TTY": "/dev/ttys009",
       "SSH_AUTH_SOCK": "/private/tmp/ssh-fixture/agent.1", "SSH_ORIGINAL_COMMAND": "fixture"}


@unittest.skipIf(os.name == "nt" or not pathlib.Path("/usr/bin/python3").exists(), "POSIX broker with /usr/bin/python3")
class MacTerminalBrokerTests(unittest.TestCase):
    def test_the_reopened_cli_receives_no_ssh_variables(self):
        with tempfile.TemporaryDirectory() as directory:
            home = pathlib.Path(directory)
            report = home / "seen.txt"
            target = home / "fixture-cli"
            target.write_text("#!/usr/bin/python3\nimport os, pathlib\npathlib.Path(%r).write_text(' '.join(sorted(k for k in os.environ if k.startswith('SSH_') or k == 'KEEP')))\n" % str(report))
            target.chmod(0o755)
            install = common.Install("antigravity-cli", "mac", target)
            context = processes.Process(1, 0, 1, str(target), "1", cwd=directory, env={**SSH, "KEEP": "yes", "PATH": "/usr/bin:/bin"})
            launched = []

            def open_terminal(argv, **kwargs):
                # Stands in for `open -a Terminal <script>`: run the brokered script as Terminal would.
                self.assertEqual(argv[:3], ["/usr/bin/open", "-a", "Terminal"])
                launched.append(subprocess.Popen(["/bin/sh", argv[3]], env={"HOME": directory, "PATH": "/usr/bin:/bin"}))

            with mock.patch.object(pathlib.Path, "home", return_value=home), mock.patch.object(terminal, "command", side_effect=open_terminal):
                terminal._broker_terminal(install, context, [])
            launched[0].wait(30)
            for _ in range(100):
                if report.exists():
                    break
                time.sleep(0.1)
            self.assertEqual(report.read_text(), "KEEP")

    def test_other_platforms_keep_the_captured_environment(self):
        context = processes.Process(1, 0, 1, "/x", "1", cwd="/", env={**SSH, "KEEP": "yes"})
        self.assertIn("SSH_CONNECTION", terminal.restart_environment(context))
        self.assertNotIn("SSH_CONNECTION", terminal.mac_terminal_environment(context))
        self.assertEqual(terminal.mac_terminal_environment(context)["KEEP"], "yes")


if __name__ == "__main__":
    unittest.main()
