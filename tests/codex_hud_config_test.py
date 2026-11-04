#!/usr/bin/env python3
"""Regression tests for the Codex HUD configuration path."""

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


class CodexHudConfigTest(unittest.TestCase):
    def run_helper(self, action, path):
        stdout = io.StringIO()
        stderr = io.StringIO()
        argv = [str(HELPER), action, str(path)]
        with mock.patch.object(sys, "argv", argv):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = HUD.main()
        return status, stdout.getvalue(), stderr.getvalue()

    def test_remove_without_tomllib_preserves_user_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory) / "config.toml"
            config.write_text('model = "user"\n', encoding="utf-8")
            self.assertEqual(self.run_helper("install", config)[0], 0)
            with mock.patch.dict(sys.modules, {"tomllib": None}):
                self.assertEqual(self.run_helper("remove", config)[0], 0)
            self.assertIn('model = "user"', config.read_text(encoding="utf-8"))
            self.assertNotIn("oh-my-setting managed", config.read_text(encoding="utf-8"))

    def test_managed_file_lifecycle(self):
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory) / "config.toml"
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

    def test_remove_preserves_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "actual.toml"
            link = pathlib.Path(directory) / "config.toml"
            target.write_text('[tui]\n', encoding="utf-8")
            self.assertEqual(self.run_helper("install", target)[0], 0)
            before = target.read_bytes()
            link.symlink_to(target)
            status, _, _ = self.run_helper("remove", link)
            self.assertNotEqual(status, 0)
            self.assertTrue(link.is_symlink())
            self.assertEqual(target.read_bytes(), before)

    def test_invalid_toml_is_rejected(self):
        with self.assertRaisesRegex(HUD.ConfigError, "not valid TOML"):
            HUD.validate_toml("[tui\nanimations = true\n")

    def test_table_codex_inserted_inside_the_block_is_kept_on_remove(self):
        # `codex mcp add` puts its table before the file's trailing comments,
        # which lands it between our status line and END when END closed the file.
        mcp = '[mcp_servers.oh-my-setting]\ncommand = "python3"\n'
        text = "[tui]\n%s\n%s\n\n%s%s\n" % (HUD.BEGIN, HUD.STATUS_LINE, mcp, HUD.END)
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory) / "config.toml"
            config.write_text(text, encoding="utf-8")
            self.assertEqual(self.run_helper("check", config)[1].strip(), "codex-hud: managed")
            status, stdout, stderr = self.run_helper("remove", config)
            self.assertEqual((status, stderr), (0, ""), stdout)
            self.assertEqual(config.read_text(encoding="utf-8"), "[tui]\n\n" + mcp)
            # Anything after END means it did not close the file: not ours to edit.
            config.write_text(text + "[other]\n", encoding="utf-8")
            self.assertIn("preserved customized", self.run_helper("remove", config)[1])


if __name__ == "__main__":
    unittest.main()
