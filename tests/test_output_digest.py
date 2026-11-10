"""Actual bounded log reader and metadata digest; --cli uses the runtime too."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("output_digest", ROOT / "scripts/lib/oms_runtime/output_digest.py")
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)
CLI = "--cli" in sys.argv
if CLI:
    sys.argv.remove("--cli")


class DigestTests(unittest.TestCase):
    def test_raw_hash_and_locators_are_exact(self):
        raw = b"ok\nERROR: bad\nFAILED tests/test_x.py\n"
        report = d.summarize(raw, 0, len(raw), exit_code=1)
        self.assertEqual(report["window"]["full_file_sha256"], hashlib.sha256(raw).hexdigest())
        self.assertEqual(report["window"]["read_bytes"], len(raw))
        self.assertEqual(report["candidates"]["total"], 2)
        self.assertTrue(report["window"]["scan_complete"])
        self.assertFalse(report["diagnosis_complete"])
        self.assertIsNone(report["process_success"])
        for row in report["candidates"]["items"]:
            line = raw[row["byte_start"]:row["byte_end"]-1]
            self.assertEqual(row["line_sha256"], hashlib.sha256(line).hexdigest())
        self.assertEqual(report["reported_exit"], {"code": 1, "source": "caller-supplied-unverified"})

    def test_no_matches_or_zero_exit_is_not_success(self):
        report = d.summarize(b"all fine\n", 0, 9, exit_code=0)
        self.assertIsNone(report["process_success"])
        self.assertEqual(report["candidates"]["total"], 0)
        self.assertIn("not a verdict", d.render(report))
        unknown = d.summarize(b"", 0, 0)
        self.assertEqual(unknown["reported_exit"]["source"], "unreported")
        self.assertEqual(unknown["window"]["complete_lines"], 0)

    def test_ansi_and_multiple_labels_do_not_count_as_distinct_lines(self):
        raw = "\x1b[31mERROR: CUDA OutOfMemoryError\x1b[0m\n한글 NaN\n".encode()
        report = d.summarize(raw, 0, len(raw))
        self.assertEqual(report["candidates"]["total"], 2)
        self.assertEqual(report["class_counts"], {"error": 1, "memory": 1, "nonfinite": 1})
        self.assertNotIn("CUDA", json.dumps(report))
        self.assertNotIn("한글", json.dumps(report, ensure_ascii=False))

    def test_tail_trims_partial_first_line_and_does_not_invent_line_number(self):
        raw = b"part of ERROR\nERROR: retained\nunfinished ERROR"
        report = d.summarize(raw, 100, 100 + len(raw))
        self.assertEqual(report["candidates"]["total"], 1)
        row = report["candidates"]["items"][0]
        self.assertIsNone(row["file_line"])
        self.assertEqual(row["byte_start"], 100 + raw.index(b"ERROR: retained"))
        self.assertIsNone(report["window"]["full_file_sha256"])
        self.assertFalse(report["window"]["scan_complete"])
        self.assertEqual(report["window"]["trailing_unterminated_bytes"], len(b"unfinished ERROR"))

    def test_no_newline_is_explicitly_incomplete(self):
        raw = b"ERROR without newline"
        report = d.summarize(raw, 0, len(raw))
        self.assertEqual(report["window"]["trailing_unterminated_bytes"], len(raw))
        self.assertEqual(report["candidates"]["total"], 0)
        self.assertFalse(report["window"]["scan_complete"])
        tail = d.summarize(raw, 10, len(raw) + 10)
        self.assertEqual(tail["window"]["leading_partial_bytes"], len(raw))

    def test_output_cap_counts_omitted_candidates(self):
        raw = b"ERROR: same\n" * 10
        report = d.summarize(raw, 0, len(raw), limit=2)
        self.assertEqual(report["candidates"]["total"], 10)
        self.assertEqual(report["candidates"]["omitted"], 8)
        self.assertEqual(len(report["candidates"]["items"]), 2)
        self.assertEqual(report["class_counts"]["error"], 10)

    def test_line_length_and_line_count_caps_are_visible(self):
        raw = b"ERROR" + b"x" * d.LINE_MAX + b"\nERROR: small\n"
        report = d.summarize(raw, 0, len(raw))
        self.assertEqual(report["window"]["oversized_lines_skipped"], 1)
        self.assertEqual(report["candidates"]["total"], 1)
        with patch.object(d, "LINE_COUNT_MAX", 2):
            raw = b"ok\nok\nERROR: outside cap\n"
            report = d.summarize(raw, 0, len(raw))
        self.assertEqual(report["window"]["unscanned_lines"], 1)
        self.assertFalse(report["window"]["scan_complete"])
        self.assertEqual(report["candidates"]["total"], 0)

    def test_invalid_utf8_binary_and_bad_windows_fail(self):
        for raw in (b"\xff\n", b"nul\0\n"):
            with self.subTest(raw=raw), self.assertRaises(d.DigestError):
                d.summarize(raw, 0, len(raw))
        for args in (("text", 0, 4), (b"a", -1, 1), (b"aa", 0, 1), (b"", True, 2)):
            with self.subTest(args=args), self.assertRaises(d.DigestError):
                d.summarize(*args)

    def test_numeric_limits_and_exit_statuses(self):
        for value in (True, -256, 256, "0"):
            with self.subTest(value=value), self.assertRaises(d.DigestError):
                d.summarize(b"", 0, 0, exit_code=value)
        for value in (True, 0, 101):
            with self.subTest(value=value), self.assertRaises(d.DigestError):
                d.summarize(b"", 0, 0, limit=value)
        self.assertEqual(d.summarize(b"", 0, 0, exit_code=-15)["reported_exit"]["code"], -15)

    def test_pure_digest_never_executes_or_echoes_content(self):
        raw = b"ERROR: fixture-private-value\n"
        with patch("builtins.open", side_effect=AssertionError("read")), \
                patch("subprocess.run", side_effect=AssertionError("exec")):
            report = d.summarize(raw, 0, len(raw))
            text = d.render(report)
        self.assertNotIn("fixture-private-value", json.dumps(report))
        self.assertNotIn("fixture-private-value", text)


class FileTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="oms-output-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / "repo"; self.repo.mkdir()
        self.path = self.repo / "run.log"
        self.path.write_bytes(b"ok\nERROR: sample\n")

    def test_file_read_is_bounded_and_unchanged(self):
        raw = b"head\n" + b"x" * 2000 + b"\nERROR: last\n"
        self.path.write_bytes(raw)
        report = d.inspect_file(self.repo, self.path, read_bytes=512)
        self.assertEqual(report["window"]["read_bytes"], 512)
        self.assertEqual(report["window"]["byte_start"], len(raw) - 512)
        self.assertEqual(report["candidates"]["total"], 1)
        self.assertEqual(self.path.read_bytes(), raw)
        head = d.inspect_file(self.repo, "run.log", read_bytes=512, window="head")
        self.assertEqual(head["window"]["byte_start"], 0)
        self.assertEqual(head["candidates"]["total"], 0)
        self.assertIsNone(head["window"]["full_file_sha256"])

    def test_links_parent_paths_and_special_files_are_rejected(self):
        (self.repo / "link").symlink_to(self.path)
        os.link(self.path, self.repo / "hard")
        (self.repo / "outside").symlink_to(self.root, target_is_directory=True)
        (self.root / "outer.log").write_bytes(b"ERROR\n")
        os.mkfifo(self.repo / "pipe")
        for value in ("link", "hard", "outside/outer.log", "../outer.log", ".", "pipe", "missing", self.root / "outer.log"):
            with self.subTest(value=value), self.assertRaises(d.DigestError):
                d.inspect_file(self.repo, value)

    def test_mid_read_change_is_refused(self):
        original_read = os.read
        def mutate(fd, number):
            data = original_read(fd, number)
            with self.path.open("ab") as handle:
                handle.write(b"new\n")
            return data
        with patch.object(d.os, "read", mutate), self.assertRaises(d.DigestError):
            d.inspect_file(self.repo, "run.log")

    def test_bad_arguments_rejected_before_open(self):
        for changes in ({"read_bytes": 0}, {"read_bytes": d.READ_MAX + 1}, {"limit": False},
                        {"window": "all"}, {"exit_code": 999}):
            with patch.object(d.os, "open", side_effect=AssertionError("opened")), self.assertRaises(d.DigestError):
                d.inspect_file(self.repo, "run.log", **changes)

    def test_reader_error_hides_path_and_input(self):
        with self.assertRaises(d.DigestError) as caught:
            d.inspect_file(self.repo, "no-private-fixture-here")
        self.assertNotIn("no-private-fixture-here", str(caught.exception))
        self.assertNotIn(str(self.root), str(caught.exception))


if CLI:
    class RuntimeTests(unittest.TestCase):
        def test_runtime_dispatch_and_child_read(self):
            with tempfile.TemporaryDirectory(prefix="oms-output-cli-") as directory:
                repo = Path(directory)
                raw = b"ERROR: private-value\n"
                (repo / "log.txt").write_bytes(raw)
                env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1", OMS_HARNESS_CHILD="1")
                def call(*options):
                    return subprocess.run(["bash", str(ROOT / "scripts/runtime.sh"), "--repo", str(repo),
                                           "output-digest", "log.txt", *options], env=env,
                                          capture_output=True, text=True, timeout=60)
                result = call("--reported-exit", "1")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["candidates"]["total"], 1)
                self.assertNotIn("private-value", result.stdout)
                text = call("--format", "text")
                self.assertEqual(text.returncode, 0, text.stderr)
                self.assertIn("candidate bytes", text.stdout)
                self.assertEqual(call("--format", "text", "--pretty").returncode, 2)
                self.assertEqual(call("--read-bytes", "10").returncode, 2)
                self.assertEqual((repo / "log.txt").read_bytes(), raw)
                self.assertEqual(sorted(p.name for p in repo.iterdir()), ["log.txt"])

if __name__ == "__main__":
    unittest.main(verbosity=2)
