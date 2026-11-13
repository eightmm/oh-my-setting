"""Context contracts; --cli optionally compares an explicit candidate OMS entry point."""
from __future__ import annotations

import argparse
from copy import deepcopy
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
LIB = ROOT / "scripts/lib/oms_runtime"
REFERENCE_COMMIT = "74f87f654a314961420be2aa8ab6dfc1999151e1"


def load(name):
    spec = importlib.util.spec_from_file_location(name, LIB / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


inspector = load("context_inspect")
policy = load("child_policy")
sys.path.insert(0, str(ROOT / "scripts/lib"))
from oms_runtime import context_session as session_view  # noqa: E402  (a package import: it shares the inspector's errors)
reporter = load("context_report")
ERROR = inspector.InspectionError
runner = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
runner.add_argument("--cli", action="store_true")
runner.add_argument("--candidate-oms")
options, remaining = runner.parse_known_args()
CLI = options.cli
CANDIDATE = os.path.abspath(options.candidate_oms) if options.candidate_oms else None
if CANDIDATE and not CLI:
    runner.error("--candidate-oms requires --cli")
if CANDIDATE and (not Path(CANDIDATE).is_file() or not os.access(CANDIDATE, os.X_OK)):
    runner.error("--candidate-oms must name an executable file")
if CANDIDATE:
    # Run from the reference checkout: a migrated dispatcher cannot be its own oracle.
    try:
        pinned = subprocess.run(["git", "-C", str(ROOT), "diff", "--quiet", "--no-ext-diff",
                                 "--no-textconv", REFERENCE_COMMIT, "--", "scripts", "config",
                                 "rules", "roles", "templates"], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        runner.error("cannot verify the frozen reference tree")
    if pinned.returncode != 0:
        runner.error("candidate comparison requires production paths at the frozen reference commit; "
                     "run this suite from its separate reference checkout")
sys.argv[1:] = remaining


def contract_cases(test, name, entrypoint):
    suite = json.loads((ROOT / "migration/contracts" / name).read_text(encoding="utf-8"))
    test.assertEqual(suite["fixture_schema"], 1)
    reference = suite["reference"]
    test.assertEqual(reference["commit"], REFERENCE_COMMIT)
    test.assertEqual(reference["source"], "scripts/lib/oms_runtime/context_inspect.py")
    test.assertEqual(reference["entrypoint"], entrypoint)
    test.assertEqual(hashlib.sha256((LIB / "context_inspect.py").read_bytes()).hexdigest(),
                     reference["source_sha256"], "reference changed; review the contract before refreshing fixtures")
    names = [case["name"] for case in suite["cases"]]
    test.assertTrue(names)
    test.assertEqual(len(names), len(set(names)))
    for case in suite["cases"]:
        test.assertNotEqual("expected" in case, "expected_error" in case, case["name"])
        if "expected_error" in case:
            test.assertIsInstance(case["expected_error"], str)
        yield case, dict(reference["arguments"], **case.get("arguments", {}))


def check_result(test, case, action):
    if "expected_error" in case:
        with test.assertRaises(ERROR) as caught:
            action()
        test.assertEqual(str(caught.exception), case["expected_error"])
    else:
        # JSON value comparison preserves booleans and numeric types, unlike Python's ==.
        canonical = lambda value: json.dumps(value, sort_keys=True, ensure_ascii=True, allow_nan=False)
        test.assertEqual(canonical(action()), canonical(case["expected"]))


def source(path="src.py", data=b"x" * 100):
    return {"path": path, "reason": "direct target", "priority": 120,
            "bytes_before": len(data), "bytes_selected": len(data), "omitted_bytes": 0,
            "selection": "full", "sha256": hashlib.sha256(data).hexdigest()}


def manifest():
    return {"schema": 1, "generated_at": "2026-11-09T00:00:00Z", "phase": "implementation",
            "budget_bytes": 1000, "selected_bytes": 140, "remaining_bytes": 860,
            "targets": ["src.py"], "selected": [source()], "omitted": [],
            "context_debt": 0, "missing_required": [], "truncated_required": [],
            "sufficient": True, "bundle_sha256": "a" * 64, "contract_state_digest": "b" * 64,
            "bundle_path": "missing-bundle.txt"}


def tree_bytes(root):
    return {p.relative_to(root).as_posix(): p.read_bytes()
            for p in root.rglob("*") if p.is_file() and not p.is_symlink()}


class ProjectionTests(unittest.TestCase):
    def test_frozen_projection_contracts(self):
        for filename, entrypoint, fields in (
                ("context-inspect-v1.json", "inspect_manifest", ("input",)),
                ("context-compare-v1.json", "compare_manifests", ("left", "right"))):
            for case, arguments in contract_cases(self, filename, entrypoint):
                inputs = [deepcopy(case[field]) for field in fields]
                before = deepcopy(inputs)
                with self.subTest(suite=filename, case=case["name"]):
                    with patch.object(inspector.os, "open", side_effect=AssertionError("unexpected I/O")):
                        check_result(self, case, lambda: getattr(inspector, entrypoint)(*inputs, **arguments))
                    self.assertEqual(inputs, before)

    def test_bad_manifests_fail_instead_of_looking_empty(self):
        variants = []
        for key, value in (("schema", True), ("schema", 2), ("budget_bytes", -1),
                           ("selected_bytes", True), ("remaining_bytes", 99),
                           ("context_debt", 1), ("sufficient", 1), ("phase", "invented"),
                           ("bundle_sha256", "bad"), ("targets", ["src.py", "src.py"]),
                           ("selected", {}), ("missing_required", ["src.py"])):
            data = manifest(); data[key] = value; variants.append(data)
        data = manifest(); data["selected"].append(deepcopy(data["selected"][0])); variants.append(data)
        data = manifest(); data["targets"].append("absent.py"); variants.append(data)
        data = manifest(); data["selected"][0]["bytes_selected"] = 101; variants.append(data)
        data = manifest(); data["selected"][0]["omitted_bytes"] = 1; variants.append(data)
        data = manifest(); data.update(context_debt=1, missing_required=["absent"],
                                     truncated_required=["absent"], sufficient=False); variants.append(data)
        for data in variants:
            with self.subTest(data=data):
                with self.assertRaises(ERROR):
                    inspector.inspect_manifest(data)

    def test_impossible_truncation_and_undercounted_truncated_required_are_rejected(self):
        data = manifest()
        data["selected"][0].update(bytes_selected=1, omitted_bytes=1, selection="head")
        with self.assertRaises(ERROR):
            inspector.inspect_manifest(data)
        data = manifest()
        data["selected"][0].update(bytes_selected=75, omitted_bytes=60, selection="head")
        data.update(context_debt=1, missing_required=["src.py"], sufficient=False)
        with self.assertRaises(ERROR):
            inspector.inspect_manifest(data)
        data["truncated_required"] = ["src.py"]
        inspector.inspect_manifest(data)

    def test_marker_sized_slices_the_compiler_can_produce_are_accepted(self):
        for policy_name in ("head", "tail"):
            data = manifest()
            # 100 bytes in 50: marker "\n[TRUNCATED: 50 bytes omitted]\n" is 31 bytes, 19 retained.
            data["selected"][0].update(bytes_selected=50, omitted_bytes=81, selection=policy_name)
            data.update(context_debt=1, missing_required=["src.py"],
                        truncated_required=["src.py"], sufficient=False)
            with self.subTest(policy=policy_name):
                inspector.inspect_manifest(data)
            data["selected"][0]["omitted_bytes"] = 80
            with self.subTest(policy=policy_name, retained=20):
                with self.assertRaises(ERROR):
                    inspector.inspect_manifest(data)

    def test_bounds_and_unsafe_path_labels(self):
        for limit in (0, 101, True, 1.2, "1"):
            with self.subTest(limit=limit), self.assertRaises(ERROR):
                inspector.inspect_manifest(manifest(), limit=limit)
        for path in ("../src.py", "/src.py", "a//b", "a/./b", "x\\y", "C:x", "x\x1by", "~user/x"):
            data = manifest(); data["selected"][0]["path"] = path
            with self.subTest(path=path), self.assertRaises(ERROR):
                inspector.inspect_manifest(data)
        data = manifest(); data["omitted"] = [{}] * (inspector.MAX_ROWS + 1)
        with self.assertRaises(ERROR):
            inspector.inspect_manifest(data)

    def test_explicit_invalid_phase_rule_digest_is_not_silently_ignored(self):
        for digest in (None, "", True, [], {}, 4, "x" * 64, "A" * 64, "a" * 63, "a" * 65):
            data = manifest()
            data["phase_rules_sha256"] = digest
            with self.subTest(digest=digest):
                for action in (lambda: inspector.inspect_manifest(data),
                               lambda: inspector.compare_manifests(data, manifest()),
                               lambda: inspector.compare_manifests(manifest(), data)):
                    with self.assertRaises(ERROR):
                        action()

    def test_child_policy_keeps_compilation_write_only(self):
        self.assertTrue(policy.child_action_is_read_only(argparse.Namespace(command="context-inspect")))
        self.assertFalse(policy.child_action_is_read_only(argparse.Namespace(command="context")))
        self.assertFalse(policy.child_action_is_read_only(argparse.Namespace(command="future-unknown")))


class ReportTests(unittest.TestCase):
    def test_debt_precedes_sizes_and_does_not_double_count_truncation(self):
        data = manifest()
        data["selected"][0].update(bytes_selected=75, omitted_bytes=60, selection="head")
        data.update(context_debt=2, missing_required=["src.py", "required.md"],
                    truncated_required=["src.py"], sufficient=False)
        data["omitted"] = [{"path": "required.md", "reason": "not available"}]
        value = inspector.inspect_manifest(data)
        before = deepcopy(value)
        text = reporter.render_report(value)
        self.assertLess(text.index("Required context debt"), text.index("Recorded byte budget"))
        self.assertIn("Required context debt (recorded): 2", text)
        self.assertIn("Truncated required sources (subset above): 1 total", text)
        self.assertIn("Recorded sufficient: no", text)
        self.assertIn("size unreported", text)
        self.assertIn("Current sources: not checked", text)
        self.assertIn("Source payload 75 B; framing 65 B", text)
        self.assertEqual(before, value)

    def test_report_uses_no_io_or_processes(self):
        value = inspector.inspect_manifest(manifest())
        with patch("builtins.open", side_effect=AssertionError("unexpected read")), \
                patch.object(os, "open", side_effect=AssertionError("unexpected descriptor")), \
                patch.object(subprocess, "run", side_effect=AssertionError("unexpected process")):
            text = reporter.render_report(value)
        self.assertIn("no bundle verification or acceptance judgment", text)
        self.assertIn("not the provider's full context", text)
        self.assertIn("Source SHA-256: " + value["sources"]["items"][0]["sha256"], text)

    def test_nested_list_limits_remain_visible(self):
        data = manifest()
        data["selected"] += [source("other.py"), source("third.py", b"z" * 200)]
        data.update(selected_bytes=500, remaining_bytes=500)
        text = reporter.render_report(inspector.inspect_manifest(data, limit=1))
        self.assertIn("Selected sources (largest recorded payload first): 3 total; 1 shown; 2 not shown", text)
        self.assertIn("Paths: 2 total; 1 shown; 1 not shown", text)
        self.assertIn("candidates only", text)
        self.assertIn("does not prove the selected slices duplicate", text)

    def test_unicode_and_terminal_control_labels_are_bounded(self):
        data = manifest()
        path = "자료/검토e\u0301.py"
        data["targets"] = [path]
        data["selected"][0]["path"] = path
        data["selected"][0]["reason"] = "한글\x1b[31m\x07\u0085\u2028\u2029\u202e" + "가" * 400
        value = inspector.inspect_manifest(data)
        before = deepcopy(value)
        text = reporter.render_report(value)
        self.assertIn(path, text)
        for char in ("\x1b", "\x07", "\u0085", "\u2028", "\u2029", "\u202e"):
            self.assertNotIn(char, text)
        self.assertIn("[label shortened]", text)
        self.assertNotIn("가" * 200, text)
        self.assertEqual(before, value)

    def test_empty_pack_is_not_a_current_verification(self):
        data = manifest()
        data.update(selected=[], targets=[], selected_bytes=0, remaining_bytes=1000)
        text = reporter.render_report(inspector.inspect_manifest(data))
        self.assertIn("Used 0 B / 1000 B; remaining 1000 B", text)
        self.assertIn("Recorded sufficient: yes", text)
        self.assertIn("Current sources: not checked", text)
        self.assertIn("0 total; 0 shown; 0 not shown", text)
        self.assertIn("no bundle verification or acceptance judgment", text)

    def test_comparison_direction_mismatch_and_change_fields(self):
        left, right = manifest(), manifest()
        right["selected"][0]["reason"] = "required context"
        right["selected"].append(source("added.py", b"new"))
        right.update(selected_bytes=180, remaining_bytes=820, phase="review", targets=["added.py"],
                     contract_state_digest="c" * 64)
        value = inspector.compare_manifests(left, right)
        before = deepcopy(value)
        text = reporter.render_report(value)
        for phrase in ("right minus left", "different workflow phases", "different target sets",
                       "different recorded contract states", "Used: +40 B", "Remaining: -40 B",
                       "Added selected sources: 1 total", "Fields: reason"):
            self.assertIn(phrase, text)
        reverse = reporter.render_report(inspector.compare_manifests(right, left))
        self.assertIn("Used: -40 B", reverse)
        self.assertIn("Removed selected sources: 1 total", reverse)
        self.assertEqual(before, value)

    def test_equal_metadata_and_missing_identity_keep_qualifications(self):
        data = manifest()
        text = reporter.render_report(inspector.compare_manifests(data, data))
        self.assertIn("Recorded scope: matches; current task/repository identity is not verified", text)
        self.assertIn("Zero deltas do not prove identical packs", text)
        data["contract_state_digest"] = None
        text = reporter.render_report(inspector.compare_manifests(data, data))
        self.assertIn("contract identity unavailable", text)
        self.assertIn("Recorded scope: different or unknown", text)

    def test_comparison_counts_remain_visible_at_limit(self):
        left, right = manifest(), manifest()
        left.update(selected=[source("left.py")], targets=["left.py"])
        right["selected"] += [source("b.py"), source("c.py")]
        right.update(selected_bytes=400, remaining_bytes=600)
        text = reporter.render_report(inspector.compare_manifests(left, right, limit=1))
        self.assertIn("Added selected sources: 3 total; 1 shown; 2 not shown", text)
        self.assertIn("Removed selected sources: 1 total; 1 shown; 0 not shown", text)

    def test_unknown_report_contract_is_refused(self):
        for value in ({"schema": True}, {"schema": 2}, {"schema": 1, "kind": "other"}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                reporter.render_report(value)


class FilesystemTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="oms-inspect-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.file = self.repo / "pack.json"
        self.file.write_text(json.dumps(manifest()), encoding="utf-8")

    def test_inspection_reads_only_explicit_manifest_and_never_writes(self):
        before = tree_bytes(self.repo)
        real_open = os.open
        opened = []
        def record(path, *args, **kwargs):
            opened.append(str(path))
            return real_open(path, *args, **kwargs)
        # supports_dir_fd contains the real function; substitute its membership too.
        with patch.object(inspector.os, "open", record), patch.object(inspector.os, "supports_dir_fd", {record}):
            result = inspector.inspect_files(self.repo, "pack.json")
        self.assertEqual(opened, [str(self.repo), "pack.json"])
        self.assertEqual(result["sources"]["total"], 1)
        self.assertEqual(before, tree_bytes(self.repo))
        self.assertEqual(inspector.inspect_files(self.repo, self.file, compare="pack.json")["delta"]["selected_bytes"], 0)

    def test_verify_current_counts_and_bounds_safe_sources(self):
        data = manifest()
        data["targets"] = []
        data["selected"] = [source(name, b"") for name in
                            ("same", "at-limit", "changed", "missing", "escape/file", "fifo", "large")]
        data.update(selected_bytes=0, remaining_bytes=1000)
        self.file.write_text(json.dumps(data))
        (self.repo / "same").write_bytes(b"")
        (self.repo / "at-limit").write_bytes(b"xxxx")
        (self.repo / "changed").write_bytes(b"x")
        (self.root / "file").write_bytes(b"outside")
        (self.repo / "escape").symlink_to(self.root, target_is_directory=True)
        os.mkfifo(self.repo / "fifo")
        (self.repo / "large").write_bytes(b"x" * 5)
        with patch.object(inspector, "MAX_SOURCE_BYTES", 4):
            value = inspector.inspect_files(self.repo, "pack.json", verify_current=True, limit=1)
        current = value["sources_current"]
        self.assertEqual([current[k] for k in ("unchanged", "changed", "missing", "unreadable")],
                         [1, 2, 1, 3])
        self.assertEqual(current["changed_paths"], {"total": 2, "items": ["at-limit"], "omitted": 1})
        self.assertEqual(current["missing_paths"]["items"], ["missing"])
        self.assertNotIn("no source or bundle files were read", value["note"])
        text = reporter.render_report(value)
        self.assertIn("7 selected; 1 unchanged; 2 changed; 1 missing; 3 unreadable", text)
        self.assertIn("Changed source paths: 2 total; 1 shown; 1 not shown", text)
        self.assertNotIn("Current sources: not checked", text)
        self.assertIn("not an atomic snapshot", text)
        with self.assertRaises(ERROR):
            inspector.inspect_files(self.repo, "pack.json", compare="pack.json", verify_current=True)

    def test_links_traversal_and_special_files_are_rejected(self):
        (self.repo / "symlink.json").symlink_to(self.file)
        os.link(self.file, self.repo / "hardlink.json")
        (self.root / "outside.json").write_text(self.file.read_text())
        (self.repo / "outside-dir").symlink_to(self.root, target_is_directory=True)
        os.mkfifo(self.repo / "fifo.json")
        for path in ("symlink.json", "hardlink.json", "../outside.json", "outside-dir/outside.json",
                     self.root / "outside.json", "fifo.json", ".", "missing.json"):
            with self.subTest(path=path), self.assertRaises(ERROR):
                inspector.inspect_files(self.repo, path)

    def test_frozen_raw_manifest_contracts(self):
        for case, arguments in contract_cases(self, "context-read-v1.json", "read_manifest"):
            with self.subTest(case=case["name"]):
                self.file.write_bytes(bytes.fromhex(case["input_hex"]))
                check_result(self, case, lambda: inspector.read_manifest(self.repo, "pack.json", **arguments))

    def test_deeply_nested_and_oversize_json_is_rejected(self):
        for raw in (b'[' * 2000, b' ' * (inspector.MAX_MANIFEST_BYTES + 1)):
            self.file.write_bytes(raw)
            with self.subTest(length=len(raw)), self.assertRaises(ERROR):
                inspector.inspect_files(self.repo, "pack.json")

    def test_manifest_changed_during_read_is_not_accepted(self):
        real_read = os.read
        changed = False
        def read_and_change(fd, count):
            nonlocal changed
            value = real_read(fd, count)
            if not changed:
                changed = True
                with self.file.open("ab") as handle:
                    handle.write(b" ")
            return value
        with patch.object(inspector.os, "read", read_and_change), self.assertRaises(ERROR):
            inspector.inspect_files(self.repo, "pack.json")

    def test_injection_ledger_is_summed_by_hook_and_session_without_text(self):
        hooks = self.repo / ".oms" / "hooks"
        hooks.mkdir(parents=True)
        self.assertEqual(session_view.inspect_injections(self.repo)["injections"], 0)
        rows = [{"action": "injection", "status": "resume", "bytes": 900, "lines": 9, "session": "s1", "source": "startup"},
                {"action": "injection", "status": "resume", "bytes": 1100, "lines": 11, "session": "s1", "source": "compact"},
                {"action": "injection", "status": "prompt", "bytes": 40, "lines": 1, "session": "s2"},
                {"action": "telemetry", "bytes": 5000, "session": "s2"},
                {"action": "injection", "status": "resume", "bytes": "many", "session": "s2"},
                {"action": "injection", "status": "private text\u0007", "bytes": 3, "session": "s2"}]
        (hooks / "events.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows) + "{broken\n", encoding="utf-8")
        before = tree_bytes(self.repo)
        value = session_view.inspect_injections(self.repo, limit=1)
        self.assertEqual((value["injections"], value["bytes"], value["invalid_rows"]), (3, 2040, 3))
        self.assertEqual(value["by_hook"], {"total": 2, "omitted": 1, "items": [
            {"hook": "resume", "calls": 2, "bytes": 2000, "largest": 1100}]})
        self.assertEqual(value["by_session"]["items"], [{"session": "s1", "calls": 2, "bytes": 2000, "largest": 1100}])
        sources = session_view.inspect_injections(self.repo)["by_session_start_source"]["items"]
        self.assertEqual({row["source"]: row["bytes"] for row in sources},
                         {"compact": 1100, "startup": 900, "unrecorded": 40})
        self.assertEqual(session_view.inspect_injections(self.repo, session="s2")["bytes"], 40)
        self.assertNotIn("private text", json.dumps(value))
        self.assertTrue(value["not_counted"])
        self.assertEqual(before, tree_bytes(self.repo))
        for bad in (0, 101, "3"):
            with self.assertRaises(session_view.InspectionError):
                session_view.inspect_injections(self.repo, limit=bad)
        with self.assertRaises(session_view.InspectionError):
            session_view.inspect_injections(self.repo, session="not a hash\n")
        (hooks / "events.jsonl").unlink()
        (hooks / "events.jsonl").symlink_to(self.file)
        with self.assertRaises(session_view.InspectionError):
            session_view.inspect_injections(self.repo)

    def test_transcript_tool_results_are_counted_in_bytes_without_text(self):
        inside, outside = str(self.repo / "src" / "a.py"), str(self.root / "elsewhere.txt")
        def use(ident, name, **fields):
            return {"type": "assistant", "message": {"content": [
                {"type": "text", "text": "private reasoning"}, {"type": "tool_use", "id": ident, "name": name, "input": fields}]}}
        def result(ident, content):
            return {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": ident, "content": content}]}}
        rows = [use("t1", "Read", file_path=inside), result("t1", "x" * 300),
                use("t2", "Read", file_path="src/a.py"), result("t2", [{"type": "text", "text": "y" * 200}]),
                use("t3", "Read", file_path=outside), result("t3", "z" * 10),
                use("t4", "Bash", command="echo private-command"), result("t4", "\u00e9" * 50),
                result("gone", "w" * 7), {"type": "user", "message": {"content": "plain prompt"}}]
        transcript = self.root / "session.jsonl"
        transcript.write_text("".join(json.dumps(row) + "\n" for row in rows) + "not json\n", encoding="utf-8")
        before = tree_bytes(self.repo)
        value = session_view.inspect_session(self.repo, str(transcript), limit=2)
        self.assertEqual(value["format"], "claude-jsonl")
        self.assertEqual((value["tool_calls"], value["tool_results"], value["unmatched_results"], value["unread_lines"]),
                         (4, 5, 1, 1))
        self.assertEqual(value["result_bytes"], 300 + 200 + 10 + 100 + 7)
        self.assertEqual(value["by_tool"]["items"], [
            {"tool": "Read", "results": 3, "bytes": 510, "largest": 300},
            {"tool": "Bash", "results": 1, "bytes": 100, "largest": 100}])
        self.assertEqual(value["by_tool"]["omitted"], 1)
        self.assertEqual(value["file_reads"], {"distinct_in_repository": 1, "outside_repository": 1,
                                               "repeated": {"total": 1, "omitted": 0, "items": [{"path": "src/a.py", "reads": 2}]}})
        text = json.dumps(value)
        for private in ("private", "elsewhere", str(self.root)):
            self.assertNotIn(private, text)
        self.assertEqual(before, tree_bytes(self.repo))
        other = self.root / "other.jsonl"
        other.write_text('{"type":"session_meta","payload":{"cwd":"x"}}\n', encoding="utf-8")
        self.assertEqual(session_view.inspect_session(self.repo, str(other))["format"], "unknown")
        with self.assertRaises(session_view.InspectionError) as caught:
            session_view.inspect_session(self.repo, str(self.root / "absent.jsonl"))
        self.assertNotIn(str(self.root), str(caught.exception))
        with patch.object(session_view, "MAX_TRANSCRIPT_BYTES", 8), self.assertRaises(session_view.InspectionError):
            session_view.inspect_session(self.repo, str(transcript))
        self.assertTrue(policy.child_action_is_read_only(argparse.Namespace(command="context-inspect", session=None)))
        self.assertFalse(policy.child_action_is_read_only(
            argparse.Namespace(command="context-inspect", session=str(transcript))))

    def test_transcript_timeline_orders_calls_and_flags_first_error_without_content(self):
        def use(ident, name, second, **fields):
            return {"type": "assistant", "timestamp": "2026-11-11T10:00:%02dZ" % second, "message": {"content": [
                {"type": "tool_use", "id": ident, "name": name, "input": fields}]}}
        def result(ident, second, content, **extra):
            return {"type": "user", "timestamp": "2026-11-11T10:00:%02dZ" % second, "message": {"content": [
                dict({"type": "tool_result", "tool_use_id": ident, "content": content}, **extra)]}}
        rows = [use("a", "Read", 0, file_path="private-path"), result("a", 1, "x" * 30),
                use("b", "Bash", 2, command="private-command"), result("b", 5, "boom-text", is_error=True),
                use("c", "Bash", 6, command="private-command"), result("c", 7, "ok"),
                use("d", "Edit", 8, old="private-edit"), result("gone", 9, "w")]
        transcript = self.root / "timeline.jsonl"
        transcript.write_text("".join(json.dumps(row) + "\n" for row in rows) + "not json\n", encoding="utf-8")
        before = tree_bytes(self.repo)
        value = session_view.inspect_timeline(self.repo, str(transcript))
        self.assertEqual([(s["step"], s["tool"], s["ok"], s["duration_ms"]) for s in value["steps"]],
                         [(1, "Read", True, 1000), (2, "Bash", False, 3000), (3, "Bash", True, 1000), (4, "Edit", None, None)])
        self.assertEqual(value["first_error"]["step"], 2)
        self.assertEqual((value["errors"], value["pending"], value["steps_after_first_error"],
                          value["unmatched_results"], value["unread_lines"]), (1, 1, 2, 1, 1))
        self.assertEqual(value["steps"][2]["repeat_of"], 2)
        self.assertEqual(value["steps"][0]["result_bytes"], 30)
        text = json.dumps(value)
        for private in ("private", "boom-text", str(self.root)):
            self.assertNotIn(private, text)
        self.assertEqual(before, tree_bytes(self.repo))
        # A late first error stays visible when the row cap would cut it.
        capped = session_view.inspect_timeline(self.repo, str(transcript), rows=2)
        self.assertEqual([s["step"] for s in capped["steps"]], [1, 2])
        with patch.object(session_view, "MAX_STEPS", 1):
            self.assertEqual(session_view.inspect_timeline(self.repo, str(transcript))["steps_not_recorded"], 3)
        with self.assertRaises(session_view.InspectionError):
            session_view.inspect_timeline(self.repo, str(transcript), rows=0)

    def test_error_does_not_echo_input_or_path(self):
        self.file.write_text('{"private-fixture-value":', encoding="utf-8")
        with self.assertRaises(ERROR) as caught:
            inspector.inspect_files(self.repo, "pack.json")
        self.assertNotIn("private-fixture-value", str(caught.exception))
        self.assertNotIn(str(self.root), str(caught.exception))


if CLI:
    class RuntimeIntegrationTests(unittest.TestCase):
        def test_real_compiler_inspector_compare_and_child_boundary(self):
            with tempfile.TemporaryDirectory(prefix="oms-inspect-cli-") as folder:
                repo = Path(folder)
                subprocess.run(["git", "init", "-q", str(repo)], check=True)
                (repo / "src.py").write_text("print('fixture')\n", encoding="utf-8")
                subprocess.run(["git", "-C", str(repo), "add", "src.py"], check=True)
                subprocess.run(["git", "-C", str(repo), "-c", "user.name=Fixture",
                                "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
                script = ROOT / "scripts/runtime.sh"
                env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
                env.pop("OMS_HARNESS_CHILD", None)
                def call(*args, child=False):
                    environment = dict(env, **({"OMS_HARNESS_CHILD": "1"} if child else {}))
                    compare = CANDIDATE is not None and args[0] == "context-inspect"
                    before_call = tree_bytes(repo) if compare else None
                    command = (["bash", str(ROOT / "scripts/oms"), "runtime"] if compare
                               else ["bash", str(script)])
                    reference = subprocess.run([*command, "--repo", str(repo), *args],
                                               env=environment, stdout=subprocess.PIPE,
                                               stderr=subprocess.PIPE, text=True, timeout=90)
                    if not compare:
                        return reference
                    self.assertEqual(tree_bytes(repo), before_call, "reference inspection wrote fixture state")
                    candidate = subprocess.run([CANDIDATE, "runtime", "--repo", str(repo), *args],
                                               env=environment, stdout=subprocess.PIPE,
                                               stderr=subprocess.PIPE, text=True, timeout=90)
                    self.assertEqual(tree_bytes(repo), before_call, "candidate inspection wrote fixture state")
                    self.assertEqual(candidate.returncode, reference.returncode, (args, candidate.stderr))
                    self.assertEqual(candidate.stdout, reference.stdout, (args, "stdout differs"))
                    self.assertEqual(candidate.stderr, reference.stderr, (args, "stderr differs"))
                    return candidate
                compiled = call("context", "--target", "src.py", "--manifest", "pack.json", "--bundle", "pack.txt")
                self.assertEqual(compiled.returncode, 0, compiled.stderr)
                (repo / "src.py").unlink()
                (repo / "pack.txt").unlink()
                before = tree_bytes(repo)
                one = call("context-inspect", "pack.json", "--pretty", child=True)
                self.assertEqual(one.returncode, 0, one.stderr)
                value = json.loads(one.stdout)
                self.assertIsNone(value["sources_current"])
                self.assertFalse(value["bundle_verified"])
                verified = call("context-inspect", "pack.json", "--verify-current", child=True)
                self.assertEqual(verified.returncode, 0, verified.stderr)
                self.assertEqual(json.loads(verified.stdout)["sources_current"]["missing"], 1)
                conflict = call("context-inspect", "pack.json", "--compare", "pack.json", "--verify-current")
                self.assertEqual(conflict.returncode, 2)
                self.assertIn("cannot be combined", conflict.stderr)
                pair = call("context-inspect", "pack.json", "--compare", "pack.json", "--limit", "1")
                self.assertEqual(pair.returncode, 0, pair.stderr)
                self.assertEqual(json.loads(pair.stdout)["delta"]["selected_bytes"], 0)
                explicit_json = call("context-inspect", "pack.json", "--format", "json")
                self.assertEqual(explicit_json.returncode, 0, explicit_json.stderr)
                self.assertEqual(json.loads(explicit_json.stdout), value)
                text = call("context-inspect", "pack.json", "--format", "text", child=True)
                self.assertEqual(text.returncode, 0, text.stderr)
                self.assertIn("OMS context inspection", text.stdout)
                self.assertIn("Current sources: not checked", text.stdout)
                current_text = call("context-inspect", "pack.json", "--format", "text", "--verify-current")
                self.assertEqual(current_text.returncode, 0, current_text.stderr)
                self.assertIn("1 missing", current_text.stdout)
                compare_text = call("context-inspect", "pack.json", "--compare", "pack.json", "--format", "text")
                self.assertEqual(compare_text.returncode, 0, compare_text.stderr)
                self.assertIn("right minus left", compare_text.stdout)
                self.assertIn("Used: +0 B", compare_text.stdout)
                # Invalid combinations fail before any manifest is read.
                bad = call("context-inspect", "absent.json", "--format", "text", "--pretty")
                self.assertEqual(bad.returncode, 2)
                self.assertIn("--pretty applies only to JSON", bad.stderr)
                self.assertEqual(bad.stdout, "")
                self.assertEqual(call("context-inspect", "pack.json", "--format", "unknown").returncode, 2)
                bad = call("context-inspect", "pack.json", "--compare", "pack.json", "--verify-current", "--format", "text")
                self.assertEqual(bad.returncode, 2)
                self.assertEqual(bad.stdout, "")
                self.assertEqual(call("context-inspect", "pack.json", "--limit", "0").returncode, 2)
                # The same verb reads the injection ledger or one transcript instead of a manifest; exactly one of the three.
                ledger = call("context-inspect", "--injections", child=True)
                self.assertEqual(ledger.returncode, 0, ledger.stderr)
                self.assertEqual(json.loads(ledger.stdout)["kind"], "oms-context-injections")
                transcript = repo / "t.jsonl"
                transcript.write_text(json.dumps({"type": "user", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t", "content": "abc"}]}}) + "\n", encoding="utf-8")
                seen = call("context-inspect", "--session", str(transcript))
                self.assertEqual(seen.returncode, 0, seen.stderr)
                self.assertEqual(json.loads(seen.stdout)["result_bytes"], 3)
                self.assertEqual(call("context-inspect", "--session", str(transcript), child=True).returncode, 2)
                line = call("context-inspect", "--session", str(transcript), "--timeline")
                self.assertEqual(line.returncode, 0, line.stderr)
                self.assertEqual(json.loads(line.stdout)["kind"], "oms-context-timeline")
                self.assertEqual(call("context-inspect", "--session", str(transcript), "--timeline", child=True).returncode, 2)
                self.assertEqual(call("context-inspect", "--injections", "--timeline").returncode, 2)
                for wrong in (("context-inspect",), ("context-inspect", "pack.json", "--injections"),
                              ("context-inspect", "--injections", "--verify-current"),
                              ("context-inspect", "pack.json", "--session-hash", "abc"),
                              ("context-inspect", "--injections", "--format", "text")):
                    self.assertEqual(call(*wrong).returncode, 2, wrong)
                transcript.unlink()
                self.assertEqual(call("context", "--target", "src.py", child=True).returncode, 2)
                self.assertEqual(before, tree_bytes(repo))
                recorded = json.loads((repo / "pack.json").read_text())
                recorded["phase_rules_sha256"] = "c" * 64
                (repo / "policy-left.json").write_text(json.dumps(recorded))
                recorded["phase_rules_sha256"] = "d" * 64
                (repo / "policy-right.json").write_text(json.dumps(recorded))
                before_policy = tree_bytes(repo)
                mismatch = call("context-inspect", "policy-left.json", "--compare", "policy-right.json", child=True)
                self.assertEqual(mismatch.returncode, 0, mismatch.stderr)
                self.assertIn("different-phase-rules", json.loads(mismatch.stdout)["scope_mismatch_reasons"])
                self.assertFalse(json.loads(mismatch.stdout)["recorded_scope_matches"])
                self.assertEqual(before_policy, tree_bytes(repo))


if __name__ == "__main__":
    unittest.main(verbosity=2)
