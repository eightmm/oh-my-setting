"""Context manifest inspector contracts; --cli also exercises the canonical runtime."""
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


def load(name):
    spec = importlib.util.spec_from_file_location(name, LIB / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


inspector = load("context_inspect")
policy = load("child_policy")
sys.path.insert(0, str(ROOT / "scripts/lib"))
from oms_runtime import context_session as session_view  # noqa: E402  (a package import: it shares the inspector's errors)
ERROR = inspector.InspectionError
CLI = "--cli" in sys.argv
if CLI:
    sys.argv.remove("--cli")


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
    def test_budget_is_bytes_not_tokens_or_proof(self):
        value = inspector.inspect_manifest(manifest())
        self.assertEqual(value["budget"], {"bytes": 1000, "used_bytes": 140,
                         "remaining_bytes": 860, "source_payload_bytes": 100, "framing_bytes": 40})
        self.assertEqual(value["validation"], "recorded-metadata-only")
        self.assertIsNone(value["sources_current"])
        self.assertFalse(value["bundle_verified"])
        self.assertNotIn("tokens", value)
        self.assertNotIn("winner", value)

    def test_partial_source_and_required_debt_are_preserved(self):
        data = manifest()
        data["selected"][0].update(bytes_selected=75, omitted_bytes=60, selection="head")
        data.update(context_debt=2, missing_required=["src.py", "required.md"],
                    truncated_required=["src.py"], sufficient=False)
        data["omitted"] = [{"path": "required.md", "reason": "budget exhausted", "bytes": 300}]
        value = inspector.inspect_manifest(data)
        self.assertEqual(value["debt"]["reported"], 2)
        self.assertFalse(value["debt"]["reported_sufficient"])
        self.assertEqual(value["sources"]["items"][0]["bytes_selected"], 75)
        self.assertEqual(value["omissions"]["items"][0]["reason"], "budget exhausted")

    def test_zero_length_source_and_empty_pack(self):
        data = manifest()
        data.update(selected=[], targets=[], selected_bytes=0, remaining_bytes=1000)
        self.assertEqual(inspector.inspect_manifest(data)["sources"]["total"], 0)
        data.update(selected=[source(data=b"")], targets=["src.py"], selected_bytes=40, remaining_bytes=960)
        self.assertEqual(inspector.inspect_manifest(data)["repeated_source_digests"]["total"], 0)

    def test_duplicate_hashes_are_candidates_and_lists_are_bounded(self):
        data = manifest()
        data["selected"] += [source("other.py"), source("third.py", b"z" * 200)]
        data.update(selected_bytes=500, remaining_bytes=500)
        value = inspector.inspect_manifest(data, limit=1)
        self.assertEqual(value["sources"]["items"][0]["path"], "third.py")
        self.assertEqual(value["sources"]["omitted"], 2)
        group = value["repeated_source_digests"]["items"][0]
        self.assertEqual(group["paths"]["total"], 2)
        self.assertEqual(group["paths"]["omitted"], 1)
        self.assertNotIn("saved_tokens", value)

    def test_untrusted_display_strings_are_not_terminal_controls(self):
        data = manifest()
        data["selected"][0]["reason"] = "reason\x1b[31m\n\u202e hidden"
        value = inspector.inspect_manifest(data)
        text = value["sources"]["items"][0]["reason"]
        self.assertNotIn("\x1b", text)
        self.assertNotIn("\u202e", text)
        self.assertNotIn("\n", text)

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

    def test_compare_changes_preserves_scope_mismatches(self):
        left, right = manifest(), manifest()
        right["selected"][0]["reason"] = "required context"
        right["selected"].append(source("added.py", b"new"))
        right.update(selected_bytes=180, remaining_bytes=820, phase="review", targets=["added.py"],
                     contract_state_digest="c" * 64)
        value = inspector.compare_manifests(left, right)
        self.assertFalse(value["recorded_scope_matches"])
        self.assertEqual(set(value["scope_mismatch_reasons"]),
                         {"different-phase", "different-targets", "different-contract-state"})
        self.assertEqual(value["added"]["items"], ["added.py"])
        self.assertEqual(value["changed"]["items"], [{"path": "src.py", "changed_fields": ["reason"]}])
        self.assertEqual(value["delta"]["selected_bytes"], 40)
        self.assertNotIn("winner", value)
        self.assertEqual(inspector.compare_manifests(right, left)["removed"]["items"], ["added.py"])

    def test_unknown_contract_and_identical_pack(self):
        data = manifest()
        self.assertTrue(inspector.compare_manifests(data, data)["recorded_scope_matches"])
        data["contract_state_digest"] = None
        value = inspector.compare_manifests(data, data)
        self.assertFalse(value["recorded_scope_matches"])
        self.assertIn("contract-identity-unavailable", value["scope_mismatch_reasons"])

    def test_child_policy_keeps_compilation_write_only(self):
        self.assertTrue(policy.child_action_is_read_only(argparse.Namespace(command="context-inspect")))
        self.assertFalse(policy.child_action_is_read_only(argparse.Namespace(command="context")))
        self.assertFalse(policy.child_action_is_read_only(argparse.Namespace(command="future-unknown")))


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

    def test_invalid_json_nonfinite_duplicates_and_oversize(self):
        for raw in (b'{"schema":1,"schema":1}', b'{"x":NaN}', b'{"x":Infinity}',
                    b'\xff', b'{invalid}', b'[' * 2000, b' ' * (inspector.MAX_MANIFEST_BYTES + 1)):
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
                    return subprocess.run(["bash", str(script), "--repo", str(repo), *args],
                                          env=dict(env, **({"OMS_HARNESS_CHILD": "1"} if child else {})),
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=90)
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
                for wrong in (("context-inspect",), ("context-inspect", "pack.json", "--injections"),
                              ("context-inspect", "--injections", "--verify-current"),
                              ("context-inspect", "pack.json", "--session-hash", "abc")):
                    self.assertEqual(call(*wrong).returncode, 2, wrong)
                transcript.unlink()
                self.assertEqual(call("context", "--target", "src.py", child=True).returncode, 2)
                self.assertEqual(before, tree_bytes(repo))


if __name__ == "__main__":
    unittest.main(verbosity=2)
