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
                pair = call("context-inspect", "pack.json", "--compare", "pack.json", "--limit", "1")
                self.assertEqual(pair.returncode, 0, pair.stderr)
                self.assertEqual(json.loads(pair.stdout)["delta"]["selected_bytes"], 0)
                self.assertEqual(call("context-inspect", "pack.json", "--limit", "0").returncode, 2)
                self.assertEqual(call("context", "--target", "src.py", child=True).returncode, 2)
                self.assertEqual(before, tree_bytes(repo))


if __name__ == "__main__":
    unittest.main(verbosity=2)
