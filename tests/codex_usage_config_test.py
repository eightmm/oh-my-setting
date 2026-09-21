#!/usr/bin/env python3
"""Contract tests for the managed Codex usage-efficiency config keys."""

import contextlib
import importlib.util
import io
import pathlib
import sys
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "lib" / "codex-usage-config.py"
SPEC = importlib.util.spec_from_file_location("codex_usage_config", HELPER)
assert SPEC is not None and SPEC.loader is not None
USAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(USAGE)

PARSER = USAGE.toml_parser()

# Shaped like a real config: root keys, then many tables. A root key appended
# at the end would silently become a key of the last [projects."..."] table.
REALISTIC = """model = "gpt-test"

[tui]
animations = true

[projects."/srv/work/repo"]
trust_level = "trusted"
"""
V2_ENABLED = REALISTIC + "\n[features.multi_agent_v2]\nenabled = true\n"


def run_helper(action, path, *flags):
    stdout, stderr = io.StringIO(), io.StringIO()
    argv = [str(HELPER), action, str(path)] + list(flags)
    with mock.patch.object(sys, "argv", argv):
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = USAGE.main()
    return status, stdout.getvalue(), stderr.getvalue()


@unittest.skipIf(PARSER is None, "needs tomllib or tomli")
class CodexUsageConfigTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.config = pathlib.Path(self.directory.name) / "config.toml"

    def load(self):
        return PARSER.loads(self.config.read_text(encoding="utf-8"))

    def test_root_key_lands_at_root_and_round_trips(self):
        self.config.write_text(REALISTIC, encoding="utf-8")
        status, stdout, stderr = run_helper("install", self.config)
        self.assertEqual((status, stderr), (0, ""))
        self.assertIn("terminal ceiling installed", stdout)
        self.assertIn("not enabled in this config", stdout)
        parsed = self.load()
        self.assertEqual(parsed["background_terminal_max_timeout"], 900000)
        self.assertNotIn(
            "background_terminal_max_timeout", parsed["projects"]["/srv/work/repo"]
        )
        self.assertNotIn("multi_agent_v2", parsed.get("features", {}))
        self.assertEqual(parsed["model"], "gpt-test")

        installed = self.config.read_bytes()
        status, stdout, _ = run_helper("install", self.config)
        self.assertEqual(status, 0)
        self.assertIn("already current", stdout)
        self.assertEqual(self.config.read_bytes(), installed)
        self.assertEqual(run_helper("check", self.config)[0], 0)

        status, stdout, _ = run_helper("remove", self.config)
        self.assertEqual(status, 0)
        self.assertIn("terminal ceiling removed", stdout)
        self.assertEqual(self.config.read_text(encoding="utf-8"), REALISTIC)
        self.assertEqual(run_helper("check", self.config)[0], 1)

    def test_user_value_is_never_overridden(self):
        original = "background_terminal_max_timeout = 60000\n" + REALISTIC
        self.config.write_text(original, encoding="utf-8")
        status, stdout, _ = run_helper("install", self.config)
        self.assertEqual(status, 0)
        self.assertIn("preserved user value", stdout)
        self.assertEqual(self.config.read_text(encoding="utf-8"), original)
        status, stdout, _ = run_helper("check", self.config)
        self.assertEqual(status, 0)
        self.assertIn("terminal-ceiling=user(60000)", stdout)

    def test_customized_managed_block_is_preserved_on_install_and_remove(self):
        self.config.write_text(REALISTIC, encoding="utf-8")
        run_helper("install", self.config)
        edited = self.config.read_text(encoding="utf-8").replace("900000", "1200000")
        self.config.write_text(edited, encoding="utf-8")
        for action in ("install", "remove"):
            status, stdout, _ = run_helper(action, self.config)
            self.assertEqual(status, 0)
            self.assertIn("preserved customized value", stdout)
            self.assertEqual(self.config.read_text(encoding="utf-8"), edited)

    def test_v2_waits_are_written_only_into_an_enabled_table(self):
        self.config.write_text(V2_ENABLED, encoding="utf-8")
        status, stdout, stderr = run_helper("install", self.config)
        self.assertEqual((status, stderr), (0, ""))
        self.assertIn("multi_agent_v2 waits installed", stdout)
        table = self.load()["features"]["multi_agent_v2"]
        self.assertEqual(
            (table["enabled"], table["min_wait_timeout_ms"], table["default_wait_timeout_ms"]),
            (True, 120000, 300000),
        )
        self.assertNotIn("max_wait_timeout_ms", table)
        run_helper("remove", self.config)
        self.assertEqual(self.config.read_text(encoding="utf-8"), V2_ENABLED)

    def test_v2_is_never_enabled_or_made_invalid(self):
        # Codex rejects the whole file unless min <= default <= max, so every
        # shape that could break that, or that the user owns, is left alone.
        cases = {
            "disabled table": (
                "[features.multi_agent_v2]\nenabled = false\n",
                "not enabled in this config",
            ),
            "boolean form": ("[features]\nmulti_agent_v2 = true\n", "as a boolean"),
            "user default": (
                "[features.multi_agent_v2]\nenabled = true\ndefault_wait_timeout_ms = 30000\n",
                "preserved user values",
            ),
            "low user max": (
                "[features.multi_agent_v2]\nenabled = true\nmax_wait_timeout_ms = 60000\n",
                "below the managed default",
            ),
            "inline table": (
                "[features]\nmulti_agent_v2 = { enabled = true }\n",
                "does not edit",
            ),
        }
        for name, (tail, expected) in cases.items():
            with self.subTest(name):
                original = "background_terminal_max_timeout = 900000\n" + tail
                self.config.write_text(original, encoding="utf-8")
                status, stdout, stderr = run_helper("install", self.config)
                self.assertEqual((status, stderr), (0, ""))
                self.assertIn(expected, stdout)
                self.assertEqual(self.config.read_text(encoding="utf-8"), original)

    def test_dry_run_and_missing_file(self):
        status, stdout, _ = run_helper("install", self.config, "--dry-run")
        self.assertEqual(status, 0)
        self.assertIn("would install", stdout)
        self.assertFalse(self.config.exists())
        self.assertEqual(run_helper("install", self.config)[0], 0)
        self.assertEqual(self.load(), {"background_terminal_max_timeout": 900000})

    def test_invalid_or_tampered_config_is_refused_unchanged(self):
        for original in (
            "[tui\nanimations = true\n",
            USAGE.ROOT.begin + "\n" + REALISTIC,
        ):
            with self.subTest(original[:12]):
                self.config.write_text(original, encoding="utf-8")
                status, _, stderr = run_helper("install", self.config)
                self.assertEqual(status, 2)
                self.assertTrue(stderr.startswith("error: "))
                self.assertEqual(self.config.read_text(encoding="utf-8"), original)

    def test_crlf_config_keeps_its_line_endings(self):
        self.config.write_bytes(REALISTIC.replace("\n", "\r\n").encode("utf-8"))
        self.assertEqual(run_helper("install", self.config)[0], 0)
        self.assertNotIn(b"\n", self.config.read_bytes().replace(b"\r\n", b""))


class CodexUsageWithoutParserTest(unittest.TestCase):
    def test_install_refuses_to_guess_but_remove_still_reverts(self):
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory) / "config.toml"
            config.write_text(REALISTIC, encoding="utf-8")
            with mock.patch.object(USAGE, "toml_parser", return_value=None):
                status, stdout, _ = run_helper("install", config)
                self.assertEqual(status, 0)
                self.assertIn("skipped", stdout)
                self.assertEqual(config.read_text(encoding="utf-8"), REALISTIC)
                self.assertEqual(run_helper("check", config)[0], 1)
            if PARSER is None:
                return
            run_helper("install", config)
            with mock.patch.object(USAGE, "toml_parser", return_value=None):
                self.assertEqual(run_helper("remove", config)[0], 0)
            self.assertEqual(config.read_text(encoding="utf-8"), REALISTIC)


if __name__ == "__main__":
    unittest.main()
