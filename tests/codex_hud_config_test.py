#!/usr/bin/env python3
"""Regression tests for the Python 3.9 Codex HUD configuration path."""

import builtins
import contextlib
import io
import importlib.util
import pathlib
import sys
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "lib" / "codex-hud-config.py"
SPEC = importlib.util.spec_from_file_location("codex_hud_config", HELPER)
assert SPEC is not None and SPEC.loader is not None
HUD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HUD)


class CodexHudTomlFallbackTest(unittest.TestCase):
    def without_toml_parser(self):
        original = builtins.__import__

        def guarded(name, globals=None, locals=None, fromlist=(), level=0):
            if name in ("tomllib", "tomli"):
                raise ImportError(name)
            return original(name, globals, locals, fromlist, level)

        return mock.patch("builtins.__import__", side_effect=guarded)

    def run_helper(self, action, path):
        stdout = io.StringIO()
        stderr = io.StringIO()
        argv = [str(HELPER), action, str(path)]
        with mock.patch.object(sys, "argv", argv):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = HUD.main()
        return status, stdout.getvalue(), stderr.getvalue()

    def test_empty_and_managed_only_configs_work_without_parser(self):
        managed = "\n".join(
            ("[tui]", HUD.BEGIN, HUD.STATUS_LINE, HUD.END, "")
        )
        with self.without_toml_parser():
            HUD.validate_toml("")
            HUD.validate_toml("[tui]\n")
            HUD.validate_toml(managed)

    def test_existing_user_config_is_not_silently_trusted_without_parser(self):
        with self.without_toml_parser():
            with self.assertRaisesRegex(HUD.ConfigError, "tomli"):
                HUD.validate_toml("[tui]\nanimations = true\n")

    def test_managed_file_lifecycle_works_without_parser(self):
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory) / "config.toml"
            with self.without_toml_parser():
                status, _, stderr = self.run_helper("install", config)
                self.assertEqual((status, stderr), (0, ""))
                installed = config.read_bytes()
                self.assertIn(HUD.STATUS_LINE.encode("utf-8"), installed)

                status, stdout, stderr = self.run_helper("install", config)
                self.assertEqual((status, stderr), (0, ""))
                self.assertIn("already current", stdout)
                self.assertEqual(config.read_bytes(), installed)

                status, _, stderr = self.run_helper("remove", config)
                self.assertEqual((status, stderr), (0, ""))
                self.assertEqual(config.read_text(encoding="utf-8"), "[tui]\n")

    def test_unvalidated_file_is_unchanged_without_parser(self):
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory) / "config.toml"
            original = b"[tui]\nanimations = true\n"
            config.write_bytes(original)
            with self.without_toml_parser():
                status, _, stderr = self.run_helper("install", config)
            self.assertEqual(status, 2)
            self.assertIn("tomli", stderr)
            self.assertEqual(config.read_bytes(), original)

    def test_invalid_toml_is_rejected_by_available_parser(self):
        try:
            import tomllib  # noqa: F401
        except ImportError:
            try:
                import tomli  # type: ignore  # noqa: F401
            except ImportError:
                expected = "TOML validation requires"
            else:
                expected = "not valid TOML"
        else:
            expected = "not valid TOML"
        with self.assertRaisesRegex(HUD.ConfigError, expected):
            HUD.validate_toml("[tui\nanimations = true\n")


if __name__ == "__main__":
    unittest.main()
