"""DVC file-stage forks with explicit immutable input and output snapshots.

Scope is deliberately executable and narrow: one JSON-encoded dvc.yaml,
cmd/deps/outs stages, regular files, and a local DVC CLI. DVC owns caching;
OMS reports the stages it requested and verifies actual output bytes.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

from oms_runtime.common import CoreError, atomic_write_bytes, canonical_json, read_bytes, safe_id, sha256_bytes
from . import execution
from .common import (
    MAX_FILE_BYTES, blob, identifier, load, local, obj, put_blob, relative, snapshot,
    store, workdir, writable,
)


def _run(argv: list[str], cwd: Path, timeout: int) -> dict:
    return execution.run(argv, cwd, timeout, env=dict(os.environ, DVC_NO_ANALYTICS="1"))


def pipeline(data: bytes) -> dict:
    try:
        raw = json.loads(data.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        raise CoreError("DVC adapter supports JSON-encoded dvc.yaml only (JSON is valid YAML)")
    raw = obj(raw, "DVC pipeline")
    if set(raw) != {"stages"}:
        raise CoreError("DVC adapter supports a root stages mapping only")
    stages = obj(raw["stages"], "DVC stages")
    if not 1 <= len(stages) <= 100:
        raise CoreError("DVC pipeline needs 1-100 stages")
    producer = {}
    for name, stage in stages.items():
        safe_id(name, "DVC stage name")
        if ":" in name:
            raise CoreError("DVC stage names cannot contain a colon")
        obj(stage, "DVC stage")
        if set(stage) - {"cmd", "deps", "outs"} or "cmd" not in stage:
            raise CoreError("DVC adapter supports cmd/deps/outs file stages only")
        command = stage["cmd"]
        if not ((isinstance(command, str) and command.strip()) or
                (isinstance(command, list) and command and all(isinstance(c, str) and c.strip() for c in command))):
            raise CoreError("DVC stage cmd must be a string or list of strings")
        commands = [command] if isinstance(command, str) else command
        if any("${" in item for item in commands):
            raise CoreError("DVC adapter does not support parameter or environment interpolation in cmd")
        for field in ("deps", "outs"):
            paths = stage.get(field, [])
            if not isinstance(paths, list) or len(paths) > 100:
                raise CoreError("DVC deps/outs must be lists of at most 100 file paths")
            for path in paths:
                relative(path, "DVC file")
                if path == "dvc.yaml" or path == "dvc.lock" or path.startswith((".git/", ".oms/", ".dvc/")):
                    raise CoreError("DVC stage file collides with workflow control state")
            if len(set(paths)) != len(paths):
                raise CoreError("DVC stage paths must be unique")
        if not stage.get("outs"):
            raise CoreError("DVC adapter requires at least one declared output per stage")
        for output in stage["outs"]:
            if output in producer:
                raise CoreError("DVC output has multiple producers: %s" % output)
            producer[output] = name
    paths = sorted(producer)
    if any(b.startswith(a + "/") for a in paths for b in paths if a != b):
        raise CoreError("DVC file outputs must not overlap")
    parents = {name: sorted({producer[p] for p in stage.get("deps", []) if p in producer})
               for name, stage in stages.items()}
    order, waiting = [], set(stages)
    while waiting:
        ready = sorted(n for n in waiting if all(parent in order for parent in parents[n]))
        if not ready:
            raise CoreError("DVC pipeline has a dependency cycle")
        order.extend(ready)
        waiting.difference_update(ready)
    external = sorted({p for stage in stages.values() for p in stage.get("deps", []) if p not in producer})
    all_paths = external + list(producer)
    if any(b.startswith(a + "/") for a in all_paths for b in all_paths if a != b):
        raise CoreError("DVC adapter accepts regular files, not overlapping file/directory paths")
    return {"stages": stages, "parents": parents, "order": order, "external": external, "outputs": producer}


def capture(repo: Path, name: str, adapter_spec: Any, *, cache_dir: str | None = None) -> dict:
    writable()
    graph = pipeline(read_bytes(local(repo, "dvc.yaml")))
    command = execution.freeze(repo, adapter_spec)
    if any(token in execution.TOKENS for token in command["argv"]):
        raise CoreError("workflow adapter argv is a command prefix, not a run/evaluator template")
    if cache_dir:
        cache = local(repo, cache_dir)
        if not cache.is_dir():
            raise CoreError("explicit DVC cache directory must already exist")
    files = ["dvc.yaml"] + graph["external"]
    if (repo / "dvc.lock").exists():
        files.append("dvc.lock")
    captured = {path: snapshot(repo, path) for path in files}
    outputs = {path: snapshot(repo, path) for path in graph["outputs"]}
    probe = workdir(repo, "workflow-probes", identifier("probe"))
    prefix = execution.materialize(repo, command, probe, {})
    status_argv = ["status", "--json", "--no-updates"] + ["dvc.yaml:" + name for name in graph["order"]]
    status = _run(prefix + status_argv, repo, command["timeout_seconds"])
    if status["status"] != "complete" or status.get("log_truncated"):
        raise CoreError("DVC status probe failed; capture requires an already completed current pipeline")
    try:
        native_status = json.loads(status.get("stdout", ""))
    except ValueError:
        raise CoreError("DVC status --json did not return bounded JSON")
    if native_status != {}:
        raise CoreError("DVC reports changed or missing pipeline files; reproduce the base before capture")
    for path, desc in dict(captured, **outputs).items():
        if sha256_bytes(read_bytes(local(repo, path), MAX_FILE_BYTES)) != desc["sha256"]:
            raise CoreError("workflow file changed during the DVC status probe: %s" % path)
    return store(repo, "workflows", name, {"kind": "oms-study-workflow-snapshot", "workflow_id": safe_id(name),
                 "adapter": "dvc", "adapter_command": command, "cache_dir": cache_dir,
                 "files": captured, "outputs": outputs, "graph": graph, "native_status": status,
                 "scope": "one JSON-encoded dvc.yaml; cmd/deps/outs regular-file stages"})


def plan(repo: Path, base_id: str, replacements: Any) -> dict:
    base = load(repo, "workflows", base_id)
    if base.get("kind") != "oms-study-workflow-snapshot":
        raise CoreError("workflow base must be a captured completed pipeline snapshot")
    replacements = obj(replacements, "replacement files")
    if len(replacements) > 200:
        raise CoreError("too many workflow replacements")
    files = {path: blob(repo, desc) for path, desc in base["files"].items()}
    for desc in base["outputs"].values():
        blob(repo, desc)
    replacement_hashes = {}
    for dest, source in replacements.items():
        relative(dest, "replacement destination")
        if dest == "dvc.lock" or dest in base["outputs"] or dest.startswith((".git/", ".oms/", ".dvc/")):
            raise CoreError("replacements may change pipeline source inputs, not outputs or control state")
        data = read_bytes(local(repo, source), MAX_FILE_BYTES)
        files[dest] = data
        replacement_hashes[dest] = {"source": relative(source), "sha256": sha256_bytes(data), "bytes": len(data),
                                    "executable": bool(local(repo, source).stat().st_mode & 0o111)}
    graph = pipeline(files["dvc.yaml"])
    if set(graph["stages"]) != set(base["graph"]["stages"]) or graph["outputs"] != base["graph"]["outputs"]:
        raise CoreError("DVC fork scope preserves stage names and output ownership; capture a new base for topology expansion")
    needed = {"dvc.yaml", "dvc.lock"} | set(graph["external"])
    if set(replacements) - needed:
        raise CoreError("replacement is not a declared input of the changed DVC pipeline")
    if set(graph["external"]) - set(files):
        raise CoreError("changed pipeline needs an explicit replacement for every new input")
    changed = []
    for name in graph["order"]:
        old_stage, new_stage = base["graph"]["stages"][name], graph["stages"][name]
        input_changed = any(p not in base["files"] or sha256_bytes(files[p]) != base["files"][p]["sha256"]
                            for p in new_stage.get("deps", []) if p in graph["external"])
        if canonical_json(old_stage) != canonical_json(new_stage) or input_changed:
            changed.append(name)
    affected = set(changed)
    for name in graph["order"]:
        if any(parent in affected for parent in graph["parents"][name]):
            affected.add(name)
    reused = [n for n in graph["order"] if n not in affected]
    value = {"schema": 1, "kind": "oms-study-workflow-plan", "base_id": base_id,
             "base_sha256": base["content_sha256"], "adapter": "dvc", "replacements": replacement_hashes,
             "directly_changed": changed, "affected_downstream": [n for n in graph["order"] if n in affected and n not in changed],
             "requested_stages": [n for n in graph["order"] if n in affected], "reused_stages": reused,
             "reused_outputs": [dict(base["outputs"][p], stage=n) for n in reused for p in graph["stages"][n]["outs"]],
             "native_cache_hits": "unknown; DVC owns caching and may skip or restore requested stages",
             "commands": [["repro", "--single-item", "dvc.yaml:" + n] for n in graph["order"] if n in affected]}
    value["plan_sha256"] = sha256_bytes(canonical_json(value))
    return value


def fork(repo: Path, base_id: str, replacements: Any, *, name: str | None = None,
         expected_plan_sha256: str | None = None) -> dict:
    writable()
    planned = plan(repo, base_id, replacements)
    if expected_plan_sha256 and planned["plan_sha256"] != expected_plan_sha256:
        raise CoreError("workflow plan changed since review")
    base = load(repo, "workflows", base_id)
    command = base["adapter_command"]
    execution.verify(repo, command)
    name = safe_id(name or identifier("fork"), "workflow fork id")
    captured = {}
    for dest, desc in planned["replacements"].items():
        data = read_bytes(local(repo, desc["source"]), MAX_FILE_BYTES)
        if sha256_bytes(data) != desc["sha256"]:
            raise CoreError("replacement changed after workflow planning")
        captured[dest] = dict(put_blob(repo, data), executable=desc["executable"])
    store(repo, "workflow-attempts", name, {"kind": "oms-study-workflow-attempt", "fork_id": name,
          "plan": planned, "replacement_objects": captured})
    cwd = workdir(repo, "workflows", name)
    combined = dict(base["files"], **base["outputs"])
    combined.update(captured)
    for path, desc in combined.items():
        atomic_write_bytes(cwd / path, blob(repo, desc), mode=0o700 if desc.get("executable") else 0o600)
    prefix = execution.materialize(repo, command, cwd, {})
    receipts, failed = [], False
    setup = [["init", "--no-scm"]]
    if base.get("cache_dir"):
        cache = local(repo, base["cache_dir"])
        if not cache.is_dir():
            raise CoreError("explicit DVC cache directory is missing")
        setup.append(["config", "cache.dir", str(cache)])
    for args in setup + planned["commands"]:
        result = _run(prefix + args, cwd, command["timeout_seconds"])
        receipts.append(dict(result, adapter_args=args))
        if result["status"] != "complete":
            failed = True
            break
    graph = pipeline(blob(repo, captured.get("dvc.yaml", base["files"]["dvc.yaml"])))
    observed, changed_outputs, reuse_violations, input_violations = {}, [], [], []
    for path in ["dvc.yaml"] + graph["external"]:
        try:
            if sha256_bytes(read_bytes(cwd / path, MAX_FILE_BYTES)) != combined[path]["sha256"]:
                input_violations.append(path)
        except (CoreError, OSError):
            input_violations.append(path)
    for path, stage in graph["outputs"].items():
        try:
            desc = put_blob(repo, read_bytes(cwd / path, MAX_FILE_BYTES))
            observed[path] = desc
            if desc["sha256"] != base["outputs"][path]["sha256"]:
                changed_outputs.append(path)
                if stage in planned["reused_stages"]:
                    reuse_violations.append(path)
        except (CoreError, OSError) as exc:
            observed[path] = {"error": str(exc)}
            failed = True
            if stage in planned["reused_stages"]:
                reuse_violations.append(path)
    status = "interrupted" if any(r["status"] == "interrupted" for r in receipts) else ("failed" if failed or reuse_violations or input_violations else "complete")
    value = {"kind": "oms-study-workflow-fork", "fork_id": name, "base_id": base_id,
             "base_sha256": base["content_sha256"], "plan": planned, "status": status,
             "workspace": cwd.relative_to(repo).as_posix(), "adapter_receipts": receipts,
             "outputs": observed, "changed_output_paths": changed_outputs,
             "reuse_verified": not reuse_violations, "reuse_violations": reuse_violations,
             "input_violations": input_violations,
             "native_cache_hits": "unknown", "cache_owner": "DVC",
             "execution_scope": "trusted-local DVC commands in an isolated directory; not an OS sandbox"}
    return store(repo, "workflow-forks", name, value)
