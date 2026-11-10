"""Integration coverage for the previously untested recipe feature family."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
ENTRY = ROOT / "scripts/recipe.sh"
sys.path.insert(0, str(ROOT / "scripts/lib"))
from oms_runtime.common import atomic_write_bytes


WORKER = '''import json, os, pathlib, socket, sys, time
mode, *args = sys.argv[1:]
if mode == "write":
    pathlib.Path(args[0]).write_text(json.dumps(args[1:]))
elif mode == "first":
    with open("calls", "a") as f: f.write("first\\n")
    pathlib.Path("first.out").write_text("first")
elif mode == "flaky":
    with open("calls", "a") as f: f.write("flaky\\n")
    gate = pathlib.Path("fail-once")
    if gate.exists():
        gate.unlink()
        raise SystemExit(7)
    pathlib.Path("second.out").write_text("second")
elif mode == "wait":
    pathlib.Path("process.pid").write_text(str(os.getpid()))
    if not pathlib.Path("allow").exists(): time.sleep(30)
    pathlib.Path("wait.out").write_text("done")
elif mode == "noise":
    print("x" * 100000)
elif mode == "sensitive":
    print("password=" + "test-fixture-value")
elif mode == "env":
    pathlib.Path("env.out").write_text(json.dumps({"value": os.getenv("MESSAGE"), "secret": os.getenv("GITHUB_TOKEN")}))
elif mode == "serve":
    pathlib.Path("companion.pid").write_text(str(os.getpid()))
    with socket.socket() as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("127.0.0.1", int(args[0])))
        s.listen()
        while True:
            c, _ = s.accept()
            with c: c.sendall(b"ready")
elif mode == "probe":
    with socket.create_connection(("127.0.0.1", int(args[0])), timeout=2) as s:
        pathlib.Path("probe.out").write_text(s.recv(20).decode() + ":" + os.environ["MESSAGE"])
elif mode == "noop":
    pass
'''


class RecipeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="oms-recipes-test.")
        self.base = Path(self.tmp.name)
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.home = self.base / "home"
        self.home.mkdir()
        self.environment = {"PATH": os.environ.get("PATH", os.defpath), "HOME": str(self.home),
                            "OMS_LOCK_DIR": str(self.base / "locks"), "PYTHONDONTWRITEBYTECODE": "1", "LC_ALL": "C.UTF-8"}
        (self.repo / "worker.py").write_text(WORKER)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, value):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))
        return path

    def call(self, *args, code=0, env=None):
        result = subprocess.run(["bash", str(ENTRY), *map(str, args), "--repo", str(self.repo)],
                                env=dict(self.environment, **(env or {})), capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        return json.loads(result.stdout) if result.stdout.strip() else result.stderr

    def command(self, *args, **extra):
        return dict(argv=["python3", "worker.py", *args], depends=["worker.py"], timeout_seconds=5, **extra)

    def recipe(self, mode="write", args=("result.json", "hello"), outputs=None):
        return {"schema": 1, "kind": "recipe", "id": "oms-test", "commands": {"work": self.command(mode, *args)},
                "steps": [{"id": "work", "command": "work", "outputs": outputs if outputs is not None else [{"path": "result.json", "min_bytes": 1}]}]}

    def flaky(self):
        value = {"schema": 1, "kind": "recipe", "id": "oms-flaky", "commands": {
            "first": self.command("first"), "flaky": self.command("flaky")}, "steps": [
            {"id": "first", "command": "first", "outputs": [{"path": "first.out"}]},
            {"id": "flaky", "command": "flaky", "outputs": [{"path": "second.out"}]}]}
        self.write("recipe.json", value)
        (self.repo / "fail-once").touch()
        return value

    def test_typed_inputs_are_single_argv_items_and_execution_is_explicit(self):
        value = self.recipe(args=("${input.output}", "${input.value}", "${input.count}", "${input.enabled}"))
        value["inputs"] = {"output": {"type": "path", "default": "result.json"},
                           "value": {"type": "string"}, "count": {"type": "integer", "min": 1, "max": 5},
                           "enabled": {"type": "boolean", "default": False}}
        self.write("recipe.json", value)
        injected = "literal; touch injected; $(touch expanded)"
        plan = self.call("plan", "recipe.json", "--input", "value=" + injected, "--input", "count=2")
        self.assertEqual(plan["steps"][0]["command"]["argv"][-3:], [injected, "2", "false"])
        self.assertFalse((self.repo / ".oms").exists())
        self.call("run", "recipe.json", "--input", "value=" + injected, "--input", "count=2", code=2)
        run = self.call("run", "recipe.json", "--input", "value=" + injected, "--input", "count=2", "--execute")
        self.assertEqual(run["status"], "succeeded")
        self.assertEqual(json.loads((self.repo / "result.json").read_text()), [injected, "2", "false"])
        self.assertFalse((self.repo / "injected").exists())
        self.assertFalse((self.repo / "expanded").exists())
        self.assertNotIn(str(self.home), json.dumps(run))
        self.call("plan", "recipe.json", "--input", "value=x", "--input", "count=true", code=2)
        self.call("plan", "recipe.json", "--input", "value=x", "--input", "count=8", code=2)

    def test_failure_resume_preserves_completed_steps_and_detects_output_drift(self):
        self.flaky()
        failed = self.call("run", "recipe.json", "--execute", code=7)
        self.assertEqual(failed["status"], "failed")
        self.call("resume", failed["id"], "--file", "recipe.json", "--execute", code=2)
        done = self.call("resume", failed["id"], "--file", "recipe.json", "--execute", "--retry-failed")
        self.assertEqual(done["status"], "succeeded")
        self.assertEqual((self.repo / "calls").read_text().splitlines(), ["first", "flaky", "flaky"])
        self.assertEqual(len(done["steps"]["first"]["attempts"]), 1)
        self.assertEqual(len(done["steps"]["flaky"]["attempts"]), 2)
        self.call("resume", done["id"], "--file", "recipe.json", "--execute")
        (self.repo / "first.out").write_text("changed")
        self.assertIn("completed output changed", self.call("resume", done["id"], "--file", "recipe.json", "--execute", code=2))

    def test_resume_rejects_changed_definition_and_dependency(self):
        value = self.flaky()
        failed = self.call("run", "recipe.json", "--execute", code=7)
        value["commands"]["flaky"]["timeout_seconds"] = 6
        self.write("recipe.json", value)
        self.assertIn("changed", self.call("resume", failed["id"], "--file", "recipe.json", "--execute", "--retry-failed", code=2))
        value["commands"]["flaky"]["timeout_seconds"] = 5
        self.write("recipe.json", value)
        (self.repo / "worker.py").write_text(WORKER + "\n# changed\n")
        self.assertIn("changed", self.call("resume", failed["id"], "--file", "recipe.json", "--execute", "--retry-failed", code=2))

    def test_outputs_must_be_fresh_bounded_and_inside_the_repo(self):
        (self.repo / "result.json").write_text("stale")
        self.write("recipe.json", self.recipe("noop", args=()))
        failed = self.call("run", "recipe.json", "--execute", code=2)
        self.assertIn("not refreshed", failed["failure"]["reason"])
        (self.repo / "result.json").unlink()
        (self.repo / "result.json").symlink_to(self.base / "outside")
        self.call("run", "recipe.json", "--execute", code=2)
        self.assertFalse((self.base / "outside").exists())
        value = self.recipe(args=("../outside", "bad"), outputs=[{"path": "../outside"}])
        self.write("recipe.json", value)
        self.call("plan", "recipe.json", code=2)

    def test_output_limit_and_timeout_are_receipted(self):
        value = self.recipe("noise", args=(), outputs=[])
        value["commands"]["work"]["max_output_bytes"] = 1024
        self.write("recipe.json", value)
        run = self.call("run", "recipe.json", "--execute", code=125)
        self.assertEqual(run["steps"]["work"]["attempts"][-1]["reason"], "output-limit")
        value = self.recipe("wait", args=(), outputs=[{"path": "wait.out"}])
        value["commands"]["work"]["timeout_seconds"] = 1
        self.write("recipe.json", value)
        run = self.call("run", "recipe.json", "--execute", code=124)
        self.assertEqual(run["steps"]["work"]["attempts"][-1]["reason"], "timeout")
        with self.assertRaises(ProcessLookupError):
            os.kill(int((self.repo / "process.pid").read_text()), 0)

    def test_interruption_stops_the_process_and_requires_acknowledged_resume(self):
        value = self.recipe("wait", args=(), outputs=[{"path": "wait.out"}])
        self.write("recipe.json", value)
        proc = subprocess.Popen(["bash", str(ENTRY), "run", "recipe.json", "--execute", "--repo", str(self.repo)],
                                env=self.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 8
            while not (self.repo / "process.pid").exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue((self.repo / "process.pid").exists())
            proc.terminate()
            stdout, stderr = proc.communicate(timeout=8)
            self.assertEqual(proc.returncode, 130, stdout + stderr)
            run = json.loads(stdout)
            self.assertEqual(run["status"], "interrupted")
            self.call("resume", run["id"], "--file", "recipe.json", "--execute", code=2)
            (self.repo / "allow").touch()
            resumed = self.call("resume", run["id"], "--file", "recipe.json", "--execute", "--retry-uncertain")
            self.assertEqual(resumed["status"], "succeeded")
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.communicate(timeout=5)

    def test_project_overlay_replaces_and_appends_steps_with_base_binding(self):
        value = self.recipe()
        self.write("recipe.json", value)
        base = self.call("validate", "recipe.json")["definition_digest"]
        overlay = {"schema": 1, "kind": "recipe-overlay", "base_digest": base,
                   "commands": {"project": self.command("write", "project.out", "project"), "extra": self.command("write", "extra.out", "extra")},
                   "steps": [{"id": "work", "command": "project", "outputs": [{"path": "project.out"}]},
                             {"id": "extra", "command": "extra", "outputs": [{"path": "extra.out"}]}]}
        self.write("overlay.json", overlay)
        run = self.call("run", "recipe.json", "--overlay", "overlay.json", "--execute")
        self.assertEqual(run["status"], "succeeded")
        self.assertFalse((self.repo / "result.json").exists())
        self.assertTrue((self.repo / "extra.out").is_file())
        value["description"] = "Changed base"
        self.write("recipe.json", value)
        self.call("validate", "recipe.json", "--overlay", "overlay.json", code=2)

    def test_workspace_setup_environment_ports_and_scoped_companions(self):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        value = {"schema": 1, "kind": "workspace", "id": "oms-workspace", "environment": {"MESSAGE": {"from_env": "LOCAL_MESSAGE"}},
                 "ports": {"http": 43123}, "commands": {"env": self.command("env"), "serve": self.command("serve", "${port.http}"),
                     "probe": self.command("probe", "${port.http}")},
                 "setup": [{"id": "env", "command": "env", "outputs": [{"path": "env.out"}]}],
                 "actions": {"check": {"steps": [{"id": "check", "command": "probe", "outputs": [{"path": "probe.out"}]}]}},
                 "companions": {"server": {"command": "serve", "ready_port": "http", "startup_seconds": 3}}}
        self.write("workspace.json", value)
        env = {"LOCAL_MESSAGE": "hello", "GITHUB_TOKEN": "never-inherit-this-value"}
        self.call("workspace", "plan", "workspace.json", "setup", env=env)
        self.assertFalse((self.repo / "env.out").exists())
        self.call("workspace", "run", "workspace.json", "setup", "--execute", env=env)
        self.assertEqual(json.loads((self.repo / "env.out").read_text()), {"value": "hello", "secret": None})
        run = self.call("workspace", "run", "workspace.json", "check", "--port", "http=" + str(port), "--with", "server", "--execute", env=env)
        self.assertEqual((self.repo / "probe.out").read_text(), "ready:hello")
        self.assertEqual(run["companion_attempts"][-1]["status"], "stopped")
        with self.assertRaises(ProcessLookupError):
            os.kill(int((self.repo / "companion.pid").read_text()), 0)
        value["environment"] = {"MESSAGE": {"from_env": "API_TOKEN"}}
        self.write("workspace.json", value)
        self.call("validate", "workspace.json", code=2)

    def pack(self):
        folder = self.repo / "local-pack"
        folder.mkdir()
        script = b'import pathlib, sys\npathlib.Path(sys.argv[1]).write_text("local pack")\n'
        renderer = b'import pathlib\npathlib.Path("report.html").write_text("<p>local pack</p>")\n'
        value = {"schema": 1, "kind": "recipe", "id": "oms-packed", "commands": {
            "work": {"argv": ["python3", "${resource.helper}", "pack.out"]},
            "render": {"argv": ["python3", "${resource.renderer}"]}}, "steps": [
            {"id": "work", "command": "work", "outputs": [{"path": "pack.out"}]},
            {"id": "render", "command": "render", "outputs": [{"path": "report.html"}]}]}
        resources = {"recipe": ("recipe", "recipe.json", json.dumps(value).encode()),
                     "helper": ("script", "helper.py", script), "renderer": ("renderer", "renderer.py", renderer)}
        manifest = {"schema": 1, "kind": "local-pack", "id": "oms-local", "version": "1.0.0", "resources": {}}
        for key, (kind, path, content) in resources.items():
            (folder / path).write_bytes(content)
            manifest["resources"][key] = {"kind": kind, "path": path, "sha256": hashlib.sha256(content).hexdigest()}
        (folder / "manifest.json").write_text(json.dumps(manifest))
        return folder, manifest

    def test_selective_local_pack_import_and_real_recipe_renderer_execution(self):
        folder, _ = self.pack()
        info = self.call("pack", "inspect", str(folder))
        imported = self.call("pack", "import", str(folder), "--digest", info["digest"], "--resource", "recipe", "--resource", "helper")
        self.assertEqual(imported["selected"], ["helper", "recipe"])
        self.assertFalse((self.repo / "pack.out").exists())
        self.call("plan", "pack:oms-local@1.0.0/recipe", code=2)
        self.call("pack", "import", str(folder), "--digest", info["digest"], "--resource", "renderer")
        run = self.call("run", "pack:oms-local@1.0.0/recipe", "--execute")
        self.assertEqual(run["status"], "succeeded")
        self.assertEqual((self.repo / "report.html").read_text(), "<p>local pack</p>")
        self.assertEqual(len(self.call("pack", "list")["packs"]), 1)
        (folder / "helper.py").write_text("changed")
        self.call("pack", "inspect", str(folder), code=2)

    def test_pack_version_digest_and_path_escape_are_rejected(self):
        folder, manifest = self.pack()
        info = self.call("pack", "inspect", str(folder))
        self.call("pack", "import", str(folder), "--digest", "0" * 64, code=2)
        self.call("pack", "import", str(folder), "--digest", info["digest"])
        manifest["description"] = "different manifest same version"
        (folder / "manifest.json").write_text(json.dumps(manifest))
        info = self.call("pack", "inspect", str(folder))
        self.call("pack", "import", str(folder), "--digest", info["digest"], code=2)
        manifest["resources"]["helper"]["path"] = "../worker.py"
        (folder / "manifest.json").write_text(json.dumps(manifest))
        self.call("pack", "inspect", str(folder), code=2)

    def test_workset_inspects_linked_repos_and_spec_drift_without_mutating_them(self):
        sibling = self.base / "service"
        sibling.mkdir()
        (sibling / "spec.md").write_text("Shared goal")
        self.write("workset.json", {"schema": 1, "kind": "workset", "id": "oms-feature", "title": "Shared feature",
                    "spec": {"repo": "service", "path": "spec.md"}, "repos": [
                        {"id": "main", "path": ".", "tasks": ["task-main"]},
                        {"id": "service", "path": "../service", "tasks": ["task-api"]},
                        {"id": "future", "path": "../future", "tasks": []}]})
        linked = self.call("workset", "put", "workset.json")
        self.assertEqual(linked["spec"]["status"], "current")
        self.assertFalse(linked["repos"][2]["available"])
        self.assertEqual(sorted(path.name for path in sibling.iterdir()), ["spec.md"])
        (sibling / "spec.md").write_text("Changed goal")
        self.assertEqual(self.call("workset", "show", "oms-feature")["spec"]["status"], "changed")
        self.call("workset", "put", "workset.json", code=2)
        self.assertEqual(self.call("workset", "put", "workset.json", "--replace")["spec"]["status"], "current")

    def test_due_requests_coalesce_and_execute_only_on_explicit_invocation(self):
        self.write("recipe.json", self.recipe())
        self.write("trigger.json", {"schema": 1, "kind": "recipe-trigger", "id": "oms-daily", "recipe": "recipe.json",
                    "schedule": {"at": "2026-01-01T00:00:00Z", "every_seconds": 60}})
        self.call("trigger", "add", "trigger.json")
        self.assertEqual(self.call("trigger", "tick", "--now", "2025-12-31T00:00:00Z")["requests"], [])
        queued = self.call("trigger", "tick", "--now", "2026-01-01T00:10:00Z")["requests"]
        self.assertEqual(len(queued), 1)
        self.assertEqual(queued[0]["metadata"]["coalesced_occurrences"], 10)
        self.assertFalse((self.repo / "result.json").exists())
        self.assertEqual(self.call("trigger", "tick", "--now", "2026-01-01T00:10:00Z")["requests"], [])
        self.call("trigger", "run", queued[0]["id"], code=2)
        done = self.call("trigger", "run", queued[0]["id"], "--execute")
        self.assertEqual(done["run"]["status"], "succeeded")
        again = self.call("trigger", "run", queued[0]["id"], "--execute")
        self.assertEqual(again["run"]["id"], done["run"]["id"])
        self.call("trigger", "disable", "oms-daily")
        self.assertEqual(self.call("trigger", "tick", "--now", "2026-01-01T00:20:00Z")["requests"], [])
        self.assertEqual(self.call("trigger", "requests")["requests"][0]["effective_status"], "succeeded")

    def test_event_filters_deduplicate_pin_definitions_and_keep_payloads_out_of_history(self):
        self.write("recipe.json", self.recipe())
        self.write("trigger.json", {"schema": 1, "kind": "recipe-trigger", "id": "oms-event", "recipe": "recipe.json",
                    "event": {"name": "changed", "match": {"branch": "main"}}})
        self.call("trigger", "add", "trigger.json")
        ignored = self.call("trigger", "event", "changed", "--event-id", "evt-1", "--data", '{"branch":"dev"}')
        self.assertEqual(ignored["requests"], [])
        queued = self.call("trigger", "event", "changed", "--event-id", "evt-2", "--data", '{"branch":"main","detail":"unique-private-content"}')
        repeat = self.call("trigger", "event", "changed", "--event-id", "evt-2", "--data", '{"branch":"main","detail":"unique-private-content"}')
        self.assertEqual(queued["requests"], repeat["requests"])
        self.call("trigger", "event", "changed", "--event-id", "evt-2", "--data", '{"branch":"different"}', code=2)
        self.assertFalse((self.repo / "result.json").exists())
        value = self.recipe()
        value["description"] = "changed"
        self.write("recipe.json", value)
        self.call("trigger", "run", queued["requests"][0], "--execute", code=2)
        self.call("trigger", "cancel", queued["requests"][0])
        self.call("trigger", "run", queued["requests"][0], "--execute", code=2)
        state = "".join(path.read_text() for path in (self.repo / ".oms").rglob("*.json*"))
        self.assertNotIn("unique-private-content", state)

    def test_child_guards_are_semantic_and_read_only_actions_create_no_state(self):
        self.write("recipe.json", self.recipe())
        for marker in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
            self.call("plan", "recipe.json", env=marker)
            for args in (("run", "recipe.json", "--execute"), ("trigger", "tick"),
                         ("pack", "import", "missing", "--digest", "0" * 64),
                         ("workset", "put", "missing"), ("trigger", "event", "changed", "--event-id", "evt")):
                self.assertIn("read-only", self.call(*args, code=2, env=marker))
        self.assertFalse((self.repo / ".oms").exists())

    def test_symlinked_state_is_refused_and_oversized_definitions_are_bounded(self):
        outside = self.base / "outside"
        outside.mkdir()
        (self.repo / ".oms").symlink_to(outside)
        self.write("recipe.json", self.recipe())
        self.call("run", "recipe.json", "--execute", code=2)
        self.assertEqual(list(outside.iterdir()), [])
        (self.repo / "huge.json").write_text(" " * (256 * 1024 + 1))
        self.call("validate", "huge.json", code=2)

    def test_sensitive_console_is_scanned_without_retaining_its_bytes(self):
        self.write("recipe.json", self.recipe("sensitive", args=(), outputs=[]))
        run = self.call("run", "recipe.json", "--execute")
        receipt = run["steps"]["work"]["attempts"][-1]
        self.assertTrue(receipt["sensitive_console_detected"])
        self.assertFalse(receipt["console_retained"])
        state = "".join(path.read_text() for path in (self.repo / ".oms").rglob("*.json*"))
        self.assertNotIn("test-fixture-value", state)

    def test_interpreter_code_interpolation_duplicate_keys_and_nested_execution_are_refused(self):
        value = self.recipe(outputs=[])
        value["inputs"] = {"value": {"type": "string", "default": "hello"}}
        value["commands"]["work"] = {"argv": ["python3", "-c", "print('${input.value}')"]}
        self.write("recipe.json", value)
        self.call("validate", "recipe.json", code=2)
        (self.repo / "duplicate.json").write_text('{"schema":1,"schema":1}')
        self.call("validate", "duplicate.json", code=2)
        value = self.recipe(outputs=[])
        value["commands"]["work"] = {"argv": ["bash", str(ENTRY), "trigger", "tick", "--repo", "."]}
        self.write("recipe.json", value)
        run = self.call("run", "recipe.json", "--execute", code=2)
        self.assertEqual(run["steps"]["work"]["attempts"][-1]["exit"], 2)
        self.assertFalse((self.repo / ".oms/recipes/triggers").exists())

    def test_imported_pack_tampering_is_detected_before_execution(self):
        folder, _ = self.pack()
        info = self.call("pack", "inspect", str(folder))
        self.call("pack", "import", str(folder), "--digest", info["digest"])
        atomic_write_bytes(self.repo / ".oms/recipes/packs/oms-local/1.0.0/resources/helper.py", b"changed\n")
        self.assertIn("digest mismatch", self.call("run", "pack:oms-local@1.0.0/recipe", "--execute", code=2))
        self.assertFalse((self.repo / "pack.out").exists())

    def test_virtualenv_interpreter_symlink_keeps_its_invocation_identity(self):
        venv = self.repo / "venv"
        subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(venv)], check=True,
                       env=self.environment, capture_output=True, timeout=20)
        value = self.recipe(outputs=[{"path": "prefix.out", "min_bytes": 1}])
        value["commands"]["work"] = {"argv": ["venv/bin/python", "-c", "import pathlib,sys; pathlib.Path('prefix.out').write_text(sys.prefix)"]}
        self.write("recipe.json", value)
        self.call("run", "recipe.json", "--execute")
        self.assertEqual(Path((self.repo / "prefix.out").read_text()).resolve(), venv.resolve())
        value["commands"]["work"]["argv"][0] = "python3"
        self.write("recipe.json", value)
        self.call("run", "recipe.json", "--execute", env={"PATH": str(venv / "bin") + os.pathsep + self.environment["PATH"]})
        self.assertEqual(Path((self.repo / "prefix.out").read_text()).resolve(), venv.resolve())


if __name__ == "__main__":
    unittest.main()
