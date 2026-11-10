"""Public-entrypoint fixtures for previously untested native session exploration.

Every native CLI is a local fake. These tests never start a paid provider.
Existing room/navigation/checkpoint suites remain authoritative for their own APIs.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

ROOT = Path(__file__).resolve().parents[1]
ENTRY = ROOT / "scripts" / "session.sh"
sys.path.insert(0, str(ROOT / "scripts" / "lib"))


FAKE = r'''#!/usr/bin/env python3
import json, os, pathlib, sys, uuid
p = pathlib.Path(sys.argv[0]).name
a = sys.argv[1:]
if "--help" in a:
    if os.environ.get("OMS_FAKE_NO_CAPABILITY"):
        print("Usage: " + p)
    elif p == "codex":
        print("Commands:\n  fork Fork an interactive session\n  resume Resume a session\nOptions:\n  --cd DIR Working directory")
    elif p == "claude":
        print("Options:\n  --resume ID Resume session\n  --fork-session Fork session\n  --session-id ID Exact new ID")
    else:
        print("Options:\n  --fork PATH Fork session\n  --session PATH Resume session")
    sys.exit(0)
with open(os.environ["OMS_FAKE_LOG"], "a") as f:
    f.write(json.dumps({"provider": p, "argv": a, "cwd": os.getcwd(),
                       "room": os.environ.get("OMS_ROOM_ID"),
                       "attempt": os.environ.get("OMS_AGENT_ATTEMPT_ID"),
                       "policy": os.environ.get("OMS_FAKE_POLICY"),
                       "inherited_authority": {key: os.environ.get(key) for key in (
                           "OMS_PANEL_MAIN_ATTEMPT", "OMS_PANEL_REPO", "OMS_PANEL_SESSION", "OMS_STATE_REPO",
                           "OMS_PLAN_ID", "OMS_LEASE_ID", "OMS_PLAN_LEASE_ID", "OMS_APPROVAL_ID", "OMS_LANDING_ID",
                           "OMS_EXECUTOR_ID", "OMS_HARNESS_CALL_ID", "OMS_INDEX_BASE_REPO", "DELEGATE_SOURCE_REPO")}}) + "\n")
if os.environ.get("OMS_FAKE_HOLD_SECONDS"):
    import time
    time.sleep(float(os.environ["OMS_FAKE_HOLD_SECONDS"]))
fork = "--fork-session" in a or "--fork" in a or "fork" in a
if fork:
    sid = a[a.index("--session-id") + 1] if "--session-id" in a else str(uuid.uuid4())
    if p == "codex":
        folder = pathlib.Path(os.environ["CODEX_HOME"]) / "sessions" / "2026" / "11" / "10"
        rows = [{"type": "session_meta", "payload": {"id": sid, "cwd": os.getcwd(), "forked_from_id": a[1]}}]
    elif p == "claude":
        folder = pathlib.Path(os.environ["CLAUDE_CONFIG_DIR"]) / "projects" / "fork"
        rows = [{"type": "user", "sessionId": sid, "cwd": os.getcwd(), "message": {"role": "user", "content": "fork child"}}]
    else:
        folder = pathlib.Path(os.environ["PI_CODING_AGENT_DIR"]) / "sessions" / "fork"
        rows = [{"type": "session", "id": sid, "cwd": os.getcwd(), "parentSession": a[a.index("--fork") + 1]}]
    folder.mkdir(parents=True, exist_ok=True)
    (folder / (sid + ".jsonl")).write_text("".join(json.dumps(r) + "\n" for r in rows))
sys.exit(int(os.environ.get("OMS_FAKE_NATIVE_EXIT", "0")))
'''


class SessionsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="oms-session-test.")
        self.base = Path(self.temp.name).resolve()
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.home = self.base / "home"
        self.home.mkdir()
        self.bin = self.base / "bin"
        self.bin.mkdir()
        for provider in ("claude", "codex", "pi"):
            executable = self.bin / provider
            executable.write_text(FAKE)
            executable.chmod(0o700)
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        CODEX_HOME=str(self.home / "codex"), CLAUDE_CONFIG_DIR=str(self.home / "claude"),
                        PI_CODING_AGENT_DIR=str(self.home / "pi"), OMS_LOCK_DIR=str(self.base / "locks"),
                        OMS_DELEGATE_WORKTREE_ROOT=str(self.base / "worktrees"),
                        OMS_FAKE_LOG=str(self.base / "native.jsonl"), OMS_FAKE_POLICY="preserved",
                        OMS_HARNESS_CHILD="0", OMS_HARNESS_DELEGATE_DEPTH="0", PYTHONDONTWRITEBYTECODE="1")
        for key in ("PI_CODING_AGENT_SESSION_DIR", "OMS_ROOM_REPO", "OMS_ROOM_ID", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            self.env.pop(key, None)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Session Fixture")
        self.git("config", "user.email", "session@example.invalid")
        (self.repo / "tracked.txt").write_text("base\n")
        (self.repo / "delete.txt").write_text("delete me\n")
        (self.repo / ".gitignore").write_text("ignored.dat\n")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.sids = {"claude": "11111111-1111-4111-8111-111111111111", "codex": "22222222-2222-4222-8222-222222222222",
                     "pi": "33333333-3333-4333-8333-333333333333"}
        self.paths = {provider: self.transcript(provider, self.sids[provider]) for provider in self.sids}

    def tearDown(self):
        self.temp.cleanup()

    def git(self, *args, cwd=None, check=True):
        result = subprocess.run(["git", "-C", str(cwd or self.repo), *args], env=self.env,
                                capture_output=True, check=check)
        return result.stdout

    def cli(self, *args, env=None, ok=True):
        process = subprocess.run(["bash", str(ENTRY), "--repo", str(self.repo), *map(str, args)],
                                 cwd=self.repo, env=dict(self.env, **(env or {})), capture_output=True, text=True)
        if ok:
            self.assertEqual(process.returncode, 0, process.stderr + process.stdout)
            return json.loads(process.stdout)
        self.assertNotEqual(process.returncode, 0, process.stdout)
        return process

    def transcript(self, provider, sid, repo=None, text="find this exact phrase", folder=None):
        cwd = str(repo or self.repo)
        if provider == "claude":
            folder = folder or Path(self.env["CLAUDE_CONFIG_DIR"]) / "projects" / "repo"
            rows = [{"type": "user", "sessionId": sid, "cwd": cwd,
                     "message": {"role": "user", "content": [{"type": "text", "text": text}]}}]
        elif provider == "codex":
            folder = folder or Path(self.env["CODEX_HOME"]) / "sessions" / "2026" / "11" / "10"
            rows = [{"type": "session_meta", "payload": {"id": sid, "cwd": cwd}},
                    {"type": "response_item", "payload": {"type": "message", "role": "assistant",
                     "content": [{"type": "output_text", "text": text}]}}]
        else:
            folder = folder or Path(self.env["PI_CODING_AGENT_DIR"]) / "sessions" / "repo"
            rows = [{"type": "session", "id": sid, "cwd": cwd},
                    {"type": "message", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}]
        folder.mkdir(parents=True, exist_ok=True)
        path = folder / (sid + ".jsonl")
        path.write_text("".join(json.dumps(row) + "\n" for row in rows))
        return path

    def source_args(self, provider="codex"):
        return ["--provider", provider, "--session", self.sids[provider]]

    def prepared(self, provider="codex"):
        return self.cli("fork", *self.source_args(provider), "--apply", "--no-launch")

    def native_calls(self):
        path = Path(self.env["OMS_FAKE_LOG"])
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def dirty(self):
        (self.repo / "tracked.txt").write_text("staged\n")
        self.git("add", "tracked.txt")
        (self.repo / "tracked.txt").write_text("unstaged\n")
        (self.repo / "delete.txt").unlink()
        (self.repo / "new.txt").write_text("untracked\n")
        (self.repo / "ignored.dat").write_text("ignored dataset\n")
        (self.repo / "link").symlink_to(self.base / "outside-secret")

    def test_scoped_cross_provider_search_exact_origins_and_redaction(self):
        foreign = self.base / "foreign"
        foreign.mkdir()
        self.transcript("claude", "foreign", foreign, "exact phrase foreign secret")
        self.transcript("pi", "sensitive", text="exact phrase api_key=TOP_SECRET")
        with self.paths["claude"].open("a") as handle:
            handle.write(json.dumps({"type": "user", "cwd": str(self.repo), "sessionId": self.sids["claude"],
                                    "message": {"role": "user", "content": [{"type": "tool_result", "content": "exact phrase TOOL_SECRET"}]}}) + "\n")
        report = self.cli("search", "exact phrase")
        self.assertEqual(len(report["matches"]), 4)
        serialized = json.dumps(report)
        self.assertNotIn("foreign secret", serialized)
        self.assertNotIn("TOP_SECRET", serialized)
        self.assertNotIn("TOOL_SECRET", serialized)
        rows = {row["provider"]: row for row in report["matches"] if not row["redacted"]}
        self.assertEqual(set(rows), set(self.sids))
        for provider, row in rows.items():
            self.assertEqual(row["path"], str(self.paths[provider]))
            self.assertEqual(row["session_id"], self.sids[provider])
            self.assertEqual(row["excerpt"], "exact phrase")
            self.assertEqual(row["excerpt_start"], 10)
            self.assertEqual(row["match_end"], 22)
            self.assertEqual(row["line"], 1 if provider == "claude" else 2)
        self.assertFalse((self.repo / ".oms").exists())
        self.assertEqual(self.native_calls(), [])

    def test_bookmarks_store_origins_without_text_and_detect_stale_lines(self):
        row = self.cli("bookmark", "add", *self.source_args("claude"), "--line", "1", "--label", "chosen turn")
        path = self.repo / ".oms" / "sessions" / "bookmarks" / (row["id"] + ".json")
        self.assertNotIn("find this exact phrase", path.read_text())
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        source = self.paths["claude"]
        with source.open("a") as handle:
            handle.write("{}\n")
        opened = self.cli("bookmark", "open", row["id"])
        self.assertEqual(opened["origin"]["excerpt"], "find this exact phrase")
        source.write_text(source.read_text().replace("exact phrase", "changed turn"))
        failed = self.cli("bookmark", "open", row["id"], ok=False)
        self.assertIn("stale", failed.stderr)
        self.cli("bookmark", "add", *self.source_args("claude"), "--line", 1,
                 "--label", "api_key=do-not-save", ok=False)

    def test_dry_run_and_missing_native_capabilities_do_not_create_state(self):
        report = self.cli("fork", *self.source_args())
        self.assertTrue(report["dry_run"])
        self.assertFalse((self.repo / ".oms").exists())
        self.assertFalse((self.base / "worktrees").exists())
        self.cli("fork", *self.source_args(), "--apply", env={"OMS_FAKE_NO_CAPABILITY": "1"}, ok=False)
        self.assertFalse((self.repo / ".oms").exists())
        self.assertEqual(self.native_calls(), [])

    def test_custom_roots_do_not_change_id_based_launch_authority(self):
        for provider in ("codex", "claude"):
            folder = self.base / ("backup-" + provider)
            self.transcript(provider, "backup-session", folder=folder)
            args = ["--provider", provider, "--session", "backup-session", "--root", str(folder)]
            opened = self.cli("open", *args)
            self.assertEqual(opened["session_id"], "backup-session")
            refused = self.cli("resume", *args, "--apply", ok=False)
            self.assertIn("outside the configured native store", refused.stderr)
        self.assertEqual(self.native_calls(), [])
        self.assertFalse((self.repo / ".oms").exists())

    def test_fork_preserves_staged_unstaged_bounded_untracked_and_source(self):
        self.dirty()
        large = self.repo / "large.bin"
        with large.open("wb") as handle:
            handle.truncate(8 * 1024 * 1024 + 1)
        before = (self.git("diff", "--binary"), self.git("diff", "--cached", "--binary"), self.git("status", "--porcelain"))
        transcript = self.paths["codex"].read_bytes()
        row = self.prepared()
        target = Path(row["worktree"])
        self.assertEqual(self.git("diff", "--binary", cwd=target), before[0])
        self.assertEqual(self.git("diff", "--cached", "--binary", cwd=target), before[1])
        self.assertEqual((target / "new.txt").read_text(), "untracked\n")
        for name in ("large.bin", "link", "ignored.dat"):
            self.assertFalse((target / name).exists())
        self.assertEqual(row["files"]["untracked_omitted"], 2)
        self.assertEqual((self.git("diff", "--binary"), self.git("diff", "--cached", "--binary"),
                          self.git("status", "--porcelain")), before)
        self.assertEqual(self.paths["codex"].read_bytes(), transcript)
        self.assertIn(b"locked OMS session exploration", self.git("worktree", "list", "--porcelain"))
        self.assertEqual(self.native_calls(), [])

    def test_each_native_fork_uses_exact_argv_cwd_and_preserves_source_identity(self):
        for provider in self.sids:
            with self.subTest(provider=provider):
                before = self.paths[provider].read_bytes()
                row = self.cli("fork", *self.source_args(provider), "--apply",
                               env={"OMS_ROOM_ID": "old-room", "OMS_AGENT_ATTEMPT_ID": "old-attempt"})
                call = self.native_calls()[-1]
                self.assertEqual(call["cwd"], row["worktree"])
                self.assertIsNone(call["room"])
                self.assertIsNone(call["attempt"])
                self.assertEqual(call["policy"], "preserved")
                self.assertEqual(self.paths[provider].read_bytes(), before)
                self.assertIsNotNone(row["native_session"])
                self.assertNotEqual(row["native_session"], self.sids[provider])
                if provider == "codex":
                    self.assertEqual(call["argv"], ["fork", self.sids[provider], "--cd", row["worktree"]])
                elif provider == "claude":
                    self.assertEqual(call["argv"], ["--resume", self.sids[provider], "--fork-session",
                                                     "--session-id", row["native_session"]])
                else:
                    self.assertEqual(call["argv"], ["--fork", str(self.paths[provider])])

    def test_native_fork_drops_parent_authority_and_state_routing(self):
        keys = ("OMS_PANEL_MAIN_ATTEMPT", "OMS_PANEL_REPO", "OMS_PANEL_SESSION", "OMS_STATE_REPO",
                "OMS_PLAN_ID", "OMS_LEASE_ID", "OMS_PLAN_LEASE_ID", "OMS_APPROVAL_ID", "OMS_LANDING_ID",
                "OMS_EXECUTOR_ID", "OMS_HARNESS_CALL_ID", "OMS_INDEX_BASE_REPO", "DELEGATE_SOURCE_REPO")
        markers = {key: "parent-authority" for key in keys}
        markers.update(OMS_PANEL_REPO=str(self.repo), OMS_STATE_REPO=str(self.repo),
                       OMS_INDEX_BASE_REPO=str(self.repo), DELEGATE_SOURCE_REPO=str(self.repo))
        row = self.cli("fork", *self.source_args(), "--apply", env=markers)
        call = self.native_calls()[-1]
        self.assertEqual(call["cwd"], row["worktree"])
        self.assertNotEqual(call["cwd"], str(self.repo))
        self.assertEqual(call["inherited_authority"], dict.fromkeys(keys))
        self.assertEqual(call["policy"], "preserved")

    def test_resume_and_conversation_restore_are_independent_of_file_restore(self):
        self.dirty()
        before = (self.git("diff", "--binary"), self.git("diff", "--cached", "--binary"))
        for provider in self.sids:
            resume = self.cli("resume", *self.source_args(provider))
            self.assertEqual(resume["action"], "resume")
            self.cli("resume", *self.source_args(provider), "--apply")
            call = self.native_calls()[-1]
            self.assertEqual(call["cwd"], str(self.repo))
            expected = {"claude": ["--resume", self.sids[provider]],
                        "codex": ["resume", self.sids[provider], "--cd", str(self.repo)],
                        "pi": ["--session", str(self.paths[provider])]}[provider]
            self.assertEqual(call["argv"], expected)
        report = self.cli("restore", "conversation", *self.source_args(), "--apply")
        self.assertFalse(report["files_restored"])
        self.assertEqual(report["action"], "fork")
        self.assertEqual((self.git("diff", "--binary"), self.git("diff", "--cached", "--binary")), before)

    def test_file_restore_preserves_conversation_and_newer_untracked_requires_same_head(self):
        self.dirty()
        saved = self.prepared()
        expected = (self.git("diff", "--binary"), self.git("diff", "--cached", "--binary"))
        transcript = self.paths["codex"].read_bytes()
        (self.repo / "tracked.txt").write_text("later\n")
        (self.repo / "new.txt").write_text("newer untracked\n")
        dry = self.cli("restore", "files", saved["checkpoint_id"])
        self.assertTrue(dry["dry_run"])
        self.assertEqual((self.repo / "tracked.txt").read_text(), "later\n")
        applied = self.cli("restore", "files", saved["checkpoint_id"], "--apply")
        self.assertTrue(applied["applied"])
        self.assertFalse(applied["conversation_changed"])
        self.assertEqual((self.git("diff", "--binary"), self.git("diff", "--cached", "--binary")), expected)
        self.assertEqual((self.repo / "new.txt").read_text(), "newer untracked\n")
        self.assertEqual(self.paths["codex"].read_bytes(), transcript)
        self.assertTrue((self.repo / ".oms" / "checkpoints" / applied["backup_id"]).is_dir())
        self.git("commit", "-qm", "different HEAD")
        failed = self.cli("restore", "files", saved["checkpoint_id"], "--apply", ok=False)
        self.assertIn("same HEAD", failed.stderr)
        self.assertEqual(self.native_calls(), [])

    def test_compare_current_diffs_index_and_artifact_hashes(self):
        first, second = self.prepared(), self.prepared()
        left, right = Path(first["worktree"]), Path(second["worktree"])
        (left / "tracked.txt").write_text("left choice\n")
        self.git("add", "tracked.txt", cwd=left)
        (right / "tracked.txt").write_text("right choice\n")
        for target, content in ((left, "left artifact"), (right, "right artifact")):
            (target / ".oms" / "artifacts").mkdir()
            (target / ".oms" / "artifacts" / "result.txt").write_text(content)
        report = self.cli("compare", first["id"], second["id"], "--patch")
        item = next(row for row in report["files"] if row["path"] == "tracked.txt")
        self.assertIn("-left choice", item["patch"])
        self.assertIn("+right choice", item["patch"])
        self.assertNotEqual(report["left"]["staged_sha256"], report["right"]["staged_sha256"])
        self.assertNotEqual(report["left"]["artifacts"]["files"][0]["sha256"],
                            report["right"]["artifacts"]["files"][0]["sha256"])
        self.assertNotIn("left artifact", json.dumps(report))

    def test_ambient_git_environment_cannot_hide_fork_untracked_changes(self):
        first, second = self.prepared(), self.prepared()
        (Path(first["worktree"]) / "choice.txt").write_text("left-only untracked\n")
        inherited = {"GIT_DIR": str(self.repo / ".git"), "GIT_WORK_TREE": str(self.repo)}
        report = self.cli("compare", first["id"], second["id"], env=inherited)
        self.assertEqual([row["path"] for row in report["files"]], ["choice.txt"])
        foreign = self.base / "foreign"
        foreign.mkdir()
        self.git("init", "-q", "-b", "main", cwd=foreign)
        (foreign / "foreign.txt").write_text("wrong repository\n")
        (self.repo / "local.txt").write_text("selected repository\n")
        plan = self.cli("fork", *self.source_args(), env={"GIT_DIR": str(foreign / ".git"), "GIT_WORK_TREE": str(foreign)})
        self.assertEqual(plan["files"]["untracked_captured"], 1)
        self.assertEqual(plan["files"]["untracked_omitted"], 0)

    def test_worker_symlink_ambiguity_and_home_scope_boundaries(self):
        for child in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "2"}):
            self.cli("search", "phrase", env=child, ok=False)
            self.cli("fork", *self.source_args(), "--apply", env=child, ok=False)
        self.cli("search", "phrase", "--provider", "codex", "--root", self.home, ok=False)
        root = self.paths["codex"].parent
        self.transcript("codex", self.sids["codex"], folder=root / "duplicate")
        self.cli("open", *self.source_args(), "--root", root, ok=False)
        (root / "duplicate" / (self.sids["codex"] + ".jsonl")).unlink()
        (self.repo / ".oms").symlink_to(self.home)
        self.cli("fork", *self.source_args(), "--apply", ok=False)
        self.assertEqual(self.native_calls(), [])
        self.assertFalse((self.home / "sessions").exists())

    def test_changed_source_and_failed_native_launch_preserve_recovery(self):
        row = self.prepared()
        with self.paths["codex"].open("a") as handle:
            handle.write("{}\n")
        failed = self.cli("launch", row["id"], "--apply", ok=False)
        self.assertIn("conversation changed", failed.stderr)
        self.assertEqual(self.native_calls(), [])
        self.assertTrue(Path(row["worktree"]).is_dir())
        failed = self.cli("fork", *self.source_args(), "--apply", env={"OMS_FAKE_NATIVE_EXIT": "7"}, ok=False)
        self.assertEqual(failed.returncode, 7)
        launch = json.loads(failed.stdout)
        self.assertEqual(launch["conversation_status"], "exited")
        self.assertTrue(Path(launch["worktree"]).is_dir())
        count = len(self.native_calls())
        self.cli("launch", launch["id"], "--apply", ok=False)
        self.assertEqual(len(self.native_calls()), count)

    def test_concurrent_launch_claims_exactly_one_native_process(self):
        row = self.prepared()
        argv = ["bash", str(ENTRY), "--repo", str(self.repo), "launch", row["id"], "--apply"]
        def invoke():
            return subprocess.run(argv, cwd=self.repo, env=self.env, capture_output=True, text=True)
        with ThreadPoolExecutor(max_workers=2) as executor:
            processes = list(executor.map(lambda _: invoke(), range(2)))
        self.assertEqual(sorted(item.returncode for item in processes), [0, 2])
        self.assertEqual(len(self.native_calls()), 1)

    def test_standalone_resume_claim_excludes_concurrent_oms_and_releases_after_exit(self):
        argv = ["bash", str(ENTRY), "--repo", str(self.repo), "resume", *self.source_args(), "--apply"]
        first = subprocess.Popen(argv, cwd=self.repo, env=dict(self.env, OMS_FAKE_HOLD_SECONDS="3"),
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 10
            while not self.native_calls() and first.poll() is None and time.monotonic() < deadline:
                time.sleep(.03)
            self.assertEqual(len(self.native_calls()), 1)
            second = subprocess.run(argv, cwd=self.repo, env=self.env, capture_output=True, text=True, timeout=10)
            self.assertEqual(second.returncode, 75, second.stdout + second.stderr)
            self.assertIn("state lock", second.stderr)
            self.assertEqual(len(self.native_calls()), 1)
            stdout, stderr = first.communicate(timeout=10)
            self.assertEqual(first.returncode, 0, stdout + stderr)
            self.cli("resume", *self.source_args(), "--apply")
            self.assertEqual(len(self.native_calls()), 2)
        finally:
            if first.poll() is None:
                first.kill()
                first.communicate()

    def test_managed_resume_keeps_the_existing_native_owner_guard(self):
        for action in (("new", "--id", "session-room", "--participant", "main-codex"),
                       ("join", "--id", "session-room", "--participant", "main-codex", "--provider", "codex",
                        "--native-session", self.sids["codex"])):
            result = subprocess.run(["bash", str(ROOT / "scripts" / "room.sh"), "--repo", str(self.repo), *action],
                                    env=self.env, cwd=self.repo, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        report = self.cli("resume", *self.source_args())
        self.assertEqual(report["route"], "managed-panel")
        self.assertIn(str(ROOT / "scripts" / "panel.sh"), report["argv"])
        self.assertIn("session-room", report["argv"])
        # The panel owns this identity; a nonterminal caller cannot launch a
        # parallel raw provider through the new session entrypoint.
        self.cli("resume", *self.source_args(), "--apply", ok=False)
        self.assertEqual(self.native_calls(), [])

    def test_hidden_index_states_and_hardlinked_untracked_are_refused(self):
        (self.repo / "intent.txt").write_text("")
        self.git("add", "-N", "intent.txt")
        self.assertIn("intent-to-add", self.cli("fork", *self.source_args(), ok=False).stderr)
        self.git("reset", "--", "intent.txt")
        self.git("update-index", "--assume-unchanged", "tracked.txt")
        (self.repo / "tracked.txt").write_text("hidden edit\n")
        self.assertIn("assume-unchanged", self.cli("fork", *self.source_args(), ok=False).stderr)
        self.git("update-index", "--no-assume-unchanged", "tracked.txt")
        os.link(self.repo / "tracked.txt", self.repo / "hardlink")
        self.assertIn("multiply-linked", self.cli("fork", *self.source_args(), ok=False).stderr)
        self.assertFalse((self.repo / ".oms").exists())

    def test_native_oversize_symlinks_and_checkpoint_tampering_are_bounded(self):
        path = self.paths["pi"]
        with path.open("ab") as handle:
            handle.truncate(32 * 1024 * 1024 + 1)
        report = self.cli("search", "phrase", "--provider", "pi")
        self.assertEqual(report["matches"], [])
        self.assertGreater(report["scan"]["skipped"], 0)
        self.cli("resume", *self.source_args("pi"), "--apply", ok=False)
        path.unlink()
        path.symlink_to(self.paths["claude"])
        self.cli("open", *self.source_args("pi"), ok=False)
        row = self.prepared()
        patch = self.repo / ".oms" / "checkpoints" / row["checkpoint_id"] / "worktree.patch"
        patch.write_text("not the recorded patch")
        before = self.git("status", "--porcelain")
        self.assertIn("checksum", self.cli("restore", "files", row["checkpoint_id"], "--apply", ok=False).stderr)
        self.assertEqual(self.git("status", "--porcelain"), before)
        self.assertEqual(self.native_calls(), [])


if __name__ == "__main__":
    unittest.main()
