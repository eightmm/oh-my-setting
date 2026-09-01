"""Exact commit of a bound task's landed patch (`oms graph exec commit`).

`patch-land` applies a reviewed patch to the working tree and never commits,
and it refuses to land onto a dirty tree. A multi-task run therefore needs a
commit between landings. This is the narrowest step that provides one: it
stages exactly the paths of the bound task's stored patch, refuses when the
tree holds any other change, and commits with `--no-verify` the way the
canonical goal-drive driver publishes its exact commits (admission plus the
task verifier are the gate for autonomous work, not commit hooks). It is not a
landing and it never touches plan state: the task must already be `done`
with a landing receipt, both read through the adapter's read-only verbs.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional

from oms_runtime.common import bounded_line, run_output

from . import events as events_module
from .adapters import plan as plan_adapter
from .errors import GraphError
from .facts import receipt_facts
from .workspace import EXCLUDED_ROOTS

GIT_TIMEOUT = 60
MESSAGE_LIMIT = 72


def patch_paths(patch_text: str) -> List[str]:
    """Every path a git patch names in its `diff --git` headers.

    Quoted (escaped) paths are refused rather than guessed: the commit must
    stage exactly what landed, and a misparsed name would either miss a file
    or stage a stranger.
    """
    paths: List[str] = []
    for line in patch_text.splitlines():
        if not line.startswith("diff --git "):
            continue
        rest = line[len("diff --git "):]
        if rest.startswith('"') or ' b/' not in rest:
            raise GraphError("patch names a path this step cannot stage exactly: %s" % bounded_line(rest, 120))
        left, right = rest.split(" b/", 1)
        if not left.startswith("a/"):
            raise GraphError("patch header is not in git format: %s" % bounded_line(rest, 120))
        for item in (left[2:], right):
            item = item.strip()
            if item and item not in paths:
                paths.append(item)
    if not paths:
        raise GraphError("patch names no paths")
    return paths


def _git(repo: Path, *args: str) -> str:
    try:
        completed = subprocess.run(["git", "-C", str(repo)] + list(args), capture_output=True, text=True, timeout=GIT_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as exc:
        raise GraphError("git %s could not run: %s" % (args[0], bounded_line(exc, 200)))
    if completed.returncode != 0:
        raise GraphError("git %s failed: %s" % (args[0], bounded_line(completed.stderr or completed.stdout, 200)))
    return completed.stdout


def _dirty_paths(repo: Path) -> List[str]:
    raw = _git(repo, "status", "--porcelain=v1", "-z", "--untracked-files=all")
    records = raw.split("\0")
    paths: List[str] = []
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if len(record) < 4:
            continue
        status, path = record[:2], record[3:]
        if status[0] in ("R", "C"):
            index += 1
        if any(path == root or path.startswith(root + "/") for root in EXCLUDED_ROOTS):
            continue
        paths.append(path)
    return sorted(paths)


def commit_bound(repo: Path, *, binding: str, run_id: str = "", message: str = "") -> Dict[str, Any]:
    """Commit exactly the landed patch of the task `binding` holds in `run_id`."""
    repo = Path(repo).resolve()
    selected = str(run_id or "") or events_module.latest_run_id(repo)
    if not selected:
        raise GraphError("no execution graph run to read the binding from")
    spec = events_module.load_run_spec(repo, selected)
    projection = events_module.project(events_module.read_events(repo, selected), spec)
    entry = projection.get("bindings", {}).get(str(binding))
    if not isinstance(entry, Mapping) or not entry.get("task_id"):
        raise GraphError("run %s holds no task binding named %s" % (selected, bounded_line(binding, 60)))
    task_id = str(entry["task_id"])
    task = plan_adapter.task_view(repo, task_id)
    if str(task.get("state", "")) != "done":
        raise GraphError("task %s is %s, not done; nothing landed to commit" % (task_id, task.get("state", "-") or "-"))
    head = run_output(["git", "-C", str(repo), "rev-parse", "HEAD"], cwd=repo)
    if not receipt_facts(repo, head=head).get("receipt.land.%s.present" % task_id):
        raise GraphError("task %s has no landing receipt; the graph commits only what patch-land applied" % task_id)
    patch_ref = str(task.get("patch", "") or "")
    patch_path = Path(patch_ref) if patch_ref and Path(patch_ref).is_absolute() else repo / patch_ref
    if not patch_ref or not patch_path.is_file() or patch_path.is_symlink():
        raise GraphError("task %s has no readable stored patch" % task_id)
    paths = patch_paths(patch_path.read_text(encoding="utf-8", errors="replace"))
    dirty = _dirty_paths(repo)
    if not dirty:
        return {"schema": 1, "run_id": selected, "binding": str(binding), "task_id": task_id,
                "commit": head, "paths": paths, "status": "clean"}
    strangers = [path for path in dirty if path not in paths]
    if strangers:
        raise GraphError("tree holds changes outside the landed patch; refusing to sweep them: %s"
                         % ", ".join(bounded_line(item, 80) for item in strangers[:5]))
    _git(repo, "add", "--", *dirty)
    title = str(message or task.get("title") or task_id)
    subject = bounded_line(" ".join(title.split()), MESSAGE_LIMIT) or ("plan task %s" % task_id)
    _git(repo, "-c", "commit.gpgsign=false", "commit", "--no-verify", "-q", "-m", subject)
    new_head = run_output(["git", "-C", str(repo), "rev-parse", "HEAD"], cwd=repo)
    if not new_head or new_head == head:
        raise GraphError("commit did not advance HEAD")
    return {"schema": 1, "run_id": selected, "binding": str(binding), "task_id": task_id,
            "commit": new_head, "paths": paths, "status": "committed"}
