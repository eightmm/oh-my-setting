"""Bound-task projection onto goal-drive's exact commit/recovery transaction."""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional

from oms_runtime.common import bounded_line, install_root, run_output

from . import events as events_module
from .adapters import plan as plan_adapter
from .errors import GraphError
from .facts import receipt_facts

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
    title = str(message or task.get("title") or task_id)
    subject = bounded_line(" ".join(title.split()), MESSAGE_LIMIT) or ("plan task %s" % task_id)
    try:
        completed = subprocess.run(
            ["bash", str(install_root() / "scripts/goal-drive.sh"), "--repo", str(repo),
             "--commit-task", task_id, "--commit-message", subject],
            cwd=str(repo), capture_output=True, text=True, timeout=GIT_TIMEOUT,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise GraphError("exact commit could not complete; retry to reconcile: %s" % bounded_line(exc, 200))
    if completed.returncode != 0:
        raise GraphError("exact commit refused: %s" % bounded_line(completed.stderr or completed.stdout, 500))
    new_head = run_output(["git", "-C", str(repo), "rev-parse", "HEAD"], cwd=repo)
    if not new_head:
        raise GraphError("commit HEAD is unreadable")
    return {"schema": 1, "run_id": selected, "binding": str(binding), "task_id": task_id,
            "commit": new_head, "paths": paths, "status": "committed" if new_head != head else "clean"}
