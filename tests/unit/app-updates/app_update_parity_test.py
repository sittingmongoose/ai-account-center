"""App-update helper/bridge parity (rows 8, 9, 24) and the base process boundaries. Fixtures and mocks only."""
import contextlib
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

    def _update(self, install, refreshed, plan, home, extra=()):
        """update_cli over a scripted scan: `plan[i](platform)` answers scan i+1.

        A callable step raises or returns a table; anything past the plan repeats the last step,
        so a post-install scan is visible in `scans` instead of silently answered.
        """
        scans, killed, restarted = [], [], []

        def fake_scan(platform):
            scans.append(platform)
            return plan[min(len(scans), len(plan)) - 1]

        def fake_terminate(_install, targets, **_kwargs):
            killed.extend(item.pid for item in targets)
            return 0

        def fake_restart(_install, contexts):
            restarted.extend(item.pid for item in contexts)
            return [{"kind": "terminal"} for _ in contexts]

        patches = [
            mock.patch.object(pathlib.Path, "home", return_value=home),
            mock.patch.object(updater, "scan", side_effect=fake_scan),
            mock.patch.object(updater, "perform_cli_update"),
            mock.patch.object(updater, "detect_cli", return_value=refreshed),
            mock.patch.object(updater, "terminate_cli", side_effect=fake_terminate),
            mock.patch.object(updater, "restart_cli", side_effect=fake_restart),
            *extra,
        ]
        with contextlib.ExitStack() as stack:
            for patch in patches:
                stack.enter_context(patch)
            row = updater.update_cli(install, time.monotonic() + 60)
        return row, scans, killed, restarted

    def test_a_terminal_on_the_new_binary_during_the_install_is_left_alone(self):
        # Reviewer probe M2: Claude's native installer repoints the launcher to the new versioned
        # executable while the installer still runs, and an attached terminal may already start
        # from it. It runs the verified version, so it is never stopped or reopened for the update.
        with tempfile.TemporaryDirectory() as directory:
            home = pathlib.Path(directory)
            versions = home / ".local/share/claude/versions"
            for version in ("1.0.0", "2.0.0"):
                binary = versions / version / "claude"
                binary.parent.mkdir(parents=True)
                binary.write_text("#!/bin/sh\n", encoding="utf-8")
            launcher = home / ".local/bin/claude"
            launcher.parent.mkdir(parents=True)
            launcher.symlink_to(versions / "1.0.0/claude")
            install = common.Install("claude-code", "ubuntu", launcher, "1.0.0", "native")
            refreshed = common.Install("claude-code", "ubuntu", launcher, "2.0.0", "native")
            terminal = processes.Process(
                42, 1, os.getuid(), str(launcher), "s42", [str(launcher)], cwd=str(home), env={"HOME": str(home)}
            )

            def installer(_install, _deadline):
                launcher.unlink()
                launcher.symlink_to(versions / "2.0.0/claude")

            plan = [
                [],
                [terminal],  # Only a post-install scan would see it, already on 2.0.0.
            ]
            extra = (
                mock.patch.object(updater, "perform_cli_update", side_effect=installer),
                mock.patch.object(updater, "check_terminal"),
                mock.patch.object(processes, "terminal_attached", return_value={42}),
                mock.patch.object(processes, "cli_context"),
            )
            row, scans, killed, restarted = self._update(install, refreshed, plan, home, extra=extra)
        self.assertEqual(scans, ["ubuntu"], "the update scans once, before the install")
        self.assertEqual(killed, [])
        self.assertEqual(restarted, [])
        self.assertEqual((row["status"], row["restartedProcesses"]), ("updated", 0))

    def test_a_failed_late_scan_never_strands_a_stopped_terminal(self):
        # Reviewer probe M3: Windows npm Codex stops its terminals before the install. Whatever a
        # later scan would answer, the run already owes those terminals a relaunch.
        with tempfile.TemporaryDirectory() as directory:
            home = pathlib.Path(directory)
            npm = home / "AppData/Roaming/npm"
            install = common.Install("codex-cli", "windows", npm / "codex.cmd", "1.0.0", "npm",
                                     package_root=npm / "node_modules/@openai/codex")
            refreshed = common.Install("codex-cli", "windows", install.path, "2.0.0", "npm",
                                       package_root=install.package_root)
            script = str(install.package_root / "bin/codex.js")
            terminal = processes.Process(
                42, 1, 0, "/fixture/nodejs/node.exe", "s42", ["node.exe", script],
                cwd=str(home), env={"HOME": str(home)}, session=1,
            )

            def read_context(_platform, item):
                item.cwd, item.env = str(home), {"HOME": str(home)}

            def failing_scan(_platform):
                raise common.UpdateFailure("restart_context")

            plan = [[terminal], [terminal], failing_scan]
            extra = (
                mock.patch.object(updater, "npm_view_latest", return_value="2.0.0"),
                mock.patch.object(updater, "check_terminal"),
                mock.patch.object(processes, "terminal_attached", return_value={42}),
                mock.patch.object(processes, "cli_context", side_effect=read_context),
            )
            row, scans, killed, restarted = self._update(install, refreshed, plan, home, extra=extra)
        self.assertEqual(len(scans), 2, "one scan, one pre-stop rescan, none after the install")
        self.assertEqual(set(killed), {42}, "the pre-install stop is kept")
        self.assertEqual(restarted, [42], "the stopped terminal is reopened")
        self.assertEqual((row["status"], row["restartedProcesses"]), ("updated", 1))
