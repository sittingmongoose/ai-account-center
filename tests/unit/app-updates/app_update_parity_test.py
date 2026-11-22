"""App-update helper/bridge parity (rows 8, 9, 24, 27). Fixtures and mocks only."""
import os
import pathlib
import re
import sys
import tempfile
import time
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: E402, F401
import app_update_common as common  # noqa: E402
import app_update_processes as processes  # noqa: E402
import app_updates as updater  # noqa: E402

BRIDGE = ROOT / "scripts/app-updates/app_update_codex.cjs"
# The bridge's fullVersion() grammar: numeric core plus an optional -/+ suffix.
JS_FULL_VERSION = re.compile(r"\d+\.\d+(?:\.\d+){0,3}(?:[-+][0-9A-Za-z.+-]+)?")
PARITY_VERSIONS = ["0.50.0", "0.50.0-alpha.1", "0.50.0-alpha-1", "0.50.0+build.1"]


class ParityTests(unittest.TestCase):
    def test_windows_argv_keeps_a_trailing_file_separator_byte(self):
        # Chromium's Windows CommandLine trims ASCII whitespace, not 0x1C-0x1F.
        from app_update_processes import desktop_arguments

        args = ["/app", "--user-data-dir=/x/b\x1c"]
        item = mock.Mock(args=args, cwd=None)
        self.assertEqual(
            desktop_arguments(item, "windows"),
            ["--user-data-dir=/x/b\x1c"],
        )

    def test_desktop_relaunch_replays_captured_xdg_config_home(self):
        item = mock.Mock(
            args=["/Applications/Codex.app/Contents/MacOS/Codex"],
            cwd="/",
            env={"XDG_CONFIG_HOME": "/custom", "KEEP": "yes"},
            pid=1,
            ppid=0,
        )
        helper_env = {"XDG_CONFIG_HOME": "/helper-home", "KEEP": "helper", "PATH": "/usr/bin"}
        mac = common.Install(
            "codex-desktop",
            "mac",
            pathlib.Path("/Applications/Codex.app"),
            "1.0.0",
            "official-download",
        )
        with mock.patch.dict(os.environ, helper_env, clear=False), mock.patch.object(
            processes, "family", return_value=[item]
        ), mock.patch.object(processes, "scan", return_value=[item]), mock.patch.object(
            processes, "desktop_roots", return_value=[item]
        ), mock.patch.object(processes, "command") as run, mock.patch.object(
            processes.subprocess, "Popen"
        ) as popen:
            self.assertEqual(processes.restart_desktops(mac, [item]), 1)
        popen.assert_not_called()
        self.assertEqual(run.call_args.kwargs["env"]["XDG_CONFIG_HOME"], "/custom")

        windows = common.Install(
            "codex-desktop",
            "windows",
            pathlib.Path("/fixture/ChatGPT.exe"),
            "1.0.0",
            "msix",
        )
        item.args = [str(windows.path)]
        with mock.patch.dict(os.environ, helper_env, clear=False), mock.patch.object(
            processes, "family", return_value=[item]
        ), mock.patch.object(processes, "scan", return_value=[item]), mock.patch.object(
            processes, "desktop_roots", return_value=[item]
        ), mock.patch.object(processes, "command") as run, mock.patch.object(
            processes.subprocess, "Popen"
        ) as popen:
            self.assertEqual(processes.restart_desktops(windows, [item]), 1)
        run.assert_not_called()
        env = popen.call_args.kwargs.get("env") or {}
        self.assertEqual(env.get("XDG_CONFIG_HOME"), "/custom")

    def test_version_text_matches_the_bridge_full_version_grammar(self):
        source = BRIDGE.read_text(encoding="utf-8")
        self.assertIn(r"(?:\.\d+){0,3}(?:[-+][0-9A-Za-z.+-]+)?", source)
        for value in PARITY_VERSIONS:
            js = JS_FULL_VERSION.search(value)
            self.assertIsNotNone(js, value)
            self.assertEqual(common.version_text(value), js.group(0), value)

    def test_python_path_rescan_restarts_a_daemon_that_started_during_install(self):
        install = common.Install("codex-cli", "ubuntu", pathlib.Path("/fixture/codex"), "1.0.0", "native")
        refreshed = common.Install("codex-cli", "ubuntu", install.path, "2.0.0", "native")
        daemon = processes.Process(
            42,
            1,
            1,
            str(install.path),
            "start-42",
            [str(install.path), "app-server"],
            cwd="/fixture",
            env={"HOME": "/fixture"},
        )
        scans = []
        restarted = []

        def fake_scan(platform):
            scans.append(platform)
            return [] if len(scans) == 1 else [daemon]

        def fake_instances(install, table):
            if not table:
                return [], [], []
            return [daemon], [daemon], []

        def fake_restart(install, contexts):
            restarted.extend(contexts)
            return [{"kind": "tmux"} for _ in contexts]

        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            pathlib.Path, "home", return_value=pathlib.Path(directory)
        ), mock.patch.object(updater, "scan", side_effect=fake_scan), mock.patch.object(
            updater, "cli_instances", side_effect=fake_instances
        ), mock.patch.object(updater, "check_terminal"), mock.patch.object(
            updater, "perform_cli_update"
        ), mock.patch.object(updater, "detect_cli", return_value=refreshed), mock.patch.object(
            updater, "terminate_cli", return_value=0
        ), mock.patch.object(updater, "restart_cli", side_effect=fake_restart):
            value = updater.update_cli(install, time.monotonic() + 60)
        self.assertGreaterEqual(len(scans), 2, "post-install rescan")
        self.assertEqual([item.pid for item in restarted], [42])
        self.assertEqual(value["status"], "updated")
