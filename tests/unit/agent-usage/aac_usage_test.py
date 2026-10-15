"""Offline tests for the aac-usage reader that agents run from the network share."""

import contextlib
import datetime as dt
import importlib.machinery
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[3] / "scripts" / "agent-usage" / "aac-usage"
loader = importlib.machinery.SourceFileLoader("aac_usage", str(SCRIPT))
spec = importlib.util.spec_from_loader("aac_usage", loader)
aac = importlib.util.module_from_spec(spec)
loader.exec_module(aac)


def iso(moment):
    return moment.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def snapshot(generated):
    window = {"key": "five_hour", "label": "5-hour", "kind": "rate_limit", "usedPercent": 70, "remainingPercent": 30,
              "resetAt": iso(generated + dt.timedelta(hours=2)), "resetPassed": False, "windowMinutes": 300,
              "used": None, "limit": None, "remaining": None, "unit": None, "expiresAt": None, "unlimited": None,
              "pool": None, "modelIds": [],
              "burn": {"last5Minutes": 120, "last15Minutes": 60, "lastHour": 30, "last6Hours": None, "last24Hours": None,
                       "pace": 120, "paceMinutes": 5, "projectedFullAt": iso(generated + dt.timedelta(minutes=15)),
                       "runsOutBeforeReset": True, "periodStartedAt": iso(generated - dt.timedelta(hours=3))}}
    base = {"provider": "codex", "providerLabel": "Codex", "plan": "Pro", "platform": "ubuntu", "host": "ubuntu",
            "status": "ok", "sampledAt": iso(generated), "ageSeconds": 30, "hidden": False, "source": "Codex usage on Ubuntu"}
    return {"version": 1, "source": "AI Account Center", "generatedAt": iso(generated), "refreshSeconds": 60,
            "staleAfterSeconds": 300,
            "accounts": [dict(base, id="codex:gmail", label="Gmail", email="one@example.com", profile="gmail",
                              active=True, windows=[window]),
                         dict(base, id="codex:party", label="Party", email="two@example.com", profile="party",
                              active=False, windows=[]),
                         dict(base, id="claude:hidden", provider="claude", providerLabel="Claude", label="Hidden",
                              email="three@example.com", profile=None, active=False, hidden=True, windows=[])]}


class ReaderTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.folder = Path(temporary.name)

    def run_reader(self, *argv, generated=None):
        generated = generated or aac.now_utc()
        (self.folder / "usage.json").write_text(json.dumps(snapshot(generated)), encoding="utf-8")
        (self.folder / "history.json").write_text(json.dumps({"version": 1, "generatedAt": iso(generated), "hours": 168, "series": [
            {"accountId": "codex:gmail", "provider": "codex", "label": "Gmail", "email": "one@example.com", "profile": "gmail",
             "window": "five_hour", "windowLabel": "5-hour", "kind": "rate_limit", "unit": None,
             "points": [[iso(generated - dt.timedelta(minutes=5))[:16] + "Z", 60, None], [iso(generated)[:16] + "Z", 70, None]]}]}),
            encoding="utf-8")
        out = io.StringIO()
        with mock.patch.object(aac.sys, "argv", ["aac-usage", "--dir", str(self.folder), *argv]), \
                contextlib.redirect_stdout(out):
            aac.main()
        return out.getvalue()

    def test_summary_names_each_account_and_its_pace(self):
        text = self.run_reader()
        self.assertIn("Gmail  one@example.com  profile gmail  [Pro, Ubuntu VM, ACTIVE]", text)
        self.assertIn("5-hour: 70% used (30% left)", text)
        self.assertRegex(text, "pace 120%/h over 5 min, 30%/h over 1 h, full in 1[45]m BEFORE the reset")
        self.assertNotIn("Hidden", text)
        self.assertNotIn("WARNING", text)
        self.assertTrue(all(ord(character) < 128 for character in text))

    def test_filters_by_provider_alias_and_account(self):
        self.assertIn("Hidden", self.run_reader("--all"))
        self.assertNotIn("Codex", self.run_reader("--provider", "anthropic", "--all").split("\n", 1)[1])
        text = self.run_reader("--account", "two@")
        self.assertIn("Party", text)
        self.assertNotIn("Gmail", text)
        data = json.loads(self.run_reader("--provider", "openai", "--json"))
        self.assertEqual([entry["id"] for entry in data["accounts"]], ["codex:gmail", "codex:party"])

    def test_old_snapshot_is_called_stale(self):
        text = self.run_reader(generated=aac.now_utc() - dt.timedelta(minutes=20))
        self.assertIn("WARNING: this snapshot is 20m old", text)

    def test_history_lists_the_measured_readings(self):
        text = self.run_reader("history", "--dir", str(self.folder), "--provider", "codex")
        self.assertIn("codex Gmail (one@example.com) 5-hour:", text)
        self.assertIn("70%", text)

    def test_options_before_the_history_command_are_kept(self):
        # run_reader passes --dir before the subcommand; the history parser must not reset it.
        data = json.loads(self.run_reader("--provider", "codex", "--json", "history"))
        self.assertEqual([entry["accountId"] for entry in data["series"]], ["codex:gmail"])

    def test_a_retained_window_says_how_old_it_is(self):
        generated = aac.now_utc()
        data = snapshot(generated)
        data["accounts"][0]["windows"].append({
            "key": "prepaid", "label": "Prepaid balance", "kind": "balance", "usedPercent": None,
            "remainingPercent": None, "resetAt": None, "resetPassed": False, "remaining": 12, "unit": "USD",
            "used": None, "limit": None, "cached": True, "sampledAt": iso(generated - dt.timedelta(hours=2)),
            "burn": None})
        (self.folder / "usage.json").write_text(json.dumps(data), encoding="utf-8")
        out = io.StringIO()
        with mock.patch.object(aac.sys, "argv", ["aac-usage", "--dir", str(self.folder)]), \
                contextlib.redirect_stdout(out):
            aac.main()
        self.assertIn("Prepaid balance: 12 USD left; earlier reading from 2h 0m ago", out.getvalue())


class RulesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)

    def test_block_is_added_once_replaced_in_place_and_keeps_hard_links(self):
        shared = self.home / ".claude-t3" / "CLAUDE.md"
        shared.parent.mkdir()
        shared.write_text("# Existing rules\n\nKeep me.\n", encoding="utf-8")
        account = self.home / ".claude-t3" / "work"
        account.mkdir()
        os.link(shared, account / "CLAUDE.md")
        self.assertEqual(aac.install_block(shared, "first\n"), "updated")
        self.assertEqual(aac.install_block(account / "CLAUDE.md", "first\n"), "unchanged")
        self.assertEqual(aac.install_block(shared, "second\n"), "updated")
        text = (account / "CLAUDE.md").read_text(encoding="utf-8")
        self.assertEqual(text.count(aac.BEGIN), 1)
        self.assertIn("Keep me.", text)
        self.assertIn("second", text)
        self.assertNotIn("first", text)
        self.assertEqual(os.stat(shared).st_nlink, 2)

    def test_a_block_without_its_end_marker_is_reported_and_the_other_files_still_install(self):
        broken = self.home / ".claude" / "CLAUDE.md"
        broken.parent.mkdir()
        broken.write_text("rules\n" + aac.BEGIN + "\nhalf a block\n", encoding="utf-8")
        (self.home / ".codex").mkdir()
        out = io.StringIO()
        with mock.patch.object(aac.Path, "home", return_value=self.home), contextlib.redirect_stdout(out):
            aac.cmd_install_rules(None)
        self.assertIn("error", out.getvalue())
        self.assertEqual(broken.read_text(encoding="utf-8").count(aac.BEGIN), 1)
        self.assertIn(aac.BEGIN, (self.home / ".codex" / "AGENTS.md").read_text(encoding="utf-8"))

    def test_falling_usage_is_not_called_a_burn(self):
        text = aac.burn_text({"pace": -3.6, "paceMinutes": 5, "projectedFullAt": None}, aac.now_utc())
        self.assertEqual(text, "usage falling -3.60%/h over 5 min")

    def test_targets_cover_every_agent_and_only_existing_t3_account_files(self):
        for folder in (".claude", ".codex", ".claude-t3/party", ".codex-t3/lexx", ".codex-t3/empty"):
            (self.home / folder).mkdir(parents=True)
        (self.home / ".codex-t3" / "lexx" / "AGENTS.override.md").write_text("x", encoding="utf-8")
        (self.home / ".claude-t3" / "party" / "CLAUDE.md").write_text("x", encoding="utf-8")
        paths = {str(path.relative_to(self.home)).replace(os.sep, "/") for _, path, _, _ in aac.rules_targets(self.home)}
        self.assertIn(".claude/CLAUDE.md", paths)
        self.assertIn(".zcode/AGENTS.md", paths)
        self.assertIn(".claude-t3/party/CLAUDE.md", paths)
        self.assertIn(".codex-t3/lexx/AGENTS.override.md", paths)
        self.assertNotIn(".codex-t3/empty/AGENTS.override.md", paths)

    def test_rules_text_is_ascii_and_names_the_command_for_this_computer(self):
        text = aac.rules_text()
        self.assertTrue(all(ord(character) < 128 for character in text))
        self.assertIn("aac-usage", text)
        self.assertIn("BEFORE the reset", text)

    def test_rules_name_the_reader_beside_its_snapshot(self):
        folder = self.home / "custom-share" / "aac-usage"
        with mock.patch.object(aac, "own_folder", return_value=folder):
            self.assertIn(str(folder / "aac-usage"), aac.rules_text())
        with mock.patch.object(aac, "own_folder", return_value=None):
            self.assertIn(str(aac.share_folder() / "aac-usage"), aac.rules_text())


if __name__ == "__main__":
    unittest.main()
